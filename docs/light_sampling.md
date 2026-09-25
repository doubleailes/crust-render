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

1. pick **one** light, **uniformly** (`LightList::pick`, `light.rs`);
2. sample a point on it **uniformly by area**, including, for a sphere, the half
   that faces away (`SphereShape::sample_point`);
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
| 1 | Sample sphere lights by the **visible cone** (Shirley et al. 1996) | per-light pdf | ~60 lines | **measured 1.3–1.7× lower relMSE** on the three sphere-lit samples (prototype, §3.6) | noise only |
| 2 | **Spherical-rectangle** sampling for rect lights (Ureña et al. 2013) + a bilinear cosine warp (Hart et al. 2020) | per-light pdf | ~200 lines | large on near/large panels | noise only |
| 3 | **Power-proportional light pick** (alias table), with the dome/sun given a deliberate share | selection | ~80 lines | large as soon as lights differ in power | noise only |
| 4 | **More than one light sample at the camera vertex** (RenderMan `numLightSamples`, Arnold per-light `samples`) | sample count | ~50 lines | ≈ k× less first-bounce direct variance | noise only |
| 5 | Make the **built-in sky** an importance-sampled light rather than an escape-only background | coverage | ~40 lines | small on the open `cornellbox` (measured 0.0125 relMSE today), large in enclosed dome-less scenes | noise only |
| 6 | Remove the `+1e-4` in `AreaLight::pdf_toward` | correctness | ~10 lines | none: it is a **bias** fix | yes, slightly |
| 7 | Evaluate BSDF and emission **before** the shadow ray | cost | ~10 lines | none (bit-identical), fewer rays | no |
| 8 | **MIS compensation** for the dome map (Karlík et al. 2019, as in pbrt-v4) | per-light pdf | ~20 lines | large on sun + sky HDRIs | noise only |
| 9 | **Per-vertex RIS**: M candidates, one shadow ray (Talbot et al. 2005) | selection | ~150 lines | large on glossy surfaces under several lights | noise only |
| 10 | A **light BVH** with orientation cones (Conty Estevez & Kulla 2018; pbrt-v4, Cycles) | selection | ~600 lines | decisive for 10²+ lights, marginal below | noise only |
| 11 | Cone-clipped sampling for shaped lights; image-importance sampling for textured rect lights; equiangular sampling in volumes | per-light pdf | moderate each | large in their niche | noise only |

"Noise only" means the change alters the estimator but not its expectation. The
image moves by noise and nothing else, which §8 shows how to prove.

**The one-line version.** Items 1, 2, 5 and 7 fix defects rather than add
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
| `SphereLight` (similarity transform) | `SphereShape`: uniform over the **whole** sphere | Everything outside the visible cap: at least half of all samples. More when close, because the visible cap is `(1 − r/d)/2` of the area. Back-facing samples are occluded by the sphere itself, so each one is a traced shadow ray that returns zero. |
| `RectLight` | `RectShape`: uniform in `(u, v)` | For a panel large or near relative to its distance, `cos θ_l / r²` varies by orders of magnitude across it. A few near samples dominate, and the QMC stratification is spent on the wrong measure. |
| `DiskLight`, `CylinderLight`, squashed sphere | `AffineShape`: uniform in local area | Same as the rect. The tube also samples its far side. |
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
binary.

relMSE, lower is better:

| Scene | Lights | power (default) | balance | light only | bsdf only | **+ sphere cone sampling** (prototype) |
|---|---|---|---|---|---|---|
| `veach_mis` | 4 spheres, glossy plates | 0.0195 | 0.0190 | 46.2 | 9.86 | **0.0113** (1.73×) |
| `cornellbox_guided` | 1 shrouded sphere, guided | 0.0761 | 0.0741 | 0.225 | 0.315 | **0.0468** (1.62×) |
| `openpbr_showcase` | 2 spheres, distant | 0.00655 | 0.00636 | 0.00579 | 0.0896 | **0.00517** (1.27×) |
| `usdlux` | 7 mixed, incl. dome | 0.137 | 0.135 | 1.14 | 16.5 | — (no uniformly scaled sphere) |
| `domelight` | dome + sun | 0.113 | 0.113 | 243 | 16.3 | — |
| `cornellbox` | **none** (sky gradient) | 0.0125 | 0.0125 | 0.0125 | 0.0125 | — |

