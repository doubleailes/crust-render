# rendering Specification

## Purpose

Turn a `Scene` (camera, world geometry, lights, render settings) into a pixel
buffer using physically-based path tracing. This capability covers the sampling
loop, the integrator (`ray_color`), participating-media transport, and the
parallel execution strategies. It is the core of `crust-core` (`tracer/`).

## Requirements

### Requirement: Path-traced integration with Multiple Importance Sampling

The renderer SHALL estimate per-pixel radiance by path tracing camera rays
(an iterative two-pass walk: a forward pass recording one vertex per bounce,
then a backward gather), combining direct light sampling (NEE) and BSDF
sampling at each surface interaction via a selectable `SamplingStrategy`
(`crust:samplingStrategy` scene attribute / `--strategy` CLI override):
`power` (β=2 power heuristic, the default), `balance` (balance heuristic), and
the diagnostic single-strategy modes `light` (NEE only) and `bsdf` (BSDF
sampling only, no shadow rays). All four strategies are unbiased.

#### Scenario: Direct and indirect lighting are combined

- **WHEN** a camera ray hits a non-emissive surface from which a light is visible
- **THEN** the pixel radiance includes a direct-lighting term (from sampling the
  light and casting a shadow ray) and an indirect term (from a BSDF-sampled
  bounce), each weighted by the active sampling strategy over the light PDF and
  BSDF PDF

#### Scenario: Recursion is bounded by max depth

- **WHEN** the bounce depth for a path reaches `max_depth`
- **THEN** the path terminates and contributes no further radiance (returns black)

#### Scenario: Russian roulette terminates long paths probabilistically

- **WHEN** a path reaches its 4th vertex or beyond
- **THEN** survival is sampled from the path's throughput (with a minimum
  survival probability floor), and the throughput is divided by the survival
  probability on paths that continue

#### Scenario: Emissive surfaces contribute their emission

- **WHEN** a ray hits a surface whose material emits light
- **THEN** the surface's emitted radiance is added to the path contribution

### Requirement: Anti-aliasing via multi-sampling with low-discrepancy sampling

The renderer SHALL average `samples_per_pixel` samples per pixel, drawing
sub-pixel offsets and all other per-sample randomness through the `Sampler`
trait — Owen-scrambled Sobol (`SobolSampler`, per-pixel decorrelated) in
production, falling back to a seeded RNG (`RngSampler`) once the Sobol
dimension budget is exhausted.

#### Scenario: Samples are averaged per pixel

- **WHEN** `samples_per_pixel` is N
- **THEN** each pixel value is the mean of N traced samples with
  low-discrepancy sub-pixel offsets

### Requirement: Adaptive sampling stops pixels early

The renderer SHALL keep, for each pixel of an adaptive pass, a convergence
index `e`: the relative standard error of the pixel mean divided by
`crust:varianceThreshold`, so that `e < 1` means the pixel passes its own
test. A pixel that has recorded no sample with non-zero luminance has
`e = +∞`: a zero measured variance from zero observations is not evidence of
convergence.

The renderer SHALL stop sampling a pixel only when all of the following hold:

- it has taken at least the effective minimum, `max(crust:minSamplesPerPixel, ⌈√N⌉)`
  samples, where `N` is the pass's per-pixel sample budget;
- its own index is below 1 (which implies it has recorded some light);
- none of its four cross neighbours (up, down, left, right, inside the
  image) that is still sampling has an index more than
  `crust:adaptiveNeighbourTolerance` above its own: `e_q − e_p ≤ t` for every
  such neighbour `q`.

A neighbour that has stopped, whether converged or out of budget, SHALL NOT
hold a pixel back, and a pixel that has stopped SHALL NOT resume. Diagonal
neighbours are not compared. A negative tolerance disables the neighbour
comparison, and each pixel then stops exactly as it would on its own. A
`crust:varianceThreshold` of 0 disables adaptive sampling. Adaptive sampling
applies to the main/final render pass, never to path-guiding training
passes, and the image SHALL be bit-identical under the tiled and scanline
strategies whatever the tolerance.

