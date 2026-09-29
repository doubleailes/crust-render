# rendering — design record

> Design record for the **rendering** capability: the reasoning, measurements and
> history behind the behaviour `spec.md` states. Moved out of `CLAUDE.md`, which
> now keeps only the rules and pointers. Section and path references such as
> "above" or "see X" may point to another capability's `design.md` —
> `openspec/specs/*/design.md` is the whole record; `docs/architecture.md` is the map.

## Sampling library: openqmc-rs

Two libraries were extracted out of this tree into repositories of their own on the
same principle (zero crust types in the API, generally useful, published) and are
consumed as ordinary dependencies:

- **`openqmc-rs`** (crates.io; crate `openqmc-rs`, lib name `openqmc`) — a self-contained, from-scratch Rust port
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
  (materials, `guiding/`, `volume.rs`, `tracer/path.rs`) for every stochastic draw. Idiomatic
  divergences from the C++: the caller-allocated `void*` cache (a GPU concern) becomes a
  lazy process-global, keeping every `Sampler<T>` a small `Copy + Send` value.
  **Performance, as of 0.2.4:** Sobol draws were 16–24% of a render here (callgrind,
  `draw_block`), almost all of it the GF(2) direction-matrix product done as a 16-step
  loop over the index bits. 0.2.4 evaluates it from two compile-time 256-entry byte
  tables per dimension (the product is linear over GF(2), so the two lookups XORed are
  the loop's result exactly): render instructions −10–18%, render time −6–20%, every
  image bit-identical. The workspace requires `0.2.4` because `Cargo.lock` is not
  committed. Profile the sampler before assuming a cost is the BSDF's — `draw_block`
  is inlined into its callers (`scatter_resolved`, NEE, the camera) and easy to misread
  as their own.
  **Do not replace `u32::reverse_bits` with a lookup table** — measured slower both ways
  (2026-09-27, prototype against 0.2.4). Bit reversal is the sampler's largest remaining
  cost (~71 M instructions on cornellbox at 2 spp), but on x86-64 `reverse_bits` is a
  `bswap` plus three mask-shift-or rounds, ~16 register-only instructions. A 256-entry
  byte table (four lookups) cost **+7.7%** `render_pixel` instructions; a 64 Ki-entry
  `u16` table (two lookups, 128 KiB, out of L1) cost +3.0% instructions and **+3–8.5%**
  wall time (`bench_ab.sh`, cornellbox / veach_mis / materialx_basic /
  usdpreview_textured). Routing dimension 0 through the GF(2) byte tables instead of
  `reverse_bits16` would be exact but saves under 0.5% of render.

## Render pipeline

## Render pipeline (the big picture)

1. **`main.rs`** builds a `Scene { camera, world, lights, settings, volumes }` — either from
   USD (`Scene::from_usd`) or the procedural fallback (`world::simple_scene` + `get_settings`).
2. **`Renderer`** (`tracer/mod.rs`) drives sampling. Two entry points, both Rayon-parallel:
   - `render_with_tiles()` — parallel over 16×16 tiles. **The CLI's default**. It was
     13–48% faster than rows when rows ran in sequence with their pixels in parallel
     (`bench_ab.sh`: cornellbox −27%, materialx_basic −25%, teapot −19%, ptex_quads
     −48%, usdlux −13%): a tile is a coherent, cache-friendly work unit, and the rows
     paid a fork/join barrier each.
   - `render()` — scanline rows as the work unit, rows in parallel (`--scanline`).
     Each worker writes its row of the buffer and of the variance map in place through
     `par_chunks_mut`, so there is no per-row barrier, no lock and no serial copy of
     the image; the borrow checker proves the rows disjoint. That made it 5–14%
     faster (min of 8 interleaved runs at 32 spp: cornellbox −5%, materialx_basic
     −14%, ptex_quads −13.5%) and within noise of the tiles (−1.7%, −0.7%, −9.2%).
     The tiled path still gathers its tiles serially: its copy is one pixel store
     each, far below what timing can see.
   The two are **bit-identical**, guided renders included: the per-pixel work is the
   same, and the tiled path hands a pass's guiding training samples and its variance
   sum (both order-dependent in floating point) on in scanline order. Keep it that way —
   a render mode must be scheduling only.
   Pixel reconstruction (`filter.rs`, `crust:pixelFilter` / `--filter`) is **filter
   importance sampling**, not splatting: each pixel warps its jitter through the
   filter's distribution and weights radiance by `f/p`, keeping every per-pixel
   mechanism (adaptive early-stop, QMC domains, pass blending) intact. The default
   is triangle at radius 1.0; box at radius 0.5 reproduces the historical in-pixel
   jitter bit-identically (`--filter box` when comparing against pre-filter
   renders). Mitchell is the only kind with negative weights.
3. **`trace_path()`** (`tracer/path.rs`, public wrapper `ray_color()`) is the integrator — an
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
     test) — with the indirect clamp off: the default `crust:indirectClamp` of 10
     biases every strategy alike, and `--indirect-clamp 0` restores the unbiased
     estimator (see "Volumes, frame, camera and render settings" in
     `openspec/specs/usd-scene-import/design.md`); route BOTH sides of any new weight through the strategy or emission gets
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
   - **Random-walk subsurface** (`subsurface.rs`, MaterialX `subsurface_bsdf` only).
     A material sample with `ScatterSample::subsurface` set is not a bounce: the
     tracer runs a self-contained walk against the hit's own `geom_id` (Typhoon's
     `RandomWalkSSS`), multiplies the entry record's factor by its throughput, and
     makes the exit the next vertex — shaded on a white `ExitLambertian`, with NEE and
     an ordinary bounce, `prev = None` and no emission of its own. The walk spends no
     depth and records no vertex; its free flights draw from `K_SSS` off the entry
     vertex (stratified on the first step, incidental after). A walk that finds no
     exit ends the path as absorbed. Design and traps: materials `design.md`.
   - **Cutouts** (`Material::opacity`: MaterialX surfaces' `opacity`, native
     `OpenPBR`'s `geometry_opacity` — `crust:openpbr` `geometryOpacity`, PxrDisney
     `alpha` — and a `UsdPreviewSurface` under `opacityThreshold`). A hit on a
     material with `has_cutout` is *present* with probability equal to its opacity
     (`pass_cutouts`, one `pcg::Rng` off `K_CUTOUT` per vertex); otherwise the segment
     carries on along the **same line**: the ray is restarted at the hit (`restarted`,
     still asking `(0.001, ∞)`) and the hit's `t` is added back onto the next one, so
     `t`, the carried medium, the volume regions and the ray cone still measure from
     the segment's origin and the free-flight competition stays exact. A surface passed
     through is no vertex — no depth, no emission, no MIS record — and the previous
     vertex's record meets whatever the segment does reach, so bounce-hit emission
     behind a cutout keeps its weight. The shadow side is its twin
     (`cutout_through`, under `cutout_shadow`): `Π(1 − opacity)` over every crossing,
     deterministic, and 0 at the first opaque hit. The two estimate the same
     visibility, which is what keeps NEE and the bounce side describing one integrand
     (`every_sampling_strategy_agrees_through_a_cutout`, in
     `crust-core/tests/mtlx_surfaces.rs`), and two details keep it so. The opacity is
     **point-sampled** on both sides (`point_sampled` zeroes the footprint): a path
     carries a ray cone and a shadow ray none, so a filtered alpha map answered the
     bounce from a coarser mip level than NEE for the same connection
     (`cutout_opacity_ignores_the_ray_footprint`). And both follow at most 256
     crossings and treat the hit past them as present, so a stack exactly that deep is
     clear to both (`a_cutout_stack_at_the_crossing_limit_is_clear_on_both_sides`).
     The learned light cache's training shadow rays go through `cutout_through` too. Both are `#[cold]` and out of line, and
     gated on `World::has_cutouts` (any material's `has_cutout`, fixed at commit): a
     world without one takes exactly the old code, bit for bit. With one, a shadow ray
     still asks the any-hit query first and walks hit by hit only when it is blocked,
     since an open segment crosses no cutout either. Three traps kept the "no cost
     without a cutout" promise from being free, each measured on cornellbox
     (callgrind, 2 spp): stepping past a hit by raising `t_min` broke the `(0.001, ∞)`
     constant propagation into the kernel, so both walks restart the ray at the hit
     instead (`restarted`, the subsurface walk's trick); an `if` yielding the hit from
     either arm copied the whole `Option<WorldHit>` at every vertex (+0.8%), so the
     hit is patched in place; and the extra code tipped `trace_path` over LLVM's
     inline threshold (+1.4% out of line), so it is `#[inline(always)]` into
     `render_pixel`. What is left is +0.04% against the tree before cutouts (+0.43%
     against that tree with the same attribute, which on its own saved 0.4%);
     `materialx_basic` runs +0.64%, of which 0.13% is the per-leaf rotation check in
     the closure collapse walk. Every sample scene without a cutout renders bit-identically.
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

## QMC sampling through the domain tree

Sampling goes through the **`openqmc`** crate's native domain-tree API (see the workspace
layout above). The integrator (`tracer/path.rs`) threads the sampler *by value* — no stateful
`&mut dyn Sampler`: `render_pixel` builds a root `PathSampler::new(x, y, frame, index)` per
sample (with an extra `new_domain(tile)` so images wider/taller than 256 stay decorrelated,
since OpenQMC's pixel decorrelation tiles at 256), draws the camera dims from a `K_CAMERA`
domain, and hands the root to `trace_path`. Each path vertex derives `path.new_domain(depth)`
and each sampling event a further keyed sub-domain (`K_NEE`, `K_BSDF`, `K_GUIDE`, `K_PHASE`,
…, keys defined atop `tracer/path.rs`); materials draw one 4D block from the `SobolSampler` domain
they are handed. Unbounded/incidental draws — Russian roulette, volume delta-tracking,
carried-medium free flight — use `draw_rnd` or a `pcg::Rng` seeded from a domain
(`domain.rng()`), matching OpenQMC's `drawSample` vs `drawRnd` split. Tests that just need
randomness use `openqmc::pcg::Rng`.

## Known gaps: path guiding

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

## Known gaps: volume regions

- **Volume regions** (`volume.rs`) have no OpenVDB / `UsdVolVolume` import — density is
  homogeneous, procedural fBm noise, or an inline voxel grid authored in the USDA.
  (`openusd-schemas` 0.7 does ship a `vol` feature — `Volume` plus `OpenVDBAsset` /
  `Field3DAsset` views — so this is now an unwritten importer rather than a missing
  dependency; it was the latter through openusd 0.6.) No volume path guiding (volume vertices push
  `train: None`; volume-heavy scenes train the surface field on noisier estimates —
  slower convergence, not bias). One global majorant per region — no coarse max-grid, so
  a high `densityScale` over a large box tracks slowly. Emissive volumes are not
  light-list entries: fire is found only by phase/BSDF-sampled paths (firefly risk near
  bright emission), never by NEE. Carried-medium (subsurface) scatter vertices run no
  NEE. Region overlap uses summed extinction (exact) with a σₛ-weighted phase mixture.

## History: Henyey-Greenstein convention and carried-medium fixes

- **HG convention fix**: `sample_henyey_greenstein` used to apply PBRT's inversion to the
  propagation direction (PBRT's frame is around the *reversed* `wo`), so `g > 0`
  scattered backward. It now scatters forward, matching `hg_phase` (value == pdf, cosθ
  against the propagation direction); the histogram test in `medium.rs` pins the pair.
  The carried-medium estimator was also fixed: it double-counted extinction for
  scattering media (analog free-flight at σ̄ *plus* full Beer-Lambert) — scattering
  interiors (subsurface) render brighter than before, correctly. And bounce-hit emission
  (`next_emit`) is now attenuated by the arriving segment (tinted glass / smoke in front
  of an emitter used to pass emission through undimmed).
