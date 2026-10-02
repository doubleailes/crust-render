+++
title = "Environment variables"
description = "Every CRUST_* environment switch, its default and its effect."
date = 2026-10-01T08:00:00+00:00
updated = 2026-10-01T08:00:00+00:00
draft = false
weight = 20
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = 'The <code>CRUST_*</code> variables switch optimizations off and tune memory budgets. A normal render needs none of them.'
toc = true
top = false
+++

## What they are for

Each `CRUST_*` switch exists to compare an optimization against the behaviour it
replaced. Unset, every switch keeps the default: the optimized behaviour. Setting a switch
to its "off" value brings back the old behaviour. The image is then either bit-identical
or differs in the documented way.

So the environment is for **diagnosis and tuning**, not for look development. To change
how a scene looks, use the [USD attributes](@/docs/usd/overview.md) or the
[command line](@/docs/reference/command-line.md).

Typical uses:

- **Isolate a problem.** `CRUST_TEX=0` renders without UV textures. If an artifact goes
  away, it comes from a texture. `CRUST_SUBDIV=0` does the same for subdivision.
- **Fit a scene in memory.** `CRUST_TEX_CACHE_MB`, `CRUST_PTEX_STREAM` and
  `CRUST_TRI_PACKETS=indexed` trade speed for memory.
- **Check a speed-up.** Render once with the switch on and once with it off.

```bash
# render without any UV or Ptex texture
CRUST_TEX=0 CRUST_PTEX=0 crust-render -i scene.usda -o untextured.exr

# stream Ptex with a 2 GiB budget
CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIPSPACE=file CRUST_PTEX_CACHE_MB=2048 \
    crust-render -i island.usda --stats
```

On Windows PowerShell, set a variable with `$env:CRUST_TEX = "0"` before running
`crust-render`.

## Value syntax

The variables are read once, when the render starts.

**Booleans** accept:

| off | on |
|-----|----|
| `0`, `false`, `off`, `no` | `1`, `true`, `on`, `yes` |

**Numbers** must be integers in the range given for each variable. **Keywords** must be
one of the listed names.

A value that can't be read logs one warning naming the variable and the accepted values,
then the default is used. A typo never stops a render, so read the warnings.

## Summary

