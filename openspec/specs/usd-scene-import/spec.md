# usd-scene-import Specification

## Purpose

Build the runtime `Scene` (camera, world geometry, lights, render settings) from
a USD stage. USD is the only supported scene format. This capability covers stage
loading, Xform-hierarchy baking, geometry and light schema mapping, material
resolution by shader id, and render-settings parsing. Lives in
`crust-core/src/scene/usd_import/` (module map in its `mod.rs`), entry point `Scene::from_usd`.
## Requirements
### Requirement: Load a scene from a USD stage

The importer SHALL open a `.usda`, `.usdc`, or `.usdz` file, import render
settings first (so the camera can derive its aspect ratio), then traverse the
prim hierarchy with an explicit stack that bakes parent Xforms into world-space
transforms.

#### Scenario: Valid USD file is loaded

- **WHEN** `Scene::from_usd` is given a readable USD stage path
- **THEN** it returns a `Scene` with camera, world, lights, and settings populated

#### Scenario: Unreadable path

- **WHEN** the path cannot be opened as a USD stage
- **THEN** loading fails with an I/O error rather than a partial scene

### Requirement: Geometry schema mapping

The importer SHALL map `UsdGeomMesh` to kernel triangle geometry and
`UsdGeomSphere` to an analytic `Sphere`, attaching both to the single
`crust-rt` scene whose BVH4 the kernel builds in `commit()`.

Mesh prims sharing identical points, topology and material binding SHALL be
treated as one *distinct mesh*. The importer SHALL choose that mesh's
representation by how many times it is placed:

- placed **exactly once** → its triangles are transformed into world space and
  attached directly, so they live in the top-level BVH;
- placed **more than once** → one shared local-space kernel scene, attached
  once per placement as an instance carrying that placement's transform.

Instancing geometry that is placed once buys no sharing while costing every
entering ray a transform into local space, a fresh traversal setup and a cold
descent into a second tree, and presents the parent BVH the transformed
bounding box of the inner tree's bounding box — a box of a box that spatial
splits cannot tighten.

Because a placement count is only final once the whole stage has been walked,
the decision SHALL be deferred: the importer reserves the `geom_id` during
traversal and fills the geometry in afterwards. The decision SHALL be made
across all streamed chunks together, never per chunk.

#### Scenario: Mesh prim placed once

- **WHEN** a `UsdGeomMesh` prim is traversed and no other prim shares its
  points, topology and material
- **THEN** its triangles are transformed into world space and attached
  directly, appearing in the top-level BVH

#### Scenario: Mesh geometry placed more than once

- **WHEN** two or more prims share points, topology and material
- **THEN** one local-space kernel scene is built for that geometry and each
  prim attaches an instance of it

#### Scenario: Mirrored placement is baked

- **WHEN** a mesh placed once has a transform with negative determinant
- **THEN** the baked triangle winding is reversed, so the surface orientation
  and hence `front_face` match what the instanced path would report

#### Scenario: Mesh prim that moves

- **WHEN** a mesh prim authors `crust:motion:translate`
- **THEN** it is attached as an instance regardless of placement count, since
  baked triangles carry no transform to interpolate over the shutter

#### Scenario: Non-invertible placement

- **WHEN** a mesh prim's transform is not invertible
- **THEN** its triangles are baked into world space, as an instance requires an
  invertible transform

#### Scenario: Sphere prim

- **WHEN** a `UsdGeomSphere` prim is traversed
- **THEN** it is added as an analytic sphere at its world-space transform

### Requirement: Directly instanced meshes have tangents

A mesh placed more than once through direct, static instances SHALL shade normal
maps with a tangent frame, computed at the hit from the prototype's vertices and the
placement's transform, exactly as a baked mesh does. A prototype part placed through
an instancer's group (a `PointInstancer` or native-instancing prototype), whose hit
id is forwarded rather than its own, and a motion-blurred instance still shade with
the geometric normal; those are the remaining gap.

#### Scenario: A normal map on a directly instanced mesh

- **WHEN** a normal-mapped material is bound to a mesh prim authored twice
- **THEN** each placement shades with a perturbed normal, and a placement whose
  transform is the identity matches the baked render of the same mesh bit for bit

### Requirement: Material resolution by shader id

The importer SHALL resolve a bound material via `MaterialBindingAPI` and dispatch
on the surface shader's `info:id`: `UsdPreviewSurface` maps into `OpenPBR`
(portable field mapping), `crust:openpbr` decodes 1:1 into `OpenPBR`, and any
geometry without a resolvable bound material falls back to a default grey `OpenPBR`.

#### Scenario: UsdPreviewSurface binding

