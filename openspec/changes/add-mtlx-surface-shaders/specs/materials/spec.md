## ADDED Requirements

### Requirement: MaterialX surface-shader nodes

A MaterialX material whose `surfaceshader` is a `standard_surface`,
`open_pbr_surface` or `gltf_pbr` node SHALL render with that node's parameters,
mapped onto `OpenPBR`, and SHALL NOT fall back to the default material. Every
input of the node SHALL take, in order of precedence: its connection, evaluated
per shading point through the same pattern graph evaluation as any other
MaterialX input; its authored `value`; or its MaterialX 1.39 nodedef default,
including the case where a document declares no nodedef. Such a material SHALL
NOT be reported as having an unsupported `standard_surface`,
`open_pbr_surface` or `gltf_pbr` node. A document whose surface is built from
standalone BSDF nodes SHALL render exactly as it did before this requirement.

#### Scenario: A constant standard_surface is no longer the fallback

- **WHEN** a `.mtlx` binds `standard_surface` with `base_color = (0.8, 0.1, 0.1)`
  and no other inputs, and a USD `Material` references it
- **THEN** the surface renders red rather than as the fallback, and no
  "no operator for node type(s) standard_surface" warning is logged

#### Scenario: An input left unauthored takes the nodedef default

- **WHEN** a `standard_surface` authors only `base_color`
- **THEN** the OpenPBR parameters reported by `examples/mtlx_shade` at any point
  equal the mapping of Standard Surface's defaults (`base = 0.8`,
  `specular_roughness = 0.2`, `specular_IOR = 1.5`, …) with that `base_color`

#### Scenario: A connected input varies over the surface

- **WHEN** an `open_pbr_surface`'s `base_color` is connected to a node graph
  driven by `texcoord`
- **THEN** `examples/mtlx_shade` reports a different `base_color` at two
  different `(u, v)`, equal to the graph's output at each point

#### Scenario: A standalone-BSDF document is unaffected

- **WHEN** `samples/materialx_basic.usda` is rendered at 16 spp before and after
  this change
- **THEN** the EXRs are bit-identical

### Requirement: Surface-shader parameter mapping

The mapping from each surface node to `OpenPBR` SHALL be fixed per node type:

- `open_pbr_surface`: each input SHALL set the `OpenPBR` parameter of the same
  name, unconverted, including `coat_darkening` at its authored or default
  value.
- `standard_surface`: parameters SHALL be exactly the outputs of MaterialX's
  published translation graph `ND_standard_surface_to_open_pbr_surface`
  (MaterialX 1.39, `libraries/bxdf/translation/standard_surface_to_open_pbr.mtlx`)
  for the same inputs, including its approximations, among them:
  `base_color` multiplied by `mix(1, coat_color, coat)`; `coat_weight` forced to
  0 where `coat · metalness > 0`; `specular_weight` forced to 1 where
  `metalness > 0`; `fuzz_roughness = sheen_roughness^0.4`; thin-film thickness
  converted from nanometres to micrometres, with `thin_film_weight = 1` exactly
  where the thickness is positive; `geometry_opacity` taken from `opacity`'s
  first channel.
- `gltf_pbr`: parameters SHALL follow the glTF 2.0 metallic-roughness model and
  its `KHR_materials_*` extensions: `base_color`, `metallic` and `roughness` →
  `base_color`, `base_metalness` and `specular_roughness`; `ior`, `specular`
  and `specular_color` → the dielectric specular; `transmission` →
  `transmission_weight`, thin-walled exactly when `thickness` is 0. With an
  authored, finite `attenuation_distance`, `attenuation_color` /
  `attenuation_distance` → `transmission_color` / `transmission_depth`.
  Without one, glTF's infinite distance, the transmission is tinted by
  `base_color` at the interface instead; `clearcoat` / `clearcoat_roughness` → the coat at IOR
  1.5 with no darkening; `sheen_color` / `sheen_roughness` → fuzz;
  `iridescence` / `iridescence_ior` / `iridescence_thickness` → thin film, with
  the thickness converted from nanometres to micrometres; `emissive ·
  emissive_strength` → emission; `dispersion` → transmission dispersion; and
  `anisotropy_strength` → `specular_roughness_anisotropy`.

Emission SHALL NOT be clamped above 1.0.

#### Scenario: open_pbr_surface is one to one

- **WHEN** an `open_pbr_surface` authors `specular_roughness = 0.35`,
  `coat_weight = 0.6` and `coat_ior = 1.6`
