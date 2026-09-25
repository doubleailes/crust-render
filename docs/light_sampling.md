# Light sampling: the state of the art, and where crust stands

Why a crust render is noisy at 16 spp, what the literature and the production
renderers do about it, and the order in which to change crust. Written the way
`docs/ptex_streaming.md` and `docs/color_management.md` are: as the record to
consult *before* touching `light.rs` or the NEE block in `tracer.rs`, not as an
announcement.

It started from a remark by a former Pixar engineer, on seeing a 16 spp crust
image: *that is too noisy — look at your light sampling strategy.* The remark
holds up, and not for one reason but for several. Most of them are independent
of each other, and a few are cheap.

---

## 1. Summary

What crust's direct lighting does today, per path vertex:

1. pick **one** light, **uniformly** (`LightList::pick`, `light.rs`). Since
   §9.3 (j) the pick is by power, defensively, and uniform is `--light-selection
   uniform`;
2. sample a point on it **uniformly by area**. Sphere lights are the exception
   since §9.2 (d): they now sample their visible cone. They used to sample the
   whole sphere, including the half that faces away. Rect lights are the
   second since §9.2 (e): they sample the spherical rectangle they subtend;
3. trace **one** shadow ray. It is traced *before* the BSDF or the emission is
   evaluated, so rays whose contribution is already known to be zero are
   traced anyway;
4. combine with BSDF sampling by the power heuristic.

Step 4 is the state of the art, and the measurements below confirm it. On every
sample scene but one, power and balance MIS are 10–1000× better than either
strategy alone. The exception is §3.6's `openpbr_showcase`. Steps 1–3 are
what the literature moved past in 1995–2013, and what every production renderer
does differently.

The recommended changes, ranked by expected noise removed per line of code. §9
has the detail and §8 the measurement protocol.

| # | Change | Layer | Effort | Gain at 16 spp | Changes the image? |
|---|---|---|---|---|---|
| 1 | ✅ **Done.** Sample sphere lights by the **visible cone** (Shirley et al. 1996) | per-light pdf | ~60 lines | **measured 1.3–12.9× lower relMSE** on the five sphere-lit samples, for about 8% more time per sample (§3.7) | noise only |
| 2 | ✅ **Done, without the warp.** **Spherical-rectangle** sampling for rect lights (Ureña et al. 2013); the bilinear cosine warp (Hart et al. 2020) is still open | per-light pdf | ~200 lines | **measured 1.4–1.5× lower relMSE** on a near softbox and in `fog`, but **4–9% higher** on the glossy tiles of `materialx_basic` / `usdpreview_textured`, at 5–22% more render time (§3.9) | noise only |
| 3 | ✅ **Done, defensively.** Power-proportional light pick, with the dome/sun given a fixed share and half the rays spread evenly | selection | ~250 lines | **measured 4.6×** on a key among dim fills, 2.6% on `usdlux`, **−6%** on `veach_mis` (§3.8) | noise only |
| 4 | **More than one light sample at the camera vertex** (RenderMan `numLightSamples`, Arnold per-light `samples`) | sample count | ~50 lines | ≈ k× less first-bounce direct variance | noise only |
| 5 | Make the **built-in sky** an importance-sampled light rather than an escape-only background | coverage | ~40 lines | small on the open `cornellbox` (measured 0.0125 relMSE today), large in enclosed dome-less scenes | noise only |
| 6 | ✅ **Done.** Remove the `+1e-4` in `AreaLight::pdf_toward` | correctness | ~10 lines | none: it is a **bias** fix, **measured −11.7%** on a 1.3e-3 m² disk light that it had brightened (§3.10) | yes, slightly: ≤0.44% per pixel on the samples |
| 7 | Evaluate BSDF and emission **before** the shadow ray | cost | ~10 lines | none (bit-identical), fewer rays | no |
| 8 | **MIS compensation** for the dome map (Karlík et al. 2019, as in pbrt-v4) | per-light pdf | ~20 lines | large on sun + sky HDRIs | noise only |
| 9 | **Per-vertex RIS**: M candidates, one shadow ray (Talbot et al. 2005) | selection | ~150 lines | large on glossy surfaces under several lights | noise only |
| 10 | A **light BVH** with orientation cones (Conty Estevez & Kulla 2018; pbrt-v4, Cycles) | selection | ~600 lines | decisive for 10²+ lights, marginal below | noise only |
| 11 | Cone-clipped sampling for shaped lights; image-importance sampling for textured rect lights; equiangular sampling in volumes | per-light pdf | moderate each | large in their niche | noise only |

"Noise only" means the change alters the estimator but not its expectation. The
image moves by noise and nothing else, which §8 shows how to prove.

**The one-line version.** Items 1, 2, 5, 6 and 7 fix defects rather than add
features. Items 3 and 4 are what every production renderer ships. Items 9 and 10
are the state of the art, and pay off in proportion to the number of lights.

---

## 2. What "16 spp" means elsewhere

Before comparing images, compare budgets. A crust sample is one camera path with
**one** shadow ray per vertex, shared across *all* lights. A production
renderer's "16 samples" usually means something else:

- **RenderMan** (`PxrPathTracer`) takes `numLightSamples` light samples and
  `numBxdfSamples` BSDF samples at each camera hit. Pixar's documentation
  recommends setting the two equal for lowest noise, and reduces them with depth
  and throughput by default.
- **Arnold** historically took `samples²` shadow rays *per light* per camera
  sample, on top of `AA²` camera samples. Since 7.2.1 it has one global light
  budget ("Global Light Sampling"), and later releases fold in BSDF glossiness
  and the quad/disk `spread`.
- **Cycles** takes one light sample per vertex by default, like crust. But since
  Blender 3.5 it picks that light from a **light tree** rather than uniformly.
- Almost every production 16 spp frame is also **denoised**. That is out of
  scope here, but it is part of why a production image at 16 spp looks the way
  it does.

So "noisy at 16 spp" is partly an unequal comparison: RenderMan at 16 spp with
`numLightSamples = 4` traces 4× crust's shadow rays at the first bounce. That is
recommendation 4. The rest of this document is about making each of crust's
shadow rays count.

---

## 3. Where crust stands

Read from `crates/crust-core/src/light.rs` and `tracer.rs` at the time of
writing.

### 3.1 Light selection: uniform

*(As audited. Selection is now by power, defensively; §3.8 has what changed and
why the pure form was not shipped.)*

```rust
// light.rs, LightList::pick
let i = (u * self.lights.len() as f32) as usize;
```

The NEE density is `light.pdf / n_lights` on both MIS sides:

- `tracer.rs`, the surface NEE block;
- `volume_nee`;
- `bounce_emission_weight`;
- `escaped_emission`.

A dim fill light, a hidden practical and the key light each get one shadow ray
in N. In `samples/usdlux.usda` seven lights of very different power share the
budget equally (disk, tube, shaped rect, IES sphere, squashed sphere,
textured card, dome), and the dome gets 1/7 of it however dim it is.

### 3.2 Per-light sampling: uniform by area

| Light | Sampling today | What is lost |
|---|---|---|
| `SphereLight` (similarity transform) | **Fixed (§9.2 d):** `SphereShape` now samples the visible cone. It used to sample uniformly over the **whole** sphere. | What area sampling lost: everything outside the visible cap, at least half of all samples. More when close, because the visible cap is `(1 − r/d)/2` of the area. Back-facing samples are occluded by the sphere itself, so each one was a traced shadow ray that returned zero. |
| `RectLight` | **Fixed (§9.2 e):** `RectShape` samples the spherical rectangle it subtends. It used to sample uniformly in `(u, v)`. | What area sampling lost: for a panel large or near relative to its distance, `cos θ_l / r²` varies by orders of magnitude across it. A few near samples dominate, and the QMC stratification is spent on the wrong measure. Sheared parallelograms and tiny lights still area-sample. |
| `DiskLight`, `CylinderLight` | `AffineShape`: uniform in local area | Same as the rect. The tube also samples its far side. |
| Squashed sphere (non-uniform scale) | **Fixed (§9.2 d):** `AffineShape` samples the unit sphere's visible cone in local space. It used to sample uniform in local area. | The same as the round sphere's. |
| Shaped rect/disk (`ShapingAPI`, IES) | uniform by area, shaping applied as a factor | Every sample the cone rejects. A 30° spot wastes most of them. |
| Textured `RectLight` | uniform by area | A card with a small bright region is sampled like a flat one. |
| `DistantLight` | uniform in its cone | Nothing. This is correct. |
| `DomeLight` with a map | piecewise-constant 2D over luminance × sin θ | This is pbrt-v3's recipe, and correct. It lacks pbrt-v4's MIS compensation (§5.3). |
| **The built-in sky gradient** | **not a light at all** | Every scene with no `DomeLight` is lit by an environment that NEE never samples. `samples/cornellbox.usda`, the first command in the README, has **no light prim**: it is lit entirely by that gradient, through pure BSDF sampling. |

That last row does not make `cornellbox` itself very noisy (§3.6 has the
number: its open box lets bounce rays find the sky easily). But it means no
light-sampling change can improve that scene. And in any enclosed scene lit by
the gradient, the whole sky is found by chance alone. See §9.1 for the fix.

### 3.3 A bias, found on the way

*(As audited. Fixed by §9.1 (b); §3.10 has the measurement.)*

```rust
// light.rs, AreaLight::pdf_toward
distance_squared / (cosine * self.shape.inv_pdf_area(light_point) + 1e-4)
```

The epsilon keeps a back-facing sample's pdf finite. But the NEE estimator
divides by this pdf while the sample was drawn from `d²/(cos·A)`, so every NEE
contribution is inflated by a factor `1 + 1e-4/(cos θ_l · A)`.

The MIS weights still sum to one, since both sides use the same number, so this
is purely an NEE bias:

- `veach_mis`'s tiny sphere (`r = 0.05`, `A = 0.031`): +0.3% face-on, +3% at 84°
  from its normal.
- A 1 cm² light in a scene modelled in metres, seen face-on: +100%.

