# Profiling ALab: where a production frame spends its time

`--profile` on a production-scale stage. The subject is Netflix Animation
Studios' ALab 2.2, shot mk020_0281, frame 1004. This document records what the
frame costs today, then keeps the first profile (2026-09-27) and the fixes it
led to as history.

Measured 2026-10-11 at `fc0b962` (openusd 0.8.0, `main` at `7d11cdd`). The
previous profile, 2026-10-05 at `4143c58` (0.5.0), is the "0.5.0" column of
every table below. The main findings:

- **The frame takes 1:58, down from 3:20 at 0.5.0, and peaks at 11.9 GiB
  instead of 28.7.** All of it is the import. The traversal takes 1:30
  against 2:53, and the composed stage holds about 8.5 GiB against 24.8.
  "Where the import went" below separates the changes that did it.
  Composition is still 76% of the frame.
- **Under openusd 0.8.0, instances share prototypes.** The 12 605 instances are now
  built from 380 prototypes instead of 1 417, so 9.2 M duplicate triangles and
  1 037 duplicate materials are no longer imported. The image is unchanged up
  to sampling noise.
- **The render is 23.9 s, as at 0.5.0, for 19% more rays.** Texture lookups
  are 38% of render thread time (1.19 µs per `eval`, against 1.57 µs) and
  tracing 40%.
- **The render scales with threads, close to what the machine allows.** 72
  threads render 5.51× faster than 8 at equal spp, against 5.16× before
  `grow-texture-microcache` and 1.25× in the first profile. This VM allows
  at most about 5.6× from 8 to 72 threads (see "Thread scaling", measured
  2026-10-08/09), so most of the gap to 9× is the machine.

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
  stream through the `.tx` cache and none is preloaded. One texture set is
  missing on disk (`tool_wrench_boxend03` roughness, `texture.unreadable`), the
  import's only warning.
- **Runs:** one unprofiled `--stats` run for wall times and memory, then one
  `--profile` run for section shares, one after the other, with no other
  render running. The page cache was warm for both (ALab had been imported
  minutes before). The profiled run's traversal was 13 s faster (1:17.2
  against 1:29.8) and its render 2.5 s slower (26.4 s), so take wall times
  from the unprofiled run and shares from the profiled one. Each is a single
  run, so a few percent either way is inside this VM's run-to-run spread.

## The run

| phase | wall | RSS at end | peak | 0.5.0 | first profile |
|---|---|---|---|---|---|
| Parse USD stage | 1:33.8 | 11.66 GiB | 11.66 GiB | 2:56.8 | 3:01.6 |
| · Open stage | 0.010 s | 13.42 MiB | 13.42 MiB | 0.004 s | 0.020 s |
| · Traverse prims | **1:29.8** | 11.30 GiB | 11.30 GiB | 2:52.8 | 2:56.3 |
| · Load assets | 2.6 s | 11.30 GiB | 11.30 GiB | 2.6 s | 3.1 s |
| · Commit acceleration structure | 1.3 s | 11.66 GiB | 11.66 GiB | 1.4 s | 2.1 s |
| Render | **23.9 s** | 11.92 GiB | 11.92 GiB | 23.4 s | 3:20.7 |
| Write output | 0.039 s | | | 0.047 s | 0.036 s |
| **total** | **1:57.7** | | **11.93 GiB** | 3:20.3, 28.65 GiB | 6:22.4, 33.20 GiB |

The import is 80% of the run, and nearly all of that is openusd composition
during the traversal (see "Where import time goes" in
`openspec/specs/usd-scene-import/design.md`). Peak memory is reached during
the traversal, and the render adds only 0.3 GiB on top.

### Where the import went

Two openusd updates landed between the two profiles, the first with the
memory-mapped reads, and each was measured on its own:

| change | measured as | traversal | peak |
|---|---|---|---|
| openusd 0.7.0 → `main` at `933aa8f`, with memory-mapped USD reads | `bench_ab.sh`, 4 runs a side, 2026-10-10 | −15% (min), −17% (mean) | 28.5 → 15.5 GiB |
| · of which the memory mapping (`mmap` feature) | `bench_ab.sh`, 3 runs a side, 2026-10-10 | −3% | −3.5 GiB |
| openusd `main` at `933aa8f` → `7d11cdd` (0.8.0) | both binaries at once, same load, 2026-10-11 | **126.2 → 86.9 s (−31%)** | **15.6 → 11.9 GiB** |

