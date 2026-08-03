## MODIFIED Requirements

### Requirement: Material resolution by shader id

The importer SHALL resolve a bound material via `MaterialBindingAPI` and dispatch
on the surface shader's `info:id`: `UsdPreviewSurface` maps into `OpenPBR`
(portable field mapping), `crust:openpbr` decodes 1:1 into `OpenPBR`, and any
geometry without a resolvable bound material falls back to a default grey `OpenPBR`.

Color-valued inputs mapped by this requirement SHALL be converted to linear
according to the color-management capability's per-source rules before being
stored on the resulting `OpenPBR` material; scalar (non-color) inputs SHALL be
used at their authored value with no such conversion.

#### Scenario: UsdPreviewSurface binding

- **WHEN** a surface binds a shader with `info:id = "UsdPreviewSurface"`
- **THEN** its inputs are mapped into an `OpenPBR` material (e.g. `diffuseColor →
  baseColor`, `metallic → baseMetalness`, `roughness → specularRoughness`)

#### Scenario: UsdPreviewSurface color inputs are decoded from sRGB

- **WHEN** a surface binds a `UsdPreviewSurface` shader authoring
  `diffuseColor` and/or `emissiveColor`
- **THEN** each is decoded from sRGB to linear before being stored as the
  `OpenPBR` material's `baseColor`/`emissionColor`, rather than passed
  through unconverted

#### Scenario: crust:openpbr binding

- **WHEN** a surface binds a shader with `info:id = "crust:openpbr"`
- **THEN** each camelCase input is decoded 1:1 into the matching `OpenPBR`
  field, with color-valued fields treated as already linear (no conversion
  applied)

#### Scenario: Unbound geometry

- **WHEN** geometry has no resolvable bound material
- **THEN** it is assigned a default grey `OpenPBR` material

### Requirement: Light schema mapping

The importer SHALL map `UsdLuxSphereLight` to an `Emissive` sphere that is both a
light and visible geometry, and `UsdLuxRectLight` to two emissive triangles
plus an `AreaLight(RectShape)` (local XY plane, emitting along -Z per UsdLux —
effectively one-sided). Other lux schemas (`DiskLight`, `DistantLight`,
`DomeLight`, `CylinderLight`) SHALL warn once and be skipped.

Every mapped light's color (`inputs:color` on any of `UsdLuxDistantLight`,
`UsdLuxDomeLight`, `UsdLuxSphereLight`, `UsdLuxRectLight`) SHALL be treated as
already linear per the color-management capability, with no conversion
applied, since it is authored in the rendering color space.

#### Scenario: Sphere light

- **WHEN** a `UsdLuxSphereLight` prim is traversed
- **THEN** it becomes an emissive sphere added to both the light list and the world

#### Scenario: Rect light

- **WHEN** a `UsdLuxRectLight` prim is traversed
- **THEN** it becomes two emissive triangles added to both the light list and
  the world

#### Scenario: Unsupported lux light

- **WHEN** a `DiskLight`, `DistantLight`, `DomeLight`, or `CylinderLight` prim
  is traversed
- **THEN** a warning is emitted and the light is skipped

#### Scenario: Light color passes through unconverted

- **WHEN** any supported lux light prim authors `inputs:color`
- **THEN** the value used for that light's emission is the authored value
  unchanged, with no color-space conversion applied

### Requirement: Volume region import

Any prim carrying a `crust:volume:type` attribute SHALL import as a
free-standing `VolumeRegion` rather than geometry — checked before the
mesh/sphere/light dispatch, so its bounds never occlude shadow rays. The
region's box comes from the prim's `size` (when it is a `Cube`) or the unit
cube, oriented and scaled by the composed prim transform, with density
(`homogeneous` | `smoke` procedural fBm noise | `grid` inline voxel data) and
σₛ/σₐ/anisotropy/emission read from `crust:volume:*` attributes.

The color-valued coefficients `crust:volume:sigmaS`, `crust:volume:sigmaA`,
and `crust:volume:emission` SHALL be treated as already linear per the
color-management capability, with no conversion applied.

#### Scenario: Volume prim

- **WHEN** a prim authors `crust:volume:type`
- **THEN** it is added to the scene's volume regions, not to world geometry

#### Scenario: Missing grid data

- **WHEN** a `grid`-type volume's `gridData` length does not match
  `gridDims`' `nx·ny·nz`
- **THEN** a warning is emitted and the volume is skipped

#### Scenario: Volume coefficients pass through unconverted

- **WHEN** a volume region prim authors `crust:volume:sigmaS`,
  `crust:volume:sigmaA`, or `crust:volume:emission`
- **THEN** the coefficient value used in transport is the authored value
  unchanged, with no color-space conversion applied
