# lighting — design record

> Design record for the **lighting** capability: the reasoning, measurements and
> history behind the behaviour `spec.md` states. Moved out of `CLAUDE.md`, which
> now keeps only the rules and pointers. Section and path references such as
> "above" or "see X" may point to another capability's `design.md` —
> `openspec/specs/*/design.md` is the whole record; `docs/architecture.md` is the map.

## The Light trait, shapes and light selection

- **`Light`** (`light/`) — `sample_point`/`pdf`/`emission`/`material`. The one
  implementation is **`AreaLight`**: a `LightShape` (pure emitting geometry —
  `SphereShape`, `RectShape`, and `AffineShape`, a unit sphere / disk / tube under any
  invertible affine) paired with the `Arc<Emissive>` its scene geometry carries.
  A light's `Emissive` is built by `Emissive::light`: **one-sided** (emission only
  toward the geometry's front, which is why the rect light's triangles are wound with
  their normal along local −Z) and optionally **shaped** (`lux::Shaping`). Both MIS
  halves ask it the same question — `Emissive::radiance_toward(dir, front)` — NEE from
  `AreaLight::sample_li`, the bounce side through `Material::emitted_at`; route any new
  directional emission through that one function or the two strategies see different
  lights. `LightShape::inv_pdf_area` is the reciprocal of the shape's *own* sampling
  density (default: its area, i.e. uniform); `AffineShape` samples uniformly in local
  area, so its world density varies under a non-uniform scale and it reports
  `local_area · |det M| · |M⁻ᵀ n|` — both MIS halves read it, so it need only be the
  density actually sampled. The cosine in the area density `d²/(|cos θ_l| · A)` is
  **unsigned**, as the Jacobian requires: a point seen from behind has a finite
  density, and whether that side emits is `radiance_toward`'s `front`, not the pdf's
  (a one-sided light returns zero radiance and NEE skips it before the shadow ray; a
  two-sided emitter keeps NEE from behind and from inside a sphere). Where the
  density is not finite (edge-on, degenerate) `AreaLight` **refuses** the point on
  both sides, pbrt-v4's way: `sample_li` returns `None` and `pdf_at_point` returns
  **`None`**, which `bounce_emission_weight` reads as "NEE never delivers this" and gives
  `SamplingStrategy::unopposed_weight` (1 for every strategy, `light` included —
  the same weight every no-competitor branch there and in `escaped_emission` takes).
  It used to add `1e-4` to that denominator instead, an NEE bias of
  `1 + 1e-4/(cos θ_l · A)` (+12.7% measured on a 1.3e-3 m² disk,
  `docs/light_sampling.md` §3.10); do not reintroduce a finite stand-in.
  The refusal is a type: light densities are `PdfSolidAngle` (`crust-core/src/pdf.rs`),
  whose `new` refuses anything not finite and positive, the area density is an
  `InvPdfArea` whose `to_solid_angle` is the one conversion, and the MIS weights take
  nothing but `PdfSolidAngle`s. A dome's `escaped` answers `None` for the pdf of a
  direction its map gives no density (a black texel), which `escaped_emission` takes
  unopposed, the same rule as a refused point.
  A shape with a better strategy than area sampling implements
  **`LightShape::solid_angle_sampler(from)`** (default `None`, meaning "sample me by
  area"): a `SolidAngleSampler` whose `sample(u, v)` gives a point plus its
  *solid-angle* pdf as seen from the shading point and whose `pdf(p)` gives the
  density of a point, which `AreaLight::sample_li` and `pdf_at_point` both prefer when
  present. The contract is what keeps the two MIS sides one strategy — whether the
  shape answers depends on `from` alone, never on `u`/`v`, and both halves answer for
  exactly the same `from`s with the same density. It used to be two independent trait
  methods kept in step by prose (and `AffineShape` repeated its sphere-only guard in
  each); one hook returning one sampler makes it structural, at no measurable cost
  once the sampler is `inline(always)` (callgrind, 2 spp: veach_mis +0.04%, usdlux
  +0.03%, rectlight +0.05%; without it `sample_li` grew 6%). **`SphereShape` samples the cone it
  subtends** (Shirley et al. 1996, pbrt-v4's `Sphere::Sample`): uniform over the
  visible cap, pdf `1/(2π(1 − cos θ_max))`, with pbrt's small-angle threshold
  `sin² θ_max < sin² 1.5°` (`SMALL_CONE_SIN2`, where `1 − cos θ_max` cancels in f32)
  but **not its approximation below it**: pbrt draws `sin² θ = u·sin² θ_max` there,
  whose density goes as `cos θ` against a constant pdf, so crust takes
  `1 − cos θ_max = sin² θ_max/(1 + cos θ_max)` and draws `t = u(1 − cos θ_max)`,
  `sin² θ = t(2 − t)` — exact and cancellation-free (`SubtendedCone::sample`). A cone
  whose pdf would overflow f32 is refused on both hooks, and area sampling only from
  inside the sphere. Area sampling spent at least half its
  shadow rays on the hemisphere facing away. An **`AffineShape` sphere** (non-uniform
  scale) samples the *unit* sphere's cone in local space and maps the point through the
  placement — exact, because an affine map preserves which points of a convex surface
  face a given point — with the direction map's solid-angle Jacobian
  `|Mω|³ / |det M|` on the pdf (`world_solid_angle_pdf`), so its density varies over
  the cap and both MIS sides evaluate it at the point. `AffineShape` tubes sample the
  arc of wall that faces the point, and disks optionally their spherical ellipse: see
  "Disk and tube lights" below.
  **`RectShape` samples the spherical rectangle it subtends** (Ureña, Fajardo & King
  2013, as pbrt-v4's `SampleSphericalRectangle`): uniform in solid angle, pdf `1/Ω`,
  and the point is returned through the rectangle's own `(s, t)` so it lies on the
  light exactly as an area sample does (the triangles a bounce hits, the texel a card
  reads). It runs in **f64**, because `Σg − 2π` cancels in f32 at the solid angles it
  hands back to area sampling at, and its setup is not the paper's: in the rectangle's
  frame the four edge-plane normals are axis-aligned in closed form, so each corner
  angle is `atan2(h·|v|, ±x·y)`, and the sums the map needs are arguments of complex
  products — **one** `atan2` in all, `g2 + g3` kept as its normalised `(cos, sin)`.
  Area sampling stays, on both hooks alike (`RectShape::spherical_rect`), for a sheared
  parallelogram, from behind the one-sided light or on its plane, and outside
  `[1e-4, 6.22]` sr (pbrt-v4's bounds). Textured cards take it too, since crust does not
  sample the image (pbrt gives the map up only because it does). It is not a free win:
  ~110 ns more per NEE sample than area sampling, and on a glossy receiver it can lose,
  because area sampling's `r²/cos θ_l` density happens to follow some highlights —
  measured in `docs/light_sampling.md` §3.9. The bilinear cosine warp (Hart et al.
  2020) is not done: it needs the receiver's normal, which `Light::sample_li` is not
  given.
  Lights are stored in a `LightList` and their surfaces are also attached to `world` as
  emissive geometry — masked out of **camera** rays by default (the industry convention:
  a light in frame does not show its source; `crust:light:cameraVisible` opts back in,
  then RenderMan's `primvars:ri:attributes:visibility:camera` (an int, non-zero
  visible) when the crust attribute is not authored, and an authored `crust:rayMask` wins
  outright). A source the camera does not see is a **transparent emitter**: shadow rays
  do not see it and bounce rays cross it (see "Hidden lights are transparent emitters"
  below); a camera-visible one is seen by every ray and is solid —
  the `AreaLight` records the geometry's `geom_id`, which is how the integrator
  attributes a bounce-hit emissive surface to its light (`LightList::find_index_by_geom_at`).
  **NEE samples one light per vertex**, picked by the `LightList`'s selection,
  `crust:lightSelection` / `--light-selection`, built in `Renderer::new` from the
  settings.
  - **`power` (the default)** is defensive:
    - lights at infinity keep their uniform share;
    - the finite lights split the rest `DEFENSIVE_SHARE` (½) evenly and ½ in
      proportion to `Light::power`;
    - a black light gets zero.
  - **`uniform`** is one in N, the renderer before this was a choice, reproduced
    **bit for bit**: 0 differing pixels on all 22 checked-in samples at 16 spp.
  - **`learned`** (opt-in, `tracer/light_cache.rs` trains it, `light/learned.rs` holds its tables; `docs/light_sampling.md` §3.12) is
    visibility-aware. Before the first pass, a deterministic pre-pass (one camera
    path per 4×4 pixels, two BSDF bounces) estimates **every** light's NEE
    contribution at each vertex with the integrand itself (radiance × BSDF ×
    shadow ray), and sums the estimates into a grid. Each trained cell picks
    `0.7 · E/ΣE + 0.3 / n_live`, and everywhere else the power table answers.
    It exists because of ALab: power gave 49% of the picks to two exterior
    lights visible from **no** receiver, and 3.2% of light samples delivered
    light (`examples/light_occlusion` measures that per light). Learned cuts
    ALab's direct-lighting relMSE **4.1×** and `usdlux`'s 2.0×, and is never
    worse per sample on the checked-in samples. **It costs time, though**:
    +14% / +23% Render (min / mean) on ALab at 128 spp. Only ~0.6 s of that is
    the pre-pass. The rest is shadow rays that now reach their light and
    traverse the whole BVH. So at equal time it is ~3.6× on ALab's direct
    lighting and only ~1.1× on its full image, which is mostly indirect. Four
    details are load-bearing:
    - **MIS.** Both sides go through `LightList::pick_index_at` / `pmf_at` /
      `find_index_by_geom_at` / `infinite_at`, keyed by the vertex NEE sampled from. The
      bounce side passes `prev.pos`. Route a new pmf read through the `*_at`
      form or emission is double-counted.
    - **Robust grid bounds.** The grid spans the receivers' 2–98% quantiles.
      With the full bounds, ALab's stray exterior bounces made it 4 cells.
    - **The defensive share is uniform, at 0.3.** Mixing with power reinstates
      the hidden lights, and at 0.2 `domelight` fireflied at shadow boundaries.
    - **Power table underneath.** The cache only installs over a power table:
      with none, `density` divides by `n` and would disagree with the per-cell
      pmf.
  - **`neural`** (opt-in and experimental, `tracer/light_visibility.rs` trains it over the
    `crust-nn` crate and `light/neural.rs` holds it; `docs/light_sampling.md` § "Neural visibility cache") is
    `learned`'s knowledge stored as a continuous function instead of a grid,
    after Bokšanský & Meister's *Neural Visibility Cache* (JCGT 2025). The
    same pre-pass walk (`light_cache::walk`) labels each receiver with the
    fraction of each light's samples that **delivered light** — backfacing,
    below the horizon and occluded all count as not — and a small network
    learns each light's visibility `V̂ℓ(x)` from position: an 8-level hash grid
    (4 to 107 cells per axis, tables of 2¹⁴ vertices) feeding 16 → 32 → 32 → N,
    trained for a fixed 8 epochs of 256-receiver batches with Adam, then
    frozen. Three details are load-bearing:
    - **The pick (D2)** is `(1 − D) · πℓ V̂ℓ(x) / Σ πV̂ + D / n_live` over the
      power table `π`, with `learned`'s `DEFENSIVE` as `D` (shared, not
      copied) and the table built by the same `light::learned::defensive_table`,
      CDF first. Where every prediction is hidden (`Σ πV̂ ≤ 1e-6`) or the
      vertex is outside the receivers' 2–98% bounds, the power table answers.
      The network learns visibility only; distance and power stay analytic.
    - **One pure function on both MIS sides (D6).** The `*_at` lookups call
      `VisibilityCache::with_table(p)`, a function of `p`'s bits alone, so the
      bounce side, recomputing at `prev.pos`, reads the bits NEE picked with. A
      grid forgave an ulp between the two positions; a continuous function
      would make it an MIS mismatch. So debug builds assert, at every NEE site
      and every bounce-side setup, that they are the same `Vec3A` bits
      (`nee_picks_from` / `bounce_weighs_from` in `tracer/path.rs`). A
      one-entry thread-local memo, keyed by the cache and `p`'s bits, serves
      the bounce side's lookup right after NEE's for free; a miss recomputes
      the same bits (`tests/neural_selection.rs` pins both). The neural arms of
      the `*_at` methods are out of line, so the other selections' code is
      unchanged.
    - **At most 64 lights (D7)**, one output unit each. Past that, and below
      two lights or with no receiver, it falls back to power — with one `WARN`
      naming the limit past 64, silently otherwise — as `learned` does past
      its 1024.

    Training is a function of the scene alone: initialisation and each
    epoch's shuffle draw from `openqmc` under `K_NVC`, and a batch's gradient
    is summed in fixed 32-receiver chunks in chunk order however many threads
    ran them (`crust_nn::Network::batch_gradient`). Plain `f32` in a fixed loop
    order, no `mul_add`: `scripts/test_simd_matrix.sh -p crust-nn` pins the
    bits under every codegen. **A trap:** the epoch count is fixed, so the
    number of Adam steps scales with the receivers. A 64 × 36 frame trains on
    ~300 receivers, 16 steps, and every prediction is still near ½ — which
    degrades the pick towards power, never below the defensive floor.

    **Measured** (`docs/light_sampling.md` §3.14): unbiased (relMSE × spp
    flat from 16 to 256 spp on `usdlux`) and MIS-consistent (`veach_mis`'s mean
    within 2e-5 of power's at 1024 spp). But one evaluation costs ~600 ns,
    ~6 000 instructions at 7 lights — 4.8–13.8 shadow rays on the samples,
    after two bit-identical passes cut it from ~820 ns — and adds 40–100% to a
    render, while visibility alone buys at most 1.2× per sample over power.
    At equal time it loses to `learned` by 1.5–5.3× and to power by 1.4–1.9×
    on every sample measured. **ALab, the scene it was built for, confirms
    it.** There one evaluation costs 0.94 of a shadow ray (1 131 against
    1 198 ns; 14 703 against 6 604 instructions), so the D9 gate passes. Yet
    the network only ties `learned` per sample on direct lighting (trimmed
    relMSE 1.03×), and loses on the full image (0.82×). It costs 19–29% more
    time: a 6.2 s pre-pass against 1.5 s, inference at every vertex, and
    shadow rays 20% slower behind it, as its 1 MiB table evicts BVH nodes. At
    equal time that is 0.79× `learned` on direct lighting and 0.69× on the
    full image. The verdict is retire.
  - **`tree`** (`light/tree.rs`; `docs/light_sampling.md` §3.15) picks the finite
    lights by walking a light BVH from the receiver, pbrt-v4's
    `BVHLightSampler` in shape, over a defensive share of the power table.
    It sees distance, orientation and the receiver's horizon, and no
    visibility. See "Light tree" below.

  **Both halves of the design were forced by measurement** (`docs/light_sampling.md`
  §3.8):
  - **The fixed infinite share.** Pure power selection, pbrt-v4's `PowerLightSampler`
    with a dome's power taken against the scene radius, made `domelight` 1.42× and
    `usdlux` 1.47× noisier at 16 spp. The sun took 88% of the rays, yet in its own
    shadows the dome is the only light.
  - **The even half.** Power is blind to distance and visibility, and the even half
    bounds what that blindness costs.
  - **Result, over four seeds:**
    - a key among seven dim fills: 4.6× lower relMSE;
    - `usdlux`: 2.6% lower;
    - `veach_mis`: 6% higher. Its equal-radiometric-power lights are tinted, so their
      *luminance* powers differ by ±10%, and each lights its own band of the plates,
      so the rays moved away from the red and blue lights cost exactly the pixels
      those lights own;
    - everything else within 1%;
    - time within noise.

  The light strategy's MIS density is `light.pdf · pmf`, times the vertex's light
  sample count, computed by `LightList::density(pdf, pmf, samples)` on **both** sides
  (see "Several light samples per vertex" below). `pick`, `find_index_by_geom_at` and `iter` all hand
  back the same `pmf` for the same light. Under uniform, `density` is the historical
  division `pdf / n`, not `pdf · (1/n)`, which rounds differently when n is not a power
  of two; that is what keeps the A/B exact. A light with `pmf = 0` keeps its bounce
  emission at full weight, since NEE never samples it.

  `Light::power` is a *flux* — a sampling weight, never shading — and `None` at
  infinity:
  - `AreaLight` uses `Emissive::flux`: `π A L` unshaped whatever the shape, times the
    texture's mean texel for a textured card.
  - A shaped light uses `Shaping::integrate`, taken in rings about the axis *out to
    the cone angle only*, so a 2° spot is resolved as well as a hemisphere.
  - The table is inverted by **CDF, not an alias table**. The map from the pick
    dimension to a light stays monotone, so the samples that pick light *k* remain one
    contiguous slice, as under uniform.
  - A `LightList` that never passes through `select_by` (a bare `ray_color`, or a list
    with a light `add`ed since) picks uniformly, consistently on both sides.
  - A per-light `DEBUG` line gives each light's power and pick probability.

  Emissive geometry with no light-list entry is handled: the bounce keeps its emission
  at full weight.

## Light tree

`crust:lightSelection = "tree"` (`light/tree.rs`, the `tree` arms of
`LightList`'s `*_at` lookups). It answers the half of light-selection waste a
position-and-flux table cannot see. On ALab, 29.5% of NEE picks went to a light
facing away from the receiver, and 15.1% to one below its horizon
(`docs/light_sampling.md` §3.12). It is the default selection in pbrt-v4 and
Cycles. It is blind to occluders, which is `learned`'s half.

- **Bounds.** `Light::bounds(luma)` gives a box, an axis `w`, a normal spread
  `θo`, an emission spread `θe`, a flux and whether it is two-sided. The flux
  is `Emissive::peak_flux`, the flux of an unshaped emitter as bright as the
  light's brightest direction (`Shaping::peak`), not `Light::power`. The cone
  is already in `θe`, and a cone-integrated flux penalised a narrow spot twice:
  inside its pool it was ranked like a dim light and weighted hugely, which
  showed as fireflies in the first `many_lights` renders. pbrt-v4 gives a spot
  the flux of an isotropic light of its peak intensity for the same reason.
  The defensive power arm keeps `Light::power`. `AreaLight` takes the
  shape's `LightShape::extent` (its world box, and its normal if flat) and the
  tighter of two direction bounds:
  - the surface's own: one-sided flat `θo = 0, θe = π/2`; two-sided flat the
    same, mirrored; curved `θo = π`;
  - the shaping cone's: `Shaping::factor` is exactly zero beyond the cone
    angle `a` from the shaping axis, whatever the shape, so `θe = min(a, π/2)`
    and `θo = a − θe`. The split keeps the emitter cosine `cos θ'` positive
    inside the cone; `θo = 0, θe = a` past `π/2` would make it negative.
    pbrt-v4 splits a spot's falloff the same way.

  The unit tests sweep every shape, sided-ness and cone, and check that every
  sampled point is in the box and every lit direction within `θo + θe` of `w`.
  A bound too tight is the one way the tree could starve a light; the
  defensive share below keeps that unbiased.
- **Importance** (`LightBounds::importance`) is pbrt-v4's
  `LightBounds::Importance`:
  - `phi · cos θ' / d²`, with `d²` clamped to the half diagonal;
  - `θ'` is the angle to `w` less `θo` and the box's subtended angle `θb`,
    zero beyond `θe`;
  - times the receiver's `cos θ_i`, reduced by `θb`.

  One change from pbrt: the receiver term is **signed at an opaque receiver**
  (`Receiver::opaque`, from `Material::transmits()`), so a box wholly below
  the horizon gets zero there. Elsewhere it is pbrt's `|n·ω|`, so glass and
  thin walls still see lights behind them. pbrt's `|n·ω|` alone cannot cut
  ALab's below-horizon picks, and a signed form everywhere would starve glass
  of light from behind. `transmits()` is per material and conservative
  (default `true`). `OpenPBR` answers from `transmission_weight` and
  thin-walled subsurface, which are never textured (only the base colour is).
  `PreviewSurface` also answers `true` for a textured `opacity`. MaterialX
  answers `false` only when every leaf reflects. The integrator asks it only
  under `tree` (`LightList::reads_receiver_surface`), so other selections pay
  no virtual call.
- **The receiver travels.** The `*_at` lookups take a `Receiver { p, n,
  opaque }` (`impl Into<Receiver>`: a bare position is a receiver with no
  normal, which is what volume vertices and the probes pass). `n` is
  `HitRecord::normal` (interpolated, before normal mapping, facing the side
  the ray came from).
  - `PrevBounce::from` stores the receiver NEE picked from, and
    `PrevVertex::Phase` rebuilds `Receiver::at(pos)`.
  - `nee_picks_from` / `bounce_weighs_from` compare all of it bit for bit.
  - The link twin takes the receiver too.
- **Build.** Binned SAOH, 12 buckets per axis (Conty Estevez & Kulla 2018, as
  pbrt-v4), with a stable partition. Ties go to the lower axis and bucket, so
  the tree is a function of the light list. Past depth 40 it halves instead,
  so a bit trail (`u64`) always reaches a leaf. The first child is the next
  node in a flat depth-first array.
- **One pmf for both MIS halves.** `LightTree::sample` walks from the root,
  choosing each child with `u < c0/(c0+c1)` and stretching `u` back over
  `[0, 1)`. `LightTree::pmf` re-walks the light's trail. Both multiply
  `c_child / (c0 + c1)` root to leaf, from the one `importance` function, so
  the pick reports the trail's bits (pinned over random receivers at 2, 7, 47
  and 1 000 lights). A node whose two children both have zero importance
  ends the walk with no light, and the trail gives zero: consistent, so
  unbiased.
- **The defensive share.** A pick is `D · power + (1 − D) · q`, with `D =
  TREE_DEFENSIVE_SHARE`. `q` gives each light at infinity `1/live` and the
  tree's lights `finite_share · p_tree`. The first `D` of `u` inverts the
  power CDF; the rest splits into the infinite lights' slice and the tree's.
  Every emitting light keeps at least `D` times its power probability, so its
  NEE weight is at most `1/D` times what `power` gives it, the bound `power`'s
  even half gives against `uniform`. Lights at infinity keep exactly their
  `1/live` share, as under `power`. Each part is monotone in its slice of
  `u`, so the stratified picks of `crust:lightSamples` stay stratified within
  each part; a light has two slices, not one.
- **Nothing to choose.** With at most one emitting finite light,
  `select_by(Tree)` builds no tree and the power table picks: such a scene
  renders under `tree` bit for bit as under `power`.
- **Measured, and opt-in** (`docs/light_sampling.md` §3.15).
  - `D = 0.1`, chosen from {0.1, 0.25, 0.5}: the lowest share won on every
    scene measured.
  - Per sample: 1.86× lower relMSE on `usdlux`, 11–22× on the 256-light
    `many_lights`. It is unbiased (relMSE × spp flat to 256 spp; `veach_mis`'s
    mean within 0.0002 of `power`'s on 2.65).
  - It costs 14–21% more instructions per sample where a tree is walked, and
    `power` about 0.6% for the receiver every vertex carries.
  - The equal-time gate passes on 5 of the 11 samples that build a tree
    (`many_lights` 8.8×, `usdlux` 1.66×, `hair`, `light_visibility`,
    `light_linking`) and fails on 6. The worst are `materialx_showcase`
    (0.61×), `materialx_teapot` and `materialx_lion` (≈ 0.8×). ALab and the
    OpenPBR Shader Playground, the gate's other scenes, were not available.
    So `power` stays the default.
- **A trap: large lights near the set.** A receiver well inside a big panel's
  subtended cone sees its box at a wide `θb`, which erases the panel's
  orientation from the bound. On `materialx_showcase` an 8 × 3 m backdrop
  light facing away from the teapot takes 28% of the tree's picks there and
  delivers nothing. Raising `D` only dilutes the misranking. The fix is
  Cycles' average of the conservative and an optimistic ("min") importance,
  or the light's exact orientation at a leaf, where the node holds one light.

## Disk and tube lights

`DiskLight` and `CylinderLight` are `AffineShape`s: a unit disk in local XY emitting
along −Z, and a unit-radius, unit-length open tube along local X, placed by the light's
transform composed with its radius and length (`(r, r, 1)` and `(length, r, r)`), so a
real tube's local space is almost never metric. Both used to be sampled uniformly in
local area. Each now has a solid-angle strategy behind its own switch, whose `area`
value is the old sampler bit for bit (pinned by the goldens and
`round_lights_fall_back_to_area_sampling_bit_for_bit`).

**Tubes: the visible arc, equiangular along the wall (`CRUST_TUBE_SAMPLING`, default
`equiangular`).**
- From `f = world_to_light(from)` outside the tube (`ρ = |(f.y, f.z)| > 1`), a wall
  point at azimuth `φ` faces `f` exactly when `|φ − φ₀| < w = acos(1/ρ)`,
  `φ₀ = atan2(f.z, f.y)`, whatever `x`. An affine map preserves which points of a convex
  surface face a point (the squashed sphere's argument), so the local arc is the world
  visible arc under any placement, sheared and elliptical tubes included. Area sampling
  spent about half its samples on the far side, every one of them zero radiance from a
  one-sided emitter.
- `φ` is uniform over the arc (`1/2w` per radian). Given `φ`, the wall points form a
  straight world segment, so the axial position is drawn **equiangularly in world arc
  length** along it — `s = s₀ + H tan θ`, `θ` uniform between the ends' angles, `H` the
  point's distance from that line — which follows its `1/r²` on a tube scaled
  `(length, r, r)` where a local-`x` draw would not. In solid angle the `r²` cancels:
  `pdf = L H / ((θ_b − θ_a) · 2w · area_scale(n) · |cos θ_l|)`. `arc` is the same arc
  with `x` uniform, kept so each half of the gain stays measurable.
- **Never as the difference of two `atan`s.** Seen far beyond an end, near the axis
  line, both ends' angles sit by ±π/2, and `θ_b − θ_a` in f32 kept about three bits:
  256 strata of `u` landed on five axial positions, which `pdf` did not describe. The
  span is one `atan2(L H, H² + s₀ (s₀ − L))`, and the sample is
  `s = R² sin ψ / (H cos ψ + s₀ sin ψ)` with `ψ = u · span` and `R² = H² + s₀²` (the
  tangent's addition rule from the `s = 0` end; the denominator is `R cos θ`, positive).
  `far_end_on_tube_samples_stay_equiangular` pins it.
- **Only for a one-sided emitter** (`AffineShape::front_only`, set by `AreaLight::new`
  from `Emissive::is_one_sided`): a two-sided tube seen through an open end shows its
  inner wall, which the outer arc never reaches. From inside the tube, or on its wall,
  it keeps area sampling. Every imported tube is one-sided.
- **The strategy may refuse a point.** Its density is infinite at the silhouette and
  zero outside the arc, so `SolidAngleSampler::sample` and `pdf` return `Option`, and
  `AreaLight` treats a strategy that exists from `from` as final on both MIS sides: a
  refused sample is refused, never re-drawn by area, and `solid_angle_pdf` returns the
  strategy's `None` rather than falling through to the area density, which would weigh
  a bounce hit against samples NEE never draws. `sample` reports the density `pdf`
  computes for the placed point, so both halves agree bit for bit.
- **Measured** (16 spp, `--indirect-clamp 0`, relMSE against 1024 spp area-sampled
  references, mean of four frames; equal time = relMSE × Render seconds, min of four):
  on a tube 1 unit above a diffuse or a glossy (metal, roughness 0.25) floor,
  length/distance 0.3, 1, 3 and radius/distance 0.01, 0.3, `equiangular` had the lower
  relMSE on all 12 and won at equal time on all 12, **1.3–20×** (the long thin tube over
  diffuse most, the long thick one over glossy least); `arc` won 1.2–8.8×, ahead of
  `equiangular` at equal time on three glossy cases only. The tiny sweep frames (40 ms)
  time too noisily to price a sample; on `usdlux` the Render phase is 3.6% longer, for
  1.89× lower relMSE: 1.82× at equal time. With `--light-samples 4` the arc is
  stratified across the four draws by `openqmc` with no help: at 4 spp × 4 samples
  `equiangular` is 13.6× below `area`, against 17.5× at 16 × 1. No solid-angle band:
  the back-facing half it skips does not shrink with distance (the smallest tube still
  won 7.4×).

**Disks: the spherical ellipse (`CRUST_DISK_SAMPLING`, default `area`).**
- A disk seen from a point subtends a spherical ellipse, sampled uniformly by Guillén
  et al. 2017's radial map (`light/ellipse.rs`) in local space and mapped out with the
  direction map's Jacobian, as the squashed sphere is; the point comes back through the
  unit disk, so it lies where a bounce ray hits it. Strictly in front of the emitting
  side, `b ≥ 1e-4 a`, and `Ω ∈ [0.1, 6.22]` sr; area sampling otherwise.
- **The map, from the paper, with three corrections.** (a) Eq. 3 as printed puts the
  disk's centre in the plane of `x̂_d` and then measures the major axis along it; the
  major axis is the one perpendicular to the plane through the point, the centre and
  the normal, and the frame is built from that symmetry (pinned by brute-force solid
  angle off axis, and `a ≥ b` over a dense grid). (b) Eq. 19's `φ − k Π` cancels for a
  small eccentric ellipse, and `Π` from `(n, φ, m)` is ill-conditioned as `n → 1`:
  `1 − n = b² cos² α / (a² cos² β)` and `1 − m = cos² α / cos² β` are formed in closed
  form, without which the solid angle was off by 3e-4 on a grazing disk. (c) Newton runs
  in eq. 20's `ψ = φ_t`, `Π`'s own argument, in which `Ω_r` is nearly linear for any
  small ellipse; in `φ` an eccentric ellipse took up to 10 iterations where `Ω_r` is
  flat (slope `b²` near `π/2`), in `ψ` at most 6 (2.7 on average over the grid), with a
  bisection safeguard and a residual tolerance at `Ω_r`'s own rounding (8 ε).
  `R_F` and `R_J` are Carlson's duplication algorithm, pinned to mpmath at 1e-12
  (`scripts/ellipse_oracle.py`, 4e-14 worst); the circle reduces to the cone exactly.
- **The low-distortion variant (eq. 23)**, Shirley–Chiu's concentric map pre-warping
  `(u, v)`, had the lower 16 spp relMSE than the plain radial map on 7 of the 8 sweep
  disks and on `usdlux`, so it is the only map.
- **The sampler stays small.** `SolidAngleSampler` holds only `Ω_r(π/2)` for a disk
  (the one elliptic integral the strategy decides with); the frame is rebuilt where a
  sample is drawn. The strategies' bodies are out of line and answer in registers — a
  point, then an `Option<PdfSolidAngle>`. Each other arrangement cost `veach_mis`, which
  has neither shape, 2–14% of `sample_li`'s instructions (callgrind): inlined bodies;
  a 200-byte ellipse in the sampler, copied for every sphere sample; a borrowed
  sampler, kept in memory; an `Option<(Vec3A, _)>` returned from the out-of-line call,
  which merged every arm's answer in memory. Pinned by
  `round_strategies_do_not_grow_the_sampler`. As built: `sample_li` −0.8%, `Bvh::hit`
  unchanged, `pdf_at_point` +0.36M of 7.78G.
- **Why it is off.** On disks 1 unit above the same floors, radius/height 0.1, 0.3, 1,
  3, it won at equal time only at radius 1 over the diffuse floor (**7.6×**: 28× lower
  relMSE) and tied at 0.3 (1.08×); at radius 3 over it, 2.3× lower relMSE was not worth
  its cost (0.62×). It lost on every glossy floor (0.30–0.62×), and on the 0.1 and 0.3
  glossy ones in relMSE alone, as the spherical rectangle does
  (`docs/light_sampling.md` §3.9: area sampling's `r²/cos θ_l` density follows some
  highlights). It costs 2–4× per frame: about 5,000 instructions per sample, 37% of a
  disk-lit render in `EllipseMap::omega_r` (about four `Π` per sample: the strategy's
  `Ω`, then Newton). On radius 0.1 (~0.03 sr) it lost in relMSE too (1.8×: it
  stratifies worse than the area map), which is what moved the band's floor from the
  rect's `1e-4` to 0.1 sr. `usdlux`: same relMSE, +50% time. D5's circumscribed-square
  alternative was not built: it is uniform in solid angle too, plus 21% zero samples,
  so it cannot win the glossy cases either.
- Measured on a sweep under the scratchpad (not checked in); ALab was not available.
  Numbers in `openspec/changes/archive/*-solid-angle-disk-and-tube-lights/notes.md`.

## Several light samples per vertex

`crust:lightSamples` / `--light-samples` (N, the first vertex of each path, whatever
kind of vertex it is) and `crust:lightSamplesIndirect` / `--light-samples-indirect`
(M, every later surface or volume vertex, a subsurface walk's exit included), both
default 1 and from 1 to 1024 (`RenderSettings::with_light_samples`, which clamps;
`DEFAULT_LIGHT_SAMPLES`, `MAX_LIGHT_SAMPLES`; a stage value outside the range is
clamped with a `WARN`, the CLI refuses it as a usage error — a count multiplies every
vertex's shadow rays, so a mistyped huge one would be a render that never ends). The motivation is in
`docs/light_sampling.md` §7.4: direct lighting at the camera vertex is the largest and
most visible term at 16 spp, a shadow ray costs about 0.27 µs against about 1.5 µs per
shading point, and N light samples there cost N − 1 shadow rays and no extra camera
paths, for direct-light variance falling about as 1/N.

- **The estimator** (`tracer/path.rs`): the surface block is `surface_light_sample`,
  one light sample at a vertex, `inline(always)`; the vertex calls it straight-line for
  its first sample, exactly the block as it stood before counts existed, and
  `extra_surface_light_samples` (cold, out of line) loops it for samples 1 … N−1.
  Getting there cost five builds (callgrind on cornellbox, 2 spp, one sample): a
  `for i in 0..count` around the block, +0.57%; its multi-sample arms as cold calls,
  +0.90%; the block as an inlined function taking a per-vertex context struct by
  reference, +1.4% (the struct's address escapes into the cold call, so it and `ray`,
  `rec`, `sp` live in memory — the trap `PathContext` documents), by value +0.96%,
  as plain parameters +1.0% — and pinning the count to 1 still left +0.76%, which
  is what finally pointed away from the count: two closures in the block (the pick
  under `then`, and the shadow-ray `visibility`) had become out-of-line functions,
  0.65% apiece, because the cold copy gave each a second call site and LLVM stopped
  inlining them. The block is now closure-free `if let` chains. Lesson: a block that
  is inlined twice must contain no closure worth inlining. `volume_nee` is already
  out of line and simply loops.
  Sample `i` draws its pick, point and shadow-ray randomness from
  `nee_sampler(v, i)` — the vertex's own domain for the first sample, so the
  one-sample render is bit-identical, and `v.new_domain(K_NEE_SAMPLES).new_domain(i)`
  for the rest. The pick coordinate is
  `stratified_pick(u, count, i) = (i + u) / count`: the pick is a monotone CDF inversion,
  so a light with selection probability `p` is picked `count · p` times, give or take
  one (pinned by `stratified_picks_sample_each_light_count_times_its_probability`:
  pmfs 0.5/0.25/0.25 and N = 4 give 2/1/1 at every vertex), where independent picks
  would give a binomial count that leaves a bright light unsampled far too often. The
  last slice is clamped below 1: `(count − 1 + u) / count` rounds to exactly 1.0 for a
  `u` within an ulp of 1, and a pick of 1.0 lands past the whole CDF on the last light
  even when that light has probability zero and a `1e-6` density floor.
  **Only the pick is stratified.** The proposal's first draft stratified the
  point-on-light coordinates by the same slice; with those pmfs the third light is only
  ever picked by slice 3 and its point would only ever come from the top quarter of its
  `(u, v)` square — a bias. The point coordinates are each sample's own draw.
- **Multi-sample MIS** (Veach 1997 §9.2): with `count` light samples against one bounce
  sample, each strategy's weight uses its sample count times its density, so the light
  side's effective density is `count · pdf · pmf`. That count is a parameter of
  `LightList::density(pdf, pmf, samples)`, so every caller — both MIS halves — states
  it: NEE weights each sample with it against the bounce pdf (the guide/BSDF mixture
  where guiding competes) **and divides the sample by it**, since Veach's estimator is
  `Σ_i w_i · f / (n_i · p_i)` and the division is also the average over the `count`
  samples. The first implementation divided by `count · p` and then by `count` again;
  the strategy-agreement test caught it as a floor at 38% of its reference, so do not
  add a `1/count` anywhere. The bounce side (`bounce_emission_weight`,
  `escaped_emission` / `escaped_split`, the hidden-light crossings through
  `bounce_emission_weight_at`) weights with the count `PrevVertex` carries from the
  vertex the ray left (`PrevBounce::nee_count`, `PrevVertex::Phase::nee_count`), so N
  at the first vertex and M after it both pair correctly.
- **Light path expressions**: one `L` event per contributing light sample. The route
  records each NEE entry's light (`Route::nee_lights`) rather than one light per vertex,
  `Route::nee` may be called once per sample, `volume_nee` records its own events (so
  the route's vertex opens before it, and the phase vertex no longer re-draws `K_NEE` to
  learn which light NEE picked), and the gather's per-entry masks moved from a
  `MAX_SPLIT` stack array to a reusable `Vec`. `C.*[LO]` stays pinned bitwise to the
  beauty at (4, 2), (4, 4) and (2, 3) (`the_full_path_expression_is_the_beauty_bitwise`),
  and a partition still sums to it.
- **Pinned**: strategy agreement (light-only, BSDF-only, power and balance MIS, and the
  one-sample renderer) on a diffuse floor under a sphere light, a rect light and a dome
  at (4, 4), (4, 1), (1, 4) and (3, 2), and in `samples/fog.usda` at (1, 4) and (4, 4)
  (`crust-core/tests/light_samples.rs`); the one-sample draws
  (`one_light_sample_draws_as_before`); the import and the CLI parse, a refused 0
  included.
- **Learned light selection** keeps its own training pass and sample count; the
  per-cell pick is a monotone CDF inversion too, so the stratification applies to it
  as it does to the power table.

- **Measured, equal-time** (2026-10-06, 72 threads; the harness: a 1024 spp reference per
  scene with adaptive sampling off and `--indirect-clamp 0`, then for each (N, M) one
  16 spp unclamped render per seed — four seeds through `-f 0..3`, which seeds the
  sampler, for the static scenes; one for ALab, whose frame is its animation — with
  the six configurations interleaved within each seed so load lands on all alike;
  time is the minimum `Render` phase over the seeds, relMSE the mean of
  `crust diff`'s 0.1%-trimmed value against the reference, and efficiency
  `1 / (time · relMSE)` relative to (1, 1). The untrimmed relMSE is unusable here: with
  the clamp off it is firefly-dominated and varies 10 000× between seeds on the
  Playground. `Kitchen_set` has no light and measures nothing. ALab is one seed, so
  its relMSE carries that seed's fireflies, which is why (1, 2) reads above (1, 1).)

  | scene | (N, M) | Render s | shadow rays / vertex | relMSE | time × | relMSE × | efficiency × |
  |---|---|---|---|---|---|---|---|
  | cornellbox | (1, 1) | 0.16 | 0.50 | 0.01003 | 1.00 | 1.00 | **1.00** |
  | cornellbox | (2, 1) | 0.18 | 0.69 | 0.00983 | 1.13 | 0.98 | **0.90** |
  | cornellbox | (4, 1) | 0.22 | 1.05 | 0.01003 | 1.38 | 1.00 | **0.73** |
  | cornellbox | (1, 2) | 0.20 | 0.82 | 0.00813 | 1.23 | 0.81 | **1.00** |
  | cornellbox | (2, 2) | 0.22 | 1.00 | 0.00793 | 1.35 | 0.79 | **0.94** |
  | cornellbox | (4, 2) | 0.26 | 1.37 | 0.00814 | 1.59 | 0.81 | **0.78** |
  | openpbr_showcase | (1, 1) | 0.10 | 0.74 | 0.00267 | 1.00 | 1.00 | **1.00** |
  | openpbr_showcase | (2, 1) | 0.12 | 1.43 | 0.00257 | 1.23 | 0.97 | **0.84** |
  | openpbr_showcase | (4, 1) | 0.15 | 2.80 | 0.00252 | 1.51 | 0.95 | **0.70** |
  | openpbr_showcase | (1, 2) | 0.11 | 0.79 | 0.00253 | 1.07 | 0.95 | **0.99** |
  | openpbr_showcase | (2, 2) | 0.13 | 1.48 | 0.00244 | 1.30 | 0.91 | **0.84** |
  | openpbr_showcase | (4, 2) | 0.17 | 2.85 | 0.00238 | 1.65 | 0.89 | **0.68** |
  | veach_mis | (1, 1) | 0.28 | 0.90 | 0.00681 | 1.00 | 1.00 | **1.00** |
  | veach_mis | (2, 1) | 0.35 | 1.52 | 0.00385 | 1.22 | 0.56 | **1.45** |
  | veach_mis | (4, 1) | 0.47 | 2.78 | 0.00262 | 1.64 | 0.38 | **1.59** |
  | veach_mis | (1, 2) | 0.31 | 1.17 | 0.00681 | 1.10 | 1.00 | **0.91** |
  | veach_mis | (2, 2) | 0.38 | 1.80 | 0.00384 | 1.33 | 0.56 | **1.34** |
  | veach_mis | (4, 2) | 0.51 | 3.05 | 0.00261 | 1.78 | 0.38 | **1.47** |
  | instancing | (1, 1) | 0.07 | 0.71 | 0.00526 | 1.00 | 1.00 | **1.00** |
  | instancing | (2, 1) | 0.09 | 1.27 | 0.00486 | 1.28 | 0.92 | **0.84** |
  | instancing | (4, 1) | 0.12 | 2.40 | 0.00459 | 1.65 | 0.87 | **0.70** |
  | instancing | (1, 2) | 0.08 | 0.85 | 0.00488 | 1.14 | 0.93 | **0.94** |
  | instancing | (2, 2) | 0.09 | 1.42 | 0.00449 | 1.31 | 0.85 | **0.89** |
  | instancing | (4, 2) | 0.12 | 2.54 | 0.00422 | 1.72 | 0.80 | **0.73** |
  | nested_instancing | (1, 1) | 0.08 | 0.71 | 0.00248 | 1.00 | 1.00 | **1.00** |
  | nested_instancing | (2, 1) | 0.10 | 1.37 | 0.00182 | 1.20 | 0.73 | **1.14** |
  | nested_instancing | (4, 1) | 0.13 | 2.67 | 0.00143 | 1.61 | 0.58 | **1.08** |
  | nested_instancing | (1, 2) | 0.08 | 0.77 | 0.00238 | 1.04 | 0.96 | **1.01** |
  | nested_instancing | (2, 2) | 0.10 | 1.43 | 0.00171 | 1.22 | 0.69 | **1.19** |
  | nested_instancing | (4, 2) | 0.13 | 2.73 | 0.00133 | 1.59 | 0.53 | **1.17** |
  | smoke | (1, 1) | 0.26 | 0.91 | 0.1115 | 1.00 | 1.00 | **1.00** |
  | smoke | (2, 1) | 0.28 | 1.13 | 0.1112 | 1.11 | 1.00 | **0.90** |
  | smoke | (4, 1) | 0.32 | 1.57 | 0.1111 | 1.26 | 1.00 | **0.80** |
  | smoke | (1, 2) | 0.37 | 1.59 | 0.1088 | 1.44 | 0.98 | **0.71** |
  | smoke | (2, 2) | 0.41 | 1.81 | 0.1086 | 1.62 | 0.97 | **0.63** |
  | smoke | (4, 2) | 0.47 | 2.26 | 0.1084 | 1.84 | 0.97 | **0.56** |
  | fog | (1, 1) | 0.08 | 0.86 | 0.0634 | 1.00 | 1.00 | **1.00** |
  | fog | (2, 1) | 0.09 | 1.11 | 0.0634 | 1.12 | 1.00 | **0.89** |
  | fog | (4, 1) | 0.10 | 1.63 | 0.0631 | 1.25 | 0.99 | **0.80** |
  | fog | (1, 2) | 0.09 | 1.46 | 0.0619 | 1.22 | 0.98 | **0.84** |
  | fog | (2, 2) | 0.10 | 1.71 | 0.0619 | 1.36 | 0.98 | **0.76** |
  | fog | (4, 2) | 0.12 | 2.23 | 0.0615 | 1.53 | 0.97 | **0.68** |
  | OpenPBR Shader Playground, `renderCam_main` | (1, 1) | 1.07 | 0.64 | 0.1868 | 1.00 | 1.00 | **1.00** |
  | Playground | (2, 1) | 1.17 | 0.86 | 0.1743 | 1.10 | 0.93 | **0.97** |
  | Playground | (4, 1) | 1.42 | 1.28 | 0.1696 | 1.33 | 0.91 | **0.83** |
  | Playground | (1, 2) | 1.38 | 1.07 | 0.1500 | 1.29 | 0.80 | **0.96** |
  | Playground | (2, 2) | 1.49 | 1.29 | 0.1377 | 1.39 | 0.74 | **0.98** |
  | Playground | (4, 2) | 1.69 | 1.71 | 0.1328 | 1.58 | 0.71 | **0.89** |
  | ALab frame 1004 (one seed) | (1, 1) | 3.92 | 0.42 | 0.873 | 1.00 | 1.00 | **1.00** |
  | ALab | (2, 1) | 3.97 | 0.58 | 0.768 | 1.01 | 0.88 | **1.12** |
  | ALab | (4, 1) | 4.18 | 0.88 | 0.560 | 1.07 | 0.64 | **1.46** |
  | ALab | (1, 2) | 4.02 | 0.70 | 1.002 | 1.02 | 1.15 | **0.85** |
  | ALab | (2, 2) | 4.15 | 0.85 | 0.868 | 1.06 | 0.99 | **0.95** |
  | ALab | (4, 2) | 4.25 | 1.16 | 0.634 | 1.08 | 0.73 | **1.27** |

  What it says. The camera count pays where direct light from area lights is the
  noise and the first hit is diffuse: `veach_mis` (4, 1) is **1.59×** more efficient,
  ALab (4, 1) **1.46×** (relMSE 0.64× for 7% more time: ALab's shadow rays are cheap
  beside its shading, 0.42 per vertex at one sample), `nested_instancing` 1.08–1.19×.
  It loses where the direct term is not the noise: dome-lit `cornellbox` (76% of its
  shadow rays occluded), the glossy `openpbr_showcase` and `instancing` (0.70× at
  (4, 1)), and the volume scenes `smoke` and `fog`, whose noise is the medium's (0.80×),
  and on the Playground, whose first hits are mostly glass and glossy spheres: there
  the camera count buys little and the indirect count more ((2, 2) 0.98×, since the
  first diffuse vertex is behind the glass), but neither pays. The indirect count M
  never pays on its own: it multiplies along the path for a smaller share of the
  noise, and only helps when the camera count is also raised. **Both defaults stay
  at 1** (the design's rule: positive everywhere or not at all); `--light-samples 4`
  is the recommendation for interiors and area-lit sets with diffuse first hits, and
  no default-change proposal follows from this measurement. What would move the
  default is an adaptive split (Rath et al. 2022, `docs/light_sampling.md` §7.4),
  which would take the extra samples only where the (4, 1) column is above 1.

## UsdLux import

- **UsdLux units follow the spec** (`LightAPI` in OpenUSD's `usdLux/schema.usda`), and
  where the spec's prose and OpenUSD's reference implementation — hdEmbree's
  `lightSamplers.cpp`, the delegate that grew the UsdLux reference — disagree, crust
  follows the implementation and says so in `lux.rs`. Every light shares
  `lux_params`: `intensity · 2^exposure · color` is the emitted **luminance in nits**,
  times `blackbody_rgb(colorTemperature)` when `enableColorTemperature` is on
  (hdEmbree's Krystek-locus → Rec.709 conversion, luminance-normalised, so 6500 K is
  (1.044, 0.983, 1.036) and not quite white — the spec text claims white, the older
  table-driven `blackbody.cpp` admits it is not). **`inputs:normalize`** divides by the
  light's `sizeFactor`: its **world-space surface area** for rect / disk / sphere /
  cylinder (transform scale included — `AffineShape::area` integrates it, exact for a
  disk and any similarity), `π·sin²θmax` for a distant light (`distant_size_factor`),
  and nothing for a dome. `inputs:diffuse` / `inputs:specular` are read as the light's
  `LobeGains` (below). **`ShapingAPI`**
  (`lux::Shaping`) — focus, focus tint, cone angle + softness, IES — is a per-direction
  factor on area lights' radiance, measured off the light's −Z. It is read off the prim
  whether or not the API is applied, but an unauthored `cone:angle` falls back to the
  schema's **90° only when `ShapingAPI` is applied** and to 180° otherwise: that is what
  Hydra hands hdEmbree (the attribute does not exist without the API), and applying 90°
  unconditionally would cut the back off every sphere light ever authored. IES profiles
  cross the `AssetLoader` seam (`load_ies` → `Arc<IesProfile>`); `crust-assets::ies` is
  a port of the Cycles LM-63 reader hdEmbree vendors, minus its type-A infinite loop.
  The profile's 0° is the light's −Z, `ies:normalize` *divides* by the profile's mean
  intensity (the spec's formula says multiply; every implementation divides), and
  `ies:angleScale` is the spec's bimodal remap. Sample: `samples/usdlux.usda` (every
  light type, `normalize`, colour temperature, a shaped spot, an IES fixture from
  `samples/ies/spot30.ies`, a squashed sphere light, and a textured window card from
  `samples/textures/window_card.exr`).
- **Per-lobe multipliers (`inputs:diffuse`, `inputs:specular`).** Non-physical controls
  (a specular-only kicker for hair highlights, a fill without specular), kept on the
  light list beside the light (`LightList::lobe_gains`, like the LPE tag, so an MCP
  edit's rebuild carries them through `LightUpdate`) rather than folded into its
  radiance, which would scale both lobes. Which lobes answer to which is
  `LobeLabel::is_specular`: specular, coat and transmission are specular; diffuse,
  sheen, subsurface and translucent diffuse. A fibre is one closure leaf whose event
  says only reflect/transmit, so it is split by its terms instead
  (`Hair::r_fraction`): R is specular, TT / TRT / TRRT+ diffuse — the classes the
  issue asking for this named (#341). A gain is a factor of the integrand, never of an
  MIS weight, and nothing it changes steers a sampling decision (selection power,
  BSDF and light sampling, roulette all ignore it), so an unguided image is exactly
  linear in the pair — `lobe_gains.rs` pins that pixel by pixel, which is the sharp test of a
  one-sided gain. Path guiding is the exception, by design: its field trains on the
  gathered radiance, gains included, as it trains on a light's intensity — a guide
  that learned the ungained light would send samples toward a highlight
  `inputs:specular = 0` has removed. A guided render is therefore not linear in the
  pair, only unbiased at each. *NEE*: a gained light's connection is made share by share
  (`gained_split`: `eval_lobes`, each share times `blend(specular_fraction)`), the
  mixed-fibre transmittances per share as before. *Bounce side*: the vertex records
  the specular share of its own sample's value (`PrevBounce::specular`, from the split
  `scatter_split` made at the sampled direction, or the delta event's label), and the
  emission it then reaches — a hit light, a crossed hidden light, an escape, a link
  twin — is scaled by `gains.blend(specular)` (`bounce_gain`). That ratio is exact for
  the beauty: `factor · Le · Σ gᵢfᵢ/Σ fᵢ = Σ gᵢ fᵢ/pdf · Le`. Volume scattering takes
  the light whole (Arnold's `volume` multiplier is a separate control). A scene with no
  gained light pays one well-predicted branch at each emission site and the split
  request (`LightList::has_lobe_gains`); the splitting itself is out of line.
- `UsdLuxDistantLight` → a `DistantLight` in the light list only (no scene geometry). It
  points down its local -Z; `inputs:angle` is the source's angular *diameter* (default
  0.53°, the sun's) and a zero angle is widened to `MIN_DISTANT_ANGLE_DEG` rather than
  made singular, so the integrator keeps one MIS path instead of a delta special case.
  **`intensity` is the sun disk's luminance in nits** — so an un-normalised 0.53° sun
  needs an intensity in the tens of thousands to light anything, exactly as in Hydra.
  With **`inputs:normalize`** it is the **illuminance in lux** on a surface facing the
  light, and widening the angle softens shadows without changing exposure. A zero angle
  is a delta light whose intensity the spec and hdEmbree both deliver as illuminance.
  All four cases reduce in `emit_distant_light` to one number — the illuminance the
  *authored* cone delivers (`distant_illuminance`, f64) — handed to
  `DistantLight::new`, which spreads it as radiance over the cone crust actually
  samples, dividing by `projected_cone_solid_angle` evaluated from the **same f32
  cosine** that bounds the cone. That is what makes the delivered lux exact: at the
  sun's size the f32 cosine is a few ulps from 1, so the cone crust samples differs
  from the authored one by up to 0.5% of its solid angle, and at 0.01° its cosine is
  exactly one. Widening to `MIN_DISTANT_ANGLE_DEG` therefore preserves the illuminance
  and moves only the penumbra. *History:* crust used to treat `intensity` as irradiance
  unconditionally and spread it as `E / Ω` with `Ω = 2π(1 − cos θ)` computed in f32 —
  whose cancellation made a 1.5° sun 2e-4 too bright — so `samples/domelight.usda` now
  authors `normalize = 1` to keep the look it was lit with. Bounce rays find it by *escaping*
  along a direction inside its cone, which is the `Light::escaped` half of MIS.
- `UsdLuxDomeLight` → a `DomeLight`: an infinite environment covering every direction
  (`Light::escaped` answers for every escaping ray). There is **no built-in sky**: with
  no light at infinity an escaping ray is black. The procedural gradient that used to
  stand in was removed with camera visibility, because a camera-invisible dome would
  otherwise have shown it behind the scene (`samples/cornellbox.usda`, which has no
  light, was lit entirely by it). Radiance is `intensity × color × 2^exposure` (× the colour
  temperature's blackbody; `normalize` does not apply to a dome) times an optional
  lat-long `EnvironmentMap`; only `latlong`/`automatic` `texture:format` is supported and
  anything else warns and falls back to the uniform colour. The prim's *rotation* orients
  the sky (a dome is at infinity, so its translation and scale are meaningless).
  **Orientation follows the UsdLux schema**, which adopts the OpenEXR lat-long
  convention ("latitude 0, longitude 0 points into positive z direction; and latitude 0,
  longitude pi/2 points into positive x direction", longitude running from +π at the left
  edge to −π at the right): in the light's own frame +Y is the top row, **+Z the image
  centre**, +X at u = ¼ and −X at u = ¾ (`environment.rs`, `u = ½ − atan2(x, z)/2π`). This
  is Typhoon's `_DirectionToLatLongUv` exactly, pinned by
  `mapping_matches_the_reference_delegate`. *History:* crust used to put −Z at the centre
  (`u = ½ + atan2(x, −z)/2π`, reasoning that a USD camera looks down −Z). That turned every
  textured dome 180° about its +Y. On the OpenPBR Shader Playground the HDRI's sun then
  landed behind the room instead of shining through its window. The four samples whose
  `sky_env.exr` dome was rotated by eye under the old convention now author
  `rotateY = 200` instead of 20, which keeps their sun where it was. The map
  is importance-sampled by luminance × sinθ — the Jacobian matters, without it polar
  texels are over-sampled — which is what keeps a small bright sun in an HDRI from
  becoming a firefly farm.
  - **`UsdLuxDomeLight_1` is the same dome, turned onto its `poleAxis`.** It derives from
    `NonboundableLightBase`, not `DomeLight`, so `DomeLight::get` (an `is_a` on the
    registry) never matches it: until `honour-dome-pole-axis` crust skipped it without a
    word, and a stage lit only by one rendered black with `crust check` reporting 0
    lights. Both schemas now feed one `emit_dome_light` through `DomeInputs`, each view
    reading its own texture attributes. The pole is a `Mat3A` folded in as
    `R_world · R_pole`: the identity, or +90° about X (+Y → +Z, +Z → −Y) when `poleAxis`
    is `Z`, or `scene` (the fallback) on a stage whose root layer's `upAxis` is `Z`. That
    is OpenUSD's `_GetDomeOffset` (`usdImaging/domeLight_1Adapter.cpp`,
    `GfRotation(GfVec3d(1, 0, 0), 90.0)`). Because it turns the dome's own frame, the
    schema's "not inherited by namespace children" holds by construction, and radiance,
    sampling and pdf see the one rotated frame. It costs nothing per ray. Pinned in
    numbers by the four-band tests in `usd_inline.rs` (`a_dome_light_1_on_a_z_up_stage_…`
    and its siblings). On JungleRuins retyped as `DomeLight_1`, the render is
    bit-identical to the stock dome hand-rotated by the same matrix.
    - **Strict on `DomeLight`.** A `DomeLight` keeps +Y even when it authors `poleAxis`,
      which its schema does not declare; OpenUSD's adapter reads it only through
      `UsdLuxDomeLight_1`. *Trap:* JungleRuins (`def DomeLight` + `poleAxis = "scene"` on
      a Z-up stage) looks like a crust bug, a sideways sky with no sun, but it is the
      asset's error: OpenUSD's own imaging ignores that `poleAxis` too.
    - **Deliberately not Typhoon.** hdEmbree imports `DomeLight_1` but never applies the
      pole: its light reads only `SampleTransform` (`delegate/light.cpp`), and none of
      hdEmbree's 222 sources mention `domeOffset`, `poleAxis` or `upAxis`, although its
      copy of `domeLight_1Adapter.cpp` is Pixar's verbatim (OpenUSD `typhoon/main`
      70c45e8). On a `DomeLight_1` whose pole resolves to +Z, crust follows the schema;
      everywhere else the two agree.
  - **crust-core decodes nothing.** `inputs:texture:file` is resolved against the USD
    layer's directory and handed to the host through the `AssetLoader` trait
    (`Scene::from_usd_with_assets`); `Scene::from_usd` passes `NoAssets`, which warns and
    falls back to the uniform colour. `crust-assets` implements it with `exr` (OpenEXR)
    and `image` (`.hdr` and LDR, the latter un-gamma'd to linear). This is the seam
    general texture support should grow through.
- The four **area lights** are one-sided `Emissive::light` surfaces paired with an
  `AreaLight`:
  - `UsdLuxRectLight` → two emissive `Triangle`s + `AreaLight(RectShape)` (local XY plane,
    emitting along −Z). The emitting normal is −Z under the *normal* transform
    (`±edge_u × edge_v`), not the transformed −Z, which stops being perpendicular to the
    rectangle under a shear; the transformed axis is kept whenever it is perpendicular,
    so ordinary lights render as before. **`inputs:texture:file`** multiplies the
    emission per point (`lux::RectTexture` on the light's `Emissive`, read by both MIS
    halves through `radiance_toward(p, …)`): image top row at the light's +Y edge,
    left column at −X, **nearest-texel** — all three hdEmbree's `_SampleLightTexture`
    conventions — and `normalize` still divides by the area. The map crosses the seam
    as `AssetLoader::load_light_texture` → `LightTexture`, linear **float** RGB decoded
    by `crust_assets::read_rgb_image` (the dome's decoder, EXR / `.hdr` kept as
    authored, LDR un-gamma'd), *not* the UV-texture path, which narrows to 8 bits when
    preloading. The light is sampled by the solid angle it subtends (see `RectShape`
    under "Core traits"), not by the map.
    Sample: `samples/rectlight.usda`.
  - `UsdLuxSphereLight`, `UsdLuxDiskLight` (local XY plane, emitting along −Z) and
    `UsdLuxCylinderLight` (along local X, emitting from its side and **not** its end caps)
    share `emit_round_light`: the light's radius / length are folded into the transform
    of a `UnitShape`, and where that is a similarity over the axes the shape is round in
    the geometry is the kernel's world-space analytic `Sphere` / `Disk` / `Cylinder`;
    under a non-uniform scale (an ellipsoid, an ellipse, an elliptical tube) it is the
    unit primitive placed by an `Instance`, which the kernel intersects exactly under
    any affine map, sampled by an `AffineShape`. The uniformly scaled sphere keeps the
    historical `SphereShape` so existing scenes stay bit-identical — but its radius
    **now includes the transform's scale**, which it used to ignore. `treatAsPoint` /
    `treatAsLine` are ignored (hints for renderers without area lights, which the schema
    lets an area-light renderer ignore).
  The source geometry is camera-invisible by default (`light_ray_mask`), and then a
  transparent emitter — see the `crust:rayMask` bullet above for the opt-ins. Sample: `samples/light_visibility.usda`.
  `MeshLightAPI` on a `Mesh` is read ("Mesh lights" below); `PortalLight`,
  `GeometryLight`, `VolumeLightAPI` and light filters are not. Light and shadow linking
  are, as below.
- **Camera visibility of lights at infinity.** A dome or distant light is visible to
  camera rays by default. `crust:light:cameraVisible`, else
  `primvars:ri:attributes:visibility:camera`, hides it (`infinite_light_escape_mask`),
  and the render setting `domeLightCameraVisibility = false` (or
  `crust:domeLightCameraVisibility`, which wins) hides them all, whatever each light
  authors (`LightList::hide_infinite_from_camera`). The mask lives in `LightList`
  beside the infinite-light index and is tested against the escaping ray's own mask in
  `escaped_emission`: a camera ray carries `MASK_CAMERA` and every continuation,
  including delta reflection and refraction, phase and carried-medium scatters,
  carries `MASK_INDIRECT`. That is the definition of a camera ray. Neither
  `depth == 0` nor `prev.is_none()` would do: the latter also holds after a
  carried-medium scatter. Hiding a light from camera rays moves no MIS weight,
  because camera rays have no NEE competitor.
- **Light linking.** Each receiver (the prim that brought a geometry in) has a
  *light class*, deduplicated over every linked light's answers, in a per-`geom_id`
  table (`World::light_class`); each linked light a bitset of the classes it
  illuminates (`LightList::illuminates`). A light that does not illuminate a
  receiver contributes zero on **every** strategy that could deliver it, judged on
  the same receiver: surface NEE skips it after the pick (the pick pmf is
  unchanged, so `density` and the bounce side still describe one strategy),
  `bounce_emission_weight` and `escaped_emission` zero it against the vertex the
  bounce *left* (`PrevBounce::class`), including after a delta bounce, and
  `light_cache::train` skips it. A volume region has a class too
  (`VolumeRegion::light_class`): volume NEE skips an excluding light, and a phase
  scatter's bounce and escape are judged against it (`PrevVertex::Phase::class`).
  A point where overlapping regions of different classes meet is lit by every
  light (`EVERY_CLASS`), and so is the camera, which has no receiver. The cost is wasted NEE
  picks on unlinked lights. Per-class renormalisation of the pick is the fix, and
  it is a `density` / `pmf` pair change (a follow-up).
- **Shadow linking** (design D3 of the change). Occluder classes are encoded in
  the kernel mask's free bits 3–31, and only when some light authors a restricted
  `shadowLink`, so an unlinked scene's masks and rays are untouched. Class 0 (what
  every restricted light is shadowed by) keeps `MASK_SHADOW`; every other caster
  clears it and carries one bit, 3–30 for the 28 most populated classes and 31
  shared. An unrestricted light's rays carry `MASK_SHADOW | bits 3–31` and match
  every caster. A restricted light's carry `MASK_SHADOW` and its classes' bits,
  and the overflow bit only if it includes every overflow class; one including
  some but not all is refused with a `WARN` and shadowed by everything. Volume
  regions carry the same encoding (`VolumeRegion::mask`, tested in
  `active_intervals`), so an excluded volume does not attenuate. Inner prototype
  geometry keeps its own mask: every shadow ray carries `MASK_SHADOW`, so it matches
  inside any instance its class bit let it into.
  A shadow-linked light's bounce side is its **link twin** (`tracer/path.rs`,
  `link_twin`; change `mis-for-shadow-linked-lights`). The ordinary bounce ray is
  stopped by occluders the light's shadow rays ignore, so the two strategies would
  disagree on its visibility, and for a while the light was NEE's alone at continuous
  vertices. That removed the half of MIS that keeps glossy reflections quiet: on ALab
  the distant sun, NEE-only, was 66% of the frame's firefly variance. Now:
  - **The twin.** At a continuous surface, volume-region or enclosure vertex, after the
    bounce direction ω and its pdf (the guide mixture when guided) are drawn and the path
    survives roulette, each *twinned* light (`LightList::twinned_lights`: restricted, not
    NEE-only) is asked where ω meets it (`LightKind::found_along`: `LightShape::hits` for
    an area light, every hit, nearest first, skipping points that emit nothing toward
    the vertex; `escaped` for a distant light). For each point a fresh shadow ray runs
    `shadow_transmittance` with the light's own mask, so it sees exactly NEE's
    visibility, crossings and volumes included. The emission × transmittance is weighted
    as `bounce_emission_weight_at` would weigh a hit there (same `density`, same `pmf_at`,
    `unopposed` where NEE never samples the point), added to the vertex's `crossed`
    (scaled by the continuation factor in the gather, like a bounce hit) and routed as a
    crossed light (`Route::cross`: the lobe's event, then `L`). Its draws come from
    `K_LINK_TWIN`, one sub-domain per light, so nothing else moves. A weight of 0 (under
    `--strategy light`) casts no ray.
  - **One owner.** The ordinary bounce collects nothing from a twinned light at a
    continuous vertex, under every strategy (`LightList::bounce_skips`, read by
    `bounce_emission_weight_at`, hence hidden-light crossings, and by `escaped_emission`
    / `escaped_split`), and NEE weighs it with the ordinary `light_weight`. So
    `--strategy bsdf` honours shadow links too, and light, bsdf and MIS estimate one
    image.
  - **Why a fresh ray, not the bounce ray continued past excluded occluders.** The
    bounce crosses cutouts and thin walls stochastically (`P/q`) and NEE by
    transmittance: one cutout the light ignores would weigh `1 − α` on one side and 1 on
    the other. Getting that right needs a second crossing walk with the light's mask,
    which is a shadow ray anyway.
  - **Why an analytic support test, not a kernel query.** Finding only light L through
    the kernel would need a mask bit per light; those bits are spent on shadow classes.
    The pair test `analytic_light_hits_match_the_kernel` fires 4 000 rays per shape
    (sphere, rect, affine sphere/disk/cylinder) at the kernel's own geometry for the
    light: hit or miss agree, distances within 1e-4·t.
  - **Domes stay NEE-only** (`LightLinks::nee_only`, now a restricted dome only): a
    dome's support is every direction, so a twin is a shadow ray on every bounce (+84%
    shadow rays on ALab), and ALab's restricted dome carries no measurable noise. Revisit
    if a restricted dome shows up in a noise attribution; the cheap version is a twin
    only when the bounce escapes or stops on geometry the dome ignores.
  - **Delta vertices are unchanged**: after a mirror or glass bounce the light is found
    at full weight through the real occluders, so a mirror shows the physical shadow.
  - **Cost.** `trace_path` is monomorphised on `TWINS` (as on `MEDIA`): a scene without
    a twinned light runs an integrator without the call sites. Guarded at run time it
    cost cornellbox +0.22% instructions — the extra call site changed the inlined
    integrator's register allocation, and keeping the `ShadingPoint` alive for its curve
    flag was another 9 instructions a vertex (the twin now takes the flag from the bounce
    ray, which carries it per lobe). Monomorphised, cornellbox's integrator runs 69 k
    *fewer* instructions and the run +0.014% (the wider per-pixel dispatch); binary
    +300 KB, build time unchanged.
  - **Measured** on the linked glossy test scene (`glossy_linked_scene` in
    `tests/light_linking.rs`: rough metal 0.15, a sphere light behind a ball its link
    excludes, 16², 64 spp, depth 2 — so the first bounce may escape to a light at
    infinity rather than end on the depth cap's lookup — clamp off, 4 seeds). Before (the twin off): light
    0.808, bsdf **0.0037**, power 0.808 mean luminance, and power's two-seed variance
    equal to light's (20.80). After: light 0.808 ± 0.007, bsdf 0.792 ± 0.012, power
    0.793 ± 0.005, and power's variance **5.61 (−73%)**. ALab: see "Measured on ALab"
    below. `samples/light_linking.usda` (diffuse receivers) moves by relMSE 2.5e-9 at
    16 spp against a seed-to-seed 3.0e-3, every unlinked sample bit-identical.
  *Measured on ALab* (frame 1004, 256 spp, `--light-samples 4 --light-selection
  learned`, two seeds; method and full table in `docs/alab_profile.md`, "Shadow-linked
  lights and their noise"): `lgt_sun_distant`'s direct-glossy variance falls
  **3 551 → 100 (−97.2%)**, its worst pixel's noise 39 → 3.6, and the frame's variance
  −34%. The rect sun moves −7%, as predicted (its noise is a glint's coverage), and the
  NEE-only dome not at all. Means agree: the NEE-only estimate of the sun's glossy term
  is heavy-tailed and read 6–11% low at 256 spp, and climbs to within 0.5σ of the
  twin's at 1024.
  Render time +2.3% (`bench_ab.sh`, n = 3), against a +5% budget.
  **`CRUST_LINK_TWIN` stays** (the change's open question): its `0` is the NEE-only
  estimator exactly, the A/B the measurements above need, at the cost of one read at
  import.
  *Throughput* on unlinked scenes (callgrind, `usdlux`, `-s 2`, single thread,
  against the renderer before camera visibility and linking): **+1.3%
  instructions**; `bench_ab.sh` +1.1% on `usdlux`, +0.7% on `cornellbox`. That is
  the per-vertex cost of the class lookup, the three `links` tests on NEE and the
  escape filter. A first build measured +2.9%, and more than half of that was
  LLVM no longer inlining `trace_path` into `render_pixel`. It is now
  `#[inline(always)]`; check the inlining before blaming a check.
  Linked lights are resolved on the index stage (payloads unloaded): a streamed
  chunk's population mask leaves out prims a nested collection names, which
  resolved it to nothing.
- **Lights linked to nothing, and backdrops.** A light whose `collection:lightLink`
  covers no receiver illuminates nothing and is *removed* from `LightList` rather
  than weighted to zero in it. No pmf, `density`, light cache or guide can then
  mention it, so the NEE ↔ bounce pair stays consistent by construction. A
  camera-visible light at infinity of that kind is a **backdrop**
  (`LightList::backdrops`, outside selection): a camera ray that escapes sees the
  backdrops *alone*, as a surface at infinity in front of every other light, and no
  other ray sees them. That is the Moana island's `sky_dome_cam_llc` (visible sky,
  lights nothing) in front of `sky_dome_env_llc` (the HDRI). An area light of that
  kind keeps its geometry masked to `MASK_CAMERA` (or to nothing), so no bounce can
  find it. How "covers no receiver" is decided is in the `usd-scene-import` record.
  *Comparison:* hdEmbree/Typhoon has the same global `domeLightCameraVisibility`, but
  on a camera escape it **sums every visible dome and ignores links**, and it has no
  per-dome camera visibility (`visibleInPrimaryRay` is for area-light shapes). It
  would show the island's two skies added together. Its linking is receiver-based on
  Hydra categories, judged against the previous vertex for bounces, which is the
  model crust's light linking takes.

## Hidden lights are transparent emitters

A rect, sphere, disk or cylinder light whose source the camera does not see — the
default — is an emitter and nothing else. `light_ray_mask` gives its geometry
`MASK_INDIRECT` alone, so no shadow ray meets it, and marks it
(`WorldBuilder::set_transparent_emitter`). A bounce segment that meets a marked
geometry collects its emission and continues along the same line as if the source were
absent: `pass_cutouts` / `pass_walls` record a `Crossing` and restart past it, sharing
the 256-crossing budget with cutouts and thin walls; past the budget the source is met
as a solid emitter. A crossing spends no depth, records no vertex, and leaves the
previous vertex's MIS record to whatever the segment reaches. Its emission goes to the
record of the vertex the segment left, weighted by `bounce_emission_weight_at` for its
own light (whole after a carried-medium scatter, which has no MIS record), and
attenuated by what the segment carried to it: thin-wall passes, the carried medium
(`medium_arrival`) and the volume regions, which `volume_event_crossing` samples a piece
at a time between the crossings — a scatter before a crossing means it was never
reached. Past the depth limit the last segment collects them with ratio-tracked
transmittance (`K_CROSS`). `VertexRec` holds the crossings' weighted sum (`crossed`,
plus `crossed_raw` for guiding training) so the gather adds one term; the light path
expressions keep each crossing apart (`Route::cross`), one `L` after the pass-through
events before it, and `C.*[LO]` stays the beauty bit for bit.

- **Why both sides.** Dropping `MASK_SHADOW` alone makes NEE count a light hidden
  behind another while the bounce stops at the nearer one, so the bounce share of the
  farther light is lost: bias. Typhoon (hdEmbree, OpenUSD `typhoon/main` 70c45e8) builds
  geometry for a light only when it is `visibleInPrimaryRay` (default false), so a
  hidden light occludes nothing; its bounce side picks the nearest finite light
  analytically, so there a nearer light still hides a farther one. crust passes through
  on both sides.
- **The measurement that forced it.** `samples/veach_mis_portable.usda` (four hidden
  sphere lights in a row): before, the four-light render was darker than the sum of the
  four single-light renders by up to 31% (G) at the wall's right edge and 3% (B) at its
  left — the big sphere shadowing its neighbour and the reverse — and 1.0000 in the
  middle. After, summed over twelve column bands of the wall's top 100 rows, the
  ratio is 1.000 everywhere (largest deviation 0.0005 at 64 spp), and its RMS falls
  0.00040 → 0.00024 → 0.00009 at 16 → 64 → 256 spp: noise, not bias. `veach_mis.usda`
  itself authors `crust:light:cameraVisible = 1`, so its spheres stay solid and it is
  unchanged.
- **Cost.** A world with no hidden source never enters the walk
  (`World::has_bounce_pass_throughs`). Cornellbox (a dome only) renders bit-identically
  at +0.12% instructions (callgrind, 1 thread, 2 spp: 4 231.7 M → 4 236.8 M): the two
  `VertexRec` sums and one add per vertex in the gather. The larger `trace_path` first
  pushed `ShadingPoint::scatter_importance` out of line (+0.30%); it is
  `inline(always)` for that reason.
- **What moved.** `scripts/check_images.sh` against goldens of the renderer before
  this change: the 15 samples with no hidden area light are bit-identical (with
  `rectlight` and `light_visibility`, whose single one-sided source has nothing behind
  it), and the 22 with one move. relMSE of the new image against the old, both at
  1024 spp, unclamped: `light_linking` 3·10⁻⁹, `thin_window` 1·10⁻⁷, `aovs_lpe`,
  `hair`, `aovs`, `usdpreview_textured`, `materialx_basic`, `animation`,
  `materialx_subsurface`, `curves`, `motionblur` 3·10⁻⁵–2·10⁻⁴; `usdlux` 4·10⁻⁴,
  `materialx_cutout` 6·10⁻⁴, `materialx_lion` 7·10⁻⁴, `materialx_teapot` 1.5·10⁻³,
  `instancing` 1.7·10⁻³, `subdivision` 1.8·10⁻³, `nested_instancing` 3·10⁻³;
  `cornellbox_guided` 0.013, `materialx_showcase` 0.019, `fog` 0.037, `smoke` 0.17. The
  last four have a source just below a ceiling or wall the camera sees: the patch behind
  it, which bounces used to reach only through the source's dark back, was black and is
  now lit (`smoke`'s top row goes 0.19 → 0.49) — the relative error of a black pixel
  turning grey. On `fog` and `smoke` light-only agrees with power MIS within 0.1% at
  512 spp; BSDF-only reads ~1% high on both, exactly as it did before this change.
- **Solid on request.** A camera-visible source (`crust:light:cameraVisible = 1`,
  RenderMan's camera visibility) is solid and occludes, like a lamp bulb. An authored
  `crust:rayMask` decides the mask and keeps the source solid on the bounce side
  whatever bits it clears: `crust:rayMask = 6` is a hidden light that occludes, as
  every hidden light did before.
- **Shadow linking.** A hidden source has no `MASK_SHADOW`, so `encode_shadows` gives it
  no class bit and no shadow ray, restricted or not, meets it.
- **One-sided only.** A bounce through a closed source meets it twice; the inside wall,
  which NEE never samples, must emit nothing. Imported sources are one-sided
  (`Emissive::light`); a hand-built transparent emitter must be too.
- **Trap: the light cache's receivers.** The learned selection's training walk stops at a
  hidden source it hits (an emitter does not scatter). Its visibility is
  `surface_visibility`, so the shadow side is consistent; only the placement of its
  receivers differs, which steers selection and moves no expectation.

## Mesh lights

A `UsdGeomMesh` with `UsdLuxMeshLightAPI` applied is a light-list entry: NEE samples
its triangles and a bounce that hits it is MIS-weighted against that, like every other
area light. It is opt-in, as Arnold's `mesh_light` and RenderMan's `PxrMeshLight` are:
emissive geometry without the API keeps its BSDF-only emission, and there is no
setting that turns every emissive mesh into a light. Typhoon / hdEmbree does not read
the API (its `lightSamplerCommon.h` records only the intent of a shared emitter
sampler), so there is no reference implementation; the decisions below are crust's.
Sample: `samples/mesh_light.usda` (and its API-less twin, `mesh_light_no_api.usda`).

- **One `AreaShape::Mesh` per mesh.** `MeshShape` (`light/mesh.rs`) holds the world-space
  triangles of one placement (36 bytes a triangle) and the CDF of their areas (4 bytes
  a triangle). `sample_point_on(u, v)` picks a triangle by `u`, rescales `u` within
  its share so one pair of numbers drives both choices, and draws a uniform point on
  it (the square-root map). With a uniform radiance, picking by area is picking by
  power, and the area density is `1 / A` everywhere, so `inv_pdf_area` keeps the
  default `area()`. `solid_angle_sampler` is `None`, and the existing `AreaLight` code
  handles NEE, the bounce density, the refusal of an edge-on point and `power()`
  unchanged; `kind()` is `"mesh"`. *Rejected:* one light per triangle (pbrt-v4), which
  breaks `by_geom`'s one geometry to one light, multiplies the light list, the power
  table, the tree and `learned`'s grid by the triangle count, and dilutes the
  defensive shares; a separate `Light` implementation, which would duplicate
  `sample_li` / `pdf_at_point` / `bounds`.
- **The triangle reaches the density through the hit's `prim_id`.** A mesh has no normal
  at a bare point, and a point on a shared edge names two triangles. So
  `Light::pdf_at_point` and `LightShape::normal_at` take the primitive the point lies
  on: the bounce side passes `WorldHit::prim_id` (`bounce_emission_weight_at`, and a
  hidden-light `Crossing`), NEE the triangle `sample_point_on` drew, and
  `ShapeHits` carries one per hit for `found_along`. `MeshShape::normal_at` recomputes
  that triangle's geometric normal from its own table, so both halves compute one
  cosine; the analytic shapes ignore the argument. The triangles are copied in the
  kernel's order — a baked placement's world vertices are the ones `flush_meshes`
  bakes (`transform_point3a` of the same local vertices), an instanced one's are its
  local triangles through its transform — so `prim_id` indexes the light's table.
  Adding the argument changed no image and no instruction of the render (callgrind,
  `cornellbox -s 2`: every tracer function's count identical; the whole run's
  4,278,949,105 → 4,279,039,068, the difference in the import's hashing and rayon's
  scheduling). NEE on a mesh goes through its own `AreaLight::sample_mesh`, out of
  line: threading the triangle through the analytic shapes' `sample_li` (a
  `(point, prim)` from the shape, a fourth dispatch arm) cost `veach_mis`, which has
  no mesh light, 2.3% of `sample_li`'s instructions; split, what is left is the one
  variant test, +1.0% of `sample_li` and +0.047% of the run (callgrind, `-s 2`).
  *Rejected:* passing the hit's geometric normal, which the kernel and the light
  compute by different code; looking the triangle up from the point, a spatial query
  per bounce hit and ambiguous on edges.
- **One radiance for every ray, through a wrapper material.** The mesh's geometry is
  bound to `MeshLightMaterial { inner, emission }` (`material/mesh_light.rs`), which
  forwards everything to the bound material but emission: `emitted`,
  `emitted_directional`, `emitted_at` and the `Resolution`'s emission all answer the
  light's `Emissive`, the same `Arc` the `AreaLight` holds, so camera, bounce and NEE
  read `Emissive::radiance_toward`. An `inner` that is a plain `OpenPBR` is resolved
  to its parameters as they stand (`ResolvedOpenPBR::of_plain`) instead of queried in
  place, where its own emission would answer; the `resolve.rs` pin covers the wrapper
  over six kinds of material. Under `noMaterialResponse` the geometry is bound to the
  `Emissive` itself, as a UsdLux light's is. It is two-sided (`Emissive::new`), as
  material emission is. The wrapper costs one virtual call per hit on mesh-light
  geometry only, and is per placement: a material shared with prims that are not
  lights is not wrapped there.
- **`light:materialSyncMode`, read as one radiance.** The schema has camera and other
  rays see the material's glow and "light rays" the light's inputs. Read literally,
  NEE and the bounce would see two radiances for one emitter, and the image would
  depend on the MIS weights. Crust reads each mode as one radiance seen alike by every
  ray, with `L = color · 2^exposure · intensity` (times the blackbody under
  `enableColorTemperature`, divided by the world area under `normalize`):
  `materialGlowTintsLight` (the fallback) is `glow · L`, where glow is
  `Material::uniform_emission` (materials design record); `independent` is `L`, the
  material still reflecting; `noMaterialResponse` is `L` and no surface response.
  With the inputs unauthored `L = 1`, so applying the API to an emissive mesh changes
  its noise, not its brightness. An unknown token is refused (`light.unknown_token`)
  and read as the fallback. *Comparison:* RenderMan's `PxrMeshLight` follows the
  schema's split.
- **What cannot be a mesh light** is refused with a `skipped` warning and keeps
  BSDF-only emission: a glow that is not uniform under `materialGlowTintsLight`
  (`light.mesh_light_varying_emission`: textured, primvar-driven, coated or fuzzed); a
  cutout material (`light.mesh_light_cutout`); a mesh inside a native instance's or a
  `PointInstancer`'s prototype (`light.mesh_light_instanced`), whose hits report a
  forwarded id that does not name the placement; a mesh that moves over the shutter
  (`light.mesh_light_moving`); the API on a prim that is not a `Mesh`
  (`light.mesh_light_not_a_mesh`); and a zero radiance (`light.mesh_light_no_emission`).
  A mesh whose triangles enclose no area is `light.degenerate_shape`.
- **Built on the traversal, after the placement.** `mesh_light::plan` reads the inputs
  and the refusals before the mesh is placed; `place_mesh_source` then hands back the
  placement's `geom_id` and its world triangles (the interned slot's local triangles,
  or a committed slot's through `Scene::triangle_vertices`, under the placement's
  transform — subdivision and displacement already applied), and `mesh_light::emit`
  builds the shape, divides by its area under `normalize`, rebinds the slot's material
  (`WorldBuilder::set_material`) and adds the light. The change's design placed this
  at `flush_meshes`; it is on the traversal instead because the light is then added
  while its prim is the one being visited, as every UsdLux light is, so its linking
  collections, its render-pass membership and its live-edit origin follow without a
  second path. The triangles are the same either way.
- **Visibility: solid geometry, with the light's camera rules.** A mesh light is
  visible to camera rays and solid by default, being scene geometry. `crust:rayMask`
  decides outright; otherwise `crust:light:cameraVisible`, then RenderMan's
  `primvars:ri:attributes:visibility:camera`, can hide it from the camera only
  (`mesh_light_ray_mask`). It is never a transparent emitter — that path intersects
  the analytic shape — so a hidden mesh light still occludes, as a masked UsdLux
  light does.
- **Light tree bounds.** `MeshShape::extent()` is the triangles' world box with no
  single normal, so a two-sided mesh is bounded with `θo = π` (as a sphere is) and its
  flux is `Emissive::peak_flux(area)`, doubled for two sides. A flat card is bounded
  no tighter than a sphere would be; per-triangle leaves are the gap below.
- **Live edits.** Applying or removing the API, or editing a mesh light's inputs or
  its material, falls back to the full re-import: `emit_light` does not rebuild a mesh,
  and the slot's `MeshLight` kind fails the material `Signature` check, so the
  "edit in place ↔ re-import" pair is not widened.

## Known gaps: light sampling

- **Light sampling is the main source of 16 spp noise**, and `docs/light_sampling.md`
  is the survey of the state of the art (SIGGRAPH/EGSR/HPG, pbrt-v4, Cycles,
  RenderMan, Arnold, Hyperion) with a ranked roadmap and a measured baseline. In
  short: the light pick is by power, defensively, by default (4.6× lower relMSE on a key
  among dim fills, 6% higher on `veach_mis`, §3.8 there; `--light-selection uniform` is
  the bit-identical A/B), with the light tree opt-in (`--light-selection tree`: 1.86×
  lower on `usdlux`, 11–22× on `many_lights`, a tie on `veach_mis`, §3.15 there; see
  the next gap); several light samples per vertex are a setting, not the default
  (`crust:lightSamples`, "Several light samples per vertex" above: 1.46–1.59× at equal
  time on ALab and `veach_mis`, a loss on dome-lit, glossy and volume scenes);
  sphere lights now sample their visible cone
  (1.3–12.9× lower relMSE at 16 spp on the five sphere-lit samples for ~8% more time
  per sample, §3.7 there),
  and rect lights their spherical rectangle (1.4–1.5× lower relMSE on near panels and
  fog, but 4–9% *higher* on the glossy `materialx_basic`/`usdpreview_textured` tiles, at
  ~110 ns more per NEE sample, §3.9 there); tube lights sample their visible arc,
  equiangularly along the wall (1.3–20× at equal time, "Disk and tube lights" above);
  disk lights still sample by area by default. (NEE runs its three tests
  cheapest first: the light's radiance, then the BSDF `eval`, then the shadow
  ray — for every material now, since `eval` goes through the vertex's
  `ShadingPoint` and reads no texture. Textured materials used to trace the ray
  first (`eval_reads_textures`, retired), because a per-query network run made
  their `eval` dearer than the ray: on ALab 87–97 s against 66 s. Either order
  is bit-identical, since the shadow ray draws from its own `K_NEE_SHADOW`
  domain, §3.11 there.)
  Measure changes with
  `crust diff ref.exr test.exr`'s `relmse:` against a 1024 spp reference (§8 there).
- **The light tree is opt-in, and sees no occluders.** It ranks lights by flux,
  distance, emission cone and the receiver's horizon ("Light tree" above), so a
  light behind a wall keeps its full rank: `learned`'s half of the problem, and
  52% of ALab's NEE waste (§3.12). Its importance treats the receiver as
  diffuse, so a glossy highlight from a small light is not favoured (the
  trimmed relMSE on `veach_mis` is 4% worse per sample). A large light near
  the set loses its orientation in the conservative bound and is over-picked
  where it faces away ("Light tree" above: 0.61× `power` at equal time on
  `materialx_showcase`). Lights at infinity keep their fixed `1/live` share. It
  stays opt-in: the equal-time gate fails on 6 of the 11 samples that build a
  tree, and ALab and the OpenPBR Shader Playground are unmeasured (§3.15 there). `power` pays about
  0.6% more instructions for the receiver every vertex carries.
- **`neural` selection loses at equal time** on every checked-in sample and on
  ALab, whose problem is visibility alone: 0.79× `learned` on its direct
  lighting, 0.69× on its full image (the lighting design above;
  `docs/light_sampling.md` §3.14). Its inputs are
  position only, so it learns the local average of a normal-dependent horizon,
  as `learned`'s grid does; it learns visibility only, leaving distance and
  orientation to the power table, which is most of what `learned` exploits on
  the samples; and with a fixed 8 epochs a small frame trains a few dozen Adam
  steps and stays near V̂ = ½.
- **The tube's density is cosine-blind.** It follows the `1/r²` along the wall but
  neither cosine, so a thick tube within a few radii of a receiver is still noisier
  than it need be. Gamito 2016 samples a finite-radius cylinder exactly in solid angle,
  by rejection, which would break `openqmc`'s stratification; a non-rejecting version
  is the pointer.
- **The disk's ellipse is opt-in and expensive.** It loses at equal time on glossy
  receivers and small disks, and costs ~5,000 instructions per sample, most of them in
  Carlson's `R_J` (`EllipseMap::omega_r`). A closed-form `R_C` inside `R_J` and fewer
  `Π` per sample (the strategy's `Ω` is recomputed on the bounce side) are the obvious
  savings; neither would win the glossy cases, which it loses in relMSE as well.

## Known gaps: lighting

- **Mesh lights, phase 1.** Triangles are sampled by area, not by the solid angle
  they subtend (Arvo 1995's spherical triangles are the follow-up), so a large card
  close to a receiver is noisier than an equal `RectLight`, which samples its
  spherical rectangle: 7.2× at equal time with a 1 × 1 quad two units from the floor,
  26× at a quarter (`docs/light_sampling.md` §3.16, which also has what the API buys:
  5% noise at ≈16 spp instead of ≈750 on `samples/mesh_light.usda`). A mesh is one light-list entry under every selection, bounded
  as a whole: the tree ranks a flat card by a loose box, and a per-triangle power
  table or triangles as tree leaves would tighten it. Only a uniform glow can be a
  light under `materialGlowTintsLight` (a textured one needs the material evaluated
  at the NEE sample: a synthetic hit with interpolated primvars). Cutouts, instanced
  and moving meshes, `GeometryLight`, `VolumeLightAPI` and emissive curves are not
  lights. `light:materialSyncMode` is one radiance for every ray, not the schema's
  split. The light copies its triangles (40 bytes each) beside the kernel's, which is
  nothing for a card and a second copy for a dense mesh light. A shadow-linked mesh
  light's bounce-side estimate (`found_along`) scans its triangles per bounce.
- **Lighting caveats.** `GeometryLight`, `PortalLight`, light filters and `ShadowAPI`
  are not read. Light and shadow linking are read, with
  these gaps: membership is judged on the prim that brought the geometry in, so a
  collection target inside a native instance's prototype, or one `PointInstancer`
  instance, cannot be told apart from its siblings (warned per collection);
  `membershipExpression` is refused with a `WARN` and read as the default; a
  shadow-linked *dome* is NEE-only at continuous vertices (noisier on glossy
  receivers; its `bsdf`-only render sees it through the physical occluders); and every
  shadow-linked light is physically shadowed through delta vertices (a mirror shows the
  occluder's shadow). Of RenderMan's per-light `visibility:*` primvars
  only `camera` is read. A textured
  `RectLight` is sampled by solid angle rather than by its map's luminance (a card
  with a small bright region is noisier than it need be), its lookup is nearest-texel
  as the reference's is, and a `.tex` (RenderMan) map is not decoded.
  A light's per-lobe multipliers (`inputs:diffuse` / `inputs:specular`) are exact in
  the beauty and in NEE's per-lobe AOV shares, but its emission reached by a bounce is
  routed to the light path expressions at the bounce's *blended* gain on every lobe of
  that vertex: the routes still sum to the beauty, while `C<RD>L` and `C<RG>L` of a
  diffuse-only light share its bounce-side half between them (the gather's per-lobe
  tables would need the gain per lobe, and the escape's terminal level per lobe of the
  vertex before it). Shaping is per *direction* only, so a shaped light
  is still sampled without regard to its cone (by solid angle for a rect, by area otherwise): a narrow spotlight wastes the NEE samples its cone
  cuts off (unbiased, but noisier than cone-aware sampling), and shaping is not applied
  to distant or dome lights (as in hdEmbree). IES evaluation is bilinear, as the
  reference's is. A tube light samples the arc of wall facing the shading point; a
  squashed sphere samples its visible cone. `DomeLight` sampling is nearest-texel with no bilinear filtering, so a
  low-resolution HDRI shows texel edges in a mirror; `inputs:texture:format` values other
  than `latlong` are refused rather than mapped wrongly (so a `DomeLight_1`'s pole only
  matters for a lat-long map); and the default light-list picking by
  power is blind to position, orientation and visibility, so a far light gets as many
  shadow rays as an equally powerful near one (`--light-selection tree` sees position
  and orientation, and `learned` visibility), and a dome or sun only its uniform share
  however much it lights, under every selection. Neither infinite light
  is visible to the guiding field's spatial structure (they have no position).
