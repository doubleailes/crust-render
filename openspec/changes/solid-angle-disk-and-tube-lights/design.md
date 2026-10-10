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

Transcribed from the arXiv preprint (1805.09048), which is the same text as CGF
36(4) pp. 179–187. Equation numbers are the paper's.

**The spherical ellipse (§3, eqs. 3–5).** The disk has centre `c`, normal `n̂` and
radius `r`, and the shading point is `o`. The paper builds a frame
`ℛ_d = (x̂_d, ŷ_d, ẑ_d)` with `ẑ_d = −n̂`. It takes the disk's boundary coordinates
`y₀` and `y₁` along `ŷ_d`, and projects them onto the unit sphere to get
`(y′₀, z′₀)` and `(y′₁, z′₁)`.

- The ellipse centre is `ẑ_e = normalize(0, y′_h, z′_h)`, with `y′_h` and `z′_h`
  the averages of those coordinates. It is not the projection of `c`.
- Reprojecting `ẑ_e` onto the disk gives a chord `x₀x₁` parallel to `x̂_d`. Its
  endpoints project to `x′₀` and `x′₁`.
- The semi-axes, semi-arcs and tangent-ellipse semi-axes are:
  ```
  a  = x′₁,          b  = ½ √((y′₁ − y′₀)² + (z′₁ − z′₀)²)
  α  = asin a,       β  = asin b
  a_t = tan α,       b_t = tan β
  ```
- The ellipse frame is `ℛ_e`, with `x̂_e ≡ x̂_d` (the `a` axis) and
  `ŷ_e = ẑ_e × x̂_e`.

**Trap, eq. 3.** As printed, `ŷ_d = ẑ_d × (c − o)/‖c − o‖` and
`x̂_d = ŷ_d × ẑ_d`. That makes `c − o ⊥ ŷ_d`, so `c` lies in the `x̂_d ẑ_d` plane.
But the text then puts the ellipse centre at `x = 0` and measures the
semi-major axis `a` along `x̂_d`.

Both cannot hold. The ellipse is symmetric about the plane through `o`, `c` and
`n̂`. Its foreshortened axis lies in that plane, so the `a` axis is the one
perpendicular to it, `∝ n̂ × (c − o)`. That is what eq. 3 calls `ŷ_d`.

So either eq. 3's `x̂` and `ŷ` are swapped, or the figure's labels are. Build the
frame from the symmetry argument, not from eq. 3 as printed:

- `x̂_d ∝ n̂ × (c − o)`;
- `ŷ_d` is the in-plane direction from `o`'s foot toward `c`;
- `ẑ_d = −n̂`.

Pin it with a test: brute-force solid angle against eq. 19's, off axis.

**Other degenerate and boundary cases:**

- **On the axis** (`c − o ∥ n̂`), `n̂ × (c − o)` vanishes. The ellipse is a circle,
  `a = b`, so any perpendicular frame works.
- **`a ≥ b`** is assumed by `m ∈ [0, 1)` in eq. 20. It holds for a disk seen from
  a moderate distance, but has not been shown near the disk plane, where both arcs
  approach `π/2`. A property test over a dense `(height, offset)` grid checks it,
  and the code swaps the axes if it ever fails.

**The radial map (§3.3, eqs. 16–22), chosen.** Per quadrant, `φ_r ∈ [0, π/2]`.
The planar ellipse radius and the altitude are:

```
r(φ)  = a b / √(a² sin² φ + b² cos² φ)         (18)
h_r(φ) = √(1 − r(φ)²)                          (17)
```

The fractional quadrant area, and its parameters, are:

```
Ω_r(φ) = φ − [b (1 − a²) / (a √(1 − b²))] · Π(n; φ_t | m)        (19)
φ_t = atan((a_t / b_t) tan φ)                                    (20)
n   = (a² − b²) / (a² (1 − b²))
m   = (a² − b²) / (1 − b²)
```

The total solid angle is `Ω_D = 4 Ω_r(π/2)` (`φ_t = π/2`, a complete `Π`).

Sampling:

1. Solve `Ω_r(φ) = ε₁ Ω_r(π/2)` for `φ` (eq. 21).
2. Set `h = (1 − ε₂) h_r(φ) + ε₂` (eq. 22).
3. The direction in `ℛ_e` is `q = (√(1 − h²) cos φ, √(1 − h²) sin φ, h)`. That is
   eq. 15 with `x̂` and `ẑ` swapped. `φ` is measured from the `a` axis, as in
   eq. 18.
4. Mirror `q`'s `x` and `y` to reach the other three quadrants. Spend part of
   `ε₁` choosing the quadrant, alternating the direction so the map stays
   continuous across quadrant edges (as eq. 23 does).
