# Spec Delta

## ADDED Requirements

### Requirement: A curve hit knows its place on the strand

A hit on a `BasisCurves` or `NurbsCurves` curve SHALL carry the index of the
curve within its prim and `v`, its position along the whole strand in [0, 1]:
the curve's parameter normalised so the first authored point (or the start of
the NURBS domain, narrowed by `ranges`) is 0 and the last is 1. `v` SHALL be
continuous across span boundaries and SHALL hold at every placement: top-level,
instanced, `PointInstancer`-placed and motion-blurred.

#### Scenario: v runs root to tip on a cubic strand

- **WHEN** a material outputs `curve:v` as its colour on a cubic `bspline`
  strand of 7 control points (4 spans)
- **THEN** the strand reads 0 at its first span's start, 0.25 at the boundary
  between its first and second spans, and 1 at its last span's end

#### Scenario: v on a linear curve follows its points

- **WHEN** the same material is on a `linear` curve of 5 points
- **THEN** a hit on the third segment at its midpoint reads `v` = 0.625

#### Scenario: v on a trimmed NURBS curve spans the trimmed domain

- **WHEN** an order-4 `NurbsCurves` curve authors `ranges = [(0, 0.5)]`
- **THEN** the rendered half of the curve reads `v` from 0 at its start to 1
  at its trimmed end

#### Scenario: Curve index counts within the prim

- **WHEN** a `BasisCurves` prim authors `curveVertexCounts = [4, 4, 4]` and
  its material outputs `curve:index`
- **THEN** the three strands read 0, 1 and 2

#### Scenario: Instanced strands keep their prototype's v and index

- **WHEN** a `PointInstancer` places a prototype hair clump at two rotations
- **THEN** both placements read the same `v` and curve index at corresponding
  points of corresponding strands

### Requirement: Curve primvars are read for the materials that use them

`constant`, `uniform`, `varying` and `vertex` primvars on `BasisCurves` and
`NurbsCurves` of type `float`, `float2`, `float3`, `color3f` and `int` (read as
float) SHALL be read when a material bound to the prim reads them by name, and
interpolated at a hit as USD specifies: `uniform` per curve, `varying` linearly
within a span, `vertex` by the curve's own basis. A primvar no bound material
reads SHALL NOT be decoded.

#### Scenario: A vertex colour ramp follows the curve's basis

- **WHEN** a cubic `bspline` strand authors a `vertex` `color3f` primvar and
  its material reads it through `geompropvalue`
- **THEN** at every hit the colour equals the B-spline of the authored control
  colours evaluated at that hit's curve parameter, inside a span as well as at
  its ends

#### Scenario: A uniform primvar colours each strand

- **WHEN** a prim of three curves authors a `uniform` `color3f` primvar
  `[(1,0,0), (0,1,0), (0,0,1)]` read by its material
- **THEN** the strands render red, green and blue

#### Scenario: Varying interpolates linearly within a span

- **WHEN** a cubic strand authors a `varying` `float` primvar
- **THEN** the value at a hit is the linear interpolation, in the span's own
  parameter, of the values at that span's two ends

#### Scenario: An unsupported primvar type is refused

- **WHEN** a curve material reads a curve primvar of type `string` or
  `matrix4d`
- **THEN** the import raises `curves.unsupported_primvar` naming the prim and
  the primvar, and the material reads the `geompropvalue`'s `default`

#### Scenario: An interpolation the curves do not support is refused

- **WHEN** a curve material reads a `faceVarying` curve primvar
- **THEN** the import raises `curves.unsupported_primvar` naming it, and the
  material reads the `geompropvalue`'s `default`

### Requirement: A curve's st feeds its texture chart

`primvars:st` (or the chart a bound material names) on a `BasisCurves` or
`NurbsCurves` prim SHALL be read when a bound material reads the default chart,
interpolated as any curve primvar, so `texcoord` and images read it on a curve
as they do on a mesh. A curve without the chart SHALL read (0, 0), as a mesh
without one does.

#### Scenario: An image along the strand

- **WHEN** a curve authors a `vertex` `primvars:st` and its material's `image`
  reads the default chart
- **THEN** the image is sampled at the interpolated `st` of each hit

#### Scenario: No st, no chart

- **WHEN** the same material is bound to a curve that authors no `st`
- **THEN** every hit reads the image at (0, 0) and the import raises no
  warning

### Requirement: The curve: primvar namespace is reserved

The `geomprop` names `curve:v`, `curve:index` and `curve:random` SHALL name the
hit's curve properties. An authored curve primvar under `primvars:curve:` SHALL
NOT be read; the import SHALL raise `curves.reserved_primvar` naming the prim
and the primvar once, and a material SHALL read the property.

#### Scenario: An authored primvars:curve:v is refused

- **WHEN** a `BasisCurves` prim authors `primvars:curve:v` and its material
  reads `geompropvalue` `curve:v`
- **THEN** the material reads the strand's own `v`, not the authored values,
  and `curves.reserved_primvar` names the prim and `primvars:curve:v`

### Requirement: Curve attributes cost nothing when no material reads them

A scene in which no material bound to a curve reads a curve property, a curve
primvar or the curve's chart SHALL build no per-span strand table and SHALL
render bit-identically to its render before this change. Meshes SHALL render
bit-identically whatever their materials read.

#### Scenario: The curves sample is unchanged

- **WHEN** `samples/curves.usda` is rendered at 16 spp
- **THEN** the image is bit-identical to the one rendered before this change

#### Scenario: Unread primvars are not decoded

- **WHEN** a groom prim authors ten primvars and its material reads one
- **THEN** only that one primvar is decoded, and the `--stats` curve-table
  memory counts only its values
