# Profiling ALab: where a production frame spends its time

The first `--profile` run on a production-scale stage. The subject is Netflix
Animation Studios' ALab 2.2, shot mk020_0281, frame 1004. This document records
what the render costs and what the profile says about it. The main finding is
that **streamed texture lookups take 89% of render thread time, and the cost is
contention, not work.** On this machine, 72 threads render only about 1.25×
faster than 8.

Measured 2026-09-27 at `bd07ea2`.

## Setup

```bash
CAM=/root/camera01/GEO/renderCam_hrc/renderCam_buffer/renderCam_srt/renderCam
cargo run --release -- -i samples/ALab/entry.usda -f 1004 --camera $CAM --stats    # timings
cargo run --release -- -i samples/ALab/entry.usda -f 1004 --camera $CAM --profile  # sections
```

- **Machine:** a 72-vCPU VM (`Intel Xeon E5-2699 v4`, reported as one socket
  of 72 cores with no SMT, 16 MiB L3, 61 GiB RAM). A physical E5-2699 v4 has 22
  cores, so these vCPUs span several physical sockets. That makes cache-line
  traffic between threads expensive, and it is what this profile is sensitive to.
- **Settings:** the stage authors no `crust:` render settings, so the importer
  defaults apply: 640×360, 128 spp, depth 32, adaptive (min 32), power MIS, power
  light selection, triangle filter, indirect clamp 10.
- **Assets:** the techvar assets are merged over `fragment/` (see "Known gaps: ALab" in
  `openspec/specs/usd-scene-import/design.md`). Every ALab texture is a tiled mip EXR, so all 5 722 of them
  stream through the `.tx` cache and none is preloaded.

## The run

| phase | wall | RSS at end | peak |
|---|---|---|---|
| Parse USD stage | 3:01.6 | 33.00 GiB | 33.04 GiB |
| · Open stage | 0.020 s | 6.91 MiB | 6.97 MiB |
| · Traverse prims | 2:56.3 | 32.28 GiB | 32.28 GiB |
| · Load assets | 3.1 s | 32.28 GiB | 32.28 GiB |
| · Commit acceleration structure | 2.1 s | 33.00 GiB | 33.04 GiB |
| Render | 3:20.7 | 33.20 GiB | 33.20 GiB |
| Write output | 0.036 s | | |
| **total** | **6:22.4** | | **33.20 GiB** |

The render is 52% of the run and the import 48%. Nearly all of the import is
openusd composition during the traversal (see "Where import time goes" in
`openspec/specs/usd-scene-import/design.md`). Peak memory is reached during the traversal, and the render adds
only 0.2 GiB on top.

A second, profiled run measured Render at 2:52.9 and Traverse at 2:39.2, *faster*
than the unprofiled run despite the profiler's cost. Both phases dropped by about
the same fraction, so this is the page cache being warm on the second run, plus
machine noise. It is not the profiler. As "Measuring a change" says, sequential
runs are not an A/B. Take section *shares* from the profiled run and wall times
from the unprofiled one.

### What the scene holds

| | |
|---|---|
| geometries | 13 302 |
| top-level BVH primitives | 1 992 137 (1 979 504 triangles, 12 596 instances, 37 analytic light shapes) |
| primitives in memory | 30 597 843 (21 166 922 triangles, 9 418 288 cubic curve spans) |
| allocated materials | 1 794: 1 749 textured `UsdPreviewSurface`, 45 `Emissive` |
| lights | 47: 23 sphere, 13 cylinder, 8 rect, 1 disk, 1 distant, 1 dome |
| kernel memory | 9.16 GiB (3.65 primitive nodes, 2.35 triangle packets, 1.89 BVH nodes, 0.87 boxed prims) |

Kernel memory is 9.16 GiB of the 33.2 GiB peak. Most of the rest is the composed
USD stage, which a single-stage import keeps allocated (`skip_stage_teardown`).

## Render profile

Thread time over the whole render: 203:43 across 72 threads, which is 98.2% of
72 × wall. The workers are busy all the time; the question is what with.

| section | glob. | thread time | calls | per call |
|---|---|---|---|---|
| **Texture** | **88.6%** | 180:25.8 | 502 809 768 | **21.55 µs** |
| Trace | 6.5% | 13:13.9 | 104 506 892 | 7.60 µs |
| Occlusion | 1.5% | 2:59.2 | 45 434 725 | 3.94 µs |
| SurfaceLighting | 1.2% | 2:25.3 | 104 494 182 | 3.10 µs |
| EvalBsdfs (local) | 1.0% | 1:59.0 | 104 494 182 | 104.82 µs incl. textures |
| MainLoop | 0.6% | 1:11.9 | 230 400 | 53 ms per pixel |
| Bounce | 0.5% | 1:02.4 | 104 494 182 | 597 ns |
| GeneratePrimary | 0.1% | 12.0 s | 28 999 152 | 413 ns |
| TextureLoad | 0.1% | 8.2 s | 45 936 | 179 µs |
| Contributions | 0.0% | 5.5 s | 28 999 152 | 191 ns |

