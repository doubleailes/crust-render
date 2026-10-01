## MODIFIED Requirements

### Requirement: Known gaps

The following SHALL be documented as unsupported: anisotropic filtering,
filtering across Ptex face boundaries, texture alpha, `UsdTransform2d`, UV sets
other than `st` and its fallbacks, tangents on instanced geometry (normal maps
fall back to the geometric normal), and a per-texture colour space for Ptex:
`half` / `float` samples keep their full range, but every `.ptx` is decoded by
gamma 2.2, so linear Ptex data is mis-decoded.

#### Scenario: A normal map on an instanced mesh

- **WHEN** a normal-mapped material is bound to geometry placed more than once
- **THEN** it shades with the geometric normal

#### Scenario: A UV texture on a subdivided mesh

- **WHEN** a UV-textured material is bound to a mesh with an authored
  subdivision scheme
- **THEN** the texture is sampled through the refined UV chart rather than
  dropped
