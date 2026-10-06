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

### Requirement: BasisCurves import as round curves shaded along the strand

A `UsdGeomBasisCurves` prim SHALL be imported as round curves of its authored
widths: `linear` as tapered segments, and `cubic` (`bezier`, `bspline` or
`catmullRom`) as cubic spans. A hit on a curve SHALL shade with the curve's
direction as its tangent, at every placement: top-level, instanced, placed by a
`PointInstancer`, and motion-blurred. Authored `normals` (ribbons) and `wrap`
are not yet read.

#### Scenario: A strand's tangent follows the curve

- **WHEN** a bent cubic `BasisCurves` strand is hit, and its material reads the
  tangent (a `chiang_hair_bsdf` with `curve_direction` unconnected)
- **THEN** the shading tangent at each hit is the strand's direction at that
  point, so the highlight runs across the strand, perpendicular to it, all the
  way along the bend

#### Scenario: Instanced fur keeps its direction

- **WHEN** a `PointInstancer` places a prototype clump of hair curves with
  differing rotations
- **THEN** each placement's highlight is oriented by that placement's own strand
  directions

#### Scenario: Curves without a hair material are unchanged

- **WHEN** `samples/curves.usda` is rendered at 16 spp
- **THEN** the image is bit-identical to the one rendered before this change

#### Scenario: Ribbon normals are not yet read

- **WHEN** a `BasisCurves` prim authors `normals`
- **THEN** it still renders as round tubes

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
colour temperature, `ShapingAPI`, camera visibility (`crust:light:cameraVisible`,
`primvars:ri:attributes:visibility:camera`) and `collection:lightLink` /
`collection:shadowLink` honoured as that capability states. `PortalLight`, mesh
lights and light filters SHALL NOT be read.

The importer SHALL read the render setting `domeLightCameraVisibility` (and
`crust:domeLightCameraVisibility`) off the stage's `RenderSettings` prim, as the
`lighting` capability's "Infinite lights" requirement states.

A light whose `collection:lightLink` has no member among the receivers the import
traversed (see "Light collection membership") illuminates nothing, as the
`lighting` capability's "Lights linked to nothing" requirement states. The test
SHALL be made after the whole stage is traversed, so it does not depend on the
order in which lights and receivers are traversed or on how the import is
streamed.

#### Scenario: Area light

- **WHEN** a `UsdLuxRectLight`, `SphereLight`, `DiskLight` or `CylinderLight`
  prim is traversed
- **THEN** it becomes an entry in the light list and emitting geometry in the
  world

#### Scenario: Infinite light

- **WHEN** a `UsdLuxDistantLight` or `UsdLuxDomeLight` prim is traversed
- **THEN** it becomes a light-list entry with no scene geometry

#### Scenario: The Moana backdrop

- **WHEN** `island.usda` is imported, where `sky_dome_cam_llc` authors
  `collection:lightLink:excludes = </island>` and every geometry lies under
  `/island`
- **THEN** `sky_dome_cam_llc` illuminates nothing, and `sky_dome_env_llc`, whose
  excludes name only the other light, illuminates everything

#### Scenario: A light traversed before what it excludes

- **WHEN** a streamed stage's backdrop dome is traversed in an earlier chunk than
  the geometry its `excludes` cover
- **THEN** it still illuminates nothing, as it does with streaming disabled

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

### Requirement: Adaptive subdivision level

When a target edge length is given (`float crust:subdivisionEdgeLength` on the
`RenderSettings` prim, in pixels, or the host override `--subdiv-edge-length`, which
takes precedence), the importer SHALL refine **unshared** subdivision geometry by its
size on screen, and **shared** geometry to one uniform level:

- Unshared geometry is a direct mesh prim, and a prototype placed exactly once — a
  native instance whose prototype has no other placement in the same top-level
  subtree of the stage, or the one placement of a `PointInstancer` prototype. Its
  meshes SHALL be tessellated per face (see "Per-face adaptive tessellation"), or,
  where that does not apply, refined to the smallest level `L` at which the mesh's
  mean cage-edge length, scaled by its world transform and by `2^-L`, projects to no
  more than the target at the nearest point of its world bounds to the render camera
  at shutter open, at the render resolution.
- Shared geometry — a prototype placed more than once — SHALL be refined to the
  resolved `crust:subdivisionLevel` / `--subdiv-level` when either is given, and to
  level 0 otherwise, whatever its distance.

Levels from the screen SHALL be clamped to `0..=max`:
- `max` is the resolved `crust:subdivisionLevel` / `--subdiv-level` when either is
  given, and 3 otherwise;
- a camera inside the bounds SHALL give `max`.

Placements SHALL be counted per top-level subtree of the stage in every import mode,
so whether geometry is shared, and every level, depends only on the stage, the
camera, the resolution and the settings, never on import order or on streaming.

Without a target edge length, the uniform level SHALL apply unchanged. With a target
but no camera resolvable before traversal (from `--camera` or `RenderSettings.camera`),
the importer SHALL warn once and apply the uniform level.

