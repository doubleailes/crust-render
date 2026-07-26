# MoonRay vs. Crust Render — a deep comparison

*Written against DreamWorks MoonRay ([dreamworksanimation/openmoonray](https://github.com/dreamworksanimation/openmoonray),
Apache-2.0, main branch as of 2026-07) and the current state of this repository
(2026-07). Everything below was read from the MoonRay/scene_rdl2/moonshine/mcrt_denoise
sources directly. As with the openqmc port, anything adopted here ports **concepts, not
code**.*

Unlike the Embree comparison (`embree_comparison.md`), MoonRay and crust are **the same
kind of software**: complete production path tracers — integrator, materials, lights,
volumes, sampling, scene I/O, image output. MoonRay is what a renderer looks like after
a decade of feature-film production hardening at feature-film scale; crust is a toy that
recently grew a real intersection kernel. The comparison therefore runs
subsystem-by-subsystem, and the interesting question is not "who is faster" (MoonRay,
by a lot, on purpose) but *which of MoonRay's architectural ideas survive translation
to a small, safe-Rust, scalar renderer* — and which are load-bearing only at production
scale.

Three questions structure the document:

1. What is MoonRay, subsystem by subsystem?
2. Where does crust stand on each of those subsystems?
3. Which MoonRay ideas are worth adopting now, and which are documented as future work?

---

## 1. What MoonRay is

DreamWorks' in-house production renderer (used on every DWA feature since *How to Train
Your Dragon: The Hidden World*), open-sourced in 2023 under Apache-2.0 as part of the
Academy Software Foundation. A Monte Carlo unidirectional path tracer whose signature
feature is **vectorized, bundled shading**: instead of shading each hit point as it is
found, hits are queued, sorted, and shaded in SIMD bundles via ISPC. The architecture
paper is Lee, Green, Xie & Tabellion, *Vectorized Production Path Tracing*, HPG 2017.

### 1.1 Layering and the render driver

The renderer is layered as `mcrt_common` (TLS, queues, ray types) → `shading` → `geom` →
`rt` (Embree, and OptiX/Metal for XPU) → `pbr` (lights, integrator, AOVs) → `rndr`
(driver, film, outputs), with `scene_rdl2` (scene description + math/util) beneath
everything. A `RenderContext` owns scene load and render prep and delegates to a single
global `RenderDriver`.

The frame is organized as **passes over 8×8 tiles**:

- A `Pass` is `{startPixelIdx, endPixelIdx, startSampleIdx, endSampleIdx}` — a range of
  tile-local pixels (0..64) at a range of per-pixel sample indices, applied to every
  tile. **Coarse passes** touch a well-dispersed *subset* of the 64 pixels at 1 spp
  (progressive preview via extrapolation); **fine passes** cover all 64 and refine spp.
- `TileWorkQueue` is a *virtual* queue: it stores no elements at all, just an atomic
  cursor over (pass, tile) pairs, and synthesizes `TileGroup{pass, startTile, endTile}`
  work items on demand. That one atomic counter is the entire load balancer.
- `TileScheduler` precomputes a **permutation of tile indices** — `MORTON` (default),
  `SPIRAL_SQUARE`, `SPIRAL_RECT`, `TOP`, `BOTTOM`, `LEFT`, `RIGHT`, `RANDOM`,
  `MORTON_SHIFTFLIP` — and can split tiles across machines for distributed rendering.
- Render modes: `BATCH`, `PROGRESSIVE`, `PROGRESSIVE_FAST` (no path tracing — quick
  normals/UV output), `REALTIME` (fixed ms budget per frame), `PROGRESS_CHECKPOINT`.

Thread-local state is cache-aligned and holds **three bump arenas with different
lifetimes** (general / per-subpixel / per-pixel) plus per-layer TLS objects; nothing in
TLS needs atomics. A war story preserved in a comment: the global cancel flag is
`alignas(64)` because false sharing on that one bool once took a 25 s render to 90 s.

### 1.2 Vectorized shading and XPU

MoonRay ships two functionally equivalent integrators — a scalar depth-first C++ one
and an ISPC breadth-first wavefront one (`ExecutionMode = SCALAR | VECTORIZED | XPU |
AUTO`):