It also makes the bias **unit-dependent**, since it scales with the scene's
metres-per-unit. pbrt-v4 has no epsilon: it returns no sample when the pdf would
be infinite.

### 3.4 Shadow rays traced before the answer is known

The surface NEE block calls `shadow_transmittance` first. It then asks the
material for `eval` and the light for its radiance, both of which can be zero:

- the light is below the surface's horizon;
- the material is delta or transmissive (`eval` returns `None`);
- the light sample is back-facing (`front == false`).

Reordering the checks changes no value, because the shadow ray's transmittance
draws from its own `K_NEE_SHADOW` domain. So the output is bit-identical, and
fewer rays are traced.

### 3.5 Sample-space use

NEE draws one 4D block from `K_NEE`: dimension 0 picks the light, dimensions 1
and 2 give the position.

- With one light that is ideal.
- With N lights, the samples that picked light *k* are the slice
  `u₀ ∈ [k/N, (k+1)/N)` of an Owen-scrambled Sobol set. Its (u₁, u₂) projection
  is still a good net when N is a power of two, and less so otherwise.

The standard fix is pbrt-v4's `SampleDiscrete(..., &uRemapped)`: reuse the
remainder of u₀. But it matters much less than §3.2. **Stratification only pays
through an area-preserving map onto the integrand's own measure.** Stratified
area samples feeding a `cos/r²` weight throw most of it away, which is the
central argument of the Arvo (1995) and Ureña (2013) papers.

### 3.6 Baseline, measured

Protocol as in §8: 1024 spp reference, 16 spp test, `exr_diff` relMSE, same
binary. These are the numbers from **before** any change in §9. §3.7 has what
the first one did.

relMSE, lower is better:

| Scene | Lights | power (default) | balance | light only | bsdf only |
|---|---|---|---|---|---|
| `veach_mis` | 4 spheres, glossy plates | 0.0195 | 0.0190 | 46.2 | 9.86 |
| `cornellbox_guided` | 1 shrouded sphere, guided | 0.0761 | 0.0741 | 0.225 | 0.315 |
| `openpbr_showcase` | 2 spheres, distant | 0.00655 | 0.00636 | 0.00579 | 0.0896 |
| `usdlux` | 7 mixed, incl. dome | 0.137 | 0.135 | 1.14 | 16.5 |
| `domelight` | dome + sun | 0.113 | 0.113 | 243 | 16.3 |
| `cornellbox` | **none** (sky gradient) | 0.0125 | 0.0125 | 0.0125 | 0.0125 |

What the table says:

- **MIS is not the problem.** Power and balance agree to within noise. On all but
  one scene both beat either single strategy by one to three orders of
  magnitude.
  - The exception is `openpbr_showcase`, where light-only *beats* both
    heuristics. That is the situation §7.2's MIS compensation and variance-aware
    MIS address: the heuristic gives BSDF samples weight in a region where they
    only add noise.
- **The per-light pdf is the cheapest noise left.** §3.7 shows the first
  per-light change, sphere cone sampling, cutting relMSE on every sphere-lit
  scene.
- **`cornellbox` confirms no NEE runs there.** All four strategies produce the
  *identical* image. Its relMSE is nonetheless low: the box is open, so bounce
  rays find the sky easily.
  - An enclosed room lit through an opening, or any interior with a dome-less
    sky, is where that absence costs. It is also a trap for anyone measuring
    strategies on the README's first scene: nothing they change in light
    sampling can show up there.

The 16 spp images' noise elsewhere is therefore mostly per-light (§5) and
selection (§6) noise, and the number of shadow rays per camera sample (§2).

### 3.7 After sphere cone sampling (§9.2 d)

The same protocol, with fresh 1024 spp references rendered by the new binary.
The five samples with a uniformly scaled `SphereLight`, relMSE at 16 spp:

| Scene | Sphere lights | area sampling (before) | cone sampling (after) | Gain |
|---|---|---|---|---|
| `light_visibility` | 3, near the objects they light | 0.00242 | 0.000188 | **12.9×** |
| `veach_mis` | 4, sizes 0.05 to 1.35 | 0.0192 | 0.0110 | **1.75×** |
| `cornellbox_guided` | 1, shrouded, guided | 0.0761 | 0.0468 | **1.62×** |
| `usdlux` | 1 (the IES fixture) of 7 | 0.138 | 0.0963 | **1.43×** |
| `openpbr_showcase` | 2, far away | 0.00655 | 0.00498 | **1.32×** |

- **The gain tracks how near and how large the light is,** as §5.2 predicts.
  `light_visibility`'s spheres sit next to what they light, so area sampling
  there wasted most of its samples on the far side and weighted the rest by a
  `cos/r²` that varied wildly. `openpbr_showcase`'s are far away, where area
  sampling was merely half-wasted.
- **It costs about 8% per sample and pays that back.** Measured with
  `scripts/bench_ab.sh` (4 interleaved reps, the scenes' own settings):
  - `veach_mis` has adaptive stopping off, so it takes exactly 128 spp either
    way and measures pure per-sample cost: **+8.4% min, +8.1% mean**. Against
    1.75× lower relMSE that is about 1.6× the efficiency
    (`1 / (time × relMSE)`).
  - The likely source is that area sampling's back-facing samples were cheap
    shadow rays, occluded by the sphere at the first hit, while every cone
    sample is a real connection that must traverse to its end. There are also a
    few more transcendentals per sample.
  - `openpbr_showcase` has adaptive stopping on (threshold 0.05), so pixels
    reach the threshold sooner: **−27.7% min, −27.2% mean render time**, and
    lower noise too.
  - `light_visibility` renders in 5 ms, too little to time.
- **It is unbiased**, checked with **independent sampler seeds**. `-f N` on a
  stage with no time samples renders the same image under frame seed N.
  - A 1024 spp `veach_mis` from the area-sampling binary (`-f 1`) and one from
    the cone binary (`-f 2`) differ by relMSE 1.9e-4. That is under what two
    independent renders of one image should differ by, roughly the sum of their
    16 spp relMSEs over 64, or 4.7e-4.
  - Rendering at 16, 64 and 256 spp (`-f 3`), each binary measured against the
    *other* binary's reference, both converge toward it with no plateau:
    - cone: 0.0103, 0.00163, 0.00045;
    - area: 0.0186, 0.00296, 0.00060.

    Each flattens only at the reference's own noise floor. A bias would stall
    above it.
- **Seeds are the trap in measuring this.** Every binary uses the same seeds
  per pixel and sample index. So a render shares its first N samples with a
  1024 spp reference from the same seed, however much the light sampling
  differs, and the two look closer than they are.
  - An earlier version of this check fell into it: same-seed references
    differing by 8e-5 to 3.3e-4 were read as proof of independence, when they
    were partly correlation.
  - Always give the reference its own `-f`.
- **Unit-tested** in `crates/crust-core/tests/lights.rs`:
  - samples lie on the cap facing the shading point and inside the subtended
    cone, near, far and in the small-angle branch;
  - `cos θ` and the azimuth are uniform (a histogram), which fails when the `u`
    mapping is perturbed;
  - a unit-radiance sphere integrates to the analytic irradiance `π sin² θ_max`;
  - inside the sphere, both MIS sides fall back to area sampling together.

**Squashed spheres.** A `SphereLight` under a non-uniform scale is an
`AffineShape`, not a `SphereShape`, and it sampled the whole surface until the
follow-up change.

- **How it works.** It now samples the *unit* sphere's cone in local space and
  maps the point through the placement.
  - That is exact because an affine map preserves visibility on a convex
    surface. With normals carried by `M⁻ᵀ`,
    `n_w · (x − p) = n_l · (x_l − p_l) / |M⁻ᵀ n_l|`, so a point faces the
    shading point in world space exactly when it does in local space.
  - The world density is the local cone's times the solid-angle Jacobian of the
    direction map `ω ↦ Mω/|Mω|`, which is `|Mω|³ / |det M|`.
  - Unlike the round sphere's, that density varies across the cap, and both MIS
    sides evaluate it at the point.
- **Result.** On a scratch scene of three squashed sphere lights near a floor
  and a wall (320×180, adaptive stopping off; not checked in), relMSE at 16 spp
  went **0.00618 → 0.00430 (1.44×)**.
- **Cost.** `bench_ab` measured **+11.4% min, +12.6% mean** per sample, about
  1.3× the efficiency. The extra transforms and the Jacobian are paid on both
  MIS sides.
- **On `usdlux`**, whose ellipsoid is one dim light of seven, the whole image
  moved 0.0997 → 0.0959 (4%) at no measurable cost (+1.6% min, +0.3% mean).
- **Tests.**
  - `ellipsoid_light_cone_matches_area_quadrature` integrates the subtended
    solid angle and a tilted receiver's irradiance both through the cone and by
    plain area quadrature over the facing side. Dropping one power of `|Mω|`
    fails it.
  - Disks and tubes still take the area path, pinned by
    `affine_shapes_without_a_cone_fall_back_to_area_sampling`.

### 3.8 Picking lights by power (§9.3 j)

What was built, what was measured on the way, and why the default is not the
textbook version.

**The textbook version made things worse.** The first implementation was
pbrt-v4's `PowerLightSampler`:
- every light's pmf proportional to its flux;
- a dome's flux counted as `π L̄ · 4π r²` over the scene's bounding sphere;
- a sun's as `E · π r²`.

relMSE at 16 spp against 1024 spp references under their own seed:

| Scene | uniform | pure power | + ½ even | + fixed infinite share | both (shipped) |
|---|---|---|---|---|---|
| `domelight` (dome + sun) | **1.011** | 1.435 | 1.263 | 1.435 | **1.011** |
| `usdlux` (6 local + dome) | 0.0967 | 0.1426 | 0.0995 | 0.1201 | **0.0938** |
| `veach_mis` (4 tinted spheres) | **0.0105** | 0.0111 | 0.0111 | 0.0111 | 0.0111 |
| `ellipsoids` (3 squashed spheres) | **0.00430** | 0.00446 | 0.00434 | 0.00446 | 0.00434 |
| `materialx_basic` (dome + rect) | 0.00252 | 0.00248 | 0.00270 | 0.00252 | 0.00252 |

Two causes, both documented weaknesses of power selection (§6.1):

- **Visibility.** On `domelight` the sun is 7× the dome by that measure, so it
  took 88% of the shadow rays. But in the sun's own shadows the dome is the
  only light, and there its estimate was ~4× noisier in variance than under a
  50/50 split. A relative metric weighs those dim pixels as heavily as lit ones.
- **Comparability.** A light at infinity has no power that means the same thing
  as a lamp's: its "flux into the bounding sphere" depends on the scene's
  extent and says nothing about whether the scene is enclosed. pbrt-v4's own BVH
  sampler and Karma both sidestep this with a fixed share, so crust does too.

**What ships (`LightSelection::Power`, the default):**
- lights at infinity keep their uniform share;
- the finite lights split the rest half evenly (`DEFENSIVE_SHARE`, Hesterberg's
  defensive mixture) and half by power;
- a black light gets zero.

The even half bounds the damage wherever power is the wrong guide: no light
falls below half its uniform share, so no light's variance more than doubles.

**Four seeds each** (`-f 2..5`), which is what separates a real difference from
the realisation of the noise:

| Scene | uniform | power (shipped) | |
|---|---|---|---|
| `keyfill` (scratch: 1 rect key + 7 dim spheres) | 0.00626–0.00633 | 0.00134–0.00138 | **4.6× lower**, every seed |
| `usdlux` | 0.0955–0.0961 | 0.0930–0.0935 | 2.6% lower, every seed |
| `openpbr_showcase` | — | — | within 0.1% |
| `ellipsoids` | — | — | 0.8% higher, every seed |
| `veach_mis` | 0.0098–0.0186 | 0.0105–0.0191 | **5–8% higher**, every seed |
| `domelight`, `materialx_basic`, `light_visibility` | — | — | unchanged (infinite or equal-power lights) |

Notes on the table:

- **`veach_mis` is the honest cost.**
  - Its lights carry equal *radiometric* power but different tints, so their
    luminance powers differ by ±10% (pmf 0.228, 0.274, 0.275, 0.223).
  - Each lights its own band of the glossy plates.
  - Moving rays from the red and blue lights to the green costs exactly the
    pixels the red and blue own.
  - That is the spatial blindness again, in miniature. No global pmf can
    serve a scene whose lights each own a region; a per-shading-point one can
    (§6.3).
- **Time.** `bench_ab` found no cost: `usdlux` −0.4% / −1.0%, `veach_mis`
  +1.5% / +2.2%, and `keyfill` at 256 spp −5.6% / −1.0% (min / mean).
- **The A/B is exact.** `--light-selection uniform` renders all 22 checked-in
  samples bit-identically to the renderer before (0 differing pixels at 16 spp).
  The density is the historical division `pdf / n`, not `pdf · (1/n)`.

**Power is a flux** (`Light::power`, `Emissive::flux`):
- `π A L` for an unshaped emitter of any shape, twice that two-sided.
- A textured card takes its texture's mean texel.
- A shaped light integrates its shaping over directions in rings about the axis,
  out to the cone angle only, so a 2° spot is resolved as well as a hemisphere
  (pinned against `π A L sin² θ`).

**The table is inverted by CDF, not an alias table**, so the map from the pick
dimension to a light stays monotone. The samples that pick a light are one
contiguous slice of that dimension, as under uniform picking.

**Tests.**
- Probabilities and pick frequencies match the rule, with a dark light never
  picked.
- `find_by_geom`, `iter` and `pick` agree.
- The uniform density is the historical division.
- End to end, a bright and a dim sphere, a sun and a dome estimate the same
  radiance under power and uniform selection, with MIS and with light sampling
  alone. Reverting one MIS side to `1/n` fails it by 18%.

### 3.9 Spherical rectangles for rect lights (§9.2 e)

What was built, what it costs, and where it loses — because it does lose
somewhere, and the table below is the reason not to call it a free win.

**The sampler.** `RectShape` implements the solid-angle hook with Ureña,
Fajardo & King's map (pbrt-v4's `SampleSphericalRectangle`): uniform in the
solid angle the rectangle subtends, pdf `1/Ω`, area-preserving from `[0,1]²`.
- **f64, one `atan2`.** `Ω = Σg − 2π` cancels in f32 at the solid angles where
  area sampling takes over. The setup is not the paper's: in the rectangle's
  frame the four edge-plane normals are axis-aligned in closed form, so the
  corner angle at `(x, y)` is `atan2(h·|v|, ±x·y)`. The map uses only sums of
  angles, and a sum of arguments is the argument of a product, so `Ω` is one
  `atan2` of a four-way complex product. `g2 + g3` is kept as its normalised
  `(cos, sin)`, which the azimuth `a_u = uΩ − (g2 + g3)` takes through the
  angle-difference identities. The paper's recipe (four normalised cross
  products, four `asin`) cost ~30 ns more per sample.
