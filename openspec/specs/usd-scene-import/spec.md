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

### Requirement: Transform stacks

The importer SHALL compose a prim's local transform from its `xformOpOrder` with
`UsdGeomXformable` semantics, in double precision, for every op kind
`UsdGeomXformOp` defines, including `!invert!` and `:suffix` instances, on any
`Xformable` prim type. A `!resetXformStack!` SHALL drop the inherited transform and the
ops listed before it. A prim that is not `Xformable` SHALL contribute no transform of
its own. An op whose kind is not a `UsdGeomXformOp` kind SHALL contribute identity, with
a warning.

#### Scenario: Multi-op stack

- **WHEN** a prim authors `xformOpOrder = ["xformOp:translate", "xformOp:scale"]` with
  translate `(1, 2, 3)` and scale `(2, 3, 4)`
- **THEN** its local transform has translation `(1, 2, 3)` and the scale on its diagonal

#### Scenario: Pivot pair

- **WHEN** a prim authors `translate`, `translate:pivot`, `rotateXYZ`, `scale`,
  `!invert!xformOp:translate:pivot`
- **THEN** its local transform equals the one C++ USD computes for the same stack

#### Scenario: Single-axis op on a non-Xform prim

- **WHEN** a `BasisCurves`, `PointInstancer` or UsdLux light prim authors
  `xformOp:translateX = 2`
- **THEN** it is placed 2 units along its parent's X axis, not at its parent's origin

#### Scenario: Reset on any prim type

- **WHEN** a `DiskLight` under a translated `Xform` authors
  `xformOpOrder = ["!resetXformStack!", "xformOp:translate"]`
- **THEN** its world transform is its own translate alone

#### Scenario: Unknown op kind

- **WHEN** a prim lists `xformOp:bogus` in its `xformOpOrder`
- **THEN** that op contributes identity, the rest of the stack still composes, and a
  warning names the prim

#### Scenario: Reset after other ops

- **WHEN** `xformOpOrder = ["xformOp:translate", "!resetXformStack!",
  "xformOp:translate:after"]` with `translate:after = (0, 2, 0)`, under a translated
  parent
- **THEN** the prim's world transform is the translate `(0, 2, 0)` alone

#### Scenario: Ops on a prim that is not Xformable

- **WHEN** an untyped prim or a `Scope` authors `xformOp:*` and an `xformOpOrder`
- **THEN** its world transform is its parent's

### Requirement: Geometry schema mapping

The importer SHALL map `UsdGeomMesh` to kernel triangle geometry,
`UsdGeomSphere` to an analytic `Sphere`, and the implicit shapes `UsdGeomCube`,
`UsdGeomPlane`, `UsdGeomCylinder`, `UsdGeomCylinder_1`, `UsdGeomCone`,
`UsdGeomCapsule` and `UsdGeomCapsule_1` to kernel triangle geometry tessellated at
import (see "Implicit shapes are tessellated as USD tessellates them"), attaching all
of them to the single `crust-rt` scene whose BVH4 the kernel builds in `commit()`.

Mesh prims sharing identical points, topology and material binding SHALL be
treated as one *distinct mesh*. The importer SHALL choose that mesh's
representation by how many times it is placed:

- placed **exactly once** → its triangles are transformed into world space and
  attached directly, so they live in the top-level BVH;
- placed **more than once** → one shared local-space kernel scene, attached
  once per placement as an instance carrying that placement's transform.

A tessellated implicit shape SHALL be a distinct mesh like any other, decided by
the same rule.

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

#### Scenario: Implicit shape prim

- **WHEN** a `Cube`, `Plane`, `Cylinder`, `Cylinder_1`, `Cone`, `Capsule` or
  `Capsule_1` prim is traversed, at the top level or inside a prototype
- **THEN** it renders as triangle geometry with its bound material, ray mask, motion
  and light links, exactly as a `Mesh` prim with those triangles would

#### Scenario: Identical implicit shapes share geometry

- **WHEN** two `Cylinder` prims author the same `radius`, `height`, `axis` and
  material and are tessellated to the same radial count
- **THEN** they are one distinct mesh, placed twice

### Requirement: Directly instanced meshes have tangents

A mesh placed more than once through direct instances SHALL shade normal maps with a
tangent frame, computed at the hit from the prototype's vertices and the placement's
transform, exactly as a baked mesh does. A moving direct instance (a time-sampled
transform or `crust:motion:translate`) SHALL use its transform interpolated at the
ray's shutter time, the one the ray was intersected through. A prototype part placed
through an instancer's group (a `PointInstancer` or native-instancing prototype), whose
hit id is forwarded rather than its own, still shades with the geometric normal; that is
the remaining gap.

#### Scenario: A normal map on a directly instanced mesh

- **WHEN** a normal-mapped material is bound to a mesh prim authored twice
- **THEN** each placement shades with a perturbed normal, and a placement whose
  transform is the identity matches the baked render of the same mesh bit for bit

#### Scenario: A normal map on an animated mesh

- **WHEN** a normal-mapped mesh has a time-sampled rotation and the camera authors a
  non-instantaneous shutter, with or without `disableMotionBlur`
- **THEN** it shades with a perturbed normal whose tangent follows the mesh's rotation at
  each ray's shutter time, as it did before the shutter was read

### Requirement: Native instance nesting

A native instance found inside another native instance's prototype SHALL be imported:
its prototype's geometry SHALL appear in every placement of the outer prototype, under
the composed outer placement, inner instance transform and inner part transforms.
Nesting deeper than the importer's instance-nesting limit SHALL be refused with a
warning.

#### Scenario: Instance inside a prototype