The first two rows are recorded in
`openspec/changes/archive/2026-10-10-retire-openusd-workarounds/notes.md`. The third is the prototype
sharing below: two `crust check -l debug` imports, `51250e8` and `fc0b962`,
run concurrently so that load fell on both.

### What the scene holds

| | now | 0.5.0 |
|---|---|---|
| geometries | 13 302 | 13 302 |
| top-level BVH primitives | 1 991 360 (1 979 504 triangles, 11 819 instances, 37 analytic light shapes) | the same |
| native-instancing prototypes | **380** | 1 417 |
| primitives in memory | **21 438 434** (12 007 504 triangles, 9 418 288 cubic curve spans, 12 605 instances) | 30 597 852 (21 166 922 triangles) |
| allocated materials | **757**: 712 textured `UsdPreviewSurface`, 45 `Emissive` | 1 794: 1 749 and 45 |
| lights | 43: 23 sphere, 13 cylinder, 4 rect, 1 disk, 1 distant, 1 dome | the same |
| kernel memory | **2.71 GiB** (0.84 curve spans, 0.76 triangle packets, 0.53 BVH nodes, 0.27 triangle records, 0.14 leaves, 0.07 vertices, 0.06 normals) | 3.82 GiB |
| packet lanes filled | 82.6%; 242.3 bytes per triangle | 82.7%; 193.6 |

- **Prototypes are shared now.** Under openusd `main` at `933aa8f`, ALab's
  12 605 instances drew on 1 417 prototypes, each with its own copy of the
  meshes and materials under it. At `7d11cdd` they draw on 380 (distinct
  `/__Prototype_N` in an `-l debug` import log). Which of the 19 upstream
  commits between the two did it was not identified. The top level, the instance count
  and the lights are unchanged, so nothing left the frame. At 16 spp with
  `--indirect-clamp 0`, 92% of the pixels are bit-identical to `51250e8`. The
  rest differ by trimmed relMSE 6.2e-3, which fits the BVH ordering exact ties
  differently. The 1/√N check across sample counts was not run.
- **Bytes per triangle rose** only because it divides all of kernel memory,
  including the 0.84 GiB of curve spans, by fewer triangles.
- **The rest of the 11.93 GiB peak, about 8.5 GiB, is the composed USD stage
  and import state**, against 24.8 GiB at 0.5.0. A single-stage import keeps it
  allocated (`skip_stage_teardown`).
- **Four fewer lights than the first profile**, because of light links (below).

## Render profile

Thread time over the whole render: 30:13 across 72 threads, which is 95.2% of
72 × wall (26.4 s in the profiled run).

| section | glob. | thread time | calls | per call | 0.5.0 | first profile |
|---|---|---|---|---|---|---|
| **Texture** | **38.0%** | 11:29.1 | 585 188 572 | **1.19 µs** | 43.0%, 1.57 µs | 88.6%, 21.55 µs |
| Trace | 32.7% | 9:52.6 | 108 645 066 | 5.45 µs | 33.7%, 5.86 µs | 6.5%, 7.60 µs |
| Occlusion | 7.1% | 2:08.1 | 48 809 443 | 2.63 µs | 6.3%, 2.65 µs | 1.5%, 3.94 µs |
| MainLoop | 6.8% | 2:04.0 | 2 982 651 | | 3.3% | 0.6% |
| EvalBsdfs (local) | 5.8% | 1:44.3 | 108 630 680 | 0.96 µs local, 7.39 µs incl. textures | 5.4%, 0.95 / 8.49 µs | 1.0% |
| SurfaceLighting (local) | 5.5% | 1:40.2 | 108 630 680 | 0.92 µs local | 4.9%, 0.85 µs | 1.2% |
| Bounce | 2.9% | 52.6 s | 108 630 680 | 484 ns | 2.5%, 437 ns | 0.5%, 597 ns |
| TextureLoad | 0.5% | 9.4 s | 55 095 | 171 µs | 0.4%, 139 µs | 0.1% |
| GeneratePrimary | 0.5% | 8.8 s | 29 309 688 | 301 ns | 0.4%, 258 ns | 0.1% |
| Contributions | 0.2% | 3.4 s | 29 309 688 | 116 ns | 0.2%, 104 ns | 0.0% |

