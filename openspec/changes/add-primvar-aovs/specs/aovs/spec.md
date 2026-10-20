## ADDED Requirements

### Requirement: Primvar sources

A RenderVar with `sourceType = "primvar"` SHALL hold the named primvar's value
at the camera ray's first hit. `sourceName` names the primvar, with or without
the `primvars:` prefix. The value SHALL be interpolated as the primvar is
authored (`constant`, `uniform`, `vertex`, `varying` or `faceVarying`, indexed
or not) and evaluated at the render's time code. Where the hit prim does not
author the primvar, or the ray escapes or scatters in a volume, the channel
SHALL hold the var's clear value.

#### Scenario: Rest position

- **WHEN** a RenderVar authors `sourceName = "rest"`, `sourceType = "primvar"`
  and `dataType = "point3f"` on a scene whose mesh authors a `vertex`
  `point3f[] primvars:rest` equal to its points at frame 1, and the mesh is
  translated at frame 5
- **THEN** the channel at frame 5 holds each hit's frame-1 position, which
  differs from the `P` channel by the translation

#### Scenario: Prefix is optional

- **WHEN** two vars author `sourceName = "rest"` and
  `sourceName = "primvars:rest"`
- **THEN** both channels are bit-identical

#### Scenario: Clear value where not authored

- **WHEN** a primvar var names `rest`, and the camera sees a mesh that
  authors it, a mesh that does not, and the background
- **THEN** the second mesh and the background hold the clear value (0, or the
  authored `driver:parameters:aov:clearValue`)

#### Scenario: Face-varying UV set

- **WHEN** a var authors `sourceName = "uv2"`, `sourceType = "primvar"` and
  `dataType = "texCoord2f"`, on a mesh whose `faceVarying` `primvars:uv2` is
  indexed by `primvars:uv2:indices`
- **THEN** the channel holds the second UV set, with a seam where the chart
  has one

#### Scenario: Per-face colour

- **WHEN** a var asks for `displayColor` as `color3f` on a mesh whose
  `primvars:displayColor` is `uniform`
- **THEN** each face's pixels hold that face's colour

### Requirement: Primvar types must fit the var

A primvar var's channel count SHALL come from its `dataType`: 1, 2 or 3
components; a four-component `dataType` SHALL refuse the var (not yet
supported). A primvar whose component count differs from the var's, or with
no numeric value per point (`string`, `token`, matrices), SHALL be refused on
that prim with one warning per var and prim; that prim reads the clear value.
An `int` primvar SHALL fit an `int` or `float` var.

#### Scenario: Mismatched component count

- **WHEN** a var asks for `rest` as `float2` and the mesh authors
  `point3f[] primvars:rest`
- **THEN** one warning names the var, the prim and both types, and the mesh
  reads the clear value

#### Scenario: Four components refused

- **WHEN** a primvar var authors `dataType = "color4f"`
- **THEN** a warning names the var and says four-component primvar vars are
  not supported yet, and the product has no channel for it

#### Scenario: Integer IDs

- **WHEN** a var asks for `objectId` as `int` and a mesh authors a `constant`
  `int primvars:objectId = 7`
- **THEN** that mesh's pixels hold 7

### Requirement: Primvars follow the refined surface

A requested primvar SHALL be refined with its mesh, at every subdivision
level, uniform or adaptive. A `vertex` or `varying` primvar SHALL be refined
like the points, and a `faceVarying` one under the mesh's
`faceVaryingLinearInterpolation`, as the chart is. Displacement SHALL NOT
change a primvar's value: a displaced point reads the primvar of the
undisplaced point it came from.

#### Scenario: Rest position on a subdivided mesh

- **WHEN** a `catmullClark` mesh whose `primvars:rest` equals its points is
  rendered at subdivision level 3 with a `rest` var and a `P` var
- **THEN** the two channels agree to within float rounding on every hit
  pixel

#### Scenario: Displacement leaves the rest position

- **WHEN** the same mesh is bound to a displacing material
- **THEN** the `rest` channel holds the undisplaced surface's rest position,
  and differs from `P` by the displacement

