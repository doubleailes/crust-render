## MODIFIED Requirements

### Requirement: Light schema mapping

The importer SHALL map every `UsdLux` light it reads — `SphereLight`,
`RectLight`, `DiskLight`, `CylinderLight`, `DistantLight` and `DomeLight` — onto
the lights defined by the `lighting` capability, with UsdLux units, `normalize`,
colour temperature, `ShapingAPI` and camera visibility
(`crust:light:cameraVisible`, `primvars:ri:attributes:visibility:camera`)
honoured as that capability states. `PortalLight`, mesh lights and light filters
SHALL NOT be read.

The importer SHALL read the render setting `domeLightCameraVisibility`
(and `crust:domeLightCameraVisibility`) off the stage's `RenderSettings` prim, as
the `lighting` capability's "Infinite lights" requirement states.

Of `collection:lightLink`, the importer SHALL read only whether the collection
covers no geometry. It covers none when `includeRoot` is false and it includes
no path. It also covers none when it authors only `excludes` and every receiver
the import traversed — the stage path of each mesh, sphere, curves, volume,
native instance and `PointInstancer` prim — is at or below an excluded path.
The absolute root `/` is above every prim. The test SHALL be judged on stage
paths, so it does not depend on instance prototypes or on how the import is
streamed. Excludes that name lights rather than receivers SHALL NOT count as
covering anything. An exclude more than four path components deep, an
`includes`, a `membershipExpression`, or excludes that cover some receivers but
not all, SHALL be read as the UsdLux default (every geometry), with one warning
per light. `shadowLink` SHALL NOT be read.

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
- **THEN** `sky_dome_cam_llc` illuminates nothing, and `sky_dome_env_llc`,
  whose excludes name only the other light, illuminates everything

#### Scenario: A light traversed before what it excludes

- **WHEN** a streamed stage's backdrop dome is traversed in an earlier chunk
  than the geometry its `excludes` cover
- **THEN** it still illuminates nothing, as it does with streaming disabled
