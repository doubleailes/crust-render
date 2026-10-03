## MODIFIED Requirements

### Requirement: Supported shading models

The engine SHALL shade every surface through one of three models. The first is
**`OpenPBR`**, a single übershader covering diffuse, metal, glass/transmission,
coat, fuzz, thin-film and subsurface, with `diffuse` / `metal` / `glass` /
`glossy` Rust-side preset constructors. The second is **`Emissive`**, a pure
emitter with no geometry knowledge. The third is a **MaterialX closure tree**, the
BSDF leaves and `layer` / `mix` / `add` / `multiply` combinators a `.mtlx`
material compiles to. `PreviewSurface` (a textured `UsdPreviewSurface`) SHALL
evaluate its inputs per shading point and delegate the BSDF to the `OpenPBR` it
resolves to. `MtlxMaterial` (a MaterialX graph) SHALL evaluate its pattern graph
per shading point and shade with its closure tree. It SHALL NOT pool that tree
onto a single `OpenPBR`.

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

## ADDED Requirements

### Requirement: MaterialX closure-tree semantics

A MaterialX material SHALL be evaluated as its closure tree with MaterialX's
combinator semantics, at every shading point:

- `mix(fg, bg, m)` SHALL be `m · fg + (1 − m) · bg`, with `m` clamped to [0, 1].
- `add(a, b)` SHALL be `a + b`.
- `multiply(x, w)` SHALL be `w · x`, per channel for a `color3` weight.
- `layer(top, base)` SHALL be `f_top + f_base · T_top(ωo)`. `T_top` is the top
  sub-tree's directional throughput toward the outgoing direction ωo:
  `1 − E(ωo)` for a reflecting leaf, the product of throughputs for nested
  layers, and the mix of throughputs for a mix.

A leaf whose weight is exactly 0, and a mix whose factor is exactly 0 or 1,
SHALL contribute nothing from the pruned branch. Importance sampling, `eval`
and the pdf SHALL describe the same distribution for every tree. A layer SHALL
never reflect more energy than its top and base would separately.

#### Scenario: A coat dims what lies beneath it at grazing angles

- **WHEN** a `layer` puts a smooth `dielectric_bsdf` (IOR 1.5) over an
  `oren_nayar_diffuse_bsdf`
- **THEN** `examples/mtlx_shade` reports the diffuse leaf's resolved weight as
  `1 − E_dielectric(ωo)`, lower at a grazing ωo than at normal incidence

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

### Requirement: MaterialX surface-shader nodes

A MaterialX material whose `surfaceshader` is a `standard_surface`,
`open_pbr_surface` or `gltf_pbr` node SHALL render with that node and SHALL NOT
fall back to the default material, nor report the node as unsupported. Every
input SHALL take, in order of precedence:

1. its connection, evaluated per shading point by the pattern graph;
2. its authored `value`;
3. its MaterialX 1.39 nodedef default, including when the document declares no
   nodedef.

#### Scenario: A constant standard_surface is no longer the fallback

- **WHEN** a `.mtlx` binds a `standard_surface` with `base_color = (0.8, 0.1, 0.1)`
  and no other inputs, and a USD `Material` references it
- **THEN** the surface renders red, and no "no operator for node type(s)
  standard_surface" warning is logged

#### Scenario: An unauthored input takes the nodedef default

- **WHEN** an `open_pbr_surface` authors only `base_color`
- **THEN** `examples/mtlx_shade` shows the specular leaf at the OpenPBR 1.1
  default roughness 0.3 and IOR 1.5

#### Scenario: A connected input varies over the surface

- **WHEN** a `gltf_pbr`'s `base_color` is connected to a node graph driven by
  `texcoord`
- **THEN** `examples/mtlx_shade` reports a different diffuse-leaf colour at two
  different `(u, v)`, each equal to the graph's output at that point

### Requirement: Surface-shader nodes expand to their MaterialX nodegraph

Each surface node SHALL be expanded into the closure tree of its MaterialX 1.39
implementation nodegraph: `NG_open_pbr_surface_surfaceshader`,
`NG_standard_surface_surfaceshader_100` and `IMPL_gltf_pbr_surfaceshader`. The
expansion SHALL have the same leaves, the same combinators in the same order,
and the same derived leaf parameters, except where this capability states an
approximation. In particular:

