# Profiling ALab: where a production frame spends its time

`--profile` on a production-scale stage. The subject is Netflix Animation
Studios' ALab 2.2, shot mk020_0281, frame 1004. This document records what the
frame costs today, then keeps the first profile (2026-09-27) and the fixes it
led to as history.

Measured 2026-10-05 at `4143c58` (0.5.0). The main findings:

- **The render is no longer the frame.** Rendering takes 23.4 s, down from
  3:20.7 in the first profile (−88%). It is now 12% of a 3:20 run. The other
  88% is USD composition during the prim traversal.
- **Texture lookups are still the largest render section, at 43% of thread
  time.** That is 1.57 µs per `eval`, against 21.55 µs before the cache fix.
  Ray tracing is next at 40%, so the frame is now close to the even split a
  healthy render shows.
- **The render scales with threads, close to what the machine allows.** 72
  threads render 5.51× faster than 8 at equal spp, against 5.16× before
  `grow-texture-microcache` and 1.25× in the first profile. This VM allows
  at most about 5.6× from 8 to 72 threads (see "Thread scaling"), so most of
  the gap to 9× is the machine. Texture lookups no longer carry contention of
  their own; what is left is idle threads at the end of the pass.

## Setup

```bash
CAM=/root/camera01/GEO/renderCam_hrc/renderCam_buffer/renderCam_srt/renderCam
cargo run --release -- render -i samples/ALab/entry.usda -f 1004 --camera $CAM --stats    # timings
cargo run --release -- render -i samples/ALab/entry.usda -f 1004 --camera $CAM --profile  # sections
```

- **Machine:** a 72-vCPU VM (`Intel Xeon E5-2699 v4`, reported as one socket
  of 72 cores with no SMT, 16 MiB L3, 93 GiB RAM). A physical E5-2699 v4 has 22
  cores, so these vCPUs span several physical sockets. That makes cache-line
  traffic between threads expensive, and it is what this profile is sensitive to.
  It also caps scaling: 72 single-threaded renders run at once deliver only 41×
  the throughput of one (see "Thread scaling").
- **Settings:** the stage authors no `crust:` render settings, so the importer
  defaults apply: 640×360, 128 spp, depth 32, adaptive (min 32), power MIS, power
  light selection, triangle filter, indirect clamp 10, bucket order.
- **Assets:** the techvar assets are merged over `fragment/` (see "Known gaps: ALab" in
  `openspec/specs/usd-scene-import/design.md`). Every ALab texture is a tiled mip EXR, so all 5 722 of them
  stream through the `.tx` cache and none is preloaded.
- **Runs:** one unprofiled `--stats` run for wall times and memory, then one
  `--profile` run for section shares, run one after the other. The profiled
  run's traversal was 22 s faster (2:30.2 against 2:52.8), because the page
  cache was warm. Take wall times from the unprofiled run and shares from the
  profiled one. The profiler's own overhead is estimated at 3.1% of thread time.

## The run

| phase | wall | RSS at end | peak | first profile |
|---|---|---|---|---|
| Parse USD stage | 2:56.8 | 28.37 GiB | 28.37 GiB | 3:01.6 |
| · Open stage | 0.004 s | 8.02 MiB | 8.02 MiB | 0.020 s |
| · Traverse prims | 2:52.8 | 28.01 GiB | 28.01 GiB | 2:56.3 |
| · Load assets | 2.6 s | 28.01 GiB | 28.01 GiB | 3.1 s |
| · Commit acceleration structure | 1.4 s | 28.37 GiB | 28.37 GiB | 2.1 s |
| Render | **23.4 s** | 28.64 GiB | 28.64 GiB | 3:20.7 |
| Write output | 0.047 s | | | 0.036 s |
| **total** | **3:20.3** | | **28.65 GiB** | 6:22.4, 33.20 GiB |