A target edge length that is not a positive finite number SHALL be ignored with a
warning.

#### Scenario: Near and far copies of one cage

- **WHEN** two prims reference the same `catmullClark` cage, one filling the frame and
  one far enough that its edges project below the target at level 0, and
  `crust:subdivisionEdgeLength = 2` is authored
- **THEN** the near one is refined, and the far one renders at its cage resolution

#### Scenario: A shared prototype gets the uniform level

- **WHEN** a `PointInstancer` places one subdivision prototype both near the camera
  and far from it, in adaptive mode, with no level setting
- **THEN** every placement renders the same level-0 version, and one copy of it is
  kept

#### Scenario: A prototype placed once is adaptive

- **WHEN** an `instanceable` prim is the only placement of its prototype in its
  top-level subtree, in adaptive mode
- **THEN** its meshes are refined by their size on screen, as a direct mesh at the same
  transform would be

#### Scenario: The level setting caps adaptive refinement and sets the shared level

- **WHEN** `--subdiv-level 2` is given in adaptive mode
- **THEN** no unshared mesh is refined past level 2 (a rate of 4 per edge), and every
  shared prototype is refined to level 2

#### Scenario: No camera known before traversal

- **WHEN** a target edge length is given but neither `--camera` nor
  `RenderSettings.camera` names a camera
- **THEN** a single warning is logged and every subdivision mesh is refined to the
  uniform level

#### Scenario: Deterministic across import modes

- **WHEN** the same adaptive render is imported streamed and with
  `CRUST_STREAM_IMPORT=0`
- **THEN** every mesh placement is refined the same way in both imports

### Requirement: Per-face adaptive tessellation

In adaptive subdivision (a target edge length is set and the render camera is known
before traversal), the importer SHALL tessellate each **unshared** subdivision mesh
(see "Adaptive subdivision level") per cage face instead of refining the whole mesh to
one level:

- Each cage edge SHALL be split into a number of segments chosen from that edge alone:
  the smallest count at which each segment, stretched by the placement and seen at the
  edge's nearest point to the render camera, projects to at most the target, between 1
  and `2^max`, where `max` is the adaptive ceiling. A mesh of a prototype placed once
  SHALL be rated through that placement's transform, edge by edge, as a direct mesh.
- Two faces sharing a cage edge SHALL split it at the same points, and the tessellated
  surface SHALL have no crack or T-junction between them, whatever the rates of their
  other edges.
- A face with an edge split more than once SHALL be tessellated on the mesh's limit
  surface, every vertex carrying the limit surface's normal there. A face whose every
  edge is split once SHALL render its cage with smooth normals, as level 0 does, its
  corners shared with a refined neighbour taking that neighbour's limit points, so the
  two meet without a crack.
- A cage edge wholly outside the render camera's view SHALL be split once (unless
  `CRUST_ADAPTIVE_FRUSTUM=0`); a mesh on the per-mesh path wholly outside the view
  SHALL take level 0.
- A `loop` mesh SHALL instead be refined to the per-mesh adaptive level, and SHALL be
  counted in `--stats`. A face-varying texture chart SHALL be evaluated on the
  tessellation, each side of a seam keeping its own values.
- A per-face mesh read through Ptex SHALL address its cage faces, as a uniformly
  refined mesh does.
- The edge rates SHALL depend only on the placement, the camera, the resolution and the
  settings, never on import order.

`CRUST_ADAPTIVE_PER_FACE=0` SHALL restore the per-mesh adaptive level. Without a target
edge length nothing here applies, and the output SHALL be unchanged.

#### Scenario: A large mesh near and far

- **WHEN** a single subdivision mesh spans from next to the camera to far beyond the
  distance where its edges project below the target, in adaptive mode
- **THEN** its faces near the camera are split finely and its far faces are not split
  at all, so it holds fewer triangles than the same mesh refined to the per-mesh level

#### Scenario: Watertight between faces of different rates

- **WHEN** two neighbouring cage faces get different edge rates on their other edges
- **THEN** every edge of the tessellation inside the mesh is shared by exactly two
  triangles, and rays aimed at the shared cage edge never pass between them

#### Scenario: A uniform rate is uniform refinement

- **WHEN** every edge of a mesh gets the rate `2^L`
- **THEN** on faces with no extraordinary vertex the tessellated vertices are the limit
  points of uniform level-`L` refinement to within floating-point rounding, and near an
  extraordinary vertex to within the approximation of the patches that cover it

#### Scenario: A face-varying chart is tessellated, a Loop cage falls back

- **WHEN** a subdivision mesh with a face-varying `primvars:st` and a `loop` mesh are
  read in adaptive mode
- **THEN** the first is tessellated per face, each triangle corner taking the chart's
  value on its own side of any seam, and the second is refined to its per-mesh level
  and counted as one fallback

#### Scenario: Out of view

- **WHEN** the same subdivision cube is placed in front of the camera and behind it
- **THEN** the one in front is split at its rate and the one behind once per edge, and
  both are still hit by rays

