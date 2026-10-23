## MODIFIED Requirements

### Requirement: Shared microfacet BRDF helpers

`OpenPBR` lobes SHALL share GGX helpers from `material/brdf.rs`: anisotropic
visible-normal (VNDF) GGX sampling and its PDF, Schlick/F82 Fresnel, EON
(energy-preserving Oren-Nayar) diffuse, Zeltner LTC sheen, thin-film, and Cauchy
dispersion. The Zeltner sheen SHALL be the one
implementation used by both `OpenPBR`'s fuzz and MaterialX's `zeltner` sheen.
MaterialX's `zeltner` sheen SHALL read it at BSDL's sheen roughness,
`max(√roughness, 0.02)`; `OpenPBR`'s fuzz reads it at `fuzz_roughness`.

#### Scenario: Microfacet lobe samples a direction

- **WHEN** a GGX-based lobe of `OpenPBR` scatters a ray
- **THEN** the outgoing direction is drawn via VNDF sampling and weighted by
  Fresnel and geometry terms

#### Scenario: Both Zeltner sheens agree

- **WHEN** an `OpenPBR` with `fuzz_weight = 1`, `fuzz_color = (1, 1, 1)` and
  `fuzz_roughness = max(√r, 0.02)` and a MaterialX `sheen_bsdf` in `zeltner`
  mode with `weight = 1`, `color = (1, 1, 1)` and `roughness = r` are
  evaluated for the same view and light directions
- **THEN** their sheen contributions are equal

### Requirement: MaterialX closure-tree semantics

A MaterialX material SHALL be evaluated as its closure tree with the combinator
semantics of MaterialX's OSL backend as OSL's `testrender` renders it, at every
shading point:

- `mix(fg, bg, m)` SHALL be `m · fg + (1 − m) · bg`, with `m` clamped to [0, 1].
- `add(a, b)` SHALL be `a + b`.
- `multiply(x, w)` SHALL be `w · x`, per channel for a `color3` weight.
- `layer(top, base)` SHALL be `f_top + f_base · (1 − clamp(O_top(ωo), 0, 1))`,
  per channel, where `O_top` is the top sub-tree's opacity toward the outgoing
  direction ωo.

The opacity of a sub-tree SHALL be linear in every weight and SHALL be clamped
only where a `layer` uses it:

- a leaf SHALL contribute `weight · E(ωo)` if it lets light through to a base,
  where `E` is its directional albedo, and `weight` otherwise. The leaves that
  let light through are a `dielectric_bsdf` or `generalized_schlick_bsdf` that
  only reflects, and a `sheen_bsdf`. Every other leaf is opaque, including a
  `dielectric_bsdf` or `generalized_schlick_bsdf` that refracts, whatever its
  weight;
- a `mix` SHALL blend its branches' opacities by `m` and `1 − m`;
- an `add` SHALL sum its branches' opacities;
- a `multiply` SHALL scale its input's opacity by its weight;
- a `layer` inside a top SHALL contribute the sum of its own top's and base's
  opacities, neither one attenuated by the other.

A leaf whose weight is exactly 0, and a mix whose factor is exactly 0 or 1,
SHALL contribute nothing from the pruned branch, neither response nor opacity.
Importance sampling, `eval` and the pdf SHALL describe the same distribution
for every tree. A layer SHALL never reflect more energy than its top and base
would separately.

#### Scenario: A coat dims what lies beneath it at grazing angles

- **WHEN** a `layer` puts a smooth `dielectric_bsdf` (IOR 1.5) over an
  `oren_nayar_diffuse_bsdf`
- **THEN** `examples/mtlx_shade` reports the diffuse leaf's resolved weight as
  `1 − E_dielectric(ωo)`, lower at a grazing ωo than at normal incidence

#### Scenario: A weighted opaque top covers only its weight

- **WHEN** a `layer` puts an `oren_nayar_diffuse_bsdf` of weight 0.3 over a
  `conductor_bsdf`
- **THEN** `examples/mtlx_shade` reports the conductor leaf's resolved weight
  as 0.7 at every ωo

#### Scenario: A multiplied top scales its opacity

- **WHEN** a `layer` puts `multiply(conductor_bsdf, 0.25)` over an
  `oren_nayar_diffuse_bsdf`
- **THEN** the diffuse leaf's resolved weight is 0.75

#### Scenario: A refracting top is opaque

- **WHEN** a `layer` puts a `dielectric_bsdf` in `RT` scatter mode, weight 1,
  over an `oren_nayar_diffuse_bsdf`