The import is now 88% of the run, and nearly all of that is openusd composition
during the traversal (see "Where import time goes" in
`openspec/specs/usd-scene-import/design.md`). It has barely moved since the
first profile, so it is now the lever for the frame as a whole. Peak memory is
reached during the traversal, and the render adds only 0.3 GiB on top.

### What the scene holds

| | |
|---|---|
| geometries | 13 302 |
| top-level BVH primitives | 1 991 360 (1 979 504 triangles, 11 819 instances, 37 analytic light shapes) |
| primitives in memory | 30 597 852 (21 166 922 triangles, 9 418 288 cubic curve spans, 12 605 instances) |
| allocated materials | 1 794: 1 749 textured `UsdPreviewSurface`, 45 `Emissive` |
| lights | 43: 23 sphere, 13 cylinder, 4 rect, 1 disk, 1 distant, 1 dome |
| kernel memory | 3.82 GiB (1.40 triangle packets, 0.86 curve spans, 0.67 BVH nodes, 0.48 triangle records, 0.18 leaves, 0.12 vertices, 0.11 normals) |
| packet lanes filled | 82.7%; 193.6 bytes per triangle |

- **Kernel memory fell from 9.16 to 3.82 GiB**, because each triangle is now
  stored once, with shared vertex and normal tables
  (`openspec/specs/intersection-kernel/design.md`).
- **The rest of the 28.65 GiB peak, about 24.8 GiB, is the composed USD
  stage.** A single-stage import keeps it allocated (`skip_stage_teardown`).
- **Four fewer lights than the first profile**, because of light links (below).

## Render profile

Thread time over the whole render: 28:56 across 72 threads, which is 95.2% of
72 × wall.

| section | glob. | thread time | calls | per call | first profile |
|---|---|---|---|---|---|
| **Texture** | **43.0%** | 12:25.7 | 480 977 911 | **1.57 µs** | 88.6%, 21.55 µs |
| Trace | 33.7% | 9:44.3 | 99 758 594 | 5.86 µs | 6.5%, 7.60 µs |
| Occlusion | 6.3% | 1:49.1 | 41 157 683 | 2.65 µs | 1.5%, 3.94 µs |
| EvalBsdfs (local) | 5.4% | 1:34.4 | 99 745 233 | 0.95 µs local, 8.49 µs incl. textures | 1.0% |
| SurfaceLighting (local) | 4.9% | 1:24.5 | 99 745 233 | 0.85 µs local | 1.2% |
| MainLoop | 3.3% | 56.7 s | 1 828 312 | | 0.6% |
| Bounce | 2.5% | 43.6 s | 99 745 233 | 437 ns | 0.5%, 597 ns |
| GeneratePrimary | 0.4% | 7.6 s | 29 278 681 | 258 ns | 0.1% |
| TextureLoad | 0.4% | 7.1 s | 51 399 | 139 µs | 0.1% |
| Contributions | 0.2% | 3.0 s | 29 278 681 | 104 ns | 0.0% |

By category: **Shading 48.4%, Raytrace 39.9%, Integrator 11.3%**, IO 0.4%. The
first profile measured 89.5 / 8.0 / 2.4. Guerilla's documentation calls a
roughly even split between Raytrace, Shading and Integrator the healthy shape.
The frame is now near it, with shading still the largest share.

The execution tree is the same shape as before: `MainLoop → EvalBsdfs →
Texture`. Of EvalBsdfs' 14:07 total, 12:33 is inside `Texture`.
`TextureLoad`, which pages a tile in from disk, is only 7 s of that. Shading a
point without its textures costs under 1 µs. The texture path is still what a
shading point costs.

### Texture lookups

| | now | first profile |
|---|---|---|
| texture `eval`s per shading point | 4.82 | 4.8 |
| texel lookups per `eval` | 6.59 | 6.5 |
| lookups | 3.17 G | 3.28 G |
| thread microcache hits | 85.3% | 74.9% |
| shard-cache hits | 14.7% | 25.1% |
| loaded | 597.8 MiB in 51 617 tiles, nothing evicted, 1 GiB budget | 503 MiB in 45 968 tiles |
| concurrent double fills | 641 | 353 |