What the table says:

- **MIS is not the problem.** Power and balance agree to within noise. On all but
  one scene both beat either single strategy by one to three orders of
  magnitude.
  - The exception is `openpbr_showcase`, where light-only *beats* both
    heuristics. That is the situation §7.2's MIS compensation and variance-aware
    MIS address: the heuristic gives BSDF samples weight in a region where they
    only add noise.
- **The per-light pdf is the cheapest noise left.** The cone-sampling prototype
  (item 1, about 60 lines, not committed) cuts relMSE by 1.3–1.7× at equal spp
  on every sphere-lit scene.
  - The gain grows as the lights get nearer and larger, as §5.2 predicts:
    `openpbr_showcase`'s lights are far away, `veach_mis`'s and the shrouded
    ceiling light are near.
  - Its cost per sample was within the noise of sequential timing: 106 s against
    100 s on `cornellbox_guided`, on a loaded machine and not `bench_ab`'d.
- **`cornellbox` confirms no NEE runs there.** All four strategies produce the
  *identical* image. Its relMSE is nonetheless low: the box is open, so bounce
  rays find the sky easily.
  - An enclosed room lit through an opening, or any interior with a dome-less
    sky, is where that absence costs. It is also a trap for anyone measuring
    strategies on the README's first scene: nothing they change in light
    sampling can show up there.

The 16 spp images' noise elsewhere is therefore mostly per-light (§5) and
selection (§6) noise, and the number of shadow rays per camera sample (§2).

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
  where `1 − cos θ_max` cancels catastrophically in f32.
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

For crust, a power table over the finite lights plus a **configurable fixed
fraction** for the infinite ones (defaulting to their share of an estimated
irradiance at the scene centre) is the pragmatic middle.

Two things are required:
- `LightList` stores the pmf, and exposes `pmf(index)`;
- `find_by_geom` returns the index. It is also a linear scan today, called on
  every bounce that hits an emitter, and should become a `geom_id`-indexed
  table.

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

**(b) The epsilon goes.**
- `pdf_toward` returns an infinite pdf at `cos ≤ 0`, and `sample_li` returns
  `None` for a back-facing sample, as pbrt-v4 does.
- On the bounce side, the one-sided `Emissive` already gives zero radiance from
  the back.

**(c) Shadow rays last.**
- Evaluate `mat.eval` and `ls.radiance` first, and skip the shadow ray when
  their product is zero.
- The output is bit-identical (the shadow domain is separate), which is what
  makes it a safe first commit. Verify with `scripts/check_images.sh`.

### 9.2 Per-light sampling

**(d) Sphere cone sampling.**
- Add a solid-angle hook to `LightShape`: `sample_solid_angle(from, u, v)` and
  `pdf_solid_angle(from, p)`, both defaulting to `None`. `AreaLight` uses it when
  present, in both `sample_li` and `pdf_at_point`.
- `SphereShape` implements it as pbrt-v4 does:
  - area fallback inside the sphere;
  - the Taylor branch below `sin² θ_max < 6.85e-4`.
- The prototype behind the §3.6 figure is exactly this, about 60 lines.
- `AffineShape` spheres (non-uniform scale) keep area sampling. Sampling the
  visible half is the cheap intermediate there.

**(e) Spherical rectangles + bilinear cosine warp.**
- Port Cycles' `area.h` (Apache-2.0) or pbrt-v4's `SampleSphericalRectangle` and
  `SampleBilinear`.
- Fall back to area sampling:
  - below about 1e-4 sr;
  - for sheared parallelograms;
  - for textured cards (until (h)).

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

**(j) Power pick.**
- An alias table in `LightList`.
- `pmf(i)` replaces `1 / n_lights` at all four sites.
- `find_by_geom` becomes an O(1) `geom_id → index` table.
- Infinite lights get a separate, documented share.

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
| **crust** | uniform | uniform area (sphere: whole sphere) | power MIS; piecewise-constant dome |

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