- **THEN** the diffuse leaf's resolved weight is 0

#### Scenario: A layer nested in a top sums its opacities

- **WHEN** a `layer` puts `layer(dielectric A, dielectric B)` over a base, both
  dielectrics reflecting only, with albedos `E_A` and `E_B` toward ωo
- **THEN** the base's resolved weight is `1 − clamp(E_A + E_B, 0, 1)`, as
  testrender computes it, and not `(1 − E_A)(1 − E_B)`

#### Scenario: The opacity rule matches testrender's

- **WHEN** the closure tests evaluate a fixed set of trees (every combinator
  in a top, weights in (0, 1), opacities summing above 1)
- **THEN** each base weight equals the value of a direct transcription of
  testrender's layer-opacity walk on the same tree, within float rounding

#### Scenario: Sampling agrees with evaluation

- **WHEN** the tree of each fixture material is tested by drawing many BSDF
  samples at a fixed ωo
- **THEN** the sampled directions' histogram matches the reported pdf, and the
  estimator `f·cos/pdf` averages to the tree's directional albedo, within
  Monte Carlo tolerance

#### Scenario: A layered surface does not create energy

- **WHEN** any fixture material is lit by a uniform white environment
- **THEN** no pixel of a non-emissive surface is brighter than the environment
  (furnace test)

### Requirement: MaterialX transmission and interior media

A MaterialX closure tree whose leaves can transmit SHALL refract light through
them. This covers a `dielectric_bsdf` in `T` or `RT` scatter mode, and a
surface node whose transmission weight is not the literal 0. A ray refracted
into a thick (not thin-walled) surface SHALL carry the interior medium the
material defines:

