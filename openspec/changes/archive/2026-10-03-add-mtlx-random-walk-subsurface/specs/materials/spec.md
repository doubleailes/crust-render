## ADDED Requirements

### Requirement: MaterialX subsurface is a random walk

A live `subsurface_bsdf` leaf with a positive radius in any channel SHALL be
rendered as a random walk through the object it belongs to: selecting the leaf
SHALL refract the path into the surface through the dielectric layered over the
leaf (IOR 1.5 and roughness 0.5 with none), walk the interior of that geometry
alone with the leaf's `color` as the target albedo, `radius` as the per-channel
mean free path and `anisotropy` as the phase anisotropy, and continue the path
from the walk's exit on a white Lambertian weighted by the walk's throughput.
The leaf SHALL contribute nothing to light sampled toward a direction at the
entry. A walk that finds no exit SHALL end the path. A leaf whose radius is
zero in every channel SHALL shade as a diffuse in its colour.

#### Scenario: A backlit sphere glows at its rim

- **WHEN** an `open_pbr_surface` with `subsurface_weight = 1` and a radius a
  sizeable fraction of the object's size is lit from behind
- **THEN** light that entered on the lit side leaves on the camera side, and
  the silhouette is brighter than a diffuse of the same colour renders it

#### Scenario: A short walk keeps its colour's balance

- **WHEN** a `subsurface_bsdf` of colour (0.8, 0.5, 0.2) and radius 0.01 on a
  unit sphere sits in a white furnace
- **THEN** it reflects (0.78, 0.45, 0.16) within 0.025 per channel

#### Scenario: Other objects are not the walk's boundary

- **WHEN** a smaller object is embedded inside a subsurface object
- **THEN** walks pass through it and exit only through the subsurface object's
  own surface

## MODIFIED Requirements

### Requirement: Unrepresentable and approximated MaterialX inputs are reported

The material SHALL be reported with one `WARN` line per material, naming the
inputs or closures, in three cases:

- **Unrepresentable input, ignored.** An input is authored away from its nodedef
  default (connected, or given a differing value) and the tree cannot represent
  it. These are `opacity`, `geometry_opacity`, `alpha`, `alpha_mode` (no
  cutout), `specular_rotation`, `coat_rotation`, `anisotropy_rotation` and
  `occlusion`.
- **Input the MaterialX graph itself ignores.** An input authored away from
  its default that the node's own nodegraph does not read. These are
  `gltf_pbr`'s `dispersion` and `thickness`, `standard_surface`'s
  `transmission_depth`, `transmission_scatter` and `transmission_dispersion`,
  and `open_pbr_surface`'s `transmission_dispersion_scale`.
- **Approximated closure, kept.** A closure the renderer approximates is live,
  meaning its weight is not the literal 0. This is `sheen_bsdf` in `zeltner`
  mode (evaluated as Charlie). `subsurface_bsdf` is not approximated and SHALL
  NOT be reported.

An input left at its default, or a closure pruned at weight 0, SHALL NOT be
reported.

#### Scenario: Authored opacity is reported, not applied

- **WHEN** a `standard_surface` authors `opacity = (0.3, 0.3, 0.3)`
- **THEN** the surface renders opaque and the log carries one `WARN` for that
  material naming `opacity`

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