- **WHEN** a prototype `_Outer` holds a sphere at its origin and an `instanceable` prim
  referencing `_Inner` translated by `(3, 0, 0)`, and `_Inner` holds a sphere at its
  origin, and one prim instances `_Outer`
- **THEN** both spheres render: one at the instance's origin and one 3 units along X

#### Scenario: Outer prototype placed several times

- **WHEN** the outer prototype is instanced by two prims at different translations
- **THEN** each placement carries both spheres, and the inner prototype's geometry is
  built once and shared

#### Scenario: Invisible inner instance

- **WHEN** the nested `instanceable` prim is `invisible`
- **THEN** none of its geometry renders, in any placement of the outer prototype

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

### Requirement: NurbsCurves import as round curves

A `UsdGeomNurbsCurves` prim SHALL be imported as round curves of its authored
widths, one Bézier span per non-empty knot interval inside the curve's domain
(narrowed by `ranges` when authored), and shade along the strand as
`BasisCurves` do, at every placement. Orders 2, 3 and 4 SHALL be read; a curve
of any other order, or whose `order` / `knots` do not match its points, SHALL be
skipped with a warning. Unequal `pointWeights` SHALL be approximated by
polynomial spans with a warning.

#### Scenario: A cubic NURBS curve renders

- **WHEN** a `NurbsCurves` prim authors one order-4 curve with clamped knots
- **THEN** it renders as a round tube through the same points as the Bézier
  curve of those control points, and the import raises no warning

#### Scenario: Ranges trim the curve

- **WHEN** the same curve authors `ranges = [(0, 0.5)]`
- **THEN** only the half of the curve over that parameter interval renders

#### Scenario: Unsupported orders are reported

- **WHEN** a `NurbsCurves` prim authors an order-5 curve
- **THEN** that curve renders nothing and `curves.unsupported_order` names the prim

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
mesh/sphere/implicit-shape/light dispatch, so its bounds never occlude shadow
rays. Inside a prototype, the same check SHALL come before any geometry check, so
a volume prim there is skipped with its warning and never imported as a surface.
The region's box comes from the prim's `size` (when it is a `Cube`) or the unit
cube, oriented and scaled by the composed prim transform, with density
(`homogeneous` | `smoke` procedural fBm noise | `grid` inline voxel data) and
σₛ/σₐ/anisotropy/emission read from `crust:volume:*` attributes.

#### Scenario: Volume prim

- **WHEN** a prim authors `crust:volume:type`
- **THEN** it is added to the scene's volume regions, not to world geometry

#### Scenario: Volume Cube is not a surface

- **WHEN** a `Cube` prim authors `crust:volume:type`
- **THEN** it is a volume region only, and no box geometry is attached for it

#### Scenario: Volume Cube inside a prototype

- **WHEN** a `Cube` prim authoring `crust:volume:type` is inside a native-instance
  or `PointInstancer` prototype
- **THEN** a warning says volumes cannot be instanced, and neither a volume region
  nor box geometry is imported for it

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
`crust:maxDepth` counts shaded path vertices, not bounces (1 is direct lighting
only); a value below 1 SHALL be refused with a `settings.invalid_value`
warning naming the attribute, and the default depth used.

#### Scenario: Authored settings

- **WHEN** the stage authors `crust:` render params
- **THEN** those values populate `RenderSettings`

#### Scenario: Missing settings fall back to defaults

- **WHEN** a `crust:` param is absent
- **THEN** the documented default is used in its place

#### Scenario: A max depth below one is refused

- **WHEN** the `RenderSettings` prim authors `int crust:maxDepth = 0` (or a
  negative value)
- **THEN** one `settings.invalid_value` warning names `crust:maxDepth`, and the
  render uses depth 32 instead of rendering black

#### Scenario: Subdivision level from render settings

- **WHEN** the `RenderSettings` prim authors `int crust:subdivisionLevel = 1`
  and no host override is given
- **THEN** every mesh whose scheme is not `none` is refined once

#### Scenario: No products

- **WHEN** the settings prim authors no `products`
- **THEN** no AOV is requested and the render writes the single beauty EXR

### Requirement: Render settings can disable motion blur