#### Scenario: A converged pixel stops early

- **WHEN** a pixel has taken at least the effective minimum, its index is
  below 1, and no still-sampling cross neighbour exceeds its index by more
  than the tolerance
- **THEN** no further samples are traced for that pixel

#### Scenario: A pixel that has seen no light does not stop early

- **WHEN** adaptive sampling is enabled and every sample a pixel has taken
  returned zero radiance
- **THEN** the pixel keeps sampling past the minimum sample count, taking the
  full per-pixel budget if no sample ever returns light

#### Scenario: The minimum grows with the budget

- **WHEN** a render asks for 1024 samples per pixel with
  `crust:minSamplesPerPixel = 8`
- **THEN** no pixel stops before 32 samples

#### Scenario: A less converged cross neighbour holds a pixel

- **WHEN** a pixel's index is 0.5, the tolerance is 1, and its left
  neighbour is still sampling with an index of 2
- **THEN** the pixel keeps sampling until that neighbour's index falls to 1.5
  or below, or that neighbour stops, or the budget is exhausted

#### Scenario: A diagonal neighbour does not hold a pixel

- **WHEN** the only neighbour of a pixel whose index exceeds its own by more
  than the tolerance is diagonal to it
- **THEN** the pixel stops as if that neighbour were converged

#### Scenario: A negative tolerance is the per-pixel stop

- **WHEN** `crust:adaptiveNeighbourTolerance` is negative
- **THEN** every pixel takes exactly the samples it would take if it were
  rendered alone

#### Scenario: Tiles and scanlines agree under the neighbour comparison

- **WHEN** a scene renders with a non-negative tolerance under both the
  tiled and the scanline strategy
- **THEN** the two images are bit-identical

### Requirement: Adaptive neighbour tolerance setting

The importer SHALL read `crust:adaptiveNeighbourTolerance` (float, in units
of the convergence index, default 1) from the render settings prim. A
non-finite value SHALL fall back to the default, with a warning naming the
authored value.

#### Scenario: Unauthored tolerance

- **WHEN** `crust:adaptiveNeighbourTolerance` is not authored
- **THEN** the render compares cross neighbours with tolerance 1

#### Scenario: Non-finite tolerance

- **WHEN** `crust:adaptiveNeighbourTolerance = nan` is authored
- **THEN** the render uses tolerance 1 and logs a warning

### Requirement: Parallel scanline and bucket rendering

The renderer SHALL offer two Rayon-parallel execution strategies that produce a
**bit-identical** image buffer, guided renders included: a scanline strategy
(`render`) parallel over pixels within each row, and a tiled strategy
(`render_with_tiles`) parallel over 16×16 buckets. The choice is the caller's —
the `Renderer` API favours neither — and the CLI defaults to tiles (see the cli
spec). Progress callbacks SHALL be delivered one at a time, with the completed
count increasing by one per report, under either strategy.

#### Scenario: Scanline rendering

- **WHEN** `render()` is called (CLI `--scanline`)
- **THEN** it fills the buffer, parallelising pixels within each scanline

#### Scenario: Bucket rendering

- **WHEN** `render_with_tiles()` is called (the CLI default)
- **THEN** it divides the image into 16×16 tiles rendered in parallel and
  reassembles them into the same buffer

#### Scenario: A guided render is independent of the strategy

- **WHEN** a guided render runs under either strategy
- **THEN** each pass's guiding training samples and variance sum are gathered
  in scanline order, so the two strategies produce the same bits

### Requirement: Participating-media transport for medium-carrying rays

When a ray carries a medium, the renderer SHALL apply Henyey-Greenstein phase
scattering and Beer-Lambert attenuation across the traversed segment. Rays
travelling in free space (no medium) SHALL be unaffected.

#### Scenario: Volumetric scattering event

- **WHEN** a ray carries a scattering medium and a sampled scatter distance is
  closer than the surface hit
