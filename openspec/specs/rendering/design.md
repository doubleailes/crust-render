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
     depth and records no vertex; its free flights and in-walk roulette draw from
     `K_SSS` off the entry vertex (stratified on the first step, incidental after). A walk that finds no
     exit ends the path as absorbed. Design and traps: materials `design.md`.
   - **Cutouts** (`Material::opacity`: MaterialX surfaces' `opacity`, native
     `OpenPBR`'s `geometry_opacity` — `crust:openpbr` `geometryOpacity`, PxrDisney
     `alpha` — and a `UsdPreviewSurface` under `opacityThreshold`). A hit on a
     material with `has_cutout` is *present* with probability equal to its opacity
     (`pass_cutouts`, one `pcg::Rng` off `K_CUTOUT` per vertex); otherwise the segment
     carries on along the **same line**: the ray is restarted just short of the hit
     (`restarted` at `resume_before(t)`, still asking `(0.001, ∞)`, which then begins
     just past it) and the restart's offset is added back onto the next hit's `t`, so
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
     constant propagation into the kernel, so both walks restart the ray instead
     (`restarted`, the subsurface walk's trick) — short of the hit by the 0.001 offset,
     since restarted *on* it that offset stepped over any surface within 0.001 behind
     a cutout and a layered card leaked light
     (`a_surface_just_behind_a_cutout_is_not_skipped`); an `if` yielding the hit from
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
5. **Adaptive sampling**: pixels stop early once they hold the effective minimum
   (`crust:minSamplesPerPixel`, floored at `⌈√spp⌉`), have seen some light, their
   relative standard error is below `crust:varianceThreshold` (0 disables), and no
   still-sampling cross neighbour is more than `crust:adaptiveNeighbourTolerance`
   less converged. Applies to main/final passes, never to guiding training passes.
   See "Adaptive sampling" below for the rounds and the traps.
6. The CLI writes the linear EXR to the `-o` path and a tone-mapped sRGB PNG next to it
   (same path, `.png` extension) — e.g. `-o renders/foo.exr` produces `renders/foo.exr`
   and `renders/foo.png`. Tone mapping and PNG encoding live in `crust-render/src/output.rs`; the engine
   crate only produces the `Buffer`.

## Adaptive sampling

The final pass stops a pixel once the relative standard error of its mean
luminance, `sqrt(var_of_mean) / max(mean, 1e-4)`, is below
`crust:varianceThreshold`, checked every 4th sample from the minimum on. Three
guards sit on top of that test, each the answer to a bias it has (Kirk & Arvo,
SIGGRAPH '91; Tamstorf & Jensen, EGWR '97: when the samples that decide whether
to stop are also averaged into the image, early samples that happen to agree
stop the pixel too soon, worst under indirect light).

- **The all-zero trap.** A pixel whose first `min` samples are all exactly zero
  has a measured variance of zero, passes the test at once and is written black,
  whatever the budget. On ALab frame 1004 (1024 spp, authored minimum 8) that left
  3,124 pixels at exactly 0.0, speckled over the glassware, where most paths
  legitimately carry nothing — shadow rays cannot see lights through a
  dielectric. With adaptive sampling off the speckles fall away as 1/spp (803 at
  32 spp, 136 at 128): a bias of the stop rule, not noise of the integrator. The
  **zero-signal gate** refuses the stop until the pixel has recorded a sample
  with non-zero luminance: its convergence index is `+∞` until then. The gate
  tests `lum_sq > 0`, not `lum_sum`: Mitchell's negative filter lobes can leave
  a lit pixel with `lum_sum <= 0`, whereas a sum of squares is zero exactly when
  every sample's luminance was. Only the all-zero case is degenerate — with `k`
  equal hits among `n` samples the relative error is about `sqrt((1 − k/n)/k)`,
  so a single hit reads as 1 and the stop needs roughly `1/threshold²` hits.
- **The √spp floor.** The effective minimum is
  `max(crust:minSamplesPerPixel, ⌈√spp⌉, 2)`, computed per pass from the pass's
  budget (after `-s`), so an authored 8 at 1024 spp takes at least 32 (Cycles
  uses the same √spp). A *floor* on the authored value, not a default when it is
  unauthored: a √spp default would drop the minimum to 4 at 16 spp, and the
  goldens (`check_images.sh`, 16 spp) rely on the unauthored 32 so that no pixel
  ever stops early — above that, a one-ulp change moves a pixel's budget and
  cascades. Two sample scenes author a lower minimum (`rectlight` 4, `motionblur`
  8) and therefore *do* early-stop at 16 spp; they are the two goldens this change
  moved (rectlight: 76.8% → 21.1% of pixels stopped early, the black penumbra
  pixels being the trap itself; motionblur: 67.3% → 62.9%, 250 pixels held by a
  neighbour). Any later change that touches the stop rule will move them again.
  That the rectlight change is bias removed rather than noise added is the
  CLAUDE.md 1/√N check, against a 4096 spp adaptive-off reference: the old rule's
  RMSE **plateaus** (2.96e-3 → 2.68e-3 → 2.66e-3 at 16 / 64 / 256 spp, relMSE
  3.1e-4 → 2.3e-4 → 2.3e-4) while the new rule's keeps falling (2.68e-3 →
  2.23e-3 → 1.89e-3, relMSE 2.5e-4 → 1.5e-4 → 1.1e-4). The fall is slower than
  1/√N because a pixel stops at 5% relative error by construction — the
  ordinary adaptive floor, not the trap.
