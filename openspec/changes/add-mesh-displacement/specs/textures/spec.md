# Spec Delta

## ADDED Requirements

### Requirement: Ptex requests carry a colour space

Every Ptex request SHALL carry a colour space, as UV texture requests do. A Ptex file
read as a displacement map SHALL be decoded raw, with no transfer curve. A Ptex file read
as a colour map SHALL keep the gamma-2.2 decode. The preloaded and streamed paths SHALL
agree bit for bit on 8-bit data in either colour space.

#### Scenario: An 8-bit displacement Ptex

- **WHEN** a `u8` `.ptx` holding the value 128 is read as displacement
- **THEN** the lookup returns 128/255, not (128/255)^2.2

#### Scenario: The island's colour Ptex

- **WHEN** a material's `inputs:surfaceMap` Ptex is rendered before and after this change
- **THEN** the images are bit-identical

## MODIFIED Requirements

### Requirement: Known gaps

The following SHALL be documented as unsupported: anisotropic filtering,
filtering across Ptex face boundaries, texture alpha, `UsdTransform2d`, UV sets
other than `st` and its fallbacks, and an authored colour space for Ptex colour maps:
`half` / `float` samples keep their full range, but every `.ptx` read as colour is
decoded by gamma 2.2, so linear colour Ptex data is mis-decoded. Ptex read as
displacement is decoded raw. Tangents on instanced geometry narrow to prototype
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
