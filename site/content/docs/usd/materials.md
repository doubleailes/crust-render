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
| a reference into a `.mtlx` file | read as a MaterialX graph: standalone BSDF nodes, `open_pbr_surface`, `standard_surface`, `gltf_pbr`, image and UDIM textures, normal maps, [hair](#hair) |
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

A universal `outputs:surface` with a decodable shader, such as a `UsdPreviewSurface`, is
still used first. The `mtlx` terminal is read when the material has nothing else Crust
Render can decode, so a stage that rendered through its preview surface renders the same.

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
so use a UsdLux light for a scene's main light sources.

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
on the view direction have no meaningful value there. An `image` that can't be loaded
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
| MaterialX: no colour space, or `raw` | nothing: the values are taken as already in the working space |
| `UsdUVTexture`: `sRGB` / `raw` | `srgb_texture` / nothing |
| `UsdUVTexture`: `auto` or unauthored | `srgb_texture` for an 8-bit RGB or RGBA image, nothing otherwise |

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

A `.tx` is a tiled, mip-mapped texture file. When one exists beside a UV texture (same
path, `.tx` extension), Crust Render streams the texture from it instead of loading the
whole image. [`--auto-tx`](@/docs/reference/command-line.md#auto-tx) creates the missing
ones, and `CRUST_TEX_STREAM=0` turns streaming off (see
[Environment variables](@/docs/reference/environment-variables.md#crust-tex-stream)).

Every mip level of a `.tx` has to be reduced in the same colour space the renderer
decodes the texture in. Otherwise the full-resolution level looks right and the coarser
levels are wrong, which looks like a filtering bug. So a `.tx` that Crust Render writes
records that colour space in its `ImageDescription` tag:

```text
crust:mipspace=srgb_texture
```

The values are `srgb_texture`, `g22_rec709`, `g18_rec709` and `raw`. If the recorded colour
space doesn't match the one the material asks for, the `.tx` is refused with a warning and
the source image is loaded instead.

A `.tx` with no `crust:mipspace` tag, for example one written by OpenImageIO's `maketx`,
is accepted as it is.