- Rays, occlusion queries, radiance splats, and AOV writes all flow through per-thread
  bundling queues; **shade queues are per-material and thread-shared**, so one
  material's hits from all threads accumulate into one queue and get shaded together.
- Queue flushes hand handlers only multiples of the SIMD lane count; leftovers are
  pushed back.
- Each queued hit carries a packed 32-bit **sort key**: light-set index (top 7 bits, so
  the primary sort minimizes integrator divergence) ‖ UDIM tile ‖ mip level ‖
  Morton-swizzled quantized UV. A radix sort orders the bundle, then an **indexed
  AoS→SoA gather** (AVX-512/AVX/NEON variants) converts `RayState`/`Intersection`
  through the sort permutation — the sort itself only ever moves 16-byte entries.
- Every shader is written twice — C++ and ISPC — with a macro system that statically
  asserts the two struct layouts match. That is the standing cost of the design.
- **XPU mode** is vectorized mode plus a GPU occlusion-ray offload (OptiX or Metal):
  per-CPU-thread ray queues flush to the GPU when it is idle, and to the *CPU handler*
  when it is busy — free load balancing, and a silent fallback to pure vectorized mode
  if the scene uses any unsupported feature.

Reported speedups in the HPG paper: ~5× shading, ~3× integration from
vectorization.

### 1.3 Sampling and integration

- Unidirectional PT with NEE + MIS; separate multi-sample (Veach) and one-sample BSDF
  sampler models; dedicated integrators for BSSRDF and volumes.
- **Per-lobe depth controls** (defaults): `max_depth 5`, diffuse 2, glossy 2, mirror 3,
  volume 1, hair 5, presence 16; ≤1 subsurface evaluation per path (beyond it, SSS
  degrades to Lambertian). Russian roulette starts only past the first non-mirror
  bounce "to avoid breaking the nice stratification of samples on the first non-mirror
  hit"; `sample_clamping_value 10` from depth 1 clamps fireflies (biased, opt-out).
