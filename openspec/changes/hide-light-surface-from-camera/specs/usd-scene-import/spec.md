## MODIFIED Requirements

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
- **THEN** it becomes an emissive sphere added to both the light list and the
  world

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