- **The point is returned through the light's own `(s, t)`**, so it lies on the
  light exactly as an area sample does: on the triangles a bounce ray hits, and
  at the texel a textured card reads.
- **Area sampling stays, on both hooks alike** (`RectShape::spherical_rect`):
  - for a sheared parallelogram (edges more than 1e-6 from perpendicular, in
    f64: the map samples an exact rectangle placed through the light's own
    edges, so any shear it let through would be a bias of that order);
  - from behind the one-sided light or on its plane;
  - outside `[1e-4, 6.22]` sr, pbrt-v4's `BilinearPatch` bounds.
- **Textured cards use it too.** pbrt-v4 gives the map up for an image quad
  only because it samples the image instead. crust does not ((h)), so the
  alternative here is area, which follows the map no better.

**Measured.** Same protocol as §3.7: a 1024 spp reference from the *old*
binary under its own seed (`-f 1`), 16 spp from each binary under `-f 2..5`.
The mean over the four seeds is shown; every seed moved the same way.
`softbox` is a scratch scene (not checked in): a 3×2 panel 0.9 above a floor,
tilted toward a ball, with a zero-intensity dome so the sky does not swamp it.
`mx key-only` is `materialx_basic` with its dome zeroed.

| Scene | area (before) | spherical (after) | relMSE | time (`bench_ab` min / mean) | efficiency |
|---|---|---|---|---|---|
| `softbox` (near, large panel) | 0.01880 | 0.01252 | **1.50× lower** | +15.5% / +15.1% | **1.30×** |
| `fog` (rect over a homogeneous volume) | 0.1029 | 0.0729 | **1.41× lower** | +22.5% / +21.7% | **1.16×** |
| `subdivision` | 0.00327 | 0.00324 | 1.0% lower | +1.0% / +5.8% | ~1 |
| `usdlux` (the rect is one of seven) | 0.0927 | 0.0924 | 0.3% lower | +2.2% / +1.4% | ~1 |
| `rectlight` (64×64, sky-dominated) | 0.00179 | 0.00183 | 2% higher | too fast to time | ~1 |
| `usdpreview_textured` (glossy tiles) | 0.00122 | 0.00127 | **4.1% higher** | +12.9% / +9.5% | 0.86× |
| `materialx_basic` (glossy tiles) | 0.00253 | 0.00277 | **9.4% higher** | +7.2% / +5.3% | 0.87× |
| `mx key-only` | 0.00360 | 0.00387 | **7.4% higher** | — | — |

- **The gain is where §5.1 says it is:** a panel near what it lights, whose
  `cos θ_l / r²` varies by orders of magnitude across it, and volume scatter
  points, which see the light from every distance and angle.
- **The loss is real, not a bug.**
  - It is unbiased. The sampler agrees with f64 area quadrature to within
    0.1–0.5σ at 4 M samples, from four points under `materialx_basic`'s own key.
  - For a *diffuse* receiver at those same points it is **1.6–3.9× lower in
    variance** than area sampling, unoccluded.
  - The loss is on the glossy tiles, and it is in the light strategy itself: NEE
    alone (`--strategy light`) is ~20% worse on `mx key-only`. The BSDF side of
    MIS then recovers most of it: 3% worse under balance, 7.5% under power.
  - Solid angle is not the integrand. A glossy lobe's `f` peaks somewhere on
    the panel, and area sampling's density in solid angle, `r²/cos θ_l`, is
    highest on the panel's far, grazing side. Here that side evidently holds the
    tiles' highlights. The coincidence is scene-dependent, but so is every
    sampler that ignores `f`.
- **The cost is latency, not instructions.** Callgrind counts +6.6% instructions
  on `softbox`, but the wall clock says +15%. A `sample_li` microbenchmark puts
  spherical sampling at ~145 ns against ~35 ns by area. That is one f64 `atan2`,
  one `sin_cos` and about a dozen serial `sqrt`/divisions, which the pipeline
  cannot overlap. Moving the post-azimuth half to f32 changed nothing
  measurable. pbrt-v4 and Cycles pay the same price.
- **Scenes without a rect light are bit-identical**: 0 differing pixels at
  16 spp on `veach_mis`, `cornellbox`, `domelight`, `light_visibility` and
  `openpbr_showcase`.

**What would close the gap on glossy receivers** is the other half of item 2,
the bilinear cosine warp (Hart et al. 2020). It needs the receiver's normal,
which `Light::sample_li` / `pdf_at_point` do not take today, so it is an API
change on both MIS sides and `volume_nee` (no normal: no warp). Beyond that,
glossy lobes are what BSDF sampling, RIS (§6.6) and Peters' LTC polygon
sampling (§5.2) exist for.

**Tests** (`crates/crust-core/tests/lights.rs`):
- The pdf is `1/Ω` against an independent formula (Van Oosterom & Strackee,
  two triangles), from above the centre, beyond every edge and corner, and just
  above the 1e-4 sr threshold. Each sample's pdf equals `pdf_at_point` for its
  point.
- Samples fall into a 4×4 grid of cells in proportion to each cell's own solid
  angle. From the test's near point these span an order of magnitude, so plain
  `(u, v)` fails it.
- A unit-radiance panel estimates a tilted receiver's irradiance to 0.2% of area
  quadrature.
- Sheared, behind, on-plane and tiny lights fall back on both hooks together.

