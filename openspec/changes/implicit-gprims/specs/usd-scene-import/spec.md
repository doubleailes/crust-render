## MODIFIED Requirements

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

## ADDED Requirements

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

- **WHEN** a capsule takes radial count `N`
- **THEN** each hemispherical cap has a quarter as many rings, rounded up, at least two

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
