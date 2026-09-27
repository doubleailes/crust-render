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
  the cap and both MIS sides evaluate it at the point. `AffineShape` disks and tubes are
  still area-sampled.
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
  an authored `crust:rayMask` wins outright, and shadow/indirect rays always see it) —
  the `AreaLight` records the geometry's `geom_id`, which is how the integrator
  attributes a bounce-hit emissive surface to its light (`LightList::find_by_geom`).
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
  - **`learned`** (opt-in, `light_cache.rs`; `docs/light_sampling.md` §3.12) is
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
    - **MIS.** Both sides go through `LightList::pick_at` / `pmf_at` /
      `find_by_geom_at` / `iter_at`, keyed by the vertex NEE sampled from. The
      bounce side passes `prev.pos`. Route a new pmf read through the `*_at`
      form or emission is double-counted.
    - **Robust grid bounds.** The grid spans the receivers' 2–98% quantiles.
      With the full bounds, ALab's stray exterior bounces made it 4 cells.
    - **The defensive share is uniform, at 0.3.** Mixing with power reinstates
      the hidden lights, and at 0.2 `domelight` fireflied at shadow boundaries.
    - **Power table underneath.** The cache only installs over a power table:
      with none, `density` divides by `n` and would disagree with the per-cell
      pmf.

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

  The light strategy's MIS density is `light.pdf · pmf`, computed by
  `LightList::density` on **both** sides. `pick`, `find_by_geom` and `iter` all hand
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
  and nothing for a dome. `inputs:diffuse` / `inputs:specular` ≠ 1 warn and are ignored
  (per-lobe multipliers crust's transport does not split). **`ShapingAPI`**
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
- `UsdLuxDomeLight` → a `DomeLight`: an infinite environment covering every direction, so
  once one exists it **replaces the built-in sky gradient** (`Light::escaped` answers for
  every escaping ray). Radiance is `intensity × color × 2^exposure` (× the colour
  temperature's blackbody; `normalize` does not apply to a dome) times an optional
  lat-long `EnvironmentMap`; only `latlong`/`automatic` `texture:format` is supported and
  anything else warns and falls back to the uniform colour. The prim's *rotation* orients
  the sky (a dome is at infinity, so its translation and scale are meaningless). The map
  is importance-sampled by luminance × sinθ — the Jacobian matters, without it polar
  texels are over-sampled — which is what keeps a small bright sun in an HDRI from
  becoming a firefly farm.
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
  The source geometry is camera-invisible by default (`light_ray_mask`) — see the
  `crust:rayMask` bullet above for the opt-ins. Sample: `samples/light_visibility.usda`.
  `PortalLight`, `GeometryLight` / `MeshLightAPI`, `VolumeLightAPI`, light filters and
  light linking are not read.

## Known gaps: light sampling

- **Light sampling is the main source of 16 spp noise**, and `docs/light_sampling.md`
  is the survey of the state of the art (SIGGRAPH/EGSR/HPG, pbrt-v4, Cycles,
  RenderMan, Arnold, Hyperion) with a ranked roadmap and a measured baseline. In
  short: the light pick is by power, defensively (4.6× lower relMSE on a key among dim
  fills, 6% higher on `veach_mis`, §3.8 there; `--light-selection uniform` is the
  bit-identical A/B); sphere lights now sample their visible cone
  (1.3–12.9× lower relMSE at 16 spp on the five sphere-lit samples for ~8% more time
  per sample, §3.7 there),
  and rect lights their spherical rectangle (1.4–1.5× lower relMSE on near panels and
  fog, but 4–9% *higher* on the glossy `materialx_basic`/`usdpreview_textured` tiles, at
  ~110 ns more per NEE sample, §3.9 there), but disk/tube lights still sample by area
  rather than solid angle; the built-in sky
  gradient is not a light, so NEE never samples it. (NEE runs its three tests
  cheapest first: the light's radiance, then the BSDF `eval`, then the shadow
  ray — for every material now, since `eval` goes through the vertex's
  `ShadingPoint` and reads no texture. Textured materials used to trace the ray
  first (`eval_reads_textures`, retired), because a per-query network run made
  their `eval` dearer than the ray: on ALab 87–97 s against 66 s. Either order
  is bit-identical, since the shadow ray draws from its own `K_NEE_SHADOW`
  domain, §3.11 there.)
  Measure changes with
  `exr_diff ref.exr test.exr`'s `relmse:` against a 1024 spp reference (§8 there).

## Known gaps: lighting

- **Lighting caveats.** Mesh lights (`MeshLightAPI` / `GeometryLight`), `PortalLight`,
  light filters, light/shadow linking and `ShadowAPI` are not read. A textured
  `RectLight` is sampled by solid angle rather than by its map's luminance (a card
  with a small bright region is noisier than it need be), its lookup is nearest-texel
  as the reference's is, and a `.tex` (RenderMan) map is not decoded.
  `inputs:diffuse` / `inputs:specular` warn and are ignored rather than split per lobe. Shaping is per *direction* only, so a shaped light
  is still sampled without regard to its cone (by solid angle for a rect, by area otherwise): a narrow spotlight wastes the NEE samples its cone
  cuts off (unbiased, but noisier than cone-aware sampling), and shaping is not applied
  to distant or dome lights (as in hdEmbree). IES evaluation is bilinear, as the
  reference's is. A tube light samples non-uniformly in world area (correct, not
  optimal); a squashed sphere samples its visible cone. `DomeLight` sampling is nearest-texel with no bilinear filtering, so a
  low-resolution HDRI shows texel edges in a mirror; `inputs:texture:format` values other
  than `latlong` are refused rather than mapped wrongly; and light-list picking by
  power is blind to position, orientation and visibility, so a far light gets as many
  shadow rays as an equally powerful near one, and a dome or sun only its uniform share
  however much it lights (a light BVH, `docs/light_sampling.md` §6.3, is the fix). Neither infinite light
  is visible to the guiding field's spatial structure (they have no position).
