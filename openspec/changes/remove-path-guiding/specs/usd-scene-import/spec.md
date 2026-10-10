## MODIFIED Requirements

### Requirement: Render settings from USD with defaults

The importer SHALL read `resolution`, `camera` and `products` from
`UsdRenderSettings`. It SHALL read per-render params from custom attributes in
the `crust:` namespace (`crust:samplesPerPixel`, `crust:maxDepth`,
`crust:minSamplesPerPixel`, `crust:varianceThreshold`, `crust:frame`,
`crust:samplingStrategy`, `crust:subdivisionLevel`). Missing attributes SHALL fall
back to defaults (128 spp, depth 32, 640×360, power MIS,
subdivision level 0, no products). `crust:subdivisionLevel` SHALL be clamped to
0–6, with a warning when the authored value is out of range. A host override
(the CLI's `--subdiv-level`) SHALL take precedence over the authored value.

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

#### Scenario: No products

- **WHEN** the settings prim authors no `products`
- **THEN** no AOV is requested and the render writes the single beauty EXR

### Requirement: Heuristics weigh colours by the working space's luminance

Every sampling heuristic that reduces a colour to one weight — light power,
lobe selection, environment importance, adaptive sampling —
and the `variance` AOV SHALL use the luminance weights of the working space,
the `Y` row of its RGB → XYZ matrix. In linear Rec.709 they SHALL be
(0.2126, 0.7152, 0.0722), and the render bit-identical to before.

#### Scenario: An ACEScg stage

- **WHEN** a stage renders in ACEScg
- **THEN** its lights and materials weigh colours by ACEScg's luminance

## ADDED Requirements

### Requirement: Removed path guiding settings are warned about

The importer SHALL NOT read `crust:pathGuiding`, `crust:guidingTrainIterations` or
`crust:guidingProb`. When the render settings prim authors any of them, whatever its
value (`false` included), the import SHALL raise one `settings.path_guiding_removed`
warning naming the attributes found, and the render SHALL run as if none were
authored.

#### Scenario: A stage that turned guiding on

- **WHEN** a stage authors `crust:pathGuiding = true` on its render settings
- **THEN** the import has exactly one `settings.path_guiding_removed` warning, and
  the render is bit-identical to the same stage without the attribute

#### Scenario: A stage that authored only the tuning attributes

- **WHEN** a stage authors `crust:guidingProb = 0.7` and nothing else about guiding
- **THEN** the import has one `settings.path_guiding_removed` warning

#### Scenario: A stage that never mentioned guiding

- **WHEN** no guiding attribute is authored
- **THEN** no `settings.path_guiding_removed` warning is raised
