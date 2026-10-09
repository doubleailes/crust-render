+++
title = "Command line"
description = "Every crust command and flag."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-09T08:00:00+00:00
draft = false
weight = 10
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Every <code>crust</code> command and flag: what it does, its default, and the USD attribute it overrides.'
toc = true
top = false
+++

## Synopsis

```bash
crust render [OPTIONS]              # render a scene
crust ls <KIND> -i <SCENE>          # list the scene's cameras, lights or materials
crust check -i <SCENE> [OPTIONS]    # what a render would use, and what crust refused
crust diff <A> <B>                  # did the image change, and by how much?
crust diagnostic -i <SCENE> [OPTIONS]  # measure how to render it faster or cleaner
```

| command | what it does |
|---------|--------------|
| `render` | renders a USD stage, or the procedural scene without `-i`. Every flag below except `-l` belongs to it. |
| [`ls`](#ls) | prints the stage's cameras, lights or materials, one prim path per line, or as JSON with the values a render reads for each. |
| [`check`](#check) | imports the stage as a render would and reports on it without rendering: the render it describes, its effective settings, the import's cost, findings and warnings. Takes the scene flags of `render`. |
| [`diff`](#diff) | compares two EXRs: whether they are identical, by how much they differ, and whether they can be compared at all. |
| [`diagnostic`](#diagnostic) | measures which settings make the stage's render faster or cleaner, within a time budget, and reports the evidence. Takes the scene flags of `render`. |

`-l, --level` applies to every command, and can go before or after it. `--log-file` belongs to
`render`: its directory is optional, so anywhere else it could take the next word as one.

From a source checkout, put `cargo run --release --` in front of the command:

```bash
cargo run --release -- render -i samples/cornellbox.usda -o out.exr
```

`crust --help` lists the commands, `crust render --help` every render flag, and
`crust --version` prints the version.

Many flags override a `crust:*` attribute on the stage's `RenderSettings` prim. A flag you
pass wins over the attribute. A flag you leave out keeps the scene's value, or the default
if the scene sets none. See [Render settings](@/docs/usd/render-settings.md) for the
attributes.

## Summary

The flags of `crust render`:

| flag | value | default | overrides |
|------|-------|---------|-----------|
| [`-i`, `--input`](#input) | path | procedural scene | — |
| [`-o`, `--output`](#output) | path | `output.exr` | first `productName` |
| [`--region`](#region) | `X0,Y0,X1,Y1` | scene / full frame | `dataWindowNDC` |
| [`-s`, `--samples`](#samples) | integer | scene / 128 | `crust:samplesPerPixel` |
| [`-f`, `--frame`](#frame) | number | default values | `crust:frame` (seed) |
| [`--camera`](#camera) | prim path | `RenderSettings.camera` | `rel camera` |
| [`--subdiv-level`](#subdiv-level) | 0–6 | scene / 0 | `crust:subdivisionLevel` |
| [`--subdiv-edge-length`](#subdiv-edge-length) | pixels | scene / off | `crust:subdivisionEdgeLength` |
| [`--strategy`](#strategy) | name | scene / `power` | `crust:samplingStrategy` |
| [`--light-selection`](#light-selection) | name | scene / `power` | `crust:lightSelection` |
| [`--light-samples`](#light-samples) | count | scene / 1 | `crust:lightSamples` |
| [`--light-samples-indirect`](#light-samples-indirect) | count | scene / 1 | `crust:lightSamplesIndirect` |
| [`--filter`](#filter) | name | scene / `triangle` | `crust:pixelFilter` |
| [`--filter-radius`](#filter-radius) | pixels | per filter | `crust:pixelFilterRadius` |
| [`--indirect-clamp`](#indirect-clamp) | number | scene / 10 | `crust:indirectClamp` |
| [`--ocio-config`](#ocio-config) | config | `$OCIO` / builtin ACES CG config | — |
| [`--working-space`](#working-space) | colour space | scene / `lin_rec709` | `renderingColorSpace` |
| [`--display`](#display) | display | `sRGB - Display` | — |
| [`--view`](#view) | view | `Un-tone-mapped` | — |
| [`--auto-tx`](#auto-tx) | flag | off | — |
| [`--scanline`](#scanline) | flag | off (tiles) | — |
| [`--checkpoint`](#checkpoint) | seconds | off | — |
| [`--stats`](#stats) | flag | off | — |
| [`--profile`](#profile) | flag | off | — |
| [`--stats-json`](#stats-json) | path or `-` | off | — |
| [`-l`, `--level`](#level) | name | `info` | — |
| [`--log-file`](#log-file) | directory | off | — |
| `-h`, `--help` | | | |
| `-V`, `--version` | | | |

## Input and output

### input

`-i, --input <INPUT>`

The scene to render: a `.usda`, `.usdc` or `.usdz` file. USD is the only scene format.

Without `-i`, Crust Render draws a built-in procedural scene. `--frame` and `--camera`
have no effect on it, and say so with a warning.

If the file can't be loaded, the run logs an error and exits with a non-zero status.

### output

`-o, --output <OUTPUT>`

Where to write the image. What it does depends on whether the stage authors
[render products](@/docs/usd/aovs.md):

- **No products** (most scenes). The render writes two files:
  - the **linear EXR** at this path (default `output.exr`), and
  - a **tone-mapped sRGB PNG** at the same path with a `.png` extension.
- **Products authored, `-o` given.** `-o` replaces the first product's `productName`, as
  husk's `-o` does. The other products keep their own paths.
- **Products authored, no `-o`.** Each product is written to its `productName`.

With products, the PNG is made from the first product's beauty and written beside it.

```bash
crust render -i shot.usda -o renders/shot.0001.exr
# without products: writes renders/shot.0001.exr and renders/shot.0001.png

crust render -i samples/aovs.usda
# writes renders/aovs_beauty.exr (+ .png) and renders/aovs_data.exr
```

### region

`--region <X0,Y0,X1,Y1>`

Render only a rectangle of the frame. The four numbers are pixels, counted from the
image's **top-left** corner as an image viewer shows them; `X1` and `Y1` are excluded,
so `--region 100,50,164,114` is a 64×64 crop starting at pixel `(100, 50)`.

Each pixel of the crop is the pixel the full render would have produced: the camera,
the resolution and every per-pixel sample stay those of the full frame, and only the
pixels outside the rectangle are skipped. The images record where the crop belongs:

- the **EXR** keeps the full resolution as its *display window* and has the region as
  its *data window*, so Nuke and other compositors place it correctly in the frame.
  [Render products](@/docs/usd/aovs.md) follow the same rule;
- the **PNG** holds only the region, at the region's size.

`--region` overrides the stage's
[`dataWindowNDC`](@/docs/usd/render-settings.md#datawindowndc). The rectangle is
clipped to the resolution. It is a usage error, raised before the scene is read, unless
it is four non-negative integers with `X1 > X0` and `Y1 > Y0`. A rectangle that falls
entirely outside the image stops the render with an error naming the resolution, and
nothing is written.

```bash
crust render -i shot.usda --region 100,50,164,114 -s 16 -o crop.exr
```

The crop is bit-identical to the same pixels of a full render with the same settings,
whenever a pixel's sample count does not depend on its neighbours: a fixed count (as at
`-s 16`, below the default adaptive minimum of 32), or adaptive sampling with
[`crust:adaptiveNeighbourTolerance`](@/docs/usd/render-settings.md#crust-adaptiveneighbourtolerance)
negative. Otherwise a pixel on the region's border may stop a little earlier than it
would in the full frame, since its neighbour outside the region is never sampled.
[Path guiding](@/docs/usd/render-settings.md#path-guiding) learns from the region's
paths only, so a guided crop differs from the same pixels of a guided full render.

[`--stats`](#stats) reports the region and the share of the frame it covers.

## Sampling and time

### samples

`-s, --samples <SAMPLES>`

The maximum number of samples per pixel. Overrides
[`crust:samplesPerPixel`](@/docs/usd/render-settings.md#crust-samplesperpixel) (default
128).

Adaptive sampling can stop a pixel early, once it has taken
[`crust:minSamplesPerPixel`](@/docs/usd/render-settings.md#crust-minsamplesperpixel)
samples (default 32) and its noise is under
[`crust:varianceThreshold`](@/docs/usd/render-settings.md#crust-variancethreshold). With
`-s` at or below the minimum, every pixel takes exactly `-s` samples.

{% alert(icon="💡") %}
To compare two images pixel by pixel, render both at `-s 16`. Every pixel then takes
exactly 16 samples. At higher counts, a tiny difference can change how many samples a
pixel takes, and the whole image looks different.
{% end %}

### frame

`-f, --frame <FRAME>`

The USD time code to render. Every animated attribute reads its time samples at this time,
and attributes with no animation read their default value. Fractional values render a
subframe, and negative values are accepted (`-f -10`).

The frame also seeds the sampler, so successive frames get different noise patterns. It
replaces [`crust:frame`](@/docs/usd/render-settings.md#crust-frame) for that.

Without `--frame`, attributes read their default (non-time-sampled) value. Transforms (`xformOp:*`) are the exception: they are read at time 0, so an op that authors both a default and time samples takes its sample at 0 (held from the first sample when that is later), not its default.

A frame outside the stage's `startTimeCode`–`endTimeCode` range logs a warning but still
renders: animated attributes hold their first or last time sample. `nan` and `inf` are
refused as usage errors.

```bash
crust render -i samples/animation.usda -f 12 -o anim.0012.exr
crust render -i shot.usda -f 1001.5 -o shot.1001_5.exr    # a subframe
```

### camera

`--camera <PRIM_PATH>`

The camera to render through, as an absolute USD prim path, for example
`/root/camera01/renderCam`.

Without it, Crust Render uses the camera named by the `RenderSettings` prim's `camera`
relationship. If there is none, it uses the first camera on the stage.

The two cases fail differently. If the `--camera` path isn't a camera on the stage, the
render stops with an error. If the `RenderSettings` camera is missing, the render logs a
warning and falls back to the first camera.

[`crust ls camera`](#ls) lists the paths `--camera` accepts.

## Geometry

### subdiv-level

`--subdiv-level <N>`

How many times to refine every mesh whose `subdivisionScheme` is not `none`. An
unauthored `subdivisionScheme` counts as USD's default, `catmullClark`. Overrides
[`crust:subdivisionLevel`](@/docs/usd/render-settings.md#crust-subdivisionlevel).

- **Default 0:** meshes are not refined. A subdivision mesh renders its control cage with
  smooth normals.
- **Maximum 6:** higher values are clamped to 6 with a warning.

Each level multiplies a mesh's face count by four, so even `--subdiv-level 1` can raise
memory use a lot on a large scene. To render every mesh as its faceted cage, with no
smooth normals, set the environment variable `CRUST_SUBDIV=0`.

With [`--subdiv-edge-length`](#subdiv-edge-length), this is the highest level adaptive
subdivision may choose instead.

### subdiv-edge-length

`--subdiv-edge-length <PX>`

Turns on adaptive subdivision: each subdivision mesh is refined only as far as its size
on screen asks. Each control-cage edge is cut into as many segments as it takes for each
to be at most `PX` pixels long, seen from the render camera at the edge's own distance,
so one mesh can be fine near the camera and coarse far away. Overrides
[`crust:subdivisionEdgeLength`](@/docs/usd/render-settings.md#crust-subdivisionedgelength).

- **The ceiling:** [`--subdiv-level`](#subdiv-level) (or `crust:subdivisionLevel`) caps
  it: at most 2^level segments per edge, the density of that uniform level. Without
  either, the ceiling is 3.
- **Instances:** only geometry used once is adaptive. A mesh placed directly, or a
  prototype with a single placement, is refined by its size on screen. A prototype placed
  several times is refined to the uniform level (`--subdiv-level`, else 0), so a forest
  costs no more than in a uniform render.
- **The camera:** it must be named before the scene is read, by [`--camera`](#camera) or
  the stage's `RenderSettings.camera`. Without one, a warning is logged and every mesh
  uses the uniform level.
- **Off-screen geometry** is not refined: faces outside the camera's view keep their
  control cage, which reflections and shadows see. Set `CRUST_ADAPTIVE_FRUSTUM=0` to
  refine them by distance too.
- **Faces that need no refinement** render their control cage with smooth normals, as at
  level 0.

The value must be a positive number. `--stats` reports how many meshes got each level.

```bash
crust render -i scene.usda --camera /cam --subdiv-edge-length 2
```

## Light transport

### strategy

`--strategy <STRATEGY>`

How light sampling (next-event estimation) and BSDF sampling are combined. Overrides
[`crust:samplingStrategy`](@/docs/usd/render-settings.md#crust-samplingstrategy).

| value | meaning |
|-------|---------|
| `power` | power-heuristic (β = 2) multiple importance sampling. **Default.** `mis` is accepted as another name for it. |
| `balance` | balance-heuristic multiple importance sampling |
| `light` | light sampling only |
| `bsdf` | BSDF sampling only |

`light` and `bsdf` are for diagnosis: they show what each strategy contributes on its
own. Try them on `samples/veach_mis.usda`.

### light-selection

`--light-selection <LIGHT_SELECTION>`

How light sampling picks which light to sample at each vertex. Overrides
[`crust:lightSelection`](@/docs/usd/render-settings.md#crust-lightselection).

| value | meaning |
|-------|---------|
| `power` | by emitted power, defensively: half the shadow rays are shared evenly among the finite lights, and lights at infinity keep their uniform share. **Default.** |
| `uniform` | every light is equally likely, whatever it emits |
| `learned` | visibility-aware: a short pre-pass learns, for each region of the scene, which lights reach it |

`learned` helps scenes with many lights where the brightest ones are often hidden, for
example a room lit through its windows.

### light-samples

`--light-samples <N>`

How many light samples (shadow rays) light sampling takes at the first vertex of each
camera path. It must be from 1 to 1024. Overrides
[`crust:lightSamples`](@/docs/usd/render-settings.md#crust-lightsamples) (default 1).

The N samples spread over the lights in proportion to their selection probabilities
(a light with probability 0.5 gets two of four samples, not a random number of them),
and each is combined with BSDF sampling by multi-sample multiple importance sampling,
so the image is the same in expectation at every N. Direct-light noise falls about as
1/N for N times the shadow rays at that vertex. With both counts at 1 the image is
bit-identical to a render before the counts existed.

### light-samples-indirect

`--light-samples-indirect <M>`

The same count at every later vertex of a path — surface and volume alike. It must be
from 1 to 1024. Overrides
[`crust:lightSamplesIndirect`](@/docs/usd/render-settings.md#crust-lightsamplesindirect)
(default 1). Paid at every bounce, so it multiplies the shadow rays along the whole
path for a smaller share of the image's noise than the camera vertex's count.

```bash
# four shadow rays at the first hit, one afterwards
crust render -i interior.usda --light-samples 4
```

### filter

`--filter <FILTER>`

The pixel reconstruction filter. Overrides
[`crust:pixelFilter`](@/docs/usd/render-settings.md#crust-pixelfilter).

| value | default radius | meaning |
|-------|----------------|---------|
| `box` | 0.5 | one-pixel box |
| `triangle` | 1.0 | tent filter. **Default.** |
| `gaussian` | 1.5 | truncated Gaussian |
| `blackman` | 1.5 | 4-term Blackman–Harris window |
| `mitchell` | 2.0 | Mitchell–Netravali. Sharp, but its negative lobes can ring. |

### filter-radius

`--filter-radius <FILTER_RADIUS>`

The filter radius in pixels, measured from the pixel center. It must be positive and
finite. Overrides
[`crust:pixelFilterRadius`](@/docs/usd/render-settings.md#crust-pixelfilterradius). Each
filter has its own default radius (see the table above).

```bash
crust render -i scene.usda --filter gaussian --filter-radius 2
```

### indirect-clamp

`--indirect-clamp <INDIRECT_CLAMP>`

The firefly clamp. It caps each sample's **indirect** light at this value in its largest
channel (linear units; the hue is kept). It must be finite and at least 0, and `0` turns
the clamp off. Overrides
[`crust:indirectClamp`](@/docs/usd/render-settings.md#crust-indirectclamp) (default 10).

The clamp removes fireflies but loses energy, so it biases the image. It is the only
biased default. Use `--indirect-clamp 0` for reference renders and for any measurement
that has to be unbiased.

## Colour

Crust Render manages colour with [OpenColorIO](https://opencolorio.org) (OCIO): every
transfer curve, gamut conversion and colour-space name comes from one OCIO config. See
[Texture colour spaces](@/docs/usd/materials.md#texture-colour-spaces) for what is
converted from where.

### ocio-config

`--ocio-config <CONFIG>`

The OCIO config to use: a `.ocio` file, an `.ocioz` archive, or a builtin URI such as
`ocio://studio-config-latest`. Without the flag, the config named by the
[`OCIO`](@/docs/reference/environment-variables.md#ocio) environment variable is used, as
in every OpenColorIO application; when that is unset or empty too, the builtin ACES CG
config `ocio://cg-config-v4.0.0_aces-v2.0_ocio-v2.5`. The config must define `raw`, `lin_rec709`,
`srgb_texture`, `g22_rec709` and `g18_rec709`, as names or aliases; every ACES CG and
studio config does. A config that can't be loaded, or lacks one of them, is an error,
whether the flag or `OCIO` named it.

```bash
crust render -i scene.usda --ocio-config /studio/config.ocio --working-space acescg
OCIO=/studio/config.ocio crust render -i scene.usda --working-space acescg
```

### working-space

`--working-space <SPACE>`

The scene-linear colour space to render in, by any name or alias of the OCIO config:
`acescg`, `lin_rec2020`, `lin_rec709`, … Overrides the stage's
[`renderingColorSpace`](@/docs/usd/render-settings.md#renderingcolorspace). The default,
when neither names one, is `lin_rec709`. A space that isn't scene-linear, or that the
config doesn't define, is an error.

The default is `lin_rec709` whatever the config: it is not taken from the config's
`scene_linear` role, so a render doesn't change when the config does. That role is ACEScg
in the builtin config and in the ACES studio configs; to render in it, name it here or in
`renderingColorSpace`.

```bash
crust render -i scene.usda --working-space acescg -o beauty.exr
```

The EXR is written in the working space, and its header says which (see
[The EXR files](@/docs/usd/aovs.md#the-exr-files)).

### display

`--display <DISPLAY>`

The OCIO display the PNG preview is encoded for. Default: `sRGB - Display`.

### view

`--view <VIEW>`

The OCIO view the PNG preview is encoded with. The default, `Un-tone-mapped`, clamps to
`[0, 1]` and applies the display's curve, so the PNG is the EXR, clipped and encoded. An
ACES output transform such as `"ACES 2.0 - SDR 100 nits (Rec.709)"` tone-maps the whole
scene-linear range instead. A display or view the config doesn't define is an error,
reported before the render starts. The EXR is never affected.

```bash
crust render -i scene.usda --working-space acescg --view "ACES 2.0 - SDR 100 nits (Rec.709)"
```

## Textures

### auto-tx

`--auto-tx`

The first time a UV texture is used, convert it to a tiled, mip-mapped `.tx` file beside
the original (same path, `.tx` extension). A texture is converted when its `.tx` is
missing or older than the source.

Crust Render always streams from a `.tx` when one exists beside a texture. This flag only
creates the missing ones. The run prints one line saying how many textures it converted,
and a texture that fails to convert is loaded fully into memory instead.

The conversion records the colour space of its mip levels in the file (see
[Materials and textures](@/docs/usd/materials.md#tx-files)).

## Rendering mode

### scanline

`--scanline`

Render one image row at a time (rows run in parallel) instead of the default 16×16 tiles.
The image is bit-identical. The progress bar counts rows.

`-b` / `--bucket` is still accepted so older command lines keep working, but it does
nothing: tiles are the default.

### checkpoint

`--checkpoint <SECONDS>`

While the render runs, rewrite the tone-mapped PNG preview every `SECONDS` from the image
so far, at the path the final PNG takes (see [output](#output)). An interval in which the
image did not change rewrites nothing. Fractions are allowed (`--checkpoint 0.5`); `0` or a
negative number is refused.

The whole frame shows early: a render first takes every pixel to 1 sample, then 2, 4, 8, …
up to the adaptive minimum, frame-wide, before the adaptive rounds refine the pixels that
are still noisy. A preview is the full image, noisy at first, not finished tiles on black.
(This order is how every render runs, with or without the flag, and it does not change the
image.)

The final EXR and PNG are the same files a render without the flag writes, and without
the flag nothing is written before the render ends. Each rewrite goes to a file beside the
PNG (`<name>.png.partial`) that is then renamed over it, so a viewer reloading the PNG
never reads half an image. Only the beauty is previewed: AOVs are written when the render
ends.

With [render products](@/docs/usd/aovs.md), the preview is the first product's beauty. When
the first product has no beauty var, a warning says no preview will be written, and the
render goes on.

```bash
crust render -i scene.usda -s 4096 --checkpoint 10 -o out.exr
# out.png appears within about ten seconds and is rewritten every ten
```

### Interrupting a render

Ctrl-C while `crust render` renders stops the render and keeps its work:

- No new sample starts; the render waits only for the pixels being traced at that moment.
- Every output the render would have written is written from the samples it traced: the
  EXR or the products, and the PNG. A pixel that took no sample is black, and its AOVs
  hold their clear values.
- A warning says the render was interrupted and how many samples its pixels reached.
- Every EXR carries `crust:renderStatus = "interrupted"` in its header (see
  [An interrupted render](@/docs/usd/aovs.md#an-interrupted-render)).
- The command exits with status `130`.

A second Ctrl-C, while those outputs are written, quits at once without writing more. So
does a Ctrl-C before the render has started (while the stage loads), which writes nothing.

A [path-guided](@/docs/usd/render-settings.md#path-guiding) render stopped in its final pass blends the
passes it completed with the partial final pass once every pixel of it has two samples or
more, and the completed passes alone before that. Stopped during its training passes, it
writes those it completed, or the first one as far as it got.

## Diagnostics

### stats

`--stats`

When the render finishes, print render statistics and a per-phase profile: the time and
memory of parsing, building, rendering and writing the output, plus scene statistics.
For streamed textures it reports the tile cache's memory and hit rates, and how many `.tx`
files were open at the peak against
[`CRUST_TEX_MAX_OPEN_FILES`](@/docs/reference/environment-variables.md#crust-tex-max-open-files),
with the reopens that cap cost.

Whether or not `--stats` is on, a texture tile that can't be read is reported: its file is
named once in a warning, and the render ends with one warning counting the failed reads.
Those lookups use the texture's fallback colour.

The report always prints, whatever `-l` is set to. It also goes to `--log-file` if one is
open.

### profile

`--profile`

Also time the render section by section (`Trace`, `EvalBsdfs`, `Texture`,
`SurfaceLighting`, …) and add those profiles to the report. Implies `--stats`.

Profiling slows the render (the report prints its own estimate of the cost, typically
15–20%). It is separate from `--stats` so that the `--stats` render time stays comparable
between runs.

### stats-json

`--stats-json <PATH|->`

Write the statistics as JSON, format `crust-stats/1` (see [JSON reports](#json-reports)),
to `PATH` once the images are written, or to stdout with `-`. It holds what `--stats`
prints: the phases (`name`, `depth`, `time_s`, `rss_end_bytes`, `peak_end_bytes`), the
image, scene, ray, texture, Ptex, subdivision and displacement counters, the materials
and lights by kind, and `peak_memory_bytes`, the same figure the text report prints as
`peak memory (RSS)`. The ratios the text report derives are included too: `total_rays`,
`mean_path_length`, `rr_kill_rate`, `rays_per_s`, the texture `hit_rate`.

It collects the statistics without printing the table: pass `--stats` as well for both.
With [`--profile`](#profile), the report gains a `profile` object (the sections and the
execution tree, in seconds of thread time).

With `-`, stdout holds only the JSON, and the log, the progress bar and any text report go
to stderr:

```bash
crust render -i scene.usda -s 64 --stats-json - > stats.json
jq '.rays.rays_per_s' stats.json
```

If the file can't be written, the command fails after the images are written.

### level

`-l, --level <LEVEL>` — default `info`

How much to log: `error`, `warn`, `info`, `debug` or `trace`.

| level | what it adds |
|-------|--------------|
| `error` | failures that stop the run |
| `warn` | anything in the scene that was refused, approximated or skipped |
| `info` | four lines per render: resolution and samples, render time, the two images written |
| `debug` | one or more lines per prim, material, texture and render pass |
| `trace` | everything |

`info` stays short whatever the size of the scene. Read the `warn` lines: they are how
Crust Render tells you it didn't use something the scene asked for.

### log-file

`--log-file [<DIR>]`

Also write the log to a file named for the time the run started,
`crust-<UTC timestamp>.log`, for example `crust-20261001T142530Z.log`.

- Bare `--log-file` writes into the current directory.
- `--log-file <DIR>` writes into that directory, and creates it if needed.

The file receives the same lines as the terminal, without colour codes. Combine it with
`-l debug` to keep a full record of a render:

```bash
crust render -i scene.usda -l debug --log-file logs
```

If the file can't be created, the run stops before loading the scene.

## ls

`crust ls <KIND> -i <SCENE> [--json <PATH|->] [-f <FRAME>]`

Prints the stage's prims of one kind, one absolute prim path per line, in namespace
order. `KIND` is one of:

| kind | lists |
|------|-------|
| `camera` | the cameras a render can go through: the paths `--camera` accepts |
| `light` | the lights the render reads (sphere, rect, disk, cylinder, distant, dome) |
| `material` | the `Material` prims a binding can reach, whether or not anything binds them |

The plurals (`cameras`, `lights`, `materials`) work too.

```bash
$ crust ls camera -i samples/cornellbox.usda
/scene/camera1
$ crust ls light -i samples/cornellbox.usda
/scene/Sky
```

The prims are found the way the render finds them, without importing anything else, so
the list is what a render would use:

- Nothing under an inactive ancestor is listed, of any kind.
- A camera or light under a `class` or proxy- or guide-purpose ancestor, inside an
  instance's prototype, or beneath a `PointInstancer` is not listed, since a render never
  uses it.
- Under an invisible ancestor, a camera is listed (its visibility only hides it in a
  viewport) and a light is not (it lights nothing).
- A light is listed whatever its values. One the render then refuses, such as a sphere
  light of radius 0 or one whose transform scales it to nothing, is skipped with a
  warning when you render: whether a light is usable depends on the frame, and `ls`
  reads none.
- A material is listed wherever a binding can reach it: only one inside an instance's
  prototype is left out.

The log goes to stderr, so stdout holds only the list and can be piped:

```bash
for cam in $(crust ls camera -i shot.usda); do
    crust render -i shot.usda --camera "$cam" -o "renders/$(basename "$cam").exr"
done
```

A stage with nothing of that kind prints nothing and logs a warning. A stage that can't
be opened is an error.

### ls --json

`--json <PATH|->` writes the listed prims as JSON, format `crust-ls/1` (see
[JSON reports](#json-reports)): `kind`, `frame`, and `prims`, the records in the text
listing's order. With `-` the JSON replaces the paths on stdout; with a path, the paths
are printed as without it.

Each record has the prim's `path` and the values a render reads for it, read by the
render's own code, so an unauthored value is the one the render falls back to:

| kind | values |
|------|--------|
| `camera` | `focal_length_mm`, `aperture_mm` (horizontal, vertical: the vertical one defaults to the horizontal over the image's aspect), `f_stop` (`0` is a pinhole), `focus_distance`, `is_render_camera`, `hidden` |
| `light` | `type` (`sphere`, `rect`, `disk`, `cylinder`, `distant`, `dome`), `intensity`, `exposure`, `color`, `normalize`, as authored on the light: no transform, colour space or colour temperature applied |
| `material` | `surface`, the `info:id` of the surface shader the render decodes (`null` when there is none), and `bound` |

`is_render_camera` marks the camera `crust render` without `--camera` goes through: the
first RenderProduct's camera, else `RenderSettings.camera`, else (that camera missing, or
none named) the first one the render meets. At most one camera is marked, and none on a
stage without cameras, where the render uses the procedural camera.

`bound` is true when the render's own binding resolution — inherited bindings, collection
bindings, binding strength, the `full` purpose falling back to the all-purpose binding —
resolves at least one mesh, sphere or curve it renders to that material. A material
targeted only by a binding that never takes effect is not bound. Working it out resolves
every geometry prim's binding, so it costs a walk of the stage.

```bash
$ crust ls camera -i samples/cornellbox.usda --json - | jq -c '.prims[] | [.path, .is_render_camera]'
["/scene/camera1",true]
```

### ls -f

`-f, --frame <FRAME>` evaluates the values `--json` reports at that time code, parsed as
[`render -f`](#frame) parses it. Without it, values read their default
(non-time-sampled) value, as a render without `-f` does. Which prims are listed does not
depend on it.

## check

`crust check -i <SCENE> [--json PATH|-] [--deny KIND[,KIND…]] [OPTIONS]`

Imports the stage exactly as `crust render` with the same flags would, renders nothing,
and reports:

- **the render it describes**: the stage, the frame, the camera the render goes through
  (after any fallback), the resolution, the region, and every file the render would write
  with its channels. A stage with no `RenderProduct` reports the one beauty EXR a render
  writes, `output.exr`;
- **effective settings**: every setting the render would run with, each with the flag
  and the `crust:*` attribute that change it, as the diagnostic reports them;
- **the import**: its phases with their time and peak memory, and the scene counts
  (geometries, primitives, lights, volumes), with the keys `--stats-json` uses;
- **findings**: the [diagnostic](#diagnostic)'s findings that need no render — textures
  without a `.tx`, many lights picked uniformly, a visualisation strategy, peak memory —
  with the same ids, evidence and actions;
- **warnings**: what the import refused, approximated or skipped, one record per
  [warning code](@/docs/reference/warnings.md) with its count and first prims.

It writes **no image** and no file beside the stage, except the `.tx` files `--auto-tx`
creates, as a render would. The text report goes to stdout and the log to stderr.

It accepts the same scene flags as [`diagnostic`](#diagnostic), with the same names,
values and defaults: [`-i`](#input) (required), [`-f`](#frame), [`--camera`](#camera),
[`--region`](#region), [`--strategy`](#strategy), [`--light-selection`](#light-selection),
[`--light-samples`](#light-samples), [`--light-samples-indirect`](#light-samples-indirect),
[`--indirect-clamp`](#indirect-clamp), [`--filter`](#filter),
[`--filter-radius`](#filter-radius), [`--subdiv-level`](#subdiv-level),
[`--subdiv-edge-length`](#subdiv-edge-length) and [`--auto-tx`](#auto-tx). The rest of
`render`'s flags (`-s`, `-o`, the colour and statistics flags) are refused. Its own flags:

| flag | value | default | what it does |
|------|-------|---------|--------------|
| `--json` | path or `-` | off | Also write the report as JSON, format `crust-check/1`. With `-`, the JSON replaces the text report on stdout. |
| `--deny` | kinds | off | Exit `3` when the import raises a warning of one of these kinds: `refused`, `approximated`, `skipped`, or `all`, comma-separated. The reports are still written, and name the matching codes (`denied`). Findings never deny. |

```bash
$ crust check -i samples/cornellbox.usda
$ crust check -i shot.usdc -f 1048 --deny refused,skipped --json check.json
```

## diff

`crust diff <A> <B> [--json <PATH|->]`

Compares two EXR files, `A` the reference. Every channel of every layer is compared, by
its full name (`layer.channel`, or the bare name in an unnamed layer); two channels are
equal only when every sample is bitwise equal, so a NaN that moved counts and two equal
infinities do not. A channel in only one file differs in every pixel.

The text report on stdout gives the resolution and the pixels where any channel differs,
then a line per differing channel. Here, the Cornell box at 16 samples per pixel against
the same at 4 (the eight listed pixels shortened to one):

```text
$ crust diff cornell16.exr cornell4.exr
640x360  differing pixels: 228227/230400 (99.0569%)
  channel B: 128881 pixels differ, max abs diff 4.4404057e-1
  channel G: 221895 pixels differ, max abs diff 3.6249244e-1
  channel R: 221014 pixels differ, max abs diff 3.0812702e-1
  differs at (0, 0): [0.6399156, 0.78394943, 1.0] vs [0.64000183, 0.7840011, 1.0]
max abs diff: 4.4404057e-1   max rel diff: 1e0
mean abs diff: 2.4019428117913372e-2
rmse: 4.149975813667258e-2
relmse: 4.757807812359412e-2
relmse (trimmed 0.1%): 4.602686768547243e-2
comparability: warn
  crust:spp differs: 16 vs 4
```

(The last two lines are on stderr.)

When both files have `R`, `G` and `B`, it lists the first eight differing pixels and the
beauty's error metrics against `A`: the largest absolute and relative difference, the mean
absolute difference, the RMSE, the relative MSE `(A − B)² / (A² + 0.01)` — the noise
metric to use against a high-spp reference — and the same with the worst 0.1% of pixels
discarded, so a few fireflies do not decide it.

It exits with:

| status | when |
|--------|------|
| `0` | the files have the same resolution and every channel is identical |
| `1` | they differ, a resolution mismatch included |
| `2` | a file can't be read, or the arguments are invalid |

`--json <PATH|->` also writes the report as JSON, format `crust-diff/1` (see
[JSON reports](#json-reports)): `identical`, `a` and `b` (path, size and stamp),
`differing_pixels`, `total_pixels`, `channels` (every channel, with its `status` and
count), `beauty` (the metrics, when both files have one) and `comparability`. With `-` the
JSON replaces the text report on stdout.

### Comparability

Every EXR `crust render` writes records how it was sampled
([the sampling stamp](@/docs/usd/aovs.md#how-the-pixels-were-sampled)). `diff` reads both
stamps and says whether the pixels can be compared at all:

| status | when |
|--------|------|
| `ok` | the stamps agree on everything that changes what a pixel holds |
| `warn` | they show a condition that makes pixel differences unreliable, each named in a note |
| `unknown` | a file has no stamp: another renderer wrote it, or an older crust |

A note is written for adaptive sampling on either side (a render that could stop
pixels early — `crust:varianceThreshold` above 0 and a budget past the first check, at
`crust:minSpp` or `√spp` rounded up to a multiple of 4 — or pixels that took different
counts: a one-ulp change then changes a pixel's sample budget and cascades, so compare
at `-s 16`), for an
`--indirect-clamp` that differs (the clamp is biased, so the metrics include its
bias), and for each other stamped value that differs: the frame, the camera, the sample
count, the depth, the light samples, the threshold, the pixel filter and its radius, the
strategy, the light selection, and `colorInteropID`. Two builds of crust are never a
reason to warn: comparing them is what `diff` is for.

In text mode the notes go to stderr, so stdout reads the same either way. Comparability
**never changes the exit status**, which answers only whether the pixels changed. And
`ok` only means the stamps match: it is not proof that a difference is noise. To show
that, render both sides at several sample counts and check that the difference falls as
1/√N rather than levelling off.

```bash
crust render -i scene.usda -s 16 --indirect-clamp 0 -o before.exr
# ... change something, rebuild ...
crust render -i scene.usda -s 16 --indirect-clamp 0 -o after.exr
crust diff before.exr after.exr && echo identical
```

## diagnostic

`crust diagnostic -i <SCENE> [--budget 120s] [--json PATH] [--baseline PREV.json] [OPTIONS]`

Imports the stage once and measures how to make its render faster or cleaner: a
full-frame baseline, then every unbiased setting tried on up to three crops of the frame
and judged by efficiency (time × error). It writes **no image** and changes no file
beside the stage. Its outputs are:

- the Markdown report on stdout, and nothing else there;
- the JSON report, format `crust-diagnostic/1`, at `--json` (default
  `crust-diagnostic.json` in the working directory);
- the log on stderr.

Reading the report, its verdicts and the loop it is made for are in
[Diagnosing a render](@/docs/help/diagnosing-a-render.md).

It accepts the flags that shape the scene and its settings, with the same names, values
and defaults as `render`, so a suggestion made as a flag can be passed straight back:

[`-i`](#input) (required), [`-f`](#frame), [`--camera`](#camera), [`--region`](#region),
[`--strategy`](#strategy), [`--light-selection`](#light-selection),
[`--light-samples`](#light-samples), [`--light-samples-indirect`](#light-samples-indirect),
[`--indirect-clamp`](#indirect-clamp), [`--filter`](#filter),
[`--filter-radius`](#filter-radius), [`--subdiv-level`](#subdiv-level),
[`--subdiv-edge-length`](#subdiv-edge-length) and [`--auto-tx`](#auto-tx).

With `--region`, that region is the only crop the trials render. `--auto-tx` is the one
flag that writes beside the stage: the `.tx` files it creates, as a render would.

The rest of `render`'s flags (`-s`, `-o`, the colour and statistics flags, `--scanline`,
`--log-file`) are refused: the diagnostic picks its own sample counts and writes no image.
Its own flags:

| flag | value | default | what it does |
|------|-------|---------|--------------|
| `--budget` | duration | `120s` | Time to spend after the import: `90s`, `5m`, `1m30s`, `1h`, `250ms`, or plain seconds. The import is reported, not counted. The baseline always runs; the later tiers reserve their estimated cost, the trials fit what is left, and anything that would overrun is listed under `not_tried`. More budget means more samples per trial: raise it when a trial reads `insufficient_samples`. |
| `--json` | path | `crust-diagnostic.json` | Where to write the JSON report. |
| `--baseline` | path | — | A previous report of the same scene, frame, camera, resolution and region. Adds a `deltas` section (what changed in time, error, settings, findings and suggestions); a report of anything else is marked *not comparable*. |
| `--repeats` | count | 3 | Interleaved baseline/trial pairs per crop, each with its own sampler seed. More resolves smaller differences on a busy machine, at the cost of fewer trials. With 1 there is no noise floor, and no trial can read `insufficient_samples`. |
| `--target-mrse` | number | threshold² | The mean relative squared error the sample-budget estimate aims for. Defaults to the square of the scene's adaptive variance threshold (0.05 → 0.0025). |

```bash
$ crust diagnostic -i samples/veach_mis.usda --budget 30s > report.md
$ crust diagnostic -i samples/veach_mis.usda --light-selection learned --baseline crust-diagnostic.json
```

## JSON reports

`--stats-json`, `ls --json`, `check --json` and `diff --json` (and the
[`diagnostic`](#diagnostic)'s report) share one shape, so a script reads them the same way:

- each is one JSON object, opening with `format` — the report and its version, such as
  `crust-stats/1` — and `crust_version`;
- keys are `snake_case`, and a quantity with a unit names it: `time_s`, `peak_bytes`,
  `focal_length_mm`;
- a value that is not finite, or not available on the platform (memory without
  `/proc`), is `null`;
- a change that removes or renames a key, or changes what it means, bumps the version.

A flag that writes one takes a path, or `-` for stdout. With `-`, stdout holds only the
JSON, and the log, the progress bar and any text report go to stderr. With a path, every
other output stays where it is without the flag.

## Exit status

`crust render` exits with `0` when the images are written, and `crust ls` when the list
is printed. `crust render` exits with `130` when Ctrl-C stopped the render (its partial
outputs are written: see [Interrupting a render](#interrupting-a-render)) or quit it before
or after. It exits with a non-zero status
when the arguments are invalid (a missing command included), the scene or the requested
camera can't be loaded, the log
file can't be created, or an image or a JSON report can't be written.

`crust diff` exits with `0` when the files are identical, `1` when they differ and `2` on
an error (see [diff](#diff)).

`crust check` exits with:

| status | when |
|--------|------|
| `0` | the import succeeded and no warning of a `--deny` kind was raised |
| `3` | a warning of a `--deny` kind was raised; the reports are still written |
| `1` | an error: the stage can't be opened or imported, the region is outside the frame, or a report can't be written. No report is written. |
| `2` | a usage error: an unknown or render-only flag, an unknown `--deny` kind, no `-i` |

`crust diagnostic` exits with:

| status | when |
|--------|------|
| `0` | the unbiased trials (tier 1) completed, whether or not the later tiers fit |
| `3` | the budget ran out before they did, the baseline included; both reports are still written |
| `1` | an error: the stage, the `--baseline` file or the region can't be read, or the JSON can't be written. No report is written. |
| `2` | a usage error: an unknown or render-only flag, a malformed value, no `-i` |

## Examples

```bash
# quick preview
crust render -i scene.usda -s 16 -o preview.exr

# final frame of a shot, through the shot camera
crust render -i shot.usdc -f 1048 --camera /shot/cam/renderCam -o shot.1048.exr

# unbiased reference
crust render -i scene.usda -s 4096 --indirect-clamp 0 -o reference.exr

# compare MIS against each strategy alone
crust render -i samples/veach_mis.usda --strategy light -o light.exr
crust render -i samples/veach_mis.usda --strategy bsdf  -o bsdf.exr

# many lights, mostly hidden
crust render -i interior.usda --light-selection learned

# textured asset: build the .tx files once, stream them afterwards
crust render -i asset.usda --auto-tx --stats

# re-render one object of a frame, placed in the frame for compositing
crust render -i shot.usdc -f 1048 --region 812,240,1100,520 -o fix.1048.exr

# did a change move any pixel? (0 identical, 1 differs)
crust diff before.exr after.exr

# render statistics for a script
crust render -i scene.usda --stats-json - > stats.json

# before a long render: what will it use, and did the import drop anything?
crust check -i shot.usdc -f 1048 --deny skipped

# what would make this shot faster or cleaner, in five minutes
crust diagnostic -i shot.usdc -f 1048 --camera /shot/cam/renderCam --budget 5m > report.md
```
