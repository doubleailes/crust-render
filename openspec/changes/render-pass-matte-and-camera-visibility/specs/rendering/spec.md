# Spec Delta

## ADDED Requirements

### Requirement: Camera rays end at a holdout

A camera ray whose segment ends at a holdout, after cutouts are met or passed
and hidden light sources crossed, SHALL end there. The sample SHALL add nothing to
the beauty: no emission, no light sampling, no scattering. A cutout holdout SHALL
be met with its opacity's probability, like any cutout, so a partly present
holdout leaves a partly covered pixel.

#### Scenario: A holdout in front of an emitter

- **WHEN** an opaque holdout plane covers the whole frame in front of an emissive
  wall
- **THEN** every beauty pixel is exactly 0

#### Scenario: A half-present holdout

- **WHEN** a holdout plane of opacity 0.5 covers the whole frame in front of a
  white emissive wall of radiance 1
- **THEN** the beauty converges to 0.5 and the alpha to 0.5

#### Scenario: Interpenetrating edges resolve per sample

- **WHEN** a holdout sphere intersects a visible sphere
- **THEN** each sample shows whichever surface its camera ray reaches first,
  with no depth comparison after the render

### Requirement: Secondary rays see holdouts as geometry

Shadow rays, indirect rays and light-sampling rays SHALL meet a holdout as the
same geometry with the same material they would meet without the pass, so it
casts shadows and shows in reflections and refractions. A camera ray that
scatters (a volume scatter before the holdout) SHALL continue as a normal path.

#### Scenario: A holdout shadows the floor

- **WHEN** a holdout sphere stands on a diffuse floor under a sphere light
- **THEN** the floor's shadow is identical to the render without the pass

#### Scenario: A holdout in a mirror

- **WHEN** a mirror faces a holdout sphere
- **THEN** the mirror reflects the sphere as shaded without the pass, while the
  pixels where the camera sees the sphere directly are 0 with alpha 0

#### Scenario: Fog in front of a holdout

- **WHEN** a homogeneous volume region fills the space between the camera and a
  holdout wall
- **THEN** the fog's in-scattered light renders as without the pass, and the
  samples that cross it unscattered end on the wall with nothing

### Requirement: A world without holdouts renders as before

A world with no holdout SHALL execute the integrator's existing work per camera
ray plus one predictable branch, and render bit-identical images.

#### Scenario: The zero-AOV instruction count

- **WHEN** `cornellbox.usda` is rendered at `-s 2` under callgrind with and
  without this capability
- **THEN** the instruction counts differ by at most one branch per camera ray,
  and the images are bit-identical