- **`open_pbr_surface`**:
  - Its metal leaf is a generalized-Schlick (F82) lobe weighted by
    `specular_weight`.
  - `specular_weight` modulates the dielectric's normal-incidence reflectance
    through its effective IOR.
  - A coat broadens the base specular roughness and makes the base IOR relative
    to the coat, both by `coat_weight`.
  - The substrate under the coat is multiplied by `mix(1, coat_color,
    coat_weight)` and by the coat-darkening factor.
  - Transmission is a combined reflect/transmit dielectric over
    `(1 − transmission_weight)` of the substrate.
- **`standard_surface`**:
  - Its metal is a conductor whose complex IOR comes from `artistic_ior(base_color,
    specular_color)`, as the nodegraph does.
  - The coat attenuates the substrate by `coat_color` and `coat_affect_color`
    as the nodegraph does.
  - It is not routed through the Standard Surface → OpenPBR translation graph.
- **`gltf_pbr`**:
  - Its base is the nodegraph's dielectric-over-diffuse / generalized-Schlick
    metal mix.
  - `clearcoat`, `sheen` and `iridescence` are layered as the nodegraph layers
    them.
  - `attenuation_color` / `attenuation_distance` define the interior medium.

#### Scenario: open_pbr_surface metal honours specular_weight

- **WHEN** an `open_pbr_surface` authors `base_metalness = 1` and
  `specular_weight = 0.5`
- **THEN** the metal leaf's resolved weight is half that at `specular_weight = 1`

#### Scenario: open_pbr_surface coat broadens the base highlight

- **WHEN** an `open_pbr_surface` authors `specular_roughness = 0.1`,
  `coat_weight = 1` and `coat_roughness = 0.5`
- **THEN** the base specular leaf's roughness is `(0.1⁴ + 2·0.5⁴)^¼`

#### Scenario: standard_surface metal is an artistic-IOR conductor

- **WHEN** a `standard_surface` authors `metalness = 1`,
  `base_color = (0.9, 0.6, 0.2)` and `specular_color = (1, 0.9, 0.7)`
- **THEN** the probe lists a conductor leaf whose complex IOR equals
  `artistic_ior((0.9, 0.6, 0.2), (1, 0.9, 0.7))`

#### Scenario: gltf_pbr clearcoat is a separate lobe

- **WHEN** a `gltf_pbr` authors `clearcoat = 1`, `clearcoat_roughness = 0` and
  `roughness = 0.6`
- **THEN** its render shows a sharp clearcoat highlight over a broad base
  highlight, and the probe lists two specular leaves at those two roughnesses

### Requirement: Per-leaf shading normal and tangent

Every BSDF leaf SHALL shade in its own frame, built from that leaf's `normal`
input (and `tangent`, where it has one), under the rule a MaterialX `normal`
follows today:

- The input is normalised.
- A normal facing away from the geometric normal is ignored rather than flipping
  the surface.
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
`T` mode SHALL weight its transmission by its own `(1 − F)`, as MaterialX GLSL
does, so that a reflection layer over it attenuates only by the layer's
throughput and no surface returns more than it receives.

#### Scenario: A glass is bounded in a white furnace

- **WHEN** a `standard_surface` glass sphere is rendered unclamped inside a
  uniform emitter of radiance 1
- **THEN** no pixel, head-on or at grazing incidence, exceeds 1

#### Scenario: A standard_surface glass is transparent

- **WHEN** a `standard_surface` authors `transmission = 1` and
  `specular_roughness = 0` and is rendered in front of the dome
- **THEN** the dome is visible, refracted, through the object

#### Scenario: Depth absorbs inside the object

- **WHEN** a `gltf_pbr` authors `transmission = 1`, `thickness = 1`,
  `attenuation_color = (1, 0.2, 0.2)` and `attenuation_distance = 0.5`
- **THEN** light through thicker parts of the object is more strongly
  red-tinted than light through thinner parts

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
  meaning its weight is not the literal 0. These are `subsurface_bsdf` (shaded
  as a diffuse-like leaf in the subsurface colour, no random walk) and
  `sheen_bsdf` in `zeltner` mode (evaluated as Charlie).

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
