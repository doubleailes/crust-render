+++
title = "Materials and textures"
description = "The crust:openpbr shader and its inputs, and the other material types Crust Render reads."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
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
| a reference into a `.mtlx` file | read as a MaterialX graph: standalone BSDF nodes, `open_pbr_surface`, `standard_surface`, `gltf_pbr`, image and UDIM textures, normal maps |

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
| `inputs:fuzzWeight` | `float` | 0.0 | weight of the fuzz (sheen) layer |
| `inputs:fuzzColor` | `color3f` | (1, 1, 1) | fuzz colour |
| `inputs:fuzzRoughness` | `float` | 0.5 | fuzz roughness |

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
`crust:openpbr` colours, a light's `inputs:color` — is taken as already in the working
space, unless its attribute carries `colorSpace` metadata, in which case it's converted
from that space:

```usda
color3f inputs:diffuseColor = (0.8, 0.2, 0.1) (
    colorSpace = "srgb_texture"
)
```

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