By category: **Shading 43.8%, Raytrace 39.8%, Integrator 15.9%**, IO 0.5%.
0.5.0 measured 48.4 / 39.9 / 11.3, the first profile 89.5 / 8.0 / 2.4.
Guerilla's documentation calls a roughly even split between Raytrace, Shading
and Integrator the healthy shape. The frame is near it, with shading still the
largest share.

- **The frame does more work than at 0.5.0.** 167.5 M ray queries against
  140.9 M, and 108.6 M shading points against 99.7 M, at 7.0 Mray/s. The
  increase was already there at `3c8b861` ("Thread scaling"). It was not
  traced to a commit.
- **The per-call costs of the light integrator sections rose 10–15%**
  (Bounce, GeneratePrimary, Contributions, SurfaceLighting), and `MainLoop`'s
  own share doubled, on 63% more calls. Each side is one profiled run, which
  cannot resolve 10%. Count instructions before reading anything into it.

The execution tree is the same shape as before: `MainLoop → EvalBsdfs →
Texture`. Of EvalBsdfs' 13:23 total, 11:39 is inside `Texture`.
`TextureLoad`, which pages a tile in from disk, is only 9 s of that. Shading a
point without its textures costs under 1 µs. The texture path is still what a
shading point costs.

### Texture lookups

| | now | 0.5.0 | first profile |
|---|---|---|---|
| texture `eval`s per shading point | 5.39 | 4.82 | 4.8 |
| texel lookups per `eval` | 6.59 | 6.59 | 6.5 |
| lookups | 3.86 G | 3.17 G | 3.28 G |
| thread microcache hits | **94.7%** | 85.3% | 74.9% |
| shard-cache hits | 5.3% | 14.7% | 25.1% |
| loaded | 665.5 MiB in 55 106 tiles, nothing evicted, 1 GiB budget | 597.8 MiB in 51 617 tiles | 503 MiB in 45 968 tiles |
| concurrent double fills | 765 | 641 | 353 |

`grow-texture-microcache` landed after 0.5.0, and its gain shows here: 5% of
lookups take a shard mutex, against 15%. That is why a lookup costs 1.19 µs
against 1.57. The textures would total 44.95 GiB with every level resident.
Streaming holds the frame under 0.7 GiB.

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
- **NEE wastes less, but most shadow rays still miss.** Of 108.6 M light
  samples, 44.9% were worth a shadow ray, and 83.0% of the 48.8 M shadow rays
  cast were occluded (81.7% at 0.5.0), against 96.0% in the first profile. So
  about 7.6% of light samples deliver light, against 1.7%. This comparison has three causes, and they are
  not separated here: the louvered windows no longer block the sun lights, the
  screen lights are gone, and the adaptive sampler has changed. Power selection
  is still the default; `--light-selection learned` is the remedy measured
  under "Outcome: NEE's misses".
- **Paths end by absorption or roulette, almost never by escaping.** Absorbed:
  45.2%. Roulette: 54.7% (79.0% of roulette tests kill). Escaped: 0.05%. Depth
  cap: 927 paths. The mean path length is 3.71 vertices (3.41 at 0.5.0).
- **Adaptive sampling barely engages.** 1.1% of pixels stopped early (0.7%
  more were held back by a neighbour), and the average is 127.2 of 128 spp.

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

## Excluded glass (2026-10-13)

Change `excluded-glass-transmits-no-linked-light`. Speckle in the bench close-up's
shadows led to this. `crust light-paths` on one of them (the oscilloscope case under
the electronics) showed pairs from the same camera sample: `C<RG>L` 243 from
`lgt_sun_distant` (the receiver's link twin, which sees the sun with the windows
absent), and `C<RG><TS><TS>L` 231 (the same bounce refracted through the window pane
and reaching the sun again). The windows are `UsdPreviewSurface` with `opacity ≈ 0`,
so thick OpenPBR glass with a *continuous* lobe. Each face ran its own NEE and twin
toward the sun. The sun was counted once straight and once refracted.