- a surface node's transmission colour, depth and scatter, converted as the
  MaterialX volume graph converts them, where the node's nodegraph builds a
  volume (`open_pbr_surface`; `standard_surface`'s graph builds none);
- `gltf_pbr`'s attenuation;
- or a `vdf` input.

A thin-walled surface SHALL transmit without a medium. A `dielectric_bsdf` in
`T` mode SHALL weight its transmission by its own `(1 − F)`, as BSDL does: it
is the reflect-and-refract lobe with its reflection tinted black, so that a
reflection layer over it attenuates only by the layer's opacity and no surface
returns more than it receives. A refracting `dielectric_bsdf` SHALL renormalise its single scattering as
BSDL does, by `1 / max(0.01, 1 − E_ms)` with `E_ms` from BSDL's
reflect-and-refract tables of its face, baked in BSDL's default configuration,
and SHALL be sampled as BSDL samples it (bounded VNDF), the sampler those
tables measure. Its value then integrates above 1 at high roughness while its
BSDF-sampled estimate returns what it receives; with light sampling and MIS
a small excess remains, as in testrender.

#### Scenario: A glass is bounded in a white furnace

- **WHEN** a `standard_surface` glass sphere is rendered unclamped inside a
  uniform emitter of radiance 1
- **THEN** no pixel, head-on or at grazing incidence, exceeds 1

#### Scenario: A rough glass is bounded in a white furnace

- **WHEN** a clear `dielectric_bsdf` sphere in `RT` mode, IOR 1.5, at
  roughness 0.5 and 1, is rendered unclamped inside a uniform emitter of
  radiance 1, with and without that emitter as a light
- **THEN** its mean is at most 1.02 without light sampling and at most 1.04
  with it

#### Scenario: A rough clear dielectric matches BSDL's albedo

- **WHEN** a lone `dielectric_bsdf` in `RT` mode, IOR 1.5, untinted, is
  integrated over all directions at roughness 0.2, 0.5 and 1 and at several ωo,
  on both faces
- **THEN** its reflected plus transmitted albedo equals BSDL's within the
  oracle's tolerance, which is 1 when smooth and exceeds 1 as roughness grows
  (about 1.04 at roughness 0.5 and 1.75 at roughness 1, head on)

#### Scenario: A standard_surface glass is transparent

- **WHEN** a `standard_surface` authors `transmission = 1` and
  `specular_roughness = 0` and is rendered in front of the dome
- **THEN** the dome is visible, refracted, through the object

#### Scenario: Depth absorbs inside the object

- **WHEN** a `gltf_pbr` authors `transmission = 1`, `thickness = 1`,
  `attenuation_color = (1, 0.2, 0.2)` and `attenuation_distance = 0.5`
- **THEN** light through thicker parts of the object is more strongly
  red-tinted than light through thinner parts

## ADDED Requirements

### Requirement: MaterialX BSDF leaves are checked against BSDL

Each MaterialX BSDF leaf except `subsurface_bsdf` and `chiang_hair_bsdf` SHALL
match BSDL's lobe from upstream OSL at a pinned commit: its BSDF value, its
directional albedo and its layering opacity, through a committed fixture of
reference values over sampled parameters and directions, within a stated
tolerance. Named deviation rules SHALL excuse a difference, each with its input
condition and measured bound.

#### Scenario: Every leaf is replayed

- **WHEN** the BSDL oracle test runs
- **THEN** it replays fixture cases for `dielectric_bsdf` (`R`, `T`, `RT`, both
  faces), `conductor_bsdf`, `generalized_schlick_bsdf` (`R` and `RT`),
  `sheen_bsdf` (`conty_kulla` and `zeltner`), `oren_nayar_diffuse_bsdf` (with
  and without energy compensation), `burley_diffuse_bsdf` and
  `translucent_bsdf`, and each matches within tolerance

#### Scenario: A known gap is excused only under its condition

- **WHEN** a fixture case differs from crust because of a recorded deviation
- **THEN** the difference is excused only by the rules whose conditions hold
  for that case, and only up to the sum of their recorded bounds

#### Scenario: A closed gap leaves no excuse behind

- **WHEN** every fixture case passes with one deviation rule removed
- **THEN** the oracle fails, naming that rule as stale

#### Scenario: CI needs no reference toolchain

- **WHEN** `cargo test --workspace` runs
- **THEN** the oracle replays the committed fixture without compiling or
  fetching OSL's sources

### Requirement: A reflection-only top lets through only what it does not reflect

Under a back face, a `dielectric_bsdf` or `generalized_schlick_bsdf` that only
reflects SHALL let a layer's base through in the share its own lobe does not
reflect, which is nothing past the critical angle, where BSDL lets through
what the front face would. The BSDL oracle SHALL excuse the difference under a
named deviation rule.

#### Scenario: A coat over glass, from inside

- **WHEN** a `layer` whose top is a reflection-only `dielectric_bsdf` and whose
  base is a refracting `dielectric_bsdf` of the same IOR is shaded under a
  back face, at and past the critical angle
- **THEN** its directional albedo exceeds 1 by no more than the back-face
  table's interpolation allows (0.07)

#### Scenario: The filter is the leaf's own

- **WHEN** a reflection-only `dielectric_bsdf` over a white diffuse base is
  shaded under a back face around the critical angle
- **THEN** one minus the weight its base receives matches the leaf's own
  directional albedo within 0.07

### Requirement: MaterialX leaves are sampled as BSDL samples them

A MaterialX leaf SHALL be sampled as BSDL samples it, with BSDL's density
toward every direction: a microfacet leaf by bounded VNDF (reflected, or
reflected with probability `P = avg F` and refracted otherwise), a sheen
uniformly (`conty_kulla`) or by its LTC (`zeltner`), and the diffuse and
translucent leaves cosine-weighted. A thin-walled leaf that transmits MAY
sample otherwise, and the choice among leaves SHALL stay crust's.

#### Scenario: Every leaf's density is BSDL's

- **WHEN** the BSDL oracle test runs
- **THEN** each leaf's density toward each fixture light matches BSDL's
  `eval_impl` pdf within tolerance, outside the named deviations

### Requirement: The walk is crust's own, not testrender's

Shading SHALL NOT roughen a lobe by the path's previous roughness, as
testrender does. `subsurface_bsdf` SHALL stay a random walk,
`chiang_hair_bsdf` MaterialX's GLSL fibre BSDF, and a thin film on a
dielectric or conductor MaterialX's GLSL Airy film, which BSDL ignores.

#### Scenario: Roughness does not depend on the path

- **WHEN** a smooth `conductor_bsdf` is seen directly and through a bounce off
  a rough diffuse surface
- **THEN** it reflects with the same roughness both times

#### Scenario: A thin film still interferes

- **WHEN** a `dielectric_bsdf` authors `thinfilm_thickness = 500` and
  `thinfilm_ior = 1.3`
- **THEN** its reflection is coloured by the film, and the oracle excuses it
  under a thin-film deviation rule

#### Scenario: Subsurface is not testrender's Lambertian

- **WHEN** a `subsurface_bsdf` is rendered
- **THEN** it is shaded by the random walk, unchanged by this BSDL port
