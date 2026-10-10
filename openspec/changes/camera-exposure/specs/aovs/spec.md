## ADDED Requirements

### Requirement: Camera exposure scales the radiance outputs

The render SHALL multiply its outputs by the render camera's exposure scale once each
pixel's estimate is resolved, after accumulation: the beauty and every light path
expression var (the raw light sources and light groups included) by the scale, the
`variance` var by its square, and every other var (alpha, depth, distance, positions,
normals, `st`, sample count, albedo, diffuse filter, motion vector) not at all. The
intermediate images a progressive render publishes SHALL carry the same scale. The
integrator, the indirect clamp and adaptive sampling SHALL work in scene radiance, so
the samples a render takes do not depend on the exposure. An exposure scale of exactly
1 SHALL leave every output bit-identical to a render that reads no exposure.

#### Scenario: One stop brighter

- **WHEN** a stage is rendered with the camera's `exposure = 0`, then with
  `exposure = 1`, at the same settings
- **THEN** every beauty and light path expression channel of the second render is
  exactly twice the first's, its `variance` channel exactly four times, and its depth,
  normal and sample count channels are identical

#### Scenario: Exposure does not change the samples taken

- **WHEN** a stage is rendered with adaptive sampling at `exposure = 0` and at
  `exposure = 3`
- **THEN** both renders take the same number of samples in every pixel

#### Scenario: No exposure, no change

- **WHEN** a camera authors no exposure attribute
- **THEN** every output is bit-identical to the same render before this requirement
