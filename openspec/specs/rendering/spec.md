# rendering Specification

## Purpose

Turn a `Scene` (camera, world geometry, lights, render settings) into a pixel
buffer using physically-based path tracing. This capability covers the sampling
loop, the integrator (`ray_color`), participating-media transport, and the
parallel execution strategies. It is the core of `crust-core` (`tracer.rs`).

## Requirements

### Requirement: Path-traced integration with Multiple Importance Sampling

The renderer SHALL estimate per-pixel radiance by recursively path tracing
camera rays, combining direct light sampling and BRDF sampling with the balance
heuristic (MIS) at each surface interaction.

#### Scenario: Direct and indirect lighting are combined

- **WHEN** a camera ray hits a non-emissive surface from which a light is visible
- **THEN** the pixel radiance includes a direct-lighting term (from sampling the
  light and casting a shadow ray) and an indirect term (from a BRDF-sampled
  bounce), each weighted by the balance heuristic over the light PDF and BRDF PDF

#### Scenario: Recursion is bounded by max depth

- **WHEN** the bounce depth for a path reaches `max_depth`
- **THEN** the path terminates and contributes no further radiance (returns black)

#### Scenario: Emissive surfaces contribute their emission

- **WHEN** a ray hits a surface whose material emits light
- **THEN** the surface's emitted radiance is added to the path contribution

### Requirement: Anti-aliasing via multi-sampling with CMJ

The renderer SHALL average `samples_per_pixel` samples per pixel, using
Correlated Multi-Jittered (CMJ) sub-pixel offsets within the CMJ budget and
falling back to uniform random offsets once the budget is exhausted.

#### Scenario: Samples are averaged per pixel

- **WHEN** `samples_per_pixel` is N
- **THEN** each pixel value is the mean of N traced samples with jittered
  sub-pixel offsets

### Requirement: Parallel scanline and bucket rendering

The renderer SHALL offer two Rayon-parallel execution strategies that produce an
equivalent image buffer: a default scanline strategy (`render`) parallel over
pixels within each row, and a tiled strategy (`render_with_tiles`) parallel over
16×16 buckets.

#### Scenario: Default scanline rendering

- **WHEN** the renderer is invoked without bucket mode
- **THEN** `render()` fills the buffer, parallelising pixels within each scanline

#### Scenario: Bucket rendering

- **WHEN** bucket mode is requested (CLI `--bucket`)
- **THEN** `render_with_tiles()` divides the image into 16×16 tiles rendered in
  parallel and reassembles them into the same buffer

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

### Requirement: Sky-gradient background

The renderer SHALL return a vertical white-to-blue gradient based on ray
direction when a ray hits no geometry.

#### Scenario: Ray escapes the scene

- **WHEN** a ray intersects no object in the world
- **THEN** the pixel receives the sky-gradient color for that ray direction
