# Spec Delta

## ADDED Requirements

### Requirement: Mass lookup nodes read the field at the hit

The MaterialX nodes `crust_mass_color` (`color3`), `crust_mass_normal`
(`vector3`, world space, unit length), `crust_mass_depth` (`float`, world
units) and `crust_mass_density` (`float`, coverage) SHALL read the mass field
of the geometry hit at the hit's position. On geometry without a field, a
field without a colour for `crust_mass_color`, or a point where the field
defines no normal for `crust_mass_normal`, each SHALL return its `default`
input, with no warning.

#### Scenario: A lookup off the field

- **WHEN** a material reading `crust_mass_normal` with `default` (0, 0, 1) is
  bound to a mesh
- **THEN** every hit reads (0, 0, 1), and the import raises no warning

#### Scenario: The interpreter and the JIT agree

- **WHEN** a program reading all four lookups is run on the same hits by the
  interpreter and by the JIT
- **THEN** every output is bit-identical between the two

### Requirement: A mass diffuse leaf shades around a normal it is given

`crust_mass_diffuse_bsdf` (`weight`, `color`, `normal`) SHALL be a Lambertian
leaf around its `normal`, sampled by the cosine about it with the pdf its
evaluation reports. Under a uniform white environment it SHALL return `color`
exactly. It is a stylisation leaf: its normal need not be the surface's, so it
is not reciprocal against the geometry.

#### Scenario: White furnace

- **WHEN** a `crust_mass_diffuse_bsdf` of `color` (0.8, 0.5, 0.2) is lit by a
  uniform white environment
- **THEN** its directional albedo is (0.8, 0.5, 0.2) for every outgoing
  direction in its normal's hemisphere

#### Scenario: Sampling agrees with evaluation

- **WHEN** many samples are drawn from the leaf with a normal tilted away from
  the hit's
- **THEN** the histogram of sampled directions matches the reported pdf

## MODIFIED Requirements

### Requirement: Per-leaf shading normal and tangent

Every BSDF leaf SHALL shade in its own frame, built from that leaf's `normal`
input (and `tangent`, where it has one), under the rule a MaterialX `normal`
follows today:

- The input is normalised.
- A normal facing away from the geometric normal is ignored rather than flipping
  the surface, except on `crust_mass_diffuse_bsdf`, which keeps its normal
  whichever way it faces and ignores it only when it is not finite or has zero
  length.
- An unconnected input takes the interpolated shading normal and tangent.

A surface node's normal inputs SHALL reach the leaves its nodegraph routes them
to. `geometry_normal` / `normal` goes to the base leaves, and `geometry_coat_normal`
/ `coat_normal` / `clearcoat_normal` to the coat leaf.

#### Scenario: A coat normal perturbs only the coat

- **WHEN** an `open_pbr_surface` connects `geometry_coat_normal` to a
  `normalmap` and leaves `geometry_normal` unconnected
- **THEN** the probe shows the coat leaf's normal perturbed and the base leaves'
  normal equal to the interpolated normal

#### Scenario: An authored tangent orients anisotropy

- **WHEN** a `gltf_pbr` with `anisotropy_strength = 0.8` connects `tangent`
  to a constant `(0, 1, 0)` in tangent space
- **THEN** the highlight's stretch follows that direction rather than the
  mesh's `dPdu`

#### Scenario: The mass leaf keeps an opposed normal

- **WHEN** a curve hit's `crust_mass_diffuse_bsdf` is given a normal opposite
  the tube's normal there, and lit from that normal's side
- **THEN** the leaf responds as lit around the given normal, and differs from
  the same leaf given the tube's normal

### Requirement: A hair vertex's rays pass out of its own strand

A continuation ray or a shadow ray that carries a live `chiang_hair_bsdf` or
`crust_mass_diffuse_bsdf` leaf's light SHALL NOT be stopped where it leaves a
curve's tube; where it enters one, it SHALL be stopped as before. Light any
other leaf at the same vertex scatters SHALL meet the tube as it would without
those leaves, whether that leaf transmits or reflects about a normal of its
own, and rays leaving any vertex without them SHALL be unchanged.

#### Scenario: Light reaches a strand through the strand

- **WHEN** a single strand with a `chiang_hair_bsdf` stands between the camera
  and a small light directly behind it
- **THEN** the light is sampled through the strand, and the strand shows its TT
  glow, rather than being shadowed by its own far wall

#### Scenario: Hair still shadows

- **WHEN** a tuft of hair stands between a light and a floor
- **THEN** the floor beneath it is shadowed, as it is with any other material
  on the curves

#### Scenario: Glass curves still refract

- **WHEN** a curve carries a transmissive `dielectric_bsdf` and no hair leaf
- **THEN** its refracted rays meet the tube's far wall from inside, exactly as
  a ray that does not pass out of curve tubes does

#### Scenario: Only the fibre's light passes out of a mixed strand

- **WHEN** a strand mixes a clear `chiang_hair_bsdf` half and half with a
  white `translucent_bsdf` and is lit from straight behind
- **THEN** with one bounce it shows half the clear fibre's glow, under light
  sampling and under BSDF sampling alike: the translucent half's light meets
  the tube's far wall rather than passing out of the strand

#### Scenario: A tilted reflecting leaf beside a fibre meets its tube

- **WHEN** a strand mixes a clear `chiang_hair_bsdf` half and half with an
  `oren_nayar_diffuse_bsdf` whose `normal` is tilted into the tube, and is lit
  from behind
- **THEN** the diffuse half's light that enters the tube meets its far wall,
  as it would on its own, while the fibre half's passes out of the strand

#### Scenario: A mass-lit strand lit from behind its own tube

- **WHEN** a strand's only leaf is a `crust_mass_diffuse_bsdf` whose normal
  points away from the camera, toward a light behind the strand
- **THEN** the strand is lit as the mass's surface would be, not shadowed by
  its own tube
