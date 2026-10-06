+++
title = "Render products and AOVs"
description = "RenderProduct and RenderVar: which files a render writes, and which AOVs go in them."
date = 2026-10-03T08:00:00+00:00
updated = 2026-10-03T08:00:00+00:00
draft = false
weight = 25
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'AOVs are asked for the way USD defines it: <code>RenderProduct</code>s on the <code>RenderSettings</code> prim, each listing its <code>RenderVar</code>s. Scenes exported from Houdini Solaris work unchanged.'
toc = true
top = false
+++

## How a render asks for AOVs

AOVs (arbitrary output variables, or render passes) are requested only through the
`UsdRender` schema. There is no `crust:*` attribute and no command-line flag for them.

- The `RenderSettings` prim lists its products in `rel products`.
- Each `RenderProduct` is one output file, named by `productName`. It lists its channels
  in `rel orderedVars`, in order.
- Each `RenderVar` is one layer of channels. `sourceName` says what to compute,
  `sourceType` how to read that name, and `dataType` its type.

A stage without `products` renders exactly as before: one RGB EXR at
[`-o`](@/docs/reference/command-line.md#output) (default `output.exr`), plus the PNG.

```usda
def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/Cam>
        int2 resolution = (1920, 1080)
        rel products = [</Render/Products/beauty>, </Render/Products/data>]
    }

    def Scope "Products"
    {
        def RenderProduct "beauty"
        {
            token productName = "renders/beauty.0001.exr"
            rel orderedVars = [</Render/Products/Vars/C>]
        }

        def RenderProduct "data"
        {
            token productName = "renders/data.0001.exr"
            rel orderedVars = [</Render/Products/Vars/Z>, </Render/Products/Vars/N>]
        }

        def Scope "Vars"
        {
            def RenderVar "C"
            {
                uniform token dataType = "color4f"
                uniform string sourceName = "Ci"
            }

            def RenderVar "Z"
            {
                uniform token dataType = "float"
                uniform string sourceName = "depth"
            }

            def RenderVar "N"
            {
                uniform token dataType = "normal3f"
                uniform string sourceName = "normal"
            }
        }
    }
}
```

`samples/aovs.usda` asks for every source on this page.

## Products

A product starts from the settings prim's `camera` and `resolution` and overrides only
what it authors itself, as `UsdRenderComputeSpec` does.

- **One camera and resolution per render.** They are the first product's. Usually that
  means the settings' own values. [`--camera`](@/docs/reference/command-line.md#camera)
  still wins over both. A later product that resolves to another camera or resolution is
  skipped with a warning.
- **`productName`** is read at the frame being rendered, so a time-sampled name gives one
  file per frame. A relative path is relative to the working directory, and missing
  directories are created.
- **[`-o`](@/docs/reference/command-line.md#output)** replaces the first product's
  `productName`, as husk's `-o` does. The other products keep theirs.
- **Two products with the same path** cannot both be written: the later one is skipped
  with a warning. This includes a product whose path `-o` gives to the first.
- **`productType`** must be `raster` (the default). A `deepRaster` product is skipped with
  a warning.
- **The PNG preview** is made from the first product's beauty var and written beside that
  product, with a `.png` extension. A first product with no beauty var gets no PNG.

Text attributes authored as `driver:parameters:*` on a product are copied into its EXR
header: `driver:parameters:artist` as `artist`, and `driver:parameters:OpenEXR:<key>` as
`<key>`. A forwarded `colorInteropID` is refused with a warning: crust sets it to the space
it rendered in.

## Vars

### Channel names

A var is one layer of channels named `<layer>.<component>`. The layer is the var's
`driver:parameters:aov:name` if authored, otherwise the `RenderVar` prim name.

| kind | channels |
|------|----------|
| colour | `<layer>.R`, `.G`, `.B` (and `.A` for a 4-channel type) |
| vector (positions, normals) | `<layer>.X`, `.Y`, `.Z` |
| UV | `<layer>.U`, `.V` |
| scalar | one channel named `<layer>` |

The product's first beauty var is written without a prefix: `R`, `G`, `B`[, `A`]. Every
viewer shows it as the image. A var named `Z` therefore gives the conventional `Z`
channel.

An authored `driver:parameters:aov:channel_prefix` replaces the layer name. Two vars that
would write the same channel name are not both written: the second is skipped with a
warning.

### Types and precision

`dataType` gives the number of channels and their precision. An authored
`driver:parameters:aov:format` (Houdini writes both) overrides it.

| type | stored as |
|------|-----------|
| `half`, `half3`, `color3h`, ... | HALF |
| `float`, `float3`, `color3f`, `normal3f`, `point3f`, `texCoord2f`, ... | FLOAT |
| `int` | UINT; `-1` is written as `0xFFFFFFFF` |

The type must fit the source: three or four channels for the beauty, three for positions
and normals, two for UVs, one for scalars. An integer type is accepted only for
`sampleCount`. A var whose type does not fit is skipped with a warning.

### Source types

| `sourceType` | meaning |
|--------------|---------|
| `raw` (default) | `sourceName` is looked up in the table below. An empty `sourceName` uses the channel name. |
| `lpe` | `sourceName` is a [light path expression](#light-path-expressions). An `lpe:` prefix (Hydra's) is dropped. |
| `primvar` | Not supported yet: skipped with a warning. |
| `intrinsic` | Unimplemented in UsdRender itself: skipped with a warning. |

`sourceType` and `driver:parameters:aov:name` may be authored as a `token` or a
`string`; both are read.

## Sources

USD does not standardise AOV names. Crust Render uses Hydra's names where Hydra has one,
and accepts the RenderMan, Arnold, Karma and Blender names as aliases. Names are
case-sensitive.

| name | aliases | type | accumulation | meaning |
|------|---------|------|--------------|---------|
| `color` | `Ci`, `C`, `RGBA`, `beauty`, `HdrColor`, `Combined` | `color3f` / `color4f` | filtered | The beauty: the same image a render without products writes. `color4f` adds alpha. |
| `alpha` | `a`, `A`, `opacity` | `float` | filtered | Coverage: the fraction of camera samples that hit geometry. |
| `depth` | `cameraDepth`, `z`, `Z`, `Depth` | `float` | closest | Camera-space depth: distance from the camera plane along the view axis, in scene units. `+inf` where nothing was hit. |
| `distance` | `DistanceToCameraSD` | `float` | closest | Distance from the camera position to the first hit. `+inf` where nothing was hit. |
| `P` | `Pworld`, `__Pworld`, `Position` | `point3f` | closest | World-space position of the first hit. |
| `Peye` | `Pcam`, `__Pcam` | `point3f` | closest | Camera-space position of the first hit. |
| `normal` | `N`, `Nworld`, `__Nworld`, `Normal` | `normal3f` | filtered | World-space shading normal, facing the camera. |
| `Neye` | `Nn` | `normal3f` | filtered | The same normal in camera space. |
| `primvars:st` | `st`, `uv`, `UV` | `texCoord2f` | filtered | The first hit's texture coordinates. |
| `sampleCount` | `__sampleCount` | `float` / `int` | per pixel | Samples the pixel took: shows where adaptive sampling stopped early. |
| `variance` | `crust:variance` | `float` | per pixel | Variance of the pixel's luminance mean: the quantity adaptive sampling stops on. |
| `albedo` | `DiffuseAlbedoSD` | `color3f` | filtered | The surface colour at the first hit that is not a perfect mirror or clear glass, for denoisers. See [Albedo](#albedo). |
| `diffuse_albedo` | `DiffuseFilter`, `diffuseFilter` | `color3f` | filtered | The diffuse colour of the surface the camera ray hits: what [raw light](#raw-light) is divided by. 0 off a surface and on surfaces with no diffuse part (mirrors, metals, clear glass). |
| `rawLight` | `RawLighting`, `rawLighting` | `color3f` / `color4f` | filtered | Direct diffuse light without the surface's colour. See [Raw light](#raw-light). |
| `rawGI` | `RawGI` | `color3f` / `color4f` | filtered | Indirect diffuse light without the surface's colour. |
| `rawTotalLight` | `RawTotalLighting` | `color3f` / `color4f` | filtered | Both. |

Any other name is skipped with a warning, and no channel is written for it. Crust Render
never writes a black channel that looks valid.

{% alert(icon="⚠️") %}
**`depth` is camera depth, not clip-space.** Hydra's own definition of `depth` is the
rasteriser's clip-space value in [0, 1]. Crust Render follows the production renderers'
Hydra delegates (hdPrman, Arnold), which map `depth` to camera-space depth, because that
is what compositors expect from a `Z` channel.
{% end %}

### Spaces

- **World space** is the stage's world, the frame every other crust quantity uses.
- **Camera space** is the USD camera's: X right, Y up, looking down −Z. Points in front
  of the camera have a negative `Peye.Z`, and `depth` is `−Peye.Z`.
- **Normals** stay in [−1, 1]. They are never remapped to [0, 1].

### Where the camera ray meets no surface

- **Escaped** (the sky or a dome): the beauty gets the background; `alpha` is 0; data
  AOVs keep their clear value. A visible dome is colour without coverage, as compositing
  expects.
- **Volume scatter**: `depth`, `distance`, `P` and `Peye` are the scatter point; the
  normals and UVs keep their clear value; `alpha` is 0.
- **Cutouts**: a surface the camera ray passes through is not a hit. The AOVs describe
  what is behind it, as the beauty does.
- **Thin glass** (a thin-walled transmissive surface): the camera ray may pass straight
  through it, but it is still the first hit. Depth, positions, normals and UVs describe
  the glass, as they would for any other surface in front of the camera. Light seen
  through it is a `TS` event in light path expressions.

## Accumulation

A pixel takes many samples. Each AOV combines them in one of two ways.

- **filtered**: the samples' average, weighted by the same pixel-filter weights as the
  beauty. Colour, alpha, normals and UVs use it by default. A filtered normal at an
  object's edge is a blend of the object's normal and the clear value, which is what a
  denoiser expects.

  A clear value that is not a finite number (depth and distance clear to `+inf`) cannot
  be averaged in. A filtered var with such a clear value averages only the samples that
  hit something, and keeps the clear value where none did.
- **closest**: the value of the sample nearest the camera. Depth, distance and positions
  use it by default. It never blends two surfaces, so a depth or position pass has no
  in-between values at an edge.

  Only samples that land inside the pixel's own square count: the default triangle
  filter places samples up to one pixel outside it, and those would fatten edges. A
  pixel whose samples all landed outside its square takes the sample nearest its centre.

These authored attributes change the mode, first match wins:

1. `driver:parameters:aov:multiSampled`: `true` is filtered, `false` is closest.
2. Arnold's `arnold:filter`: `closest_filter` is closest, anything else filtered.
3. Karma's `driver:parameters:aov:filter`: `["closest", ...]` is closest.
4. RenderMan's `ri:accumulationRule` or `ri:displayChannel:filter`: `zmin` is closest.
   Other rules (`zmax`, `min`, `max`, `sum`, ...) are refused with a warning.

`driver:parameters:aov:clearValue` replaces the clear value: what a pixel holds where
nothing was hit. Houdini authors `0` for every var, including depth.

The beauty, `sampleCount` and `variance` are per-pixel quantities and ignore the mode.

## Light path expressions

A var with `sourceType = "lpe"` holds the light that travels along the paths its
expression describes. The syntax is OSL's, as RenderMan, Arnold and Karma accept it, so
expressions copied from their documentation work unchanged.

A path is a sequence of **events**, from the camera to a light:

| event | meaning |
|-------|---------|
| `C` | the camera: every path starts with it |
| `R`, `T` | reflection, transmission at a surface |
| `V` | a scatter in a volume (a volume prim, or the medium inside glass or skin) |
| `L` | emission from a light: an area, sphere, rect, disk, cylinder, dome or distant light |
| `O` | emission from anything else: an emissive material, a volume's own emission |

`R` and `T` events also have a scatter kind:

| kind | meaning |
|------|---------|
| `D` | diffuse |
| `G` | glossy: a rough specular, coat or sheen lobe |
| `S` | singular: a mirror-like lobe (roughness about 0.03 or less), thin glass |
| `s` | straight: a cutout's transparent part, passed through |

and a **label**: the OpenPBR component the lobe belongs to — `'diffuse'`, `'specular'`,
`'coat'`, `'sheen'`, `'transmission'`, `'subsurface'` or `'translucent'`. A light's label
is its [`crust:light:lpeTag`](@/docs/usd/lights.md#crust-light-lpetag).

The grammar:

| syntax | means |
|--------|-------|
| `<RD>`, `<T.>`, `<RG'coat'>` | one event: type, scatter kind, labels; `.` is anything |
| `D`, `R`, `'coat'` | shorthands for `<.D>`, `<R.>`, `<..'coat'>` |
| `.` | any one event |
| `[LO]`, `[^R]`, `<R[DG]>`, `<RS[^'coat']>` | sets of events, or of values in one position |
| `*`, `+`, `{n}`, `{n,m}`, `{n,}` | repetition |
| `(…)`, `\|` | grouping, alternation |

The whole path must match: `C<RD>L` is direct diffuse light only.

```usda
def RenderVar "diffuse_indirect"
{
    uniform token dataType = "color3f"
    uniform string sourceName = "C<RD>.+[LO]"
    uniform token sourceType = "lpe"
}
```

The usual compositing set — each path's first event is exactly one of these, so the
layers add back up to the beauty:

| layer | expression |
|-------|------------|
| direct, indirect diffuse | `C<RD>[LO]`, `C<RD>.+[LO]` |
| direct, indirect glossy | `C<RG>[LO]`, `C<RG>.+[LO]` |
| mirror-like reflection | `C<RS>.*[LO]` |
| transmission | `C<T.>.*[LO]` |
| volume | `C<V.>.*[LO]` |
| emission seen directly | `C[LO]` |

### Light groups

Tag lights with `crust:light:lpeTag`, then ask for each group with its own var:

```usda
def SphereLight "Key"
{
    token crust:light:lpeTag = "key"
}

def RenderVar "key"
{
    uniform token dataType = "color3f"
    uniform string sourceName = "C.*<L.'key'>"
    uniform token sourceType = "lpe"
}
```

`C.*<L.[^'key' 'fill']>` is every light that is in neither group.

### What the expressions guarantee

- Each expression is an unbiased estimate of the light it selects. A lobe's share of a
  sample goes to that lobe's events even when another lobe drew the sample.
- A set of expressions that splits the paths between them adds up to the beauty, to
  floating-point rounding, firefly clamp included.
- `C.*[LO]` is the beauty, bit for bit.
- `color4f` adds the beauty's alpha (coverage).

### Not supported

Refused with a warning that quotes the expression and the column:

- `?` (OSL has none; write `{0,1}`), `!` inversion;
- RenderMan lobe tokens (`D1`, `U2`, …) and prefixes (`unoccluded`, `shadows`, …);
- `B`: crust has no background event. A dome is a light, `L`.

Limits, each refused with a warning when the scene loads:

- at most 64 different expressions per render;
- an expression whose repetitions unroll to more than 4096 events, such as nested bounds
  `((.{32}){32}){32}`;
- a set of expressions whose automaton would need more than 4096 states. A pattern like
  `C.*<RD>.{16}[LO]` ("a diffuse event exactly 17 events before the end") doubles the
  states with every bound; the compositing expressions above need a few dozen between them.

A light tag that no expression names is ignored: that light counts as untagged.

### Cost

The first expression of a render costs about a fifth of the render's time: every
vertex splits its BSDF by lobe. Each further expression adds 2 to 4% (cornellbox,
measured by instruction count). Every expression is one more colour layer to compress
when the EXR is written.

## Albedo

`albedo` is the surface colour a denoiser such as OIDN wants beside the beauty and the
normal: each lobe's colour times its weight in the material, clamped to [0, 1], with no
lighting.

It is taken at the first hit that is not a perfect mirror or clear glass. Through a
window or a mirror, the albedo is what lies beyond it, dimmed by the window. A path that
escapes reports what the glass in front of the sky let through, or 1 with no glass.

A volume scatter before any surface reports 1. Materials crust can't describe as lobes
report 1 too.

## Raw light

Raw light is the light a diffuse surface received, with the surface's own colour
removed: V-Ray's `RawLighting`. A compositor relights or recolours a surface with
it, since `lighting = raw light × diffuse colour`:

| raw | is | times `diffuse_albedo` gives |
|-----|----|------------------------------|
| `rawLight` | `C<RD>[LO]`, raw | direct diffuse light |
| `rawGI` | `C<RD>.+[LO]`, raw | indirect diffuse light |
| `rawTotalLight` | `C<RD>.*[LO]`, raw | all diffuse light |

The division happens **per camera sample**, before the pixel average. Dividing the
finished pixels in a compositor instead goes wrong wherever a pixel mixes two colours
(a texture's detail, an object's edge), because the average of a product is not the
product of the averages.

- Per sample, `raw × diffuse_albedo` is the diffuse light exactly.
- Per pixel it is exact wherever the colour is the same across the pixel. At a
  texture or object edge it is close but not exact: V-Ray's raw passes behave the
  same. So is a surface seen through a volume: some samples scatter in the haze
  first and see no diffuse colour, so the pixel's colour is diluted while its raw
  light is not.
- Where the diffuse colour is black (below 1e-4 in a channel), raw light is 0 there
  rather than light divided by almost nothing.
- A surface with no diffuse part (a mirror, a metal, clear glass), the sky, and a
  diffuse surface seen through glass or a mirror have no raw light: raw light is
  about the surface the camera sees first.

Any light path expression whose every path starts with a diffuse reflection can be
made raw with `crust:aov:raw`, for example one light group's diffuse light:

```usda
def RenderVar "key_raw"
{
    uniform token dataType = "color3f"
    uniform string sourceName = "C<RD>.*<L.'key'>"
    uniform token sourceType = "lpe"
    bool crust:aov:raw = 1
}
```

An expression that can start with anything else (`C.*[LO]`, `C<RG>L`, or a bare
label like `C'diffuse'.*L`, which matches any event type) is refused with a warning;
write `<RD'diffuse'>` for the label.

{% alert(icon="⚠️") %}
`diffuse_albedo` used to be another name for `albedo`. It is now the diffuse colour
alone, at the first surface; `albedo` is unchanged.
{% end %}

## The EXR files

Each product is one single-part, scanline, ZIP-compressed EXR. The header carries
`software = crust-render <version>` and `colorInteropID`, the ASWF Color Interop ID of the
[working colour space](@/docs/usd/render-settings.md#renderingcolorspace) the colour
channels are in: `lin_rec709_scene` by default, `lin_ap1_scene` when rendering in ACEScg.
In any working space other than linear Rec.709, the header also carries the space's
`chromaticities`, so any EXR reader knows the primaries. (An EXR without `chromaticities`
is Rec.709 by the format's definition.) The single beauty EXR written without products
carries the same two attributes in a non-Rec.709 working space, and none in `lin_rec709`.

## Guarantees

- Asking for AOVs never changes the beauty. It is bit-identical to the same render
  without products.
- Every channel is bit-identical between the default tiles and
  [`--scanline`](@/docs/reference/command-line.md#scanline).
- A render whose products ask for nothing beyond a 3-channel beauty does no AOV work at
  all.
- AOVs see exactly the samples the beauty took, adaptive sampling and path guiding
  included. A guided render blends its passes' AOVs with the same weights as its beauty.

## Not supported

Each of these is refused with a warning when authored, never ignored silently:

- identity AOVs (`primId`, `instanceId`, `elementId`) and ID mattes. ID mattes are
  planned as [OpenEXRId](https://github.com/MercenariesEngineering/openexrid) deep EXRs,
  not Cryptomatte, once crust can write deep EXRs;
- `sourceType = "primvar"`;
- the geometric normal `Ng`;
- deep output (`productType = "deepRaster"`);
- more than one camera or resolution in a render;
- `pixelAspectRatio`, `dataWindowNDC`, `disableMotionBlur`, `instantaneousShutter` and
  `disableDepthOfField` (warned only when authored with a value that would change the
  image).

The `resolution` fallback is crust's 640×360, not the schema's 2048×1080, so scenes
without a `resolution` keep their size.

For MaterialX surfaces whose `normal` input is connected (a normal map inside the
MaterialX graph), `normal` and `Neye` give the interpolated mesh normal, before that
input. UsdPreviewSurface normal maps are applied.
