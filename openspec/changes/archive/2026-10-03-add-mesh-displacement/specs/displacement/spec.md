# Spec Delta

## Purpose

Move the vertices of tessellated meshes along their normals by a scalar the bound
material defines, so that displaced assets render with their authored shape (silhouette,
shadows, occlusion) rather than as their base surface.

## ADDED Requirements

### Requirement: Displacement sources

The importer SHALL read a scalar displacement from a mesh's bound material when the
material is any of:
- a `UsdPreviewSurface` whose `inputs:displacement` is authored, as a value or through a
  `UsdUVTexture`, honouring the texture's channel, `scale` and `bias`;
- a MaterialX material whose `displacementshader` is a `displacement` node with a
  `float` `displacement` input, multiplied by `scale`;
- a material whose `outputs:ri:displacement` reaches a `PxrDisplace`, as
  `dispScalar · dispAmount`.

A material with none of these SHALL NOT displace.

#### Scenario: A constant preview displacement

- **WHEN** a mesh is bound to a `UsdPreviewSurface` authoring `inputs:displacement = 0.1`
- **THEN** every vertex of its tessellation is moved 0.1 local units along its normal

#### Scenario: A textured preview displacement

- **WHEN** `inputs:displacement` connects to a `UsdUVTexture` reading `outputs:r` with
  `scale = 0.2` and `bias = -0.1`
- **THEN** each vertex is moved by `0.2 · r − 0.1`, where `r` is the texture's red
  channel at the vertex's texture coordinate

#### Scenario: A MaterialX displacement

- **WHEN** a `.mtlx` material connects `displacementshader` to a `displacement` node whose
  `float` input is an `image` and whose `scale` is 0.05
- **THEN** each vertex is moved by 0.05 times the image value at its texture coordinate,
  and this matches the preview-surface render of the same map and scale

#### Scenario: A RenderMan displacement read through Ptex

- **WHEN** a material's `outputs:ri:displacement` reaches a `PxrDisplace` whose
  `dispScalar` reads a Ptex file and whose `dispAmount` is 2
- **THEN** each vertex is moved by 2 times the Ptex value at its cage face and its
  position within that face

#### Scenario: No displacement authored

- **WHEN** a mesh's material authors none of these inputs
- **THEN** the mesh's geometry is identical to what it was before displacement existed

### Requirement: Vector displacement is refused

A displacement whose value is a vector (a MaterialX `vector3` `displacement`, or
`PxrDisplace.dispVector` / `modelDispVector`) SHALL be ignored, with one warning per
material naming it.

#### Scenario: A vector displacement map

- **WHEN** a MaterialX material's `displacement` node takes a `vector3` input
- **THEN** the mesh renders undisplaced and a single warning names the material

### Requirement: Displacement is applied along the normal in local space

Each unique vertex of the tessellated mesh SHALL be moved once, along the unit smooth
normal the tessellation had before displacement, by the displacement value times one
local unit. The offset SHALL be applied in the mesh's local space, before its placement
transform, so a scaled placement scales the offset with the mesh. Texture coordinates and
Ptex face addressing SHALL be unchanged by displacement.

#### Scenario: A scaled placement

- **WHEN** a mesh with a constant displacement of 0.1 is placed with a uniform scale of 2
- **THEN** its rendered surface lies 0.2 world units outside the undisplaced surface

#### Scenario: Textures still follow the surface

- **WHEN** a displaced mesh's material also reads a colour texture
- **THEN** each point of the displaced surface shows the texel it showed before
  displacement

### Requirement: Displaced meshes are watertight

A displaced mesh SHALL have no crack wherever its undisplaced tessellation had none,
including along texture-coordinate seams, along Ptex face boundaries and between faces
tessellated at different rates. A vertex shared by several faces SHALL take its
displacement from a single deterministic sample.

#### Scenario: A UV seam

- **WHEN** a cube with a face-varying UV chart whose faces meet at seams is displaced
  through a UV texture whose values differ across those seams
- **THEN** every interior edge of the displaced tessellation is shared by exactly two
  triangles, and rays aimed at the seams never pass through

#### Scenario: A Ptex face boundary

- **WHEN** a mesh is displaced through a Ptex file whose neighbouring faces hold different
  values along their shared edge
- **THEN** the displaced surface is closed along that edge

### Requirement: Displacement is filtered at the dicing rate

Each displacement lookup SHALL use a footprint equal to the spacing of the tessellation
around the vertex, in the texture's own coordinates. A coarse tessellation SHALL read a
correspondingly coarse level of the map rather than point-sampling its finest level.

