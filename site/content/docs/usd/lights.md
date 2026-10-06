+++
title = "Lights"
description = "Camera visibility and light groups of UsdLux lights."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
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
`inputs:shaping:ies:angleScale`, `inputs:shaping:ies:normalize`).

Light linking and shadow linking use the standard `collection:lightLink` and
`collection:shadowLink` collections. See `samples/light_linking.usda`.

Not supported: mesh lights (`MeshLightAPI`), portal lights, light filters, and shaping on
distant and dome lights. `inputs:diffuse` and `inputs:specular` are ignored with a
warning.

## Camera visibility

By default the **camera doesn't see the surface of an area light**, which is the usual
convention in Arnold, RenderMan and Karma. A light can sit inside the frame without
showing up, while it still lights the scene. Shadow and indirect rays still see the
surface: it casts shadows, and appears in reflections.

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

`int`, default: bits 1 and 2 (shadow and indirect), plus bit 0 (camera) when the light is
camera-visible.

[`crust:rayMask`](@/docs/usd/geometry.md#crust-raymask) works on the surface of an area
light too. When it is authored, it replaces the visibility above completely, and
`crust:light:cameraVisible` is ignored:

```usda
def SphereLight "Masked"
{
    float inputs:radius = 0.3
    int crust:rayMask = 7      # camera, shadow and indirect: fully visible
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
