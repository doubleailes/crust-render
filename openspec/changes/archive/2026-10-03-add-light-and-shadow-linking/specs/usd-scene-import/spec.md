## ADDED Requirements

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

## MODIFIED Requirements

### Requirement: Light schema mapping

The importer SHALL map every `UsdLux` light it reads — `SphereLight`,
`RectLight`, `DiskLight`, `CylinderLight`, `DistantLight` and `DomeLight` — onto
the lights defined by the `lighting` capability, with UsdLux units, `normalize`,
colour temperature, `ShapingAPI`, and `collection:lightLink` /
`collection:shadowLink` honoured as that capability states. `PortalLight`, mesh
lights and light filters SHALL NOT be read.

#### Scenario: Area light

- **WHEN** a `UsdLuxRectLight`, `SphereLight`, `DiskLight` or `CylinderLight`
  prim is traversed
- **THEN** it becomes an entry in the light list and emitting geometry in the
  world

#### Scenario: Infinite light

- **WHEN** a `UsdLuxDistantLight` or `UsdLuxDomeLight` prim is traversed
- **THEN** it becomes a light-list entry with no scene geometry
