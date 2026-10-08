# Spec Delta

## MODIFIED Requirements

### Requirement: Supported shading models

The engine SHALL shade every surface through one of three models. The first is
**`OpenPBR`**, a single übershader covering diffuse, metal, glass/transmission,
coat, fuzz, thin-film and subsurface, with `diffuse` / `metal` / `glass` /
`glossy` Rust-side preset constructors. Its fuzz SHALL be a Zeltner sheen layer
whose attenuation of the layers beneath depends on the view direction. The second
is **`Emissive`**, a pure emitter with no geometry knowledge. The third is a
**MaterialX closure tree**, the BSDF leaves and `layer` / `mix` / `add` /
`multiply` combinators a `.mtlx` material compiles to. `PreviewSurface` (a
textured `UsdPreviewSurface`) SHALL evaluate its inputs per shading point and
delegate the BSDF to the `OpenPBR` it resolves to. `MtlxMaterial` (a MaterialX
graph) SHALL evaluate its pattern graph per shading point and shade with its
closure tree. It SHALL NOT pool that tree onto a single `OpenPBR`.

#### Scenario: A model is selected for a surface

- **WHEN** a surface is assigned a material
- **THEN** rays scatter according to `OpenPBR`'s layered BSDF and parameters,
  or according to the MaterialX closure tree the material compiled to, or the
  surface is a pure `Emissive` light

#### Scenario: A MaterialX graph keeps its leaves apart

- **WHEN** a `.mtlx` material mixes two `dielectric_bsdf`s of roughness 0.05 and
  0.8 at `mix = 0.5`
- **THEN** its reflection shows both a sharp and a broad highlight, and
  `examples/mtlx_shade` lists two dielectric leaves with their own roughness,
  rather than one lobe at an averaged roughness

### Requirement: Shared microfacet BRDF helpers

`OpenPBR` lobes SHALL share GGX helpers from `material/brdf.rs`: anisotropic
visible-normal (VNDF) GGX sampling and its PDF, Schlick/F82 Fresnel, EON
(energy-preserving Oren-Nayar) diffuse, Zeltner LTC sheen, thin-film, and Cauchy
dispersion. The Zeltner sheen SHALL be the one
implementation used by both `OpenPBR`'s fuzz and MaterialX's `zeltner` sheen.

#### Scenario: Microfacet lobe samples a direction

- **WHEN** a GGX-based lobe of `OpenPBR` scatters a ray
- **THEN** the outgoing direction is drawn via VNDF sampling and weighted by
  Fresnel and geometry terms

#### Scenario: Both Zeltner sheens agree

- **WHEN** an `OpenPBR` with `fuzz_weight = 1`, `fuzz_color = (1, 1, 1)` and
  `fuzz_roughness = r` and a MaterialX `sheen_bsdf` in `zeltner` mode with
  `weight = 1`, `color = (1, 1, 1)` and `roughness = r` are evaluated for the
  same view and light directions
- **THEN** their sheen contributions are equal

### Requirement: Unrepresentable and approximated MaterialX inputs are reported

The material SHALL be reported with one `WARN` line per material, naming the
inputs or closures, in three cases:

- **Unrepresentable input, ignored.** An input is authored away from its nodedef
  default (connected, or given a differing value) and the tree cannot represent
  it. This is `gltf_pbr`'s `occlusion`.
- **Input the MaterialX graph itself ignores.** An input authored away from
  its default that the node's own nodegraph does not read. These are
  `gltf_pbr`'s `dispersion` and `thickness`, `standard_surface`'s
  `transmission_depth`, `transmission_scatter` and `transmission_dispersion`,
  and `open_pbr_surface`'s `transmission_dispersion_scale`.
- **Approximated closure, kept.** A closure the renderer approximates is live,
  meaning its weight is not the literal 0. No closure is currently in this case.
  `sheen_bsdf` in either mode and `subsurface_bsdf` are not approximated and
  SHALL NOT be reported.

Opacity (`opacity`, `geometry_opacity`, `alpha`, `alpha_mode`,
`alpha_cutoff`) and anisotropy rotation (`specular_rotation`,
`coat_rotation`, `anisotropy_rotation`) are applied and SHALL NOT be reported.
An input left at its default, or a closure pruned at weight 0, SHALL NOT be
reported.

#### Scenario: Authored opacity is applied, not reported

- **WHEN** a `standard_surface` authors `opacity = (0.3, 0.3, 0.3)`
- **THEN** no warning is logged for that material and the surface is a cutout

#### Scenario: Default-valued inputs are silent

- **WHEN** a `gltf_pbr` authors `alpha = 1` and `alpha_mode = 0` explicitly and
  no sheen
- **THEN** no warning is logged for that material