### Requirement: Primvar sources read the hit prim only

*Not yet supported: inherited and per-instance primvars.*

A primvar var SHALL read only the primvar authored on the geometry prim that
was hit: a mesh, or a `BasisCurves` / `NurbsCurves` prim. A constant primvar
authored on an ancestor prim, a per-instance primvar of a `PointInstancer`,
and a primvar authored on an instance prim SHALL NOT be inherited. Such a
primvar SHALL be reported with one warning per var, and the prims below it
SHALL read the clear value.

#### Scenario: Ancestor primvar is reported, not inherited

- **WHEN** a var asks for `objectId`, and an `Xform` authors
  `constant int primvars:objectId = 3` above a mesh that authors none
- **THEN** a warning says the primvar is authored on an ancestor and is not
  inherited, and the mesh reads the clear value

#### Scenario: Curves primvar

- **WHEN** a var asks for `displayColor` on a `BasisCurves` prim authoring a
  `uniform` `primvars:displayColor`
- **THEN** each strand's pixels hold its curve's colour

### Requirement: Primvar sources cost nothing unless asked for

A render whose RenderVars name no primvar SHALL read, store and evaluate no
primvar beyond what its materials read. Its memory and instruction count
SHALL match the same render without this capability.

#### Scenario: No primvar var

- **WHEN** a scene with `primvars:rest` on every mesh is rendered with a
  product requesting only `depth` and `normal`
- **THEN** `--stats` reports 0 bytes of primvar tables, and the beauty is
  bit-identical to the same render with no `primvars:rest` authored

#### Scenario: Primvar tables are reported

- **WHEN** the same scene is rendered with a `rest` primvar var and `--stats`
- **THEN** the stats report the primvar tables' bytes

## MODIFIED Requirements

### Requirement: Accumulation modes

Each AOV SHALL be accumulated in one of two modes:

- **filtered:** weighted by the same pixel-filter weights, and normalised by
  the same weight sum, as the beauty;
- **closest:** the value of the sample nearest the camera among the pixel's
  samples that fall inside the pixel's own box.

The default mode SHALL come from the source:

- colour, LPE, alpha, normal, albedo and UV are filtered;
- depth, distance, position, motion vectors and IDs are closest;
- a primvar source's default comes from its `dataType`. `point*`,
  `vector*`, `normal*` and integer types are closest, because they are
  positions, directions or identifiers that must never be blended across an
  edge. `color*`, `float*`, `half*` and `texCoord*` types are filtered.

The default SHALL be overridden by `driver:parameters:aov:multiSampled`
(true → filtered, false → closest), and then by Arnold `arnold:filter`, Karma
`driver:parameters:aov:filter` or RenderMan `zmin` rules when one of these is
authored. An authored `driver:parameters:aov:clearValue` SHALL replace the
default clear value.

#### Scenario: Depth is never blended across an edge

- **WHEN** a pixel straddles a foreground object at depth 2 and a background
  at depth 10
- **THEN** the `depth` channel holds 2 or 10, never a value in between

#### Scenario: Forcing filtered depth

- **WHEN** a depth var authors `driver:parameters:aov:multiSampled = true`
- **THEN** that pixel's depth is the filter-weighted average of its samples

#### Scenario: Motion vectors are never blended across an edge

- **WHEN** a pixel straddles a moving foreground object and a static
  background, and its `motionvector` var authors no accumulation override
- **THEN** the pixel holds either the foreground's vector or `(0, 0)`, never
  a value in between

#### Scenario: A rest position is never blended across an edge

- **WHEN** a pixel straddles two meshes whose `primvars:rest` differ, and a
  `point3f` primvar var for `rest` authors no accumulation override
- **THEN** the pixel holds one mesh's rest position, never a value in between

#### Scenario: A colour primvar is filtered

- **WHEN** the same pixel is read through a `color3f` var for `displayColor`
- **THEN** the pixel holds the filter-weighted blend of the two colours