- **WHEN** a surface binds a shader with `info:id = "UsdPreviewSurface"`
- **THEN** its inputs are mapped into an `OpenPBR` material (e.g. `diffuseColor →
  baseColor`, `metallic → baseMetalness`, `roughness → specularRoughness`)

#### Scenario: crust:openpbr binding

- **WHEN** a surface binds a shader with `info:id = "crust:openpbr"`
- **THEN** each camelCase input is decoded 1:1 into the matching `OpenPBR` field

#### Scenario: Unbound geometry

- **WHEN** geometry has no resolvable bound material
- **THEN** it is assigned a default grey `OpenPBR` material

### Requirement: Light schema mapping

The importer SHALL map every `UsdLux` light it reads — `SphereLight`,
`RectLight`, `DiskLight`, `CylinderLight`, `DistantLight` and `DomeLight` — onto
the lights defined by the `lighting` capability, with UsdLux units, `normalize`,
colour temperature and `ShapingAPI` honoured as that capability states.
`PortalLight`, mesh lights, light filters and light linking SHALL NOT be read.

#### Scenario: Area light

- **WHEN** a `UsdLuxRectLight`, `SphereLight`, `DiskLight` or `CylinderLight`
  prim is traversed
- **THEN** it becomes an entry in the light list and emitting geometry in the
  world

#### Scenario: Infinite light

- **WHEN** a `UsdLuxDistantLight` or `UsdLuxDomeLight` prim is traversed
- **THEN** it becomes a light-list entry with no scene geometry

### Requirement: Volume region import

Any prim carrying a `crust:volume:type` attribute SHALL import as a
free-standing `VolumeRegion` rather than geometry — checked before the
mesh/sphere/light dispatch, so its bounds never occlude shadow rays. The
region's box comes from the prim's `size` (when it is a `Cube`) or the unit
cube, oriented and scaled by the composed prim transform, with density
(`homogeneous` | `smoke` procedural fBm noise | `grid` inline voxel data) and
σₛ/σₐ/anisotropy/emission read from `crust:volume:*` attributes.

#### Scenario: Volume prim

- **WHEN** a prim authors `crust:volume:type`
- **THEN** it is added to the scene's volume regions, not to world geometry

#### Scenario: Missing grid data

- **WHEN** a `grid`-type volume's `gridData` length does not match
  `gridDims`' `nx·ny·nz`
- **THEN** a warning is emitted and the volume is skipped

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

### Requirement: Subdivision surfaces follow the scheme

The importer SHALL treat a `UsdGeomMesh` as a subdivision surface unless its
`subdivisionScheme` is `none`. An unauthored scheme SHALL be USD's schema
fallback, `catmullClark`, which is how production assets (ALab, Kitchen_set)
mark their subdivision meshes. A mesh authoring `none` SHALL render as its
faceted polygon cage. A `loop` mesh with any non-triangle face SHALL warn and
render as its cage.

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

### Requirement: Per-corner mesh tables are derived, not stored

The importer SHALL NOT keep a per-triangle copy of anything a hit can recompute from
the kernel's shared vertices and the triangle's corner texture coordinates. Tangents
SHALL be computed at the hit from the kernel's vertices, with the formula the stored
table used, and for an unmirrored placement SHALL match that table bit for bit (a
mirrored baked placement's stored tangent paired swapped vertices with unswapped
corners; the derived one pairs them correctly). Texture-footprint densities stay a
4-byte table per triangle, because they are defined in the mesh's local frame, which
a baked mesh no longer has. A face-varying chart SHALL be kept as its values plus one
index per triangle corner. A subdivided mesh's Ptex sub-faces SHALL be kept as one
4-byte dyadic cell per triangle (origin, depth and the corner at the origin, in the
base cage face, beside the face id the face table already holds) from which the triangle's corner coordinates are reconstructed
exactly. Importer-side positions and normals SHALL be stored as three `f32`s, not
padded to four.

#### Scenario: A refined Ptex mesh keeps its face ids

- **WHEN** `samples/ptex_quads.usda` is rendered at `--subdiv-level 2` before and after
  this change
- **THEN** the images are bit-identical, and the importer's face table costs 4 bytes of
  cell per triangle beside the face id, fan slice and density, rather than 24 bytes of
  corner coordinates

#### Scenario: A textured subdivided mesh's tables

- **WHEN** a UV-textured `catmullClark` mesh is refined to level 2 and rendered
- **THEN** the image is bit-identical to the stored-table render, and the chart's
  per-triangle tables cost 16 bytes (12 of corner indices, 4 of density) beside its
  values, which are stored once per distinct `(u, v)` rather than per corner