### 3.10 Without the epsilon (§9.1 b)

**The change.** `AreaLight::pdf_toward` is `d² / (cos θ_l · A)` with nothing
added to the denominator. A point whose density is not finite (back-facing,
edge-on, degenerate) is refused on both MIS sides: `sample_li` returns `None`,
and `pdf_at_point` returns 0, which `bounce_emission_weight` reads as "NEE
never delivers this point" and answers with full weight.

It touches only area sampling: disks, tubes, and the sphere, ellipsoid and rect
cases that fall back to it (inside the sphere, sheared, tiny or grazing
rects). The cone and spherical-rectangle pdfs never carried the epsilon.

**Measured.** A scratch scene, not checked in: a disk light of radius 0.02
(`A = 1.26e-3`) 0.5 above a diffuse floor, facing down, under a zero-intensity
dome, seen from above at 64×64 with adaptive sampling off. The image mean
against the BSDF-only estimate, which never reads the light's pdf and so is
the unbiased reference both binaries share. The reference is 16 384 spp,
averaged over three seeds: **0.5756 ± 0.0011** (standard error).

| binary | light only, 1024 spp | power MIS, 1024 spp | vs reference |
|---|---|---|---|
| with `+1e-4` | 0.6489 | 0.6489 | **+12.7%** |
| without | 0.5727 (three seeds: 0.57266 / 0.57270 / 0.57273) | 0.5727 | −0.5% |

- **The +12.7% is the predicted `1 + 1e-4/(cos θ_l · A)`.** That factor is 8%
  straight under the disk and grows toward the frame edge as `cos θ_l` falls.
  MIS does not hide it: at this size NEE carries nearly all the weight, so
  power MIS shows the same image as light-only.
- **The −0.5% residual** is 2.6 standard errors from the reference, below
  anything the epsilon could explain. It was not investigated further. On a
  15× larger disk (radius 0.3) the three estimates agree to 0.1%, with the old
  binary 0.15% high.
- **The checked-in samples barely move**, because their lights are large. At
  16 spp:
  - 16 of the 23 are bit-identical (0 differing pixels), including
    `veach_mis`, `domelight`, `rectlight` and `openpbr_showcase`;
  - `usdlux` (disk + tube) and the three DPEL MaterialX scenes change on
    88–94% of pixels by at most 0.44% (max relative difference);
  - `fog`, `smoke` and `instancing` change on under 0.3% of their pixels.

**What else it fixes.** A **two-sided** emitter seen from behind by an
area-sampled shape used to be lost to both strategies. NEE divided by a pdf of
about `d²/1e-4`, and the bounce weight against that pdf was nearly zero. The
bounce ray now carries it at full weight. crust's UsdLux lights are one-sided,
so this only reaches the procedural fallback's two-sided sphere light, seen
from inside.

**Tests.**
- `area_pdf_has_no_epsilon` (`light.rs`) checks `pdf == d²/(cos·A)` to 1e-4
  relative on a small tilted ellipse, where the old epsilon was a 4.5% error.
- `back_facing_area_samples_are_refused_on_both_sides` checks the refusal from
  behind and edge-on.
- `rect_light_is_effectively_one_sided`,
  `affine_shapes_without_a_cone_fall_back_to_area_sampling` and
  `sphere_light_from_inside_falls_back_to_area_sampling` (`tests/lights.rs`)
  used to pin the exploding pdf. They now pin the refusal.

---

## 4. The anatomy of the direct-lighting estimator

One NEE sample at a shading point *x* estimates

```
L_d(x) = Σ_ℓ ∫_{A_ℓ} f(x, ω) L_e(y) V(x, y) G(x, y) dA(y)
```

with the one-sample estimator

```
         w(ω) · f · L_e · V · G
⟨L_d⟩ = ───────────────────────── ,   ℓ ~ P(ℓ | x),   y ~ p_ℓ(y | x)
            P(ℓ | x) · p_ℓ(y | x)
```

Its variance has four separable sources, and each has its own literature:

| Factor | What it is in crust | The question it answers | Section |
|---|---|---|---|
| `P(ℓ \| x)` | `1/N` | *which* light | §6 |
| `p_ℓ(y \| x)` | uniform area | *where* on it | §5 |
| `w(ω)` | power heuristic | how to combine with BSDF sampling | §7 |
| number of samples, their stratification | one, one 4D domain | how many, and how spread | §7.4, §3.5 |

`V` (visibility) is the one factor none of the classic methods model. It is what
the learned methods (§6.5) and Hyperion's cache points add.

**Crust's invariant.** Every change below alters `P` or `p_ℓ`, and the product
`P · p_ℓ` must be evaluated **identically** on the bounce side:

- `bounce_emission_weight` for lights with geometry;
- `escaped_emission` for lights at infinity;
- the phase arm for volumes.

`CLAUDE.md` states this for the uniform case. It becomes more demanding as `P`
starts depending on the shading point and normal (a light BVH) or on the BSDF
(RIS). Every design below says how it meets it.

---

## 5. Sampling one light

### 5.1 Why area sampling is noisy

Sampling `y` uniformly by area gives an estimate proportional to
`f · L_e · cos θ_x · cos θ_ℓ · A / r²`.

- For a small, distant light, `cos θ_ℓ / r²` is almost constant across it, and
  area sampling is nearly optimal.
- For a light that is large relative to its distance, `1/r²` and `cos θ_ℓ` vary
  by orders of magnitude across it.

Sampling by **solid angle** makes the pdf the constant `1/Ω`. Only `f`, `cos θ_x`
and `V` are left to vary. The good maps are area-preserving from `[0,1]²`, so the
Owen-Sobol stratification crust already draws carries straight onto the sphere
of directions.

### 5.2 Per shape

**Sphere: sample the visible cone.**
- Shirley, Wang & Zimmerman, *Monte Carlo techniques for direct lighting
  calculations*, ACM ToG 15(1), 1996.
- From a point outside, the sphere subtends a cone of half-angle
  `θ_max = asin(r/d)`. Sample `cos θ = 1 − u(1 − cos θ_max)` and a uniform
  azimuth, then find the point on the sphere in closed form (law of cosines, no
  ray cast).
- pdf `1/(2π(1 − cos θ_max))`, constant. No sample is wasted on the back.
- pbrt-v4 (`Sphere::Sample(ctx, u)`) falls back to area sampling inside the
  sphere, and switches to a Taylor expansion below `sin² θ_max < sin² 1.5°`,
  where `1 − cos θ_max` cancels catastrophically in f32. Its expansion draws
  `sin² θ = u·sin² θ_max`, whose solid-angle density goes as `cos θ` rather than
  being the constant it reports — a bias of at most `sin² θ_max / 4`, 1.7e-4.
  crust keeps the threshold and replaces the expansion with the exact
  `t = u·sin² θ_max/(1 + cos θ_max)`, `sin² θ = t(2 − t)`.
- Cycles does the same (`kernel/light/point.h`).
- **Everyone does this.** Next step up is **projected** solid angle, which
  samples proportionally to `cos θ_x` as well and is ideal for diffuse. See
  Ureña & Georgiev, *Stratified Sampling of Projected Spherical Caps*, CGF (EGSR)
  2018, and Peters & Dachsbacher, *Sampling Projected Spherical Caps in Real
  Time*, PACMCGIT (I3D) 2019, at 2–3× the cost of plain cone sampling.

**Rectangle: spherical rectangles.**
- Ureña, Fajardo & King, *An Area-Preserving Parametrization for Spherical
  Rectangles*, CGF 32(4) (EGSR 2013).
- A closed-form, area-preserving map from `[0,1]²` onto the rectangle's
  spherical projection: invert the spherical-quad area formula for the azimuth,
  then solve the elevation within that slice.
- pdf `1/Ω`. One `asin`/`acos` pair and a few square roots per sample.
- Requires a true rectangle. Crust's sheared-normal case (see `RectShape`) must
  keep area sampling.
- Used by Cycles (`kernel/light/area.h`, which cites the paper, is Apache-2.0
  and portable) and pbrt-v4 (`BilinearPatch::Sample` when the patch is a
  rectangle with no emission image and `Ω > 1e-4` sr).
- The authors were at Solid Angle, and the paper is hosted by Arnold.
- Pixar compared rect-light samplers in Pekelis & Hery, *A Statistical
  Framework for Comparing Importance Sampling Methods, and an Application to
  Rectangular Lights*, Pixar Tech Memo 14-01.

**The receiver's cosine: composed warps.**
- Hart, Pharr, Müller, Lopes, McGuire & Shirley, *Practical Product Sampling by
  Fitting and Composing Warps*, CGF 39(4) (EGSR 2020).
- Before the spherical-rectangle (or triangle) map, warp `u` by a **bilinear**
  density whose four corner values are `max(0.01, |n · ω_corner|)`. The pdfs
  multiply, so MIS is unaffected.
- pbrt-v4 does this for both triangles and rectangles in about 30 lines. It is
  what turns uniform solid angle into approximately cosine-weighted solid angle.

**Triangles and polygons.**
- Arvo, *Stratified Sampling of Spherical Triangles*, SIGGRAPH 1995: the
  area-preserving map, used by pbrt-v4 for triangle emitters, with a solid-angle
  band `[3e-4, 6.22]` sr outside which it falls back to area sampling.
- The state of the art is Peters, *BRDF Importance Sampling for Polygonal
  Lights*, ACM ToG 40(4) (SIGGRAPH 2021). It samples a convex polygon exactly
  proportional to projected solid angle (diffuse) or to a linearly transformed
  cosine fitted to GGX (specular), and reports nearly noise-free unoccluded
  shading at 2 spp. It is heavier than Ureña + Hart. The LTC foundation is
  Heitz, Dupuy, Hill & Neubelt, *Real-Time Polygonal-Light Shading with Linearly
  Transformed Cosines*, SIGGRAPH 2016.
- Crust has no triangle-mesh lights today (`MeshLightAPI` is not read), so this
  becomes relevant only with mesh lights.

