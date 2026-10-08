## ADDED Requirements

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
