## MODIFIED Requirements

### Requirement: Render settings from USD with defaults

The importer SHALL read `resolution` from `UsdRenderSettings` and per-render
params from custom attributes in the `crust:` namespace (`crust:samplesPerPixel`,
`crust:maxDepth`, `crust:minSamplesPerPixel`, `crust:varianceThreshold`,
`crust:frame`, `crust:samplingStrategy`, `crust:pathGuiding`,
`crust:guidingTrainIterations`, `crust:guidingProb`, `crust:subdivisionLevel`,
`crust:geometryLayout`).
Missing attributes SHALL fall back to defaults (128 spp, depth 32, 640×360,
power MIS, guiding off, subdivision level 0, geometry layout `packed`). `crust:subdivisionLevel` SHALL be
clamped to 0–6, with a warning when the authored value is out of range. A host
override (the CLI's `--subdiv-level`) SHALL take precedence over the authored
value. `crust:geometryLayout` SHALL accept `packed` or `compact`; any other
value SHALL fall back to `packed` with a warning, and the CLI's
`--geometry-layout` SHALL take precedence over it.

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

#### Scenario: Geometry layout from render settings

- **WHEN** the `RenderSettings` prim authors `token crust:geometryLayout = "compact"`
  and no host override is given
- **THEN** the scene's triangles are stored in the compact layout, and `--stats`
  reports it
