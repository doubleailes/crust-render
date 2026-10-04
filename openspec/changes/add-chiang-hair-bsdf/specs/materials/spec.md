# Spec Delta

## ADDED Requirements

### Requirement: MaterialX hair is a Chiang fibre BSDF

A live `chiang_hair_bsdf` leaf SHALL scatter over the whole sphere as Chiang 2016's
fibre model: R, TT, TRT and TRRT+ lobes. It SHALL read MaterialX's inputs as
follows. Each lobe's tint scales that lobe (TRRT+ takes TRT's). Each roughness
is a (longitudinal variance, azimuthal scale) pair clamped to [0.001, 1].
`absorption_coefficient` is per unit radius. `cuticle_angle` maps from [0, 1] to
[−π/2, π/2]. The fibre direction SHALL be the leaf's `curve_direction`, or
else the hit's tangent.

#### Scenario: A backlit tuft glows

- **WHEN** a tuft of curves with a `chiang_hair_bsdf` of zero absorption is lit
  only from behind, relative to the camera
- **THEN** the strands render bright from forward (TT) scattering, where the
  same curves with an `oren_nayar_diffuse_bsdf` render dark

#### Scenario: A clear fibre conserves energy

- **WHEN** a `chiang_hair_bsdf` with zero absorption and white tints is
  integrated over all incident directions, for any outgoing direction and any
  roughness, by uniform sphere sampling and by the leaf's own sampling
- **THEN** both estimates of the directional albedo lie within [0.95, 1.05]

#### Scenario: Sampling agrees with evaluation

- **WHEN** many samples are drawn from a `chiang_hair_bsdf` at a fixed outgoing
  direction
- **THEN** each sample's weight `f·|cos|/pdf` equals the leaf's tints times its
  attenuation, and the histogram of sampled directions matches the reported
  pdf over the whole sphere

#### Scenario: Absorption colours the hair

- **WHEN** `absorption_coefficient` is (0.2, 0.6, 1.2), lit by a white furnace
- **THEN** the strand's colour is warm, with red > green > blue, and every
  channel stays below the clear fibre's

#### Scenario: The cuticle tilt moves the primary highlight

- **WHEN** `cuticle_angle` rises above 0.5, with the fibre and the view held
  fixed
- **THEN** the R lobe's peak moves along the fibre in the direction MaterialX's
  GLSL implementation moves it, and the TRT lobe's peak moves the opposite way
  by about twice as much

#### Scenario: Hair combines like any other leaf

- **WHEN** a `mix` blends a `chiang_hair_bsdf` with an `oren_nayar_diffuse_bsdf`
  at `mix = 0.25`
- **THEN** `examples/mtlx_shade` lists both leaves, with weights 0.25 and 0.75,
  and the material passes the furnace and sampling-agreement checks that every
  closure tree passes

### Requirement: A hair vertex's rays pass out of its own strand

A continuation ray, or a shadow ray toward a sampled light, that leaves a
vertex whose closure holds a live `chiang_hair_bsdf` SHALL NOT be stopped where
it leaves a curve's tube. Where it enters a tube, it SHALL be stopped as before.
Rays leaving any other vertex SHALL be unchanged.

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
- **THEN** its refracted rays meet the tube's far wall, and the image is
  bit-identical to the one rendered before this change

### Requirement: MaterialX hair helper nodes evaluate as MaterialX's GLSL reference

`chiang_hair_roughness`, `chiang_hair_absorption_from_color` and
`deon_hair_absorption_from_melanin` SHALL evaluate as MaterialX 1.39's genglsl
implementations do, to within 1e-5 relative in every lane, including when inputs
are left unauthored. MaterialX's genosl implementations of these nodes are
placeholders, so the reference is the genglsl code, not the OSL oracle.

#### Scenario: Roughness from artist parameters

- **WHEN** `chiang_hair_roughness` is evaluated with `longitudinal = 0.3`,
  `azimuthal = 0.5` and default scales
- **THEN** `roughness_R`, `roughness_TT` and `roughness_TRT` equal genglsl's
  values. TT's variance is ¼ and TRT's is 4× R's, and all three share R's
  azimuthal scale

#### Scenario: Absorption from a colour

- **WHEN** `chiang_hair_absorption_from_color` is evaluated for white, and for
  (0.6, 0.4, 0.2) at `azimuthal_roughness = 0.3`
- **THEN** white gives zero absorption, and the colour gives genglsl's value,
  with its absorption largest in blue and smallest in red

#### Scenario: Melanin

- **WHEN** `deon_hair_absorption_from_melanin` is evaluated with
  `melanin_concentration = 0` and with `melanin_concentration = 0.9`
- **THEN** the first gives zero absorption, and the second gives an absorption
  with blue > green > red, as genglsl does

#### Scenario: The hair reference cases pass

- **WHEN** `cargo test -p crust-mtlx --test hair_helpers` runs over the committed
  reference table
- **THEN** every lane of every output matches the reference
