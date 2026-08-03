# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Crust Render is a toy, physically-based path tracer written in safe Rust (edition 2024),
inspired by PBRT, *Ray Tracing in One Weekend*, and Autodesk Standard Surface / OpenPBR.
Scenes are loaded exclusively from **USD** (`.usda` / `.usdc` / `.usdz`) via the pure-Rust
[`openusd`](https://github.com/mxpv/openusd) crate — RON support was removed.

## Commands

```bash
# Build / render (single binary in the workspace, so bare cargo run works)
cargo run --release -- -i samples/openpbr_showcase.usda -o out.exr
cargo run --release -- -i samples/cornellbox.usda
cargo run --release                 # no -i → hard-coded procedural fallback (world::simple_scene)
cargo run --release -- --bucket -i samples/cornellbox.usda   # tiled/bucket rendering

# CLI flags: -i/--input, -o/--output (default output.exr), -l/--level (log level),
# -b/--bucket, -s/--samples (override spp), --strategy (power|balance|light|bsdf),
# --filter (box|triangle|gaussian|blackman|mitchell) + --filter-radius (pixels),
# --stats (per-phase profile + scene statistics)

# Where did the time and memory actually go? (parse vs build vs render vs output)
cargo run --release -- -i samples/curves.usda --stats

# Tests (integration tests live in crust-core/tests/usd_scene.rs, load sample USD files)
cargo test
cargo test -p crust-core loads_cornellbox_usda     # run a single test by name

# Benchmarks (criterion)
cargo bench -p crust-core            # bench targets: "vec3 dot", "simple world", "simple world guided"
cargo bench -p crust-rt              # kernel traversal: intersect/occluded over 3 scene kinds + build

# Perf probes (better than criterion for kernel A/B: min-of-N, not a drifting mean)
cargo run --release -p crust-rt --example ray_throughput          # Mray/s per scene & query
cargo run --release -p crust-render --example exr_diff -- a.exr b.exr   # did the image change?

# Placing a camera in a downloaded production asset, and settling whether a
# texture is display-encoded or linear (see "Ptex" under USD import).
cargo run --release -p crust-render --example scene_bounds -- scene.usda
cargo run --release -p crust-render --example tex_probe -- texture.ptx
cargo run --release -p crust-render --example tex_probe -- render.png [x0 y0 x1 y1]

# Is a Ptex file actually being addressed correctly? Neither check renders
# anything -- a wrong Ptex lookup still produces a plausible-looking surface,
# so both answer in numbers instead. See "Ptex" under USD import.
#   1. face ids: does the .ptx's embedded base cage match the mesh it is bound
#      to, face for face and vertex order for vertex order?
cargo run --release -p crust-render --example ptex_verify -- model.usd /mesh/prim color.ptx
#   2. (u,v) orientation: are texels continuous across the seams the file's own
#      adjacency data declares, and more so than transposed or than chance?
cargo run --release -p crust-render --example ptex_seams -- color.ptx

# openusd composition probes. Each was written to pin down a bug that is now
# fixed upstream (see docs/issues/ and "Known incomplete work"); keep them as the
# regression check to run when bumping openusd.
cargo run --release -p crust-render --example proto_probe -- stage.usda [/prim]   # prototypes
cargo run --release -p crust-render --example rel_probe   -- stage.usda [relName] # rel targets
cargo run --release -p crust-render --example xform_probe -- stage.usda /prim     # xformOpOrder

# Kernel correctness is stated in exact float bits, so it must hold under every
# codegen: runs the suite with AVX/AVX2/AVX-512 off, with AVX2+FMA, and native.
scripts/test_simd_matrix.sh -p crust-rt

# --- The optimization loop (see "Measuring a change" below) --------------
scripts/bench_scenes.sh                        # min-of-N Render seconds + Mray/s per scene
scripts/check_images.sh record <dir>           # golden EXRs at 16 spp
scripts/check_images.sh check  <dir>           # re-render and diff; exits non-zero on any change
scripts/bench_ab.sh -a <binA> -b <binB> [scenes...]   # interleaved A/B of two binaries

# CI runs: cargo build --verbose && cargo test --verbose
```

Logging uses `tracing`; set verbosity with `-l debug|info|warn|error|trace` (default `info`).

Environment overrides, all of which exist to A/B an optimization against the behaviour it
replaced: `CRUST_STREAM_IMPORT=0` forces the single-stage USD import; `CRUST_MESH_BAKE=0`
forces every mesh to be instanced instead of baking single-placement geometry flat (output
is bit-identical with it set, which is what separates a deferral bug from a baking
difference); `CRUST_PTEX=0` declines every Ptex texture so surfaces fall back to their
constant `baseColor`; `CRUST_PTEX_MAX_LOG2` caps the per-face texture resolution loaded
(log2 edge length, default 5 = 32x32).

## Measuring a change

Three things about this codebase make naive measurement actively misleading, so the
tooling above exists to work around each.

1. **Sequential wall-clock comparisons lie.** On a busy machine, measuring before and
   after minutes apart reported a **12% regression** for a change `bench_ab.sh` then showed
   to be a 4-5% *improvement* — the difference was background load. Always compare two
   binaries with `bench_ab.sh`, which alternates A and B within the same seconds so load
   lands on both. Report min *and* mean.
2. **Small steps are below the noise floor.** The run-to-run spread reaches 15%, so a
   1-5% change cannot be resolved by timing at all. Count instructions instead — they are
   deterministic:

   ```bash
   RAYON_NUM_THREADS=1 valgrind --tool=callgrind --cache-sim=no --branch-sim=no \
       target/release/crust-render -i samples/cornellbox.usda -o /tmp/x.exr -s 2
   callgrind_annotate --inclusive=no callgrind.out.<pid>
   ```

   Function names resolve from the symbol table with no debug info; add
   `RUSTFLAGS='-C debuginfo=line-tables-only'` only when you want line-level detail.
   Note callgrind counts instructions, not cycles, so it under-reports anything whose gain
   is cache behaviour — and it cannot see `panic = "abort"` at all.
3. **Compare images at `-s 16`, never higher.** `min_samples_per_pixel` defaults to 32 and
   the adaptive early-stop needs `taken >= min_spp`, so at 16 spp every pixel takes exactly
   16 samples. Above that a single-ulp difference changes a pixel's sample budget and
   cascades, making a bit-identical change look structural. `check_images.sh` pins this.

For a change that *does* legitimately alter output (a different BVH reorders exact ties),
prove it is noise rather than bias by checking the difference falls as 1/√N across spp
rather than plateauing.

## Workspace layout

Five crates under `crates/`:

- **`crust-rt`** (lib name `crust_rt`) — the intersection kernel, factored out the way
  `openqmc-rs` was, behind a deliberately **Embree-shaped API**: `Geometry` values
  (triangle meshes with optional per-vertex shading normals, analytic spheres, round
  curve segments, `Instance`s — which nest — with transform motion blur) attach to a
  `SceneBuilder` with per-geometry visibility masks; `commit()` builds the acceleration
  structure; `Scene::intersect`/`Scene::occluded` mirror `rtcIntersect1`/`rtcOccluded1`.
  Hits are plain `Copy` `RayHit`s carrying `geom_id`/`prim_id` — the kernel never sees
  materials. Instanced hits report the *instance's* top-level `geom_id` with the inner
  `prim_id`. Internals: watertight Woop-2013 triangles, rounded-cone curves, and the
  parallel deterministic SBVH build collapsed to BVH4 (details below). Depends only on
  glam + rayon; deliberately swappable for Embree bindings behind the same seam.

- **`crust-core`** — the engine as a library (`crust_core`): renderer, integrator,
  materials, lights, volumes, path guiding, USD import — everything above the
  intersection layer, which it consumes from `crust-rt` through `rt_world.rs`
  (`WorldBuilder`/`World`: kernel geometries paired with a `geom_id`-indexed
  material table).
  UI-free by design: no progress-bar or image-encoding dependencies; progress is
  reported through a `ProgressCallback`, and fallible entry points return
  `crust_core::Error` instead of exiting.
  **`stats.rs`** collects per-phase timings and scene counts (`RenderStats`,
  inspired by Guerilla Render's "Profiling And Statistics"): the importer fills
  in its own phases and the counters onto `Scene::stats`, the host appends
  render/output phases, and the report prints three blocks — statistics, profile
  by execution tree, profile by time. Collection is one `Instant` per *phase*,
  never per ray, so it costs nothing in the integrator and is always on; only
  printing is gated (`--stats`). Two primitive views are reported because for an
  instanced scene they answer different questions: `top_level` is what the root
  BVH traverses, `unique` descends into instances counting each distinct
  prototype **once** and is therefore what occupies memory.
- **`crust-render`** — the thin CLI binary. Parses args, builds a `Scene`, calls the
  `Renderer` (wiring an `indicatif` bar to the progress callback), writes the EXR and
  the tone-mapped PNG. `main.rs` is the only file.
- **`utils`** — math/RNG helpers (`random*`, `random_cosine_direction`, `align_to_normal`,
  `balance_heuristic`, `power_heuristic`, `clamp`, `Lerp`). Depended on by `crust-core`.
- **`openqmc-rs`** (crate/lib name `openqmc`) — a self-contained, from-scratch Rust port
  of [AcademySoftwareFoundation/openqmc](https://github.com/AcademySoftwareFoundation/openqmc)
  (Apache-2.0), the quasi-Monte Carlo sampling library. Modules map one-to-one to the
  upstream `oqmc/*.h` headers (`pcg`, `reverse`, `rotate`, `permute`, `encode`, `float`,
  `range`, `owen`, `rank1`, `lookup`, `stochastic`, `state`, `sampler`, plus the samplers)
  and reproduce the upstream sample values **bit-for-bit** (pinned by
  `tests/golden_upstream.rs`, generated from the real C++ headers). It exposes the native
  **domain-tree** API: build a root `Sampler<T>` per `(x, y, frame, index)`, derive
  independent 4D sub-patterns with `new_domain(key)` (padding), draw ≤4 dims per domain
  with `draw_sample*` (high-quality stratified) or `draw_rnd*` / `rng()` (a `pcg::Rng`
  stream for incidental/unbounded draws). Six samplers — Owen-scrambled `SobolSampler`
  (the renderer's, aliased `crust_core::PathSampler`), `LatticeSampler`, `PmjSampler`, and
  their blue-noise variants `SobolBnSampler`/`LatticeBnSampler`/`PmjBnSampler` (optimised
  tables bundled as LE binary blobs in `src/data/`). Depended on by `crust-core`
  (materials, `guiding/`, `volume.rs`, `tracer.rs`) for every stochastic draw. Idiomatic
  divergences from the C++: the caller-allocated `void*` cache (a GPU concern) becomes a
  lazy process-global, keeping every `Sampler<T>` a small `Copy + Send` value.

`crust-core/src/lib.rs` re-exports the public surface (`Renderer`, `Scene`, `Camera`, the
material types, `simple_scene`, `get_settings`). Prefer importing from `crust_core::` roots.

## Render pipeline (the big picture)

1. **`main.rs`** builds a `Scene { camera, world, lights, settings, volumes }` — either from
   USD (`Scene::from_usd`) or the procedural fallback (`world::simple_scene` + `get_settings`).
2. **`Renderer`** (`tracer.rs`) drives sampling. Two entry points, both Rayon-parallel:
   - `render()` — parallel over pixels within each scanline row.
   - `render_with_tiles()` — parallel over 16×16 tiles (the `--bucket` path).
   Pixel reconstruction (`filter.rs`, `crust:pixelFilter` / `--filter`) is **filter
   importance sampling**, not splatting: each pixel warps its jitter through the
   filter's distribution and weights radiance by `f/p`, keeping every per-pixel
   mechanism (adaptive early-stop, QMC domains, pass blending) intact. The default
   is triangle at radius 1.0; box at radius 0.5 reproduces the historical in-pixel
   jitter bit-identically (`--filter box` when comparing against pre-filter
   renders). Mitchell is the only kind with negative weights.
3. **`trace_path()`** (`tracer.rs`, public wrapper `ray_color()`) is the integrator — an
   **iterative** path tracer in two passes: a forward walk that traces one segment per
   bounce and records a `VertexRec` per vertex, then a backward gather that folds the
   records into the radiance estimate and emits guiding training samples (which need
   the radiance from the rest of the path — the reason for the backward pass). Features:
   - **MIS** combining direct light sampling and BRDF sampling. The heuristic is
     selectable via `SamplingStrategy` (`crust:samplingStrategy` attr / `--strategy`
     flag): `power` (β=2 power heuristic — the default and historical behavior),
     `balance`, plus diagnostic single-strategy modes `light` (NEE at full weight,
     bounce-hit emission on light-list lights dropped) and `bsdf` (no shadow rays,
     bounce emission at full weight). All four are unbiased — the strategy's
     `light_weight`/`bounce_weight` pair is a partition of unity (pinned by a unit
     test); route BOTH sides of any new weight through the strategy or emission gets
     double-counted. `utils` now has both `balance_heuristic` (true `a/(a+b)` —
     historically this name computed the power formula) and `power_heuristic`
     (`a²/(a²+b²)`). `samples/veach_mis.usda` is the classic Veach comparison scene.
     Emission at a bounce-arrival vertex is owned by the *previous* vertex's record
     (`next_emit` + MIS weight); counting it at the vertex itself too would double it.
   - **Russian roulette** from the 4th vertex on (`RR_START_BOUNCE`): survival tracks
     path throughput with a probability floor (`RR_MIN_PROB`), factor divided out on
     survival.
   - Carried-medium transport (subsurface / glass interiors) when a ray holds
     `Some(Medium)` (set by transmissive OpenPBR refraction — see `ray.rs` /
     `medium.rs`): weighted analog free-flight sampling at the extinction majorant with
     the chromatic correction `e^{(σ̄−σₜ)t}` (gray media reduce to the classic
     albedo/Beer-Lambert forms). Carried-medium scatter vertices run **no NEE** and keep
     `prev = None` (full-weight next emission) — that pairing is what avoids double
     counting there. Free-space rays are unaffected.
   - **Volume regions** (`volume.rs`): free-standing smoke/fog/absorption/fire volumes
     held on `Renderer.volumes`, *outside* the surface BVH so their bounds never occlude
     shadow rays. Each `VolumeRegion` is an oriented box (composed prim xform) with a
     `DensityField` (`Homogeneous` | `Noise` fBm | `Grid` trilinear voxels) and
     σₛ/σₐ/g/emission (densityScale pre-folded into the coefficients). Per segment the
     integrator clips `Volumes::sample_interaction` to `min(t_surface, t_medium)` — the
     nearest-event competition between surface, carried medium and regions is then exact.
     Distance sampling is weighted delta tracking against the summed per-region majorant
     (chromatic-safe null collisions; absorption decays the weight instead of
     analog-terminating). Volume scatter vertices run **NEE with MIS** (`volume_nee`):
     the phase mixture's value == pdf (`PhaseMix`), and the bounce-side
     `bounce_emission_weight` has a matching `PrevVertex::Phase` arm — the two are a
     coupled pair exactly like the surface NEE pair; change one and you double-count
     emission. Shadow rays use `shadow_transmittance` (boolean surface occlusion ×
     `Volumes::transmittance` — ratio tracking, exact Beer-Lambert fast path when all
     crossed regions are homogeneous). Volume emission accumulates `σₐ·Lₑ` along the walk
     into `VertexRec.segment_emit`, which the gather adds **outside** `atten` (it is
     already weighted; folding it in would double-attenuate). Emission reached by a
     bounce (`next_emit`) is stored pre-multiplied by the arriving segment's attenuation.
     `volumes.is_empty()` short-circuits everything — volume-free scenes render as before.
   - A sky-gradient background when nothing is hit (attenuated by, and adding the
     emission of, any volumes the escaping segment crossed).
4. **Path guiding** (opt-in via `crust:pathGuiding`, `guiding/` module): a pure-Rust
   Practical Path Guiding SD-tree (`GuidingField`). `render_guided()` runs training
   passes at 2, 2, 4, 8, … spp (geometric, floored at 2 so every pass can estimate
   its own variance), splats `(position, direction, luminance·cos²)` samples
   into the field between passes, then renders the final pass with one-sample MIS
   between the frozen field and the BSDF (mixture pdf; secondary bounces only —
   primary vertices sit far below the field's spatial resolution). All passes
   (training + final) are blended into the output weighted by inverse variance, so
   the training budget is not discarded. Delta/transmissive
   materials (`Material::eval` → `None`) and untrained regions fall back to pure BSDF
   sampling. The NEE weight competes against the same mixture pdf — keep the two sides
   consistent or emission gets double-counted.
   The training passes double as a **guiding efficiency estimate** (Li et al. 2026,
   "Path Guiding in Disney's Zootopia 2"): efficiency `E = 1/(wall-clock cost × MRSE)`,
   comparing the first pass (field untrained → effectively unguided) against the last
   training pass (field most trained). MRSE normalizes each pass's per-pixel variance
   by one *shared* reference image (the blend of all training passes) — never by the
   pass's own noisy mean, which would correlate numerator and denominator and break
   the 1/spp scaling the comparison relies on. If `ΔEff < 1`, the final pass renders
   unguided (training passes still blend in; every pass is unbiased either way).
5. **Adaptive sampling**: pixels stop early once they hold `crust:minSamplesPerPixel`
   samples and the relative standard error of the pixel mean drops below
   `crust:varianceThreshold` (0 disables). Applies to main/final passes, never to
   guiding training passes.
6. The CLI writes the linear EXR to the `-o` path and a tone-mapped sRGB PNG next to it
   (same path, `.png` extension) — e.g. `-o renders/foo.exr` produces `renders/foo.exr`
   and `renders/foo.png`. Tone mapping and PNG encoding live in `main.rs`; the engine
   crate only produces the `Buffer`.

## Core traits (extension points)

- **Geometry & intersection** — there is no `Hittable` trait anymore: all intersection
  lives in the **`crust-rt`** kernel crate (see the workspace layout), and crust-core
  talks to it through `rt_world.rs`: a `WorldBuilder` pairs every attached
  `rt::Geometry` with its `Arc<dyn Material>` (`attach(...) -> geom_id`), and the
  committed `World` resolves kernel hits back to materials **by `geom_id`** —
  `World::intersect` returns a `WorldHit { rec: HitRecord, mat, geom_id, prim_id }`,
  `World::occluded` is the shadow-ray early-exit query (NEE never uses closest-hit).
  `HitRecord` (`hittable.rs`) remains the material-facing `Copy` hit geometry (point,
  ray-facing normal, `t`, `front_face`). Inside the kernel: `Sphere`,
  triangles with one shared **watertight** intersector (Woop et al. 2013 — dominant-axis
  shear, 2D edge functions with f64 fallback on exact-zero ties; no pinholes along
  shared edges), `RoundCurves` (sphere-swept cones for hair), and `Instance` (a
  committed inner `rt::Scene` placed by a transform — rays transform into local space
  with unnormalized direction so `t` carries over, normals map back by
  inverse-transpose; an optional end-of-shutter transform lerps per-ray for motion
  blur). Per-geometry masks gate intersection on `ray.mask` (`MASK_*` consts,
  re-exported from the kernel). The build is reference-based **SBVH** (binned object
  SAH + spatial splits gated by the α-overlap test, references clipped via exact
  Sutherland-Hodgman for triangles and duplicated across children), runs subtrees in
  parallel via `rayon::join` above 4096 refs, is **deterministic** (input-only
  decisions, pinned by a build-twice test), and is then **collapsed to BVH4**: 128-byte
  4-wide SoA nodes whose slab tests run on `Vec4` lanes (`safe_inv3` keeps
  zero-direction components NaN-free; closest-hit traversal orders lanes near-to-far,
  occlusion traversal early-exits). Traversal is mask-driven: `RaySlab` pre-splats the
  ray once per query, `cmple(..).bitmask()` yields all four lane verdicts at once, and
  a validity nibble in `WideNode::flags` masks unused lanes (they cannot just be given
  empty bounds — the slab test's per-axis min/max un-inverts an inverted box).
  Leaf payloads live in a side `Leaf` table (keeping the node at two cache lines), and
  each leaf's triangles are packed into **4-wide `Tri4` SIMD packets**: the Woop shear
  is per-*ray* (`RayShear`, derived once per traversal), so four triangles are
  intersected per vector round, with lanes whose edge functions come out exactly `0.0`
  handed back to the scalar path for its f64 tie-break — watertightness intact. The
  packet and scalar intersectors are **bit-identical** (pinned by
  `simd_matches_scalar_bitwise`); change one and you must change the other.
  `MIN_LEAF_PACKED` (4) is the leaf floor for all-triangle ranges so packets fill,
  while non-packable prims keep `MIN_LEAF` (2) — see `docs/simd.md` for the audit,
  the measurements, and why `std::simd` is not used (nightly-only on stable).
- **`Material`** (`material/material.rs`) —
  `scatter_importance(r_in, rec) -> Option<ScatterSample>` used by the integrator
  (`ScatterSample.delta` marks singular lobes like transmission: never mixed with a
  continuous density, no tracer cosine, emission carried at full weight),
  `eval(r_in, rec, wi) -> Option<(value, pdf)>` (evaluate the *continuous* component
  toward a given direction — what NEE and guided MIS need; `None` = no continuous
  component at all, and per its contract that decision must never depend on `wi`),
  and `emitted()`. Exactly two implementations: **`OpenPBR`**,
  the single übershader for all surfaces (with `diffuse`/`metal`/`glass`/`glossy` preset
  constructors used by `world.rs` and the USD fallback), and **`Emissive`**, a pure
  emitter with no geometry knowledge. Shared shading helpers (aniso GGX VNDF sampling,
  Schlick/F82 Fresnel, EON diffuse, Charlie sheen, thin-film, Cauchy dispersion) live in
  `material/brdf.rs`. The OpenPBR formulas are aligned against the MaterialX nodegraph
  and Adobe's `openpbr-bsdf` reference — the item-by-item alignment record (with the
  remaining gaps, e.g. no LUT-based multiple-scattering compensation and no random-walk
  SSS entry) is `docs/openpbr_reference_alignment.md`.
- **`Light`** (`light.rs`) — `sample_point`/`pdf`/`emission`/`material`. The one
  implementation is **`AreaLight`**: a `LightShape` (pure emitting geometry —
  `SphereShape`, `RectShape`) paired with the `Arc<Emissive>` its scene geometry carries.
  Lights are stored in a `LightList` and their surfaces are also attached to `world` as
  emissive geometry (Cornell-box semantics: a light is both light and visible object) —
  the `AreaLight` records the geometry's `geom_id`, which is how the integrator
  attributes a bounce-hit emissive surface to its light (`LightList::find_by_geom`).
  **NEE samples one light per vertex** (uniform pick), so the light strategy's MIS
  density is `light.pdf / n_lights` — the bounce side evaluates the exact same
  expression for the light it hit; keep the two sides identical or emission is
  double-counted. Emissive geometry with no light-list entry is handled: the bounce
  keeps its emission at full weight.

Sampling goes through the **`openqmc`** crate's native domain-tree API (see the workspace
layout above). The integrator (`tracer.rs`) threads the sampler *by value* — no stateful
`&mut dyn Sampler`: `render_pixel` builds a root `PathSampler::new(x, y, frame, index)` per
sample (with an extra `new_domain(tile)` so images wider/taller than 256 stay decorrelated,
since OpenQMC's pixel decorrelation tiles at 256), draws the camera dims from a `K_CAMERA`
domain, and hands the root to `trace_path`. Each path vertex derives `path.new_domain(depth)`
and each sampling event a further keyed sub-domain (`K_NEE`, `K_BSDF`, `K_GUIDE`, `K_PHASE`,
…, keys defined atop `tracer.rs`); materials draw one 4D block from the `SobolSampler` domain
they are handed. Unbounded/incidental draws — Russian roulette, volume delta-tracking,
carried-medium free flight — use `draw_rnd` or a `pcg::Rng` seeded from a domain
(`domain.rng()`), matching OpenQMC's `drawSample` vs `drawRnd` split. Tests that just need
randomness use `openqmc::pcg::Rng`.

## USD import (`scene/usd_import.rs`)

The only scene format. `load_scene` opens the stage, imports `RenderSettings` first (the
camera needs the aspect ratio), then traverses prims with an explicit stack that bakes the
Xform hierarchy into world matrices.

**Streaming import.** A production stage is dominated by USD itself, not by the
renderer's own structures: composing all of Moana costs openusd 75.74 GiB and 6m19s,
against 1.10 GiB and 2.6s for a stage masked to one element. So `load_scene` opens a
cheap index stage (`InitialLoadSet::LoadNone`) for the settings and the list of
top-level subtrees, then composes, traverses and **drops one masked stage per subtree**
(`stream_roots` + `traverse_into`), bounding the composed set at about one element.
On the island: 117.10 GiB / 13:20 → 43.76 GiB / 09:19, output pixel-identical.
`MIN_STREAM_CHUNKS` keeps the single-stage path for scenes too small to repay the
re-opens; `CRUST_STREAM_IMPORT=0` forces it.
The subtlety is cache keying: prototype paths (`/__Prototype_N`) are **numbered per
composition**, so every masked stage has its own `/__Prototype_0`. Anything keyed on
such a path must be scoped per stage, or one chunk's data is silently handed to the
next — see `MaterialCache::key` and `ImportCaches::epoch`. Keying materials on the bare
path cost 5 835 258 triangles before it was caught, and *no single element reproduces
it*: it needs two chunks that both carry prototype-internal materials.

Schema mapping:

- `UsdGeomMesh` → **either** world-space triangles in the top-level BVH **or** a
  local-space committed `rt::Scene` placed by an `rt::Geometry::Instance`, decided by how
  many times the geometry is placed. Prims with identical points/topology/material are one
  *distinct mesh* (content hash + memoized material Arcs, so binding paths compare by
  pointer); a distinct mesh placed **exactly once** is baked flat, anything placed more
  than once keeps one shared kernel scene and an instance per placement. Instancing what
  is placed once buys no sharing and costs every entering ray a transform, a slab setup and
  a cold descent into a second tree — and presents the parent BVH a box-of-a-box that
  spatial splits cannot tighten. On cornellbox this took instance descents from 3.85 to
  0.13 per camera ray.
  The decision is deferred: `emit_mesh` interns the triangles and reserves a `geom_id`
  (`WorldBuilder::reserve_slot`), and `flush_meshes` fills the slots in after the last
  streamed chunk — placement counts are only final then, and deciding **per chunk** would
  both make results depend on stage layout and give geometry shared between two elements
  one resident copy each. `CRUST_MESH_BAKE=0` forces the all-instanced behaviour; with it
  set the output is bit-identical, which is what separates a deferral bug from a baking
  difference. Baking a mirrored (`det < 0`) placement **swaps two indices**: world-space
  vertices wind the opposite way, so without it `front_face` inverts.
  Non-invertible transforms still bake immediately, and a mesh authoring
  `crust:motion:translate` always instances (a baked mesh has no transform left to lerp).
  `UsdGeomSphere` → analytic `Sphere` geometry.
- `UsdGeomBasisCurves` → an instanced `rt::Geometry::RoundCurves` batch: `linear` curves
  directly, `cubic` (bezier | bspline | catmullRom) flattened at 8 samples per span; widths
  (USD diameters) resolve per-vertex / per-curve / constant by array length.
- **Instancing** — both USD mechanisms reduce to the same thing, and share one code path
  (`collect_proto_parts` → `attach_proto_parts`): build a prototype's geometry *once*, then
  place it by transform. A prototype becomes a `Vec<ProtoPart>` — one part per bound leaf
  geometry, each a committed local-space `rt::Scene` plus its prototype-relative transform,
  material and ray mask. It is split per *part* rather than per prototype because `World`
  maps materials by top-level `geom_id`: a prototype binding two materials must become two
  instances or one material is lost. Prototypes are memoized by path in `ImportCaches`.
  - `UsdGeomPointInstancer` → one instance per entry of the per-instance arrays. The
    transform is USD's `translate ∘ orient ∘ scale`, under the instancer's own world
    matrix; `orientationsf` (quatf) wins over `orientations` (quath); `invisibleIds`
    prunes by `ids` (array index where `ids` is absent). The instancer's children are
    **not** traversed — prototypes are conventionally authored beneath it and are drawn
    only through it. Nested instancers and volumes inside prototypes warn and are skipped.
  - Native instancing (`instanceable = true` + a composition arc) → the prim's prototype
    (`/__Prototype_N`) is built once and shared by every instance; the instance's own proxy
    subtree is never descended into. Without this the importer re-read and re-hashed each
    instance's geometry (~30% of load time on a 2000-instance scene).
  - **Nesting.** A `PointInstancer` inside a prototype expands into real nested sub-scenes
    (`nested_instancer_parts`), one part per (prototype, part) so the grouping stays
    per-material: M nested placements of a K-part prototype become K parts, each a
    committed scene holding M instances. Flattening instead would multiply the outer
    instance count by the inner one — the blow-up instancing exists to prevent. The kernel
    nests to arbitrary depth. A nested *native* instance is skipped (upstream bug, below).
    Sample scene: `samples/nested_instancing.usda`. `MAX_INSTANCE_NESTING` (8) is a
    backstop against a malformed stage describing an instancing cycle.
  - `class` prims are abstract and never drawn on their own — only reached through the
    prototypes that reference them. `collect_proto_parts` deliberately ignores that rule,
    since naming a class as a prototype is how "geometry that exists only to be instanced"
    is authored. Sample scene: `samples/instancing.usda`.
  - Non-invertible instance placements (a zero scale — a common "hide this" idiom) are
    skipped: `rt::Geometry::Instance` requires an invertible transform.
- Any geometry prim may author `crust:rayMask` (int; bit 0 camera, bit 1 shadow, bit 2
  indirect — default all) to hide from ray categories, and `crust:motion:translate`
  (float3, world-space) to streak through that translation over the shutter (transform
  motion blur; primary rays draw a `K_TIME` shutter sample and every secondary/shadow ray
  inherits the path's time). Sample scenes: `samples/motionblur.usda`, `samples/curves.usda`.
- Materials resolve via `MaterialBindingAPI`, dispatched on the bound shader's `info:id`:
  - `UsdPreviewSurface` → mapped into `OpenPBR` (portable; `diffuseColor→baseColor`,
    `metallic→baseMetalness`, `roughness→specularRoughness`, etc.).
  - `crust:openpbr` → decoded 1:1 into `OpenPBR`; every input is the camelCase mirror of the
    Rust field name (lossless but non-portable). Reference scene: `samples/openpbr_showcase.usda`.
  - `PxrDisneyBsdf` → mapped into `OpenPBR` (both descend from Burley's model). Checked
    **before** `compute_surface_source()`, by looking for a child shader with that
    `info:id` — a material with several render-context outputs resolves through that call
    to whichever one USD prefers, and on the Moana island (which authors
    `outputs:ri:surface`, `outputs:glslfx:surface` and `outputs:ri:displacement` on every
    material) that is the *preview* shader, whose inputs are all `.connect`ed to the
    material interface rather than authored as values. Decoding it yields every parameter
    at its default. Parameters are therefore read off the **Material** prim, where the
    island authors them. `sheen` is deliberately **not** mapped to `fuzz_weight`: Disney
    adds sheen at grazing angles, OpenPBR mixes fuzz *over* the layers beneath, so the
    island's `sheen = 1` erased all base colour (Ptex included) and rendered smooth
    plastic. `subsurface*`, `diffuseTransmission` and `specularTint` have no equivalent
    lobe and are dropped.
  - Unbound geometry → grey diffuse `OpenPBR`.
- **Ptex** (`texture.rs`, plus the decoder in `crust-render/src/main.rs`) — per-face colour textures via
  the pure-Rust [`ptex-rs`](https://github.com/doubleailes/ptex-rs) reader, driving
  `OpenPBR::base_color`. A material's `inputs:surfaceMap` asset is the hook (both of the
  island's Ptex shader paths — `PxrPtexture.filename` and `HwPtexTexture_1.file` —
  `.connect` to it, so no network walk is needed). Asset paths come from openusd's
  `resolved_path()`, which anchors against the *authoring* layer — essential here, since a
  production stage's `../../../textures/foo.ptx` is authored several directories below the
  root layer.
  - **crust-core still decodes nothing**, but the `AssetLoader` seam *inverts* for Ptex:
    an environment map crosses it as a decoded pixel buffer, whereas a `.ptx` — a per-face
    mip pyramid that can reach gigabytes — crosses it as a **sampler**
    (`load_ptex → Arc<dyn PtexTexture>`) the host owns. Defaulted to `None`, so existing
    hosts are unaffected.
  - **Face ids are mesh face indices**, so `triangulate` records per emitted triangle its
    source face plus which slice of that face's fan it is (`FanSlice`), and `World`
    resolves a hit's barycentrics into `(face_id, u, v)` — Ptex parameterises a quad
    `v0=(0,0) v1=(1,0) v2=(1,1) v3=(0,1)`, so the lower fan half gives `(u+v, v)` and the
    upper `(u, u+v)`. The table lives on `World` keyed by `geom_id`, **not** on `MeshGeom`,
    which is dropped the moment a mesh is baked or committed. A skipped face must still
    consume its face id or everything after it shades from the wrong texel
    (`face_table_tests`). `bake_indices`' mirror swap exchanges `u` and `v`, so the table
    carries that flag per placement. Built only when the material reports a
    `face_texture()`, so an untextured stage allocates nothing.
  - The table is carried on **both** geometry paths. A direct mesh gets it in
    `flush_meshes`; a prototype carries it on its `ProtoPart` and
    `attach_proto_parts` records it against the instance's `geom_id`. Wiring only
    the direct path is not enough and is not obviously broken either: the island's
    geometry is almost entirely prototype-based, so Ptex silently applied to none
    of it and every textured surface fell back to its constant `baseColor` — which
    for a `PtexBaseMaterial` is an unused placeholder, so the gardenias rendered
    flat red. A part is always exactly one leaf geometry (the walk splits per bound
    mesh; a nested instancer groups per (prototype, part)), so one table serves
    every placement and the `prim_id` a hit reports indexes it unambiguously
    however many instance levels it passed through.
  - The host **preloads every face** into one immutable buffer: `PtexReader` reads from
    disk on each call (`&mut self`, pixel data uncached), and a path tracer asks from every
    thread in an unpredictable order. Faces load **mip-reduced**, capped at 32×32 by
    default (`CRUST_PTEX_MAX_LOG2` overrides as a log2 edge length) — full resolution is
    authored for close-ups, so `isLavaRocks`' 631 MB / 11 384-face colour file costs
    130 MiB instead of several GB, at a resolution far past what a 595×520 framing
    resolves. Texels are decoded to linear once at load (the island's graph gammas raw
    Ptex, and `HwPtexTexture_1` declares `sourceColorSpace = "sRGB"`; treating the data as
    already linear overshoots albedo ~4×, which `examples/tex_probe` exists to settle).
  - `CRUST_PTEX=0` declines every texture so the same scene renders on its constant
    `baseColor` — the A/B switch that separates a wrong Ptex lookup from a wrong material
    or wrong lighting.
  - **Verified numerically, not by eye** — a wrong face id or a transposed `(u,v)` still
    renders as plausible rock, so appearance proves nothing and the reference image
    (different camera, lighting, displacement and subdivision) proves less. Two
    render-free checks, both passing on the island:
    `ptex_verify` exploits the fact that each island `.ptx` embeds the base cage it was
    baked against (`PtexFaceVertCounts` / `PtexFaceVertIndices` / `PtexVertPositions`), so
    the texture can be asked which vertices *its* face N has and the answer compared with
    the mesh's face N. All 10 isLavaRocks meshes and isMountainA pass with face-vertex
    index sequences equal **in order** (45 536 and 134 012 indices respectively) — which
    pins the face correspondence *and* the corner ordering that fixes the UV orientation.
    `ptex_seams` then tests the quad convention itself against the file's `adjface` /
    `adjedge` data: mean texel difference across shared edges is 1.5–16x lower under
    `v0=(0,0)` than transposed, and 1.9–97x lower than between unrelated faces.
    Alongside those, 10 textures / 28 816 faces against 57 632 triangles (exactly 2 per
    quad). The importer warns when a texture's `numFaces` disagrees with its mesh. Not reproduced: no
    displacement (`inputs:displacementMap` is unread), no subdivision (meshes are
    `catmullClark`; the base cage is rendered, which Ptex is indifferent to since its
    faces *are* cage faces), and the reference's `islandsunEnv.tex` environment is a
    RenderMan-only format.
- `UsdLuxDistantLight` → a `DistantLight` in the light list only (no scene geometry). It
  points down its local -Z; `inputs:angle` is the source's angular *diameter* (default
  0.53°, the sun's) and a zero angle is widened to `MIN_DISTANT_ANGLE_DEG` rather than
  made singular, so the integrator keeps one MIS path instead of a delta special case.
  `intensity × color × 2^exposure` is the **irradiance** on a surface facing the light and
  the radiance is derived as `E / Ω` — widening the angle softens shadows without changing
  exposure (Hydra's normalized convention). Bounce rays find it by *escaping* along a
  direction inside its cone, which is the `Light::escaped` half of MIS.
- `UsdLuxDomeLight` → a `DomeLight`: an infinite environment covering every direction, so
  once one exists it **replaces the built-in sky gradient** (`Light::escaped` answers for
  every escaping ray). Radiance is `intensity × color × 2^exposure` times an optional
  lat-long `EnvironmentMap`; only `latlong`/`automatic` `texture:format` is supported and
  anything else warns and falls back to the uniform colour. The prim's *rotation* orients
  the sky (a dome is at infinity, so its translation and scale are meaningless). The map
  is importance-sampled by luminance × sinθ — the Jacobian matters, without it polar
  texels are over-sampled — which is what keeps a small bright sun in an HDRI from
  becoming a firefly farm.
  - **crust-core decodes nothing.** `inputs:texture:file` is resolved against the USD
    layer's directory and handed to the host through the `AssetLoader` trait
    (`Scene::from_usd_with_assets`); `Scene::from_usd` passes `NoAssets`, which warns and
    falls back to the uniform colour. `crust-render` implements it with `exr` (OpenEXR)
    and `image` (`.hdr` and LDR, the latter un-gamma'd to linear). This is the seam
    general texture support should grow through.
- `UsdLuxSphereLight` → emissive `Sphere` geometry + `AreaLight(SphereShape)`;
  `UsdLuxRectLight` → two emissive `Triangle`s + `AreaLight(RectShape)` (local XY plane,
  emitting along -Z per UsdLux; effectively one-sided). Sample scene: `samples/rectlight.usda`.
  Other lux types (`DiskLight`, `CylinderLight`) warn once and are skipped.
- **Volumes**: any prim carrying `crust:volume:type` imports as a `VolumeRegion` (checked
  *first* in the dispatch, so it never becomes geometry — its bounds must not occlude
  shadow rays). The local box is `[-size/2, size/2]³` when the prim authors `size` (a
  `Cube`; USD's default size is 2), else the unit cube; placement/orientation/scale come
  from the composed prim transform. Attributes (all in `crust:volume:`, defaults in
  parentheses): `type` = `homogeneous` | `smoke` | `grid` (required); `densityScale` (1);
  `sigmaS` color3f (0.5 grey); `sigmaA` color3f (0); `emission` color3f (0);
  `anisotropy` (0, clamped ±0.99). Smoke adds `noiseScale`/`noiseOctaves`/`noiseGain`/
  `noiseLacunarity`/`noiseThreshold`/`noiseSeed` (4 / 4 / 0.5 / 2 / 0.3 / 0); grid needs
  `gridDims` int[3] + `gridData` float[] (x-fastest, length must equal nx·ny·nz — warns
  and skips otherwise). Sample scenes: `samples/fog.usda` (homogeneous god rays),
  `samples/smoke.usda` (noise plume + emissive ember + tiny explicit grid).
- `UsdRenderSettings` gives `resolution`; per-render params live as custom attrs in the
  `crust:` namespace (`crust:samplesPerPixel`, `crust:maxDepth`, `crust:minSamplesPerPixel`,
  `crust:varianceThreshold`, `crust:frame`, `crust:samplingStrategy` token = `power` |
  `balance` | `light` | `bsdf`, `crust:pixelFilter` token = `box` | `triangle` |
  `gaussian` | `blackman` | `mitchell` + `crust:pixelFilterRadius` float). Missing attrs
  fall back to defaults (128 spp, depth 32, 640×360, power MIS, triangle filter at
  radius 1.0) defined as consts at the top of the file.

Note: `openusd` is a hard dependency and USD is always compiled in — there is no `usd`
feature flag.

**`openusd` is tracked at `0.6`**, not 0.5. Two composition bugs that made the Moana
island unreadable were fixed in 0.6.0 (both written up under `docs/issues/`), and the
importer is written against that release's API: `Stage::prim` (0.5's `prim_at`) and
`sdf::Value::Token` carrying an interned `tf::Token` rather than a `String`. Going back
to 0.5 means undoing those two renames *and* reinstating the island workaround.

## Rendering the Moana island

**`usd/island.usda` imports directly**: 3 151 850 geometries, 21 904 388 top-level BVH
primitives, ~6:18 to parse (~4:45 of it traversal), ~47.6 GiB peak. It reads from its own
root layer with no preparation — the openusd bugs that used to prevent that are fixed in
0.6.0. The only geometry still lost is 35 empty xgen prototypes (beach shells, fibers,
seaweed, palm debris); the six `PointInstancer`s that used to vanish with them now import,
which is worth ~74 500 resident instances of bay cedar understory.

`renders/moana_island/island_root.usda` (gitignored) is a hand-assembled root layer that
references each element at **stage root** instead of under `/island`. It is no longer
needed, and is kept only as the workaround for openusd 0.5, where the nested-reference
bug otherwise yields almost no geometry. `/island` carries no transform, so the two are
geometrically equivalent; they differ ~0.06% in the *unique* (resident) triangle count,
which is prototype-sharing accounting rather than rendered geometry — the likely cause
being that `/island` is a single top-level subtree, so `MIN_STREAM_CHUNKS` keeps the
direct read single-stage while the 22-prim layer streams, giving the two different
`MaterialCache` epochs. That layer also authors a camera inline as a copy of `shotCam`,
because the importer takes the *first* `UsdGeomCamera` it meets and traversal order is
unspecified — reading `island.usda` directly gets whichever of its seven cameras comes
first.

Measured per element (Ptex declined, so geometry only): **~57.7 M top-level triangles**
across the 20 elements, the largest being `osOcean` (15.6 M), `isCoral` (14.5 M),
`isMountainA` (6.7 M) and `isMountainB` (6.4 M). Worst single-element openusd composition
peak is ~12.9 GiB (`isCoral`), which streaming keeps as a transient rather than a sum.
Ptex over the whole island is 2 576 238 faces: **4.58 GiB** at the default 32x32 cap,
1.84 GiB at 16x16, 736 MiB at 8x8 — and **494 GiB** at full resolution, which is why the
cap is not an optimisation but the thing that makes the island possible.

Two costs specific to the full rig: `island.usda` authors *two* `DomeLight`s, and crust
has no per-light camera-visibility, so both light the scene (the sky is doubled) and both
textures decode — `islandsunVIS.png` is 16384x8192 and the pair peaks at ~11 GiB. Dropping
`sky_dome_cam_llc` (`active = false`) is the first lever if memory or exposure matters.

## Known incomplete work

- **Geometry/acceleration caveats.** Motion blur is transform-only and lerps the *matrix*
  linearly (no deformation blur, no quaternion motion — a large shutter rotation bows
  slightly, but the union-of-endpoints bbox stays conservative). Curve import flattens
  cubic spans to polylines (no exact cubic intersector) and lerps widths across a span in
  parameter; the rounded-cone can report an interior sphere surface for rays *starting
  inside* the hull (irrelevant for opaque hair). Mesh-BVH sharing needs identical
  points/topology *and* material binding. Emissive curves/instances are not light-list
  entries (BSDF-sampled only, like emissive volumes).
  Baking single-placement meshes (above) leaves *resident* memory unchanged — the same
  triangles, one fewer BVH — but it moves work into a single large top-level SBVH build,
  and that build's **transient** peak is higher: on `Kitchen_set` (1 394 meshes baked,
  414 599 top-level triangles) kernel memory went 134.59 → 134.44 MiB while peak RSS went
  374 → 579 MiB. The cause is pre-existing and not specific to baking: `PrimRef` and the
  binary `Node` are 48 bytes each, and `merge` concatenates child node arrays while both
  children are still alive, so one build over ~600 K references (with SBVH duplication)
  transiently holds a few hundred MiB where 1 788 small builds held almost nothing. If that
  peak ever matters more than the ~20% render win, the lever is a triangle-count cap on
  baking; the real fix is a builder that does not materialise the whole binary tree.

- **Instancing caveats.** The kernel nests instances to arbitrary depth (transforms
  compose, normals map back through every level, masks gate per level — pinned by
  `instances_nest`, `nested_instances_compose_transforms_and_normals` and
  `nested_instances_respect_masks_at_each_level` in `crust-rt`), and the importer expands
  a `PointInstancer` inside a prototype into real nested sub-scenes. What is *not*
  supported is a natively-instanced (`instanceable`) prim inside another instance's
  prototype: `openusd` 0.5 cannot read its contents at all (see the upstream bug below),
  so the importer skips it with a warning. Volumes inside prototypes are skipped — they
  live outside the surface BVH by design and cannot ride an instance transform.
  `PointInstancer`
  `velocities` / `accelerations` / `angularVelocities` are ignored, so vectorized instances
  do not motion-blur (`crust:motion:translate` still works on ordinary prims), and all
  per-instance arrays are read at the default time sample. Top-level `UsdGeomSphere` prims
  still bake their centre into world space and so ignore scale; spheres *inside* a
  prototype go through the instanced path and scale correctly.

- **SIMD stops at 128 bits** (`docs/simd.md` has the audit and the numbers). Everything
  vectorized — `glam`'s `Vec3A`/`Vec4`, the BVH4 slab test, `Tri4` leaf packets — is
  SSE2/NEON-width, because `std::simd` is still nightly-only and crust builds on stable.
  Current practice says target AVX2 instead, so the reasons not to are recorded: merely
  *enabling* AVX2 codegen is worth 2–4% (LLVM cannot widen a 4-lane algorithm), and
  8-wide leaf packets would buy exactly nothing because no leaf holds more than 4
  triangles — pinned by `eight_wide_packets_would_not_reduce_vector_rounds`. **BVH8
  nodes** are the one place 256-bit vectors would still pay, and that is a project
  decision, not just an optimization: reaching 256 bits at *runtime* needs `unsafe`
  `core::arch` intrinsics (against this crate's "100% safe Rust" claim), a new
  dependency (`wide`/`multiversion`), or a non-distributable `-C target-cpu`.
  Also: only *triangles* pack — sphere, curve and instance leaves still run scalar (no
  `Sphere4`); rays are traced one at a time, so there is no coherent ray-packet tracing
  (that needs the integrator restructured, not just the kernel); and the checked-in
  sample scenes are shading-bound, so kernel speedups barely show up there — use
  `scripts/gen_stress_scene.py` to benchmark traversal changes end to end.

- **`openusd` xformOp bug, worked around locally; fixed upstream in 0.6.0.** `openusd`
  0.5.0 composed multi-op `xformOpOrder` stacks in the wrong order (the authored
  translate came back multiplied by the scale), which used to make
  `samples/cornellbox.usda` render as floating objects against sky. `usd_import.rs`
  therefore composes the individual `xformOp:*` attributes itself
  (`compose_xform_ops`: translate/scale/rotateX·Y·Z/rotate-Euler-triples/orient/
  transform, `!invert!` prefixes, namespaced suffixes), falling back to openusd's
  composition — with a warning — only for op kinds it cannot decode. Regression test:
  `cornellbox_transforms_compose_correctly`.
  **On 0.6.0 the case that motivated it is fixed**: a translate+scale stack composes to the
  authored translation with the scale on the diagonal (`examples/xform_probe`). That is one
  case, not the whole surface — rotations, Euler triples, `orient`, `!invert!` prefixes and
  namespaced suffixes are unverified — so the local composer stays authoritative and the
  fallback stays in place. Retiring either means checking those kinds first.

- **Fixed in openusd 0.6.0, keep in mind when reading old branches.** Two composition
  bugs used to make the Moana island import as almost nothing, and both failed silently —
  the data was readable, the API reported success, and the importer just saw less than was
  there. A prototype did not materialize when the `instanceable` prim arrived through a
  reference on a non-root prim (which is exactly `island.usda`'s shape), and a
  relationship's targets resolved to zero when its prim reached the prototype through a
  variant selection (worth six `PointInstancer`s, all isBayCedarA1's variant geometry).
  Written up with minimal reproductions under `docs/issues/`, kept because the symptoms
  are worth recognising, and because pinning to 0.5 brings both back. `examples/proto_probe`
  and `examples/rel_probe` are the diagnostics.

- **Nested native instances are still skipped, but no longer have to be.** An
  `instanceable` prim *inside another instance's prototype* could not be read on
  `openusd` 0.5.0: resolving its prototype, or reading the type name of anything beneath
  it, tripped a `debug_assert!` in `pcp/instancing.rs::materialize_prototype` (debug builds
  aborted, release had it compiled out). So `collect_proto_parts` tests `is_instance()`
  **before** any schema lookup — a schema `get()` reads the type name, which is what
  aborted — and skips such prims with a warning. Regression test:
  `nested_native_instance_degrades_gracefully`.
  **On 0.6.0 that abort is gone**: a debug build resolves the nested prototype to a valid
  prim with its geometry (checked with `examples/proto_probe` on a four-line stage). The
  skip arm is therefore now conservative rather than necessary, and deleting it would
  recover this geometry — splice the inner prototype's parts in with composed transforms,
  since a native instance is a single placement and needs no extra level of kernel
  indirection. Not done yet, and it costs the Moana island nothing (that arm never fires
  there), so it is a correctness improvement for other stages rather than a fix for this
  one. The regression test would need rewriting to assert the geometry arrives instead of
  that it is skipped.
- **Lighting caveats.** `DiskLight` (needs a disk primitive) and `CylinderLight` are still
  skipped. `DomeLight` sampling is nearest-texel with no bilinear filtering, so a
  low-resolution HDRI shows texel edges in a mirror; `inputs:texture:format` values other
  than `latlong` are refused rather than mapped wrongly; and light-list picking stays
  uniform, so a dim dome costs as many shadow rays as a bright sun. Neither infinite light
  is visible to the guiding field's spatial structure (they have no position).
- **Path guiding** covers surfaces only (no volume/phase guiding) and trains on luminance
  (no chromatic distributions). Thick transmission — dispersive or not — is a
  continuous Walter et al. 2007 microfacet BTDF — sampled via VNDF + Snell, evaluable
  over the full sphere, and part of the NEE/guide mixtures (guide-chosen directions
  cross the interface via `Material::make_ray`, which tags the interior medium).
  Dispersion is continuous per-channel: each RGB channel refracts with its own
  Cauchy/Abbe-derived IOR (`cauchy_ior`, anchored at the Fraunhofer d line), sampling
  picks one channel's IOR uniformly, and evaluation runs three per-channel
  BTDF evaluations whose sampling pdfs average into the channel-mixture density. Only
  thin-walled transmission remains a per-sample delta lobe (`ScatterSample::delta`),
  excluded from continuous mixtures — carrying window-model energy
  (`(1−R)/(1+R)` transmittance, boosted `2R/(1+R)` reflection, view-dependent tint). The guide-vs-BSDF selection probability is fixed (no learned α), and
  spatial lookups are not parallax-compensated.
- **Volume regions** (`volume.rs`) have no OpenVDB / `UsdVolVolume` import (openusd 0.5
  ships no `vol` feature) — density is homogeneous, procedural fBm noise, or an inline
  voxel grid authored in the USDA. No volume path guiding (volume vertices push
  `train: None`; volume-heavy scenes train the surface field on noisier estimates —
  slower convergence, not bias). One global majorant per region — no coarse max-grid, so
  a high `densityScale` over a large box tracks slowly. Emissive volumes are not
  light-list entries: fire is found only by phase/BSDF-sampled paths (firefly risk near
  bright emission), never by NEE. Carried-medium (subsurface) scatter vertices run no
  NEE. Region overlap uses summed extinction (exact) with a σₛ-weighted phase mixture.
- **HG convention fix**: `sample_henyey_greenstein` used to apply PBRT's inversion to the
  propagation direction (PBRT's frame is around the *reversed* `wo`), so `g > 0`
  scattered backward. It now scatters forward, matching `hg_phase` (value == pdf, cosθ
  against the propagation direction); the histogram test in `medium.rs` pins the pair.
  The carried-medium estimator was also fixed: it double-counted extinction for
  scattering media (analog free-flight at σ̄ *plus* full Beer-Lambert) — scattering
  interiors (subsurface) render brighter than before, correctly. And bounce-hit emission
  (`next_emit`) is now attenuated by the arriving segment (tinted glass / smoke in front
  of an emitter used to pass emission through undimmed).
