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