| variable | default | area |
|----------|---------|------|
| [`CRUST_STREAM_IMPORT`](#crust-stream-import) | on | USD import |
| [`CRUST_MESH_BAKE`](#crust-mesh-bake) | on | USD import |
| [`CRUST_SUBDIV`](#crust-subdiv) | on | USD import |
| [`CRUST_ADAPTIVE_PER_FACE`](#crust-adaptive-per-face) | on | USD import |
| [`CRUST_ADAPTIVE_FRUSTUM`](#crust-adaptive-frustum) | on | USD import |
| [`CRUST_BVH_PACKET_SAH`](#crust-bvh-packet-sah) | on | ray tracing |
| [`CRUST_TRI_PACKETS`](#crust-tri-packets) | `auto` | ray tracing |
| [`CRUST_MTLX_OPT`](#crust-mtlx-opt) | on | shading |
| [`CRUST_SHADER_JIT`](#crust-shader-jit) | on | shading |
| [`CRUST_RAY_CONES`](#crust-ray-cones) | on | textures |
| [`CRUST_TEX`](#crust-tex) | on | UV textures |
| [`CRUST_TEX_MAX`](#crust-tex-max) | 1024 | UV textures |
| [`CRUST_TEX_MIP`](#crust-tex-mip) | on | UV textures |
| [`CRUST_TEX_STREAM`](#crust-tex-stream) | on | UV textures |
| [`CRUST_TEX_CACHE_MB`](#crust-tex-cache-mb) | 1024 | UV textures |
| [`CRUST_PTEX`](#crust-ptex) | on | Ptex |
| [`CRUST_PTEX_MAX_LOG2`](#crust-ptex-max-log2) | 5 preloaded / uncapped streamed | Ptex |
| [`CRUST_PTEX_MIP`](#crust-ptex-mip) | on | Ptex |
| [`CRUST_PTEX_STREAM`](#crust-ptex-stream) | **off** | Ptex |
| [`CRUST_PTEX_CACHE_MB`](#crust-ptex-cache-mb) | 1024 | Ptex |
| [`CRUST_PTEX_STREAM_MIN_MB`](#crust-ptex-stream-min-mb) | 8 | Ptex |
| [`CRUST_PTEX_STREAM_MIPSPACE`](#crust-ptex-stream-mipspace) | `linear` | Ptex |
| [`RAYON_NUM_THREADS`](#rayon-num-threads) | all cores | threads |

## USD import

### CRUST_STREAM_IMPORT

Boolean, default **on**.

On, a large stage is imported one top-level subtree at a time. Each subtree is composed
through its own masked stage and released before the next, so only about one subtree of
composed USD is in memory at once. On the Moana Island this cut peak memory from 117 GiB
to 44 GiB, with the same image. Small scenes are always imported through one stage.

`0` imports the whole scene through one stage.

### CRUST_MESH_BAKE

Boolean, default **on**.

On, a mesh placed only once is baked into world space. `0` keeps every mesh as an
instance with its own transform.

The result is not bit-identical: an instanced mesh is intersected in its local space, so
a very small number of pixels differ in the last bit.

### CRUST_SUBDIV

Boolean, default **on**.

`0` renders every mesh as its faceted control cage, whatever `--subdiv-level` or
[`crust:subdivisionLevel`](@/docs/usd/render-settings.md#crust-subdivisionlevel) say.

This isn't the same as `--subdiv-level 0`, which still shades the cage with smooth
normals. Use `CRUST_SUBDIV=0` to tell whether an artifact comes from subdivision.

### CRUST_ADAPTIVE_PER_FACE

Boolean, default **on**. Only with
[`--subdiv-edge-length`](@/docs/reference/command-line.md#subdiv-edge-length).

`0` refines each mesh to one level, chosen for its nearest point to the camera, instead
of cutting each face by its own size on screen. A large mesh close to the camera is then
refined everywhere, its far end included. Use it to compare the two.

### CRUST_ADAPTIVE_FRUSTUM

Boolean, default **on**. Only with
[`--subdiv-edge-length`](@/docs/reference/command-line.md#subdiv-edge-length).

`0` refines geometry outside the camera's view by its distance like the rest, so its
reflections and shadows keep their detail, at the cost of memory.

## Ray tracing

### CRUST_BVH_PACKET_SAH

Boolean, default **on**.

On, the BVH builder sizes its leaves by how many SIMD packets of triangles they hold. `0`
uses the older per-triangle leaf cost.

The trees have different shapes, so ties between exactly equal hits can resolve
differently. The difference is noise, not bias.

### CRUST_TRI_PACKETS

Keyword, default **`auto`**.

The memory layout of the ray tracing kernel's triangle packets. Every layout gives
bit-identical results.

| value | meaning |
|-------|---------|
| `auto` | the measured default, currently `gathered` |
| `gathered` | 192-byte packets that carry their vertices. Fastest. |
| `indexed` | 92-byte packets of vertex indices. About a quarter less kernel memory per triangle, with 13–30% slower traversal. On the Moana island at subdivision level 1 it saves 8.4 GiB of 36.6 for an 8–9% slower render, and the import gets slightly faster. |

Use `indexed` when a scene doesn't otherwise fit in memory.

## Shading

### CRUST_MTLX_OPT

Boolean, default **on**.

On, MaterialX shading programs are optimized when they are compiled (constant folding,
hoisting, removing unused nodes). `0` runs them as compiled. Bit-identical.

### CRUST_SHADER_JIT

Boolean, default **on**.

On, MaterialX shading programs are compiled to machine code. `0` interprets them instead.
Bit-identical, only slower.

The JIT exists only in builds with the `jit` feature, which is the default. A build with
`--no-default-features` always interprets.

## Textures

### CRUST_RAY_CONES

Boolean, default **on**.

On, each texture lookup estimates its footprint from a ray cone and picks a matching mip
level. `0` makes every footprint zero, so the finest mip level is always read. This shows
whether an artifact comes from texture filtering.

### CRUST_TEX

Boolean, default **on**.

`0` declines every UV texture, so each surface renders with its constant (untextured)
parameter values.

### CRUST_TEX_MAX

Integer ≥ 1, default **1024**.

The largest edge, in pixels, of a UV texture tile that is loaded fully into memory. Larger
images are reduced to fit. Textures streamed from a `.tx` file aren't affected.

### CRUST_TEX_MIP

Boolean, default **on**.

`0` builds no mip pyramid for UV textures loaded into memory, so every lookup reads full
resolution.

### CRUST_TEX_STREAM

Boolean, default **on**.

On, a UV texture with a `.tx` file beside it is streamed tile by tile from that file. `0`
loads every texture fully into memory, even when a `.tx` exists. See
[`--auto-tx`](@/docs/reference/command-line.md#auto-tx) to create the `.tx` files.

### CRUST_TEX_CACHE_MB

Integer ≥ 1, default **1024**.

The memory budget, in MiB, of the tile cache for streamed `.tx` textures. `0` is refused
(the default is used).

## Ptex

### CRUST_PTEX

Boolean, default **on**.

`0` declines every Ptex texture. Surfaces fall back to their authored constant colour.

### CRUST_PTEX_MAX_LOG2

Integer in 0–14, default **5** when preloaded, **uncapped** when streamed.

The highest Ptex face resolution read, as the log2 of the face edge: `5` is 32 texels,
`10` is 1024. Faces authored at a higher resolution are read at this one. Unset, preloaded
Ptex is capped at 5 and streamed Ptex isn't capped.

### CRUST_PTEX_MIP

Boolean, default **on**.

`0` builds no per-face mip pyramid for Ptex textures.

### CRUST_PTEX_STREAM

Boolean, default **off**.

`1` streams Ptex tiles through a cache instead of loading every file fully into memory.
This is the only switch that is off by default.

{% alert(icon="⚠️") %}
On its own, `CRUST_PTEX_STREAM=1` usually changes nothing. With Ptex mip maps on (the
default), a mip-mapped `.ptx` is only streamed if
[`CRUST_PTEX_STREAM_MIPSPACE=file`](#crust-ptex-stream-mipspace) is also set. Otherwise it
is loaded into memory. Set both to get the memory saving.
{% end %}

### CRUST_PTEX_CACHE_MB

Integer ≥ 1, default **1024**.

The memory budget, in MiB, for streamed Ptex tiles. It is shared by every streamed file.

### CRUST_PTEX_STREAM_MIN_MB

Integer ≥ 0, default **8**.

A Ptex file that would take less than this many MiB in memory is loaded fully even when
streaming is on: a texture smaller than the cache space it would take is cheaper to load.
`0` streams every file.

### CRUST_PTEX_STREAM_MIPSPACE

Keyword, default **`linear`**.

Which mip levels a streamed `.ptx` may use.

| value | meaning |
|-------|---------|
| `linear` | Refuse the file's stored mip levels. A mip-mapped `.ptx` is loaded into memory instead, and its mip levels are rebuilt in linear light. Correct, but uses the most memory. |
| `file` | Use the file's own stored mip levels and stream them. Uses much less memory, but minified textures come out slightly darker. |

Crust Render reads Ptex colour as display-encoded (gamma 2.2) and decodes it, while the
mip levels stored in a `.ptx` were averaged in the file's own encoding. Averaging encoded
values makes the coarser levels too dark. The full-resolution level is always correct, so
the error only shows where a texture is seen from far away.

`file` is what production Ptex caches do. It cut the Moana Island's Ptex memory from
5.98 GiB to 0.61 GiB. Choosing it accepts that bias in exchange for the memory.

```bash
CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIPSPACE=file crust-render -i island.usda
```

## Other variables

### RAYON_NUM_THREADS

Read by the [rayon](https://docs.rs/rayon) thread pool, not by Crust Render itself. It
sets the number of render threads. Unset, every core is used.

```bash
RAYON_NUM_THREADS=8 crust-render -i scene.usda
```
