# usd-scene-import Specification

## Purpose

Build the runtime `Scene` (camera, world geometry, lights, render settings) from
a USD stage. USD is the only supported scene format. This capability covers stage
loading, Xform-hierarchy baking, geometry and light schema mapping, material
resolution by shader id, and render-settings parsing. Lives in
`crust-core/src/scene/usd_import.rs`, entry point `Scene::from_usd`.

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

The importer SHALL map `UsdLuxSphereLight` to an `Emissive` sphere and
`UsdLuxRectLight` to two emissive triangles plus an `AreaLight(RectShape)`
(local XY plane, emitting along -Z per UsdLux — effectively one-sided). Each
becomes both a light-list entry and scene geometry.

A mapped light's emissive **surface** SHALL be attached visible to every ray
category except the camera, so the light shape is not directly photographed —
matching the convention that lighting rigs may be placed inside the camera
frustum without appearing in the image. The surface SHALL remain visible to
shadow and indirect rays, because the light's `geom_id` is how a bounce ray's
arrival at the light is attributed to it for multiple-importance-sampling
weighting; hiding it from indirect rays would drop the bounce half of that
estimator.

A light prim MAY author `crust:rayMask`, which SHALL be honored verbatim and
override the default — so `crust:rayMask = 7` (camera + shadow + indirect)
restores a directly visible light surface.

`UsdLuxDistantLight` and `UsdLuxDomeLight` SHALL be mapped as light-list-only
entries with no scene geometry, and are therefore unaffected by surface
visibility. Other lux schemas (`DiskLight`, `CylinderLight`) SHALL warn once
and be skipped.

#### Scenario: Sphere light

- **WHEN** a `UsdLuxSphereLight` prim is traversed
- **THEN** it becomes an emissive sphere added to both the light list and the world

#### Scenario: Rect light

- **WHEN** a `UsdLuxRectLight` prim is traversed
- **THEN** it becomes two emissive triangles added to both the light list and
  the world

#### Scenario: Light surface is not photographed by default

- **WHEN** a camera ray is traced along a line that passes through a mapped
  light's surface, and the light prim authors no `crust:rayMask`
- **THEN** the ray does not intersect the light's surface and reaches whatever
  lies behind it

#### Scenario: Light surface still bounces and occludes by default

- **WHEN** an indirect ray or a shadow ray is traced along that same line
- **THEN** it does intersect the light's surface, so the light is still found
  by bounce-sampled paths and still occludes

#### Scenario: Authoring the ray mask restores a visible light surface

- **WHEN** a mapped light prim authors `int crust:rayMask = 7`
- **THEN** a camera ray traced at its surface intersects it

#### Scenario: Masking the surface does not drop the light

- **WHEN** a mapped light's surface is hidden from the camera by default
- **THEN** the light is still present in the light list and still illuminates
  the scene

#### Scenario: Unsupported lux light

- **WHEN** a `DiskLight` or `CylinderLight` prim is traversed
- **THEN** a warning is emitted and the light is skipped

### Requirement: Ray visibility mask

Any geometry prim MAY author `crust:rayMask` (int; bit 0 camera, bit 1 shadow,
bit 2 indirect) to control which ray categories intersect it. When the
attribute is absent, an ordinary geometry prim SHALL default to visible to all
categories, while a `UsdLuxSphereLight`/`UsdLuxRectLight` surface SHALL default
to all categories except the camera.

#### Scenario: Geometry hidden from the camera

- **WHEN** a mesh authors `int crust:rayMask = 6`
- **THEN** camera rays pass through it while shadow and indirect rays are
  blocked, so it darkens the scene without appearing in the image

#### Scenario: Absent on ordinary geometry

- **WHEN** a geometry prim authors no `crust:rayMask`
- **THEN** it is visible to every ray category

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
`crust:guidingTrainIterations`, `crust:guidingProb`). Missing attributes SHALL
fall back to defaults (128 spp, depth 32, 640×360, power MIS, guiding off).

#### Scenario: Authored settings

- **WHEN** the stage authors `crust:` render params
- **THEN** those values populate `RenderSettings`

#### Scenario: Missing settings fall back to defaults

- **WHEN** a `crust:` param is absent
- **THEN** the documented default is used in its place
