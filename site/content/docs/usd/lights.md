+++
title = "Lights"
description = "UsdLux lights, mesh lights, camera visibility and light groups."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-22T08:00:00+00:00
draft = false
weight = 40
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Lights are standard UsdLux prims, and meshes with MeshLightAPI. Crust attributes only decide whether the camera sees a light’s surface.'
toc = true
top = false
+++

## Supported lights

Crust Render reads these UsdLux light types with their standard `inputs:intensity`,
`inputs:exposure`, `inputs:color`, `inputs:normalize`, `inputs:enableColorTemperature` and
`inputs:colorTemperature` attributes:

| light | notes |
|-------|-------|
| `SphereLight` | `inputs:radius` |
| `DiskLight` | `inputs:radius` |
| `CylinderLight` | `inputs:radius`, `inputs:length` |
| `RectLight` | `inputs:width`, `inputs:height`, and an optional `inputs:texture:file` |
| `DistantLight` | `inputs:angle` |
| `DomeLight` | `inputs:texture:file`, a lat-long environment map (`inputs:texture:format` unauthored, `latlong` or `automatic`) |
| `DomeLight_1` | the same inputs as `DomeLight`, plus `poleAxis` (see below) |

A lat-long dome is oriented as the UsdLux schema specifies (the OpenEXR convention): in
the light's own frame the top row is +Y, the centre of the image faces **+Z**, a quarter
of the way in faces +X and three quarters faces −X. The prim's transform then rotates
that sky, so other UsdLux renderers place an HDRI's sun in the same direction. Releases
before 0.5.2 put −Z at the image centre. A scene whose dome rotation was tuned on those
releases needs 180° more about Y to keep its sun where it was.

### poleAxis

A `DomeLight_1` first turns that frame onto its `poleAxis`, as the schema specifies:

| `poleAxis` | stage `upAxis` | the dome's top pole | the image centre faces |
|------------|----------------|---------------------|------------------------|
| `scene` (the default) | `Y`, or not authored | +Y | +Z |
| `scene` (the default) | `Z` | **+Z** | **−Y** |
| `Y` | any | +Y | +Z |
| `Z` | any | **+Z** | **−Y** |

The turn is +90° about X, the rotation OpenUSD's own imaging computes. It applies to the
dome alone, before the prim's transform, and never to the prim's children. The stage's
`upAxis` is read from the root layer, as every USD stage reads its metadata, so a wrapper
layer that sublayers a Z-up asset must repeat `upAxis = "Z"`.

A `DomeLight` always keeps its pole on +Y, even when it authors a `poleAxis`. That
schema declares no such attribute, and OpenUSD's imaging ignores it there too. To get
the pole turned, author the prim as `DomeLight_1`. To keep a Z-up `DomeLight_1` lit as
a Y-pole sky, for instance one tuned in a renderer that ignores `poleAxis` (the OpenUSD
reference renderer, Typhoon, is one), author `poleAxis = "Y"`.

Earlier releases did not read `DomeLight_1` at all: a stage lit only by one rendered
black.

