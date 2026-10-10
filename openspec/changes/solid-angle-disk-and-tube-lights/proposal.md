## Why

Light sampling is the main source of noise at 16 spp, and the two round UsdLux
shapes are the last ones still sampled uniformly by area (`lighting` design record,
"Known gaps: light sampling"; `docs/light_sampling.md` §3.2). Sphere lights sample
their visible cone and rect lights their spherical rectangle; `DiskLight` and
`CylinderLight` go through `AffineShape`'s uniform local-area sampler.

That costs the two shapes differently:

- **The tube wastes half its samples.** An open tube is convex, so from any point
  outside it about half its wall faces away. Every sample there is back-facing and
  carries zero radiance from a one-sided emitter. The estimator becomes "twice the
  visible contribution, half the time", which puts a floor of CV² ≈ 1 per sample
  under it, whatever the distance. Along a long tube near a receiver, the `1/r²`
  term also varies by orders of magnitude, and uniform `x` does not follow it.
- **The disk loses less, and only up close.** A one-sided disk is either entirely
  visible or entirely behind, so nothing is wasted. What is left is the variance of
  `cos θ_l / r²` across the disk. On axis over a diffuse receiver, a single
  area-sampled weight has CV ≈ 0.006 at radius/height 0.1, 0.41 at 1, and 1.6 at 3.
  So large softbox disks and receivers within about one radius gain, while small or
  distant downlights barely do.

The published methods are known (`docs/light_sampling.md` §5.2): Guillén et al.
2017 sample the spherical ellipse a disk subtends, and Arnold uses it. Gamito 2016
samples disks and cylinders by rejection, and Peters 2021 samples thin linear
lights. pbrt-v4 and Cycles still area-sample both shapes.

## What Changes

- **Cylinder lights sample only the visible part of their wall.** In the light's
  local space, the azimuth is drawn uniformly over the arc that faces the shading
  point, `|φ − φ₀| < acos(1/ρ)`. That arc is exact under any affine placement and
  closed form. Given the azimuth, the axial position is drawn by an equiangular
  density along that wall line, measured in world units, which follows its `1/r²`.
  This applies only to one-sided emitters and only from outside the tube. Everywhere
  else the light keeps area sampling.
- **Disk lights sample the spherical ellipse they subtend** (Guillén et al. 2017),
  in local space, mapped to world space with the solid-angle Jacobian the squashed
  sphere already uses. This applies strictly in front of the emitting side and
  within a solid-angle band like the rect's. Outside the band the light keeps area
  sampling.
- **`SolidAngleSampler` may refuse a single point** at which its density is not
  finite: the tube's silhouette, where the area-to-solid-angle Jacobian is infinite.
  Both MIS sides refuse it, which is the area path's existing convention. Sphere and
  rect strategies never refuse, and their output and instruction counts stay as they
  are.
- **Two environment switches** A/B each shape against the behaviour it replaces:
  `CRUST_TUBE_SAMPLING = area | arc | equiangular` and
  `CRUST_DISK_SAMPLING = area | ellipse`. `area` is today's sampler, bit for bit. The
  defaults are decided by the measurement in the tasks: each shape's new strategy
  becomes its default only if it does not lose at equal time on the checked-in
  samples and the scratch sweep.
- **Docs are updated where the sampler is listed.** That covers the `lighting`
  design record and its known gaps, `docs/light_sampling.md` §3.2 and §5.2, the
  `docs/architecture.md` switch table, and the user documentation's environment
  variables page.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `lighting`: the "Area lights" requirement no longer has disk and cylinder lights
  sample by area. It states which strategy each uses, and from where.

## Impact

- **`crust-core`, `light/`:**
  - `shape.rs`: two new `Strategy` variants, and `sample` / `pdf` returning
    `Option`.
  - A new `light/ellipse.rs` for the spherical-ellipse map and its elliptic
    integrals.
  - `area.rs`: no structural change. It already handles an absent sample and pdf.
- **The cylinder's strategy depends on the emitter**, so `AreaLight::new` tells the
  shape whether the emitter is one-sided. A two-sided tube seen through its open end
  shows its inner wall, which the visible arc would miss.
- **`config.rs`:** two `Config` fields.
- **No new dependency.** The elliptic integrals are Carlson's symmetric forms,
  implemented in safe Rust beside the map.
- **Renders without a disk or cylinder light are unchanged, bit for bit.** Renders
  with one change: they should be less noisy at 16 spp, and they must be unbiased
  against the area-sampled image.
