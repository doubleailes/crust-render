## MODIFIED Requirements

### Requirement: Render settings from USD with defaults

The importer SHALL read `resolution` from `UsdRenderSettings` and per-render
params from custom attributes in the `crust:` namespace (`crust:samplesPerPixel`,
`crust:maxDepth`, `crust:minSamplesPerPixel`, `crust:varianceThreshold`,
`crust:frame`, `crust:samplingStrategy`, `crust:pathGuiding`,
`crust:guidingTrainIterations`, `crust:guidingProb`, `crust:subdivisionLevel`).
Missing attributes SHALL fall back to defaults (128 spp, depth 32, 640×360,
power MIS, guiding off, subdivision level 0). `crust:subdivisionLevel` SHALL be
clamped to 0–6, with a warning when the authored value is out of range. A host
override (the CLI's `--subdiv-level`) SHALL take precedence over the authored
value.

An authored value outside its valid range SHALL be refused with a warning and replaced
by its default, never converted into a different number: `crust:samplesPerPixel` below
1, a negative `crust:maxDepth` or `crust:minSamplesPerPixel`, and a `resolution` with
either component below 1. A `crust:maxDepth` of 0 is valid.

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

#### Scenario: Negative sample count

- **WHEN** the `RenderSettings` prim authors `int crust:samplesPerPixel = -1`
- **THEN** the load completes, a warning names the attribute and its value, and the
  render uses 128 samples per pixel

#### Scenario: Negative depth

- **WHEN** the `RenderSettings` prim authors `int crust:maxDepth = -4`
- **THEN** a warning is logged and the render uses depth 32

#### Scenario: Non-positive resolution

- **WHEN** `RenderSettings` authors `int2 resolution = (0, 360)` or `(-640, 360)`
- **THEN** a warning is logged and the image is 640×360

## ADDED Requirements

### Requirement: Malformed topology is refused per prim

A prim whose topology cannot be read consistently SHALL be skipped with one warning
naming the prim, and the rest of the stage SHALL load. Malformed input SHALL never
end the process. This holds wherever the prim is reached: directly, as an instance
prototype's part, under uniform or adaptive subdivision, and in the base-cage
fallback.

#### Scenario: Negative face vertex count

- **WHEN** a `Mesh` authors `faceVertexCounts = [4, -1, 3]`
- **THEN** that mesh contributes no geometry, a warning names the prim and the face,
  and every other prim on the stage imports

#### Scenario: Negative face vertex count on a subdivision mesh

- **WHEN** a `catmullClark` mesh with a negative `faceVertexCounts` entry is imported
  at `crust:subdivisionLevel = 2`
- **THEN** the mesh is skipped with a warning rather than falling back to a cage that
  cannot be triangulated

#### Scenario: Negative curve vertex count

- **WHEN** a `BasisCurves` prim authors a negative `curveVertexCounts` entry
- **THEN** that prim contributes no curves and a warning names it

#### Scenario: Empty widths

- **WHEN** a `BasisCurves` prim authors `float[] widths = []`
- **THEN** its curves import with the unauthored default width of 1

#### Scenario: Volume grid too large to address

- **WHEN** a `grid` volume authors `crust:volume:gridDims` whose product exceeds the
  addressable cell count
- **THEN** the volume is skipped with the existing dims/data mismatch warning

### Requirement: Transforms honour resetXformStack on every prim

A transformable prim whose `xformOpOrder` begins with `!resetXformStack!` SHALL take
its local transform as its world transform, ignoring every ancestor's. This SHALL
apply whatever the prim's type, and wherever transforms are composed: the stage
traversal, instance prototypes, and the camera. The token anywhere but first in
`xformOpOrder` SHALL be ignored with a warning.

#### Scenario: Reset on curves

- **WHEN** a `BasisCurves` prim under an Xform translated by `(10, 0, 0)` authors
  `xformOpOrder = ["!resetXformStack!", "xformOp:translate"]` with translate `(1, 0, 0)`
- **THEN** its curves are placed at `x = 1`, not `x = 11`

#### Scenario: Reset on a PointInstancer

- **WHEN** a `PointInstancer` under a translated Xform resets the transform stack
- **THEN** its instances are placed relative to the world origin, not to the parent

#### Scenario: Reset inside a prototype

- **WHEN** a part of an instance prototype resets the transform stack
- **THEN** the part is placed relative to the prototype root, exactly as before this
  change, and that holds for every prim type

#### Scenario: Misplaced reset token

- **WHEN** `!resetXformStack!` appears after the first entry of `xformOpOrder`
- **THEN** it is ignored with a warning and the prim inherits its parent's transform

### Requirement: Nested native instances are imported

An `instanceable` prim inside another instance's prototype SHALL contribute its own
prototype's geometry, placed by the composition of every transform between the outer
prototype root and each part. A nested prototype SHALL be built once per stage and
shared by every placement that reaches it. Nesting deeper than the importer's
instance-nesting limit SHALL be refused with a warning, as for nested `PointInstancer`s.

#### Scenario: One level of nesting

- **WHEN** prototype `_Outer` holds a sphere at the origin and an `instanceable` prim
  referencing `_Inner` (a sphere) translated by `(3, 0, 0)`, and the stage places one
  instance of `_Outer`
- **THEN** both spheres render: one at the origin and one at `x = 3`

#### Scenario: Nested prototype shared across outer placements

- **WHEN** two instances of `_Outer` are placed at different positions
- **THEN** each shows the nested `_Inner` sphere offset by `(3, 0, 0)` from its own
  placement, and `_Inner` is built once

#### Scenario: Nested prototype in adaptive subdivision

- **WHEN** a target edge length is set and a nested native prototype holds a
  subdivision mesh
- **THEN** that mesh is refined to the uniform level, as shared geometry is
