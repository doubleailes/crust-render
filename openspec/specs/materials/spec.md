# materials Specification

## Purpose

Define how surfaces scatter and emit light. Materials are the integrator's
extension point: each implements a common trait exposing importance-sampled
scattering and emission. This capability covers the material trait contract, the
supported shading models, the shared microfacet helpers, and emissive surfaces.
Lives in `crust-core/src/material/`.

## Requirements

### Requirement: Material trait contract

Every material SHALL implement a common `Material` trait providing
importance-sampled scattering (`scatter_importance` returning an optional
`ScatterSample` — scattered ray, BRDF value, PDF, and a `delta` flag marking
singular lobes such as transmission), a continuous-component evaluator
(`eval(r_in, rec, wi) -> Option<(value, pdf)>`, used by NEE and guided MIS;
`None` means no continuous component and must not depend on `wi`), and an
emission query (`emitted`).

#### Scenario: Integrator queries a material

- **WHEN** the integrator interacts with a hit surface
- **THEN** it obtains a `ScatterSample` (scattered ray, BRDF value, PDF, delta
  flag) via `scatter_importance`, or `None` when the material absorbs the ray

#### Scenario: Non-emissive material

- **WHEN** `emitted` is queried on a non-light material
- **THEN** it returns zero radiance

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

### Requirement: Shared microfacet BRDF helpers

`OpenPBR` lobes SHALL share GGX helpers from `material/brdf.rs`: anisotropic
visible-normal (VNDF) GGX sampling and its PDF, Schlick/F82 Fresnel, EON
(energy-preserving Oren-Nayar) diffuse, Charlie sheen, thin-film, and Cauchy
dispersion.

#### Scenario: Microfacet lobe samples a direction

- **WHEN** a GGX-based lobe of `OpenPBR` scatters a ray
- **THEN** the outgoing direction is drawn via VNDF sampling and weighted by
  Fresnel and geometry terms

### Requirement: Emissive surfaces act as light-emitting geometry

An Emissive material SHALL return non-zero radiance from `emitted`, allowing the
same surface to serve as both visible geometry and a light source.

#### Scenario: Emissive sphere is hit directly

- **WHEN** a ray hits an emissive surface
- **THEN** the surface contributes its emission color to the path

### Requirement: Unbound geometry falls back to grey OpenPBR

Geometry with no resolvable bound material SHALL be assigned a default grey
`OpenPBR` material, not a separate shading model.

#### Scenario: Unbound geometry

- **WHEN** a mesh or sphere has no resolvable bound material
- **THEN** it renders with a default grey diffuse `OpenPBR` material

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
- **THEN** the histogram of sampled directions matches the reported pdf over
  the whole sphere, and the importance-sampled estimate of any smooth integral
  of `f·|cos|` matches its quadrature

#### Scenario: A clear fibre's samples all weigh one

- **WHEN** a `chiang_hair_bsdf` with zero absorption and white tints is sampled
- **THEN** every sample's weight `f·|cos|/pdf` is 1, because its lobe
  attenuations sum to one and the lobe choice follows them

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
- **THEN** its refracted rays meet the tube's far wall from inside, exactly as
  a ray that does not pass out of curve tubes does

### Requirement: MaterialX pattern nodes evaluate as the MaterialX reference

Every MaterialX standard-library pattern node signature that `crust-mtlx`
compiles, over the `float`, `vector2`, `vector3`, `vector4`, `color3` and
`color4` value types, SHALL evaluate to the value MaterialX's reference
implementation (its `genosl` code generator, run by OSL) produces for the same
node and inputs, to within 1e-5 relative in every lane and at exactly the
reference's width, including when inputs are left unauthored. The only
exceptions SHALL be these guards, which keep a value finite or physical where
the reference does not:

- `divide` by a zero lane gives 0, and `remap` over an empty input range
  (`inlow == inhigh`) gives `outlow`, instead of ±inf or NaN;
- `modulo` by a zero lane returns the dividend, for every value type, and a
  `modulo` whose quotient overflows gives the exact floored remainder instead
  of ±inf;
- `normalmap` raises the decoded tangent-space z to at least 1e-4;
- `artistic_ior` clamps `edge_color` to [0, 1];
- an unauthored `convert` input is a zero `float`, whatever the signature.

Each exception SHALL be tested as a rule naming the input condition under
which it applies, so it cannot excuse a difference under any other input.

#### Scenario: An unauthored multiply input takes the nodedef default

- **WHEN** a `multiply` node of type `float` authors no inputs
- **THEN** it evaluates to 0 (`in1` defaults to 0 and `in2` to 1)

#### Scenario: modulo floors

- **WHEN** a `modulo` node computes `-0.2` modulo `1`
- **THEN** it evaluates to 0.8, not -0.2

#### Scenario: modulo by a subnormal divisor stays finite

- **WHEN** a `modulo` node computes `1` modulo `1e-40`
- **THEN** it evaluates to a finite value between 0 and `1e-40`

#### Scenario: Unauthored inputs take the node's width

- **WHEN** an `add` node of type `color3` authors no inputs
- **THEN** it evaluates to a three-lane zero, not a `float`

#### Scenario: sign of zero

- **WHEN** a `sign` node's input is 0
- **THEN** it evaluates to 0

#### Scenario: A division by zero stays finite

- **WHEN** a `divide` node's `in2` is 0 in some lane
- **THEN** that lane evaluates to 0, and every other lane to the reference's
  value

#### Scenario: The reference cases pass

- **WHEN** `cargo test -p crust-mtlx --test osl_oracle` runs over the
  committed reference cases
- **THEN** every value has the reference's width, every lane matches the
  reference or falls under one of the listed exceptions, and every signature
  in the fixture has all of its cases

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

### Requirement: A subsurface walk ends without bias

A subsurface random walk SHALL end in exactly one of four ways: at an exit through
its own object's surface, which continues the path weighted by the walk's
throughput; as absorbed, when its throughput is below 1e-6 in every channel; by
roulette, once its throughput is below 0.05 in every channel, where the walk
survives with probability `peak / 0.05` (never below 0.05) and its throughput is
divided by that probability on survival; or at 256 steps, as absorbed. The
roulette SHALL NOT change the expected radiance of any pixel: a render with it is
an unbiased estimate of the same image as a render without it, and its noise at a
given sample count SHALL be the same within measurement error. The 256-step cap
is a known bias: on an object many mean free paths thick a near-white medium loses
several percent of its energy to it (measured 7.5% at α → 1 and 4% in the red
channel of a skin-like medium on a semi-infinite slab; `docs/subsurface_walk.md`),
and this change does not remove it.

#### Scenario: The roulette costs no noise

- **WHEN** `samples/materialx_subsurface.usda` is rendered at 16 and at 32 samples
  per pixel and compared against a 2048-sample reference of the same scene with
  `exr_diff`
- **THEN** the relative MSE at each sample count equals the walk's without roulette
  to three significant digits, and halves from 16 to 32 samples

#### Scenario: A slab still reflects its colour

- **WHEN** 8192 walks with a cosine-weighted entry enter a semi-infinite slab of
  colour (0.8, 0.5, 0.2), radius 0.1 and zero anisotropy
- **THEN** the mean walk weight, absorbed walks counted as zero, is within 0.05 of
  the colour in every channel

#### Scenario: A dim chromatic walk stops early

- **WHEN** a walk's throughput has fallen below 0.05 in every channel and it has
  not exited
- **THEN** it either ends there or continues with its throughput divided by its
  survival probability, so that a scene's `--stats` mean steps per walk falls for
  a medium whose channels decay at different rates (skin) and is unchanged for a
  grey one (marble)