- **The convergence index and the cross-neighbour tolerance** (Guerilla's model).
  Each pixel of the adaptive pass carries an f32 index `e = relative error /
  threshold`: below 1 when it passes its own test, `+∞` before it has seen light.
  A pixel stops only if, in addition, none of its four cross neighbours (up, down,
  left, right; no diagonals, no radius) that is *still sampling* has an index more
  than `crust:adaptiveNeighbourTolerance` (default 1) above its own:
  `e_q − e_p ≤ t`. One-sided (a more converged neighbour never holds), absolute in
  index units (both sides are already normalised by the threshold), and stopped
  neighbours never hold — converged or out of budget, more samples beside them
  change nothing, which bounds the cost at the edge of a truly black region. A
  held pixel keeps its own `e`, so holding never chains. A negative tolerance
  skips the comparison entirely: "off" is the per-pixel stop with no dependence on
  neighbour values, and the A/B side. The own-pixel test stays the f64 comparison;
  the rounded f32 index feeds only the comparison between neighbours, so a pixel
  at the boundary decides exactly as it did alone.

**Rounds.** A neighbour comparison is impossible when each pixel runs its whole
sample loop inside its work unit, so the adaptive pass runs in global rounds over
full-frame state (`PixelState`: the accumulators, `taken`, the index, `converged`,
`stopped`, `held` — about 60 B per pixel). A first sweep brings every pixel to the
first check point (the smallest multiple of 4 at or above the effective minimum,
capped at the budget); each round then freezes every pixel's index and whether it
is still sampling into two image-order buffers, decides against that frozen buffer
which pixels stop (`held_by_neighbour`), and traces the next batch — `max(4,
taken / 4)` samples, capped at the budget — for the pixels still active. Nothing writes the buffer while a decision reads it, so no decision
depends on the order the units run in — tiles and scanlines stay bit-identical
with the comparison on (pinned by `tiles_and_scanlines_agree_under_the_neighbour_comparison`).
The batch grows 25% a round (`batch_schedule`: `max(4, taken / 4)`, capped at
the budget), so a 1024 spp pass from 32 runs 16 rounds rather than 248; a pixel
that converges mid-batch overshoots by at most 25% of what it had taken (on ALab
that cost 0.7% of mean spp). The schedule is a pure function of `(spp,
first_check)`, computed once, so the round count and the progress total are
known up front. With a fixed batch of 4 the check points sat exactly where the
per-pixel loop had them, and `t < 0` was proved bit-identical to the pre-change
binary (cornellbox 64 spp, minimum 32: `exr_diff` all zeros, 102,171 pixels
stopped early on both); the growing batch retires that comparison, which had
served its purpose. What stays pinned: the same sample indices whatever the
schedule, "exactly the samples the pixel would take alone" under `t < 0`, and
tiles ↔ scanlines. Work units are tiles or rows as before — rows are now
`width`×1 tiles through the one gather — with one `PathScratch` per rayon
worker (`for_each_init`). Progress reports one tick per unit in the first
sweep, then one per round; an early finish walks the remaining ticks so
`completed` reaches `total` one step at a time, and the counter advances
whether or not a callback is attached.

**Costs.** Truly black pixels now run the full budget, and hold their lit cross
neighbours while they do; pixels beside noise take more samples — that is the
comparison's purpose, and `--stats` prints "adaptive: held by neighbour" so the
cost is visible. Measured 2026-09-30 on ALab frame 1004 at 1024 spp, authored
minimum 8 (a shot layer over `entry.usda`), 640×360:

| rule | exact-zero pixels | stopped early | mean spp | held | single run |
|------|------------------:|--------------:|---------:|-----:|-----------:|
| before this change | 7,233 (3.1%) | 21.4% | 925.9 | — | 185 s |
| `t = −1`, batch 4 (gate + floor only) | 0 | 17.9% | 961.7 | 0 | 209 s* |
| `t = 1`, batch 4 | 0 | 8.7% | 992.1 | 23,472 (10.2%) | 206 s |
| `t = 1`, growing batch (shipped) | 0 | 6.1% | 998.7 | 19,476 (8.5%) | 203 s |

The pixel counts are exact. The last column is one un-interleaved run each,
minutes apart (*and that one overlapped a goldens check), so it says only that
the rule's cost is of the order of the extra samples — not a timing comparison,
which per CLAUDE.md needs `bench_ab.sh`; the interleaved numbers are the ones
below. The black speckle
over the glassware is gone at either tolerance; what remains on the glass is
caustic fireflies through the dielectric, integrator noise. The samples the
guards spend are the 4–8% of mean spp above, of which the growing batch's
overshoot is 0.7%.

**Why the default tolerance is 1.** A sweep on the same ALab frame (1024 spp,
min 8), each render diffed against an adaptive-off 1024 spp render of the same
frame, so the error is what stopping cost at that tolerance:

| `t` | stopped early | held | mean spp | saved | RMSE vs off | relMSE |
|----:|--------------:|-----:|---------:|------:|------------:|-------:|
| −1 (per-pixel) | 13.9% | — | 975.7 | 4.7% | 1.30e-2 | 4.9e-4 |
| 0 | 1.0% | 13.5% | 1018.2 | 0.6% | 1.56e-3 | 7.2e-6 |
| 0.5 | 5.3% | 9.5% | 1003.2 | 2.0% | 4.83e-3 | 6.7e-5 |
| **1** | 6.1% | 8.5% | 998.7 | 2.5% | 4.98e-3 | 7.7e-5 |
| 2 | 7.1% | 7.1% | 995.1 | 2.8% | 5.52e-3 | 9.2e-5 |
| 4 | 10.2% | 4.7% | 988.4 | 3.5% | 9.61e-3 | 2.0e-4 |

The per-pixel stop saves twice the samples of `t = 1` for 6.4× the error, and
its glass still carries *dark* (not zero) speckle: pixels that saw one or two
dim hits and passed their own test — the sparse-but-non-zero case the gate
alone cannot catch, and the reason the comparison exists. `t = 0` holds so
much that it saves nothing. Between 0.5 and 1 the error is flat; from 2 it
rises faster than the savings, and at 4 it has nearly doubled. So 1 sits at
the knee: neither eager nor lax on this scene. Note the savings themselves are
small here — at threshold 0.05 few pixels of a glass-heavy dome-lit frame
converge within 1024 samples, and all six renders took 198–206 s — so the
tolerance decides *what the few stopped pixels look like*, not the render
time.

The rounds themselves cost time even when they change nothing. `bench_ab.sh`,
pre-change binary against `t = −1`, min / mean of interleaved runs, cornellbox
at 64 spp (bit-identical output at batch 4, so pure scheduling): **+8.1% /
+8.9%** at a fixed batch of 4, **+7.0% / +4.8%** with the growing batch (4
rounds instead of 8); ALab at 64 spp (where the zero gate also fires): +6.6% /
+13.6% at batch 4, −7.6% / −0.8% with the growing batch — noise. It is not
work: callgrind (`RAYON_NUM_THREADS=1`, cornellbox 64 spp, `t = −1`, batch 4)
counts 112.72 G instructions for the rounds against 113.32 G before, every
integrator function identical to the instruction. It is the fork/join tail of
each round: the last unit of a round leaves every other worker idle. Halving the
round count barely moved cornellbox because the tail is proportional to one
unit's work *in that round*, so summed over the pass it is about one unit's
share of everything traced after the first sweep, whatever the batch schedule —
growing batches only save the fixed per-barrier cost. What would shorten it is
smaller work units in the rounds (an 8×8 tile's tail is a quarter of a 16×16
tile's), not fewer rounds; not done, since on the production-sized scene the
rounds already measure as noise and on cornellbox the 5–7% is a 0.5 s render.
Skipping units with no active pixel does not touch the tail either.

The full-frame state is the other cost: `PixelState` is 64 B (`Vec3A`
alignment), plus 5 B of index/active buffers, per pixel. At 3840×2160 the render
phase's peak RSS went from 442 MB to 755 MB (cornellbox, 8 spp) — about +500 MB,
as the design's estimate predicted. If it ever matters, `sum` as an unaligned
`[f32; 3]` and dropping `samples_end` outside training passes bring it to 48 B.
Checked and holding elsewhere: tiles ↔ scanlines are `exr_diff`-identical on
cornellbox at 128 spp with `t = 1`, and on `cornellbox_guided.usda` (training
passes non-adaptive, the final pass adaptive under the guiding field); Mitchell,
the one filter with negative weights, renders rectlight with a finite error and
the same black half as the triangle filter, which is what the `lum_sq` gate is
for.

Traps already fallen into: the per-pixel loop hid the all-zero stop for as long as
adaptive sampling existed, because a black pixel in glass looks like a shadow; and
the rounds' progress counter must advance whether or not a callback is attached,
or the walk-to-total loop never ends on a render without one.

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
