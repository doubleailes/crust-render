## MODIFIED Requirements

### Requirement: Render settings from USD with defaults

The importer SHALL read `resolution` from `UsdRenderSettings` and per-render
params from custom attributes in the `crust:` namespace (`crust:samplesPerPixel`,
`crust:maxDepth`, `crust:minSamplesPerPixel`, `crust:varianceThreshold`,
`crust:frame`, `crust:samplingStrategy`, `crust:pathGuiding`,
`crust:guidingTrainIterations`, `crust:guidingProb`,
`crust:guidingRoughnessThreshold`, `crust:subdivisionLevel`).
Missing attributes SHALL fall back to defaults (128 spp, depth 32, 640×360,
power MIS, guiding off, guiding roughness threshold 0.1, subdivision level 0).
`crust:guidingRoughnessThreshold` SHALL be clamped to 0–1, with a warning when the
authored value is out of range. `crust:subdivisionLevel` SHALL be
clamped to 0–6, with a warning when the authored value is out of range. A host
override (the CLI's `--subdiv-level`) SHALL take precedence over the authored
value.

#### Scenario: Authored settings

- **WHEN** the stage authors `crust:` render params
- **THEN** those values populate `RenderSettings`

#### Scenario: Missing settings fall back to defaults

- **WHEN** a `crust:` param is absent
- **THEN** the documented default is used in its place

#### Scenario: Subdivision level from render settings

- **WHEN** the `RenderSettings` prim authors `int crust:subdivisionLevel = 1`
  and no host override is given
- **THEN** every mesh whose scheme is not `none` is refined once

#### Scenario: Out-of-range guiding roughness threshold

- **WHEN** the `RenderSettings` prim authors `float crust:guidingRoughnessThreshold = 1.5`
- **THEN** a warning is emitted and the threshold used is 1