- **THEN** a Henyey-Greenstein scattering event is kicked at that distance and
  the surface interaction is skipped for that bounce

#### Scenario: Free-space rays are unattenuated

- **WHEN** a ray carries no medium
- **THEN** no Beer-Lambert attenuation is applied to its contribution

### Requirement: Free-standing volume regions

The renderer SHALL transport light through the scene's volume regions
(smoke, fog, absorption and emissive volumes) held outside the surface BVH:
distance sampling by weighted delta tracking against each region's
extinction majorant, direct lighting with MIS at volume scatter vertices,
and transmittance-aware shadow rays (stochastic ratio tracking for
heterogeneous regions, exact Beer-Lambert for homogeneous ones). Scenes
without volume regions SHALL render exactly as before.

#### Scenario: Scatter event inside a volume region

- **WHEN** a path segment crosses a volume region and the tracking walk
  produces a real collision before the nearest surface
- **THEN** the path scatters there via the Henyey-Greenstein phase function,
  gathers direct lighting with the light/phase balance heuristic, and the
  bounce-hit emission of the continuation is MIS-weighted against the same
  light strategy

#### Scenario: Shadow rays attenuate through volumes

- **WHEN** an NEE shadow ray crosses a volume region without surface occlusion
- **THEN** the direct-lighting contribution is multiplied by the volumetric
  transmittance along the segment rather than treated as fully visible

#### Scenario: Emissive volumes glow

- **WHEN** a path segment crosses a region with nonzero emission
- **THEN** the segment accumulates `σₐ·Lₑ` source radiance weighted by the
  transmittance up to each emission point

### Requirement: Opt-in path guiding

When `crust:pathGuiding` is set on the scene's render settings, the renderer
SHALL run `render_guided()`: a pure-Rust Practical Path Guiding SD-tree
(`GuidingField`) trained over progressive passes at geometrically growing spp
budgets, then a final pass sampling secondary bounces by one-sample MIS
between the trained field and the BSDF. All passes (training and final) SHALL
be blended into the output, weighted by inverse variance. Scenes without
`crust:pathGuiding` SHALL render via the ungated path (no SD-tree, no extra
training passes).

#### Scenario: Guiding trains then renders

- **WHEN** `crust:pathGuiding = true` on the render settings
- **THEN** the renderer runs geometrically-growing training passes that splat
  samples into the SD-tree, then a final pass that mixes guided and
  BSDF-sampled directions at secondary bounces

#### Scenario: Delta lobes and untrained regions fall back to the BSDF

- **WHEN** a scattering event has no continuous BSDF component (a delta lobe)
  or lands in an untrained region of the field
- **THEN** the direction is sampled from the BSDF alone, and the estimate
  stays unbiased

### Requirement: Cutout surfaces are stochastic presence

A hit on a surface whose material reports an opacity below 1 SHALL be met with
probability equal to that opacity, and otherwise passed through. The path SHALL
continue along the same line to the next hit, spending no depth, adding no
emission and recording no vertex, while the carried medium, volume regions and
texture footprint keep measuring the segment from its origin. A shadow ray
SHALL be attenuated by `1 − opacity` at every cutout it crosses, by the factor
"Thin walls report their straight transmittance" defines at every thin-walled
transmissive surface it crosses, and blocked by any other surface. A world with
no cutout material and no thin-walled transmissive material SHALL render exactly
as it did before cutouts existed.

#### Scenario: A half-present sphere in a furnace

- **WHEN** a black sphere of opacity 0.5 sits in a white furnace of radiance 1
- **THEN** a ray through it sees 0.25, the chance of passing both of its
  crossings

#### Scenario: Every strategy agrees through a cutout

- **WHEN** a black sheet of opacity 0.5 hangs between a diffuse floor and a
  sphere light
- **THEN** the power-MIS, light-only and BSDF-only estimates of the floor all
  agree with half of the unoccluded power-MIS estimate

#### Scenario: Opaque occluders are unaffected