**Disks and cylinders.**
- Gamito, *Solid Angle Sampling of Disk and Cylinder Lights*, CGF 35(4) (EGSR
  2016), is the only direct reference for crust's `CylinderLight`.
- Guillén, Ureña, King, Fajardo, Georgiev, López-Moreno & Jarabo,
  *Area-Preserving Parameterizations for Spherical Ellipses*, CGF 36(4) (EGSR
  2017), treats the disk: a disk projects to a spherical ellipse.
- Both need iterative inversion. Neither pbrt-v4 nor Cycles bothers: both still
  area-sample disks and cylinders. So rank these after everything else.
- For a thin tube, Peters, *BRDF Importance Sampling for Linear Lights*, CGF
  40(8) (HPG 2021), sampling the axis segment, is often the better fit.
- A cheap interim step for the tube is to sample only the half facing the shading
  point.

**Shaped and IES lights: clip the domain.**
- Emission restricted to a cone of half-angle `θ_s` about the light's −Z can only
  reach *x* from points of the light plane within a disk of radius `h · tan θ_s`
  around *x*'s projection, where `h` is *x*'s height above the plane.
- Cycles (`area_light_spread_clamp_light`) intersects that disk with the
  rectangle and samples only the overlap, still by spherical rectangle.
- The same geometry fits UsdLux `shaping:cone:angle` directly. For IES, use the
  profile's largest non-zero angle as `θ_s`.
- The pdf is over the clipped region, on both MIS sides.

**Textured rect lights: sample the image.**
- A 2D piecewise-constant distribution over the emission texture, in area
  measure. pbrt-v4 `BilinearPatch` does this and **gives up** spherical-rectangle
  sampling for image-textured quads.
- Composing an image warp with a cosine warp is Hart et al.'s general answer, but
  no renderer is confirmed to do it.
- For crust, a textured `RectLight` whose map is nearly uniform would be better
  served by solid angle. Choosing between the two per light at import, by the
  map's max/mean ratio, is a reasonable heuristic.

### 5.3 The environment

- **Piecewise-constant 2D over luminance × sin θ** is what crust has, and it is
  correct.
  - pbrt-v4 moved to an **equal-area octahedral** parameterisation (square maps,
    no Jacobian).
  - Cycles keeps lat-long.
  - Nearest-texel lookup is consistent with a piecewise-constant pdf. It is a
    visual issue in mirrors, not a sampling one.
- **MIS compensation.**
  - Karlík, Šik, Vévoda, Skřivan & Křivánek, *MIS Compensation: Optimizing
    Sampling Techniques in Multiple Importance Sampling*, ACM ToG 38(6)
    (SIGGRAPH Asia 2019).
  - When a technique will be MIS'd with others, the variance-optimal pdf for it
    is *not* proportional to the integrand. It should leave to BSDF sampling what
    BSDF sampling already does well.
  - For an environment map the practical rule is to build the distribution from
    `max(L − mean(L), 0)`. The dim, flat sky goes to BSDF sampling, and the sun
    takes the light samples.
  - pbrt-v4 ships exactly this (`ImageInfiniteLight::compensatedDistribution`,
    with `allowIncompletePDF = true` passed to both `SampleLi` and `PDF_Li`). Its
    uniform infinite light then takes **no** light samples at all under MIS.
  - Crust's `DomeLight` needs the compensated pdf on both sides. It is only valid
    where BSDF sampling can reach the direction: MIS on, and not a delta or
    transmission-only vertex. Otherwise it needs the full pdf there.
- **Portals.** Bitterli, Novák & Jarosz, *Portal-Masked Environment Map
  Sampling*, CGF 34(4) (EGSR 2015), samples the environment through a window. It
  is decisive for interiors lit from outside. Cycles has background portals, and
  RenderMan has `PxrPortalLight`. Crust reads no `PortalLight`.
- **Product sampling** of environment × BSDF:
  - Clarberg, Jarosz, Akenine-Möller & Jensen, *Wavelet Importance Sampling*,
    SIGGRAPH 2005;
  - Clarberg & Akenine-Möller, *Practical Product Importance Sampling for Direct
    Illumination*, EG 2008.

  Both exist and are rarely shipped; compensation plus MIS is the production
  answer.

---

## 6. Choosing the light

Selection only matters from two lights up. With one light, `pick` always returns
it and adds no variance. So for `rectlight.usda` or a lone dome, §5 is
everything.

### 6.1 Uniform versus power

Shirley et al. 1996 is again the start: a pmf over lights proportional to
energy.

pbrt-v4 has three selectors (`lightsamplers.h`):
- `UniformLightSampler`;
- `PowerLightSampler`, an alias table over each light's `Phi`, O(1) to sample and
  to evaluate;
- `BVHLightSampler`, **the default**.

The weak point of power selection is **infinite lights**: a dome's "power"
depends on the scene's extent, which pbrt approximates by the bounding-sphere
radius. The production answers differ:
- pbrt-v4's BVH sampler keeps infinite lights outside the tree and gives them a
  fixed share, `n_inf / (n_inf + 1)` — half of all samples for one dome, however
  dim;
- Karma always samples dome and directional lights outside its tree;
- Cycles folds distant lights into its importance measure.

What crust does, having measured pbrt's convention and seen it fail (§3.8), is
a power table over the finite lights with a fixed uniform share for the
infinite ones, like Karma, plus a defensive even half. Pure power selection
made the dome-and-sun sample 1.4× noisier, because the sun took the dome's
rays even in the sun's own shadows.

### 6.2 Spatial selection before trees

pbrt-v3's default was `SpatialLightDistribution`: a voxel grid of up to 64³ over
the scene bounds, each voxel with a lazily built pmf from the lights' estimated
contribution at a few points in it. Conty Estevez & Kulla measured it against
their tree at one shadow ray and 16 spp: noisier, and 90 minutes (mostly
initialisation) against 22 seconds.

It is simple to write, but the literature has moved on from it.

### 6.3 Light BVHs: the standard answer

**Conty Estevez & Kulla**, *Importance Sampling of Many Lights with Adaptive
Tree Splitting*, PACMCGIT 1(2) (HPG 2018; first a SIGGRAPH 2017 talk), from
Sony Pictures Imageworks.

- **Tree.** A binary BVH over emitters. Each node stores:
  - a bounding box;
  - an **orientation cone** (axis, `θ_o` bounding the normals, `θ_e` the
    emission spread, π/2 for Lambertian);
  - summed energy;
  - energy variance.
- **Traversal.** One random number walks the tree, choosing the child with
  probability `I_L / (I_L + I_R)` and rescaling (1D hierarchical warping).
- **Importance.** `I = E · |cos θ'_i| · cos θ' / d²`, where `θ'` is the smallest
  angle any emitter in the node can present toward *x*, and zero outside
  `θ_e`.
- **Adaptive splitting.** Near the root, where bounds are loose, both children
  are traversed whenever a variance estimate says one sample would be
  unreliable. This yields a small cut of 1–20 lights, each sampled.
- **Build.** The *Surface Area Orientation Heuristic* (SAOH), a binned SAH with an
  orientation measure.
- **MIS.** Unbiased. The bounce side recomputes a light's pmf by walking a stored
  bit trail.
- **Reported results.**
  - 300 k lights at 16 spp with splitting beat 256 spp without.
  - On a mesh light: +5 dB PSNR over uniform from distance, and +3 dB more from
    orientation.
- **Stated limits.** No visibility, a diffuse proxy for the BSDF, and a
  hand-tuned split threshold.
- **Adoption.** The descendants:
  - **pbrt-v4 `BVHLightSampler`** (the default; one sample, no splitting;
    `CompactLightBounds` with an octahedral axis and quantised cosines;
    `PMF(ctx, light)` via the bit trail). About 300 lines, and the most direct
    template for a Rust port.
  - **Cycles light tree** (Blender 3.5, on by default). It averages a
    conservative *max* importance with an optimistic *min* one, to stop large
    near clusters being over-sampled, and adds distant lights.
  - **Falcor `LightBVH`**. Moreau & Clarberg, *Importance Sampling of Many Lights
    on the GPU*, Ray Tracing Gems ch. 18, 2019, and Moreau, Pharr & Clarberg,
    *Dynamic Many-Light Sampling for Real-Time Ray Tracing*, HPG 2019, which
    refits a two-level tree for moving lights within 6% of a rebuild.

**Crust specifics.**
- `P(ℓ | x, n)` now depends on the shading normal. So `PrevVertex` must carry
  the previous vertex's normal as well as its position, and
  `bounce_emission_weight` walks the tree to evaluate the same number.
- A volume vertex has no normal: drop the `cos θ'_i` factor there (pbrt-v4 does
  the same), and use Conty & Kulla's `1/d` falloff for media.
- The tree is built at import, next to the light list. It needs no BVH kernel
  code from `crust-rt`, since it is a different tree over a different measure.

### 6.4 Lightcuts and stochastic lightcuts

- **Lightcuts.** Walter, Fernandez, Arbree, Bala, Donikian & Greenberg,
  *Lightcuts: A Scalable Approach to Illumination*, SIGGRAPH 2005.
  - Cluster (virtual) point lights in a tree.
  - Per shading point, pick a cut whose every cluster's error bound is under a
    perceptual threshold, and shade each cluster with one representative light.
  - Biased (bounded). The ancestor of the whole family.
- **Stochastic lightcuts.**
  - Yuksel, *Stochastic Lightcuts*, HPG 2019, and *Stochastic Lightcuts for
    Sampling Many Lights*, IEEE TVCG 27(10), 2021.
  - Each cut node samples a light from its subtree instead of using a fixed
    representative. That trades correlation artefacts for noise, is unbiased,
    and combines importance, adaptive and stratified sampling.
  - Lin & Yuksel, *Real-Time Stochastic Lightcuts*, I3D 2020, adds GPU builds and
    cut sharing.

For crust, stochastic lightcuts are the principled form of the "several lights
at the camera vertex" idea (§7.4) once the light count is large.

### 6.5 Learned and visibility-aware selection

Every method above ignores `V`. In an interior whose key light is behind a wall
for most of the room, they keep aiming there.