#### Scenario: A cage-resolution mesh

- **WHEN** a high-frequency height map displaces a mesh that is not refined
- **THEN** each cage vertex takes the map's average over roughly its neighbourhood, not
  the value of the single finest texel under it

### Requirement: Shading normals follow the displaced surface

After displacement, a mesh's per-vertex shading normals SHALL be recomputed from the
displaced triangles. A normal or bump map in the bound material SHALL still perturb those
normals at the hit.

#### Scenario: A displaced plane

- **WHEN** a flat plane is displaced into a ridge
- **THEN** the ridge's flanks shade with normals tilted toward their slope, not with the
  plane's original normal

### Requirement: Displacement does not change the dicing rate

Displacement SHALL be applied to the tessellation that the subdivision level, the
adaptive edge-length target and the scheme select. It SHALL NOT add vertices of its own.
A displaced mesh rendered at its cage resolution SHALL be counted in `--stats`. A stage
where at least one displaced mesh is at cage resolution SHALL emit one warning, pointing
at `--subdiv-level` and `--subdiv-edge-length`.

#### Scenario: Default settings

- **WHEN** a displaced mesh is loaded with no refinement level and no edge-length target
- **THEN** its cage vertices are displaced, one warning recommends a refinement setting,
  and `--stats` counts the mesh as displaced at cage resolution

#### Scenario: Refinement raises detail

- **WHEN** the same mesh is loaded at `--subdiv-level 3`
- **THEN** its level-3 vertices are displaced, and its silhouette shows detail the cage
  render does not

### Requirement: Adaptive dicing accounts for the displacement bound

In adaptive mode, the frustum test and the nearest-point distance of a displaced mesh
SHALL use bounds grown by its displacement bound. The bound is:
- the absolute value of a constant displacement;
- otherwise `float crust:displacementBound`, authored on the mesh prim or else on its
  material, in local units.

A displaced mesh with no bound SHALL skip the frustum term. A sampled offset that exceeds
an authored bound SHALL warn once per mesh.

#### Scenario: Displaced into view

- **WHEN** a mesh just outside the camera frustum is displaced into view by an amount
  within its authored `crust:displacementBound`, in adaptive mode
- **THEN** its edges in view are diced at their screen rate, not split once

#### Scenario: No bound known

- **WHEN** a texture-displaced mesh authors no `crust:displacementBound`, in adaptive mode
- **THEN** none of its edges is treated as out of view, and `--stats` counts it

#### Scenario: A bound that is too small

- **WHEN** a mesh authors `crust:displacementBound = 0.01` and its map moves a vertex by
  0.05
- **THEN** the offset is applied unclamped and one warning names the mesh

### Requirement: Shared meshes are displaced once

Mesh prims sharing points, topology and material SHALL share one displaced result,
computed once, whether the mesh is placed directly or as an instanced prototype.
Displacement SHALL NOT change whether a mesh is shared.

#### Scenario: One cage placed twice

- **WHEN** two prims author the same displaced mesh and material
- **THEN** `--stats` reports one displaced mesh, and both placements render the same
  displaced surface

### Requirement: Displacement is deterministic

The displaced positions SHALL depend only on the stage, the camera, the resolution and the
settings, never on import order, on streaming, or on the number of threads.

#### Scenario: Streamed and unstreamed import

- **WHEN** a displaced scene is imported streamed and with `CRUST_STREAM_IMPORT=0`
- **THEN** the two renders are bit-identical

### Requirement: Displacement switch

`CRUST_DISPLACE` (boolean, default on) SHALL be parsed once into `Config`. With
`CRUST_DISPLACE=0` every mesh SHALL be imported exactly as it was before displacement
existed: undisplaced, and a `none` mesh as its faceted cage.

#### Scenario: Switched off

- **WHEN** `samples/displacement.usda` is rendered with `CRUST_DISPLACE=0`
- **THEN** the image is bit-identical to rendering the same stage with its displacement
  inputs removed

### Requirement: Displacement in the stats report

With `--stats`, the scene report SHALL print the number of displaced meshes and vertices,
the time spent displacing, the largest absolute offset applied, the number of displaced
meshes at cage resolution, and the number whose frustum term was skipped. A scene with no
displacement SHALL print none of these lines.

#### Scenario: A displaced scene's report

- **WHEN** `samples/displacement.usda` is rendered with `--stats`
- **THEN** the report shows the displacement lines, with a nonzero mesh count and a
  largest offset equal to the scene's largest authored displacement times its map maximum
