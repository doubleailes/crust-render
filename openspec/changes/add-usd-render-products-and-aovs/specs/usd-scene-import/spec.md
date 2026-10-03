## ADDED Requirements

### Requirement: Render products and render vars

The importer SHALL follow the render settings prim's `products` relationship
to `RenderProduct` prims, and each product's `orderedVars` to `RenderVar`
prims. Resolution follows `UsdRenderComputeSpec`:

- each product starts from the settings prim's `RenderSettingsBase` values,
  with schema fallbacks;
- it overrides only the attributes it authors (`camera`, `resolution`);
- var order follows `orderedVars`, and a var targeted twice in one product is
  used once.

Attributes SHALL be evaluated at the render's time code, so a time-sampled
`productName` gives per-frame names.

The importer SHALL accept authored values in these forms:

- `sourceType` authored as a token or a string;
- `driver:parameters:aov:name` authored as a token or a string;
- `driver:parameters:aov:*` keys on vars, and `driver:parameters:*` keys on
  products.

The importer SHALL refuse the following, each with one warning:

- a target that is not a RenderProduct or RenderVar;
- a product whose camera does not resolve;
- `sourceType = "intrinsic"`.

The render's camera and resolution SHALL be the first product's resolved
values. `--camera` SHALL still take precedence.

#### Scenario: Product inherits settings

- **WHEN** the settings author `resolution = (640, 360)` and a camera, and a
  product authors neither
- **THEN** the product renders at 640×360 through the settings' camera

#### Scenario: Product overrides resolution

- **WHEN** the first product authors `resolution = (320, 180)`
- **THEN** the render is 320×180

#### Scenario: Houdini-authored var

- **WHEN** a RenderVar `Z` authors `token dataType = "float"`,
  `sourceName = "Z"`, `string driver:parameters:aov:name = "depth"` and
  `bool driver:parameters:aov:multiSampled = 0`
- **THEN** the product gets a closest-mode camera-depth channel named `depth`

#### Scenario: Intrinsic source refused

- **WHEN** a RenderVar authors `sourceType = "intrinsic"`
- **THEN** a warning names the var and it is skipped

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
