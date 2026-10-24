+++
title = "Materials and textures"
description = "The crust:openpbr shader and its inputs, and the other material types Crust Render reads."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-02T08:00:00+00:00
draft = false
weight = 50
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Crust Render shades every surface with one OpenPBR übershader. The <code>crust:openpbr</code> shader sets its parameters directly.'
toc = true
top = false
+++

## Material types

A prim gets its material through the standard `MaterialBindingAPI` (`rel material:binding`).
Crust Render picks the material's surface shader by its `info:id`:

| shader | how it is read |
|--------|----------------|
| `info:id = "crust:openpbr"` | Crust's own shader: every OpenPBR parameter, one to one. [See below](#the-crust-openpbr-shader). |
| `info:id = "UsdPreviewSurface"` | mapped onto OpenPBR, with `UsdUVTexture` and Ptex texture inputs |
| `info:id = "PxrDisneyBsdf"` | mapped onto OpenPBR (as authored by the Moana Island) |
| a reference into a `.mtlx` file | read as a MaterialX graph: standalone BSDF nodes, `open_pbr_surface`, `standard_surface`, `gltf_pbr`, `UsdPreviewSurface`, image and UDIM textures, normal maps, [hair](#hair) |
| `info:id = "ND_…"` (a MaterialX nodedef) | an inline MaterialX network, read exactly as the same graph in a `.mtlx`. [See below](#materialx-networks-in-usd). |

A material Crust Render can't read logs a warning and renders as grey diffuse. Look for
these warnings when a surface comes out grey.

A MaterialX material is a `Material` prim that references a material inside a `.mtlx`
document:

```usda
def Material "Ceramic" (
    prepend references = @materials.mtlx@</MaterialX/Materials/mtlx_ceramic>
)
{
}
```

## MaterialX networks in USD

A MaterialX material can also be authored inline, as `Shader` prims whose `info:id` names
a MaterialX nodedef, wired to the material's `outputs:mtlx:surface` terminal. This is how
`usdMtlx` brings a `.mtlx` into a stage, how DCCs export MaterialX, and how NVIDIA's
Typhoon renderer receives materials.

```usda
def Material "Copper"
{
    token outputs:mtlx:surface.connect = </World/Looks/Copper/Surface.outputs:out>

    def Shader "Surface"
    {
        uniform token info:id = "ND_open_pbr_surface_surfaceshader"
        float inputs:base_metalness = 1
        color3f inputs:base_color = (0.95, 0.64, 0.54)
        token outputs:out
    }
}
```

The node's type comes from the nodedef name (`ND_mix_vdf` is a `mix` of VDFs,
`ND_dielectric_bsdf` a `dielectric_bsdf`). Inputs are read as values, or followed through
connections, including through `NodeGraph` outputs and `Material` interface inputs.
`asset` inputs resolve against the layer that authored them.

`ND_UsdPreviewSurface_surfaceshader`, the surface a DCC's MaterialX export often writes, is
built from MaterialX's own implementation of it, so its inputs can come from any
MaterialX graph. It differs from the UsdShade `info:id = "UsdPreviewSurface"` in one
respect: that one is mapped onto OpenPBR, this one is MaterialX's graph of diffuse,
Schlick specular, metal and clearcoat BSDFs, as a MaterialX renderer shades it.
`occlusion` and `displacement` are not applied, and say so in a warning.

### Custom MaterialX nodes

A studio node library — a DCC's own nodes, a show's helpers — defines each node
with a `<nodedef>` and implements it as a `<nodegraph nodedef="…">` of standard nodes.
Crust Render finds those definitions where USD does, in the `.mtlx` files under
[`PXR_MTLX_PLUGIN_SEARCH_PATHS`](@/docs/reference/environment-variables.md#pxr-mtlx-plugin-search-paths),
and also reads the ones a `.mtlx` document declares itself, which take precedence.

```bash
PXR_MTLX_PLUGIN_SEARCH_PATHS=/studio/mtlx crust render -i shot.usda
```

An instance of such a node, in a `.mtlx` or as a `Shader` whose `info:id` names the
nodedef, renders as its graph: each input bound to the graph's interface
takes the instance's input, or the nodedef's default when the instance leaves it
unauthored, and an instance's named outputs are the graph's.
An instance that authors a `version` uses the nodedef of that version. One whose type
or version no nodedef of its category has is named in the `mtlx.incomplete` warning
rather than rendered with another nodedef's graph.
Custom nodes inside custom nodes expand too, up to 16 levels deep. A definition that
instantiates itself, directly or through others, is reported as a cycle and not
expanded. A node inside a graph
that Crust Render doesn't implement is named in the `mtlx.incomplete` warning, and the
inputs it drives keep their defaults. A definition never replaces one of Crust Render's
own nodes: a library's `mix` is ignored in favour of the built-in one.

These standard nodes, which such libraries are built from, evaluate as MaterialX
defines them:

| node | what it does |
|------|--------------|
| `separate2`, `separate3`, `separate4` | one channel per output (`outr`, `outg`, `outb`, `outa`, or `outx`… for vectors) |
| `switch` | `in1` when `which` is below 1, `in2` below 2, … `in10` below 10, and zero from 10 on |
| `over`, `in`, `out`, `difference`, `premult` | `color4` compositing, `fg` over (inside, outside, the difference with) `bg`, blended with `bg` by `mix` |
| `place2d` | moves a texture coordinate: scale, rotate (in degrees) and offset about a pivot, in SRT or TRS order |
| `transformmatrix` | a vector times a matrix, as in MaterialX's OSL. A `vector3` by a `matrix44` is a direction: the matrix's translation is not applied (MaterialX's GLSL applies it). A `vector2` by a `matrix33` is translated by its last row |

`transformmatrix` takes its matrix only as a constant: a value, or a `constant` node's
value. A matrix computed from anything else is reported, and the vector passes through
unchanged. `position` reads the world-space position; another `space` is reported.

A universal `outputs:surface` with a decodable shader, such as a `UsdPreviewSurface`, is
still used first. The `mtlx` terminal is read when the material has nothing else Crust
Render can decode, so a stage that rendered through its preview surface renders the same.

### How MaterialX BSDFs are shaded

Crust Render shades a MaterialX material as its closure tree, with the semantics of
MaterialX's OSL backend as OSL's `testrender` renders it. Each BSDF node is the
corresponding lobe of [BSDL](https://github.com/AcademySoftwareFoundation/OpenShadingLanguage/tree/main/src/libbsdl),
the library `testrender` shades with: its microfacet model, energy tables, Fresnel,
sheen and diffuse lobes. A test replays reference values BSDL itself computed and checks
every node against them.

`layer(top, base)` lets the base through in proportion to how transparent the top is.
Each node in the top adds to its opacity:

- a `dielectric_bsdf` or `generalized_schlick_bsdf` that only reflects, and a
  `sheen_bsdf`, add their weight times the share of light they reflect toward the viewer;
- every other node adds its whole weight. That includes a dielectric that refracts, and a
  diffuse or a conductor. A diffuse at weight 0.3 over a metal leaves 70% of the metal
  showing;
- `mix` blends opacities, `add` sums them, `multiply` scales them, and a `layer` nested in
  a top adds its own top's and base's opacities together.

The base is weighted by one minus the top's opacity, clamped to [0, 1].

A few behaviours follow BSDL rather than MaterialX's GLSL shaders:

- Each node is also sampled as BSDL samples it. A rough glass (a `dielectric_bsdf` that
  refracts) relies on that: BSDL's energy tables are made for its own sampler. Lit by a
  light that light sampling can also reach, a very rough glass still returns about 2% more
  than it receives, as in `testrender`.
- A `sheen_bsdf` in `zeltner` mode reads its fibre table at the square root of
  `roughness`, so it is not the same as `crust:openpbr`'s fuzz at the same roughness. A
  sheen is not shaded on a back face.
- A `generalized_schlick_bsdf`'s `color82` and a thin film (`thinfilm_thickness`) are
  MaterialX's own, since BSDL does not apply them.
- A `chiang_hair_bsdf` in a layer's top hides what lies beneath it.

One behaviour deliberately differs from `testrender`. Seen from inside an object (a back
face), a `dielectric_bsdf` or `generalized_schlick_bsdf` that only reflects, in a layer's
top, lets through only the light it does not reflect, and none past the critical angle,
where it reflects everything. BSDL lets about as much through as it would from outside, so
in `testrender` a coat over glass returns up to twice the light it receives from inside, and
light trapped inside a coated glass object grows with every bounce.

## UsdPreviewSurface opacity

`inputs:opacity` means one of two things, chosen by `inputs:opacityThreshold`:

- **Translucency**, when `opacityThreshold` is 0 (the default). An `opacity` below 1
  makes the surface refract, as glass at `inputs:ior`.
- **A cutout**, when `opacityThreshold` is above 0. A point is kept where `opacity` is at
  or above the threshold and cut away otherwise. Nothing refracts.

A cutout's `opacity` is usually a texture's alpha, as DCCs export leaves and grass cards:

```usda
def Shader "Surface"
{
    uniform token info:id = "UsdPreviewSurface"
    color3f inputs:diffuseColor.connect = </World/Looks/Leaf/Tex.outputs:rgb>
    float inputs:opacity.connect = </World/Looks/Leaf/Tex.outputs:a>
    float inputs:opacityThreshold = 0.5
    token outputs:surface
}
def Shader "Tex"
{
    uniform token info:id = "UsdUVTexture"
    asset inputs:file = @leaf.png@
    float3 outputs:rgb
    float outputs:a
}
```

`outputs:a` reads the file's alpha channel: a PNG's or TIFF's alpha, or an EXR's `A`
channel. A file without alpha reads 1.0, so the surface is kept everywhere. Alpha is
never colour-decoded: `sourceColorSpace` applies to `rgb` only.

## Volume materials

A material's `volume` terminal (`outputs:mtlx:volume`, or `outputs:volume`) describes the
medium **inside** the geometry the material is bound to. It is an `ND_volume` shader whose
`vdf` input is a VDF network:

| node | medium |
|------|--------|
| `ND_anisotropic_vdf` | `absorption` and `scattering` coefficients (per unit length), Henyey–Greenstein `anisotropy` |
| `ND_absorption_vdf` | `absorption` alone |
| `ND_mix_vdf`, `ND_add_vdf`, `ND_multiply_vdfC` / `vdfF` | combinations: coefficients combine linearly, the anisotropy is weighted by how much each side scatters |
| `ND_mix_volumeshader` | a mix of two `ND_volume` shaders |

There are two cases:

- **A volume with no surface** makes the object a **medium boundary**. Its surface is
  invisible. Rays cross it without scattering, and travel through the medium inside. The
  medium is lit through the boundary, scatters light, and casts shadows. Objects inside it
  are seen and lit through it. This is how to make fog, smoke or murky water in a shape.
- **A volume with a MaterialX surface** is the interior of that surface. A ray that
  refracts into a thick (not thin-walled) transmissive surface travels through it. It
  replaces the medium the surface's own `transmission_*` inputs would describe.

```usda
def Material "Fog"
{
    token outputs:mtlx:volume.connect = </World/Looks/Fog/Volume.outputs:out>

    def Shader "Volume"
    {
        uniform token info:id = "ND_volume"
        token inputs:vdf.connect = </World/Looks/Fog/Vdf.outputs:out>
        token outputs:out
    }

    def Shader "Vdf"
    {
        uniform token info:id = "ND_anisotropic_vdf"
        vector3f inputs:absorption = (0.02, 0.02, 0.02)
        vector3f inputs:scattering = (0.9, 0.9, 0.9)
        float inputs:anisotropy = 0.5
        token outputs:out
    }
}
```

A `.mtlx` document can do the same with a `volumematerial` node.

Rules for the geometry of a medium boundary:

- It must be **closed**, with normals facing **out**. Rays enter through its front faces.
- The medium is **homogeneous**: one set of coefficients per object. For smoke and noise,
  use a [volume region](@/docs/usd/volumes.md).
- One medium at a time: inside a medium boundary, other boundaries and glass don't change
  the medium.
- A camera that starts inside a medium boundary doesn't see the medium until its rays leave
  and enter it again.
- A volume's `edf` (volume emission) is ignored, with a warning.

`samples/materialx_volume.usda` shows a fog cube with a ball inside it, a glass sphere with
a volume interior, and an inline OpenPBR surface.

## The crust:openpbr shader

A `Shader` prim with `info:id = "crust:openpbr"` sets the parameters of the
[OpenPBR Surface](https://academysoftwarefoundation.github.io/OpenPBR/) model directly.
Each input is named after the OpenPBR parameter in camelCase: `base_color` becomes
`inputs:baseColor`, `specular_roughness` becomes `inputs:specularRoughness`.

Author only the inputs you want to change. The others keep their default.

```usda
def Material "red_plastic"
{
    token outputs:surface.connect = </World/Looks/red_plastic/Surface.outputs:surface>

    def Shader "Surface"
    {
        uniform token info:id = "crust:openpbr"
        color3f inputs:baseColor = (0.8, 0.1, 0.1)
        float inputs:specularRoughness = 0.25
        float inputs:coatWeight = 1
        token outputs:surface
    }
}
```

`samples/openpbr_showcase.usda` shows metal, plastic, glass, coat and other presets.

### Input types

The `crust:openpbr` inputs are read with these types. An input authored with another type
is ignored, and its default is used.

| type | you can author |
|------|----------------|
| `float` | `float` or `double` |
| `color3f` | a single-precision 3-vector: `color3f`, `float3`, `vector3f`, … (not `double3`) |
| `bool` | `bool` only |

Colours are **linear**: no transfer curve is applied. Values are constants: the inputs are
read as values, not as connections to texture nodes. For textures, use
`UsdPreviewSurface` or MaterialX.

### Base

| input | type | default | meaning |
|-------|------|---------|---------|
| `inputs:baseWeight` | `float` | 1.0 | weight of the base layer |
| `inputs:baseColor` | `color3f` | (0.8, 0.8, 0.8) | diffuse albedo, or metal colour |
| `inputs:baseDiffuseRoughness` | `float` | 0.0 | diffuse roughness (0 = Lambertian) |
| `inputs:baseMetalness` | `float` | 0.0 | 0 = dielectric, 1 = metal |

### Specular

| input | type | default | meaning |
|-------|------|---------|---------|
| `inputs:specularWeight` | `float` | 1.0 | weight of the specular reflection |
| `inputs:specularColor` | `color3f` | (1, 1, 1) | specular tint |
| `inputs:specularRoughness` | `float` | 0.3 | microfacet roughness |
| `inputs:specularIor` | `float` | 1.5 | index of refraction |
| `inputs:specularRoughnessAnisotropy` | `float` | 0.0 | roughness anisotropy |

### Transmission

| input | type | default | meaning |
|-------|------|---------|---------|
| `inputs:transmissionWeight` | `float` | 0.0 | 1 = fully transmissive, like glass |
| `inputs:transmissionColor` | `color3f` | (1, 1, 1) | transmission tint |
| `inputs:transmissionDepth` | `float` | 0.0 | distance at which the tint is reached. 0 tints at the surface. |
| `inputs:transmissionScatter` | `color3f` | (0, 0, 0) | scattering inside the medium |
| `inputs:transmissionScatterAnisotropy` | `float` | 0.0 | phase-function anisotropy of that scattering |
| `inputs:transmissionDispersionScale` | `float` | 0.0 | strength of dispersion |
| `inputs:transmissionDispersionAbbeNumber` | `float` | 20.0 | Abbe number of the dispersion |

### Subsurface

| input | type | default | meaning |
|-------|------|---------|---------|
| `inputs:subsurfaceWeight` | `float` | 0.0 | weight of subsurface scattering |
| `inputs:subsurfaceColor` | `color3f` | (0.8, 0.8, 0.8) | subsurface albedo |
| `inputs:subsurfaceRadius` | `float` | 1.0 | mean free path, in scene units |
| `inputs:subsurfaceRadiusScale` | `color3f` | (1.0, 0.5, 0.25) | per-channel scale of the radius |
| `inputs:subsurfaceScatterAnisotropy` | `float` | 0.0 | phase-function anisotropy |

### Fuzz

| input | type | default | meaning |
|-------|------|---------|---------|
| `inputs:fuzzWeight` | `float` | 0.0 | coverage of the fuzz (sheen) layer |
| `inputs:fuzzColor` | `color3f` | (1, 1, 1) | fuzz colour |
| `inputs:fuzzRoughness` | `float` | 0.5 | fuzz roughness, from 0 (a narrow rim at grazing angles) to 1 (a soft, broad sheen) |

The fuzz is the one Adobe's OpenPBR reference implements: Zeltner, Burley and Chiang's
sheen, a fit to a layer of fibres. How much light it reflects depends on the angle you see
it from. A smooth fuzz is almost invisible head-on and shows as a bright rim toward the
silhouette; a rough one reflects about a third of the light even head-on. Whatever the
fuzz reflects toward you, it takes from the layers beneath it (coat, specular, base and
emission), so a fuzzy surface never gains energy. Unlike Adobe's, crust's fuzz is also
seen on the back of a surface that is not thin-walled, as cloth usually is.

### Coat

| input | type | default | meaning |
|-------|------|---------|---------|
| `inputs:coatWeight` | `float` | 0.0 | weight of the clear coat |
| `inputs:coatColor` | `color3f` | (1, 1, 1) | coat tint |
| `inputs:coatRoughness` | `float` | 0.0 | coat roughness |
| `inputs:coatRoughnessAnisotropy` | `float` | 0.0 | coat roughness anisotropy |
| `inputs:coatIor` | `float` | 1.6 | coat index of refraction |
| `inputs:coatDarkening` | `float` | 1.0 | how much the coat darkens and saturates the layers below |

### Thin film

| input | type | default | meaning |
|-------|------|---------|---------|
| `inputs:thinFilmWeight` | `float` | 0.0 | weight of the thin-film iridescence |
| `inputs:thinFilmThickness` | `float` | 0.5 | film thickness, in micrometres |
| `inputs:thinFilmIor` | `float` | 1.4 | film index of refraction |

### Emission

| input | type | default | meaning |
|-------|------|---------|---------|
| `inputs:emissionLuminance` | `float` | 0.0 | emitted radiance, multiplied by the colour |
| `inputs:emissionColor` | `color3f` | (1, 1, 1) | emission colour |

The surface emits `emissionColor × emissionLuminance`, in the same linear units as the
image. An emissive surface is only found by rays that hit it, not sampled like a light,
unless its mesh applies [`MeshLightAPI`](@/docs/usd/lights.md#mesh-lights). Use that, or a
UsdLux light, for a scene's main light sources.

### Geometry

| input | type | default | meaning |
|-------|------|---------|---------|
| `inputs:geometryOpacity` | `float` | 1.0 | 0 = fully cut out |
| `inputs:geometryThinWalled` | `bool` | false | treat the surface as an infinitely thin sheet (leaves, paper) rather than the boundary of a solid |

A thin-walled surface with `transmissionWeight` above 0 is a window: what it transmits
goes straight through, unbent. Shadows see that. A shadow ray through the sheet is tinted
by what it lets through instead of blocked, so a light behind a pane, a lampshade or a
leaf is found by light sampling, and the surfaces it lights converge without fireflies.
MaterialX surfaces with `thin_walled` (`open_pbr_surface`'s `geometry_thin_walled`,
`standard_surface`'s `thin_walled`) work the same way. A closed, thick glass object still
casts a solid shadow ([why](@/docs/architecture/limitations.md#materials-and-textures)).

## Hair

Hair and fur are `BasisCurves` prims shaded with MaterialX's `chiang_hair_bsdf`: the fibre
model of Chiang et al. (2016), as pbrt-v3 implements it. It scatters light the way a
strand does: a primary highlight (R), a secondary coloured highlight shifted along the
strand (TRT), a glow when the hair is lit from behind (TT), and the light that bounces
around inside the fibre several times. Bind it like any MaterialX material:

```xml
<materialx version="1.39">
  <chiang_hair_roughness name="rough" type="multioutput">
    <input name="longitudinal" type="float" value="0.3" />
    <input name="azimuthal" type="float" value="0.5" />
  </chiang_hair_roughness>
  <deon_hair_absorption_from_melanin name="melanin" type="vector3">
    <input name="melanin_concentration" type="float" value="0.6" />
    <input name="melanin_redness" type="float" value="0.3" />
  </deon_hair_absorption_from_melanin>
  <chiang_hair_bsdf name="hair" type="BSDF">
    <input name="roughness_R" type="vector2" nodename="rough" output="roughness_R" />
    <input name="roughness_TT" type="vector2" nodename="rough" output="roughness_TT" />
    <input name="roughness_TRT" type="vector2" nodename="rough" output="roughness_TRT" />
    <input name="absorption_coefficient" type="vector3" nodename="melanin" />
  </chiang_hair_bsdf>
  <surface name="hair_surface" type="surfaceshader">
    <input name="bsdf" type="BSDF" nodename="hair" />
  </surface>
  <surfacematerial name="brown_hair" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="hair_surface" />
  </surfacematerial>
</materialx>
```

| input | meaning |
|-------|---------|
| `tint_R`, `tint_TT`, `tint_TRT` | a colour multiplying that lobe. The longer paths inside the fibre take `tint_TRT`. |
| `ior` | the fibre's index of refraction. Default 1.55. |
| `roughness_R`, `roughness_TT`, `roughness_TRT` | a `vector2` per lobe: longitudinal variance and azimuthal scale, each clamped to [0.001, 1]. Connect them to `chiang_hair_roughness` rather than authoring them. |
| `cuticle_angle` | the tilt of the fibre's scales, in [0, 1]: 0.5 is none, and the range maps to −90° to +90°. Real hair is about 2°, so about 0.51. |
| `absorption_coefficient` | how strongly the fibre absorbs, per unit radius, per channel. Connect it to one of the absorption helpers. |
| `curve_direction` | the strand's direction. Unconnected, it is the curve's own direction at the hit, which is what you want. |

The three helper nodes turn artist parameters into those inputs:

| node | outputs |
|------|---------|
| `chiang_hair_roughness` | `roughness_R`, `roughness_TT` and `roughness_TRT` from a `longitudinal` and an `azimuthal` roughness in [0, 1] |
| `chiang_hair_absorption_from_color` | the `absorption` that makes a dense groom read as `color`, for a given `azimuthal_roughness` |
| `deon_hair_absorption_from_melanin` | the `absorption` of natural hair from a `melanin_concentration` and a `melanin_redness` |

`samples/hair.usda` renders one tuft per way of reaching the node. Light reaching a strand
through the strand itself is already part of the model, so a strand never shadows its
own transmitted light. It still shadows everything else, other strands included. If the
hair node is mixed with a BSDF that transmits, such as a refracting `dielectric_bsdf` or
a `translucent_bsdf`, only the hair's share of the light passes through the strand. The
other BSDF's light meets the far side of the tube, as it would without the hair. Path
guiding is off where such a mix is hit.

Author grooms in centimetres (`metersPerUnit = 0.01`, the USD default). Crust Render
starts every ray 0.001 units away from the surface it leaves; in a groom authored in
metres that is 1 mm, wider than a hair, and neighbouring strands then stop lighting and
shadowing each other. See [limitations](@/docs/architecture/limitations.md#materials-and-textures).

The node also works on a mesh, such as a hair card. There, `curve_direction` defaults to
the mesh's UV tangent, so the card needs UVs whose `u` runs along the hair.

### Variation nodes

A hair material can read where on its strand a hit lies, which curve it is, a random
value per curve, and the groom's own primvars, through `geompropvalue`: see
[curve attributes](@/docs/usd/geometry.md#curve-attributes-in-materials). Any other
material reads its mesh's primvars the same way: see
[primvars in materials](@/docs/usd/geometry.md#primvars-in-materials). These
MaterialX nodes are what a per-strand look is usually built from:

| node | what it does |
|------|--------------|
| `rgbtohsv`, `hsvtorgb` | convert a colour (`color3`, or `color4` keeping its alpha) between RGB and hue, saturation and value, so a hue shift is an `add` on the first channel |
| `ifgreater`, `ifgreatereq`, `ifequal` | `in1` when `value1` is greater than (at least, equal to) `value2`, else `in2`. The `boolean` form answers 1 or 0 |
| `randomfloat` | a value in [`min`, `max`] that depends only on `in` and `seed`, so the same inputs give the same value at every hit, frame and render |
| `randomcolor` | a colour whose hue, saturation and brightness each fall in their own range, chosen by `in` and `seed` |
| `cellnoise2d` | one value in [0, 1] per unit cell of `texcoord`; the random nodes are built from it |

They evaluate as MaterialX's own implementation does, random values included, so a look
built for another MaterialX renderer draws the same values here. `randomfloat` reads a
`float` `in` in steps of 1/4096, as MaterialX defines it, so two inputs closer than that
draw the same value.

A per-strand hue shift, for instance, is `curve:random` remapped to a small range and
added to the hue:

```xml
<geompropvalue name="rand" type="float">
  <input name="geomprop" type="string" value="curve:random" />
</geompropvalue>
<remap name="shift" type="float">
  <input name="in" type="float" nodename="rand" />
  <input name="outlow" type="float" value="-0.03" />
  <input name="outhigh" type="float" value="0.03" />
</remap>
<rgbtohsv name="hsv" type="color3"><input name="in" type="color3" value="0.35, 0.2, 0.1" /></rgbtohsv>
<combine3 name="delta" type="color3">
  <input name="in1" type="float" nodename="shift" />
</combine3>
<add name="shifted" type="color3">
  <input name="in1" type="color3" nodename="hsv" />
  <input name="in2" type="color3" nodename="delta" />
</add>
<hsvtorgb name="hair_colour" type="color3"><input name="in" type="color3" nodename="shifted" /></hsvtorgb>
```

`curve:random` changes when a prim's curves are reordered. For variation that survives
that, author a stable id per curve and feed it to `randomfloat`:

```xml
<geompropvalue name="id" type="integer">
  <input name="geomprop" type="string" value="strandId" />
</geompropvalue>
<randomfloat name="rand" type="float">
  <input name="in" type="integer" nodename="id" />
</randomfloat>
```

## Displacement

A material can move the surface it's bound to, not just shade it. Crust Render reads a
**scalar** displacement: each vertex of the mesh moves along its normal by a distance the
material gives, in the mesh's own (local) units, so a scaled placement scales the offset
with the mesh. Silhouettes, contact shadows and self-shadowing then come from the
displaced shape. A normal map in the same material still perturbs the shading on top.

Displacement is applied once, when the mesh is loaded, to the vertices its tessellation
has. It adds no vertices of its own, so detail finer than the dicing rate is lost: set
[`crust:subdivisionLevel`](@/docs/usd/render-settings.md#crust-subdivisionlevel) (or
`--subdiv-level`, or adaptive `--subdiv-edge-length`) high enough for the map. At the
default level 0 only the cage's vertices move, and the import warns once. Each lookup is
filtered over the spacing between vertices, so a coarse tessellation reads a blurred
version of the map rather than a random sample of it.

A mesh with `subdivisionScheme = "none"` and a displacing material is diced bilinearly so
that it has vertices to move. See [geometry](@/docs/usd/geometry.md#crust-displacementbound).

### UsdPreviewSurface

`inputs:displacement`, as a value or connected to a `UsdUVTexture`. The texture's output
channel, `scale` and `bias` apply, as for any other input. The file is read as data (no
colour decoding) unless `sourceColorSpace = "sRGB"` says otherwise.

```usda
def Shader "Surface"
{
    uniform token info:id = "UsdPreviewSurface"
    float inputs:displacement.connect = </World/Looks/Ground/Height.outputs:r>
    token outputs:surface
}
def Shader "Height"
{
    uniform token info:id = "UsdUVTexture"
    asset inputs:file = @height.png@
    float4 inputs:scale = (0.3, 0.3, 0.3, 1)
    float outputs:r
}
```

### MaterialX

A `surfacematerial` whose `displacementshader` is a `displacement` node with a `float`
`displacement` input is displaced by that input times the node's `scale`. The input can be
any graph. It's evaluated at each vertex with the vertex's texture coordinate, and its
object-space `position` and `normal`, as MaterialX defines displacement. Nodes that depend
on the view direction have no meaningful value there. A `geompropvalue` reads no primvar
there: it gives its `default`, with an
[`mtlx.incomplete`](@/docs/reference/warnings.md) warning. An `image` that can't be loaded
falls back to its `default` (0.5 unless authored), as it does for shading.

```xml
<image name="height" type="float">
  <input name="file" type="filename" value="height.png" />
</image>
<displacement name="disp" type="displacementshader">
  <input name="displacement" type="float" nodename="height" />
  <input name="scale" type="float" value="0.3" />
</displacement>
<surfacematerial name="ground" type="material">
  <input name="surfaceshader" type="surfaceshader" nodename="surf" />
  <input name="displacementshader" type="displacementshader" nodename="disp" />
</surfacematerial>
```

A `vector3` `displacement` input is vector displacement, which is refused with a warning.

### RenderMan (PxrDisplace)

A material whose `outputs:ri:displacement` reaches a `PxrDisplace` (as on the Moana
Island) is displaced by `dispAmount` times `dispScalar`. `dispScalar` can be a value, or a
`PxrPtexture` whose file is read as data, optionally through a `PxrDispTransform` and its
remap mode, centre, depth and height. In place of the one `PxrPtexture`, a `PxrBlend`
that multiplies two of them (`operation = 18`) is read as the product of the two maps.
Inputs connected to the material's interface (`inputs:dispScale`,
`inputs:displacementMap`) are followed.

Any other network driving `dispScalar`, such as another `PxrBlend` operation or a UV
`PxrTexture`, is refused with a warning. RenderMan's
`primvars:displacementbound:sphere` on the mesh is read as its
[displacement bound](@/docs/usd/geometry.md#crust-displacementbound).

### Watertight by construction

A vertex shared by several faces moves once, sampled from the first face that uses it. A
mesh therefore can't crack along a UV seam or between Ptex faces, even where the map
itself jumps. Where it does jump, the step shows as one ring of stretched triangles at the
seam.

### Not supported

- **Vector displacement** is refused with a warning, and the mesh is not displaced.
- **Spheres** (`UsdGeomSphere`) are not displaced.

`CRUST_DISPLACE=0` turns displacement off for an A/B; see
[environment variables](@/docs/reference/environment-variables.md#crust-displace).

## Texture colour spaces

Textures are converted to linear light in the
[working colour space](@/docs/usd/render-settings.md#renderingcolorspace) once, when they
load. The colour space a texture is authored in comes from:

- the `colorspace` of a MaterialX `image` with a `color3` or `color4` output: the `file`
  input's own, else its node's, else its node graph's, else the document's (the root
  `<materialx colorspace="...">`). An image of any other type, such as a `float` roughness
  or a `vector3` normal map, is data and is never converted.
- the `colorSpace` metadata on a `UsdUVTexture`'s `inputs:file`, when authored; otherwise
  its `sourceColorSpace`.

| authored | converted from |
| --- | --- |
| any colour space the [OCIO config](@/docs/reference/command-line.md#ocio-config) defines, by any name or alias: `srgb_texture`, `g22_rec709`, `acescg`, `g22_ap1`, `lin_rec2020`, `Utility - sRGB - Texture`, … | that space: its transfer curve, then its primaries into the working space's |
| `srgb` (older MaterialX documents) | `srgb_texture` |
| MaterialX: `raw`, or an explicit empty `colorspace=""` | nothing: the values are taken as already in the working space |
| MaterialX: no colour space at any scope, on a `color3` / `color4` image | `auto`, as below |
| `UsdUVTexture`: `sRGB` / `raw` | `srgb_texture` / nothing |
| `UsdUVTexture`: `auto` or unauthored | `auto`, as below |

`auto` is decided by the file, as Arnold decides it: an image with integer samples (8- or
16-bit, any number of channels, so a greyscale PNG too) is `srgb_texture`, and a half or
float image (EXR, Radiance `.hdr`) is used as stored. Crust Render reads an unset
MaterialX colour image and `auto` this way so that it renders those textures as Arnold
does and shares their [`.tx` files](#tx-files). A greyscale roughness or mask you want read
as data needs `sourceColorSpace = "raw"`, or a MaterialX `float` image. A `UsdUVTexture`
feeding `displacement` stays data under `auto`.

So an sRGB albedo map rendered in ACEScg has its curve removed and its Rec.709 primaries
converted to ACEScg's, and an `acescg` texture rendered in the default `lin_rec709` is
converted the other way. Names are matched without regard to case. A colour that falls
outside the working space's gamut has its negative components set to zero. A name the
config doesn't know is refused with a warning, and the texture is used as stored.

Colour Ptex is decoded as `g22_rec709`, the 2.2 power law the Moana island's networks
apply, and converted into the working space. A Ptex displacement map isn't decoded.

### Constant colours

A constant colour — UsdPreviewSurface `diffuseColor` and `emissiveColor`, the
`crust:openpbr` colours, a light's `inputs:color` — is converted from the colour space
`UsdColorSpaceAPI` gives it: its attribute's `colorSpace` metadata, else the
`colorSpace:name` of its prim, else of the nearest ancestor that authors one. With none of
those it's taken as already in the working space.

```usda
def Scope "Looks" (
    prepend apiSchemas = ["ColorSpaceAPI"]
)
{
    uniform token colorSpace:name = "lin_rec709_scene"   # every colour below

    def Material "Red" { ... }
}

color3f inputs:diffuseColor = (0.8, 0.2, 0.1) (
    colorSpace = "srgb_texture"                          # this one only
)
```

A texture file's own colour space (`inputs:file`, a light's `texture:file`) is resolved
the same way.

A MaterialX `color3` or `color4` value is converted from its colour space (the input's,
its node's, its node graph's or the document's) when it has one. `PxrDisneyBsdf`
`baseColor` is decoded as `g22_rec709` unless its `colorSpace` metadata says otherwise.

## .tx files

A `.tx` is a tiled, mip-mapped texture file. Crust Render uses Arnold's convention for
them, so it and Arnold 7.5 share the same files. When the `.tx` Arnold would use stands
beside a UV texture, Crust Render streams the texture from it instead of loading the whole
image. [`--auto-tx`](@/docs/reference/command-line.md#auto-tx) creates the missing ones,
and `CRUST_TEX_STREAM=0` turns streaming off (see
[Environment variables](@/docs/reference/environment-variables.md#crust-tex-stream)).

A texture that is already a tiled, mip-mapped EXR streams from that file as it is: Crust
Render writes no `.tx` for it, even under `--auto-tx`, and does not look for one. Arnold's
auto-tx converts such a file too (to `<stem>_raw.exr.tx`); Crust Render leaves that copy
unread.

### Names

The name records the conversion, as Arnold's auto-tx names it:

| binding of `albedo.png` | `.tx` looked for |
| --- | --- |
| `srgb_texture`, `srgb_tx` or the full `sRGB Encoded Rec.709 (sRGB)` | `albedo_sRGB Encoded Rec.709 (sRGB)_ACEScg.png.tx` |
| `lin_rec709` | `albedo_Linear Rec.709 (sRGB)_ACEScg.png.tx` |
| the OCIO `Raw` space | `albedo_Raw_ACEScg.png.tx` |
| nothing authored, the keyword `raw`, or `ACEScg` itself | `albedo_raw.png.tx` |
| `auto` | the sRGB name for an 8- or 16-bit image, `_raw` for a float one |

The source space is the OCIO config's own name for it, never an alias, and the last part
is the target space, [`--tx-color-space`](@/docs/reference/command-line.md#tx-color-space)
(`ACEScg` by default). A UDIM number stays in the stem: `chair.1001.tif` becomes
`chair.1001_…_ACEScg.tif.tx`.

### What `--auto-tx` writes

The file Arnold 7.5 would write for the same binding, so Arnold uses it as its own and
does not rebuild it:

- an 8-bit source in a space with a transfer curve (sRGB) becomes a `half` OpenEXR
  with DWAA compression, holding linear values in the target space;
- an 8-bit source in a linear or data space becomes an 8-bit TIFF, converted to the
  target space;
- a 16-bit, `half` or float source keeps its sample type;
- every level is half the size of the one above, rounded down, and 64x64-tiled;
- an image whose three channels are equal everywhere is stored as one channel, an alpha
  that is 1 everywhere is left out, and an image of one colour is stored as one tile;
- the file carries Arnold's metadata, its `maketx` command line included, and its
  modification time is the source's.

A `.tx` is converted again when its modification time is not its source's (in whole
seconds, as Arnold compares it). Without `--auto-tx`, such a `.tx` is still used, with a
warning.

### How a `.tx` is read

- A `.tx` found beside its source under a converted name is read as linear values in the
  target space, then converted to the render's
  [working colour space](@/docs/usd/render-settings.md#renderingcolorspace).
- A `_raw` `.tx` is read in the binding's colour space, as its source would be.
- A `.tx` bound directly (the file path in the material is the `.tx` itself) is read in
  the binding's colour space, whatever the file holds, as Arnold reads it. A `.tx`
  already converted to ACEScg and bound as sRGB is converted again, and renders about
  four times too dark in both renderers: bind such a file with no colour space.

### Alpha

A `.tx` keeps its source's alpha when the alpha cuts something, and stores the colour
premultiplied by it, as OIIO and Arnold do. Every UV texture lookup returns premultiplied
colour, whether the texture is streamed or loaded whole, so a partly transparent texel
reads darker than its source's straight colour.

### Moving from earlier versions

Earlier versions of Crust Render wrote and looked for `<stem>.tx` (`albedo.tx`). Those
files are no longer looked for, and are left where they are: without `--auto-tx` the
texture is loaded whole, and with it the `.tx` is written again under its Arnold name.
Delete the old files once nothing else reads them.

A UsdUVTexture with `sourceColorSpace = "sRGB"` is read as sRGB and looks for the
canonical name above. Arnold, with Crust Render's OCIO config, passes the token as it is,
finds no colour space of that name, and writes `albedo_sRGB_ACEScg.png.tx` with the bytes
unconverted, so the two renderers don't share that file. Author `srgb_texture` instead,
which both read as sRGB under the same name.
