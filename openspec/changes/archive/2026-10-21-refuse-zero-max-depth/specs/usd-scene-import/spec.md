# Spec Delta

## MODIFIED Requirements

### Requirement: Render settings from USD with defaults

The importer SHALL read `resolution`, `camera` and `products` from
`UsdRenderSettings`. It SHALL read per-render params from custom attributes in
the `crust:` namespace (`crust:samplesPerPixel`, `crust:maxDepth`,
`crust:minSamplesPerPixel`, `crust:varianceThreshold`, `crust:frame`,
`crust:samplingStrategy`, `crust:pathGuiding`, `crust:guidingTrainIterations`,
`crust:guidingProb`, `crust:subdivisionLevel`). Missing attributes SHALL fall
back to defaults (128 spp, depth 32, 640×360, power MIS, guiding off,
subdivision level 0, no products). `crust:subdivisionLevel` SHALL be clamped to
0–6, with a warning when the authored value is out of range. A host override
(the CLI's `--subdiv-level`) SHALL take precedence over the authored value.
`crust:maxDepth` counts shaded path vertices, not bounces (1 is direct lighting
only); a value below 1 SHALL be refused with a `settings.invalid_value`
warning naming the attribute, and the default depth used.

#### Scenario: Authored settings

- **WHEN** the stage authors `crust:` render params
- **THEN** those values populate `RenderSettings`

#### Scenario: Missing settings fall back to defaults

- **WHEN** a `crust:` param is absent
- **THEN** the documented default is used in its place

#### Scenario: A max depth below one is refused

- **WHEN** the `RenderSettings` prim authors `int crust:maxDepth = 0` (or a
  negative value)
- **THEN** one `settings.invalid_value` warning names `crust:maxDepth`, and the
  render uses depth 32 instead of rendering black

#### Scenario: Subdivision level from render settings

- **WHEN** the `RenderSettings` prim authors `int crust:subdivisionLevel = 1`
  and no host override is given
- **THEN** every mesh whose scheme is not `none` is refined once

#### Scenario: No products

- **WHEN** the settings prim authors no `products`
- **THEN** no AOV is requested and the render writes the single beauty EXR

