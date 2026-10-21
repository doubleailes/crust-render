# Spec Delta

## MODIFIED Requirements

### Requirement: Light schema mapping

The importer SHALL map every `UsdLux` light it reads — `SphereLight`,
`RectLight`, `DiskLight`, `CylinderLight`, `DistantLight` and `DomeLight` — onto the lights
defined by the `lighting` capability, with UsdLux units, `normalize`,
colour temperature, `ShapingAPI`, camera visibility (`crust:light:cameraVisible`,
`primvars:ri:attributes:visibility:camera`) and `collection:lightLink` /
`collection:shadowLink` honoured as that capability states. A `UsdGeomMesh` with
`UsdLuxMeshLightAPI` applied SHALL be read as a mesh light, with its `LightAPI` inputs,
`light:materialSyncMode` and linking collections, as the `lighting` capability's "Mesh
lights" requirements state; the mesh SHALL still be imported as geometry with its bound
material. `PortalLight`, `VolumeLightAPI`, `GeometryLight` and light filters SHALL NOT be
read.

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

#### Scenario: Mesh light

- **WHEN** a `UsdGeomMesh` prim with `UsdLuxMeshLightAPI` applied is traversed
- **THEN** it becomes geometry in the world and an entry in the light list, and
  `crust ls light` lists its path

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