`inputs:color` and `inputs:shaping:focusTint` are in the
[working colour space](@/docs/usd/render-settings.md#renderingcolorspace), as UsdLux
specifies, unless a colour space is authored for them: `colorSpace` metadata on the
attribute, or a `UsdColorSpaceAPI` `colorSpace:name` on the light or an ancestor (see
[Constant colours](@/docs/usd/materials.md#constant-colours)). A colour temperature's tint
is computed straight into the working space, at unit luminance there. An image file is
decoded from the colour space authored for it the same way; with none, an 8-bit image is
sRGB and a float image (EXR, `.hdr`) is taken as already in the working space. An EXR's
colour is its first layer's `R`, `G` and `B` channels, matched by base name, so a
layer-prefixed `rgb.R` reads too; a single channel, or a `Y` channel, is grey.

Area lights also read `ShapingAPI`: the cone (`inputs:shaping:cone:angle`,
`inputs:shaping:cone:softness`), focus (`inputs:shaping:focus`,
`inputs:shaping:focusTint`) and IES profiles (`inputs:shaping:ies:file`,
`inputs:shaping:ies:angleScale`, `inputs:shaping:ies:normalize`).

Light linking and shadow linking use the standard `collection:lightLink` and
`collection:shadowLink` collections. See `samples/light_linking.usda`.

A `Mesh` with `MeshLightAPI` applied is a light too: see [Mesh lights](#mesh-lights).

Not supported: portal lights, `GeometryLight`, `VolumeLightAPI`, light filters, and
shaping on distant and dome lights.

## Diffuse and specular multipliers

`inputs:diffuse` and `inputs:specular` (`float`, 1 by default) scale a light's
contribution to the diffuse and the specular response of the surfaces it lights. Every
light type reads them.

| multiplier | scales |
|------------|--------|
| `inputs:specular` | specular, coat and transmission lobes; a hair fibre's primary reflection (R) |
| `inputs:diffuse` | diffuse, sheen, subsurface and translucent lobes; a hair fibre's other terms (TT, TRT and beyond) |

A light with `inputs:diffuse = 0` is a specular-only "kicker", the usual way to put
highlights in hair without washing out its colour; a fill light with
`inputs:specular = 0` lights without a highlight:

```usda
def SphereLight "HairKicker"
{
    float inputs:intensity = 40
    float inputs:diffuse = 0
}
```

The multipliers are non-physical, and any finite value is taken, negative or above 1. A
non-finite one falls back to 1 with the warning `light.non_finite_input`. Light that a
volume scatters is not scaled: the multipliers apply to surfaces only.

In [light path expression AOVs](@/docs/usd/aovs.md), the light a multiplied light sends
straight to a surface is split by lobe exactly. The part a surface receives through a
reflected or refracted ray is shared between that surface's lobes at the multipliers'
average for that ray. The AOVs still add up to the beauty, but `C<RD>L` can hold some of
a specular-only light's reflection.

## Camera visibility

By default the **camera doesn't see the surface of an area light**, which is the usual
convention in Arnold, RenderMan and Karma. A light can sit inside the frame without
showing up, while it still lights the scene.

Such a **hidden light** is an emitter and nothing else, as in the OpenUSD reference
renderer (hdEmbree): its surface casts no shadow, so two hidden lights never shadow each
other and whatever is behind one is lit as if it were not there. A ray bouncing through
it picks up its light and carries on past it, so it still appears in reflections and
still lights the scene indirectly.

A light the camera **does** see (`crust:light:cameraVisible = 1`) is solid, like a lamp
bulb: it casts shadows and blocks whatever is behind it. To keep a light hidden from the
camera but solid, author [`crust:rayMask = 6`](#crust-raymask-on-a-light).

Lights at infinity (dome and distant lights) are the opposite: the camera **does** see
them by default, as the sky behind the scene.

### crust:light:cameraVisible

`bool`, default **false** for area lights, **true** for dome and distant lights.

Whether camera rays see this light.

```usda
# A Cornell-box style light, visible as a bright patch in the image.
def RectLight "Ceiling"
{
    float inputs:width = 1
    float inputs:height = 1
    float inputs:intensity = 10
    bool crust:light:cameraVisible = 1
}

# A dome that lights the scene but leaves the background black.
def DomeLight "Sky"
{
    asset inputs:texture:file = @sky.exr@
    bool crust:light:cameraVisible = 0
}
```

If `crust:light:cameraVisible` isn't authored, RenderMan's
`primvars:ri:attributes:visibility:camera` is read instead (an `int`, non-zero meaning
visible). Published assets such as the Moana Island carry it on their lights.

To hide **every** dome and distant light from the camera at once, use
[`crust:domeLightCameraVisibility`](@/docs/usd/render-settings.md#crust-domelightcameravisibility)
on the `RenderSettings` prim.

### crust:rayMask on a light

`int`, default: bit 2 (indirect) for a hidden light, all three bits (camera, shadow and
indirect) for a camera-visible one.

[`crust:rayMask`](@/docs/usd/geometry.md#crust-raymask) works on the surface of an area
light too. When it is authored, it replaces the visibility above completely,
`crust:light:cameraVisible` is ignored, and the surface is **solid** whatever bits the mask
leaves: a ray that reaches it stops there.

```usda
def SphereLight "Masked"
{
    float inputs:radius = 0.3
    int crust:rayMask = 7      # camera, shadow and indirect: fully visible
}

def SphereLight "HiddenButSolid"
{
    float inputs:radius = 0.3
    int crust:rayMask = 6      # hidden from the camera, but it casts shadows
}
```

`crust:rayMask` has no effect on dome and distant lights: they have no surface to mask.

`samples/light_visibility.usda` shows the three cases side by side: the default, a light
with `crust:light:cameraVisible = 1`, and a light with `crust:rayMask = 7`.

## Light groups

### crust:light:lpeTag

`token`, unset by default.

The light's group for [light path expression AOVs](@/docs/usd/aovs.md#light-groups): the
label its `L` events carry, so `C.*<L.'key'>` holds the light of every light tagged
`key`. Any light type takes it, dome and distant lights included. An empty tag is no tag.

```usda
def RectLight "Key"
{
    token crust:light:lpeTag = "key"
}
```

Other renderers' light-group attributes (Karma's, RenderMan's, Arnold's) are not read
yet.

## Mesh lights

Apply `MeshLightAPI` to a `Mesh` to make it a light. Crust then samples its triangles
directly, as it samples a `RectLight`, instead of waiting for a reflected ray to hit it
by chance. A fire card, a screen, a sign or a lamp modelled as geometry lights the scene
with far less noise. The mesh keeps its material and stays in the picture:

```usda
def Mesh "FireCard" (
    prepend apiSchemas = ["MeshLightAPI"]
)
{
    rel material:binding = </Looks/FireGlow>
    # points, faceVertexCounts, faceVertexIndices ...
}
```

The mesh is the surface it renders with, after subdivision and displacement, and it
emits from both sides. Light linking, shadow linking, `inputs:diffuse`,
`inputs:specular`, `inputs:enableColorTemperature` and `crust:light:lpeTag` apply as for
any light. `crust ls light` lists the mesh with the kind `mesh`.

### What a mesh light emits

`light:materialSyncMode` decides the light's radiance. With `L` the light's own inputs,
`inputs:color` × 2^`inputs:exposure` × `inputs:intensity` (divided by the mesh's area
under `inputs:normalize = 1`):

| `light:materialSyncMode` | radiance | the material |
|--------------------------|----------|--------------|
| `materialGlowTintsLight` (the default) | the material's glow × `L` | reflects and emits |
| `independent` | `L` | reflects; its own glow is ignored |
| `noMaterialResponse` | `L` | ignored: the mesh only emits |

With the light's inputs unauthored, `L` is 1. So applying `MeshLightAPI` to an emissive
mesh changes how noisy it is, not how bright it is.

Crust gives the camera, reflections and light sampling one radiance. The USD schema lets
"light rays" see the light's inputs while the camera sees the material's glow. Crust
does not split them, because an image would then depend on how it is sampled rather
than on the scene. RenderMan follows the schema's split, so a mesh light that authors
inputs far from its glow can look different there.

### Visibility

A mesh light is scene geometry, so the camera sees it and it casts shadows by default,
unlike a UsdLux light. `crust:light:cameraVisible = 0` (or RenderMan's
`primvars:ri:attributes:visibility:camera = 0`) hides it from the camera only. It keeps
lighting the scene, appears in reflections and still casts shadows. An authored
`crust:rayMask` decides outright, as on any geometry.

### What cannot be a mesh light

These meshes keep emitting through the path tracer only, as without the API, each with a
[warning](@/docs/reference/warnings.md):

- A glow that varies over the surface or with the view (a texture, a primvar, a coat or a
  fuzz) under `materialGlowTintsLight`. Author `light:materialSyncMode = "independent"`
  and the light's inputs to light it with a flat colour.
- A cutout material.
- A mesh inside an instance prototype (a native instance or a `PointInstancer`).
- A mesh that moves during the shutter.
- `MeshLightAPI` on anything but a `Mesh`.
- A mesh light that emits nothing.

### Limits

Crust picks points on the mesh by area. A large card very close to what it lights is
noisier than an equal `RectLight`, which crust samples by the solid angle it covers. A
mesh is one light, so `--light-selection tree` ranks the whole mesh rather than its
parts. Crust keeps its own copy of the light's triangles, about 40 bytes each.

## Emissive materials

An emissive material (for example a `crust:openpbr` shader with
`inputs:emissionLuminance` above 0) lights the scene through the path tracer. Without
`MeshLightAPI` it is not sampled like a light: only rays that happen to hit it find it,
so a small or distant bright emitter is noisy. Apply [`MeshLightAPI`](#mesh-lights) to
its mesh, or use a UsdLux light for a scene's main light sources.