The microcache hit rate is the one the cache fix reached, so 15% of lookups
still take a shard mutex. The textures would total 44.95 GiB with every level
resident. Streaming holds the frame under 0.6 GiB.

## Thread scaling

Measured 2026-10-08 at `3c8b861`: the same frame and camera, one render at a
time on an idle machine, varying only `RAYON_NUM_THREADS`. The frame now does
about 18% more ray queries than at 0.5.0 (165.8 M against 140.9 M at 128 spp),
so its render times are longer than the ones above. What matters here is the
ratio.

| run | 8 threads | 72 threads | 8 → 72 |
|---|---|---|---|
| 32 spp, `--stats` | 38.27 s | 7.42 s (repeat: 7.44 s) | **5.16×** |
| 32 spp, `--profile` | 41.38 s | 8.02 s | 5.16× |
| default 128 spp, adaptive | 2:31.7 | 29.27 s | **5.18×** |

- **72 threads buy 5.16× over 8.** These are renders run one after the other
  on an idle machine, not interleaved: the two 72-thread runs agree within
  0.3%. `bench_ab.sh` with thread-count wrappers ("Reproducing the scaling
  pair") is the interleaved form, which a later measurement should use.
  At 128 spp, adaptive sampling stops only 1.1% of pixels, so each of its
  rounds is nearly a full sweep and the ratio matches 32 spp's.
- **The 5.4× this section used to report was an extrapolation.** It came from
  two profiled runs at 8 and 32 spp, with the 8-thread time scaled ×4. At equal
  spp, the ratio is 5.16×.

### What the machine allows

The thread ratio, 9×, is out of reach on this VM whatever the renderer does.
The control is N copies of a single-threaded cornellbox render, started
together (`RAYON_NUM_THREADS=1 crust render -i samples/cornellbox.usda -s 16`).
They share nothing in software, so whatever each copy loses, the machine
takes:

| copies | seconds per copy (mean) | efficiency | throughput |
|---|---|---|---|
| 1 | 4.96 | 1.00 | 1× |
| 8 | 5.43 | 0.91 | 7.3× |
| 16 | 6.04 | 0.82 | 13.1× |
| 24 | 6.23 | 0.80 | 19.1× |
| 36 | 6.27 | 0.79 | 28.4× |
| 48 | 6.70 | 0.74 | 35.5× |
| 60 | 7.64 | 0.65 | 38.9× |
| 72 | 8.70 | 0.57 | **41.0×** |

- **At 72 threads the machine gives 41×, and 8 → 72 gives at most 5.6×.**
  - Up to 36 copies, the loss fits the turbo clock falling from 3.6 to 2.8 GHz
    as more cores wake (a ratio of 0.78). Cornellbox fits in L2, so it is not
    memory.
  - Past 36, the vCPUs behave like hyperthread siblings or shared cores, and
    the copies spread out: the slowest of the 72 took 9.71 s.
  - The VM reports one socket and one NUMA node, so these causes are inferred
    from the curve, not observed.
- **A thread doing the same work is 1.6× slower at 72-way occupancy than at
  8-way.** That, not 1.0×, is the bar a per-call slowdown has to be judged
  against.
- **Crust itself reaches 35.2× on cornellbox at 72 threads** (128 spp), 86% of
  the 41×.
- **ALab cannot be calibrated this way.** A copy peaks at 29 GiB, so 72 of them
  do not fit.

### Per call, against the machine

The profiled pair, at 32 spp:

| section | 8 threads | 72 threads | slowdown | against the machine's 1.6× |
|---|---|---|---|---|
| GeneratePrimary | 168 ns | 263 ns | 1.57× | at it |
| Bounce | 293 ns | 480 ns | 1.64× | at it |
| SurfaceLighting (local) | 1.46 µs | 2.16 µs | 1.48× | below |
| Trace | 4.05 µs | 5.88 µs | 1.45× | below |
| Occlusion | 2.07 µs | 2.91 µs | 1.41× | below |
| **Texture** | **895 ns** | **1.71 µs** | **1.91×** | **1.19× above** |
| TextureLoad | 76.5 µs | 176.5 µs | 2.31× | above, but 1.5% of thread time |

- **Texture is the one section with contention of its own, about 1.19×.** The
  1.8× this section used to attribute to contention was mostly the machine.
  - The microcache still hits 84.4%, so 15.6% of 972 M lookups reach a shard,
    about one per `eval`.
  - A shard hit writes to cache lines that every thread reading that tile
    shares: it locks the shard `Mutex` and increments the tile's `Arc` count,
    and the microcache drops another `Arc` when it evicts an entry later.
  - The excess, about 0.28 µs per `eval`, is consistent with one or two
    cross-socket line transfers.
- **Traversal has no contention problem.** Trace and Occlusion slow down less
  than the machine factor: memory-bound work loses less to the clock and gains
  more from a hyperthread sibling. So ALab's own limit is probably somewhat
  above cornellbox's 5.6×.
- **Thread time grows 1.67×** (5:28.2 → 9:08.9), against the machine's 1.60×.
- **At 8 threads the frame is still nearly even**: Texture 39.1%, Trace 33.8%.

### Where the 72-thread render goes

72 threads × 8.02 s is 577 s of thread capacity:

| | thread time | share |
|---|---|---|
| work at the machine's 72-way speed | 510.3 s | 88.4% |
| texture time beyond the 1.6× factor | 38.6 s | 6.7% |
| idle | 28.5 s | 4.9% |

- **The idle time is the end of the pass.** Threads are busy 95.1% of the wall
  at 72 threads, against 99.1% at 8. At 640×360 the pass is 920 tiles of 16×16,
  about 13 per thread, and the last expensive ones finish alone.
- **Without both losses, the render would take about 7.2 s**, 5.8× over 8
  threads. That is about 11% of the 72-thread render's time. If texture should
  scale like Trace (1.45×) rather than like compute, the figure is 14%.

### After `grow-texture-microcache` (2026-10-09)

The per-thread texture microcache grew from 64 to 512 tiles a thread, chosen by
measurement (textures design record, "Streaming"). Lookups reaching a shard fell
from 152.0 M to 50.3 M (microcache hits 84.4% → 94.8%). The same profiled pair,
at 32 spp:

| section | 8 threads | 72 threads | slowdown | before |
|---|---|---|---|---|
| **Texture** | **770 ns** | **1.24 µs** | **1.61×** | 895 ns / 1.71 µs, 1.91× |
| Trace | 3.89 µs | 5.71 µs | 1.47× | 1.45× |
| Occlusion | 1.91 µs | 2.71 µs | 1.42× | 1.41× |
| Render (profiled, run one after the other) | 37.80 s | 6.86 s | **5.51×** | 41.38 s / 8.02 s, 5.16× |

- **Texture's slowdown is now the machine's own 1.6×.** The contention is gone.
- **The 72-thread render is 15.9% faster** (`bench_ab.sh -n 3`, unprofiled,
  min and mean alike: 7.28 / 7.34 s → 6.12 / 6.17 s), and the 8-thread one
  8.7% (profiled).
- **What is left of the 72-thread render's capacity is idle time.**
  72 × 6.86 s is 494 s, against 469 s of thread time. Threads are busy 94.9%
  of the wall, against 99.6% at 8. That idle 5.1% is the end of the pass, and
  removing it would take 72 threads to about 5.7× over 8.

## Other findings

- **Light and shadow links are read now.** The import splits the scene into 3
  occluder classes. `lgt_env_dome` ignores 1 class, `lgt_sun_distant` ignores
  2, and `lgt_sun_area_01` and `_02` ignore 1 each. A light that ignores an
  occluder class is MIS-combined through its link twin, the dome excepted (see
  "Shadow-linked lights and their noise" below; the history section "Shadow
  links" has the rig's exclusions).
- **The four `lgt_screenLights` rect lights are dropped.** Their
  `collection:lightLink` has `includeRoot = 0`, and its `includes` name
  `/root/electronics_ham_equipment03/GEO/…/screen0N_M_geo`. That path matches
  no receiver in the composed stage, where the placed assets live under
  `/root/alab_set01/…/electronics_ham_equipment03_000N`. A light that
  illuminates nothing leaves the light list (DEBUG:
  `collection:lightLink includes no receiver`). The first profile warned
  about the link, ignored it, and let these lights illuminate everything. It
  has not been checked whether the targets are an authoring quirk or a path
  openusd should have remapped through the reference.
- **NEE wastes less, but most shadow rays still miss.** Of 99.7 M light
  samples, 41.3% were worth a shadow ray, and 81.7% of the 41.2 M shadow rays
  cast were occluded, against 96.0% before. So about 7.6% of light samples now
  deliver light, against 1.7%. This comparison has three causes, and they are
  not separated here: the louvered windows no longer block the sun lights, the
  screen lights are gone, and the adaptive sampler has changed. Power selection
  is still the default; `--light-selection learned` is the remedy measured
  under "Outcome: NEE's misses".
- **Paths end by absorption or roulette, almost never by escaping.** Absorbed:
  58.0%. Roulette: 42.0% (76.0% of roulette tests kill). Escaped: 0.04%. Depth
  cap: 919 paths. The mean path length is 3.41 vertices.
- **Adaptive sampling barely engages.** 1.2% of pixels stopped early (0.6%
  more were held back by a neighbour), and the average is 127.1 of 128 spp.

## Shadow-linked lights and their noise (2026-10-08)

Change `mis-for-shadow-linked-lights`. All four exterior lights are shadow-linked:
the sun lights ignore the louvered windows, and the distant sun and the dome also
ignore the sky-dome sphere. Before the change each was sampled by NEE alone at
continuous vertices, so MIS did nothing for them. Now the distant sun and the two
rect suns get a link twin; the dome stays NEE-only.

### Method

Each measurement is two renders that differ only in sampler seed:

- `groups.usda` sublayers `entry.usda`, sets `crust:light:lpeTag` on the four
  exterior lights (`sun_distant`, `sun_area_01`, `sun_area_02`, `env_dome`), and
  defines `/Render/settings` with `crust:varianceThreshold = 0` (adaptive sampling
  off) and one product of `C<RG><L.'tag'>` / `C<RD><L.'tag'>` vars per light, plus
  `C<RG>[LO]` and `C<RD>[LO]`. The settings prim must be `/Render/settings`, the
  only path the importer reads; a differently named one is ignored without a word
  beyond a DEBUG line, and the render falls back to the stage's first camera.
- `groups_seed2.usda` is `subLayers = [@./groups.usda@ (offset = 1)]`. Rendered at
  `-f 1005`, it evaluates the scene at 1004 with seed 1005.
- Per pixel, the noise variance of the two-seed mean is `(a − b)² / 4` of the
  luminance, summed over the frame. "Top 0.1%" is the share of that sum held by the
  beauty's 0.1% noisiest pixels; "worst" is the noisiest pixel's standard deviation.

```bash
CAM=/root/camera01/GEO/renderCam_hrc/renderCam_buffer/renderCam_srt/renderCam
crust render -i groups.usda       -f 1004 --camera $CAM -s 256 --light-samples 4 --light-selection learned
crust render -i groups_seed2.usda -f 1005 --camera $CAM -s 256 --light-samples 4 --light-selection learned
```

The overlays and the comparison tool are scratch files and not committed.

### Before and after

256 spp, `--light-samples 4 --light-selection learned`, default clamp (10). The
"before" binary is `e228475`.

| layer | before: variance (top 0.1%, worst) | after | change |
|---|---|---|---|
| beauty | 5 589 (96.8%, 39.2) | 3 692 (95.2%, 25.4) | −34% |
| `C<RG>[LO]` direct glossy | 5 445 (99.2%, 39.2) | 3 137 (98.2%, 25.4) | −42% |
| `C<RD>[LO]` direct diffuse | 102.6 | 71.7 | −30% |
| `lgt_sun_distant` glossy | **3 551 (99.5%, 39.2)** | **100.4 (74.2%, 3.6)** | **−97.2%** |
| `lgt_sun_distant` diffuse | 96.4 | 64.8 | −33% |
| `lgt_sun_area_01` glossy | 3 079 | 2 853 | −7% |
| `lgt_env_dome` glossy / diffuse | 0.1 / 0.5 | 0.1 / 0.5 | none (NEE-only) |

- **The distant sun is fixed.** Its direct glossy variance falls 97.2%, which is
  the drop predicted by deactivating the excluded blockers (3 545 → 103). A second
  seed pair agrees: 2 063 → 102.5.
- **The rect sun barely moves**, as the design predicted from the same blocker
  experiment (−9% there). The design located 89% of its variance in about 20
  pixels of a real highlight at the right frame edge (x 638–639, y 116–126):
  sub-pixel coverage noise of a glint, not light-sampling noise (not re-measured
  here).
- **The frame's noise is now mostly the rect sun's**: 2 853 of the 3 692.
- **The means agree.** Direct diffuse is unchanged to 4 digits (0.14740 against
  0.14737). The distant sun's direct glossy read 0.00508 and 0.00537 before (two
  seed pairs) against 0.00573 and 0.00575 after. That is the heavy tail of the
  NEE-only estimator, not a bias: rendered at 1024 spp, the before estimate climbs
  to 0.00564, within 0.5σ, while its variance falls only 1.5× where 4× would be
  Gaussian. The beauty reads 3% brighter after (0.2100 → 0.2165): the firefly clamp
  at 10 is biased, and it clamps the lower-variance estimate less.

### Cost

`scripts/bench_ab.sh -n 3 -p Render` against `e228475`, same flags, frame 1004:
60.1 s → 61.5 s, **+2.3%** (min and mean alike), inside the change's +5% budget.
The twin casts a shadow ray only when the bounce direction actually reaches a
twinned light: the 20° sun cone or one of the two rect suns.

## What is left, in order of what it would buy

1. **The import.** At 2:53 of a 3:20 frame, composition is now where a
   single frame's time goes. The render could halve again and the frame would
   be 6% faster.
2. **The end of the pass.** Texture contention is gone
   (`grow-texture-microcache`: a texture call's 8 → 72 slowdown is now the
   machine's 1.61×). What is left of the 72-thread gap is threads idling while
   the last tiles finish: 5.1% of capacity. Removing it would take 72 threads
   from 5.51× to about 5.7× over 8, not to 9×, which this VM cannot reach
   ("Thread scaling").
3. **Traversal.** Trace is now a third of render thread time, at 5.9 µs per
   closest-hit query on this VM.
4. **Light selection.** 82% of shadow rays are still occluded under power
   selection.

## History: the first profile (2026-09-27, `bd07ea2`)

The first `--profile` run on a production-scale stage. Its main finding was
that **streamed texture lookups took 89% of render thread time, and the cost was
contention, not work.** On this machine, 72 threads rendered only about 1.25×
faster than 8. The machine reported 61 GiB of RAM at the time. Everything
below is as measured then.

### The run

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

#### What the scene holds

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

### Render profile

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

### Diagnosis: the texture cache serialises the render

#### It scales with the thread count, not the work

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

#### Where the workers are

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

#### Why: two things shared by all 72 threads on every lookup

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

### Other findings

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

### What to do about it, in order

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

### Outcome: fixes 1–3 landed

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

### Outcome: NEE's misses

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

### Shadow links: the louvered windows block the key

Measured 2026-09-29. The light rig
(`fragment/lightrig/lighting/mk020_0281_export/base/placement/…_placement.usda`)
authors `collection:shadowLink` on its exterior lights, and crust read none of it
at the time (it does now; see "Light and shadow links" above):
- **`lgt_env_dome`** excludes `/root/dmp_skydome_alab01`, the matte-painting sphere
  (radius 350 000) around the set that carries the same sky texture as the dome.
- **`lgt_sun_distant`** excludes the skydome and the two
  `wall04/structure_window_louvered02_000{1,2}` windows.
- **`lgt_sun_area_01` / `_02`** exclude the two louvered windows.

Two lights also author per-object `lightLink` (`lgt_screenLights*`:
`includeRoot = 0`, `includes` = the display screens). That link only warns and is
ignored, as before.

The same `light_occlusion` run (160×90 receivers, 4 samples per light, power
selection) over wrapper layers that deactivate the shadow-excluded geometry.
Deactivating also hides it from the camera, so this attributes the misses; it is
not a render.

| NEE visible, weighted by pick | as is | skydome off | skydome + louvers off |
|---|---|---|---|
| all lights | 3.2% | 3.2% | **16.9%** |
| rect #3, pmf 0.355 (the brightest) | 0.0% | 0.0% | **38.3%** |
| distant sun | 0.0% | 0.0% | 1.9% |
| dome | 0.0% | 0.1% | 0.3% |

- **The skydome costs nothing in this shot.** The camera looks at the interior,
  and the walls and roof block the dome and the sun either way.
- **The louvered windows are the loss.** They take the most-picked light from 0%
  to 38% visible, 29% of its samples having been stopped by their glass. That is
  the light the rig excludes them from.
- **This revises "Outcome: NEE's misses" above.** At least one of the "exterior
  lights visible from none of the frame" is invisible only because shadow linking
  is not read. `learned` selection learns to avoid a light the rig intends to be
  the key.
- **This is `add-light-and-shadow-linking`'s shadow-linking half.** Its D3 needs
  three shadow classes here (skydome, louver 1, louver 2), well inside the 28-bit
  budget.

## Reproducing the scaling pair

```bash
CAM=/root/camera01/GEO/renderCam_hrc/renderCam_buffer/renderCam_srt/renderCam
# the ratio, interleaved: one wrapper per thread count, alternated by bench_ab.sh
# (the 8 -> 72 ratio is A min / B min)
for t in 8 72; do
  printf '#!/bin/sh\nRAYON_NUM_THREADS=%s exec %s "$@"\n' $t "$PWD/target/release/crust" > /tmp/crust_t$t
  chmod +x /tmp/crust_t$t
done
scripts/bench_ab.sh -a /tmp/crust_t8 -b /tmp/crust_t72 -n 3 -x "-f 1004 --camera $CAM -s 32" samples/ALab/entry.usda
# per-call times: one --profile run at each count
RAYON_NUM_THREADS=72 cargo run --release -- render -i samples/ALab/entry.usda -f 1004 --camera $CAM -s 32 --profile
RAYON_NUM_THREADS=8  cargo run --release -- render -i samples/ALab/entry.usda -f 1004 --camera $CAM -s 32 --profile
# what the machine allows: N single-threaded copies at once (N = 1, 8, ..., 72);
# a lone copy's Render time over each copy's here is the machine's efficiency at N
N=72; mkdir -p /tmp/cal$N
for k in $(seq 1 $N); do
  RAYON_NUM_THREADS=1 target/release/crust render -i samples/cornellbox.usda \
    -o /tmp/cal$N/$k.exr -s 16 --stats > /tmp/cal$N/$k.log 2>&1 &
done; wait; grep -h '^  Render ' /tmp/cal$N/*.log
# light and shadow links, and why a light left the list:
cargo run --release -- render -i samples/ALab/entry.usda -f 1004 --camera $CAM -s 1 -l debug 2>&1 | grep light_links
# stack snapshots during the render phase of the first:
eu-stack -p "$(pgrep -x crust)" > stacks.txt
```

Each run peaks at about 29 GiB, so run them one at a time.