### Method

`groups.usda` sublayers `alab.usda` and sets:
- `crust:light:lpeTag` on the four exterior lights (`sd`, `sa1`, `sa2`, `dome`);
- `varianceThreshold = 0`, `indirectClamp = 0`, `samplesPerPixel = 64` and
  `lightSamples = 4` on `/Render/settings`;
- one product of `C<RG><L.'t'>`, `C<RD><L.'t'>` and `C.*<T.><L.'t'>` (the last
  event a transmission) per sun, plus `C.*<L.'dome'>`.

Seed 2 is `groups_seed2.usda`, `subLayers = [@./groups.usda@ (offset = 1)]`, rendered
at `-f 1005` (scene at 1004, seed 1005). The statistics are those of "Shadow-linked
lights and their noise": the mean, the two-seed variance `(a − b)²/4` summed over the
frame, the share of the 0.1% noisiest pixels, and the noisiest pixel's σ. The
reference deactivates both `structure_window_louvered02_000N` prims on top of the
same layers. The overlays are scratch files and not committed.

```bash
crust render -i groups.usda       -f 1004 --light-selection learned -o b1.exr
crust render -i groups_seed2.usda -f 1005 --light-selection learned -o b2.exr
```

### Before and after

Beauty and per-sun terms: mean luminance, then the two-seed variance (top 0.1%, worst σ).
"Before" is `bffa7a3`.

| layer | before | after | windows deactivated |
|---|---|---|---|
| beauty | **0.6253**, 24.1 M (56.4%, 899) | **0.2624**, 2.08 M (98.4%, 385) | **0.2508**, 0.42 M (92.7%, 244) |
| `lgt_sun_area_01`, last event a transmission | 0.3351, 21.9 M | 0.0092, 1.14 M | 0.0023, 0.15 M |
| `lgt_sun_distant`, last event a transmission | 0.0647, 91.6 k | 0.0289, 31.4 k | 0.0280, 25.8 k |
| `lgt_sun_area_02`, last event a transmission | 0.0013 | 0.0001 | 0.0001 |
| `lgt_sun_area_01` direct diffuse / glossy | 0.1097 / 0.0154 | identical | 0.1097 / 0.0154 |
| `lgt_sun_distant` direct diffuse / glossy | 0.0268 / 0.0044 | identical | 0.0268 / 0.0044 |
| `lgt_env_dome`, all paths | 0.0016 | identical | 0.0025 |

- **The frame was 2.5× too bright, and almost all of its noise was the double
  count.** The beauty falls 58%, to within 4.6% of the frame with no windows at
  all. The no-window frame is what the shadow link means for the suns, though it also
  drops the glass's own reflections and lets the dome in. Variance falls 91%.
- **Direct light is untouched**: the per-sun direct terms and the dome are
  bit-identical. Only paths that crossed the glass after a receiver changed.
- **What is left** of `lgt_sun_area_01`'s transmission term (0.009 against the
  no-window 0.002) comes from the rule's documented gaps: chains that start at the
  camera, and chains that mix the windows with glass the sun does see.
- **Render time was not measured with `bench_ab.sh`.** These were sequential runs on a
  shared machine: before took 66.2 and 64.9 s, and the after runs took between 63.6 and
  71.4 s. A dropped light sample casts *fewer* shadow rays, so no slowdown is expected.
  Unlinked scenes run the same instructions (callgrind, see the lighting design record).

## What is left, in order of what it would buy

1. **The import.** At 1:30 of a 1:58 frame, composition is still where a
   single frame's time goes, even after the two openusd updates halved it. The
   render could halve again and the frame would be 10% faster.
2. **The end of the pass.** Texture contention is gone
   (`grow-texture-microcache`: a texture call's 8 → 72 slowdown is now the
   machine's 1.61×). What is left of the 72-thread gap is threads idling while
   the last tiles finish: 5.1% of capacity. Removing it would take 72 threads
   from 5.51× to about 5.7× over 8, not to 9×, which this VM cannot reach
   ("Thread scaling").
3. **Traversal.** Trace is now a third of render thread time, at 5.5 µs per
   closest-hit query on this VM.
4. **Light selection.** 83% of shadow rays are still occluded under power
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