By category: **Shading 89.5%**, Raytrace 8.0%, Integrator 2.4%, IO 0.1%.
Guerilla's documentation calls a roughly even split between Raytrace, Shading
and Integrator the healthy shape, and treats a strong deviation as the sign of a
problem in the scene or engine. This is a strong deviation.

The execution tree places it precisely: `MainLoop → EvalBsdfs → Texture`. Of
EvalBsdfs' 182:33 total, 180:34 is inside `Texture`, and `TextureLoad` (paging a
tile in from disk) is only 8 s of that. **The texture time is not I/O.** The
profiler's own overhead is estimated at 0.5% of thread time, so it does not
distort these shares.

## Diagnosis: the texture cache serialises the render

### It scales with the thread count, not the work

The same frame at the same settings, varying only the thread count (profiled
runs; per-call times are what matter here):

| threads | spp | Render | Texture µs/call | Trace µs/call | EvalBsdfs µs/call |
|---|---|---|---|---|---|
| 72 | 32 | 48.0 s | 23.38 | 7.90 | 113.7 |
| 8 | 8 | 15.1 s | 2.26 | 4.46 | 11.4 |

At 9× the threads, each texture call is **10× slower**. Normalised to the same
32 spp, 8 threads would take about 60 s, so **72 threads buy a 1.25× speedup over
8.** With 8 threads the Texture share is already 58.6% (Trace 24.7%), so the
lookup path is expensive even without much contention. At 72 threads it swamps
everything else. Trace also slows, from 4.46 to 7.90 µs, which is consistent
with the texture traffic saturating the shared cache and memory bus that
traversal needs too.

### Where the workers are

Three `eu-stack` snapshots of the 72-thread render, taken 4 s apart (`perf` is
not installed). Top frames, sample 2:

| where | threads |
|---|---|
| `StreamingTexture::texel` (microcache probe + shared counters, inlined) | 43 |
| `TileCache::get` itself (the shard-map path) | 5 |
| `Mutex::lock_contended` → futex wait, from `TileCache::get` | 7 |
| futex syscalls made from `TileCache::get` (lock hand-off) | 4 |
| BVH traversal (`Bvh::hit`, `Scene::occluded`) | 3 |
| everything else (sampling, BSDF, lights, rayon) | 10 |

The other two samples agree: 39–46 threads in `texel` and 18–28 in or waiting on
`TileCache::get`, against 3–4 tracing rays.

### Why: two things shared by all 72 threads on every lookup

The texture path does about **31 texel lookups per shading point**: 4.8
texture `eval`s per shading point (502.8 M / 104.5 M) times 6.5 lookups per
`eval` (3.28 G / 502.8 M), the taps of a trilinear filter. At that rate, anything
a lookup writes to shared memory is paid 3.3 billion times.

1. **The `micro_hits` counter** (`tiled/cache.rs`, `with_tile`). Every lookup,
   including the 74.9% that hit the per-thread microcache, does
   `cache.stats.micro_hits.fetch_add(1, Relaxed)`. That is one atomic on one
   cache line shared by every worker. The line has to move between cores on
   every increment, and on a VM spanning sockets that move is expensive. This
   turns the *fast* path into a serialisation point.
2. **The microcache is too small for a production material.** It holds the two
   most recently used tiles per thread, shared across *all* textures. It was
   sized on a single textured plane, where a bilinear tap reads one tile four
   times and trilinear alternates between two levels (98.6% hit rate measured
   there). An ALab material samples about five textures per shading point, so
   each `eval` finds the two slots holding the previous texture's tiles and
   misses on its first tap at each of its two levels. That is roughly 2 misses
   out of 6.5 lookups, which matches the **25.1% miss rate** measured. Each
   miss then takes the shared path:
   - the shard `Mutex` (64 shards for 72 threads, and the hot tiles of a frame
     concentrate on few of them);
   - a write to `e.used = true`, which dirties the shard's line even on a pure
     read;
   - `stats.hits.fetch_add`, a second shared counter;
   - an `Arc<Tile>` clone into the microcache, and a decrement when the
     displaced slot drops. These are two atomics on the tile's refcount, which
     is shared by every thread reading that tile.

The miss path accounts for the threads parked in `lock_contended`. The counter
accounts for the threads that stay inside `texel`, where the stack shows no lock
at all.

## Other findings

- **NEE mostly misses.** Of 104.5 M light samples, only 43.5% were worth a
  shadow ray. The rest were refused by the cheap tests, zero radiance first,
  because a one-sided light was seen from behind. Of the 45.4 M shadow rays
  cast, **96.0% were occluded**. So about 1.7% of light samples deliver light.
  ALab is an interior lit by practicals in fixtures (lamps, 13 cylinder button
  lights), and power-based light selection cannot see that most lights are
  hidden from most points. This is the noise case the light BVH in
  `docs/light_sampling.md` §6.3 exists for, and the main thing to measure once
  texture cost stops dominating.