5. Intersect the ray `o + t q` with the disk's plane to get the point.

The density is `1 / Ω_D` everywhere on the ellipse.

**Newton's derivative is free.** `dΩ_r/dφ = 1 − h_r(φ)` (eq. 16), in closed form.
So each Newton step costs one `Π`. The paper reports 1–4 iterations. Use a
bisection safeguard (`Ω_r` is monotone on `[0, π/2]`) and start from the circular
case's answer, `φ = ε₁ π/2`.

**A free correctness check: the circle.** At `a = b`, `n = m = 0` and
`Π(0; φ | 0) = φ`, so eq. 19 reduces to `Ω_r = φ (1 − cos α)`. That is the cone's
`2π (1 − cos α)` over four quadrants, so the on-axis disk must agree with
`SubtendedCone` to rounding.

**Why the radial map, not the parallel one (§3.2, eqs. 6–15).** The parallel map
needs `F` and `Π` per Newton step (eq. 11). It also visibly distorts stratification
(converging lines, Figure 1c). The paper prefers radial for both reasons, and so do
we.

**The low-distortion radial variant (eq. 23)** pre-warps `(ε₁, ε₂)` through
Shirley–Chiu's concentric map, then an inverse polar map, before the radial map.
That removes the polar map's convergence at the ellipse centre. It costs a few
flops and no elliptic integral. Task 2.4 measures it against the plain radial map
under `openqmc` points, and keeps whichever has the lower 16 spp error on the sweep.
There is no switch value for it: it is a quality choice inside `ellipse`, settled
by the test.

**Not the tabulation (§4.1).** For speed, the paper replaces the Newton inversion
with a 1024² table over `(β/α, φ_r)` of spherical triangles. Samples that land
outside the ellipse are rejected for unbiasedness, but Arnold gives them zero
weight, which the paper notes underestimates the light. Both options break a crust
rule:

- rejection breaks `openqmc` stratification;
- zero weight is a bias.

The table would also add 4 MiB. Crust takes the exact Newton inversion and pays
for it.

**Cost.** The paper measures the Newton version at up to 10× area sampling's cost
in simple scenes, amortised away in production scenes (its Figure 9, Arnold).
Arnold also switches to area sampling for far lights, which is what the
solid-angle band below does. D7's equal-time measurement decides whether that
cost pays in crust.

**Elliptic integrals.** These are Carlson's symmetric forms, as the paper itself
recommends (§4.2, [Car95]):

```
Π(n; φ | m) = s R_F(c², 1 − m s², 1) + (n/3) s³ R_J(c², 1 − m s², 1, 1 − n s²)
s = sin φ_t,  c = cos φ_t
```

This is the convention `Π = ∫₀^φ dθ / ((1 − n sin² θ) √(1 − m sin² θ))`. The circle
check above and the brute-force solid angle pin the sign of `n` against the
paper's.

`R_F` and `R_J` use the duplication algorithm, in f64, in `light/ellipse.rs`. They
are tested against reference values from an independent source (mpmath),
generated offline like the OSL and Adobe oracles.

`n < 1` always holds, so `1 − n s² > 0`. But as `b → 0` (an edge-on disk),
`n → 1` and `Π` grows while its prefactor `b` shrinks. Below `b < 1e-4 · a` the
strategy answers `None`, and the light is area-sampled. That is no worse than
today: area sampling of a nearly edge-on disk is what crust does now.

**Two-sidedness.** The paper's Figure 7 uses a two-sided disk, and its frame works
from either side, because `ẑ_d = −n̂` only fixes a handedness. Crust's imported
disks are one-sided, so the strategy is still built only in front, as below. A
two-sided disk seen from behind keeps area sampling, which is correct, just not
improved.

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

*Alternative considered: Gamito 2016's disk sampler.* It samples the circumscribed
spherical rectangle and rejects samples outside the disk. Guillén et al. report
that it needs a progressive low-discrepancy generator for its candidates and
cannot use a fixed-size point set. Its error is higher than the radial map's on
surfaces (their Figures 6 and 7). It is rejected for the same reason as D3.

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

1. ~~Which of Guillén's two maps~~: settled in task 2.1. It is the radial map, with
   exact Newton inversion and no tabulation (D5). Plain versus low-distortion
   radial is left to task 2.4's measurement.
2. Should the tube also get a solid-angle band (area sampling for a tube that is
   tiny on screen)? D1 is cheap and its back-facing win does not shrink with
   distance, so the answer is probably no. The sweep in task 5 answers it.
3. Does `--light-samples N` (several samples per vertex) want the tube's arc
   stratified across the N draws? It gets that for free from `openqmc`'s
   stratification over `(u, v)`. Confirm it in the sweep rather than design for it.
