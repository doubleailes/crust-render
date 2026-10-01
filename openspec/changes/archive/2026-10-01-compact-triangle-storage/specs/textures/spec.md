# Spec Delta

## MODIFIED Requirements

### Requirement: Known gaps

The following SHALL be documented as unsupported: anisotropic filtering,
filtering across Ptex face boundaries, texture alpha, `UsdTransform2d`, UV sets
other than `st` and its fallbacks, and a per-texture colour space for Ptex: `half` /
`float` samples keep their full range, but every `.ptx` is decoded by gamma 2.2, so
linear Ptex data is mis-decoded. Tangents on instanced geometry narrow to prototype
parts placed through an instancer's group and to motion-blurred instances (normal
maps fall back to the geometric normal there); UV charts on subdivided meshes are no
longer a gap.

#### Scenario: A normal map on a directly instanced mesh

- **WHEN** a normal-mapped material is bound to a mesh prim authored twice
- **THEN** it shades with the mapped normal, using a tangent computed at the hit from
  the prototype's vertices and the placement's transform

#### Scenario: A normal map on a prototype placed through an instancer

- **WHEN** a normal-mapped material is bound to a prototype part placed through a
  `PointInstancer` or native-instancing group, or to a motion-blurred instance
- **THEN** it shades with the geometric normal

#### Scenario: A UV texture on a subdivided mesh

- **WHEN** a UV-textured material is bound to a mesh with an authored
  subdivision scheme
- **THEN** the texture is sampled through the refined UV chart rather than
  dropped
