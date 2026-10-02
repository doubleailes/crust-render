+++
title = "Command line"
description = "Every crust-render command-line flag."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
draft = false
weight = 10
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'Every <code>crust-render</code> flag: what it does, its default, and the USD attribute it overrides.'
toc = true
top = false
+++

## Synopsis

```bash
crust-render [OPTIONS]
```

From a source checkout, put `cargo run --release --` in front of the flags:

```bash
cargo run --release -- -i samples/cornellbox.usda -o out.exr
```

`crust-render --help` lists every flag, and `crust-render --version` prints the version.

Many flags override a `crust:*` attribute on the stage's `RenderSettings` prim. A flag you
pass wins over the attribute. A flag you leave out keeps the scene's value, or the default
if the scene sets none. See [Render settings](@/docs/usd/render-settings.md) for the
attributes.

## Summary

| flag | value | default | overrides |
|------|-------|---------|-----------|
| [`-i`, `--input`](#input) | path | procedural scene | — |
| [`-o`, `--output`](#output) | path | `output.exr` | — |
| [`-s`, `--samples`](#samples) | integer | scene / 128 | `crust:samplesPerPixel` |
| [`-f`, `--frame`](#frame) | number | default values | `crust:frame` (seed) |
| [`--camera`](#camera) | prim path | `RenderSettings.camera` | `rel camera` |
| [`--subdiv-level`](#subdiv-level) | 0–6 | scene / 0 | `crust:subdivisionLevel` |
| [`--subdiv-edge-length`](#subdiv-edge-length) | pixels | scene / off | `crust:subdivisionEdgeLength` |
| [`--strategy`](#strategy) | name | scene / `power` | `crust:samplingStrategy` |
| [`--light-selection`](#light-selection) | name | scene / `power` | `crust:lightSelection` |
| [`--filter`](#filter) | name | scene / `triangle` | `crust:pixelFilter` |
| [`--filter-radius`](#filter-radius) | pixels | per filter | `crust:pixelFilterRadius` |
| [`--indirect-clamp`](#indirect-clamp) | number | scene / 10 | `crust:indirectClamp` |
| [`--auto-tx`](#auto-tx) | flag | off | — |
| [`--scanline`](#scanline) | flag | off (tiles) | — |
| [`--stats`](#stats) | flag | off | — |
| [`--profile`](#profile) | flag | off | — |
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

`-o, --output <OUTPUT>` — default `output.exr`

Where to write the image. Each render writes two files:

- the **linear EXR** at this path, and
- a **tone-mapped sRGB PNG** at the same path with a `.png` extension.

```bash
crust-render -i shot.usda -o renders/shot.0001.exr
# writes renders/shot.0001.exr and renders/shot.0001.png
```

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

Without `--frame`, attributes read their default (non-time-sampled) value.

A frame outside the stage's `startTimeCode`–`endTimeCode` range logs a warning but still
renders: animated attributes hold their first or last time sample. `nan` and `inf` are
refused as usage errors.

```bash
crust-render -i samples/animation.usda -f 12 -o anim.0012.exr
crust-render -i shot.usda -f 1001.5 -o shot.1001_5.exr    # a subframe
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
crust-render -i scene.usda --camera /cam --subdiv-edge-length 2
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
crust-render -i scene.usda --filter gaussian --filter-radius 2
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

## Diagnostics

### stats

`--stats`

When the render finishes, print render statistics and a per-phase profile: the time and
memory of parsing, building, rendering and writing the output, plus scene statistics.

The report always prints, whatever `-l` is set to. It also goes to `--log-file` if one is
open.

### profile

`--profile`

Also time the render section by section (`Trace`, `EvalBsdfs`, `Texture`,
`SurfaceLighting`, …) and add those profiles to the report. Implies `--stats`.

Profiling slows the render (the report prints its own estimate of the cost, typically
15–20%). It is separate from `--stats` so that the `--stats` render time stays comparable
between runs.

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
`crust-render-<UTC timestamp>.log`, for example `crust-render-20261001T142530Z.log`.

- Bare `--log-file` writes into the current directory.
- `--log-file <DIR>` writes into that directory, and creates it if needed.

The file receives the same lines as the terminal, without colour codes. Combine it with
`-l debug` to keep a full record of a render:

```bash
crust-render -i scene.usda -l debug --log-file logs
```

If the file can't be created, the run stops before loading the scene.

## Exit status

`crust-render` exits with `0` when the images are written. It exits with a non-zero status
when the arguments are invalid, the scene or the requested camera can't be loaded, the log
file can't be created, or an image can't be written.

## Examples

```bash
# quick preview
crust-render -i scene.usda -s 16 -o preview.exr

# final frame of a shot, through the shot camera
crust-render -i shot.usdc -f 1048 --camera /shot/cam/renderCam -o shot.1048.exr

# unbiased reference
crust-render -i scene.usda -s 4096 --indirect-clamp 0 -o reference.exr

# compare MIS against each strategy alone
crust-render -i samples/veach_mis.usda --strategy light -o light.exr
crust-render -i samples/veach_mis.usda --strategy bsdf  -o bsdf.exr

# many lights, mostly hidden
crust-render -i interior.usda --light-selection learned

# textured asset: build the .tx files once, stream them afterwards
crust-render -i asset.usda --auto-tx --stats
```
