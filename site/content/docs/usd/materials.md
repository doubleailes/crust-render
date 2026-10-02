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
| a reference into a `.mtlx` file | read as a MaterialX graph: standalone BSDF nodes, `open_pbr_surface`, `standard_surface`, `gltf_pbr`, image and UDIM textures, normal maps |
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