- **Donikian, Walter, Bala, Fernandez & Greenberg**, *Accurate Direct
  Illumination Using Iterative Adaptive Sampling*, IEEE TVCG 12(3), 2006.
  Per-block light pdfs refined from earlier passes' samples.
- **Wang & Åkerlund**, *Bidirectional Importance Sampling for Unstructured
  Direct Illumination*, CGF 28(2) (EG 2009).
- **Vévoda, Kondapaneni & Křivánek**, *Bayesian Online Regression for Adaptive
  Direct Illumination Sampling*, ACM ToG 37(4) (SIGGRAPH 2018).
  - Learns light-cluster selection **including visibility**, online, as
    regularised Bayesian regression in a spatial cache.
  - Unbiased, progressive, with almost no preprocessing.
- **Wang, Wu, Li & Chuang**, *Learning to Cluster for Rendering with Many
  Lights*, ACM ToG 40(6) (SIGGRAPH Asia 2021). Adapts the clustering itself
  online, with a convergence proof.
- **Disney Hyperion, "cache points".**
  - Li, Zhu, Nichols, Kutz, Huang, Adler, Burley & Teece, *Cache Points for
    Production-Scale Occlusion-Aware Many-Lights Sampling and Volumetric
    Scattering*, DigiPro 2024. Background in Burley et al., *The Design and
    Evolution of Disney's Hyperion Renderer*, ACM ToG 37(3), 2018.
  - A spatial structure over the points light sampling happens **from**, each
    holding learned, normal-blended light distributions with occlusion folded
    in.
  - Hyperion's default since 2014. The flagship production example.
- **Neural.**
  - Figueiredo, He, Bako & Kalantari, *Neural Importance Sampling of Many
    Lights*, SIGGRAPH 2025. An online-trained network predicts a spatially
    varying cluster pmf over an existing light tree.
  - Bokšanský & Meister, *Neural Visibility Cache for Real-Time Light
    Sampling*, JCGT 14(2), 2025.
- Also: Liu, Xu & Yan, *Adaptive BRDF-Oriented Multiple Importance Sampling of
  Many Lights*, CGF 38(4) (EGSR 2019), and Tokuyoshi, Ikeda, Kulkarni & Harada,
  *Hierarchical Light Sampling with Accurate Spherical Gaussian Lighting*,
  SIGGRAPH Asia 2024. Both make tree importance BSDF-aware: glossy lobes, which
  Conty & Kulla's diffuse proxy misses.