- **Paths end by roulette or absorption, almost never by escaping.** Escaped:
  0.04%. Absorbed: 43.8%. Roulette: 56.1% (77.6% of roulette tests kill). Depth
  cap: 172 paths. The mean path length is 3.6 vertices, and the depth ceiling of
  32 costs nothing.
- **Adaptive sampling barely engages.** 2.5% of pixels stopped early, and the
  average is 125.9 of 128 spp. The default threshold is not tuned for a scene
  this noisy.
- **Streaming works as designed.** The textures total 44.95 GiB if every level
  were resident. The cache loaded 503 MiB, holds 500 MiB at the end under a
  1 GiB budget, and evicted nothing. Loaded tiles: 45 968. Concurrent double
  fills: 353, which is 72 workers first-touching the same tiles.
- **Shading work itself is cheap.** Excluding textures, EvalBsdfs is about
  1.1 µs per shading point, Bounce 0.6 µs and SurfaceLighting 1.4 µs excluding
  its shadow ray. There is little to gain in the BSDF or integrator code until
  the texture path is fixed.

## What to do about it, in order

1. **Take the shared counters off the lookup path.** Count per thread and sum
   when the report is built, or count only on the miss path. This is the
   cheapest change and removes a write to a shared line from every one of the
   3.3 G lookups. It is output-preserving.
2. **Make the microcache survive texture interleaving.** Options: more slots
   (8–16), or slots keyed per texture so each texture keeps its own recent
   tiles. Target a microcache hit rate on ALab close to the plane scene's 98.6%.
   The four-slot Ptex microcache (`docs/ptex_streaming.md`) is the precedent.
3. **Make a shard hit read-only.** Set `used` only when it is not already set,
   and hand the tile to the caller without an `Arc` clone where the microcache
   does not need to keep it.
4. **Re-profile**, including the thread-scaling pair above. The 8-thread
   2.26 µs per call is a ceiling, not a target: it still includes the same
   counters, just contended less.

The streaming *design* (bounded budget, clock eviction, three tiers) is not what
is wrong. The top tier was sized for a workload with one texture per shading
point, and its bookkeeping writes to memory shared by all threads.

## Outcome: fixes 1–3 landed

The first three fixes above landed together (`tex-cache-contention`):
- per-lookup counters are `StripedCounter`s, one cache line per thread;
- the microcache is set-associative by file, 16 sets × 4 ways;
- a shard hit sets `used` only when it is clear.

Measured with `bench_ab.sh` against the binary profiled here (Render, min / mean):

| scene | before | after | Δ |
|---|---|---|---|
| ALab frame 1004, 32 spp | 45.1 / 47.1 s | 6.34 / 6.63 s | −85.9% |
| alias plane (one streamed texture) | 0.743 / 0.822 s | 0.081 / 0.085 s | −89% |
| materialx_basic (preloaded) | 0.261 / 0.266 s | 0.261 / 0.266 s | 0 |

The re-profile (fix 4), at 72 threads and 32 spp:
- Texture falls from 23.38 to 1.57 µs per `eval`, and from 88.7% to 41% of
  thread time; Trace is now 34.5%.
- Microcache hits rise from 74.9% to 84.9% (8 sets measured 81.2%).
- Images are bit-identical.

The alias plane is the telling row. Its microcache already hit 98.6% of
lookups, so the shared counter by itself was about 90% of that render. The
counter, not the miss rate, was the main cause.

What remains: 15% of lookups still reach the shard mutexes, and consecutive
shading points on one thread are often different materials, which no per-thread
cache absorbs.

## Outcome: NEE's misses

`examples/light_occlusion` settled the NEE finding above. It was selection, not
glass: power sent 49% of the picks to two exterior lights that are visible from
none of the frame. `--light-selection learned` (`docs/light_sampling.md` §3.12)
does the following at 16 spp:
- occluded shadow rays fall from 94.2% to 33.4% in direct lighting;
- direct-lighting relMSE falls **4.1×**;
- the full image's trimmed relMSE falls 1.31×.

It costs +14–23% render time at 128 spp, because shadow rays that reach their
light traverse the whole BVH. At equal time that leaves about 3.6× on direct
lighting and about 1.1× on the full image. The rest of the full image's noise is
indirect light from the windows, which selection cannot reach.

## Reproducing the scaling pair

```bash
CAM=/root/camera01/GEO/renderCam_hrc/renderCam_buffer/renderCam_srt/renderCam
cargo run --release -- -i samples/ALab/entry.usda -f 1004 --camera $CAM -s 32 --profile
RAYON_NUM_THREADS=8 cargo run --release -- -i samples/ALab/entry.usda -f 1004 --camera $CAM -s 8 --profile
# stack snapshots during the render phase of the first:
eu-stack -p "$(pgrep -f crust-render)" > stacks.txt
```