#### Scenario: A live fuzz layer reports its sheen approximation

- **WHEN** an `open_pbr_surface` authors `fuzz_weight = 0.5`
- **THEN** no warning is logged for that material: its `zeltner` sheen is
  evaluated as Zeltner, not approximated

#### Scenario: A live subsurface is silent

- **WHEN** an `open_pbr_surface` authors `subsurface_weight = 1`
- **THEN** no warning is logged for that material

## ADDED Requirements

### Requirement: OpenPBR fuzz reflects its view-dependent albedo

`OpenPBR`'s fuzz SHALL reflect `fuzz_weight · fuzz_color · R(θ_o, fuzz_roughness)`
of the light arriving from a white environment. R is the directional albedo of
Disney's Zeltner sheen "Volume" table, read bilinearly in cos θ_o and
`fuzz_roughness`, with no roughness floor. The fuzz SHALL be importance-sampled
exactly: a sample of the lobe carries the weight `fuzz_color · R`, with no
variance from the lobe's shape.

#### Scenario: A smooth fuzz is a rim

- **WHEN** an `OpenPBR` with only a white fuzz (`base_weight = 0`,
  `specular_weight = 0`, `fuzz_weight = 1`, `fuzz_roughness = 0.3`) is viewed
  head-on in a white furnace
- **THEN** it reflects 0.0008 ± 0.0002, and at cos θ_o = 0.25 it reflects
  0.166 ± 0.002

#### Scenario: A rough fuzz reflects a third of the light head-on

- **WHEN** the same material has `fuzz_roughness = 1` and is viewed head-on
- **THEN** it reflects 0.342 ± 0.002

#### Scenario: Fuzz samples are exact

- **WHEN** that material's BSDF is sampled at a view where R is at least 0.05
- **THEN** the median sample weighs its directional albedo R, to within 0.2%
  (samples near an absent lobe's peak meet the selection share the material
  keeps for it)

### Requirement: OpenPBR fuzz attenuates the layers beneath by its albedo

Below a fuzz of weight `w`, every layer of `OpenPBR` (coat, specular, metal,
diffuse, transmission) and its emission SHALL be scaled by `1 − w · R(θ_o)`.
θ_o is the view angle, and the scale does not depend on the light direction. A
fuzz SHALL raise the coat's roughness as Adobe's reference does. Unlike Adobe's,
the fuzz SHALL stay present when a surface that is not thin-walled is hit from
its back.

#### Scenario: A fuzz over a white diffuse conserves energy

- **WHEN** an `OpenPBR` with a white diffuse base, `specular_weight = 0`, and a
  white fuzz of any weight and roughness is placed in a white furnace
- **THEN** the reflected radiance does not exceed the environment's at any view
  angle

#### Scenario: Emission dims behind a fuzz

- **WHEN** an `OpenPBR` with `emission_luminance > 0`, no coat, and a fuzz of
  weight 1 and roughness 1 is viewed head-on
- **THEN** its emission is scaled by 1 − 0.342 (to within 0.002)

#### Scenario: The back of an open cloth mesh keeps its fuzz

- **WHEN** a single-sided mesh that is not authored thin-walled, with a fuzz, is
  seen from its back
- **THEN** its fuzz shades as it does from the front

#### Scenario: No fuzz leaves the image unchanged

- **WHEN** a scene contains no `OpenPBR` with `fuzz_weight > 0` and no `zeltner`
  sheen
- **THEN** its image is bit-identical to the image before this change

### Requirement: Native OpenPBR is checked against Adobe's reference

`OpenPBR` SHALL be compared with Adobe's `openpbr-bsdf` reference at a pinned
commit, through a committed fixture of reference values: BSDF value,
directional albedo and emission, over sampled parameters and directions. It
SHALL match within a stated tolerance, except where named deviation rules
excuse the difference. Each rule SHALL name its input condition and
measured bound.

#### Scenario: The fuzz matches the reference

- **WHEN** the oracle replays a fixture case whose only active layer is a fuzz
  over a black base
- **THEN** crust's values, albedo and emission match the reference within
  tolerance, with no deviation rule applied

#### Scenario: A known gap is excused only under its condition

- **WHEN** a fixture case differs from crust because of a recorded gap (for
  example, a rough metal, which lacks multiple-scattering compensation)
- **THEN** the difference is excused only by the rules whose conditions hold
  for that case, and only up to the sum of their recorded bounds

#### Scenario: A closed gap leaves no excuse behind

- **WHEN** every fixture case passes with one deviation rule removed
- **THEN** the oracle fails, naming that rule as stale

#### Scenario: CI needs no reference toolchain

- **WHEN** `cargo test --workspace` runs
- **THEN** the oracle replays the committed fixture without compiling or
  fetching Adobe's sources
