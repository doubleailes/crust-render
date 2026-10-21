# Spec Delta

## ADDED Requirements

### Requirement: MaterialX constant emission is known without a hit

A MaterialX surface whose emission depends on no texture, primvar, curve property or
shading-point input, and whose emission falloff toward the viewer is neutral, SHALL
report that emission as a constant, known when the material is loaded. The constant
SHALL equal, bit for bit, the emission the material gives at every hit, with or without
the `jit` feature. Any other MaterialX surface SHALL report no constant emission.

#### Scenario: A flat emissive card

- **WHEN** an `open_pbr_surface` with `emission_luminance = 5` and a literal
  `emission_color` is loaded
- **THEN** its constant emission is reported, and equals bit for bit what a camera ray
  hitting it at any point and angle collects

#### Scenario: A textured emission

- **WHEN** a `standard_surface`'s `emission_color` is connected to an `image` node
- **THEN** no constant emission is reported, and the surface renders as before this
  requirement

#### Scenario: Emission under a coat

- **WHEN** an `open_pbr_surface` with a literal emission has `coat_weight = 1`
- **THEN** no constant emission is reported, because its emission fades with the
  viewing angle

#### Scenario: Rendering is unchanged

- **WHEN** a stage with a constant-emission MaterialX surface and no `MeshLightAPI` is
  rendered at 16 spp
- **THEN** the image is bit-identical to the same render before this requirement