- **WHEN** an opaque sheet hangs between a floor and its light, in a world with
  or without a cutout elsewhere
- **THEN** light sampling alone finds the floor unlit

### Requirement: Thin walls report their straight transmittance

A material whose thin-walled transmission leaves a ray's direction unchanged
SHALL report, for each crossing direction ω, the RGB weight `T(ω)` its BSDF
gives that straight transmission. With opacity `α`, the fraction of a ray that
continues unscattered along the same line is `P(ω) = (1 − α) + α · T(ω)`.
`Material::resolve` SHALL report the same `T(ω)` as per-query shading.

#### Scenario: Straight transmittance plus the rest is the whole BSDF

- **WHEN** a thin-walled `open_pbr_surface` with `transmission_weight = 1` is
  evaluated for a crossing direction
- **THEN** `T(ω)` plus the albedo of the material without its straight
  transmission equals the full material's albedo

#### Scenario: Thick glass reports none

- **WHEN** a closed, thick (not thin-walled) dielectric stands between a floor
  and a light
- **THEN** it reports no straight transmittance, and shadow rays are blocked by
  it, as before this change

### Requirement: Shadow rays pass thin walls

A shadow ray SHALL be multiplied by `P(ω)` at every surface that reports a
straight transmittance, and SHALL NOT be blocked by it.

#### Scenario: Light through a window is found by every strategy

- **WHEN** a thin-walled sheet with `transmission_weight = 1` and a coloured
  `transmission_color` hangs between a diffuse floor and a sphere light
- **THEN** the power-MIS, light-only and BSDF-only estimates of the floor agree,
  and light sampling alone finds the light through the sheet

#### Scenario: A sheet that is also a cutout

- **WHEN** a thin-walled transmissive sheet has an opacity of 0.5 and a straight
  transmittance `T`
- **THEN** shadow rays through it are multiplied by `0.5 + 0.5 · T`, and every
  strategy agrees on the floor beneath it

### Requirement: Paths pass thin walls stochastically

A path SHALL pass a surface that reports a straight transmittance with
probability `q = max over channels of P(ω)`, scaling its throughput by
`P(ω) / q`, spending no depth, adding no emission, recording no vertex and
keeping the previous vertex's MIS record. Otherwise it SHALL scatter through the
material without its straight transmission, scaling its throughput by
`α / (1 − q)`.

#### Scenario: The expectation does not move

- **WHEN** the window scene is rendered before and after this change, at
  increasing sample counts with the indirect clamp off
- **THEN** the difference between the two falls as 1/√N, with either sign pixel
  to pixel, and the relative error at equal time is lower after

### Requirement: Thin-wall passes keep their meaning

A pass through a thin wall SHALL be a specular transmission event (`TS`) for
light path expressions, as the delta transmission it replaces was. Passing thin
walls SHALL NOT change any pixel's expected value, only its noise.

#### Scenario: Light path expressions keep their meaning

- **WHEN** a product requests `C<TS>.*L` and `C.*[LO]` on the window scene
- **THEN** light seen through the sheet lands in the `TS` expression, and
  `C.*[LO]` equals the beauty bit for bit

### Requirement: A pass-through crosses each surface once

A path segment or shadow ray that passes hidden light sources, cutouts or thin walls
SHALL count each surface it crosses exactly once. A hit on the same primitive, from the
same side, within `1e-3 · t` of a crossing of it is a numerical re-hit and SHALL NOT add
emission, opacity or transmittance a second time. Two distinct surfaces, however close,
SHALL both be crossed — including two placements of one instanced prototype that report
the same geometry id.

#### Scenario: A small hidden light far away

- **WHEN** a diffuse plane under one camera-invisible sphere light, at a
  distance-to-radius ratio of 40, 160 or 200, is rendered with BSDF sampling alone and
  with light sampling alone
- **THEN** the two estimates of the plane agree within noise that falls as 1/√N, and no
  pixel's BSDF-only value is twice that of the same light made solid

