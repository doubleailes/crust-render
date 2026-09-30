# Spec Delta

## MODIFIED Requirements

### Requirement: Known gaps

The following SHALL be documented as unsupported: anisotropic filtering,
filtering across Ptex face boundaries, texture alpha, `UsdTransform2d`, UV sets
other than `st` and its fallbacks, and a per-texture colour space for Ptex: `half` /
`float` samples keep their full range, but every `.ptx` is decoded by gamma 2.2, so
linear Ptex data is mis-decoded. Tangents on instanced geometry and UV charts on
subdivided meshes are no longer gaps.

#### Scenario: A normal map on an instanced mesh

- **WHEN** a normal-mapped material is bound to geometry placed more than once
- **THEN** it shades with the mapped normal, using a tangent computed at the hit from
  the prototype's vertices and the placement's transform
