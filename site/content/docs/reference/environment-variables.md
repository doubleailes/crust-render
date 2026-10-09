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
lead = 'The <code>CRUST_*</code> variables switch optimizations off and tune memory budgets. A normal render needs none of them. <code>OCIO</code> names the colour config, as in every OpenColorIO application.'
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
CRUST_TEX=0 CRUST_PTEX=0 crust render -i scene.usda -o untextured.exr

# stream Ptex with a 2 GiB budget
CRUST_PTEX_CACHE_MB=2048 crust render -i island.usda --stats
```

On Windows PowerShell, set a variable with `$env:CRUST_TEX = "0"` before running
`crust`.

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
| [`CRUST_DISPLACE`](#crust-displace) | on | USD import |
| [`CRUST_ADAPTIVE_PER_FACE`](#crust-adaptive-per-face) | on | USD import |
| [`CRUST_ADAPTIVE_FRUSTUM`](#crust-adaptive-frustum) | on | USD import |
| [`CRUST_TRI_PACKETS`](#crust-tri-packets) | `auto` | ray tracing |
| [`CRUST_MTLX_OPT`](#crust-mtlx-opt) | on | shading |
| [`CRUST_SHADER_JIT`](#crust-shader-jit) | on | shading |
| [`CRUST_RAY_CONES`](#crust-ray-cones) | on | textures |
| [`CRUST_LINK_TWIN`](#crust-link-twin) | on | lighting |
| [`CRUST_TEX`](#crust-tex) | on | UV textures |
| [`CRUST_TEX_MAX`](#crust-tex-max) | 1024 | UV textures |
| [`CRUST_TEX_MIP`](#crust-tex-mip) | on | UV textures |
| [`CRUST_TEX_STREAM`](#crust-tex-stream) | on | UV textures |
| [`CRUST_TEX_CACHE_MB`](#crust-tex-cache-mb) | 1024 | UV textures |
| [`CRUST_TEX_MAX_OPEN_FILES`](#crust-tex-max-open-files) | 256 | UV textures |
| [`CRUST_PTEX`](#crust-ptex) | on | Ptex |
| [`CRUST_PTEX_MAX_LOG2`](#crust-ptex-max-log2) | 5 preloaded / uncapped streamed | Ptex |
| [`CRUST_PTEX_MIP`](#crust-ptex-mip) | on | Ptex |
| [`CRUST_PTEX_STREAM`](#crust-ptex-stream) | on | Ptex |
| [`CRUST_PTEX_CACHE_MB`](#crust-ptex-cache-mb) | 1024 | Ptex |
| [`CRUST_PTEX_STREAM_MIN_MB`](#crust-ptex-stream-min-mb) | 8 | Ptex |
| [`CRUST_PTEX_STREAM_MIPSPACE`](#crust-ptex-stream-mipspace) | `capped` | Ptex |
| [`OCIO`](#ocio) | builtin ACES CG config | colour |
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

### CRUST_DISPLACE

Boolean, default **on**.

`0` imports every mesh undisplaced, whatever its material's
[displacement](@/docs/usd/materials.md#displacement) says. A mesh with
`subdivisionScheme = "none"` then renders as its faceted cage again instead of being diced
bilinearly so it can be displaced. The result is bit-identical to the same stage with its
displacement inputs removed, so use it to tell whether a change in shape comes from
displacement.

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

## Lighting

### CRUST_LINK_TWIN

Boolean, default **on**.

A light whose `collection:shadowLink` leaves some occluders out casts its shadow rays past
them, but an ordinary bounce ray is still stopped by them. On, each surface or volume
scatter also traces a shadow ray toward such a light along the bounce direction, with the
light's own shadow set, so both sampling strategies see the light the same way and are
combined by multiple importance sampling as for any other light. This is what keeps glossy
reflections of a shadow-linked light quiet.

`0` samples every shadow-linked light by light sampling alone, as crust did before: the
same average image, noisier on glossy surfaces. A scene without shadow links renders
identically either way. Shadow-linked dome lights are sampled by light sampling alone
in both cases (see [Limitations](@/docs/architecture/limitations.md)).

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

The budget covers the tiles each render thread keeps for itself as well as the shared
cache:
- **Each thread keeps at most half the budget divided by the thread count** (up to 512
  tiles). The thread count is the render pool's (`RAYON_NUM_THREADS`) or the core
  count, whichever is larger, plus one. A thread that reaches its share gives back its
  oldest tiles to keep a new one. With a small budget on many threads the share can be
  less than one tile. The threads then keep nothing, and every lookup goes to the shared
  cache: slower, with the same image.
- **A tile the shared cache evicts while a thread still holds it keeps counting** until
  the thread lets go. `--stats` reports the most such bytes at once as
  `peak held after eviction`, beside `peak resident / budget`.

A scene whose tiles fit in the budget never evicts, so this line reads `0 B`.

### CRUST_TEX_MAX_OPEN_FILES

Integer ≥ 0, default **256**.

How many streamed `.tx` files the tile cache keeps open between reads, across all files.
When a read needs a file that isn't open and the cap is reached, the least recently used
open file is closed first. A render thread never waits for the cap, so at most the cap
plus one file per render thread is open at once. Every `.tx` file is closed when the
render ends, before the images are written.

The cap never changes the image, only how often files are reopened. `--stats` reports the
peak number of open files and the reopens: reads that had to open a file the cap had
closed. Many reopens mean the scene touches more files than the cap, which usually costs
little. Raise the cap only if `--profile` shows `TextureLoad` taking a large share of the
render, and keep it plus the thread count under the process's open-file limit
(`ulimit -n`, often 1024).

`0` never closes a file during the render, which was the behaviour before the cap. A
scene that streams thousands of `.tx` files can then run out of file descriptors.

## Ptex

### CRUST_PTEX

Boolean, default **on**.

`0` declines every Ptex texture. Surfaces fall back to their authored constant colour.

### CRUST_PTEX_MAX_LOG2

Integer in 0–14, default **5** when preloaded, **uncapped** when streamed.

The highest Ptex face resolution read, as the log2 of the face edge: `5` is 32 texels,
`10` is 1024. Faces authored at a higher resolution are read at this one. Unset, preloaded
Ptex is capped at 5 and streamed Ptex isn't capped.

The cap is also where a streamed texture's mip chain switches from the file's levels to the
levels rebuilt in linear light (see
[`CRUST_PTEX_STREAM_MIPSPACE`](#crust-ptex-stream-mipspace)). Set explicitly, it caps the
streamed texture too, which then matches the preloaded one at every distance.

### CRUST_PTEX_MIP

Boolean, default **on**.

`0` builds no per-face mip pyramid for Ptex textures.

### CRUST_PTEX_STREAM

Boolean, default **on**.

Large Ptex files stream their tiles through a cache instead of being loaded fully into
memory. Files under [`CRUST_PTEX_STREAM_MIN_MB`](#crust-ptex-stream-min-mb) are still
loaded fully, so a scene with only small `.ptx` files renders exactly as with streaming off.

`0` loads every Ptex file fully into memory, capped at
[`CRUST_PTEX_MAX_LOG2`](#crust-ptex-max-log2). This was the default before streaming was.

### CRUST_PTEX_CACHE_MB

Integer ≥ 1, default **1024**.

The memory budget, in MiB, for streamed Ptex tiles. It is shared by every streamed file.

### CRUST_PTEX_STREAM_MIN_MB

Integer ≥ 0, default **8**.

A Ptex file that would take less than this many MiB in memory is loaded fully even when
streaming is on: a texture smaller than the cache space it would take is cheaper to load.
`0` streams every file.

Each streamed file keeps one file open for the whole render. Lowering the threshold far
enough to stream thousands of files can run past the process's open-file limit
(`ulimit -n`, often 1024); see [Limitations](@/docs/architecture/limitations.md).

### CRUST_PTEX_STREAM_MIPSPACE

Keyword, default **`capped`**.

Which mip levels a streamed `.ptx` uses.

| value | meaning |
|-------|---------|
| `capped` | The file's own levels above the [`CRUST_PTEX_MAX_LOG2`](#crust-ptex-max-log2) cap (32×32 by default); at and below it, the levels the in-memory texture would use, rebuilt in linear light. Wherever the in-memory texture has texels, the streamed one returns exactly the same values, and close-ups also get the detail above the cap. |
| `linear` | Don't stream a mip-mapped `.ptx`: load it into memory, where its mip levels are rebuilt in linear light. Uses the most memory. |
| `file` | Use every level the file stores. Minified textures come out slightly darker. |

Crust Render reads Ptex colour as display-encoded (gamma 2.2) and decodes it, while the
mip levels stored in a `.ptx` were averaged in the file's own encoding. Averaging encoded
values makes the coarser levels too dark. So the levels used when a texture is seen from
far away have to be rebuilt after decoding. `capped` rebuilds them from the face at the
cap, which is what a texture loaded into memory holds anyway. A level finer than the cap
is still the file's, because rebuilding it would mean reading the full-resolution face.

`file` is what production Ptex caches do, and accepts the darker minified texture.

```bash
CRUST_PTEX_STREAM_MIPSPACE=file crust render -i island.usda
```

## Other variables

### OCIO

OpenColorIO's own variable: the config every colour is managed with, as a `.ocio` file,
an `.ocioz` archive or an `ocio://` builtin URI. Crust Render uses it when
[`--ocio-config`](@/docs/reference/command-line.md#ocio-config) is not given, which takes
precedence. Unset or empty, the builtin ACES CG config is used. A config that can't be
loaded, or lacks a space Crust Render needs, stops the render with an error naming
`$OCIO`.

```bash
OCIO=/studio/config.ocio crust render -i scene.usda --working-space acescg
```

### RAYON_NUM_THREADS

Read by the [rayon](https://docs.rs/rayon) thread pool, not by Crust Render itself. It
sets the number of render threads. Unset, every core is used.

```bash
RAYON_NUM_THREADS=8 crust render -i scene.usda
```