- **THEN** `examples/mtlx_shade` reports exactly those three values on the
  OpenPBR parameters, and every unauthored parameter at the OpenPBR 1.1 nodedef
  default

#### Scenario: standard_surface metal keeps full specular weight

- **WHEN** a `standard_surface` authors `metalness = 1` and `specular = 0.25`
- **THEN** the reported `specular_weight` is 1.0, as the translation graph's
  `ifgreater(metalness, 0)` gives it

#### Scenario: standard_surface coat tints the base

- **WHEN** a `standard_surface` authors `base_color = (1, 1, 1)`,
  `coat = 0.5` and `coat_color = (1, 0, 0)`
- **THEN** the reported `base_color` is `(1, 0.5, 0.5)` and `coat_weight`
  is 0.5

#### Scenario: gltf_pbr volume becomes transmission depth

- **WHEN** a `gltf_pbr` authors `transmission = 1`, `thickness = 0.1`,
  `attenuation_color = (0.5, 0.8, 1)` and `attenuation_distance = 2`
- **THEN** the reported parameters have `transmission_weight = 1`,
  `transmission_color = (0.5, 0.8, 1)`, `transmission_depth = 2` and are not
  thin-walled

#### Scenario: gltf_pbr iridescence thickness is converted

- **WHEN** a `gltf_pbr` authors `iridescence = 1` and
  `iridescence_thickness = 400`
- **THEN** the reported `thin_film_weight` is 1 and `thin_film_thickness` is 0.4

### Requirement: Surface-shader shading normal

A surface node's normal input (`normal` for `standard_surface` and `gltf_pbr`,
`geometry_normal` for `open_pbr_surface`) SHALL, when connected, replace the
shading normal for the whole BSDF under the same rule a BSDF node's `normal`
follows today. The normal is normalised, and a normal facing away from the
geometric normal is ignored rather than flipping the surface. An unconnected
normal input SHALL leave the interpolated shading normal unchanged.

#### Scenario: A normal map perturbs a surface shader

- **WHEN** a `standard_surface`'s `normal` is connected to a `normalmap` node
- **THEN** its render shows the normal map's relief, and the probe's shading
  normal differs from the geometric normal where the map is not flat

#### Scenario: An unconnected normal is the geometry's

- **WHEN** a `gltf_pbr` has no `normal` connection
- **THEN** it shades with the mesh's interpolated normal

### Requirement: Surface-shader transmission

A surface-shader material whose transmission weight can be non-zero SHALL
transmit and refract light through `OpenPBR`'s transmission lobe, and rays it
refracts into a thick (not thin-walled) surface SHALL carry that surface's
interior medium, as for an authored `crust:openpbr` glass. A material whose
transmission input is the literal value 0 SHALL pay no extra per-ray cost for
this.

#### Scenario: A standard_surface glass is transparent

- **WHEN** a `standard_surface` authors `transmission = 1`,
  `specular_roughness = 0` and is rendered in front of the dome
- **THEN** the dome is visible, refracted, through the object, rather than the
  object rendering opaque

#### Scenario: Coloured depth absorbs inside the object

- **WHEN** a `gltf_pbr` authors `transmission = 1`, `thickness = 1` and
  `attenuation_color = (1, 0.2, 0.2)` at `attenuation_distance = 0.5`
- **THEN** light passing through thicker parts of the object is more strongly
  red-tinted than through thinner parts

### Requirement: Unrepresentable surface-shader inputs are reported

An input authored away from its nodedef default that the mapping cannot represent
SHALL be ignored and reported with one `WARN` line per material naming the
inputs. Such an input is either authored as a differing value or connected. The
inputs are:

- `opacity`, `geometry_opacity`, `alpha` and `alpha_mode` (no cutout).
- `specular_rotation`, `coat_rotation` and `anisotropy_rotation`.
- `coat_normal`, `geometry_coat_normal` and `clearcoat_normal`.
- `tangent`, `geometry_tangent` and `geometry_coat_tangent`.
- `occlusion` (a path tracer computes its own).
- `transmission_extra_roughness` and `coat_affect_color`, which the translation
  graph ignores.

An input left at its default SHALL NOT be reported.

#### Scenario: Authored opacity is reported, not applied

- **WHEN** a `standard_surface` authors `opacity = (0.3, 0.3, 0.3)`
- **THEN** the surface renders opaque and the log carries one `WARN` for that
  material naming `opacity`

#### Scenario: A default-valued input is silent

- **WHEN** a `gltf_pbr` authors `alpha = 1` and `alpha_mode = 0` explicitly
- **THEN** no warning is logged for them