**For crust.** The guided renderer already runs training passes and owns a
spatial structure (`guiding/`'s SD-tree). A per-leaf table of learned light
weights, fed by the same training passes, is the natural route to
visibility-aware selection. It is also what Hyperion's cache points amount to.

The MIS constraint is the usual one: the learned pmf must be frozen for the final
pass, and looked up identically on the bounce side.

### 6.6 Resampling: RIS and ReSTIR

- **RIS.** Talbot, Cline & Egbert, *Importance Resampling for Global
  Illumination*, EGSR 2005.
  - Draw M cheap candidates from a source pdf `p` (uniform or power pick, then
    the light's own sampler).
  - Weight each by `p̂ / p`, where `p̂` is the *unshadowed* contribution
    `f · L_e · G` **including the BSDF**.
  - Keep one, and trace **one** shadow ray.
  - The estimator `f(y) · (1/p̂(y)) · (1/M) Σ p̂(x_i)/p(x_i)` is unbiased when
    `p̂ > 0` wherever `f > 0`.
  - Reported: 10–70% variance reduction over plain importance sampling for
    direct light.
- **ReSTIR.** Bitterli, Wyman, Pharr, Shirley, Lefohn & Jarosz, *Spatiotemporal
  Reservoir Resampling for Real-Time Ray Tracing with Dynamic Direct Lighting*,
  ACM ToG 39(4) (SIGGRAPH 2020).
  - Streams RIS through weighted reservoirs and **reuses** them across frames and
    neighbouring pixels.
  - Follow-ups:
    - ReSTIR GI (Ouyang et al., HPG 2021);
    - GRIS / ReSTIR PT (Lin, Kettunen, Bitterli, Pantaleoni, Yuksel & Wyman,
      SIGGRAPH 2022), the theory;
    - Area ReSTIR (Zhang et al., SIGGRAPH 2024);
    - ReGIR (Boksansky, Jukarainen & Wyman, Ray Tracing Gems II, 2021), a
      world-space reservoir grid.
  - The entry point is the SIGGRAPH 2023 course *A Gentle Introduction to
    ReSTIR* (Wyman et al.).
- **Production.** Conty Estevez, Hellmuth & Lecocq, *A Resampled Tree for Many
  Lights Rendering*, SIGGRAPH 2024 Talks.
  - SPI simplified its tree to spatial-only importance plus stochastic
    splitting, giving 1–60 candidates.
  - It then resamples to about 16 with a directional heuristic, and to 1–4 shadow
    rays with the full light × BSDF evaluation.
  - In production since 2023, with the largest gains on glossy metals and in
    media.

**For crust.** Spatiotemporal *reuse* is a real-time amortisation, and in a
progressive offline renderer it is a liability. It introduces cross-pixel
correlation, which the adaptive stop and the inverse-variance pass blending both
assume away.

**Per-vertex RIS without reuse**, on the other hand, is plain importance
sampling with a better pdf:
- It costs M BSDF and light evaluations but no extra rays.
- It is the only method here that is BSDF-aware for free, which is what an
  OpenPBR glossy surface under several lights needs.

The MIS catch: a RIS-selected sample has no closed-form pdf. Use a *proxy*
density for the MIS weight, the source pdf `P · p_ℓ`, computed **identically on
the bounce side**. MIS weights need only sum to one per path, so this stays
unbiased, merely less than optimal. This is the same "route both sides through
one function" rule `CLAUDE.md` already enforces.

---

## 7. Combining strategies, and how many samples

### 7.1 Heuristics

Veach & Guibas, *Optimally Combining Sampling Techniques for Monte Carlo
Rendering*, SIGGRAPH 1995: the balance, power, cutoff and maximum heuristics,
and one-sample MIS. Crust has balance and power, and §3.6 shows they are not
where the noise is.

### 7.2 Beyond the heuristics

- **Optimal MIS.** Kondapaneni, Vévoda, Grittmann, Skřivan, Slusallek &
  Křivánek, SIGGRAPH 2019. Provably variance-minimising weights, possibly
  negative, from estimated second moments. A learning system, not a drop-in.
- **Variance-aware MIS.** Grittmann, Georgiev, Slusallek & Křivánek, SIGGRAPH
  Asia 2019. Scales the balance heuristic by per-technique variance estimates.
  Crust's adaptive sampler and guiding passes already estimate per-pixel
  variance.
- **MIS compensation** (Karlík et al. 2019, §5.3). The one with a clear, cheap
  production use.
- **Continuous MIS** (West, Georgiev, Gruson & Hachisuka, SIGGRAPH 2020) and
  **efficiency-aware MIS** (Grittmann, Yazici, Georgiev & Slusallek, SIGGRAPH
  2022). Not NEE-relevant for crust.

### 7.3 Stratification

Crust's sampler is OpenQMC's Owen-scrambled Sobol with keyed padding, following
Kollig & Keller, *Efficient Multidimensional Sampling*, EG 2002, and Burley,
*Practical Hash-based Owen Scrambling*, JCGT 2020. Blue-noise variants are in the
crate (Heitz, Belcour et al., SIGGRAPH 2019 talk). Pixar's own progressive
multi-jittered sequences (Christensen, Kensler & Kilpatrick, EGSR 2018) are in
OpenQMC too.

Nothing is missing here. §3.5 is the one refinement, and §5's area-preserving
maps are what make the stratification reach the image.

### 7.4 More samples where they matter: splitting

The camera vertex is where 16 spp noise lives: its direct lighting is the
largest and most visible term, and it is computed once per camera path.
Production renderers split there:
- RenderMan: `numLightSamples` / `numBxdfSamples`;
- Arnold: per-light `samples`, now a global light budget;
- SPI's tree: adaptive splitting.

k light samples at depth 0, each weighted `1/k` inside the same MIS, cost k − 1
extra shadow rays and **no** extra camera paths. That is roughly k× less
first-bounce direct variance, modulo visibility. With OpenQMC they should be k
indices of one padded `K_NEE` domain so that they stratify against each other.
The MIS bookkeeping for k light samples against one BSDF sample is Veach's
multi-sample model: the light strategy's effective density is `k · P · p_ℓ` in
both weights.

The learned version is Rath, Grittmann, Herholz, Weier & Slusallek, *EARS:
Efficiency-Aware Russian Roulette and Splitting*, SIGGRAPH 2022.

---

## 8. Measuring a light-sampling change

Light sampling changes alter the image by noise and nothing else, so what needs
checking is *how much* noise, and proof that nothing *but* noise moved. On top of
`CLAUDE.md`'s "Measuring a change":

1. **Reference.** Render the scene at 1024 spp with the *unchanged* binary, and
   keep it.
2. **Test.** Render at `-s 16`, once per binary. At 16 spp every pixel takes
   exactly 16 samples (below `minSamplesPerPixel`), so the comparison is
   equal-sample.
3. **Metric.**
   `exr_diff ref.exr test.exr` prints `relmse:`, the mean of
   `(test − ref)² / (ref² + 0.01)`, the literature's standard. Use it, not
   `rmse`, which on `veach_mis` is dominated by the camera-visible light
   spheres.
4. **Equal time, not just equal spp.** A better sampler that costs more per
   sample must win on `relMSE × render seconds`, the inverse efficiency. Time it
   with `scripts/bench_ab.sh`, never sequentially.
5. **Bias check.** Render both binaries at 64 and 256 spp against a 4096 spp
   reference. An unbiased change has relMSE falling as 1/N for both. A bias
   plateaus. That is how §3.3's `1e-4` would show, on a scene with a tiny light.
6. **Scenes.** Each change should cite a scene that exercises it:
   - `veach_mis` (sphere lights, four sizes, glossy plates);
   - `usdlux` (seven heterogeneous lights: selection);
   - `openpbr_showcase` (two sphere lights);
   - `domelight` (sun + dome: compensation);
   - `rectlight` (a single rect: spherical rectangles);
   - `fog` (volumes: equiangular);
   - `cornellbox` (no light at all: §9.1).

   `scripts/gen_stress_scene.py` is the model to follow for a many-lights stress
   scene, which does not exist yet.

---

## 9. A roadmap for crust

In order. Each step is independent of the ones after it, and each keeps the
estimator unbiased.

### 9.1 Defects first

**(a) The sky gradient becomes a light.**
- When no `DomeLight` exists, `escaped_emission` falls back to a gradient that
  NEE cannot sample.
- Promote it to a `DomeLight` built at import from the same gradient: a tiny
  procedural `EnvironmentMap`, or an analytic pdf.
- The image's expectation is unchanged, and every scene without a dome
  (`cornellbox.usda` above all, but also every rect-lit sample, whose sky fill
  is BSDF-sampled today) gains NEE plus MIS for the sky.
- It then competes for picks, which is §9.3's problem.

**(b) The epsilon goes. ✅ Done; measured in §3.10.**
- `pdf_toward` returns `None` wherever the area density is not finite
  (`cos ≤ 0`, or a degenerate shape), and `sample_li` returns `None` for such a
  sample, as pbrt-v4 does (`Shape::Sample` gives no sample).
- On the bounce side `pdf_at_point` reports **0** for the same points, as
  pbrt-v4's `Shape::PDF` does, not ∞. That is the MIS-consistent reading: NEE
  never delivers the point, so `bounce_emission_weight` gives the bounce full
  weight there, exactly as for a light NEE never picks. An infinite pdf would
  weight it to nothing, which is only harmless while the emitter is one-sided.

**(c) Shadow rays last.**
- Evaluate `mat.eval` and `ls.radiance` first, and skip the shadow ray when
  their product is zero.
- The output is bit-identical (the shadow domain is separate), which is what
  makes it a safe first commit. Verify with `scripts/check_images.sh`.

### 9.2 Per-light sampling

**(d) Sphere cone sampling. ✅ Done; measured in §3.7.**
- `LightShape` has a solid-angle hook, `sample_solid_angle(from, u, v)` and
  `solid_angle_pdf(from, p)`, both defaulting to `None`. `AreaLight` prefers it,
  when present, in both `sample_li` and `pdf_at_point`.
  - The contract: whether a shape answers depends on `from` alone, and both
    methods answer for the same `from`s with the same density.
  - Items (e), (g) and (h) plug into the same hook.
- `SphereShape` implements it as pbrt-v4 does:
  - area fallback inside the sphere;
  - a cancellation-free branch below `sin² θ_max < 6.85e-4` (`SMALL_CONE_SIN2`),
    exact where pbrt's Taylor expansion is not (§5.2);
  - no cone at all, on both hooks, when its pdf would overflow f32.
- `AffineShape` spheres (non-uniform scale) sample the unit sphere's cone in local
  space, with the direction map's Jacobian `|Mω|³ / |det M|` on the pdf (§3.7).
  Disks and tubes remain area-sampled. §5.2 lists what would replace that:
  Gamito 2016, Guillén et al. 2017, or Peters' line lights.

**(e) Spherical rectangles. ✅ Done; measured in §3.9. The bilinear cosine
warp is not.**
- `RectShape` implements the solid-angle hook with Ureña et al.'s map, as
  pbrt-v4's `SampleSphericalRectangle`, in f64.
- Area sampling remains, on both hooks alike:
  - below 1e-4 sr and above 6.22 sr (pbrt-v4's `BilinearPatch` bounds);
  - for sheared parallelograms;
  - from behind the one-sided light or on its plane.
- **Textured cards do use it**, against the plan above. pbrt-v4 gives the map up
  for an image quad because it samples the image instead; crust does not (that
  is (h)), so the alternative to solid angle is area, which is no better at
  following the map and worse at everything else.
- **The warp** (Hart et al. 2020, pbrt-v4's `SampleBilinear` over the corners'
  `|n · ω|`) needs the receiver's normal, which `LightShape` and `Light` are
  not given: `sample_li` and `pdf_at_point` take the shading point alone. It is
  the next step here, and an API change on both MIS sides.

**(f) Dome MIS compensation**, with a per-vertex fallback to the full pdf where
BSDF sampling cannot reach the direction.

**(g) Cone-clipped shaped lights** (Cycles' spread clamp).

**(h) Image-sampled textured rect lights**, chosen per light by the map's
max/mean ratio.

**(i) Equiangular distance sampling for volume NEE**, MIS'd against delta
tracking.
- Kulla & Fajardo, *Importance Sampling Techniques for Path Tracing in
  Participating Media*, CGF 31(4) (EGSR 2012); in Cycles.
- It matters in `fog.usda`, where free-flight rarely scatters near the light.
- The Pixar work alongside it, on sampling emissive volumes *as lights*: Villemin
  & Hery, *Practical Illumination from Flames*, JCGT 2(2), 2013.

### 9.3 Selection

**(j) Power pick. ✅ Done, defensively; measured in §3.8.**
- `LightList::select_by` builds a CDF (not an alias table, to keep the pick
  dimension's stratification) over `Light::power`.
- `LightList::density(pdf, pmf)` replaces `pdf / n_lights` at all four sites.
- `find_by_geom` is an O(1) `geom_id → index` map.
- Infinite lights get their uniform share, and half the finite lights' rays are
  spread evenly, because the pure form measured worse (§3.8).

**(k) Camera-vertex splitting.** `crust:lightSamples` (int, default 1) as a render
setting, applied at depth 0 only, MIS'd as in §7.4.

**(l) Per-vertex RIS** over (j)'s source pdf, with the proxy-density MIS of §6.6.
`crust:lightCandidates` (default 1 = off).

**(m) A light BVH** (pbrt-v4's `BVHLightSampler` as the template) once crust
imports scenes with hundreds of lights. That means mesh lights (`MeshLightAPI`),
emissive curves and instances. The Moana island's light rig is small, so this is
not urgent for it.

**(n) Visibility-aware selection** learned during the guiding training passes
(§6.5).

### 9.4 What each step needs to keep

- **Both MIS sides.** Every step that changes `P` or `p_ℓ` changes
  `bounce_emission_weight`, `escaped_emission` and `volume_nee`'s pdf in the same
  commit.
- **A unit test.** A test that integrates the new pdf over its domain to one
  (the `pdf_is_normalized_over_the_sphere` pattern in `environment.rs`).
- **A sample-vs-pdf histogram test** of the kind `medium.rs` uses for
  Henyey-Greenstein.
- **The §8 measurement**, reported in the commit message.

---

## 10. Production survey

What each renderer is documented to do. Anything a vendor does not publish is
marked, and not guessed.

| Renderer | Selection | Per-light | Notes |
|---|---|---|---|
| **pbrt-v4** | light BVH (default), power, uniform | sphere cone; spherical triangle/rectangle + bilinear cos warp; disks/cylinders by area; image-sampled quads | MIS compensation for image infinite lights; equal-area octahedral envmaps |
| **Cycles** | light tree since 3.5 (Conty-Kulla, modified: min/max importance, distant lights) | sphere cone; spherical rectangles; ellipses by area; spread-clipped area lights | equiangular volume sampling; background portals |
| **SPI Arnold** | adaptive tree splitting (2018) → resampled tree (2024) | — | the origin of the light BVH lineage |
| **Arnold** | "Global Light Sampling" since 7.2.1 (structure not documented) | spherical-rectangle lineage (the authors were at Solid Angle; not confirmed in Arnold docs) | glossiness- and spread-aware since 7.4.x |
| **RenderMan RIS** | importance-weighted light pick, per-light `importance` multiplier, optional "light localization" (power, distance, orientation) | `PxrRectLight` / `PxrSphereLight` documented as sampling better than geometry lights | `numLightSamples` / `numBxdfSamples` splitting; "light selection learning". XPU many-light support announced as future work (RenderMan 27). The ToG 2018 architecture paper's light-sampling section was not verified. |
| **Hyperion** | cache points: learned, occlusion-aware, normal-blended (default since 2014) | — | DigiPro 2024 |
| **Karma** | light tree (CPU and XPU); dome and directional lights sampled outside it | — | — |
| **V-Ray** | "Adaptive Lights": learned from the light cache; light-tree fallback | — | — |
| **Iray** | a light hierarchy over per-triangle flux (Keller et al., arXiv 2017) | — | details not verified |
| **Manuka** | not verified | — | — |
| **crust** | power, defensive (infinite lights fixed, ½ even); uniform as the A/B | sphere: visible cone; everything else uniform area | power MIS; piecewise-constant dome |

---

## 11. References

Checked against the publisher, author or project page, or against the source
code in pbrt-v4 (`mmp/pbrt-v4`), Cycles (`blender/cycles`) and Falcor, as of
2026-09. Where only the abstract could be read, the summary above goes no further
than the abstract.

**Per-light sampling**
- J. Arvo. *Stratified Sampling of Spherical Triangles.* SIGGRAPH 1995. doi:10.1145/218380.218500
- P. Shirley, C. Wang, K. Zimmerman. *Monte Carlo Techniques for Direct Lighting Calculations.* ACM ToG 15(1), 1996. doi:10.1145/226150.226151
- C. Ureña, M. Fajardo, A. King. *An Area-Preserving Parametrization for Spherical Rectangles.* CGF 32(4), EGSR 2013. doi:10.1111/cgf.12151
- M. Gamito. *Solid Angle Sampling of Disk and Cylinder Lights.* CGF 35(4), EGSR 2016. doi:10.1111/cgf.12946
- E. Heitz, J. Dupuy, S. Hill, D. Neubelt. *Real-Time Polygonal-Light Shading with Linearly Transformed Cosines.* ACM ToG 35(4), SIGGRAPH 2016.
- I. Guillén, C. Ureña, A. King, M. Fajardo, I. Georgiev, J. López-Moreno, A. Jarabo. *Area-Preserving Parameterizations for Spherical Ellipses.* CGF 36(4), EGSR 2017. arXiv:1805.09048
- C. Ureña, I. Georgiev. *Stratified Sampling of Projected Spherical Caps.* CGF 37(4), EGSR 2018.
- C. Peters, C. Dachsbacher. *Sampling Projected Spherical Caps in Real Time.* PACMCGIT 2(1), I3D 2019.
- D. Hart, M. Pharr, T. Müller, W. Lopes, M. McGuire, P. Shirley. *Practical Product Sampling by Fitting and Composing Warps.* CGF 39(4), EGSR 2020. doi:10.1111/cgf.14060
- C. Peters. *BRDF Importance Sampling for Polygonal Lights.* ACM ToG 40(4), SIGGRAPH 2021.
- C. Peters. *BRDF Importance Sampling for Linear Lights.* CGF 40(8), HPG 2021.
- A. Pekelis, C. Hery. *A Statistical Framework for Comparing Importance Sampling Methods, and an Application to Rectangular Lights.* Pixar Technical Memo 14-01.

**Environment**
- P. Clarberg, W. Jarosz, T. Akenine-Möller, H. W. Jensen. *Wavelet Importance Sampling.* ACM ToG 24(3), SIGGRAPH 2005.
- P. Clarberg, T. Akenine-Möller. *Practical Product Importance Sampling for Direct Illumination.* CGF 27(2), EG 2008.
- B. Bitterli, J. Novák, W. Jarosz. *Portal-Masked Environment Map Sampling.* CGF 34(4), EGSR 2015.
- O. Karlík, M. Šik, P. Vévoda, T. Skřivan, J. Křivánek. *MIS Compensation: Optimizing Sampling Techniques in Multiple Importance Sampling.* ACM ToG 38(6), SIGGRAPH Asia 2019.

**Light selection**
- B. Walter, S. Fernandez, A. Arbree, K. Bala, M. Donikian, D. P. Greenberg. *Lightcuts: A Scalable Approach to Illumination.* ACM ToG 24(3), SIGGRAPH 2005. doi:10.1145/1073204.1073318
- M. Donikian, B. Walter, K. Bala, S. Fernandez, D. P. Greenberg. *Accurate Direct Illumination Using Iterative Adaptive Sampling.* IEEE TVCG 12(3), 2006. doi:10.1109/TVCG.2006.41
- R. Wang, O. Åkerlund. *Bidirectional Importance Sampling for Unstructured Direct Illumination.* CGF 28(2), EG 2009.
- A. Conty Estevez, C. Kulla. *Importance Sampling of Many Lights with Adaptive Tree Splitting.* PACMCGIT 1(2), HPG 2018. doi:10.1145/3233305
- C. Kulla, A. Conty, C. Stein, L. Gritz. *Sony Pictures Imageworks Arnold.* ACM ToG 37(3), 2018. doi:10.1145/3180495
- P. Vévoda, I. Kondapaneni, J. Křivánek. *Bayesian Online Regression for Adaptive Direct Illumination Sampling.* ACM ToG 37(4), SIGGRAPH 2018. doi:10.1145/3197517.3201340
- P. Moreau, P. Clarberg. *Importance Sampling of Many Lights on the GPU.* Ray Tracing Gems, ch. 18, 2019. doi:10.1007/978-1-4842-4427-2_18
- P. Moreau, M. Pharr, P. Clarberg. *Dynamic Many-Light Sampling for Real-Time Ray Tracing.* HPG 2019. doi:10.2312/hpg.20191191
- C. Yuksel. *Stochastic Lightcuts.* HPG 2019. doi:10.2312/hpg.20191192. Extended as *Stochastic Lightcuts for Sampling Many Lights*, IEEE TVCG 27(10), 2021. doi:10.1109/TVCG.2020.3001271
- Y. Liu, K. Xu, L.-Q. Yan. *Adaptive BRDF-Oriented Multiple Importance Sampling of Many Lights.* CGF 38(4), EGSR 2019. doi:10.1111/cgf.13776
- D. Lin, C. Yuksel. *Real-Time Stochastic Lightcuts.* PACMCGIT 3(1), I3D 2020. doi:10.1145/3384543
- Y.-C. Wang, Y.-T. Wu, T.-M. Li, Y.-Y. Chuang. *Learning to Cluster for Rendering with Many Lights.* ACM ToG 40(6), SIGGRAPH Asia 2021. doi:10.1145/3478513.3480561
- M. Pharr, W. Jakob, G. Humphreys. *Physically Based Rendering*, 4th ed., §12.6 "Light Sampling". MIT Press, 2023.
- Y. Tokuyoshi, S. Ikeda, P. Kulkarni, T. Harada. *Hierarchical Light Sampling with Accurate Spherical Gaussian Lighting.* SIGGRAPH Asia 2024. doi:10.1145/3680528.3687647
- Y. K. Li, C. Zhu, G. Nichols, P. Kutz, W.-F. W. Huang, D. Adler, B. Burley, D. Teece. *Cache Points for Production-Scale Occlusion-Aware Many-Lights Sampling and Volumetric Scattering.* DigiPro 2024. doi:10.1145/3665320.3670993
- A. Conty Estevez, C. Hellmuth, P. Lecocq. *A Resampled Tree for Many Lights Rendering.* SIGGRAPH 2024 Talks. doi:10.1145/3641233.3664352
- P. Figueiredo, Q. He, S. Bako, N. K. Kalantari. *Neural Importance Sampling of Many Lights.* SIGGRAPH 2025. doi:10.1145/3721238.3730754
- J. Bokšanský, D. Meister. *Neural Visibility Cache for Real-Time Light Sampling.* JCGT 14(2), 2025.

**Resampling**
- J. Talbot, D. Cline, P. Egbert. *Importance Resampling for Global Illumination.* EGSR 2005.
- B. Bitterli, C. Wyman, M. Pharr, P. Shirley, A. Lefohn, W. Jarosz. *Spatiotemporal Reservoir Resampling for Real-Time Ray Tracing with Dynamic Direct Lighting.* ACM ToG 39(4), SIGGRAPH 2020. doi:10.1145/3386569.3392481
- Y. Ouyang, S. Liu, M. Kettunen, M. Pharr, J. Pantaleoni. *ReSTIR GI: Path Resampling for Real-Time Path Tracing.* CGF 40(8), HPG 2021. doi:10.1111/cgf.14378
- J. Boksansky, P. Jukarainen, C. Wyman. *Rendering Many Lights with Grid-Based Reservoirs.* Ray Tracing Gems II, ch. 23, 2021.
- D. Lin, M. Kettunen, B. Bitterli, J. Pantaleoni, C. Yuksel, C. Wyman. *Generalized Resampled Importance Sampling: Foundations of ReSTIR.* ACM ToG 41(4), SIGGRAPH 2022. doi:10.1145/3528223.3530158
- C. Wyman et al. *A Gentle Introduction to ReSTIR: Path Reuse in Real-Time.* SIGGRAPH 2023 Courses. doi:10.1145/3587423.3595511
- S. Zhang, D. Lin, M. Kettunen, C. Yuksel, C. Wyman. *Area ReSTIR.* ACM ToG 43(4), SIGGRAPH 2024. doi:10.1145/3658210

**MIS, splitting, sampling**
- E. Veach, L. J. Guibas. *Optimally Combining Sampling Techniques for Monte Carlo Rendering.* SIGGRAPH 1995. doi:10.1145/218380.218498
- T. Kollig, A. Keller. *Efficient Multidimensional Sampling.* CGF 21(3), EG 2002.
- P. Christensen, A. Kensler, C. Kilpatrick. *Progressive Multi-Jittered Sample Sequences.* CGF 37(4), EGSR 2018.
- I. Kondapaneni, P. Vévoda, P. Grittmann, T. Skřivan, P. Slusallek, J. Křivánek. *Optimal Multiple Importance Sampling.* ACM ToG 38(4), SIGGRAPH 2019.
- P. Grittmann, I. Georgiev, P. Slusallek, J. Křivánek. *Variance-Aware Multiple Importance Sampling.* ACM ToG 38(6), SIGGRAPH Asia 2019. doi:10.1145/3355089.3356515
- R. West, I. Georgiev, A. Gruson, T. Hachisuka. *Continuous Multiple Importance Sampling.* ACM ToG 39(4), SIGGRAPH 2020.
- B. Burley. *Practical Hash-based Owen Scrambling.* JCGT 9(4), 2020.
- A. Rath, P. Grittmann, S. Herholz, P. Weier, P. Slusallek. *EARS: Efficiency-Aware Russian Roulette and Splitting.* ACM ToG 41(4), SIGGRAPH 2022.
- P. Grittmann, Ö. Yazici, I. Georgiev, P. Slusallek. *Efficiency-Aware Multiple Importance Sampling for Bidirectional Rendering Algorithms.* ACM ToG 41(4), SIGGRAPH 2022.

**Volumes**
- C. Kulla, M. Fajardo. *Importance Sampling Techniques for Path Tracing in Participating Media.* CGF 31(4), EGSR 2012.
- R. Villemin, C. Hery. *Practical Illumination from Flames.* JCGT 2(2), 2013.

**Production architecture** (ACM ToG 37(3), 2018 special issue)
- P. Christensen et al. *RenderMan: An Advanced Path-Tracing Architecture for Movie Rendering.* doi:10.1145/3182162
- I. Georgiev et al. *Arnold: A Brute-Force Production Path Tracer.* doi:10.1145/3182160
- B. Burley et al. *The Design and Evolution of Disney's Hyperion Renderer.* doi:10.1145/3182159
- L. Fascione et al. *Manuka: A Batch-Shading Architecture for Spectral Path Tracing in Movie Production.* doi:10.1145/3182161
- A. Keller et al. *The Iray Light Transport Simulation and Rendering System.* arXiv:1705.01263, 2017.

**Not verified.** These were searched for and not confirmed, so nothing above
relies on them:
- the light-selection sections of the RenderMan, Arnold and Manuka ToG papers
  (paywalled);
- Arnold's Global Light Sampling data structure;
- any Pixar tech memo on learned light selection;
- the internal details of Hyperion's cache points beyond the abstract.
