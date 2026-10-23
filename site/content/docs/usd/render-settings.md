+++
title = "Render settings"
description = "The crust:* attributes of the RenderSettings prim."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-23T08:00:00+00:00
draft = false
weight = 20
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Sampling, light transport, filtering, path guiding, subdivision and the precomp, set on the stage’s <code>RenderSettings</code> prim.'
toc = true
top = false
+++

All the attributes on this page go on the `RenderSettings` prim. This is the prim named
by the stage's `renderSettingsPrimPath` metadata, or `/Render/settings` if there is none.
See [Overview](@/docs/usd/overview.md#which-prim-holds-the-render-settings). A
[render pass](@/docs/usd/render-passes.md#rendersource) rendered with `--pass` can name
another `RenderSettings` prim with its `renderSource`.

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
        int crust:lightSamples = 4
        int crust:lightSamplesIndirect = 1
        float crust:indirectClamp = 0

        # reconstruction
        token crust:pixelFilter = "gaussian"
        float crust:pixelFilterRadius = 2

        # the QMC sampler
        token crust:sampler = "sobol"

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
| `float4 dataWindowNDC` | `(0, 0, 1, 1)` | the part of the frame to render. The first render product's own value overrides it. See [below](#datawindowndc). |
| `token aspectRatioConformPolicy` | `expandAperture` | how the camera's aperture is fitted to the image when their aspect ratios differ. The first render product's own value overrides it. See [below](#aspectratioconformpolicy). |
| `rel camera` | first camera on the stage | the camera to render through. The first render product's own `camera` overrides it, and [`--camera`](@/docs/reference/command-line.md#camera) overrides both. |
| `rel products` | none | the `RenderProduct`s to write: output files and the AOVs in each. See [Render products and AOVs](@/docs/usd/aovs.md). Without products, the render writes one RGB EXR at [`-o`](@/docs/reference/command-line.md#output). |
| `bool disableMotionBlur` | `false` | render moving geometry sharp, where it is at the frame. The first render product's own value overrides it. See [below](#disablemotionblur). |
| `bool instantaneousShutter` | `false` | the same switch under its older `UsdRender` name. Either one set to `true` turns the blur off. |
| `bool disableDepthOfField` | `false` | render through a pinhole, sharp at every depth, whatever the camera's `fStop`. The first render product's own value overrides it. See [below](#disabledepthoffield). |
| `token renderingColorSpace` | `lin_rec709` | the working colour space. See [below](#renderingcolorspace). |

### disableMotionBlur

`uniform bool disableMotionBlur = 1`

Turns off motion blur for the whole render. `UsdRender` defines it as a `[0, 0]` shutter:
one sample at the current time code. Moving geometry therefore renders sharp where it is
at the frame, in the beauty and in every AOV: at its authored position for
[`crust:motion:translate`](@/docs/usd/geometry.md#crust-motion-translate), and at its
transform at the frame for a time-sampled one, whatever the camera's
[shutter](#camera-shutter). The motion itself stays in the scene: the
[`motionvector`](@/docs/usd/aovs.md#motion-vectors) AOV is still written, measuring the
shutter's motion from that sharp position, so the blur can be added in compositing from a
sharp beauty.

`instantaneousShutter` is the attribute's older, deprecated name in `UsdRender`, which
closes the shutter where it opens. It also turns the blur off, but the sharp image is at
shutter open, `frame + shutter:open`. The two agree unless the shutter opens before or
after the frame: with a `-0.25 .. 0.25` shutter, `instantaneousShutter` renders a quarter
frame before the frame. When both are set, `disableMotionBlur` wins.

Both are resolved like the render's camera and resolution, from the first `RenderProduct`
when it authors them, else from this prim. A product that authors only one of the two
inherits the other from this prim. A later product that asks for another setting is still
written, with the first product's, and a warning says so: one render has one shutter, as
it has one camera.

```usda
def RenderSettings "settings"
{
    uniform bool disableMotionBlur = 1
}
```

`samples/motionvector.usda` renders moving geometry this way.

### disableDepthOfField

`uniform bool disableDepthOfField = 1`

Turns off the camera's [depth of field](#camera-depth-of-field) for the whole render.
Every camera ray starts at the camera's position, as with a camera that authors no
`fStop`: the image is sharp at every depth. The field of view and the framing do not
change, and neither does anything else the camera authors. A stage with no camera
renders through crust's built-in fallback camera, and this closes its lens too.

It is resolved like `disableMotionBlur`: from the first `RenderProduct` when it
authors it, else from this prim. A later product that asks for the other setting is
still written, with the first product's, and a
[`product.depth_of_field_mismatch`](@/docs/reference/warnings.md) warning says so: one
render has one camera.

```usda
def RenderSettings "settings"
{
    uniform bool disableDepthOfField = 1
}
```

### dataWindowNDC

`uniform float4 dataWindowNDC = (0.5, 0, 1, 1)`

Renders only part of the frame: `(xmin, ymin, xmax, ymax)` in normalized coordinates of
the image, with `(0, 0)` at the **bottom-left** corner and `(1, 1)` at the top-right, as
`UsdRender` defines it. A pixel is rendered when its centre lies inside the window, so
the example renders the right half of the image. The default, `(0, 0, 1, 1)`, is the whole
frame.

The window changes which pixels are traced, nothing else: the camera, the resolution and
each pixel's samples are those of the full frame, so the rendered pixels match a full
render's. The EXR keeps the full resolution as its display window and has the region as
its data window; the PNG holds the region alone. See
[`--region`](@/docs/reference/command-line.md#region), which overrides this attribute
from the command line, in pixels.

It is resolved like the resolution: from the first `RenderProduct` when it authors one,
else from this prim. A later product that asks for another window is still written, over
the first product's region, and a warning says so: one render has one region, as it has
one camera.

- A window reaching outside `[0, 1]` (overscan) is clipped to the frame, with a warning.
  Rendering outside the frame is not supported.
- A window that selects no pixel, one lying wholly outside the frame included, is refused
  with a warning, and the full frame renders.

```usda
def RenderSettings "settings"
{
    uniform int2 resolution = (1920, 1080)
    uniform float4 dataWindowNDC = (0.25, 0.25, 0.75, 0.75)
}
```

### aspectRatioConformPolicy

`uniform token aspectRatioConformPolicy = "adjustApertureHeight"`

Decides what the render frames when the camera's aperture (`horizontalAperture` ×
`verticalAperture`) has another aspect ratio than the image (`resolution`). The aperture is
conformed to the image before the field of view is derived, as OpenUSD's
`CameraUtilConformWindow` does; the camera prim itself is not changed. For a 36 × 24 mm
aperture (3:2):

| policy | keeps | on a square image | on a 2:1 image |
|--------|-------|-------------------|----------------|
| `expandAperture` (default) | the whole aperture: one side grows | 36 × 36 | 48 × 24 |
| `cropAperture` | the aperture's middle: one side shrinks | 24 × 24 | 36 × 18 |
| `adjustApertureWidth` | the vertical aperture | 24 × 24 | 48 × 24 |
| `adjustApertureHeight` | the horizontal aperture | 36 × 36 | 36 × 18 |
| `adjustPixelAspectRatio` | the aperture, with non-square pixels | not supported: as `expandAperture` | not supported: as `expandAperture` |

`adjustApertureHeight` is the horizontal fit Maya and Nuke use; `adjustApertureWidth` is
a vertical fit. An aperture that already has the image's aspect, and a camera that authors
no `verticalAperture` (it follows the image's aspect), frame the same under every policy.

The policy is a render-settings attribute, read from the first `RenderProduct` when it
authors one, else from this prim, like the resolution. Authored on the **camera** prim it
is not read, as in OpenUSD. A later product that asks for another policy is still written,
with the first product's framing, and a warning says so. A token that is none of the five
is refused with a warning and read as `expandAperture`.

Crust renders square pixels only, so `adjustPixelAspectRatio`, which keeps the aperture
and changes the pixels' shape instead, frames as `expandAperture` and warns with the
`pixelAspectRatio` it would have needed (none when the aperture already fits).
`crust ls camera --json` reports each camera's authored `aperture_mm` and the
`conformed_aperture_mm` the render frames.

```usda
def RenderSettings "settings"
{
    uniform int2 resolution = (2048, 858)
    uniform token aspectRatioConformPolicy = "adjustApertureHeight"
}
```

#### Stages written for older versions

Before Crust Render read this attribute it always kept the vertical aperture, as
`adjustApertureWidth` does, whatever the stage asked for. Under the default
`expandAperture`, a camera whose aperture is **wider** than the image (a 36 × 24 mm
aperture on a square image, say) now shows its whole authored width instead of being
cropped at the sides. To keep the old framing, author
`aspectRatioConformPolicy = "adjustApertureWidth"` on the render settings. Cameras whose
aperture is narrower than the image, or as wide, frame as before.

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

`int`, default **32**. CLI: [`--min-samples`](@/docs/reference/command-line.md#min-samples).

The number of samples every pixel takes before adaptive sampling may stop it. It is never
lower than ⌈√`samplesPerPixel`⌉. A negative value is refused with a warning, and the
default is used.

When `crust:samplesPerPixel` is at or below this value, adaptive sampling never stops a
pixel early: every pixel takes the full count.

### crust:varianceThreshold

`float`, default **0.05**. CLI: [`--variance-threshold`](@/docs/reference/command-line.md#variance-threshold).

The adaptive sampling target. A pixel stops once the relative standard error of its
estimate is below this value, after at least `crust:minSamplesPerPixel` samples. A pixel
that hasn't seen any light yet keeps sampling.

Lower values mean less noise and longer renders.

### crust:adaptiveNeighbourTolerance

`float`, default **1.0**. CLI: [`--neighbour-tolerance`](@/docs/reference/command-line.md#neighbour-tolerance).

A pixel is not allowed to stop while one of its four direct neighbours is much less
converged than it is. This value is how much less converged a neighbour may be, in units
of the convergence measure. It stops isolated pixels in a noisy area from stopping too
early.

A negative value turns the comparison off. A value that isn't finite (`nan`, `inf`) is
refused with a warning, and the default is used.

## Light transport

### crust:maxDepth

`int`, default **32**.

The longest path, counted in shaded vertices rather than bounces: `1` shades only the
surface the camera sees, so it renders visible emitters and direct lighting with no
indirect light; `2` adds one bounce of indirect light, and so on. (In a renderer that
counts bounces, such as Arnold's `GI_total_depth`, `crust:maxDepth = N` is a depth of
`N - 1`.) Russian roulette usually ends paths sooner.

A value below `1` would shade nothing and render black, so it is refused with a warning
and the default is used. There is no upper bound: a very large value, such as
`2147483647`, means "no limit". Russian roulette then ends most paths, but it only
ends a path whose throughput has fallen below 1: in a closed scene of lossless
(albedo 1) surfaces a path keeps bouncing to the limit, so a render there may not
finish. Keep a finite depth for such scenes.

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
| `tree` | by a light BVH: at each shading point, lights are ranked by power, distance, which way they face, and whether they are above an opaque surface; a tenth of the picks still go by power, and lights at infinity keep their uniform share. With one finite light or none it picks as `power` does |
| `neural` | visibility-aware, experimental: the same pre-pass trains a small network that predicts, at any point, which lights reach it; picks by power weighted by that prediction. Up to 64 lights; past that, or with a single light, it picks by power |

An unknown name logs a warning and uses `power`.

### crust:lightSamples

`int`, default **1**. CLI: [`--light-samples`](@/docs/reference/command-line.md#light-samples).

How many light samples (shadow rays) light sampling takes at the first vertex of each
camera path. The samples spread over the lights in proportion to their selection
probabilities and are combined with BSDF sampling by multi-sample multiple importance
sampling, so the image is the same in expectation at every count; direct-light noise
falls about as 1/N. A value below 1 is refused with a warning, and 1 is used; a value
above 1024 is clamped to 1024 with a warning. At 1 the image is bit-identical to a
render before this setting existed.

### crust:lightSamplesIndirect

`int`, default **1**. CLI: [`--light-samples-indirect`](@/docs/reference/command-line.md#light-samples-indirect).

The same count at every later vertex of a path, surface and volume alike. It is paid at
every bounce, so it costs more per unit of noise removed than `crust:lightSamples`. A
value below 1 is refused with a warning, and 1 is used; a value above 1024 is clamped to
1024 with a warning.

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

## Sampler

### crust:sampler

`token`, default **`sobol`**. CLI: [`--sampler`](@/docs/reference/command-line.md#sampler).

The quasi-Monte Carlo sampler every number of the render is drawn from: the sub-pixel
offsets, the lens, the time, every light pick and every BSDF sample. OpenQMC's three base
sequences and their blue-noise variants:

| value | meaning |
|-------|---------|
| `sobol` | Owen-scrambled Sobol. **Default**, and bit-identical to every render before the setting existed. |
| `sobolBn` | Sobol, with the error distributed as blue noise across pixels. |
| `lattice` | rank-1 lattice. |
| `latticeBn` | rank-1 lattice, blue noise across pixels. |
| `pmj` | progressive multi-jittered (0, 2) sequence. |
| `pmjBn` | PMJ, blue noise across pixels. |

No sampler wins everywhere, so none replaces `sobol`. Measured against it, unclamped, with
a reference image: `lattice` lowered the error by 9% on the cornell box and raised it by 20%
on a MaterialX scene, so whether it helps depends on the scene. The `*Bn` variants converge to
the same error, but push it toward high frequencies, which a preview at a few samples a
pixel, a progressive render or a denoiser sees as less visible noise; they cost 3–10% more
render time. Use them for low-spp previews and denoised output, and compare on your scene
before changing a final render's sampler.

The sampler is recorded in every EXR as `crust:sampler`, and
[`crust diff`](@/docs/reference/command-line.md#diff) warns when two images were sampled
differently: their pixels then differ by noise even where both converge to the same image.
The pre-pass that trains `crust:lightSelection = "learned"` (or `"neural"`) always draws
from `sobol`, so the learned table is the same under every sampler and a comparison measures the sampler
alone.

An unknown name logs a warning and uses `sobol`.

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
- The camera must be named by `rel camera` on this prim, by `--camera`, or by
  [`crust:dicingCamera`](#crust-dicingcamera). Otherwise a warning is logged and the
  uniform level applies.
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

### crust:dicingCamera

`rel`, unset by default. No CLI flag.

The camera that geometry is tessellated for, when it is not the camera the image is seen
through. It sets:

- the levels and per-edge rates of
  [adaptive subdivision](#crust-subdivisionedgelength), and which faces count as in view;
- [how many segments](@/docs/usd/geometry.md#how-many-segments-a-curved-shape-gets) a
  cylinder, cone or capsule is cut into around its axis.

Unset, the render camera dices, as it always has. The render's resolution and framing
apply to the dicing camera as they do to the render camera. A render product's own
`camera` replaces the render camera, not the dicing camera.

Use it when the render camera moves but the geometry should not, for example:

- a fly-through of a static set, diced once from a camera that sees all of it;
- a Hydra viewport or an interactive session, where the stage's shot camera dices and
  the camera that tumbles only looks. Moving a render camera that does not dice is then
  an [in-place edit](@/docs/help/claude-desktop.md) rather than a re-import.

Geometry is diced for the dicing camera's view. Seen from a render camera that is much
closer, it can look coarse, and faces outside the dicing camera's view are not refined
at all.

A target that is not a camera, or no prim, is ignored with a
[`camera.dicing_not_a_camera`](@/docs/reference/warnings.md) warning, and the render
camera dices.

```usda
def RenderSettings "settings"
{
    rel camera = </World/ShotCam>
    rel crust:dicingCamera = </World/LayoutCam>
    float crust:subdivisionEdgeLength = 2
}
```

## Camera depth of field

A render camera that authors an `fStop` greater than 0 has a lens, and renders sharp
only at its `focusDistance`. The lens is a disk whose diameter, in scene units, is

```text
diameter = focalLength / fStop × 0.001 / metersPerUnit
```

`focalLength` is read in millimetres, as a camera's lens is specified, and the stage's
`metersPerUnit` converts it into scene units. A 50 mm lens at f/2 is 25 mm across: 2.5
units on a centimetre stage (`metersPerUnit = 0.01`), 0.025 units on a metre stage
(`metersPerUnit = 1`). `focusDistance` is in scene units. Unauthored, `focalLength` is
50, `focusDistance` 10 and `metersPerUnit` 0.01, USD's fallback. An `fStop` of 0, or
none, is a pinhole: no blur at all.

The field of view depends only on `focalLength` and the apertures, so neither `fStop`,
`focusDistance` nor `metersPerUnit` reframes the image.

A `metersPerUnit` that is not a positive number (0, say), or one so small that the lens
would be wider than a float can hold (`1e-42`), is refused with a
[`camera.invalid_meters_per_unit`](@/docs/reference/warnings.md) warning and read as
0.01. Only a camera with a lens checks it, so a pinhole render says nothing about it.
[`disableDepthOfField`](#disabledepthoffield) on the render settings turns the lens off
without editing the camera.

```usda
#usda 1.0
(
    metersPerUnit = 1
)

def Camera "cam"
{
    float focalLength = 50
    float fStop = 2.8
    float focusDistance = 4
}
```

**Stages written for older versions.** Crust Render used to take `focalLength / fStop`
as a diameter in scene units directly, which made the blur 10 times too strong on a
centimetre stage (and on one that authors no `metersPerUnit`) and 1000 times too strong
on a metre stage. Such a stage now renders much sharper. To keep an old look, divide its
`fStop` by the same factor: 10 on a centimetre stage, 1000 on a metre stage.

**Hydra.** OpenUSD's own camera (`GfCamera`, which usdview and hdEmbree use) reads lens
lengths in tenths of a scene unit whatever `metersPerUnit` says. That agrees with
millimetres only at `metersPerUnit = 0.01`. On a centimetre stage, or one that authors no
units, Crust Render's depth of field matches Hydra's. On any other stage it does not. A
metre stage exported in Hydra's convention, with `focalLength = 0.5` for a 50 mm lens,
gets a lens 100 times smaller than intended. Its framing is unaffected, because the
apertures are in the same units as the focal length.

## Camera shutter

The render camera's `shutter:open` and `shutter:close` say over which interval motion is
measured. They are offsets in time codes from the frame given by `-f`, and each defaults
to `0`, as USD defines it. With frame `f`, motion runs from `f + shutter:open` (shutter
open) to `f + shutter:close` (shutter close):

```usda
def Camera "cam"
{
    double shutter:open = 0
    double shutter:close = 0.5   # half a frame after the frame
}
```

Over that interval, every mesh, sphere, implicit shape, native instance and
`PointInstancer` placement whose transform is time-sampled (its own `xformOp`s or an
ancestor's) moves from its transform at shutter open to its transform at shutter close.
The beauty is blurred along that path, unless [`disableMotionBlur`](#disablemotionblur)
is set (the prim is then sharp where it is at the frame), and the
[`motionvector`](@/docs/usd/aovs.md#motion-vectors) AOV measures the same motion. Only the
transform is read at the two ends: every other attribute, points included, is read at the
frame, and prims that get no motion (curves, lights, volumes) sit where they are at the
frame.

The shutter is **instantaneous**, and nothing time-sampled moves, when:

- neither attribute is authored, or `shutter:close` is not after `shutter:open`. This
  is the default, so a stage that authors no shutter renders exactly as before;
- no frame is given. Time samples are then not evaluated, and a
  [`time.motion_needs_frame`](@/docs/reference/warnings.md) warning says so if the
  stage has any.

The shutter is read before the stage is walked, from the camera the render goes
through: the one named by `--camera` or `RenderSettings.camera`, even inside a payload,
else the first camera on the stage. If the render ends up going through another camera
whose shutter differs (for example, the first camera sits in an unloaded payload), a
[`camera.shutter_mismatch`](@/docs/reference/warnings.md) warning names both. Naming the
camera avoids it.

Only the shutter's two ends are read. A path that curves or turns back inside the
interval is rendered as the straight chord between them, and a rotation is interpolated
the same way, so a large turn within one shutter shrinks the object in the middle of the
blur. See [limitations](@/docs/architecture/limitations.md#geometry).

## Camera exposure

The render camera's exposure multiplies the image, as USD defines it
(`UsdGeomCamera::ComputeLinearExposureScale`). Five camera attributes give the scale:

```text
scale = exposure:time × exposure:iso × 2^exposure × exposure:responsivity
        / (100 × exposure:fStop × exposure:fStop)
```

Each one is read at the render's frame, and an unauthored one takes its schema default
(`exposure` 0, `exposure:time` 1, `exposure:iso` 100, `exposure:fStop` 1,
`exposure:responsivity` 1), which together give exactly 1. `exposure:fStop` is the
photometric f-stop. The lens's `fStop`, which sets the depth of field, does not change
the brightness. So `float exposure = 2` on the camera renders four times brighter.

The scale applies to the finished image only:

- The beauty and every [light path expression](@/docs/usd/aovs.md) AOV are multiplied
  by the scale. The `variance` AOV, and an expression's `crust:aov:variance`, are
  multiplied by its square.
- Depth, positions, normals, `st`, alpha, albedo, the diffuse filter, motion vectors
  and the sample count are untouched.
- Sampling, the [indirect clamp](#crust-indirectclamp) and adaptive sampling all work
  before the exposure, so an exposure changes the brightness and nothing else: the
  same samples, the same noise.

Every EXR records the scale as `crust:exposureScale`, `crust check` reports it as
`exposure_scale`, and `crust diff` notes when two files were written at different
scales. A camera whose attributes give a scale that is not a positive number (an
`exposure:fStop` of 0, say) is refused with a
[`camera.invalid_exposure`](@/docs/reference/warnings.md) warning, and the image is
not scaled.

If you raised your lights to make up for an exposure Crust Render used to ignore,
lower them again: the exposure now applies on top of them.

### crust:enableExposureCompensation

`bool`, default **true**.

Whether the render camera's exposure applies. `false` renders at a scale of 1 whatever
the camera authors.

Hydra's standard `enableExposureCompensation` setting, as used by hdEmbree, is read
too. If both are authored, `crust:enableExposureCompensation` wins.

```usda
def RenderSettings "settings"
{
    rel camera = </World/cam>
    bool enableExposureCompensation = false
}
```

## Precomp

Some looks are built in compositing rather than in the renderer: the surfaces write
AOVs (lighting splits, mattes, motion vectors) and a comp recombines them. A *precomp*
puts that recombination in the stage, next to the render settings, so `crust render`'s
PNG, [`--checkpoint`](@/docs/reference/command-line.md#checkpoint) and an MCP session
all show the look while the image refines. It is a preview of the look, not a
replacement for comp: no roto, no tracking, nothing across frames.

It has two stages, both optional:

- **Sample filters** run on every camera sample, from that sample's own AOV values,
  before it is accumulated. Their outputs are accumulated with the beauty's own
  pixel-filter weights, so an edge in a recombined matte is antialiased exactly as the
  beauty is, which a comp of finished pixels cannot do.
- **Display filters** are image operations on the accumulated image: blurs,
  convolutions, merges, grades and 2D motion blur.

A filter never changes the beauty: the EXR's beauty channels, the samples each pixel
takes and where adaptive sampling stops are bit-identical with and without a precomp.
Filter outputs are new channels, written by a [`crust:filter`
RenderVar](@/docs/usd/aovs.md#filter-outputs). The settings follow the active
[render pass](@/docs/usd/render-passes.md#rendersource), so two passes can carry two
precomps.

```usda
def RenderSettings "settings"
{
    rel products = </Render/Products/beauty>
    rel crust:sampleFilters = [</Render/Precomp/look>]
    rel crust:displayFilters = [</Render/Precomp/glow>, </Render/Precomp/speed>]
    token crust:display = "look_final"
}
```

`samples/precomp.usda` is a complete stage: three light groups, a matte and a motion
vector recombined into a stylised look, then a glow and speed lines.

### crust:sampleFilters

`rel`, ordered, default **none**.

Each target is a MaterialX `NodeGraph` of `ND_*` standard-library `Shader` prims, as
UsdShade authors any MaterialX network. Its interface inputs are what it reads, per
camera sample:

- `inputs:<name>` reads the sample's value of the RenderVar whose layer name is
  `<name>` (its `driver:parameters:aov:name`, else its prim name), in any product of the
  render. A `closest` var such as `depth` is read as the sample's own value, not the
  pixel's.
- `inputs:beauty` reads the sample's beauty radiance (after the firefly clamp, before
  the camera's exposure), unless a var is named `beauty`.
- An input named after a var that writes an earlier sample filter's output reads that
  output, for the same sample.
- An input that names nothing reads its authored value
  ([`precomp.unbound_input`](@/docs/reference/warnings.md#precomp)), and one that names
  a var with no per-sample value, a variance or the sample count, reads it too
  (`precomp.unbindable_input`).

Every `outputs:<name>` of the graph is an output a `crust:filter` var can write.

```usda
def NodeGraph "look"
{
    color3f inputs:key
    color3f inputs:fill
    float inputs:matte = 0
    color3f outputs:out.connect = </Render/Precomp/look/select.outputs:out>

    def Shader "warm"
    {
        uniform token info:id = "ND_multiply_color3"
        color3f inputs:in1.connect = </Render/Precomp/look.inputs:key>
        color3f inputs:in2 = (1, 0.72, 0.42)
        color3f outputs:out
    }

    def Shader "select"
    {
        uniform token info:id = "ND_ifgreater_color3"
        float inputs:value1.connect = </Render/Precomp/look.inputs:matte>
        float inputs:value2 = 0.5
        color3f inputs:in1.connect = </Render/Precomp/look/warm.outputs:out>
        color3f inputs:in2.connect = </Render/Precomp/look.inputs:fill>
        color3f outputs:out
    }
}
```

The graph may use the value nodes of the standard library: math (`add`, `multiply`,
`divide`, `power`, `min`, `max`, …), `mix`, `ifgreater` / `ifgreatereq` / `ifequal`,
`clamp`, `remap`, `smoothstep`, `contrast`, `invert`, `luminance`, `convert`,
`extract`, `combine2` / `combine3`, `colorcorrect`, `rgbtohsv` / `hsvtorgb`. A sample
has no shading point, so a texture (`image`), a `geompropvalue`, a geometry node
(`position`, `normal`, `texcoord`, …) or a closure (a BSDF) is refused, as is any node
crust does not implement (`hsvadjust`, for one): the graph's outputs then read 0
([`precomp.unsupported_node`](@/docs/reference/warnings.md#precomp)) and the render
proceeds. The graph is compiled once, by the same MaterialX compiler and JIT as
surfaces.

A sample filter costs one evaluation of its graph per camera sample, on top of the AOVs
it reads.

### crust:displayFilters

`rel`, ordered, default **none**.

Each target is a `Shader` prim whose `info:id` is one of the operations below. Its image
inputs are `token`s naming what they read: a RenderVar's layer name, a sample filter's
output as `<graph>.<output>` (`look.out`), an earlier display filter's prim name, or
`beauty`. A name matching none of them reads black
([`precomp.unbound_input`](@/docs/reference/warnings.md#precomp)); a filter that comes
later in the list cannot be read (`precomp.cycle`).

| `info:id` | inputs | does |
|-----------|--------|------|
| `crust:blur` | `in`, `float radius` (pixels, default 1) | a gaussian of standard deviation `radius / 3`, truncated at the radius |
| `crust:convolve` | `in`, `asset kernel`, `float kernelScale` (default 1), `bool normalize` (default true) | convolves `in` with an image, each texel covering `kernelScale` pixels; the kernel is normalised to sum 1 per channel unless `normalize` is false |
| `crust:vectorBlur` | `in`, then one of: `token motion` (a vector var), `float2 direction`, or `float2 center` and `float strength`; `int taps` (default 8) | averages `taps` lookups along each pixel's vector, centred on the pixel: the motion vectors, one direction for every pixel, or a radial blur away from `center` (pixels from the bottom-left of the image, default its centre) of `strength` times the distance to it. A pixel whose vector is zero is unchanged |
| `crust:merge` | `a`, `b`, `token operation` (`over`, `plus`, `multiply`, `screen`; default `over`), `alpha`, `float mix` (default 1) | per pixel: `over` is `b + a·(1 − alpha)` with `alpha` an image (1 when unconnected), the others as named; `mix` fades from `a` to the result |
| `crust:grade` | `in`, `color3f lift` (0), `gain` (1), `gamma` (1), `offset` (0) | Nuke's `Grade`: `(gain − lift)·x + offset + lift`, raised to `1/gamma` where positive |

Edges clamp: a blur near the image's border reads the border pixel. A render of a
[region](@/docs/usd/render-settings.md#datawindowndc) runs the filters on the region,
clamped at its edges, so a blurred crop differs from the same pixels of a full frame
near the crop's border.

An unknown `info:id` passes its `in` through
([`precomp.unknown_operation`](@/docs/reference/warnings.md#precomp)), and so does a
`crust:convolve` whose kernel cannot be read (`precomp.missing_kernel`).

Each operation is one pass over the image, independent of the sample count. The result
is bitwise the same for any number of threads, and in tiles or scanlines. `--stats`
reports the chain's time on its `precomp` line.

### crust:display

`token`, default **unauthored** (the beauty).

The output a host shows as *the* image: the name of a RenderVar of one of the products,
usually one reading a filter. It becomes the tone-mapped PNG (made even when the first
product has no beauty var), the `--checkpoint` preview and the image of an MCP session's
`render` and `snapshot`. The EXR's beauty stays the beauty. A one-channel output is shown
as grey.

With a displayed output, `--checkpoint` and session snapshots show the display filters
run over the units finished so far: an unfinished unit reads as black, and a blur bleeds
into it, as a progressive comp would.

A name matching no var shows the beauty
([`precomp.unknown_display`](@/docs/reference/warnings.md#precomp)).

### Exposure

Sample filters see the sample's values before the camera's
[exposure](#camera-exposure), and display filters see the exposed image. The exposure
multiplies a sample filter's output after accumulation, as it multiplies the beauty,
unless its var authors `bool crust:filter:radiance = 0`, as a matte or a ratio should.
A recipe moved from a sample filter to a display filter (or back) therefore sees its
inputs scaled differently: a threshold on `key` compares scene radiance in a sample
filter and exposed radiance in a display filter.

### A stage without a beauty

Adaptive sampling stops a pixel on the beauty's variance alone. A stage whose surfaces
write only AOVs, with no light, has a black beauty: every pixel stops at
`crust:minSamplesPerPixel` whatever its filter outputs look like
([`precomp.black_beauty`](@/docs/reference/warnings.md#precomp)). Author
`crust:minSamplesPerPixel` equal to `crust:samplesPerPixel` for such a stage, so every
pixel takes every sample.

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
