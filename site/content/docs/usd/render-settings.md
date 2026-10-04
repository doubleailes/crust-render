+++
title = "Render settings"
description = "The crust:* attributes of the RenderSettings prim."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
draft = false
weight = 20
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Sampling, light transport, filtering, path guiding and subdivision, set on the stage’s <code>RenderSettings</code> prim.'
toc = true
top = false
+++

All the attributes on this page go on the `RenderSettings` prim. This is the prim named
by the stage's `renderSettingsPrimPath` metadata, or `/Render/settings` if there is none.
See [Overview](@/docs/usd/overview.md#which-prim-holds-the-render-settings).

## Example

```usda
def Scope "Render"
{
    def RenderSettings "settings"
    {
        # standard UsdRender attributes
        rel camera = </World/cam>
        int2 resolution = (1920, 1080)

        # sampling
        int crust:samplesPerPixel = 512
        int crust:minSamplesPerPixel = 64
        float crust:varianceThreshold = 0.02
        int crust:maxDepth = 16

        # light transport
        token crust:samplingStrategy = "power"
        token crust:lightSelection = "learned"
        float crust:indirectClamp = 0

        # reconstruction
        token crust:pixelFilter = "gaussian"
        float crust:pixelFilterRadius = 2

        # path guiding
        bool crust:pathGuiding = true
        int crust:guidingTrainIterations = 8

        # geometry
        int crust:subdivisionLevel = 2
    }
}
```

## Standard attributes

Crust Render also reads these standard `UsdRenderSettings` attributes:

| attribute | default | meaning |
|-----------|---------|---------|
| `int2 resolution` | `(640, 360)` | image width and height in pixels. The first render product's own `resolution` overrides it. |
| `rel camera` | first camera on the stage | the camera to render through. The first render product's own `camera` overrides it, and [`--camera`](@/docs/reference/command-line.md#camera) overrides both. |
| `rel products` | none | the `RenderProduct`s to write: output files and the AOVs in each. See [Render products and AOVs](@/docs/usd/aovs.md). Without products, the render writes one RGB EXR at [`-o`](@/docs/reference/command-line.md#output). |
| `token renderingColorSpace` | `lin_rec709` | the working colour space. See [below](#renderingcolorspace). |

### renderingColorSpace

`uniform token renderingColorSpace = "acescg"`

The scene-linear colour space Crust Render renders in, by any name or alias of the
[OCIO config](@/docs/reference/command-line.md#ocio-config): `acescg` (or `lin_ap1_scene`,
`ACEScg`), `lin_rec2020`, `lin_rec709` (the default), and so on.
[`--working-space`](@/docs/reference/command-line.md#working-space) overrides it.
The default is `lin_rec709` whatever the config: it is not taken from the config's
`scene_linear` role (ACEScg in the builtin config), so a render doesn't change when the
config does.

Every colour that names its colour space — a texture's, a MaterialX `colorspace`, a
`colorSpace` metadatum — is converted into the working space when the scene loads. A
colour that names none is taken as already in it (see
[Texture colour spaces](@/docs/usd/materials.md#texture-colour-spaces)). Light transport
then happens in that space's primaries: a wider gamut such as ACEScg keeps saturated
colours that Rec.709 can't hold. The EXRs are written in the working space and say so in
their header (see [The EXR files](@/docs/usd/aovs.md#the-exr-files)).

A space that isn't scene-linear, such as `srgb_texture`, or one the config doesn't define,
is refused with a warning, and the render uses `lin_rec709`.

## Sampling

### crust:samplesPerPixel

`int`, default **128**. CLI: [`-s`, `--samples`](@/docs/reference/command-line.md#samples).

The maximum number of samples per pixel. Adaptive sampling can stop a pixel before it
gets there.

### crust:minSamplesPerPixel

`int`, default **32**.

The number of samples every pixel takes before adaptive sampling may stop it. It is never
lower than ⌈√`samplesPerPixel`⌉. A negative value is refused with a warning, and the
default is used.

When `crust:samplesPerPixel` is at or below this value, adaptive sampling never stops a
pixel early: every pixel takes the full count.

### crust:varianceThreshold

`float`, default **0.05**.

The adaptive sampling target. A pixel stops once the relative standard error of its
estimate is below this value, after at least `crust:minSamplesPerPixel` samples. A pixel
that hasn't seen any light yet keeps sampling.

Lower values mean less noise and longer renders.

### crust:adaptiveNeighbourTolerance

`float`, default **1.0**.

A pixel is not allowed to stop while one of its four direct neighbours is much less
converged than it is. This value is how much less converged a neighbour may be, in units
of the convergence measure. It stops isolated pixels in a noisy area from stopping too
early.

A negative value turns the comparison off. A value that isn't finite (`nan`, `inf`) is
refused with a warning, and the default is used.

## Light transport

### crust:maxDepth

`int`, default **32**.

The maximum number of bounces along a path. Russian roulette usually ends paths sooner.

### crust:indirectClamp

`float`, default **10**. CLI: [`--indirect-clamp`](@/docs/reference/command-line.md#indirect-clamp).

The firefly clamp. Each sample's indirect light is capped at this value in its largest
channel (linear units; the hue is kept). `0` turns the clamp off.

The clamp biases the image: it is the only biased default. Set it to `0` for reference
renders and measurements.

### crust:samplingStrategy

`token`, default **`power`**. CLI: [`--strategy`](@/docs/reference/command-line.md#strategy).

How light sampling and BSDF sampling are combined.

| value | meaning |
|-------|---------|
| `power` | power-heuristic (β = 2) multiple importance sampling. `mis` is accepted as another name for it. |
| `balance` | balance-heuristic multiple importance sampling |
| `light` | light sampling only (diagnostic) |
| `bsdf` | BSDF sampling only (diagnostic) |

An unknown name logs a warning and uses `power`.

### crust:lightSelection

`token`, default **`power`**. CLI: [`--light-selection`](@/docs/reference/command-line.md#light-selection).

How light sampling picks which light to sample.

| value | meaning |
|-------|---------|
| `power` | by emitted power, defensively: half the shadow rays are shared evenly among the finite lights, and lights at infinity keep their uniform share |
| `uniform` | every light is equally likely |
| `learned` | visibility-aware: a short pre-pass learns, for each region of the scene, which lights reach it |

An unknown name logs a warning and uses `power`.

## Pixel filter

### crust:pixelFilter

`token`, default **`triangle`**. CLI: [`--filter`](@/docs/reference/command-line.md#filter).

The pixel reconstruction filter.

| value | default radius (pixels) | meaning |
|-------|-------------------------|---------|
| `box` | 0.5 | one-pixel box |
| `triangle` | 1.0 | tent filter |
| `gaussian` | 1.5 | truncated Gaussian |
| `blackman` | 1.5 | 4-term Blackman–Harris window |
| `mitchell` | 2.0 | Mitchell–Netravali. Sharp, but its negative lobes can ring. |

An unknown name logs a warning and uses `triangle`.

### crust:pixelFilterRadius

`float`, default **the filter's own radius** (see the table above). CLI: [`--filter-radius`](@/docs/reference/command-line.md#filter-radius).

The filter radius in pixels, measured from the pixel center. Very small or negative
values are raised to a minimum of 0.01.

## Sampler seed

### crust:frame

`int`, default **0**.

Seeds the sampler, so that successive frames of an animation get different noise patterns
instead of the same one. It doesn't choose which time is rendered.

[`--frame`](@/docs/reference/command-line.md#frame) chooses the time and replaces this
seed with the frame number.

## Path guiding

Path guiding learns where light comes from during a few training passes. It then sends
part of the bounce rays in those directions. It helps scenes where most light arrives
along a few indirect paths, such as a room lit through a door. It is off by default.

### crust:pathGuiding

`bool`, default **false**.

Turns path guiding on.

### crust:guidingTrainIterations

`int`, default **4**, minimum 1.

The number of training passes before the final render.

### crust:guidingProb

`float`, default **0.5**.

The probability, at each bounce, of sampling the learned guide instead of the BSDF.

```usda
def RenderSettings "settings"
{
    bool crust:pathGuiding = true
    int crust:guidingTrainIterations = 8
    float crust:guidingProb = 0.5
}
```

`samples/cornellbox_guided.usda` is an example. Set `crust:pathGuiding = false` there to
compare with plain BSDF sampling at the same sample count.

## Geometry

### crust:subdivisionLevel

`int`, default **0**, maximum **6**. CLI: [`--subdiv-level`](@/docs/reference/command-line.md#subdiv-level).

How many times to refine every mesh whose `subdivisionScheme` is not `none`. An
unauthored `subdivisionScheme` counts as USD's default, `catmullClark`, so most exported
meshes are subdivision meshes.

- At **0**, nothing is refined. A subdivision mesh renders its control cage with smooth
  normals.
- Each level multiplies a mesh's face count by four. Values above 6 are clamped with a
  warning.

USD has no per-mesh refinement level, so this one level applies to the whole stage. To
keep a mesh from being subdivided, author `subdivisionScheme = "none"` on it.

{% alert(icon="⚠️") %}
Older scenes put `crust:subdivisionLevel` on each `Mesh`. That is no longer read: a
warning is logged once, and the mesh uses the `RenderSettings` level instead.
{% end %}

With [`crust:subdivisionEdgeLength`](#crust-subdivisionedgelength) set, this is the
highest level adaptive subdivision may choose.

### crust:subdivisionEdgeLength

`float`, in pixels, unset by default. CLI:
[`--subdiv-edge-length`](@/docs/reference/command-line.md#subdiv-edge-length).

Turns on adaptive subdivision. Each control-cage edge is cut until its segments are at
most this many pixels long, seen from the render camera at the edge's own distance. So
the near part of a mesh is refined and its far part keeps its cage.

- [`crust:subdivisionLevel`](#crust-subdivisionlevel) becomes the ceiling (2^level
  segments per edge), 3 when it is unauthored.
- Only geometry used once is adaptive. A prototype placed several times is refined to
  `crust:subdivisionLevel` (else 0) everywhere.
- Faces outside the camera's view are not refined.
- The camera must be named by `rel camera` on this prim or by `--camera`. Otherwise a
  warning is logged and the uniform level applies.
- A value that is not a positive number is ignored with a warning.

```usda
def RenderSettings "settings"
{
    rel camera = </World/Cam>
    float crust:subdivisionEdgeLength = 2
}
```

`samples/subdivision_adaptive.usda` places one cube five times at growing distances,
refined to levels 3, 3, 2, 1 and 0.

## Lights

### crust:domeLightCameraVisibility

`bool`, default **true**.

Whether camera rays see the lights at infinity: dome lights, distant lights and the
backdrop. `false` hides all of them from the camera, whatever the lights themselves
author. How they light the scene doesn't change.

Hydra's standard `domeLightCameraVisibility` setting, as used by usdview and hdEmbree, is
read too. If both are authored, `crust:domeLightCameraVisibility` wins.

To hide one dome light rather than all of them, use
[`crust:light:cameraVisible`](@/docs/usd/lights.md#crust-light-cameravisible) on that
light.
