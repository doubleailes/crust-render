+++
title = "FAQ"
description = "Answers to frequently asked questions and common problems."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
draft = false
weight = 10
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = "Answers to frequently asked questions and common problems."
toc = true
top = false
+++

## Which scene formats does Crust Render read?

Only USD: `.usda`, `.usdc` and `.usdz`. To render a scene from another format, convert it
to USD first. Most DCCs can export USD.

## My surface renders grey. Why?

Crust Render couldn't read its material, and used grey diffuse instead. The log has a
`WARN` line naming the material and the reason: an unknown shader `info:id`, a shader
without `info:id`, or an unresolvable material. See
[Materials and textures](@/docs/usd/materials.md#material-types) for what is supported.

## I can't see my area light in the render

That's on purpose. Like most production renderers, Crust Render hides an area light's
surface from camera rays by default, and still lets it light the scene. To show it, add
`bool crust:light:cameraVisible = 1` to the light. See
[Lights](@/docs/usd/lights.md#camera-visibility).

## How do I hide the background but keep the dome lighting?

Add `bool crust:light:cameraVisible = 0` to the dome light, or
`bool crust:domeLightCameraVisibility = 0` to the `RenderSettings` prim to hide every
dome and distant light at once.

## My render is noisy

- Raise the sample count: [`-s`](@/docs/reference/command-line.md#samples) or
  [`crust:samplesPerPixel`](@/docs/usd/render-settings.md#crust-samplesperpixel).
- Lower [`crust:varianceThreshold`](@/docs/usd/render-settings.md#crust-variancethreshold)
  so that adaptive sampling stops pixels later.
- Many lights, most of them hidden? Try
  [`--light-selection learned`](@/docs/reference/command-line.md#light-selection).

## My render has bright speckles (fireflies)

The default firefly clamp (10) already removes most of them. Lower
[`--indirect-clamp`](@/docs/reference/command-line.md#indirect-clamp) to remove more, at
the cost of a little energy.

## My mesh looks faceted, or too smooth

By default subdivision meshes aren't refined. They render their control cage with smooth
normals. To refine them, use
[`--subdiv-level`](@/docs/reference/command-line.md#subdiv-level) or
[`crust:subdivisionLevel`](@/docs/usd/render-settings.md#crust-subdivisionlevel). To
render a mesh flat-faceted, author `subdivisionScheme = "none"` on it.

## The render runs out of memory

- Run with [`--stats`](@/docs/reference/command-line.md#stats) to see where the memory
  goes.
- Create `.tx` files with [`--auto-tx`](@/docs/reference/command-line.md#auto-tx) so UV
  textures are streamed instead of loaded whole, and lower
  [`CRUST_TEX_CACHE_MB`](@/docs/reference/environment-variables.md#crust-tex-cache-mb) if
  needed.
- Stream more Ptex files, by lowering
  [`CRUST_PTEX_STREAM_MIN_MB`](@/docs/reference/environment-variables.md#crust-ptex-stream-min-mb)
  (large files already stream by default).
- Use the smaller triangle layout,
  [`CRUST_TRI_PACKETS=indexed`](@/docs/reference/environment-variables.md#crust-tri-packets).
- Lower `--subdiv-level`. Each level multiplies the face count by four.

## How do I limit the number of threads?

Set [`RAYON_NUM_THREADS`](@/docs/reference/environment-variables.md#rayon-num-threads),
for example `RAYON_NUM_THREADS=8`.

## How do I find out where an artifact comes from?

Turn features off one at a time with the
[environment variables](@/docs/reference/environment-variables.md) and see whether the
artifact goes away:

| to rule out | set |
|-------------|-----|
| UV textures | `CRUST_TEX=0` |
| Ptex textures | `CRUST_PTEX=0` |
| texture filtering | `CRUST_RAY_CONES=0` |
| subdivision | `CRUST_SUBDIV=0` |
| the MaterialX JIT | `CRUST_SHADER_JIT=0` |

Also run with `-l debug --log-file logs` to keep a full log of what the importer read.

## Where do I report a bug?

Open an issue on [GitHub](https://github.com/doubleailes/crust-render/issues) with the
command line, the log (`-l debug --log-file`), and a scene that shows the problem if you
can share one.