`disableMotionBlur = true` SHALL render moving geometry sharp where it is at the
frame (UsdRender's `[0, 0]` shutter at the current time code), in the beauty and
every AOV. `instantaneousShutter = true` SHALL also turn blur off, but sharp at
shutter open, `f + shutter:open`; with both, `disableMotionBlur` wins. Motion
stays in the scene, so motion vectors are still produced. Each value is resolved
like the render's camera: the first product's, else the settings prim's.
Neither SHALL be warned about as not honoured.

#### Scenario: Sharp beauty with motion authored

- **WHEN** a sphere authors `crust:motion:translate = (1, 0, 0)` and the
  render settings author `disableMotionBlur = true`
- **THEN** the sphere renders sharp at its authored position, with no streak
  along X, and no warning about `disableMotionBlur` is logged

#### Scenario: Sharp beauty at the frame for an animated transform

- **WHEN** a sphere's transform is time-sampled, the camera authors
  `shutter:open = -0.25` and `shutter:close = 0.25`, the render is made with `-f 5`, and the
  render settings author `disableMotionBlur = true`
- **THEN** the sphere renders sharp where its transform places it at time code 5, and its
  `motionvector` is the screen displacement its transform makes from time code 4.75 to
  5.25, measured from there

#### Scenario: instantaneousShutter is sharp at shutter open

- **WHEN** the same stage authors `instantaneousShutter = true` and not
  `disableMotionBlur`
- **THEN** the sphere renders sharp where its transform places it at time code 4.75

#### Scenario: instantaneousShutter is a synonym

- **WHEN** the camera's `shutter:open` is 0, or the stage authors no shutter, and the
  render settings author `instantaneousShutter = true` and not `disableMotionBlur`
- **THEN** the render is the same as with `disableMotionBlur = true`

#### Scenario: Toggling the frame pose in a session re-imports

- **WHEN** an MCP session's stage has a time-sampled transform and a shutter opening
  before the frame, and an edit authors `disableMotionBlur = true`
- **THEN** the edit re-imports the stage, and the render equals a fresh import of the same
  layer

#### Scenario: Product overrides the settings

- **WHEN** the settings prim authors `disableMotionBlur = true` and the first
  product authors `disableMotionBlur = false`
- **THEN** moving geometry is motion blurred

#### Scenario: Blur stays on by default

- **WHEN** neither attribute is authored
- **THEN** a prim authoring `crust:motion:translate` is motion blurred, as
  before this change

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
- a `vertex` or `varying` primvar SHALL be refined like the points;
- a `constant` or `uniform` primvar SHALL be read as the faceVarying primvar it
  is equivalent to (every face-vertex indexing value 0, or its face's value),
  so it reads one coordinate per mesh or per face, on the cage and at every
  level alike. An unauthored `interpolation` SHALL be read as `vertex`, except
  on a chart of exactly one value, which is `constant`.

A UV-textured material bound to a subdivided mesh SHALL therefore sample its
textures. Ptex lookups SHALL keep addressing the authored cage's face ids; a
`loop` mesh whose material reads Ptex SHALL therefore render as its smooth cage
with a warning rather than refine.

#### Scenario: A uniform chart reads one coordinate per face

- **WHEN** a two-quad mesh authors `primvars:st = [(0.25, 0.5), (0.75, 0.5)]`
  with `interpolation = "uniform"`, at level 0 or 2
- **THEN** every hit on the first face reads `(0.25, 0.5)` and every hit on the
  second `(0.75, 0.5)`

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

### Requirement: Imported opacity inputs are cutouts

`crust:openpbr`'s `geometryOpacity` and `PxrDisneyBsdf`'s `alpha` SHALL be
the material's opacity, a cutout the integrator honours (see the rendering
capability). A `UsdPreviewSurface` whose `opacityThreshold` is above 0 SHALL be
a cutout mask: a point is present where `opacity ≥ opacityThreshold` and absent
otherwise, from a constant or a texture-driven `opacity` alike, and SHALL NOT
refract. A texture-driven `opacity` connected to a `UsdUVTexture`'s `a` output
SHALL read the file's alpha (see the textures capability). Under an
`opacityThreshold` of 0 (the default) `opacity` below 1 SHALL remain
translucency (transmission at `ior`), not a cutout.

#### Scenario: A constant preview cutout

- **WHEN** a `UsdPreviewSurface` authors `opacity = 0` and `opacityThreshold = 0.5`
- **THEN** its material is a cutout of opacity 0, and its BSDF transmits nothing

#### Scenario: A textured preview mask

- **WHEN** a `UsdPreviewSurface` under `opacityThreshold = 0.5` reads `opacity`
  from a texture whose value at a hit is 0.2, and at another 0.8
- **THEN** the first hit has opacity 0 and the second opacity 1

#### Scenario: A cutout from a texture's alpha

- **WHEN** a `UsdPreviewSurface` under `opacityThreshold = 0.5` reads `opacity`
  from the `a` output of a `UsdUVTexture` whose PNG has alpha 0 on its left half
  and 255 on its right
- **THEN** a hit on the left half has opacity 0 and one on the right half
  opacity 1, preloaded and streamed alike, and the import raises no warning
  about the material

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

### Requirement: Data window from render settings

The importer SHALL read `float4 dataWindowNDC` (`xmin, ymin, xmax, ymax`,
`(0, 0)` at the bottom-left) from the first `RenderProduct`, else from the
`RenderSettings` prim, as it resolves `resolution`. The render's region SHALL
be the pixels whose centres lie inside the window. The default
`(0, 0, 1, 1)` is the full frame.

- `dataWindowNDC` SHALL no longer appear in the "not honoured" warning.
- A window extending outside [0, 1] SHALL be clipped to the frame with one
  warning.
- A window that selects no pixel SHALL be refused with one warning, and the
  full frame SHALL render.
- The CLI's `--region` SHALL take precedence over any authored window.

#### Scenario: A window selecting the right half

- **WHEN** the settings author `resolution = (640, 360)` and
  `dataWindowNDC = (0.5, 0, 1, 1)`
- **THEN** pixels with `x` in 320..640 and every `y` are rendered, and no
  warning about `dataWindowNDC` is logged

#### Scenario: NDC y is bottom-up

- **WHEN** the settings author `resolution = (100, 100)` and
  `dataWindowNDC = (0, 0, 1, 0.25)`
- **THEN** the rendered rows are `y` in 75..100, the bottom quarter of the
  image

#### Scenario: Product overrides settings

- **WHEN** the settings author `dataWindowNDC = (0, 0, 0.5, 0.5)` and the
  first product authors `dataWindowNDC = (0.5, 0.5, 1, 1)`
- **THEN** the product's window is used

#### Scenario: Overscan is clipped

- **WHEN** the window is `(-0.1, 0, 1.1, 1)`
- **THEN** one warning is logged, and the full width renders

#### Scenario: An empty window

- **WHEN** the window is `(0.5, 0.5, 0.5, 0.6)`
- **THEN** one warning is logged, and the full frame renders

### Requirement: A material's volume terminal is read

A `Material` whose `outputs:mtlx:volume` (or universal `outputs:volume`)
terminal connects to a MaterialX shader SHALL be imported with that volume,
paired with its `mtlx` surface (or a universal MaterialX surface) when it has
one. When its surface is a shader of another kind, the volume SHALL be ignored
with one warning, and the material imported as before.

#### Scenario: A volume-only material binds a medium boundary

- **WHEN** a mesh is bound to a material with only an `outputs:mtlx:volume`
  terminal over an `ND_volume`
- **THEN** the imported world has a medium boundary, and the mesh is it

### Requirement: Camera exposure

The importer SHALL read the render camera's exposure as USD's linear exposure scale
(C++ `UsdGeomCamera::ComputeLinearExposureScale`):
`exposure:responsivity × exposure:time × (exposure:iso / 100) × 2^exposure /
exposure:fStop²`, each attribute read at the import's evaluation time, an unauthored
one at its schema fallback (`exposure:responsivity` 1, `exposure:time` 1,
`exposure:iso` 100, `exposure` 0, `exposure:fStop` 1). The camera's depth-of-field
`fStop` SHALL NOT enter the scale. A scale that is not finite or not positive SHALL be
refused with a `camera.invalid_exposure` warning naming the camera, and SHALL read 1.
Without a render camera prim (the procedural fallback scene), the scale SHALL be 1.
The importer SHALL read the render setting `enableExposureCompensation` (and
`crust:enableExposureCompensation`, which wins when both are authored) off the stage's
`RenderSettings` prim, default `true`, as Hydra renderers do; `false` SHALL make the
scale 1 whatever the camera authors.

#### Scenario: Nothing authored

- **WHEN** the render camera authors none of the exposure attributes
- **THEN** the exposure scale is exactly 1

#### Scenario: Stops

- **WHEN** the render camera authors `exposure = 2`
- **THEN** the exposure scale is 4

#### Scenario: The full photometric set

- **WHEN** the render camera authors `exposure = 1`, `exposure:time = 0.5`,
  `exposure:iso = 400`, `exposure:fStop = 2` and `exposure:responsivity = 1.5`
- **THEN** the exposure scale is `1.5 × 0.5 × 4 × 2 / 4 = 1.5`

#### Scenario: The depth-of-field f-stop does not expose

- **WHEN** the render camera authors `fStop = 4` and no `exposure:fStop`
- **THEN** the exposure scale is 1

#### Scenario: Animated exposure

- **WHEN** the render camera's `exposure` is time-sampled 0 at frame 1 and 1 at frame 2,
  and the stage is rendered at `-f 2`
- **THEN** the exposure scale is 2

#### Scenario: A scale that cannot apply

- **WHEN** the render camera authors `exposure:fStop = 0`
- **THEN** a `camera.invalid_exposure` warning names the camera and the exposure scale
  is 1

#### Scenario: Only the render camera is checked

- **WHEN** a camera whose exposure cannot apply is on the stage, but the
  `RenderSettings` prim names another camera with `exposure = 2`
- **THEN** no `camera.invalid_exposure` warning is raised and the exposure scale is 4

#### Scenario: Exposure compensation turned off

- **WHEN** the render camera authors `exposure = 2` and the stage's `RenderSettings`
  prim authors `bool enableExposureCompensation = false`
- **THEN** the exposure scale is 1

#### Scenario: The crust alias wins

- **WHEN** the `RenderSettings` prim authors `enableExposureCompensation = false` and
  `crust:enableExposureCompensation = true`, and the camera authors `exposure = 2`
- **THEN** the exposure scale is 4

### Requirement: Implicit shapes are tessellated as USD tessellates them

An implicit shape SHALL be tessellated into the points and faces OpenUSD's
`GeomUtil` mesh generators produce for its authored attributes, read at the
render's time code with the schema fallbacks: the same vertex order, cap rings
split from the side, seam, and `axis` frame. Curved shapes SHALL shade with the
generators' smooth vertex normals; `Cube` and `Plane` SHALL shade faceted. The
tessellation SHALL never be subdivided.

#### Scenario: Matches GeomUtil point for point

- **WHEN** a shape is tessellated at the radial and cap counts of a case in the
  committed GeomUtil fixture
- **THEN** its points and vertex normals match the fixture to 1e-6 relative, and
  its face counts and indices match exactly

#### Scenario: Schema fallbacks

- **WHEN** a `Cylinder` prim authors none of its attributes
- **THEN** it is tessellated with `radius = 1`, `height = 2`, `axis = "Z"`

#### Scenario: Axis

- **WHEN** a `Cone` authors `axis = "X"`
- **THEN** its spine runs along local X, as `UsdImagingGprimAdapter::GetImplicitBasis`
  orients it

#### Scenario: Unequal radii

- **WHEN** a `Cylinder_1` authors `radiusBottom = 1` and `radiusTop = 0`
- **THEN** it renders as a cone; and a `Capsule_1` with unequal radii renders with
  caps tangent to its sides

#### Scenario: Cylinder side is smooth, caps are flat

- **WHEN** a ray hits the side of a `Cylinder`
- **THEN** the shading normal is radial at the hit, and a hit on a cap shades with
  the cap's axis-aligned normal

#### Scenario: Not subdivided

- **WHEN** a curved shape is imported with `crust:subdivisionLevel = 2`
- **THEN** its triangles are exactly the generated ones; the level has no effect on it

### Requirement: Adaptive radial count of implicit shapes

With the render camera resolvable before traversal (`--camera` or
`RenderSettings.camera`), each **unshared** curved implicit shape SHALL take the
smallest radial count, within a fixed range, whose silhouette chord error projects
to at most a fixed fraction of a pixel at the nearest point of its world bounds.
Unshared and shared are as for adaptive subdivision. Shared shapes, and all shapes
without an early camera, SHALL take one fixed count.

#### Scenario: Larger on screen means more segments

- **WHEN** two otherwise identical cylinders are placed once each, one ten times
  closer to the render camera
- **THEN** the closer one has at least as many radial segments, and more unless
  both counts are clamped

#### Scenario: Independent of adaptive subdivision

- **WHEN** a stage authors `RenderSettings.camera` and no
  `crust:subdivisionEdgeLength`
- **THEN** implicit shapes are still adaptive

#### Scenario: No camera known before traversal

- **WHEN** neither `--camera` nor `RenderSettings.camera` names the camera
- **THEN** every curved implicit shape takes the fixed count, and no warning is
  emitted for it

#### Scenario: Shared prototype

- **WHEN** a prototype containing a `Cylinder` is placed more than once
- **THEN** the cylinder takes the fixed count, whatever its distance

#### Scenario: Nested native instance

- **WHEN** a `Cylinder` is inside a prototype reached only through a native instance
  nested in another prototype, and the outer prototype is placed once
- **THEN** the cylinder takes the fixed count, as nested prototypes are always
  built shared

#### Scenario: Off screen

- **WHEN** a placed-once cylinder lies wholly outside the camera's view pyramid,
  with the frustum test on (the default)
- **THEN** it takes the minimum count

#### Scenario: Off screen with the frustum test off

- **WHEN** `CRUST_ADAPTIVE_FRUSTUM=0` and a placed-once cylinder lies wholly outside
  the camera's view pyramid
- **THEN** its count follows its distance alone, as if it were on screen

#### Scenario: Capsule caps follow

- **WHEN** a capsule with equal radii takes radial count `N`
- **THEN** each hemispherical cap has a quarter as many rings, rounded up, at least two;
  with unequal radii the caps share twice that many between them, as GeomUtil splits
  them

### Requirement: Unread geometry types are reported

A `UsdGeomGprim` prim that the importer reads as neither a surface nor a volume
SHALL raise the `geom.unsupported_type` warning instead of being dropped silently.
It SHALL be raised once per prim type per load, from the top-level traversal and
the prototype walk alike, carrying a count: one per prim met by the top-level walk,
and one per prim per prototype build, not per placement. A streamed load may build
a prototype once per chunk.

#### Scenario: Points prim

- **WHEN** a stage contains three `Points` prims and one `NurbsPatch`
- **THEN** the load's warnings carry `geom.unsupported_type` once for `Points`
  (count 3) and once for `NurbsPatch` (count 1), and nothing is rendered for them

#### Scenario: Inside a prototype

- **WHEN** an unread `Gprim` type is inside a prototype
- **THEN** it is reported in the same way

#### Scenario: Placements do not multiply the count

- **WHEN** a prototype holding one `Points` prim is placed fifty times, in a load
  that is not streamed
- **THEN** the warning for `Points` carries count 1

#### Scenario: Non-geometry prims are not reported

- **WHEN** a stage contains `Xform`, `Scope`, `Camera`, `Material` and UsdLux prims
- **THEN** no `geom.unsupported_type` warning is raised for them

### Requirement: Displacement on implicit shapes is refused

A displacement bound to an implicit shape SHALL be ignored with a warning, and the
shape SHALL render undisplaced with the rest of its material.

#### Scenario: Displaced cube

- **WHEN** a `Cube` is bound to a material with a displacement
- **THEN** a warning names the prim, and the cube renders as an undisplaced box

### Requirement: The camera shutter defines the motion interval

The importer SHALL read the render camera's `shutter:open` and `shutter:close`, which are
offsets in time codes from the frame given by `-f`, with USD's fallback of 0 for each. With
frame `f`, motion SHALL be measured from `f + open` (shutter open) to `f + close` (shutter
close). The shutter SHALL be instantaneous when `close <= open`, or when no frame is given.
The same interval SHALL drive the beauty's motion blur and the `motionvector` AOV.

#### Scenario: Shutter authored on the camera

- **WHEN** the render camera authors `shutter:open = 0` and `shutter:close = 0.5`, and the
  render is made with `-f 5`
- **THEN** motion is measured from time code 5 to time code 5.5

#### Scenario: Shutter opening before the frame

- **WHEN** the camera authors `shutter:open = -0.25` and `shutter:close = 0.25`, and the
  render is made with `-f 5`
- **THEN** motion is measured from time code 4.75 to time code 5.25

#### Scenario: No shutter authored

- **WHEN** the camera authors neither attribute
- **THEN** the shutter is instantaneous, and a stage with time-sampled transforms renders
  bitwise as it did before this change, with or without `-f`

#### Scenario: Shutter of a named camera

- **WHEN** `--camera` or `RenderSettings.camera` names a camera whose shutter is authored,
  and another camera with a different shutter comes first on the stage
- **THEN** the named camera's shutter is used

#### Scenario: Camera settled on differs from the one whose shutter was read

- **WHEN** no camera is named, and the camera the render goes through is not the first
  camera on the stage with payloads unloaded, and the two cameras' shutters differ
- **THEN** one warning names both cameras and the shutter the motion was measured over

### Requirement: Time-sampled transforms move geometry over the shutter

When the shutter is not instantaneous, each mesh, sphere, implicit shape, native
instance and `PointInstancer` placement whose world transform varies over time (its own
ops or an ancestor's) SHALL move from its shutter-open world transform to its
shutter-close one, interpolated linearly. Unless motion blur is disabled, the beauty SHALL
be blurred along that motion.

#### Scenario: Animated translate is blurred

- **WHEN** a sphere's `xformOp:translate` has time samples (-2, 0.6, 0) at frame 1 and
  (2, 0.6, 0) at frame 10, the camera authors `shutter:close = 1`, and the render is made
  with `-f 5`
- **THEN** the sphere is streaked along +X over the distance it covers from frame 5 to
  frame 6, about 0.44 units

#### Scenario: Animated ancestor

- **WHEN** a static mesh sits under an `Xform` whose `xformOp:rotateY` is time-sampled,
  and the shutter is not instantaneous
- **THEN** the mesh moves with its ancestor's rotation over the shutter

#### Scenario: Animated instanceable prim

- **WHEN** an instanceable prim whose prototype holds two meshes has a time-sampled
  transform, and the shutter is not instantaneous
- **THEN** both meshes of that instance move over the shutter, and other instances of the
  same prototype with static transforms do not

#### Scenario: Animated PointInstancer

- **WHEN** a `PointInstancer`'s own `xformOp:translate` is time-sampled, and the shutter
  is not instantaneous
- **THEN** every instance it places moves with that translation over the shutter

#### Scenario: Static prims are unchanged

- **WHEN** the shutter is not instantaneous and a prim's world transform has no time
  samples on itself or any ancestor
- **THEN** that prim renders exactly as it would with an instantaneous shutter

#### Scenario: Other attributes stay at the frame

- **WHEN** a light's intensity and a moving mesh's transform are both time-sampled, and
  the shutter opens before the frame
- **THEN** the light's intensity is evaluated at the frame, and only the mesh's transform
  is evaluated at shutter open and shutter close

### Requirement: Authored translation adds to animated motion

`crust:motion:translate` SHALL remain a world-space displacement over the shutter,
whatever the shutter's length, including an instantaneous one. On a prim whose world
transform also varies over the shutter, the authored translation SHALL be added to that
motion: the prim's shutter-close placement SHALL be its shutter-close world transform,
moved by the authored translation.

#### Scenario: Translation without a shutter

- **WHEN** a sphere authors `crust:motion:translate = (1, 0, 0)` and the camera authors no
  shutter
- **THEN** the sphere is blurred over one unit along X, as before this change

#### Scenario: Translation on an animated prim

- **WHEN** a mesh whose `xformOp:translate` moves it by (0, 1, 0) over the shutter also
  authors `crust:motion:translate = (1, 0, 0)`
- **THEN** each of its points moves by (1, 1, 0) over the shutter

### Requirement: Time-sampled motion that is not produced is reported

The importer SHALL warn, once per cause with a count of the prims involved, when a drawn
prim's world transform varies over time but produces no motion:

- the shutter is not instantaneous but no frame was given;
- the prim is a curve, a light or a volume;
- it lies inside a prototype;
- it is a mesh baked for a non-invertible transform.

Static stages and stages with an instantaneous shutter SHALL raise none of these warnings.

#### Scenario: No frame given

- **WHEN** the camera authors `shutter:close = 0.5`, a mesh's transform is time-sampled,
  and the render is made without `-f`
- **THEN** one warning says that time samples are not evaluated without a frame, and the
  mesh renders sharp

#### Scenario: Animated curves

- **WHEN** a `BasisCurves` prim's transform is time-sampled and the shutter is not
  instantaneous
- **THEN** the curves render sharp where their transform places them at the frame, and
  one warning counts them

#### Scenario: Instantaneous shutter is not warned about here

- **WHEN** the camera authors no shutter and a mesh's transform is time-sampled
- **THEN** none of these warnings is raised

### Requirement: Camera depth of field

The render camera's lens SHALL have diameter `focalLength / fStop × 0.001 /
metersPerUnit` scene units: `focalLength` read in millimetres, `metersPerUnit`
the stage's. It SHALL focus at `focusDistance` scene units. Fallbacks:
`focalLength` 50, `focusDistance` 10, `metersPerUnit` 0.01. An `fStop` of 0 or
less, or unauthored, SHALL be a pinhole. The field of view SHALL not depend on
`fStop`, `focusDistance` or `metersPerUnit`.

#### Scenario: A centimetre stage

- **WHEN** a stage authors `metersPerUnit = 0.01` and the render camera
  authors `focalLength = 50`, `fStop = 2` and `focusDistance = 500`
- **THEN** the lens diameter is 2.5 scene units (25 mm), and rays from every
  lens sample through the frame centre meet 500 units down the view axis

#### Scenario: A metre stage

- **WHEN** a stage authors `metersPerUnit = 1` and the render camera authors
  `focalLength = 50` and `fStop = 2`
- **THEN** the lens diameter is 0.025 scene units (25 mm)

#### Scenario: metersPerUnit unauthored

- **WHEN** a stage does not author `metersPerUnit` and the render camera
  authors `focalLength = 50` and `fStop = 2`
- **THEN** the lens diameter is 2.5 scene units, as on a centimetre stage

#### Scenario: No fStop is a pinhole

- **WHEN** the render camera authors no `fStop`, or `fStop = 0`
- **THEN** every camera ray through a pixel starts at the camera's position,
  whatever the lens sample

#### Scenario: The field of view does not change

- **WHEN** the same camera is rendered on a stage with `metersPerUnit = 0.01`
  and on one with `metersPerUnit = 1`
- **THEN** the ray through each pixel's centre from the lens centre has the
  same direction on both

### Requirement: An unusable metersPerUnit is refused

A stage `metersPerUnit` that is not finite, not positive, or so small that the
lens diameter is not a finite `f32` SHALL be refused with
one `camera.invalid_meters_per_unit` warning naming the value, and SHALL read as
0.01. The check SHALL apply only when the render camera has a lens, so a
pinhole render raises no warning.

#### Scenario: A zero metersPerUnit

- **WHEN** a stage authors `metersPerUnit = 0` and the render camera authors
  `focalLength = 50` and `fStop = 2`
- **THEN** one `camera.invalid_meters_per_unit` warning is raised and the lens
  diameter is 2.5 scene units

#### Scenario: A pinhole does not check

- **WHEN** a stage authors `metersPerUnit = 0` and the render camera authors
  no `fStop`
- **THEN** no `camera.invalid_meters_per_unit` warning is raised

#### Scenario: A unit too small for the lens

- **WHEN** a stage authors `metersPerUnit = 1e-42` and the render camera authors
  `focalLength = 50` and `fStop = 2`
- **THEN** one `camera.invalid_meters_per_unit` warning is raised, the lens
  diameter is 2.5 scene units and every camera ray is finite

### Requirement: Render settings can disable depth of field

`disableDepthOfField = true` SHALL render through a pinhole at the camera's
position, whatever its `fStop`. The field of view and the focus plane SHALL not
change. The value SHALL be resolved like `disableMotionBlur`: the first
product's authored value, else the settings prim's, default `false`. It SHALL
not be warned about as not honoured. An MCP edit of it SHALL take effect as a
full re-import would.

#### Scenario: Sharp render with a lens authored

- **WHEN** the render camera authors `fStop = 2` and the render settings author
  `disableDepthOfField = true`
- **THEN** every camera ray starts at the camera's position, and no
  `product.unhonoured_attribute` warning names `disableDepthOfField`

#### Scenario: Product overrides the settings

- **WHEN** the settings prim authors `disableDepthOfField = true` and the first
  product authors `disableDepthOfField = false`, and the camera authors
  `fStop = 2`
- **THEN** the render has depth of field

#### Scenario: Depth of field stays on by default

- **WHEN** `disableDepthOfField` is not authored and the camera authors
  `fStop = 2`
- **THEN** the render has depth of field

#### Scenario: Products that disagree

- **WHEN** the first product authors `disableDepthOfField = true` and a second
  product with the same camera and resolution authors
  `disableDepthOfField = false`
- **THEN** both products are written without depth of field and one
  `product.depth_of_field_mismatch` warning names the second product

### Requirement: The camera's aperture is conformed to the image's aspect

The importer SHALL read `aspectRatioConformPolicy` from the first `RenderProduct`,
else from the `RenderSettings` prim, as it resolves `resolution`; unauthored it SHALL
be `expandAperture`. Before the field of view is derived, the render camera's
apertures SHALL be conformed to the image's aspect as OpenUSD's
`CameraUtilConformWindow` does, with square pixels. An unauthored `verticalAperture`
SHALL still follow the image's aspect, so it needs no conform.

#### Scenario: expandAperture keeps a narrow aperture's height

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (200, 100)` and no policy
- **THEN** the render frames a 48 × 24 aperture: the vertical field of view is the
  authored one, and the image is identical to the render before this change

#### Scenario: expandAperture keeps a wide aperture's width

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (100, 100)` and no policy
- **THEN** the render frames a 36 × 36 aperture: the whole authored width is in frame

#### Scenario: adjustApertureHeight keeps the width

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (200, 100)` and
  `aspectRatioConformPolicy = "adjustApertureHeight"`
- **THEN** the render frames a 36 × 18 aperture, and the image differs from the one
  rendered without the policy

#### Scenario: cropAperture keeps the smaller extent

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (100, 100)` and
  `aspectRatioConformPolicy = "cropAperture"`
- **THEN** the render frames a 24 × 24 aperture

#### Scenario: adjustApertureWidth keeps the height

- **WHEN** the camera authors `horizontalAperture = 36`, `verticalAperture = 24`, and
  the settings author `resolution = (100, 100)` and
  `aspectRatioConformPolicy = "adjustApertureWidth"`
- **THEN** the render frames a 24 × 24 aperture

#### Scenario: A product's policy overrides the settings'

- **WHEN** the settings author `aspectRatioConformPolicy = "adjustApertureWidth"` and
  the first product authors `aspectRatioConformPolicy = "adjustApertureHeight"`
- **THEN** the product's policy frames the render

#### Scenario: An aperture that matches the image is unchanged

- **WHEN** the camera's authored aperture has the image's aspect, or authors no
  `verticalAperture`
- **THEN** every policy renders the same image as before this change

#### Scenario: The policy on the camera is not read

- **WHEN** the camera prim, not the settings or a product, authors
  `aspectRatioConformPolicy`
- **THEN** it has no effect, as in OpenUSD, and the settings' policy (or
  `expandAperture`) frames the render

### Requirement: Everything that reads the camera's framing reads the conformed one

Adaptive subdivision, on-screen implicit-shape sizing and the MCP session's
in-place settings edits SHALL read the same conformed apertures as the render
camera, so a policy that reframes the image also re-sizes what was tessellated for
the old frame.

#### Scenario: Adaptive subdivision follows the policy

- **WHEN** a stage with adaptive subdivision changes only its
  `aspectRatioConformPolicy` so that the conformed vertical aperture changes
- **THEN** the tessellation is computed for the conformed field of view, and an MCP
  edit making that change re-imports rather than applying in place

#### Scenario: An in-place policy edit matches a re-import

- **WHEN** an MCP edit changes `aspectRatioConformPolicy` on a stage whose geometry
  does not read the camera
- **THEN** the edit applies in place, and its render is bit-identical to a full
  import of the same layer

### Requirement: adjustPixelAspectRatio falls back to expandAperture

Crust SHALL render square pixels only. A resolved policy of
`adjustPixelAspectRatio` whose aperture aspect differs from the image's SHALL be
conformed as `expandAperture`, with one `product.unhonoured_attribute` warning naming
the pixel aspect ratio it would have needed.

#### Scenario: A pixel-aspect policy on a mismatched aperture

- **WHEN** the camera authors a 36 × 24 aperture, and the settings author
  `resolution = (200, 100)` and
  `aspectRatioConformPolicy = "adjustPixelAspectRatio"`
- **THEN** one `product.unhonoured_attribute` warning names a pixel aspect ratio of
  0.75, and the render is the `expandAperture` one

#### Scenario: A pixel-aspect policy on a matching aperture

- **WHEN** the policy is `adjustPixelAspectRatio` and the aperture already has the
  image's aspect
- **THEN** no warning is raised and the framing is unchanged

### Requirement: An unknown conform policy is refused

An authored `aspectRatioConformPolicy` that is not one of the five schema tokens
SHALL be refused with one `product.invalid_conform_policy` warning naming the prim
and the token, and read as `expandAperture`.

#### Scenario: A misspelt policy

- **WHEN** the settings author `aspectRatioConformPolicy = "adjustApertureHieght"`
- **THEN** one `product.invalid_conform_policy` warning is raised, and the render is
  framed as `expandAperture`

### Requirement: Products that differ in conform policy share the first's framing

Crust SHALL render one framing per render. A product after the first whose
resolved `aspectRatioConformPolicy` differs from the first product's SHALL still be
written, framed by the first's policy, with one `product.framing_mismatch` warning.

#### Scenario: A second product with another policy

- **WHEN** two products share the camera and resolution, the first resolves
  `expandAperture` and the second `adjustApertureHeight`
- **THEN** both files are written with the `expandAperture` framing, and one
  `product.framing_mismatch` warning names the second product

### Requirement: A curve hit knows its place on the strand

A hit on a `BasisCurves` or `NurbsCurves` curve SHALL carry the index of the
curve within its prim and `v`, its position along the whole strand in [0, 1]:
the curve's parameter normalised so the first authored point (or the start of
the NURBS domain, narrowed by `ranges`) is 0 and the last is 1. `v` SHALL be
continuous across span boundaries and SHALL hold at every placement: top-level,
instanced and `PointInstancer`-placed (curves carry no motion blur).

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
float), in any precision and as an array or a lone `constant` value, SHALL be
read when a material bound to the prim reads them by name, and
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

### Requirement: A curve's chart is filtered by the ray's footprint

Images SHALL filter a curve's chart by the ray's footprint, converted through
how fast the chart changes along the strand, as they filter a mesh's chart
through its UV density.

#### Scenario: A chart seen from afar is filtered

- **WHEN** a ray whose footprint is 0.01 wide hits a strand whose `vertex` `st`
  changes by half a unit per world unit along it
- **THEN** the hit's texture footprint is 0.005 chart units, and 0 for a chart
  authored once per curve

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
render bit-identically to its render before this change. A mesh SHALL render
bit-identically unless its material uses a node that did not compile before
(it then had the reported unsupported-node grey).

A curve whose material reads only the chart, and which authors none, SHALL
build no strand table either.

#### Scenario: A textured groom with no st

- **WHEN** a curve that authors no `st` is bound to a material whose only
  curve need is the chart
- **THEN** the import builds no strand table for it, and every hit reads the
  chart at (0, 0)

#### Scenario: The curves sample is unchanged

- **WHEN** `samples/curves.usda` is rendered at 16 spp
- **THEN** the image is bit-identical to the one rendered before this change

#### Scenario: Unread primvars are not decoded

- **WHEN** a groom prim authors ten primvars and its material reads one
- **THEN** only that one primvar is decoded, and the `--stats` curve-table
  memory counts only its values

### Requirement: The dicing camera

Tessellation that reads a camera SHALL read the dicing camera: adaptive subdivision levels, per-face edge rates and their view test, and the radial count of implicit shapes. Where those requirements say "the render camera", they mean the dicing camera. The dicing camera SHALL be the target of `rel crust:dicingCamera` on the `RenderSettings` prim when that target is a camera, and the render camera otherwise.

#### Scenario: Unauthored, the render camera dices

- **WHEN** a stage authors no `crust:dicingCamera`
- **THEN** every subdivision level, edge rate and radial count is the one the
  import gave before this requirement, and a 16 spp render is bit-identical to it

#### Scenario: Geometry is diced for the dicing camera

- **WHEN** a stage authors `crust:subdivisionEdgeLength`, renders through camera
  `/A`, and sets `crust:dicingCamera` to camera `/B`, placed elsewhere
- **THEN** every mesh is tessellated exactly as when the same stage renders
  through `/B` with no `crust:dicingCamera`, while the image is seen through `/A`

#### Scenario: A dicing camera alone enables adaptive tessellation

- **WHEN** a stage authors `crust:subdivisionEdgeLength` and `crust:dicingCamera`
  but names no render camera (no `--camera`, no `RenderSettings.camera`)
- **THEN** adaptive subdivision and on-screen radial counts are computed through
  the dicing camera, and no `subdiv.adaptive_needs_camera` warning is emitted

#### Scenario: A target that is not a camera

- **WHEN** `crust:dicingCamera` targets a prim that is not a camera, or no prim
- **THEN** one warning naming the target is emitted, and the render camera dices

#### Scenario: Products do not override it

- **WHEN** the first render product authors its own `camera`
- **THEN** the dicing camera is still `crust:dicingCamera` when authored, and the
  product's camera only when it is not
