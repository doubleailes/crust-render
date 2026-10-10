+++
title = "Overview"
description = "How Crust Render reads its own settings from a USD stage."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
draft = false
weight = 10
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Crust Render reads standard USD schemas, plus its own settings as custom attributes in the <code>crust:</code> namespace.'
toc = true
top = false
+++

## The `crust:` namespace

A `crust:*` attribute is an ordinary USD custom attribute. Other applications ignore it,
so you can add these attributes to a stage that is also rendered elsewhere. Declare each
one with the type given in this documentation, with or without the `custom` keyword:

```usda
def RenderSettings "settings"
{
    int crust:samplesPerPixel = 256
    custom float crust:indirectClamp = 0
}
```

**Types matter.** Crust Render reads each attribute with its documented type:

| documented type | you can author |
|-----------------|----------------|
| `int` | `int` only |
| `float` | `float` or `double` |
| `bool` | `bool`, or an `int` where non-zero means true |
| `token` | `token` or `string` |
| `float3` / `color3f` | `color3f`, `float3`, `vector3f`, `double3`, … (any 3-vector) |
| `float[]` | `float[]` or `double[]` |
| `int[]` | `int[]` only |

An attribute of the wrong type is ignored, and the default is used. The `crust:openpbr`
shader inputs are stricter; see [Materials](@/docs/usd/materials.md#input-types).

**Time samples work.** Like any USD attribute, a `crust:*` attribute can be animated with
`timeSamples`. It is read at the frame given by
[`--frame`](@/docs/reference/command-line.md#frame).

## Where each attribute goes

| attributes | on which prim | page |
|------------|---------------|------|
| `crust:samplesPerPixel`, `crust:maxDepth`, `crust:pixelFilter`, … | the `RenderSettings` prim | [Render settings](@/docs/usd/render-settings.md) |
| `crust:rayMask`, `crust:motion:translate` | geometry: `Mesh`, `Sphere`, `BasisCurves` | [Geometry](@/docs/usd/geometry.md) |
| `crust:light:cameraVisible` | UsdLux lights | [Lights](@/docs/usd/lights.md) |
| `info:id = "crust:openpbr"` and its `inputs:*` | a `Shader` prim | [Materials and textures](@/docs/usd/materials.md) |
| `crust:volume:*` | any prim, typically a `Cube` | [Volumes](@/docs/usd/volumes.md) |

## Which prim holds the render settings

Crust Render looks for the render settings on:

1. the prim named by the stage's `renderSettingsPrimPath` metadata, or else
2. `/Render/settings`.

That prim should be a `RenderSettings` prim. Its standard `resolution` and `camera`
attributes are read as well:

```usda
#usda 1.0
(
    renderSettingsPrimPath = "/Render/settings"
)

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/cam>
        int2 resolution = (1920, 1080)
        int crust:samplesPerPixel = 512
    }
}
```

Without a `RenderSettings` prim, every setting takes its default: 640×360,
128 samples per pixel, and so on.

## Precedence

When a value is set in more than one place, the first one in this list wins:

1. the command-line flag (for example `--samples`),
2. the `crust:*` attribute on the stage,
3. the built-in default.

A few Crust attributes have a standard or third-party equivalent. When both are authored,
the `crust:` attribute wins:

| Crust attribute | also read |
|-----------------|-----------|
| `crust:domeLightCameraVisibility` | `domeLightCameraVisibility` (Hydra) |
| `crust:enableExposureCompensation` | `enableExposureCompensation` (Hydra) |
| `crust:light:cameraVisible` | `primvars:ri:attributes:visibility:camera` (RenderMan) |

## All attributes

| attribute | type | prim | default |
|-----------|------|------|---------|
| [`crust:samplesPerPixel`](@/docs/usd/render-settings.md#crust-samplesperpixel) | `int` | RenderSettings | 128 |
| [`crust:minSamplesPerPixel`](@/docs/usd/render-settings.md#crust-minsamplesperpixel) | `int` | RenderSettings | 32 |
| [`crust:varianceThreshold`](@/docs/usd/render-settings.md#crust-variancethreshold) | `float` | RenderSettings | 0.05 |
| [`crust:adaptiveNeighbourTolerance`](@/docs/usd/render-settings.md#crust-adaptiveneighbourtolerance) | `float` | RenderSettings | 1.0 |
| [`crust:maxDepth`](@/docs/usd/render-settings.md#crust-maxdepth) | `int` | RenderSettings | 32 |
| [`crust:indirectClamp`](@/docs/usd/render-settings.md#crust-indirectclamp) | `float` | RenderSettings | 10 |
| [`crust:samplingStrategy`](@/docs/usd/render-settings.md#crust-samplingstrategy) | `token` | RenderSettings | `power` |
| [`crust:lightSelection`](@/docs/usd/render-settings.md#crust-lightselection) | `token` | RenderSettings | `power` |
| [`crust:pixelFilter`](@/docs/usd/render-settings.md#crust-pixelfilter) | `token` | RenderSettings | `triangle` |
| [`crust:pixelFilterRadius`](@/docs/usd/render-settings.md#crust-pixelfilterradius) | `float` | RenderSettings | per filter |
| [`crust:frame`](@/docs/usd/render-settings.md#crust-frame) | `int` | RenderSettings | 0 |
| [`crust:subdivisionLevel`](@/docs/usd/render-settings.md#crust-subdivisionlevel) | `int` | RenderSettings | 0 |
| [`crust:domeLightCameraVisibility`](@/docs/usd/render-settings.md#crust-domelightcameravisibility) | `bool` | RenderSettings | true |
| [`crust:enableExposureCompensation`](@/docs/usd/render-settings.md#crust-enableexposurecompensation) | `bool` | RenderSettings | true |
| [`crust:rayMask`](@/docs/usd/geometry.md#crust-raymask) | `int` | geometry, lights | 7 (geometry) |
| [`crust:motion:translate`](@/docs/usd/geometry.md#crust-motion-translate) | `float3` | Mesh, Sphere | none |
| [`crust:light:cameraVisible`](@/docs/usd/lights.md#crust-light-cameravisible) | `bool` | lights | false (true for domes) |
| [`info:id = "crust:openpbr"`](@/docs/usd/materials.md#the-crust-openpbr-shader) | `token` | Shader | — |
| [`crust:volume:type`](@/docs/usd/volumes.md#crust-volume-type) and the other `crust:volume:*` | various | any | — |