- **Stateless hashed sample sequences**: a `SequenceID` is a variadic compile-time hash
  over `(pixel, SequenceType, subpixel index, …)` where `SequenceType` enumerates ~24
  decorrelated stochastic dimensions (`Bsdf`, `Light`, `RussianRouletteBsdf`,
  `VolumeEquiAngular`, `Presence`, …). Any sample is reproducible from its key alone —
  which is what makes checkpointing, wavefront reordering, and distributed rendering
  sampling-exact. (Crust's OpenQMC domain-tree usage is the same idea.)
- Sample values come from pluggable precomputed tables (PMJ02 or best-candidate);
  the pixel partition is 64-wide while the lens partition is 31² and the time partition
  29² — **sizes chosen prime and coprime** so tiling the tables over the image
  decorrelates pixel/lens/time maximally. Kensler-style correlated multi-jitter hashing
  covers incidental draws.
- **Pixel filtering is a warp, not a weight**: the uniform pixel sample is warped
  through the inverse CDF of the filter (box / quadratic B-spline footprint 3 / cubic
  B-spline footprint 4), so accumulation stays a plain unweighted mean and every
  sample lands in exactly one pixel.

### 1.4 Adaptive sampling

A variation on Dammertz et al., *A Hierarchical Automatic Stopping Condition for Monte
Carlo Global Illumination* (cited in the source):

- The film keeps a second buffer accumulating **only the odd-indexed samples**. The
  per-pixel error estimate is `|mean_all − mean_odd|` — two correlated estimators of
  the same integral whose difference vanishes as 1/√n — normalized as
  `luma(|Δ|)·rsqrt(luma(mean))` plus an alpha-difference term so silhouettes resolve.
- Errors aggregate into an `AdaptiveRegionTree` — a kd-tree over screen space split at
  the marginal-error median — with the screen divided into **4×4 overlapping regions**
  (overlap = one tile) so tiles near region borders see their neighbours' error. The
  last thread to finish a region's tiles updates that region's tree while others keep
  rendering; Morton tile order is explicitly relied on for this to work well.
- Each tile runs a state machine: `UNIFORM_STAGE` (a uniform floor of samples,
  `min_adaptive_samples 16`) → `ADAPTIVE_STAGE` → `COMPLETED`. Scene vars:
  `max_adaptive_samples 4096`, `target_adaptive_error 10`.

### 1.5 Checkpoint / resume

`PROGRESS_CHECKPOINT` mode snapshots the film into the output EXR itself, periodically
(`checkpoint_interval`, default 15 min, or quality-stepped), with background writing so
the render never stalls on I/O:

- The resume file contains the **raw accumulation state** — including the per-pixel
  weight/sample-count buffer — plus history metadata, so resume is *exact*, not
  approximate.
- The scheduling insight: a **global tile sample id** (0..64·spp, ordered by the pass
  sequence) can be converted back into a legal pass list —
  `convertSampleIdRangeToPasses()` synthesizes a head fragment (finishing a partial
  pass), whole-pass body, and tail fragment. Any sample count is therefore resumable at
  exact granularity.
- A windowed ray-cost estimator predicts how many samples fit in the remaining time
  budget, and a snapshot estimator keeps checkpoint overhead under a user-set fraction.

### 1.6 Light sampling

- The `LightAccelerator` holds *two* structures: an Embree BVH over light geometry
  (answering "did my BSDF sample hit a light?") and a **`LightTree`** for importance
  sampling — a direct implementation of Conty Estevez & Kulla 2018, *Importance
  Sampling of Many Lights with Adaptive Tree Splitting*: SAOH build (surface area ×
  orientation-cone heuristic) and stochastic traversal with adaptive splitting
  (`light_sampling_quality`). Below a small light count it falls back to a linear scan.
- `light_samples` (default 2) light samples at the primary hit; clamped to **1 per
  light past the first non-mirror bounce**. Delta lights get exactly one sample;
  multi-sample MIS is Veach's model.
- Light types: Cylinder, Disk, Distant, Env, **Mesh**, **Portal**, Rect, Sphere, Spot.
- **Light filters** are a separate composable concept (BarnDoor, ColorRamp, Cookie,
  Decay, Rod, VDB, …) attachable to any light.
- Distinctive tricks: **ray-termination lights** (excluded from NEE but visible to
  depth-clamped paths, cheaply filling in energy lost to depth limits) and first-class
  **shadow linking** (per-part light sets, shadow sets, shadow-receiver sets).

### 1.7 Materials, shading system, scene description

- **RDL2**: scene classes declare typed attributes at runtime; every shader is a DSO
  plugin discovered on a path; scene files are Lua (`.rdla`) or binary (`.rdlb`). A
  `Layer` maps `(geometry, part)` → {material, light set, displacement, volume shader,
  shadow sets} and hands out dense assignment IDs used as fast keys everywhere.
- `BsdfBuilder`/`BsdfComponent` is the material-author API — declare components (fuzz,
  outer specular, specular, diffuse, transmission, glitter…) and the builder handles
  layering and Fresnel coupling. `DwaBase` is the shared übershader behind the Dwa
  material family, with hair (R/TT/TRT/TRRT), fabric, toon, glitter, iridescence.
  Microfacet lobes carry **energy-compensation** tables; BSSRDF options include dipole,
  normalized diffusion, and random-walk.
- The shading `Intersection` carries P, Ng, N, st, dPds/dPdt, screen-space texture
  derivatives, assignment id, medium IOR, min-roughness, and a typed per-primitive
  attribute blob with required-attribute validation.
- **Displacement** is its own shader interface, run during tessellation.

### 1.8 Geometry

- Procedurals are DSO plugins that emit primitives at render prep (this is how
  USD/Alembic loading plugs in). Primitive types: polygon mesh, subdivision mesh
  (OpenSubdiv, adaptively tessellated to quads, with a separate `dicing_camera`),
  curves (Bézier/B-spline spans handed to Embree's native curve intersectors), points,
  instances (with per-instance attribute overrides), VDB volumes.
- Motion blur: two motion steps (more is asserted away), shutter open/close remapped
  into the motion interval, optional slerp for rigid transforms, per-part local blur
  scaling.

### 1.9 Volumes

- Volumes are a first-class layer assignment (a `VolumeShader` on geometry), sourced
  from VDB grids. Integration is **ray marching with adaptive step size** — step =
  feature size / quality, coarsened with ray depth — over intervals delimited by
  volume-boundary events; a bitset tracks which volumes the ray is inside.
- Overlap resolves by `SUM`, `MAX`, or `RND`; emissive volumes get MIS between phase
  sampling and **emissive-voxel importance sampling** (an emission distribution built
  over the grid); equi-angular sampling is a dedicated sequence type.
- Multiple scattering is throttled and art-directable: `max_volume_depth 1` by default
  with per-depth attenuation/contribution falloff factors — explicitly biased, by
  design.

### 1.10 Texturing, AOVs, deep output, denoising

- Texturing wraps **OpenImageIO's TextureSystem** with per-thread caches
  (`texture_cache_size 4000` MB), UDIM as a dedicated texture type (and 7 bits of the
  shade sort key), and live invalidation of edited textures during interactive
  sessions. Ray differentials propagate along the path and convert to texture
  derivatives (and a scalar mip selector) only at the hit.
- **AOVs**: a flat schema of typed channel ids — geometric state (P, N, st, depth,
  motion), primitive attributes, material lobes (with their own small expression
  grammar), visibility, per-light. **Light path expressions** are compiled through
  OSL's LPE automata into a DFA, evaluated per path vertex in both C++ and ISPC.
- Deep output (OpenDCX-style) and **cryptomatte** (Psyop spec, with separate
  reflected/refracted channel sets) are built in.
- Denoising is a thin facade over OptiX / Metal / OpenImageDenoise taking (beauty,
  albedo, normal) — the AOV system feeds it.

### 1.11 Smaller ideas worth noting

- **Presence** (partial opacity) is not alpha blending: evaluated before shading; when
  path throughput is low it is stochastically rounded to 0/1, otherwise the integrator
  *splits* and continues a continuation ray through the surface. Presence shadow rays
  get their own bundled queue since a boolean occlusion test cannot answer them.
- **Nested dielectrics** via material priorities (Schmidt & Budge), which as a side
  effect removes self-overlapping same-material geometry.
- **Roughness clamping** path regularization: `minRoughness` carried on the path
  vertex, monotonically increasing along the path, applied to indirect lobes only.
- Profiling accumulators are built into the queue system (every stage tagged), and a
  runtime debug console can be telnetted into a running render.

---

## 2. Crust on MoonRay's terms

| Subsystem | MoonRay | Crust today |
|---|---|---|
| Execution model | Scalar + ISPC wavefront + XPU GPU offload | Scalar, one ray at a time, `&dyn Material` dispatch; SIMD only inside BVH4 traversal |
| Scheduler | 8×8 tiles, virtual pass/tile work queue, Morton/spiral orders, coarse/fine passes | Scanline rows (Rayon barrier per row) or hard-coded 16×16 tiles, no ordering |
| Sampler | Hashed stateless SequenceIDs + PMJ02/best-candidate tables, coprime partitions | OpenQMC Sobol domain tree keyed by `(x, y, frame, index)` — same statelessness property, stronger sequences |
| Pixel filter | Warp-based box/B-spline (footprint up to 4) | Implicit box (uniform jitter) |
| Adaptive sampling | Odd-buffer Dammertz error, tile state machine, region kd-tree | Per-pixel luminance relative-SE, checked every 4 samples inside the pixel loop, numerically fragile sum-of-squares |
| Checkpoint/resume | Exact resume from EXR, sample-id→pass conversion, background writes | None |
| Light sampling | Conty-Kulla light tree, 9 light types, filters, shadow linking | Uniform 1-of-N pick, 2 light types (sphere/rect area), linear `find_by_geom` |
| Integrator depth | Per-lobe depth limits, RR after first non-mirror, sample clamping | Single `max_depth`, RR from vertex 4, training-only clamp |
| Materials | DSO plugins, BsdfBuilder layering, Dwa family, energy compensation | Two built-ins: OpenPBR übershader + Emissive; no plugin seam |
| Volumes | VDB, marching w/ adaptive step, emissive-voxel MIS, overlap modes | Procedural/inline-grid regions, delta/ratio tracking (unbiased), summed-extinction overlap, no NEE toward emissive volumes |
| Path guiding | none | Practical Path Guiding SD-tree with variance-weighted pass blending |
| Texturing | OIIO cache, UDIM, ray-differential mips | None (no UVs anywhere) |
| AOVs / LPEs | Full schema + OSL LPE automata + cryptomatte + deep | Single beauty RGB buffer |
| Denoising | OptiX/Metal/OIDN facade | None |
| Scene I/O | RDL2 (Lua/binary), procedural DSOs | USD only (openusd) |

Points where crust is *not* simply behind:

- **Sampling quality**: crust's OpenQMC port (Owen-scrambled Sobol with a proper domain
  tree, bit-for-bit against upstream) is a stronger low-discrepancy foundation than
  MoonRay's table-based PMJ02/best-candidate approach, and the domain-tree API already
  delivers the stateless-resumability property MoonRay engineered SequenceIDs for.
- **Volume integration** is unbiased in crust (weighted delta tracking, ratio-tracking
  transmittance, exact nearest-event competition), where MoonRay ray-marches with
  art-directable biased falloffs — a deliberate production trade crust doesn't make.
- **Path guiding** exists in crust and not in MoonRay (DWA's guiding work is recent and
  not in the open-source drop).
- Crust is ~15k lines of safe Rust; MoonRay is hundreds of thousands of lines of C++,
  ISPC, and CUDA. Every adoption below is judged against that budget.

---

## 3. Takeaways

### 3.1 Adopted in this pass — as built

The three items below were implemented in this adoption pass. Where the as-built code
lives: `crates/crust-core/src/scheduler.rs` (tiles, orders, passes, work queue),
`film.rs` (accumulation + split-buffer error), `checkpoint.rs` + the driver in
`tracer.rs` (engine side), `crates/crust-render/src/checkpoint_io.rs` (resumable EXRs).

1. **Unified pass/tile scheduler.** The dual scanline/16×16-bucket drivers are gone,
   replaced by MoonRay's shape: 8×8 tiles in a precomputed order (Morton default;
   spiral, scanline, random via `--tile-order` / `crust:tileOrder`), a virtual work
   queue (one atomic cursor synthesizing tile groups, drained by every Rayon pool
   thread under `rayon::broadcast`), and a pass schedule `Pass{pixel range, sample
   range}` — sample 0 dispersed across four coarse passes over a Bayer-ordered fill
   pattern, then geometrically growing fine passes capped at 16 samples per pass.
   Per-tile results merge in tile order after each pass, so renders are deterministic
   under any thread scheduling. Deviation: no realtime/progressive display modes (the
   CLI is batch), so coarse passes exist for scheduling and resume granularity, not
   extrapolated preview.