#### Scenario: The switch restores per-mesh levels

- **WHEN** an adaptive render is imported with `CRUST_ADAPTIVE_PER_FACE=0`
- **THEN** every mesh is refined to its per-mesh adaptive level, as before this change

#### Scenario: Ptex on a per-face mesh

- **WHEN** a Ptex-textured subdivision mesh is tessellated per face
- **THEN** each hit resolves to the cage face and the position within it that the limit
  surface parameterization gives, so the texture is continuous across the
  tessellation

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

### Requirement: Imported opacity inputs are cutouts

`crust:openpbr`'s `geometryOpacity` and `PxrDisneyBsdf`'s `alpha` SHALL be
the material's opacity, a cutout the integrator honours (see the rendering
capability). A `UsdPreviewSurface` whose `opacityThreshold` is above 0 SHALL be
a cutout mask: a point is present where `opacity ≥ opacityThreshold` and absent
otherwise, from a constant or a texture-driven `opacity` alike, and SHALL NOT
refract. Under an `opacityThreshold` of 0 (the default) `opacity` below 1
SHALL remain translucency (transmission at `ior`), not a cutout.

#### Scenario: A constant preview cutout

- **WHEN** a `UsdPreviewSurface` authors `opacity = 0` and `opacityThreshold = 0.5`
- **THEN** its material is a cutout of opacity 0, and its BSDF transmits nothing

#### Scenario: A textured preview mask

- **WHEN** a `UsdPreviewSurface` under `opacityThreshold = 0.5` reads `opacity`
  from a texture whose value at a hit is 0.2, and at another 0.8
- **THEN** the first hit has opacity 0 and the second opacity 1

#### Scenario: Translucency is no cutout

- **WHEN** a `UsdPreviewSurface` authors `opacity = 0` with no threshold
- **THEN** its material has no cutout and refracts at `ior`

### Requirement: Light collection membership

For every UsdLux light it reads, the importer SHALL resolve `collection:lightLink`
and `collection:shadowLink` with `UsdCollectionAPI` semantics. A geometry prim is a
member when the nearest of its own path and its ancestors named in `includes` or
`excludes` is an include, or, when none is named, when `includeRoot` is true (the
UsdLux fallback). `expansionRule = explicitOnly` SHALL match only the named paths.
`expandPrims` (the default) and `expandPrimsAndProperties` SHALL match the named
paths' descendants. Included collections SHALL be resolved recursively, and a
cycle SHALL be refused with a warning. Membership SHALL be judged on the prim that
owns the emitted geometry. For native instances and PointInstancer instances, that
is the instance prim, and targets inside a prototype SHALL warn once per collection.

#### Scenario: The nearest path decides

- **WHEN** a collection includes `/World` and excludes `/World/Set`, and
  `/World/Set/Chair` is traversed
- **THEN** `/World/Set/Chair` is not a member

#### Scenario: Default collection

- **WHEN** a light authors no `collection:lightLink` properties
- **THEN** every geometry is a member and no link data is built

#### Scenario: Explicit only

- **WHEN** a collection sets `expansionRule = "explicitOnly"` and includes
  `/World/Hero`
- **THEN** `/World/Hero/Body` is not a member

### Requirement: Working colour space

The importer SHALL render in the scene-linear colour space named by the
`RenderSettings` prim's `renderingColorSpace`, resolved through the OCIO config,
unless the host names one, which SHALL win; with neither, `lin_rec709`. A
stage-authored space that is unknown or not scene-linear SHALL be refused with
a warning and `lin_rec709` used; a host-named one SHALL be an error. An
authored colour SHALL be converted into the working space from the
colour space `UsdColorSpaceAPI` resolves for it — its `colorSpace` metadatum,
else the `colorSpace:name` of its prim or nearest authoring ancestor — and
taken as already in it when none is authored.

#### Scenario: An ACEScg stage

- **WHEN** `renderingColorSpace = "acescg"` and a light's `inputs:color`
  carries `colorSpace = "lin_rec709"`
- **THEN** the scene's working space is ACEScg and the light's colour is the
  Rec.709 value converted to AP1, while a light colour with no metadata is
  used as authored

#### Scenario: An inherited colour space

- **WHEN** a scope authors `colorSpace:name = "lin_rec709_scene"` and a light
  two prims below it authors an untagged `inputs:color`, in an ACEScg render
- **THEN** the light's colour is converted from Rec.709 to AP1

### Requirement: Heuristics weigh colours by the working space's luminance

Every sampling heuristic that reduces a colour to one weight — light power,
lobe selection, environment importance, guiding training, adaptive sampling —
and the `variance` AOV SHALL use the luminance weights of the working space,
the `Y` row of its RGB → XYZ matrix. In linear Rec.709 they SHALL be
(0.2126, 0.7152, 0.0722), and the render bit-identical to before.

#### Scenario: An ACEScg stage

- **WHEN** a stage renders in ACEScg
- **THEN** its lights and materials weigh colours by ACEScg's luminance
