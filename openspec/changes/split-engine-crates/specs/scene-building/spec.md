# Spec Delta

## ADDED Requirements

### Requirement: A scene can be built from plain data

The engine library (`crust-core`) SHALL build a renderable scene from plain data
handed to it by a host, with no scene file and no USD library involved. That data is
meshes (points, faces, optional normals and UVs, a subdivision scheme, displacement,
transform samples), instances of prototypes, lights, cameras, materials (MaterialX
documents, OpenPBR parameters or UsdPreviewSurface parameters), render settings and
AOV requests. Subdivision, displacement, instancing and acceleration-structure
construction SHALL happen inside the engine, whichever host supplied the data. The
engine library SHALL compile without the `openusd` crates.

#### Scenario: A host with no USD

- **WHEN** a program that depends on `crust-core` but not on `crust-usd` builds a
  subdivided mesh, a rect light, an OpenPBR material and a camera, then renders
- **THEN** the render completes and the mesh is shaded by the material and lit by the
  light

#### Scenario: The engine builds without USD

- **WHEN** `crust-core` is built on its own
- **THEN** neither `openusd` nor `openusd-schemas` appears in its dependency tree

### Requirement: The USD import builds through the same interface

The USD importer (`crust-usd`) SHALL produce its scene only through the interface
other hosts use. A stage rendered through `crust render` SHALL be bit-identical to
the same stage rendered before this interface existed, at 16 samples per pixel.

#### Scenario: Every sample is unchanged

- **WHEN** every checked-in sample scene is rendered at 16 spp with
  `--indirect-clamp 0`, before and after the importer moves onto this interface
- **THEN** `crust diff` reports every pair identical

### Requirement: Built entries can carry a source key

Each mesh, light and material handed to the builder MAY carry a source key (for USD,
the prim path). The scene SHALL map each key to the entries built from it, so a host
can update a keyed material or light in place under the same rules the MCP session
already applies.

#### Scenario: Updating a keyed light in place

- **WHEN** a host builds a scene with a light keyed `/lights/key`, then replaces that
  light's intensity by its key
- **THEN** the update is applied without rebuilding the acceleration structure, and
  the next render equals a render of a scene built from scratch with the new
  intensity, bit for bit