2. **Split-buffer adaptive sampling.** The `Film` accumulator keeps an odd-sample
   buffer; per-pixel error `luma(|mean − mean_odd|)/max(luma(mean), 1e-3)`; per-tile
   Uniform → Adaptive → Completed state machine evaluated at pass boundaries, retiring
   a tile only when the tile *plus a 1-pixel neighbour ring* has converged.
   Deviations from MoonRay: error is mean-normalized (not `rsqrt`) to preserve the
   existing `crust:varianceThreshold` relative-standard-error semantics — calibration
   factor √(2/π) ≈ 0.798, since `mean − mean_odd` is half the even/odd split whose std
   is `2σ/√n` (derivation in `film.rs`, pinned by a statistical test); the region
   kd-tree is replaced by the neighbour ring (the overlap idea at 1/100 the
   machinery); no alpha term (no alpha channel yet).
3. **Checkpoint / resume.** Film snapshots at uniform pass boundaries on a time
   interval, exposed by the engine as plain data (`CheckpointState` via
   `Renderer::render_with_options` — crust-core stays encoding-free) and written by
   the CLI as a multi-channel EXR (`--checkpoint-interval`; raw sums + odd sums +
   counts, resume metadata in header attributes, atomic tmp+rename). Resume
   (`--resume`, extendable with a larger `-s`) is **bit-exact, adaptive sampling
   included** — guaranteed by the stateless sampler, the pure pass schedule whose
   suffix the resumed run replays, raw-f32 round-tripping, and stage re-derivation
   from the restored per-pixel counts; pinned by integration tests comparing straight
   vs. interrupted-and-resumed renders pixel-bit for pixel-bit. Validation is by
   settings fingerprint (stable FNV-1a; `samples_per_pixel` deliberately excluded).
   `PassSchedule::from_tile_sample_range` implements MoonRay's sample-id→pass
   conversion, enabling future mid-pass (SIGINT) checkpoints; v1 snapshots only at
   pass boundaries. Guided renders don't checkpoint (N blended pass buffers +
   SD-tree state; rejected with `Error::CheckpointUnsupported`).

