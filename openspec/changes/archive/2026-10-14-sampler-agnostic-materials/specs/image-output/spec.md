## MODIFIED Requirements

### Requirement: EXRs record how they were sampled

Every EXR `crust render` writes, the single beauty and each product alike,
SHALL carry as `crust:*` header attributes every setting that changes how its
pixels were sampled or what they converge to, named as the CLI flags and USD
`crust:*` attributes that set them, plus the fewest and most samples any pixel
took.

#### Scenario: The attribute set

- **WHEN** any render writes an EXR
- **THEN** it carries `crust:spp`, `crust:minSpp`, `crust:sppTaken`,
  `crust:varianceThreshold`, `crust:indirectClamp` (`0` when off),
  `crust:maxDepth`, `crust:lightSamples`, `crust:lightSamplesIndirect`,
  `crust:samplingStrategy`, `crust:lightSelection`, `crust:pixelFilter`,
  `crust:pixelFilterRadius` and `crust:sampler`

#### Scenario: A fixed-budget render

- **WHEN** `samples/cornellbox.usda` is rendered with `-s 16
  --indirect-clamp 0`
- **THEN** its EXR has `crust:spp = 16`, `crust:minSpp = 32`, `crust:sppTaken`
  of 16 and 16, and `crust:indirectClamp = 0`

#### Scenario: The filter used

- **WHEN** the render runs with `--filter gaussian --filter-radius 2`
- **THEN** its EXR has `crust:pixelFilter = gaussian` and
  `crust:pixelFilterRadius = 2`

#### Scenario: The sampler used

- **WHEN** the render runs with `--sampler latticeBn`
- **THEN** its EXR has `crust:sampler = latticeBn`; a render with no sampler setting has
  `crust:sampler = sobol`

#### Scenario: Every product is stamped

- **WHEN** a stage authors two RenderProducts
- **THEN** both EXRs carry the same `crust:*` attributes
