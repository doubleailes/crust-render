## Context

`AffineShape` (`crust-core/src/light/shape.rs`) carries all three round UsdLux
shapes: a unit sphere, a unit disk in local XY emitting along −Z, and a
unit-radius, unit-length open tube along local X. Each is placed by an arbitrary
invertible affine map. The importer composes the light's radius and length into
that map (`usd_import/lights.rs`):

- a disk is scaled by `(r, r, 1)`;
- a tube is scaled by `(length, r, r)`.

So a real tube's local space is almost never metric. A 1 m tube of 1 cm radius is
scaled 100× more along its axis than across it.

Only the sphere has a solid-angle strategy today: `Strategy::AffineCone`, which
samples the unit sphere's cone in local space and maps the density out with
`AffineShape::world_solid_angle_pdf`. The disk and the tube fall back to
`UnitShape::sample`, uniform in local area, through `AreaLight::sample_li`'s area
path.

Every imported light's emitter is `Emissive::light`, which is one-sided. The
importer never makes a two-sided disk or tube, but `Emissive::new` is two-sided and
the types allow pairing it with an `AffineShape`.

The rules this change has to keep (`CLAUDE.md`, "Pairs that must change
together"):

- The solid-angle sample and its pdf come from the one
  `LightShape::solid_angle_sampler(from)`, decided on `from` alone.
- A non-finite density is refused on both MIS sides.
- An environment switch's "off" side is the old behaviour.

## Goals / Non-Goals

**Goals:**

- Remove the tube's back-facing waste exactly, and follow its `1/r²` along the axis.
- Sample a disk uniformly in the solid angle it subtends, where that pays.
- Stay unbiased against the area-sampled image, under every placement the importer
  produces and under any invertible affine map.
- Leave renders without a disk or cylinder light bit-identical, and keep the sphere
  and rect strategies instruction-neutral.
- Decide the defaults by measurement, at equal time, not by expectation.

**Non-Goals:**

- Projected-solid-angle or BSDF-aware sampling, such as Peters 2019 / 2021 or LTCs.
  Uniform solid angle is the step the sphere and rect already took.
- Cone-aware sampling of shaped (`ShapingAPI`, IES) disks. That is its own gap
  (`docs/light_sampling.md` §5.2, "Shaped and IES lights").
- Gamito 2016's exact solid-angle sampler for thick cylinders. See D3.
- Light selection (power / uniform / learned) is untouched, and so is
  `AreaLight::power`. Power still integrates the area sampler, which stays as it is.

## Decisions

### D1. The tube's azimuth: the visible arc, in local space

Let `f = world_to_light(from)`, `ρ = |(f.y, f.z)|` and `φ₀ = atan2(f.z, f.y)`.

A wall point `p = (x, cos φ, sin φ)` with outward normal `n = (0, cos φ, sin φ)`
faces `f` when `n · (f − p) > 0`. That reduces to `ρ cos(φ − φ₀) > 1`, so the
visible arc is `|φ − φ₀| < w` with `w = acos(1/ρ)`. It does not depend on `x`.

The squashed-sphere argument carries over: an affine map preserves which points of
a convex surface face a point. With `n_w ∝ M⁻ᵀ n_l`,
`n_w · (x − p) = n_l · (x_l − p_l) / |M⁻ᵀ n_l|`. So the local arc is the world
visible arc under any placement, sheared and elliptical tubes included.

Draw `φ = φ₀ + w (2v − 1)`, with local density `1 / (2w)` per radian.

- `ρ² ≤ 1` (on the wall or inside the tube): no strategy (`None`). From inside, a
  one-sided tube shows only its back, and area sampling already gives zero
  radiance there at no shadow-ray cost.
- Through an open end, from outside, the inner wall that shows is the back side,
  which a one-sided emitter does not emit from. So restricting to the outer arc
  loses nothing, but only for one-sided emitters (D4).

*Alternative considered:* the doc's interim "sample the half facing the shading
point", `w = π/2`. It is simpler but not exact: near the tube the visible arc is
narrower than a half, and the rest still lands on back-facing points.

### D2. The tube's axial position: equiangular along the sampled wall line

Given `φ`, the wall points at that azimuth form a straight segment in world space:

- it starts at `W = light_to_world((−½, cos φ, sin φ))`;
- its direction is `A = M · X`, of world length `L = |A|`.

The shading point's foot on that line, measured from `W` in world units, is
`s₀ = (from − W) · Â`. Its distance from the line is `H = |(from − W) × Â|`.

Draw the world arc length `s ∈ [0, L]` with density proportional to `1 / (H² + (s − s₀)²)`,
the equiangular density used for point lights in media:

```
θa = atan((0 − s₀) / H),  θb = atan((L − s₀) / H)
θ  = θa + u (θb − θa),    s = s₀ + H tan θ
p(s) = H / ((θb − θa) (H² + (s − s₀)²))
```

Then `x = s / L − ½`, with `p(x | φ) = L · p(s)`.

Working in world units along the line, rather than in local `x`, is what makes the
density follow the real `1/r²` on a tube scaled `(length, r, r)`. It is exact in
shape for any affine map, because the line at fixed `φ` is straight in world space.
Taking `H` per sampled azimuth, rather than the distance to the axis, carries
`r²`'s dependence on `φ`. What the density does not follow is the two cosines. That
is the uniform-solid-angle gap the sphere and rect also accept.

`H → 0` means the shading point lies on the wall line's extension, which needs
`ρ = 1` and so is excluded by D1. Near it the `atan` stays well conditioned.

The tube's density, in solid angle, at a wall point `p` seen from `from`:

```
p_local(φ, x) = (1 / 2w) · p(x | φ)                 per unit local area (dφ dx)
p_area(p)     = p_local / area_scale(n_local)        AffineShape's existing Jacobian
p_Ω(p)        = p_area · |p − from|² / |cos θ_l|
```

`pdf(p)` recovers `(x, φ)` from `world_to_light(p)` and recomputes `w`, `H` and
`s₀` from the sampler's `from`. That gives the bounce side the same number NEE
used. A point outside the arc has density 0, and the strategy answers `None` for it.
It is back-facing, so a one-sided emitter's radiance there is zero anyway, and the
bounce side never asks.

**`CRUST_TUBE_SAMPLING`:**

- `area`: today's sampler.
- `arc`: D1 with uniform `x` (`p(x | φ) = 1`).
- `equiangular`: D1 and D2.

The middle value exists so each step's share of the gain is measured on its own.

### D3. Not Gamito 2016 for the tube

Gamito samples a finite-radius cylinder uniformly in solid angle, by rejection.
Rejection breaks the QMC stratification that every other crust strategy keeps
(`openqmc` draws one `(u, v)` per sample).

Its gain over D1 + D2 is the cosine profile across a thick tube's visible arc. That
matters only when the receiver is within a few radii of the wall, which is rare for
production tubes (ALab's 13 `CylinderLight`s are button lights). If the measurement
shows thick tubes up close still noisy, that becomes a known gap with a pointer, not
part of this change.

### D4. The tube's strategy needs a one-sided emitter

A two-sided tube seen through an open end shows its inner wall, which emits toward
the viewer and lies outside the outer arc. D1 would never sample it: that is bias,
not noise.

`AffineShape` is pure geometry and does not know its emitter. So `AreaLight::new`
sets a `front_only` flag on the shape from `Emissive::is_one_sided()`, and the
cylinder's `solid_angle_sampler` answers `None` without it. The decision is still
on `from` alone for a given light, so both MIS sides see the same strategy. The
importer always passes a one-sided emitter, so every imported tube takes the new
strategy.

The disk needs no such flag. Its strategy is only built strictly in front of the
emitting side (D5), where both kinds of emitter show the same face. From behind it
falls back to area sampling, exactly as the rect does.

### D5. The disk: Guillén et al. 2017's spherical ellipse, in local space

A disk seen from a point is bounded by an elliptical cone, so it subtends a
spherical ellipse. Guillén, Ureña, King, Fajardo, Georgiev, López-Moreno and Jarabo
(*Area-Preserving Parameterizations for Spherical Ellipses*, CGF 36(4), EGSR 2017)
give area-preserving maps from the unit square onto it. Their solid angle and CDFs
are elliptic integrals.

This is written from the paper's abstract and from memory. The network was blocked
when this was drafted, so task 2.1 transcribes the formulas from the paper before
any code. In outline:

- **Frame.** From the shading point, build the cone's frame: its axis, and the
  semi-axis angles `α ≥ β` of the spherical ellipse. These follow from the disk's
  centre, normal and radius in closed form.
- **Solid angle.** A complete elliptic integral of the third kind in `α` and `β`.
- **Sampling, polar map.** The azimuth comes from inverting the CDF of an incomplete
  elliptic integral of the third kind (Newton, safeguarded by bisection). The
  radial coordinate follows in closed form. The paper's other map trades this for a
  different inversion. Pick by the paper's own cost and stratification figures, and
  record why.
- **Elliptic integrals.** Carlson's symmetric forms `R_F` and `R_J` (duplication
  algorithm, ~60 lines, f64), in `light/ellipse.rs`. They are tested against
  reference values from an independent source (mpmath or Boost, generated
  offline like the OSL and Adobe oracles).

**Local, then mapped.** The map runs on the unit disk seen from `f`, in local space.
The world density is the local one times the solid-angle Jacobian of the direction
map, which is `AffineShape::world_solid_angle_pdf`, the same as `AffineCone`. That
makes an elliptical disk (non-uniform placement) correct for free: a direction
bijection carries a density exactly. The sampled point is returned through the
disk's own local `(r, φ)` mapped by `light_to_world`, so it lies on the surface a
bounce ray hits, as the rect's does.

**When the strategy applies**, decided from `f` alone:

- strictly in front of the emitting side (`f.z < 0` in local space);
- the subtended solid angle within `[1e-4, 6.22]` sr, the rect's band
  (`MIN_SPHERICAL_RECT_SR` / `MAX_SPHERICAL_RECT_SR`). Below it, area sampling is
  already near-optimal (the CV above) and the elliptic integrals lose precision.
  Above it, the shading point is almost on the disk. The measurement may move the
  lower bound up, if the strategy's cost buys nothing for small disks.

**`CRUST_DISK_SAMPLING`:**

- `area`: today's sampler.
- `ellipse`: D5.

*Alternative considered: circumscribe the disk in a square and use the existing
`SphericalRect`.* Points outside the disk become zero-contribution samples, about
`1 − π/4 ≈ 21 %` of them. It is unbiased, cheap, and reuses tested code. It breaks
"every returned point lies on the shape", it wastes a fifth of the samples where
the exact map wastes none, and it only works where the square stays a rectangle
(no shear). It is kept as the fallback if task 5 shows Guillén's per-sample cost
eating its gain.

*Alternative considered: Gamito 2016's disk sampler.* It is rejection-based, for
the same reason as D3.

### D6. `SolidAngleSampler` may refuse a point

The tube's solid-angle density is infinite at its silhouette (`cos θ_l = 0`), and
zero outside the arc. The area path already refuses an infinite density pointwise
(`AreaLight::pdf_toward` → `None`, and `pdf_at_point` reads it as "NEE never
delivers this point"). So the sampler adopts the same convention:

- `SolidAngleSampler::sample` returns `Option<(Vec3A, PdfSolidAngle)>`, and `pdf`
  returns `Option<PdfSolidAngle>`. Both go through `PdfSolidAngle::new` for the
  strategies that can refuse.
- `AreaLight::sample_li` returns `None` on a refused sample instead of falling back
  to area sampling. The fallback must stay decided by `from` alone, which
  `solid_angle_sampler` already does.
- `AreaLight::solid_angle_pdf` keeps one rule: a sampler exists from `from`, so its
  answer, `None` included, is final. Today it falls through to area sampling when
  the sampler's pdf is `None`. That has to change, or a refused tube point would be
  given the area density on the bounce side while NEE never delivers it.

Cone, AffineCone and Rect wrap their current values in `Some` and never refuse.
Task 1.3 checks with callgrind that `Bvh::hit`, `render_pixel` and `sample_li` keep
their instruction counts on cornellbox and `veach_mis`: the hot path matches on the
strategy, and an `Option` that is always `Some` should fold away.

### D7. Defaults by measurement

`docs/light_sampling.md`'s rect precedent is the warning:

- the spherical rectangle won 1.4–1.5× on near panels;
- it lost 4–9 % on the glossy `materialx_basic` / `usdpreview_textured` tiles;
- it costs about 110 ns per NEE sample.

The tasks measure each switch value at equal time against a 1024 spp reference
(`crust diff` relMSE, `--indirect-clamp 0`, `-s 16`) on:

- `samples/usdlux.usda`, which holds a disk and a tube;
- a scratch sweep:
  - disks at radius/height 0.1, 0.3, 1 and 3;
  - tubes at length/distance 0.3, 1 and 3, and radius/distance 0.01 and 0.3;
  - a diffuse and a glossy floor;
- ALab frame 1004, for its 13 `CylinderLight`s, where the dataset is present.

A shape's new strategy becomes its default only when no case loses beyond noise at
equal time. Where one does (a glossy receiver, a tiny disk), the default stays, or
the band moves, and the result goes in the design record either way.

## Risks / Trade-offs

- **Guillén's inversion is the expensive, fragile part.**
  - *Cost:* Newton on an incomplete elliptic integral per sample. The band (D5)
    keeps it away from tiny solid angles, and D5's square alternative is the
    fallback.
  - *Fragility:* near-degenerate ellipses (edge-on disks, `β → 0`) and near-circular
    ones (`α ≈ β`). Pin both with a histogram test against brute-force solid-angle
    integration, like the sphere's and rect's.
- **D6 touches every area light's hot path.** The callgrind check in task 1.3
  guards it. If the `Option` does not fold, keep a separate refusing entry point
  used by the tube variant only.
- **The equiangular density ignores the cosines.** For a thick tube up close it can
  still be noisy. That is accepted and documented (D3), not hidden.
- **`front_only` couples the shape to its emitter.** It is one bool set at
  construction, and the alternative (tube sampling that is biased for two-sided
  emitters) is worse. A test pins a two-sided tube through its open end against the
  area-sampled render.
- **Bit-identity is lost for scenes with disks or tubes**, by design. `usdlux` and
  any DPEL sample with round lights change. The proof that it is noise, not bias,
  is light-only, BSDF-only and MIS agreeing, plus the difference falling as 1/√N.

## Open Questions

1. Which of Guillén's two maps: settle in task 2.1 from the paper's measurements.
2. Should the tube also get a solid-angle band (area sampling for a tube that is
   tiny on screen)? D1 is cheap and its back-facing win does not shrink with
   distance, so the answer is probably no. The sweep in task 5 answers it.
3. Does `--light-samples N` (several samples per vertex) want the tube's arc
   stratified across the N draws? It gets that for free from `openqmc`'s
   stratification over `(u, v)`. Confirm it in the sweep rather than design for it.
