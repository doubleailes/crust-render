## ADDED Requirements

### Requirement: MaterialX opacity is a cutout

A MaterialX surface's opacity SHALL be the `opacity` of the `surface` node its
graph ends in: `open_pbr_surface`'s `geometry_opacity`, the luminance of
`standard_surface`'s `opacity` with MaterialX's default (ACEScg) weights,
`gltf_pbr`'s `alpha` through `alpha_mode` (OPAQUE: 1; MASK: 1 where
`alpha ≥ alpha_cutoff`, else 0; BLEND: `alpha`), and a stdlib `surface`
node's own `opacity`. It SHALL be clamped to [0, 1], with a non-finite value
read as 1. A surface whose opacity folds to 1 at load SHALL have no cutout,
and SHALL cost nothing per hit. Opacity SHALL NOT change the closure tree: a
partly present surface neither refracts nor carries an interior medium
because of it.

#### Scenario: Standard Surface opacity is a luminance

- **WHEN** a `standard_surface` authors `opacity = (0.2, 0.5, 0.8)`
- **THEN** the probe reports an opacity of `0.2722287·0.2 + 0.6740818·0.5 +
  0.0536895·0.8`, the closure does not transmit, and nothing is reported

#### Scenario: glTF MASK keeps or discards each point whole

- **WHEN** a `gltf_pbr` in `alpha_mode = 1` with `alpha_cutoff = 0.5` has an
  `alpha` driven by `u`
- **THEN** its opacity is 0 at `u = 0.3` and 1 at `u = 0.5` and `u = 0.7`

#### Scenario: OPAQUE ignores alpha

- **WHEN** a `gltf_pbr` in `alpha_mode = 0` connects `alpha` to a varying input
- **THEN** the material has no cutout

### Requirement: MaterialX anisotropy rotation turns the tangent

A surface node's rotation of its tangent SHALL turn the shading frame of the
leaves its graph wires the rotated tangent to, about each leaf's own normal,
with the sign of MaterialX's `rotate3d` (Rodrigues' formula at minus its
angle): `standard_surface`'s `specular_rotation` (a fraction of a full turn)
on its specular, transmission and metal leaves and `coat_rotation` on its
coat, each only where the matching anisotropy is above 0; `gltf_pbr`'s
`anisotropy_rotation` (radians, counter-clockwise toward the bitangent) on
every base leaf and not on the clearcoat. The sampled and the evaluated lobe
SHALL use the same turned frame.

#### Scenario: A quarter turn swaps the highlight's axes

- **WHEN** a `standard_surface` metal authors `specular_anisotropy = 0.5` and
  `specular_rotation = 0.25` on a surface whose tangent is +X and normal +Z
- **THEN** the probe reports the conductor leaf's tangent as −Y, and the
  diffuse leaf's as +X

#### Scenario: An isotropic lobe is not turned

- **WHEN** a `standard_surface` authors `specular_rotation = 0.25` and no
  anisotropy
- **THEN** its specular leaf keeps the +X tangent

## MODIFIED Requirements

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
  meaning its weight is not the literal 0. This is `sheen_bsdf` in `zeltner`
  mode (evaluated as Charlie). `subsurface_bsdf` is not approximated and SHALL
  NOT be reported.

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
- **THEN** the log carries one `WARN` naming the `zeltner` sheen as evaluated
  with Charlie

#### Scenario: A live subsurface is silent

- **WHEN** an `open_pbr_surface` authors `subsurface_weight = 1`
- **THEN** no warning is logged for that material