#### Scenario: Two stacked placements of one card

- **WHEN** two placements of one half-opaque card prototype, labelled to report the same
  geometry id, stand 0.0005 apart facing the same way between a floor and a light
- **THEN** both are crossed, on the shadow side and the bounce side, as `(1 − 0.5)²`

#### Scenario: Two cards close together

- **WHEN** two half-opaque cutout cards on different primitives stand 0.0005 apart
  between a floor and a light
- **THEN** shadow rays are attenuated by both cards, as `(1 − 0.5)²`

### Requirement: Rendering a region of the frame

When the render has a region smaller than the frame, the renderer SHALL
trace only the pixels inside it. It SHALL keep the full-frame camera,
resolution and per-pixel sampling. Everything derived from the resolution
SHALL be computed for the full frame:

- ray-cone texture filtering;
- adaptive subdivision's screen rate;
- frustum culling.

A pixel's value SHALL be bit-identical to the same pixel in a full-frame
render of the same scene and settings whenever its sample count does not
depend on its neighbours: a fixed sample count, or adaptive sampling with
no neighbour tolerance. Under the adaptive neighbour hold, a neighbour
outside the region SHALL count as absent.

Path guiding SHALL train on the region's paths only. A guided region is
therefore not bit-identical to the same pixels of a guided full render.

#### Scenario: A crop matches the full render

- **WHEN** `samples/cornellbox.usda` is rendered at `-s 16` once full-frame
  and once with `--region 37,21,101,77`
- **THEN** every pixel, every AOV channel and `sampleCount` of the crop are
  bitwise equal to the full render's at the same coordinates

#### Scenario: Tiles and scanlines agree on a region

- **WHEN** the same region is rendered once with tiles and once with
  `--scanline`
- **THEN** the two images are bitwise equal

#### Scenario: The full frame is unchanged

- **WHEN** no region is authored and `--region` is not given
- **THEN** the image is bitwise equal to the image rendered before this
  change

### Requirement: Paths cross medium boundaries without a vertex

A segment that hits a medium boundary SHALL continue past it without a vertex,
spending no path depth and keeping the previous vertex's MIS record, in the
medium the crossing leaves it in: a boundary's front face met in vacuum enters
its medium; that boundary's own back face leaves it; any other crossing changes
nothing. While a path is inside a boundary's medium, every ray it traces SHALL
travel in that medium.

#### Scenario: An absorbing boundary dims by its chord

- **WHEN** a unit sphere bound to a volume-only absorber with σₐ = (0.25, 0.5,
  1) is seen through its centre against a white sky
- **THEN** the radiance is e^{−2σₐ} within the 0.001 restart epsilon, whatever
  the length of the camera ray's direction

### Requirement: Scatters inside a medium boundary sample lights

A scatter in a boundary's medium SHALL run light sampling with the medium's
phase function, MIS-weighted against phase sampling, and its shadow rays SHALL
cross boundaries under the same rules as paths, attenuated by Beer–Lambert
through each medium they travel in.

#### Scenario: A white furnace stays white

- **WHEN** a non-absorbing scattering volume-only sphere, optionally with a
  white Lambertian sphere inside it, sits under a uniform white sky
- **THEN** the mean radiance through it is 1 within 0.025, under power MIS,
  light sampling alone and phase sampling alone

### Requirement: A world without medium boundaries renders as before

A world in which no material is a medium boundary SHALL render bit for bit as
it did before medium boundaries existed.

#### Scenario: Existing samples are unchanged

- **WHEN** every sample scene without a medium boundary or a volume region is
  rendered at 16 spp before and after
- **THEN** the images are bit-identical

### Requirement: Volume regions measure distance

Free flights and transmittance through `crust:volume` regions SHALL be measured
in distance, whatever the length of the ray's direction.

#### Scenario: Region fog does not depend on the focus distance

- **WHEN** a pinhole camera renders a region of fog at focus distance 1 and at
  focus distance 10
- **THEN** the two images are the same within noise
