+++
title = "Lights"
description = "Camera visibility and light groups of UsdLux lights."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-09T08:00:00+00:00
draft = false
weight = 40
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Lights are standard UsdLux prims. Crust attributes only decide whether the camera sees a light’s surface.'
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

An input a light doesn't author takes the fallback the UsdLux schema gives it for that
light's type, as in other UsdLux renderers. A blocked input, or a value that isn't a
finite number (refused with a warning), takes the same fallback.

| input | fallback |
|-------|----------|
| `inputs:intensity` | **50000** on a `DistantLight`, 1 on every other light |
| `inputs:exposure` | 0 |
| `inputs:color` | (1, 1, 1) |
| `inputs:normalize` | false |
| `inputs:enableColorTemperature`, `inputs:colorTemperature` | false, 6500 |
| `inputs:radius` (sphere, disk, cylinder) | 0.5 |
| `inputs:length` (cylinder), `inputs:width`, `inputs:height` (rect) | 1 |
| `inputs:angle` (distant) | 0.53 |

A distant light's `inputs:intensity` is the luminance of the sun's disk in nits, which is
why its fallback is so high. An unauthored 0.53° sun puts about 3.4 lux on a surface
facing it. Earlier releases used 1 for every light, which left a distant light that
authors no intensity 50000 times too dark.

A lat-long dome is oriented as the UsdLux schema specifies (the OpenEXR convention): in
the light's own frame the top row is +Y, the centre of the image faces **+Z**, a quarter
of the way in faces +X and three quarters faces −X. The prim's transform then rotates
that sky, so other UsdLux renderers place an HDRI's sun in the same direction. Releases
before 0.5.2 put −Z at the image centre. A scene whose dome rotation was tuned on those
releases needs 180° more about Y to keep its sun where it was.

`inputs:color` and `inputs:shaping:focusTint` are in the
[working colour space](@/docs/usd/render-settings.md#renderingcolorspace), as UsdLux
specifies, unless a colour space is authored for them: `colorSpace` metadata on the
attribute, or a `UsdColorSpaceAPI` `colorSpace:name` on the light or an ancestor (see
[Constant colours](@/docs/usd/materials.md#constant-colours)). A colour temperature's tint
is computed straight into the working space, at unit luminance there. An image file is
decoded from the colour space authored for it the same way; with none, an 8-bit image is
sRGB and a float image (EXR, `.hdr`) is taken as already in the working space.

Area lights also read `ShapingAPI`: the cone (`inputs:shaping:cone:angle`,
`inputs:shaping:cone:softness`), focus (`inputs:shaping:focus`,
`inputs:shaping:focusTint`) and IES profiles (`inputs:shaping:ies:file`,
`inputs:shaping:ies:angleScale`, `inputs:shaping:ies:normalize`). These inputs are read
even on a light that doesn't apply `ShapingAPI`. They fall back to the schema's
values (no focus, a black focus tint, no softness, no IES scaling), with one
exception: the cone. An unauthored `inputs:shaping:cone:angle` is the schema's **90°**
only when the light applies `ShapingAPI`. Without the API it is **180°**, no cone at
all, because the 90° belongs to the API and would otherwise cut off the back half of
every sphere light.

Light linking and shadow linking use the standard `collection:lightLink` and
`collection:shadowLink` collections. See `samples/light_linking.usda`.

Not supported: mesh lights (`MeshLightAPI`), portal lights, light filters, `ShadowAPI`,
and shaping on distant and dome lights. `inputs:diffuse` and `inputs:specular` are
ignored with a warning. A `DomeLight_1`'s `poleAxis` is not read either: its pole is
always the light's +Y, as a `DomeLight`'s is. On a Z-up stage, the schema's fallback
(`"scene"`) would put the pole on +Z. Rotate such a dome with its transform instead.

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

## Emissive materials

An emissive material (for example a `crust:openpbr` shader with
`inputs:emissionLuminance` above 0) lights the scene through the path tracer. It is not
sampled like a UsdLux light, so use a UsdLux light for a scene's main light sources.
