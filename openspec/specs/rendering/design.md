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
  image bit-identical. The workspace requires `0.2.4`, so no lock file can
  resolve an older one. Profile the sampler before assuming a cost is the BSDF's — `draw_block`
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
     (`cutout_through`, under `shadow_transmittance`): `Π(1 − opacity)` over every crossing,
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
     (`a_surface_just_behind_a_cutout_is_not_skipped`) — and that restart assumes the
     kernel's `t` is accurate to better than its 1e-5·t step, which the analytic
     sphere's was not (below); an `if` yielding the hit from
     either arm copied the whole `Option<WorldHit>` at every vertex (+0.8%), so the
     hit is patched in place; and the extra code tipped `trace_path` over LLVM's
     inline threshold (+1.4% out of line), so it is `#[inline(always)]` into
     `render_pixel`. What is left is +0.04% against the tree before cutouts (+0.43%
     against that tree with the same attribute, which on its own saved 0.4%);
     `materialx_basic` runs +0.64%, of which 0.13% is the per-leaf rotation check in
     the closure collapse walk. Every sample scene without a cutout renders bit-identically.
   - **A pass-through crosses each surface once** (`LastCrossing`, in every walk on
     both sides). A hidden sphere light 8 units from a diffuse plane was collected
     twice on a third of the bounces that crossed it: the sphere's textbook
     discriminant `half_b² − a·c` cancelled two terms of order |oc|² to leave one of
     order r², and the reported `t` was 1e-4 to 2e-3 short of the surface — more than
     `resume_before`'s 1e-5·t step past it — so the restarted segment met the entry
     again, front-facing and emitting (BSDF-only 1.04×, 1.34× and 1.64× light-only at
     distance/radius 40, 160 and 200; veach_mis's smoothest plate 16.6% bright under
     LightTiny alone). The kernel is fixed (closest-approach discriminant for spheres
     and cylinders, intersection-kernel design record), and the walks keep a backstop
     for every primitive: each records the crossing it last accepted
     `(geom_id, prim_id, placement, side, t)`, and a hit on the same surface, from
     the same side, within `1e-3 · max(|t|, 1)` of it is its rounding, not a surface
     — no closed or single-sided surface is entered twice in a row from the same side
     — so the walk steps past it without counting it, and without spending a crossing
     of the budget. The guard alone also covered the old sphere at these distances
     (`every_strategy_agrees_on_a_small_far_hidden_light`, which is 2.000× without
     either). It is not only spheres: on `materialx_showcase` (hidden rect lights, no
     sphere) the guard skips some eighty re-hits of a light's triangle per 16-spp
     frame, reported 1e-5 to 1.9e-4·t past the first hit (2.5e-4 on
     `openpbr_showcase`) — watertight triangles lose that much at grazing incidence —
     and the image moves by 1e-6 on 32 of 2 M pixels. Widening `resume_before`
     instead would re-open the skipped-surface leak above. The surface identity
     carries the kernel's `placement` (`RayHit::placement`, an identity per instance
     placement chain) because `(geom_id, prim_id)` alone is not one surface: inside a
     nested prototype every placement of a leaf part reports one id
     (`InstanceHitId::As`), and a second instanced card stacked within the window
     read as the first met again and was passed for free
     (`stacked_placements_sharing_an_id_are_both_crossed`, half a millimetre apart:
     one card's attenuation instead of `(1 − 0.5)²`). Two cards on different
     primitives that close are both crossed too
     (`two_cards_close_together_are_both_crossed`).
   - **Thin walls are pass-throughs** (`Material::has_straight_transmission`:
     `crust:openpbr` `geometryThinWalled` with `transmissionWeight > 0`, a MaterialX
     surface whose `thin_walled` reaches a transmitting dielectric or generalized
     Schlick leaf, a `UsdPreviewSurface` over a thin-walled base). A thin wall's
     transmission is a delta lobe that keeps the ray's direction, so it is a cutout
     with a coloured pass, which is exact in crust's model: the BSDF splits as
     `f = f_rest + T(ω)·δ(straight)`, and with opacity `α` the fraction of a ray
     continuing straight is `P = (1 − α) + α·T`. That is Typhoon's
     `_CombinePresenceAndTransmissionVisibility` (hdEmbree on OpenUSD `typhoon/main`
     70c45e8, `integrator/visibility.cpp`: "thin-walled transmission always uses
     straight RGB shadow attenuation"). The shadow side multiplies `P` over every
     crossing (`walls_through`, behind `cutout_through`); the bounce side passes with
     `q = max_c P_c` carrying `P / q`, and otherwise meets the wall carrying
     `α / (1 − q)`, to scatter through `f_rest` alone (`pass_walls`, behind
     `pass_cutouts`, draws off `K_THIN` per crossing). For a grey `P` that is the
     cutout rule exactly. A world with thin walls takes `pass_walls`/`walls_through`,
     one without takes the cutout loops verbatim, and one with neither neither.
     - **`T` comes out of the resolved BSDF** (`ShadingPoint::straight_transmittance`),
       so it is the one the vertex would shade with. Native OpenPBR returns the window
       model's delta value before its selection compensation (`tint^(1/cos θt) ·
       (1 − R)/(1 + R) · transmission_weight`, `straight_transmittance_is_the_delta_samples_weight`).
       A MaterialX thin leaf transmits `tint·(1 − F(v·h))` at a microfacet normal its
       VNDF draws, and the mean of that over the microfacets has no closed form for a
       rough leaf: `T` is an **unbiased one-draw estimate** (`K_NEE_THIN` per shadow
       crossing, `K_THIN` on the bounce side). The pass stays unbiased with a random
       `T`, since `q` is a function of the same draw: `E[1{u<q}·P/q | T] = P(T)`, and
       the meet carries `α f_rest` whatever `T` was drawn.
     - **A met wall scatters without its straight lobe** (`ShadingPoint::exclude_straight`).
       Native OpenPBR drops the thin transmission's selection mass
       (`LobePmf::selecting::<false>`); the closure drops its transmission-only thin
       leaves and samples a thin `RT` leaf as `R`. Both renormalise, and `eval`'s pdf
       follows, so NEE at a met wall weighs against the sampling the bounce does
       (`the_reduced_lobe_set_samples_what_eval_reports`); `T` plus the rest's albedo is
       the whole BSDF's (`straight_transmittance_plus_the_rest_is_the_whole_bsdf`, and
       its closure twin on `open_pbr_surface` and `standard_surface`).
     - **Everything past a wall carries its pass weight, nothing before it does.** With
       volume regions, the segment is sampled a piece at a time between the walls
       (`volume_event_past_walls`; delta tracking is memoryless, so the pieces are the
       same process as the whole), each piece's transmittance, emission and scatter
       weight carrying the walls before it. The meet weight is folded into the
       transmittance of a segment that reaches its wall, not applied at the vertex.
     - **Events and AOVs.** A thin-wall pass is a `TS` `'transmission'` event, the event
       its delta vertex was, so `C<TS>…` keeps its meaning; a cutout pass stays `Ts`.
       A segment with walls names its events in order (`Route::arrival_through`).
       `C.*[LO]` stays the beauty bit for bit through a window
       (`the_full_path_expression_is_the_beauty_bitwise`, `light_through_a_window_is_a_specular_transmission`).
       The data AOVs keep the first wall passed as the camera's first hit (`first_wall`;
       glass is not a hole: `a_thin_window_is_the_first_hit_whether_passed_or_met`),
       with its geometric normal, and the albedo carries the pass weights as it carried
       the delta sample's throughput. The learned light cache trains on the luminance of
       `P` (the value itself where `P` is grey, so a cutout world trains as it did).
     - **Measured** on `samples/thin_window.usda` (a sphere light over a tinted native
       pane and a MaterialX window, `--indirect-clamp 0`, adaptive sampling off), against
       an 8192-spp reference at another seed: relMSE before 141 / 37.3 / 8.72 / 1.99 at
       16 / 64 / 256 / 1024 spp (0.067 at 16384), after 2.3e-4 / 5.6e-5 / 1.4e-5 /
       3.9e-6. Both fall as 1/N with no plateau, and the image means agree (before at
       16384 spp 0.21506 / 0.12728 / 0.05929, after 0.21477 / 0.12709 / 0.05919; light-only
       0.21477 and BSDF-only 0.21481 after). A sample costs 1.35–1.45× here (shadow rays
       walk the sheets; a MaterialX wall runs its program per crossing), so at equal time
       after is ~5·10⁵ lower in relMSE (64 spp in 0.44 s against before's ~27 at that
       time). Before, light-only rendered exactly the power-MIS image: NEE found nothing,
       and every bit of light came from bounces through the delta sheet, which carry full
       weight under any strategy. The default firefly clamp also darkened it, since through
       a sheet the light is a deeper vertex.
     - **Cost without thin walls** (callgrind, 2 spp, one thread): cornellbox +0.30%,
       `materialx_cutout` +0.42%, `aovs_lpe` +0.42%. What is left is the per-vertex
       `walls` flag, two branches and the registers they hold. Each trap below was measured
       on the way: both `ResolvedOpenPBR` instantiations in one match arm un-inlined
       `ShadingPoint::scatter_importance` (+0.5%, so the reduced side is `#[cold]`);
       `lobe_spread` went out of line (+0.1%, now forced); `t_surf.min(t_med)` hoisted to
       every vertex (+0.2%, now computed where used); returning the shading point from an
       `if` to exclude the straight lobe copied it at every vertex (+1.6%, so it is
       patched in place); one loop for cutouts and walls cost `materialx_cutout` 0.7%,
       so the two are apart; and walking an empty event slice at every arrival cost the
       LPE gather 1.3%. Every sample scene without a thin wall renders bit-identically.
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
   - **Medium boundaries** (`tracer/path.rs`, `Enclosure` / `cross_boundary`): a
     volume-only material (materials record § MaterialX volume terminals) is a closed
     mesh whose interior medium the path travels in — Typhoon's
     `_UpdatePathMedium` / `_TraceVolumeTransmission` / `_Visibility`. A segment that
     hits one is **not a vertex**: no depth, no record, `prev` untouched. The
     stretch up to the hit is *carried* (`carry`: its transmittance and pre-weighted
     region emission) and folded into whatever the next segment reaches (`carried`,
     on the `VolumeEvent` before the branches), and the ray restarts just past the hit
     (`restarted_past`) in the medium the crossing leaves it in. The rules are
     Typhoon's single-owner ones: a front face met in vacuum enters the boundary's
     medium and records its `geom_id` as owner; only that owner's back face leaves;
     inside, other boundaries are crossed unchanged; a ray already carrying a
     refracting interior enters nothing. While inside, **every** ray the path traces
     carries the enclosure's medium — a surface's own scattered ray (which knows
     nothing of it) is overridden, the random walk's exit too — so an opaque object
     in fog keeps its fog. Each stretch draws from its own vertex domain, numbered
     `records.len() + crossings` (which grows at every step, vertex or crossing, and is
     the plain vertex index when nothing was crossed); reusing the vertex's domain
     made the stretch after a crossing repeat the free-flight draw of the one before. A carried-medium scatter inside
     an enclosure runs **NEE with MIS** through `volume_nee` with the single HG lobe,
     taking the vertex's light-sample count as a region scatter does, and leaves `PrevVertex::Phase` — the same pair as a region scatter — with the
     event weight moved into `atten` so it multiplies NEE; outside one it keeps the
     old no-NEE, `prev = None` pairing (a refracting interior's shadow rays are
     blocked anyway). Shadow rays take the enclosure they start in
     (`shadow_transmittance(.., inside, ..)`); an occluded one in a world with
     boundaries goes to `medium_shadow` → `through_boundaries`, which walks hit by hit with
     `cross_boundary`'s rules, Beer–Lambert per stretch, cutouts and thin walls as
     their `P = (1 − α) + α·T`, anything else blocking — deterministic where the path tracks free flights, the
     same transmittance either way. Its boundaries count against the path's own
     `MAX_PATH_CROSSINGS` (4096), its cutouts and walls against `MAX_CUTOUT_CROSSINGS`
     (256): on one shared count of 256, a light behind 150 fog slabs — 300
     crossings, which any path makes — was blocked to every shadow ray
     (`shadow_rays_cross_as_many_boundaries_as_a_path`). The depth-exhausted emission
     lookup passes boundaries with `pass_boundaries`, on the same 4096. The light
     cache's training visibility sees a boundary as clear (a choice: the cache only
     steers selection, and region media are left out of it too), and its training
     rays cross boundaries rather than shading them: shaded, the boundary's empty
     closure trained a receiver that saw no light and ended the path, so nothing
     inside or behind the fog was trained
     (`training_crosses_a_medium_boundary_to_the_wall_behind`). A world with no boundary renders every sample
     bit-identically, and at +0.04% instructions on cornellbox (callgrind, 2 spp):
     the integrator is monomorphised on `MEDIA` (`advance_pixel` / `trace_path` /
     `shadow_transmittance` / `volume_nee`), chosen per render from
     `World::has_medium_boundaries`, so that world runs a copy with none of the
     boundary branches. Guarded at run time instead, every check was cheap and
     together they cost +0.7% once `main` grew the loop further (`ShadingPoint::new`
     and the `VolumeEvent` rebinding were each pushed out of registers or out of
     line). Two traps on the way: making `volume_nee` generic over its phase closure
     gave `LightList::pick_index_at` another call site and LLVM stopped inlining it
     (+0.6%) — the `Phase` enum keeps one body — and moving the loop's medium state
     into `PathScratch` measured worse than locals.
     Pinned by `tests/volume_materials.rs`: Beer–Lambert through an absorbing
     boundary exact to the restart epsilon (σ·0.001, for unit and ×10 directions),
     and white furnaces at 1 within 0.025 for a scattering boundary under power
     MIS, NEE alone and BSDF alone, and with a white Lambertian ball *inside* the
     fog. Measured against the same medium as a homogeneous `crust:volume` region
     (a mesh cube vs the region box, floor and ball inside, 512 spp): image means
     agree to 0.3% in the band where camera rays graze the box top, ≤0.06%
     elsewhere — the restart epsilon.
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
   consistent or emission gets double-counted. The quadtree descent draws a fresh pair
   per level from the `K_GUIDE` domain's `rng()`; it used to hash the guide seed into a
   hand-rolled PCG32 outside `openqmc`. Switching streams changed `cornellbox_guided`'s
   noise only: against the old stream, relmse 3.1e-2 / 2.1e-2 / 7.6e-3 at 16 / 64 /
   256 spp (`--indirect-clamp 0`), no plateau, and both stand exactly as far from an
   unguided 256-spp reference.
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
   and `renders/foo.png`. Tone mapping and PNG encoding live in `main.rs`; the engine
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
binary (cornellbox 64 spp, minimum 32: `crust diff` all zeros, 102,171 pixels
stopped early on both); the growing batch retires that comparison, which had
served its purpose. What stays pinned: the same sample indices whatever the
schedule, "exactly the samples the pixel would take alone" under `t < 0`, and
tiles ↔ scanlines. Work units are tiles or rows as before — rows are now
`width`×1 tiles through the one gather — with one `PathScratch` per rayon
worker (`for_each_init`). Progress counts samples, not units: each unit has
`min(spp, 64)` steps (`PROGRESS_STEPS`), and a unit finishing a stage of the first
sweep or a round reports the share of them that stage or round scheduled —
`taken · steps / spp`, so one a sample up to 64 spp. A step per unit per stage let the
first stages (1, 1, 2, 4, … new samples) race through the bar and fooled the CLI's
ETA. The cap is there because reports stay one at a time and +1 each, every one a
callback under the progress lock: one a sample would let `-s 4294967295` (or an
authored `crust:samplesPerPixel = -1`) spend days walking callbacks. The last stage or
round always targets `spp`, so the shares telescope and `total` is units ×
min(spp, 64), known before the pass starts. An early finish walks the rest in one call;
without a callback the counter just adds. A unit reports only while the render runs, so
a cancelled render's progress stays where it stopped. The steps follow *scheduled*
samples: a round reports its share for a unit whose pixels have all stopped (it is
instant), so an adaptive render's bar runs ahead in late rounds and the ETA corrects
itself; and a guided render's training passes report nothing.

**Staged first sweep.** The first sweep does not take a pixel to the first check
point in one advance: it runs in stages of 1, 2, 4, … spp up to it (`sweep_stages`),
each over the whole region, so a watched render shows the full frame at 1 spp first
(§ Progressive output and cancellation). Scheduling only, and pinned bitwise against
the unstaged sweep. Its cost is one more `advance_pixel` call per pixel per stage,
and that call has a fixed price: LLVM hoists ~200 instructions of loop-invariant setup
(renderer fields, constants, spills) out of the inlined path loop to the function's
entry, 216 instructions a call by callgrind's per-instruction counts. On cornellbox
(9 k instructions a sample, the cheapest scene here, `RAYON_NUM_THREADS=1`) that is
**+1.61%** at 2 spp (stages 1, 2: 4.2439 G → 4.3124 G) and **+0.29%** at 32 spp (six
stages; a 160×90 crop, 7.0492 G → 7.0697 G), images identical; the share falls with
the cost of a sample and with the budget past the first check. `bench_ab.sh`:
10 interleaved runs per scene (min / mean, the
change against its parent, the CLI attaching a cancel-only control), cornellbox at its
default 128 spp **−1.9% / +1.8%**, `usdpreview_textured` (64 spp, textured) **−3.1% /
+1.1%** — noise, the signs disagreeing. If it ever matters, starting the list at 2 or 4 spp is still
bit-identical (it changes the spec's stage list, not the image), and moving the pixel
loop inside the monomorphised integrator would pay the setup once per unit instead.

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
Checked and holding elsewhere: tiles ↔ scanlines are `crust diff`-identical on
cornellbox at 128 spp with `t = 1`, and on `cornellbox_guided.usda` (training
passes non-adaptive, the final pass adaptive under the guiding field); Mitchell,
the one filter with negative weights, renders rectlight with a finite error and
the same black half as the triangle filter, which is what the `lum_sq` gate is
for.

Traps already fallen into: the per-pixel loop hid the all-zero stop for as long as
adaptive sampling existed, because a black pixel in glass looks like a shadow; and
the progress counter must advance whether or not a callback is attached, or a
walk-to-total that loops until the counter reaches the total never ends on a render
without one (the walk is one call now); and a progress step per sample, uncapped,
made a huge budget's early finish walk billions of callbacks.

## Render regions

A render can trace a rectangle of the frame only (`RenderSettings::region`, a
`PixelRect`; set from `dataWindowNDC` or `--region`). It is built so that a crop
renders *exactly* the pixels the full frame would, because the diagnostic trial
renders (`add-diagnostic-command`) are only meaningful on that condition:

- **Nothing derived from the resolution moves.** The camera, `Camera::pixel_span`
  (ray cones), adaptive subdivision's screen rate and frustum culling, the learned
  light cache and every sampling key keep the full frame. Shrinking the resolution or
  the camera window instead would have changed all of them.
- **Only the work units change.** `generate_tiles` / `generate_rows` take the region
  (raster space). Tiles keep the *frame's* 16-pixel grid, clipped to the region, so a
  pixel sits in the same tile, at the same place, in the same scanline-replay order as
  in a full render; over the full frame the generators emit exactly the old units
  (pinned by `a_full_frame_region_yields_the_frame_tiles_and_rows`).
- **Everything per pixel is region-sized** — `Buffer`, `AovFilm`, the variance map,
  the convergence-index and active planes, the guiding reference luminance — and is
  indexed through the one `PixelRect::index`, so an offset region cannot be read with
  the frame's stride.
- **Two coordinate spaces.** The region is stored in image space (top-left origin, as
  `--region`, a viewer and the EXR data window count). The tracer's `(i, j)` are raster
  coordinates with row 0 at the *bottom* (the camera's `v = 0`), so it works on
  `settings.raster_region()`, the region's `flip_y`. `Buffer::get_rgb` is top-down over
  the region; `get_pixel` / `set_pixel` take frame raster coordinates.

Bit-identity holds whenever a pixel's sample count does not depend on its neighbours —
a fixed count (`-s 16` with the default minimum of 32), or adaptive sampling with a
negative `crust:adaptiveNeighbourTolerance` (pinned in `crust-core/tests/region.rs`
for the beauty, an LPE, depth and `sampleCount`, and tiles ↔ scanlines on a region).
Two exceptions, by design:

- **The neighbour hold.** A neighbour outside the region is never sampled, so it is
  *absent* (`held_by_neighbour` checks the region's bounds, as it checked the frame's):
  a border pixel that a still-sampling outside neighbour would have held stops earlier
  than in the full frame.
- **Path guiding** trains on the region's paths only; the field, and therefore a guided
  crop, differs from a guided full render. A guided render is not bit-identical across
  schedules anyway, and a field trained on the region is arguably the better one for it.

A full-frame region is the old render: same units, same planes, same order. Verified
with `scripts/check_images.sh check` against goldens of the parent commit and with the
zero-AOV callgrind count (see the change's tasks).

## QMC sampling through the domain tree

Sampling goes through the **`openqmc`** crate's native domain-tree API (see the workspace
layout above). The integrator (`tracer/path.rs`) threads the sampler *by value* — no stateful
`&mut dyn Sampler`: `render_pixel` builds a root `PathSampler::new(x, y, frame, index)` per
sample (with an extra `new_domain(tile)` so images wider/taller than 256 stay decorrelated,
since OpenQMC's pixel decorrelation tiles at 256), draws the camera dims from a `K_CAMERA`
domain, and hands the root to `trace_path`. Each path vertex derives `path.new_domain(depth)`
and each sampling event a further keyed sub-domain (`K_NEE`, `K_BSDF`, `K_GUIDE`, `K_PHASE`,
…, keys defined atop `tracer/path.rs`); materials draw one 4D block from the `SobolSampler` domain
they are handed. With several light samples at a vertex (`crust:lightSamples` /
`crust:lightSamplesIndirect`), the first sample draws its pick, point and shadow ray
off `v` itself, as the one sample always did, and sample `i ≥ 1` off
`v.new_domain(K_NEE_SAMPLES).new_domain(i)`; each pick coordinate is stratified by
hand to `(i + u) / N`
(`stratified_pick`), since the pick is a monotone CDF inversion and the point
dimensions must stay independent of the slice (lighting's design record, "Several
light samples per vertex"). Unbounded/incidental draws — Russian roulette, volume delta-tracking,
carried-medium free flight, the guide's quadtree descent — use `draw_rnd` or a `pcg::Rng` seeded from a domain
(`domain.rng()`), matching OpenQMC's `drawSample` vs `drawRnd` split. Tests that just need
randomness use `openqmc::pcg::Rng`.

## Progressive output and cancellation

A render can be watched while it runs and stopped (`RenderControl`, `RenderOutcome`,
`tracer/control.rs`; change `progressive-cancellable-render`). It was built in the engine
first, with the CLI as its first consumer (Ctrl-C and `--checkpoint`, `cli` design record),
so the API is honest before any Hydra or FFI code depends on it.

- **Staged first sweep (D1).** See § Adaptive sampling: stages of 1, 2, 4, … spp, the
  convergence test only after the last (at the `taken` an unstaged sweep tests at — a
  `finish_round` at `taken = 2` could set `converged`, which no stop rule reads before
  the first round, but keeping it out avoids the question). Always on for final passes:
  one code path, so the bitwise test (`a_staged_sweep_renders_the_unstaged_one_bit_for_bit`:
  adaptive, adaptive off, AOVs plane by plane, guided; tiles and scanlines) exercises
  what users run. `Instruments::unstaged` exists for that test alone — not a switch,
  since there is nothing to A/B. Training passes stay unstaged: each pixel's
  `SampleData` must sit contiguously in its unit's buffer for the scanline-order replay
  the SD-tree needs. One quantity was not keyed on the sample index: the clamp counter
  summed a pixel's removals per `advance_pixel` call and then added that sum, so
  splitting an advance re-associated the f32 sum. It now sums per sample, which makes it
  independent of every schedule (stages and rounds alike); the diagnostic's clamp
  figures moved in their last bits, once. The learned light cache is built before the
  passes and read-only during them; texture caches change timing, not values.
- **Per-unit publish (D2).** After each stage or round in which it traced anything, a
  unit writes its pixels' estimates into the control's region-sized `Buffer` under one
  `Mutex` and bumps an `AtomicU64` generation under the same lock, so a snapshot's
  image and generation agree. Not round-boundary snapshots: a late round of a 1024 spp
  render is ~200 spp a pixel over the whole frame, and the viewer would freeze for it.
  A reader clones the buffer under the lock (O(pixels), fine at checkpoint rates). The
  last snapshot of an unguided render that completed is the returned image, bit for bit
  (`the_last_snapshot_is_the_returned_image`).
- **One caller-owned control (D3).** `RenderControl::new()` / `without_snapshots()`,
  `cancel`, `is_cancelled`, `generation`, `snapshot`; `Renderer::render_with_control(tiled,
  progress, request, &control) -> Rendered { buffer, film, rays, outcome }`. The other
  `render*` methods go through the same `render_impl` with no control and keep their
  signatures. One control per render (cancel is sticky; a restart takes a fresh one).
  Not a callback pushed from the workers — Hydra pulls, and user code would run on rayon
  workers inside the hot loop — and not a session owning a thread: threading is the
  host's policy. A control `without_snapshots` publishes nothing: the CLI takes one
  unless `--checkpoint` asks for previews, since each publish costs ~60 instructions a
  pixel (a 0.66% of cornellbox at 2 spp that nobody would read).
- **Cancel per pixel (D4).** Each pixel's advance first reads the flag (one relaxed
  load; nothing without a control), so latency is bounded by the advances in flight,
  not by a unit (a whole row under `--scanline`) or a round (~200 spp a pixel). Between
  stages and rounds the driving thread stops scheduling. A pass is *interrupted* when a
  unit skipped a pixel or the driver a stage or round; a cancel landing after the last
  advance leaves the render `Completed`. The gather then runs as usual: pixels of one
  unit may differ in `taken`, which the per-pixel estimator handles. The progress
  callback is not walked to the total, and `RayStats::early_stopped` counts only pixels
  the adaptive rule stopped (`stopped && taken < spp`, which every pixel of a completed
  pass satisfies exactly when it did before) — a pixel the cancel left is not "stopped
  early". The counters cover exactly the samples traced (`camera_rays ==
  adaptive_samples`), and a render cancelled after its 4 spp stage is the 4 spp render,
  image, AOVs and counters (`a_render_cancelled_after_a_stage_is_that_stage_everywhere`).
- **A pixel with no sample is zero (D5).** `PixelState::estimate` returns `(0, 0)` at
  `taken == 0` instead of `0 / 0`, and `AovFilm::store` leaves every plane at its clear
  value. Unreachable in a completed render (every pixel takes at least one sample), so
  nothing there moves.
- **Guided renders (D6).** Every pass publishes (training ones included), so the
  display shows the pass in progress over the previous one. On cancel no further pass
  starts and the field is never consulted again (an interrupted training pass's samples
  never reach `field.update`). The render blends the completed passes, plus the
  interrupted one when every pixel in it took `≥ 2` samples — fewer has no variance
  estimate and would skew the pass weight — or returns the interrupted pass alone when
  none completed. The blends (`blend_passes`, `AovFilm::blend`) run unchanged.
  `a_guided_render_cancelled_in_its_final_pass_blends_what_it_can_weigh` cancels from the
  progress callback (deterministic: only the final pass reports) after the 4 spp stage
  and within or after the 1 spp stage; the latter two give the same training blend, bit
  for bit.

## Known gaps: progressive output and cancellation

- **Progressive AOVs.** Only the beauty is published while the render runs; the AOVs
  are gathered once, when it returns (cancelled or not).
- **The guided preview gets noisier when the final pass starts.** Its 1 spp stage
  overwrites the last training pass's units with a noisier estimate, until the final
  pass catches up. Accepted for now.
- **Not everything can be cancelled.** The import, `Renderer::new` / `reconfigure`
  and the `learned` light selection's pre-pass never read the flag. The CLI exits at
  once (status 130) on a Ctrl-C there; a library caller waits for them.
- **An interrupted guided render's `crust:sppTaken`** describes its final pass alone —
  the adaptive counters cover nothing else — and is `(0, 0)` when the image holds no
  final pass: stopped in training, or before the final pass gave every pixel two samples
  (the pass is left out of the blend, and `RayStats::forget_adaptive` drops its counters
  with it, so neither the stamp nor the CLI's warning describes a pass that is not in the
  image; `SamplingStamp::for_outcome` stamps `(0, 0)` rather than `new`'s
  no-counters fallback, the budget). The image's training samples are not counted there.
  `crust:renderStatus = "interrupted"` says the frame is partial either way.
- **A completed guided render at 1 spp has the same mismatch, before this change too.**
  Its final pass has no variance estimate, so the blend gives it no weight and the image
  is the training passes', yet its counters (and `crust:sppTaken = (1, 1)`) describe it.
  Only the cancelled case clears them.
- **Progress counts scheduled samples, capped at 64 steps a unit** (§ Adaptive sampling,
  "Rounds"): adaptive renders run ahead in late rounds, and guided training passes are
  silent.
- **The per-stage call cost** (§ Adaptive sampling, "Staged first sweep"): +1.6% of
  cornellbox's instructions at 2 spp, +0.3% at 32, for nobody watching. Paid by every
  final pass, with or without a control.
- **The snapshot is a clone under the lock workers publish through.** Fine at
  checkpoint rates (at 4K, ~100 MB/s at 1 Hz); a 60 Hz viewport will want the double
  buffer D2 leaves room for, without an API change.

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
  thin-walled transmission remains a delta lobe (`ScatterSample::delta`), excluded
  from continuous mixtures — carrying window-model energy (`(1−R)/(1+R)`
  transmittance, boosted `2R/(1+R)` reflection, view-dependent tint) — which the
  integrator takes over as a pass-through (above), so the guide never sees it. The guide-vs-BSDF selection probability is fixed (no learned α), and
  spatial lookups are not parallax-compensated.
- **Guided renders of ALab are darker than unguided ones.** `crust diagnostic`'s
  picture check flagged it (`diagnostics` design record, "Calibration"), and a direct
  test confirmed it: a 272×272 crop of frame 1004 at 64 spp, clamp and adaptive
  sampling off, 40 seeds each, read 0.4294 ± 0.0055 guided against 0.4611 ± 0.0046
  unguided — 6.9% darker, z −4.4 on the standard error across seeds. Not yet
  diagnosed ([#244](https://github.com/doubleailes/crust-render/issues/244)). Two
  suspects: the pass blend, whose weights are each pass's *estimated* mean variance,
  so a pass that caught a firefly is down-weighted along with the firefly's energy (a
  known bias of weights estimated from the data they weigh, largest where fireflies
  carry the image, as on ALab) — 8 of 9 guided renders darkened with an *unguided*
  final pass, and one gave a 2-spp training pass 69% of the weight; and the guide
  mixture ↔ NEE pair. The Cornell box and `veach_mis` show no such shift (|z| < 2 at
  64–128 spp).

## Known gaps: volume regions

- **Volume regions** (`volume.rs`) have no OpenVDB / `UsdVolVolume` import — density is
  homogeneous, procedural fBm noise, or an inline voxel grid authored in the USDA. (A
  homogeneous medium inside an arbitrary closed mesh is a volume *material*; § Medium
  boundaries above.)
  (`openusd-schemas` 0.7 does ship a `vol` feature — `Volume` plus `OpenVDBAsset` /
  `Field3DAsset` views — so this is now an unwritten importer rather than a missing
  dependency; it was the latter through openusd 0.6.) No volume path guiding (volume vertices push
  `train: None`; volume-heavy scenes train the surface field on noisier estimates —
  slower convergence, not bias). One global majorant per region — no coarse max-grid, so
  a high `densityScale` over a large box tracks slowly. Emissive volumes are not
  light-list entries: fire is found only by phase/BSDF-sampled paths (firefly risk near
  bright emission), never by NEE. Carried-medium scatter vertices run no NEE unless the
  medium is a boundary's enclosure (a refracting interior's shadow rays are blocked by
  its own surface, so NEE there would be a wasted ray per scatter). Region overlap uses
  summed extinction (exact) with a σₛ-weighted phase mixture.

## Known gaps: medium boundaries

- **One owner at a time** (Typhoon's model): nested or overlapping boundaries do not
  compose — inside one, another is crossed unchanged, and glass inside fog is
  travelled in the fog's medium. Typhoon's planned fix, priority-ordered interior
  lists with false-interface skipping (its `doc/plan-volume-ids.md`), is the shape
  to follow here too.
- **A camera inside a boundary** starts in vacuum and sees the medium only after its
  rays leave and re-enter. Rays starting inside are not detected.
- **Closed, outward-facing meshes only.** Entry is a front face; an open or inverted
  mesh lets paths in without letting them out (or the reverse).
- **Majorant free flight**, not Typhoon's Chiang channel MIS: a chromatic medium is
  noisier, not biased. One medium value per entry (the VDF evaluated where the ray
  enters).
- **The restart epsilon**: a crossing restarts 0.001 short of the boundary in the new
  medium, so each entry adds σ·0.001 of optical depth (the 0.3% above).
- **Light path expressions see a crossing as nothing.** Cutouts and thin walls passed
  before a boundary on the same segment are not `Ts` events of the vertex the segment
  reaches (the arrival is the last stretch's), and the camera's first-hit AOV skips a
  thin wall in front of a boundary. The beauty is unaffected.

## History: volume tracking read the ray parameter as a distance

Until volume materials were measured against volume regions, `Volumes::
sample_interaction` and `Volumes::transmittance` stepped and integrated in the
ray's **parameter** `t` with coefficients per unit of **distance**. Every ray
the integrator builds from a material is unit, but a camera ray is not: it
reaches the focus plane at `t = 1`, so its length is the focus distance, 10 by
default. Region fog seen straight from the camera was therefore ten times
thinner than the same fog seen in a reflection or along a shadow ray — and the
image changed with `focusDistance` on a pinhole camera (`samples/fog.usda`
mean ×5.9 between focus 10 and 1). Both now scale by the direction's length
(`tracking_measures_distance_not_the_ray_parameter`). Shadow and bounce rays
are unit, so only camera segments through regions changed: `fog` and `smoke`
re-render denser, every other sample is bit-identical. The medium-boundary
restart normalises its ray for the same reason, and steps back 0.001 in
*distance*: 0.001 of a camera ray's parameter put a hundredth of a unit of fog
outside the box, which was the 1% the first A/B against regions showed.

## Known gaps: thin walls and glass shadows

- **Thick glass still blocks shadow rays.** A closed dielectric (a bottle, a glass jar)
  refracts what crosses it, so a straight shadow through it would be biased; it stays
  an occluder, and its interior and what lies behind it are lit by refracted paths
  alone. Typhoon offers the straight shadow only with caustics off
  (`ty:enableCaustics = false`). It could come to crust the same way, behind an
  explicit opt-in, never by default.
- **A closed thin-walled object keeps paths inside it longer.** A path leaves through a
  wall with probability `q = max P`, below the selection probability the delta
  transmission had as a lobe (a soap bubble at `transmissionWeight` 0.4: 0.22 against
  0.43). Inside, each meet samples the specular lobe with the mass `LobePmf` gives it
  from `F0` — far below the window reflectance `2R/(1 + R)`, more so under a thin
  film — so a chain of specular bounces carries `value/pdf ≈ 8` per bounce. Unbiased,
  but a rare heavy sample: `openpbr_showcase`'s bubble shows a single ~7000-weight
  sample in some 1024-spp renders (trimmed relMSE is lower than before at every sample
  count; the default firefly clamp catches it). The fix is the thin wall's specular
  selection weight, which changes those materials' sampling and needs its own A/B.
- **The first-hit AOVs see a passed wall's geometric normal.** A wall passed through is
  never shaded, so a normal map on it does not reach `N`.
- **Two branches per vertex remain** in a world without thin walls (+0.30% of
  cornellbox's instructions). Removing them would mean swapping a met wall's material
  for a precomputed twin without its straight lobe, which was not judged worth it.

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