### 3.2 Documented only — future work

| Idea | Why not now |
|---|---|
| Wavefront/vectorized shading (shade queues, sort keys, AoS→SoA) | MoonRay's signature feature, and the worst fit: it demands every material in two forms and SoA everywhere. Crust's scalar `&dyn Material` integrator with per-value samplers is the opposite design commitment. Revisit only behind the `crust-rt`-style seam, if ever. |
| Conty-Kulla light tree | Crust scenes have ≤ a handful of lights; uniform picking is exact and adequate. Adopt when many-light scenes exist — the `LightList::pick` seam is where it goes. |
| Light types (distant/env/disk/spot/mesh) + filters | Needs non-area `Light` impls and integrator support for lights without scene geometry; USD lux schemas are already parsed-and-skipped. Highest-value next light work is Env/Distant (the sky gradient is currently un-NEE-able). |
| LPEs / AOV schema / cryptomatte | All gated on a multi-channel `Film`; the Film introduced in this pass is the first step (beauty + odd + count now; albedo/normal/depth channels are the natural next). |
| Texturing (OIIO-style cache, UDIM, ray-differential mips) | Crust has no UVs at any layer (kernel barycentrics are discarded before materials). Prerequisite plumbing: uv arrays on `TriangleMesh`, uv on `HitRecord`, `primvars:st` import, ray differentials. A project of its own. |
| Per-lobe depth limits, sample clamping, roughness clamping | Small and valuable; roughness clamping in particular is ~15 lines on the path state. Deferred only to keep this pass focused; good first follow-ups. |
| Presence (stochastic partial opacity) | Needs `geometry_opacity` in OpenPBR first (currently listed as not implemented there). |
| Nested dielectrics via material priorities | Crust's carried-medium model is single-medium; priorities matter once overlapping dielectrics appear in scenes. |
| Region kd-tree adaptive aggregation | The tile+ring approximation should be measured first; adopt the tree if tile-boundary artifacts appear at low thresholds. |
| Background checkpoint writing, SIGINT checkpoints | `from_tile_sample_range` and the atomic-write CLI seam are designed to make both incremental follow-ups. |
| Denoising facade (OIDN) | Wants albedo/normal AOVs; sequence after the Film grows channels. |
| Distributed rendering, runtime console, arena TLS | Production-scale machinery crust doesn't need at toy scale. |

