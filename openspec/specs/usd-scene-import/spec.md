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

The importer SHALL map `UsdGeomMesh` to world-baked triangles wrapped in a
`BVHNode`, and `UsdGeomSphere` to an analytic `Sphere`. The top-level world
remains a linear list; BVH acceleration is built only per imported mesh.

#### Scenario: Mesh prim

- **WHEN** a `UsdGeomMesh` prim is traversed
- **THEN** its triangles are baked into world space and added as a BVH-wrapped mesh

#### Scenario: Sphere prim

- **WHEN** a `UsdGeomSphere` prim is traversed
- **THEN** it is added as an analytic sphere at its world-space transform

### Requirement: Material resolution by shader id

The importer SHALL resolve a bound material via `MaterialBindingAPI` and dispatch
on the surface shader's `info:id`: `UsdPreviewSurface` maps into `OpenPBR`
(portable field mapping), `crust:openpbr` decodes 1:1 into `OpenPBR`, and any
geometry without a resolvable bound material falls back to a grey `Lambertian`.

#### Scenario: UsdPreviewSurface binding

- **WHEN** a surface binds a shader with `info:id = "UsdPreviewSurface"`
- **THEN** its inputs are mapped into an `OpenPBR` material (e.g. `diffuseColor →
  baseColor`, `metallic → baseMetalness`, `roughness → specularRoughness`)

#### Scenario: crust:openpbr binding

- **WHEN** a surface binds a shader with `info:id = "crust:openpbr"`
- **THEN** each camelCase input is decoded 1:1 into the matching `OpenPBR` field

#### Scenario: Unbound geometry

- **WHEN** geometry has no resolvable bound material
- **THEN** it is assigned a grey Lambertian material

### Requirement: Light schema mapping

The importer SHALL map `UsdLuxSphereLight` to an `Emissive` sphere that is both a
light and visible geometry. Other lux schemas (`RectLight`, `DiskLight`,
`DistantLight`, `DomeLight`, `CylinderLight`) SHALL warn once and be skipped.

#### Scenario: Sphere light

- **WHEN** a `UsdLuxSphereLight` prim is traversed
- **THEN** it becomes an emissive sphere added to both the light list and the world

#### Scenario: Unsupported lux light

- **WHEN** a non-sphere lux light prim is traversed
- **THEN** a warning is emitted and the light is skipped

### Requirement: Render settings from USD with defaults

The importer SHALL read `resolution` from `UsdRenderSettings` and per-render
params from custom attributes in the `crust:` namespace (`crust:samplesPerPixel`,
`crust:maxDepth`, `crust:minSamplesPerPixel`, `crust:varianceThreshold`,
`crust:frame`). Missing attributes SHALL fall back to defaults (128 spp, depth
32, 640×360).

#### Scenario: Authored settings

- **WHEN** the stage authors `crust:` render params
- **THEN** those values populate `RenderSettings`

#### Scenario: Missing settings fall back to defaults

- **WHEN** a `crust:` param is absent
- **THEN** the documented default is used in its place
