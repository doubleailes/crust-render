# Spec Delta

## MODIFIED Requirements

### Requirement: Subdivision surfaces follow the scheme

The importer SHALL treat a `UsdGeomMesh` as a subdivision surface unless its
`subdivisionScheme` is `none`. An unauthored scheme SHALL be USD's schema
fallback, `catmullClark`, which is how production assets (ALab, Kitchen_set)
mark their subdivision meshes. A mesh authoring `none` SHALL render as its
faceted polygon cage, unless its bound material displaces it (see the
`displacement` capability): such a mesh SHALL be refined with the `bilinear`
scheme, so its faces keep their flat shape until displaced. A `loop` mesh with
any non-triangle face SHALL warn and render as its cage.

A refined mesh SHALL be uniformly subdivided to the stage's refinement level
(see "Render settings from USD with defaults" and the CLI's `--subdiv-level`),
snapped to the limit surface, and shaded with smooth per-vertex normals. At
level 0 a subdivision surface SHALL render as its cage, shaded with smooth
per-vertex normals of the cage. The authored `interpolateBoundary`,
`creaseIndices` / `creaseLengths` / `creaseSharpnesses` and `cornerIndices` /
`cornerSharpnesses` SHALL be honoured. `holeIndices` is not supported.

The mesh's texture-coordinate primvar (`st`, or a fallback name) SHALL be
refined with the surface:

- a `faceVarying` primvar SHALL be interpolated under the mesh's
  `faceVaryingLinearInterpolation` (`none`, `cornersOnly`, `cornersPlus1`,
  `cornersPlus2`, `boundaries` or `all`; `cornersPlus1` when unauthored);
- a `vertex` or `varying` primvar SHALL be refined like the points.

A UV-textured material bound to a subdivided mesh SHALL therefore sample its
textures. Ptex lookups SHALL keep addressing the authored cage's face ids; a
`loop` mesh whose material reads Ptex SHALL therefore render as its smooth cage
with a warning rather than refine.

#### Scenario: Loop mesh with a Ptex texture

- **WHEN** a `loop` mesh whose material reads a Ptex texture is loaded at level 1
- **THEN** it renders its authored triangles, each hit resolving to its authored
  face id, and a warning is emitted

#### Scenario: A smooth cage and a faceted cage with identical arrays

- **WHEN** two meshes share points, topology and material, one authoring
  `subdivisionScheme = "none"` and one leaving it unauthored, at level 0
- **THEN** the first shades faceted and the second smooth

The per-prim attribute `crust:subdivisionLevel` SHALL have no effect. A stage
whose meshes author it SHALL emit a single warning naming the render-settings
attribute and the CLI flag that replace it.

`CRUST_SUBDIV=0` SHALL render every mesh as its faceted cage, whatever the
scheme or level.

#### Scenario: Nothing is refined by default

- **WHEN** a mesh authors `subdivisionScheme = "catmullClark"` and neither the
  stage nor the host sets a refinement level
- **THEN** the mesh renders its cage's triangles, shaded with smooth normals

#### Scenario: A requested level refines

- **WHEN** a mesh authors `subdivisionScheme = "catmullClark"` and the stage's
  `crust:subdivisionLevel` (or `--subdiv-level`) is 1
- **THEN** the mesh renders as its level-1 limit surface with smooth normals

#### Scenario: Unauthored scheme

- **WHEN** a mesh does not author `subdivisionScheme`
- **THEN** it is a Catmull-Clark surface, refined to the stage's level

#### Scenario: Level 0

- **WHEN** the refinement level is 0 and a mesh does not author
  `subdivisionScheme = "none"`
- **THEN** it renders its cage's triangles, shaded with smooth normals

#### Scenario: Scheme none

- **WHEN** a mesh authors `subdivisionScheme = "none"`
- **THEN** it renders as its faceted polygon cage

#### Scenario: Scheme none with displacement

- **WHEN** a mesh authors `subdivisionScheme = "none"`, its material displaces it, and
  the refinement level is 2
- **THEN** it is refined bilinearly to level 2 and its level-2 vertices are displaced;
  with `CRUST_DISPLACE=0` it renders as its faceted polygon cage

#### Scenario: Textured subdivided mesh keeps its UVs

- **WHEN** a subdivided mesh with a `faceVarying` `primvars:st` is bound to a
  UV-textured material
- **THEN** the refined surface samples the texture through the refined chart,
  and no "texture coordinates are not refined" warning is emitted

#### Scenario: Face-varying linear interpolation all

- **WHEN** a subdivided mesh authors `faceVaryingLinearInterpolation = "all"`
- **THEN** its refined UVs are the bilinear interpolation of the cage UVs
  within each face

#### Scenario: Legacy per-prim level

- **WHEN** a mesh authors `crust:subdivisionLevel = 3` and the stage leaves the
  level at its default
- **THEN** the mesh is not refined (level 0, not 3), and the stage emits one
  warning pointing at the render-settings level and `--subdiv-level`

#### Scenario: Kill switch

- **WHEN** `CRUST_SUBDIV=0` is set
- **THEN** every mesh renders as its faceted cage regardless of scheme or level
