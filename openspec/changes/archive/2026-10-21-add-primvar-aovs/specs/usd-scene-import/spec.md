## MODIFIED Requirements

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

The vars SHALL be resolved before any geometry is loaded, so the geometry
import reads the primvars that `sourceType = "primvar"` vars name (see the
aovs capability, "Primvar sources"), and no others for them.

The importer SHALL refuse the following, each with one warning:

- a target that is not a RenderProduct or RenderVar;
- a product whose camera does not resolve;
- `sourceType = "intrinsic"`;
- `sourceType = "primvar"` with an empty `sourceName`.

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

#### Scenario: Primvar var accepted

- **WHEN** a RenderVar `Pref` authors `uniform token dataType = "point3f"`,
  `uniform string sourceName = "rest"` and
  `uniform token sourceType = "primvar"`
- **THEN** the product gets a closest-mode three-channel `Pref`, and no
  warning is raised for the var

#### Scenario: Primvar var without a name refused

- **WHEN** a RenderVar authors `sourceType = "primvar"` and no `sourceName`
- **THEN** a warning names the var and it is skipped