---

## 4. Sources

- Source read directly from `github.com/dreamworksanimation/{moonray, scene_rdl2,
  moonshine, mcrt_denoise, openmoonray}` (main, 2026-07): `lib/rendering/rndr/`
  (driver, TileWorkQueue/TileScheduler, adaptive/, checkpoint), `lib/rendering/pbr/`
  (integrator, lights, LightTree, sampler, AOVs), `lib/rendering/mcrt_common/`
  (queues, TLS), `lib/rendering/shading/`, `lib/rendering/geom/`,
  `scene_rdl2/lib/scene/rdl2/`.
- Lee, Green, Xie & Tabellion, *Vectorized Production Path Tracing*, HPG 2017.
- Conty Estevez & Kulla, *Importance Sampling of Many Lights with Adaptive Tree
  Splitting*, HPG 2018 (cited in `pbr/light/LightTree.h`).
- Dammertz, Hanika, Keller & Lensch, *A Hierarchical Automatic Stopping Condition for
  Monte Carlo Global Illumination*, WSCG 2010 (cited in
  `rndr/adaptive/AdaptiveRegionTree.h`).
- Kensler, *Correlated Multi-Jittered Sampling*, Pixar TM 13-01 (cited in
  `pbr/sampler/`).
- Schmidt & Budge, *Simple Nested Dielectrics in Ray Traced Images* (cited in
  `pbr/integrator/PathIntegrator.cc`).
