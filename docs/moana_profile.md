# Profiling the Moana island: where a ray spends 6 ms

The first `--profile` run of the full Moana island (`usd/island.usda`, `shotCam`).
The main finding is that **99.9% of render thread time is ray traversal, at
5.97 ms per closest-hit query**, about 800× ALab's 7.6 µs. Shading, textures and
the integrator do not register. The cause is not the kernel. It is how the
importer lays out one element, **isDunesB**. Deactivating that element made the
same render about 62× faster in single runs, 323.9 s → 5.2 s. Grouping its
prototypes instead (the fix, "Outcome" below) makes it **262× faster** in an
interleaved `bench_ab.sh` comparison, 312.6 s → 1.195 s (min of 2).

Measured 2026-09-27 at `abae004`, on the same machine as `docs/alab_profile.md`
(72 vCPUs, 61 GiB).

## Setup

```bash
ISLAND=~/Workspace/samples/island/usd/island.usda
cargo run --release -- -i $ISLAND --camera /island/cam/shotCam -s 4 --profile
```

Every measurement on this page is of the island's **unrefined cages**. The island
authors `subdivisionScheme = "catmullClark"` in 189 of its 213 mesh-bearing files, and
since `usd-driven-subdivision` they stay unrefined at the default level 0, only shaded
with smooth cage normals (so the images differ slightly; the triangle counts do not).
`--subdiv-level 1` would cost 4× their triangles on a scene already holding 60.9 M
triangles in 39 GiB of kernel memory; that has not been measured here.

- **Settings:** the stage authors no `crust:` settings, so the importer defaults
  apply (640×360, depth 32, power MIS and light selection, triangle filter,
  indirect clamp 10), except that `-s 4` replaces 128 spp. At 128 spp the render
  alone would take about three hours.
- **Ptex:** preloaded at the default 32×32 cap: 3 618 textures, 2 564 203 faces,
  6.02 GiB.

## The run

| phase | wall | RSS at end | peak |
|---|---|---|---|
| Parse USD stage | 5:44.8 | 50.70 GiB | 51.51 GiB |
| · Traverse prims | 3:33.8 | 46.12 GiB | 46.12 GiB |
| · Load assets | 1:41.5 | 46.12 GiB | 46.12 GiB |
| · Commit acceleration structure | 29.4 s | 50.70 GiB | 51.51 GiB |
| Render (4 spp) | 5:23.9 | 50.79 GiB | 51.51 GiB |
| **total** | **11:08.8** | | **51.51 GiB** |

### What the scene holds

| | |
|---|---|
| geometries | 3 151 850 |
| top-level BVH primitives | 21 904 388 (18 765 854 triangles, 3 138 534 instances) |
| primitives in memory | 120 152 660 (60.9 M triangles, 19.3 M cubic curve spans, 39.9 M instances) |
| materials | 17 265 (17 244 OpenPBR, 21 Emissive) |
| lights | 23 (21 rect, 2 dome) |
| kernel memory | 39.31 GiB (14.32 primitive nodes, 10.65 boxed prims, 7.01 BVH nodes, 5.71 packets) |

## Render profile

Thread time is 324:01, which is 83.4% of 72 × wall. The missing 16.6% is load
imbalance: a snapshot near the end found 34 of 72 threads idle, waiting on the
last expensive tiles.

| section | glob. | thread time | calls | per call |
|---|---|---|---|---|
| **Trace** | **82.4%** | 267:02.9 | 2 682 858 | **5.97 ms** |
| **Occlusion** | **17.5%** | 56:38.3 | 1 164 247 | **2.92 ms** |
| SurfaceLighting (local) | 0.0% | 7.3 s | 2 166 556 | |
| MainLoop | 0.0% | 7.2 s | 230 400 | |
| EvalBsdfs + Texture | 0.0% | 2.7 s | 2 166 556 | 1.22 µs |

By category, Raytrace is 99.9%. The ray statistics are unremarkable. 56.0% of
paths escape to the sky, the mean path length is 2.35, and 80.8% of shadow rays
are occluded. There are just not many rays: 3.85 M queries in 5:24.

## Diagnosis: 64 724 instances that all cover the same dunes

### The top level does not cull

A `--features traversal-stats` build counts traversal work per camera ray (1 spp,
the same frame):

| level | queries | nodes | leaves | triangle packets | scalar tests |
|---|---|---|---|---|---|
| top-level | 2.91 | 10 572 | 22 823 | 0.77 | 36 065 |
| instanced | 36 504 | 47 904 | 253 | 15.9 | 464 |

Per closest-hit query, the root BVH visits ~3 600 nodes and ~7 800 leaves, and
descends into ~12 500 instances. It tests almost no triangles directly. Inside
an instance the average descent visits 1.3 nodes and finds a leaf 0.7% of the
time: nearly every instance a ray enters, it leaves again from the root. So the
rays are crossing a large pile of **instance boxes that overlap in space but
are mostly empty**.

The debug line `top-level extents` did not catch this. It reports a mean
primitive diagonal of 0.0001 of the scene, which is dominated by 18.8 M small
triangles. A mean hides a tail of 65 k huge boxes.

### Which instances

The same build now also counts descents per top-level `geom_id`, and the
importer's DEBUG lines print each instancer's `geom ids a..b` range. Together
they attribute the work. Unlike the table above, these descents include shadow
rays (44 274 per camera ray, against 36 504 closest-hit ones), because an
occluded ray pays for the same boxes. Of 3.1 M top-level instances, 239 230 were
entered at all. **99% of all descents go to 65 223 of them.** Nearly all of those 65 k are
isDunesB, whose prototype builds **64 866 parts**:

```
Expanded nested PointInstancer at /__Prototype_0/geometry/xgTreeFill/instancer (679 instances -> 64724 part(s))
Prototype /__Prototype_0 (epoch 15, nesting depth 0): built 64866 part(s) in 60.14s
```

`xgTreeFill` scatters 679 bay cedars over the dunes. Each bay cedar variant
(`isBayCedarA1_base`, `_bonsaiA/B/C`) is a prototype of **16 181 parts**: the
trunk, one part per branch mesh (`hires_part/isBayCedarA_base_hBranch_*_geo`,
about 15 000 of them), and the leaf instancer. The whole tree binds three
materials.

`nested_instancer_parts` groups placements **per (prototype, part)**, so that
each output part keeps one material ("Nesting" in
`openspec/specs/usd-scene-import/design.md`). With K = 16 181 that produces
64 724 parts. Each part is a committed scene holding the ≤ 679
placements of *one branch*, scattered across the whole dune field, and so each
part's box *is* the dune field. The root BVH gets 64 724 near-identical boxes
that no split can separate. A ray crossing the dunes enters most of them and
finds one branch in a few.

The same shape explains the rest of the hot list. The ten highest per-instance
counts (1.7–4 descents per camera ray, i.e. every query) are isMountainA/B's
nested-instancer parts, the ocean and the coastline. Each spans its whole
element, but there are only ~2 400 of them.

### Confirmed by knocking it out

The same render with only `over "isDunesB" (active = false)` changed:

| | full island | without isDunesB | |
|---|---|---|---|
| Render (4 spp) | 323.9 s | **5.25 s** | **−98.4% (62×)** |
| Trace per call | 5.97 ms | 96.0 µs | 62× |
| Occlusion per call | 2.92 ms | 36.7 µs | 80× |
| mean ray query | 84.2 µs | 1.37 µs | |
| top-level instances | 3 138 534 | 3 073 668 | −64 866 |
| instances in memory | 39.9 M | 28.8 M | **−11.1 M** |
| kernel memory | 39.31 GiB | 32.84 GiB | −6.47 GiB |
| Traverse prims | 3:33.8 | 2:22.3 | −71 s |

isDunesB is in frame: the images differ on 18.9% of pixels. So deactivating it
is a diagnosis, not a fix.

The element costs memory as well as time, for the same reason. Grouping per
part stores every branch placement as its own inner instance: 679 trees ×
16 181 parts is 11 M boxed `InstancePrim`s, where 679 per variant would do.

## What to do about it

1. **Group a nested instancer's output per prototype, not per part.** Commit
   each prototype's parts into *one* scene and place that scene M times. Every
   top-level box is then a tree, and the ~679 trees get a real BVH. This needs
   one thing from the kernel that it does not do yet: a hit has to say *which
   part* it landed in. Today a hit reports only the top-level instance's
   `geom_id` plus the innermost `prim_id`, which is exactly why parts are split
   at the top level (`World` maps materials, Ptex face tables and UV tables by
   top-level `geom_id`). The Embree-shaped answer is Embree's own: report the
   instance id stack (`instID[]`) together with the inner `geomID`, and let
   `World` resolve `(top-level geom_id, part index)` through a per-prototype
   table. Expected effect is roughly the knockout column above with isDunesB
   still rendered: ~64 k top-level instances become a handful, and ~11 M inner
   instances become ~700, one per tree.
2. **The same fix applies to native instances of many-part prototypes.**
   isBayCedarA1 places 9 bay cedars natively as 136 133 top-level instances, and
   isDunesB's own 16 k-part tree variants go through the same path. These boxes
   are one tree each rather than the whole field, so they cull far better and
   do not show up in the hot list. They still spend 16 k top-level primitives
   per tree, and memory with them.
3. **Cheaper stop-gap, if (1) is too big a step:** group by `(material, mask)`
   instead of by part. The bay cedar would become three parts instead of 16 181.
   The catch is that parts carry per-part Ptex face and UV tables, indexed by an
   inner `prim_id` that is only unambiguous within one part. Merging is only
   safe for parts whose material samples neither.
4. **Re-profile** after (1). With traversal back to microseconds, the other
   phases matter again: the import is 5:45, of which Ptex preload is ~1:40, and
   isDunesB's own prototype build took 60 s of the traversal.

## Outcome: grouped per prototype

Fixes 1 and 2 landed together. The kernel gained `InstanceHitId` in place of
an id stack. It is one id per hit, composed on the way out: a member instance
reports `As(k)`, and a placement of a group reports `Offset(base)`, adding its
base to whatever the inner level reported. So `RayHit` and `World`'s lookup
stay a single index, and `InstancePrim` does not grow: the offset fits the
padding its 16-byte-aligned transforms leave, pinned at 240 bytes.

In the importer, a prototype of several parts becomes a *group*:
- **The scene:** one committed scene of its parts, each labelled with its slot.
- **Placing it:** one instance, taking one `geom_id` per slot, so materials,
  Ptex and UV tables stay per part.
- **When:** a nested instancer always places its prototypes' groups. A
  top-level placement groups from 64 parts, because grouping costs every ray
  that enters it one more transform. The threshold is a round number, not a
  measured optimum.

The render time, from `scripts/bench_ab.sh` alternating the two binaries (the
parent `origin/main` and this change, built from one `Cargo.lock`), 2 reps
each, with isDunesB still in frame:

| Render (4 spp) | min | mean |
|---|---|---|
| before | 312.6 s | 334.1 s |
| after | **1.195 s** | **1.204 s** |
| Δ | **−99.6% (262×)** | −99.6% (277×) |

The rest comes from one `--profile` run of each side, the before side being the
first table above. The per-call figures are thread time, and their ratio is
far outside run-to-run noise. The memory figures are deterministic.

| | before | after | |
|---|---|---|---|
| Trace per call | 5.97 ms | 22.3 µs | 268× |
| Occlusion per call | 2.92 ms | 6.85 µs | 426× |
| mean ray query | 84.2 µs | 0.31 µs | |
| top-level instances | 3 138 534 | 2 912 239 | |
| instances in memory | 39.9 M | 27.6 M | |
| kernel memory | 39.31 GiB | 33.92 GiB | −5.39 GiB |
| peak RSS | 51.51 GiB | 46.17 GiB | −5.34 GiB |
| Traverse prims | 3:33.8 | 3:02.7 | |

That beats the knockout (5.25 s, a single sequential run, so only indicative). The trees in isBayCedarA1 and the mountains'
scatters were costing traversal too, just less visibly. The render is now 0.4%
of the run, so the island is back to being an import benchmark: 5:05 of parse,
of which Ptex preload is 1:37.

The images are not bit-identical. A grouped part's transform is applied as two
matrices, the placement and then the part's, where it used to be one product,
so a few paths diverge by an ulp. The difference is noise, not a lost material
or part:
- against the pre-fix render at the same seed, 10.1% of pixels differ, with
  relMSE 4.8e-3 (trimmed: 6.7e-4);
- two fixed-build renders at *different* seeds differ by relMSE 1.40
  (trimmed: 0.81), so the change sits three orders of magnitude below the
  sampling noise;
- on the checked-in samples at 16 spp, against the pre-fix binary built from
  the same `Cargo.lock`, 23 of 25 are bit-identical. The two that move are the
  two whose layout changed: `nested_instancing` (510 pixels, max 8.1e-4) and
  `Kitchen_set_instanced` (9 pixels, relMSE 1.3e-6);
- the new tests in `tests/usd_scene.rs` pin that every part of a grouped tree
  resolves to the material it binds, both natively instanced and inside a
  nested scatter.

## The sky rig: backdrop and camera visibility

Measured 2026-09-29 for `light-camera-visibility-and-link-exclusion`. `island.usda`
lights the island with two `DomeLight`s:
- `sky_dome_env_llc` (`islandsun.exr`) is the HDRI.
- `sky_dome_cam_llc` (`islandsunVIS.png`) is a camera-visible backdrop with
  `collection:lightLink:excludes = </island>`.

Before the change, crust read neither the link nor any camera visibility, so both
domes lit the island and the camera saw their sum. Now the backdrop is imported as
a camera-only backdrop in front of the HDRI. `islandPrman.usda` additionally hides
the HDRI from the camera with `primvars:ri:attributes:visibility:camera = 0`.

Setup for all three renders: `renders/moana_island/island_water.usda` (the ocean
reshaded as clear water) at 1024×512, `--camera /island/cam/shotCam -s 16
--indirect-clamp 0`. The islandPrman leg points its two dome maps back at the
`.exr` / `.png`, because crust cannot decode RenderMan `.tex`. Colour is the mean
of the tone-mapped PNG, sRGB-decoded, after a 1200 px downscale: the island is the
left 65% of the frame, the sky the top-right patch. The reference is the published
RenderMan JPEG, measured the same way.

| render | island R/G | island B/G | sky, linear RGB | sky clipped |
|---|---|---|---|---|
| RenderMan reference | 0.920 | 0.337 | (0.395, 0.537, 0.729) | 0% |
| before (both domes light, camera sees the sum) | 0.450 | 0.490 | (0.922, 0.989, 1.000) | 100% |
| after, `island.usda` | **0.542** | **0.432** | (0.530, 0.582, 0.603) | 0% |
| after, over `islandPrman.usda` | 0.474 | 0.479 | (0.530, 0.582, 0.603) | 0% |

- **The sky is fixed.** The camera sees the backdrop alone in both layers
  (identical sky values), with clouds instead of clipped white.
- **The island moves toward the reference**, by about a fifth of the red gap
  and a third of the blue one. It is still much cooler than RenderMan.
- **The islandPrman leg is cooler than `island.usda`**, because that layer
  raises the HDRI's exposure from 0.3 to 1: twice the blue sky.

What is left is not the sky rig. Candidates, none measured yet:
- The three `distantPalm_key` lights link to `isPalmRig` only (`includes`).
  They are warned about and light everything.
- The cyan `palm_bounce` rect lights. `islandPrman.usda` drops them to exposure
  0.05; `island.usda` keeps them at 1.
- The per-channel clip tone map, against the reference's grade.

**Found 2026-10-02: the sun was shadowing itself.** The sun is `sun_quad_llc`, a
20000 × 20000 `RectLight` about 2.9·10⁵ units out, and it was imported correctly.
Its shadow rays, though, stopped at `distance − 0.001`. At that distance an `f32`
ulp is about 0.03, so the bound rounded back to `distance`. The ray then reached
the light's own surface, which is in the shadow mask, and was blocked by it most of
the time. With only the island's light rig over a grey ground, the sun's direct
light came out speckled and at about 30% of its value. That explains much of the
"cooler than RenderMan" gap: the warm key light and its hard shadows were mostly
missing.
- **The fix:** `shadow_t_max` in `tracer/path.rs` stops every shadow ray short by
  the larger of 0.001 and 1e-5 × `distance`. Nearer than 100 units the bound is
  unchanged, and every checked-in sample and both Kitchen_set variants stay
  bit-identical.
- **Pinned by:** `a_far_rect_light_does_not_shadow_itself` in
  `crust-core/tests/usd_inline.rs`. Before the fix it measured 0.000201 against
  0.000678 for the scaled-down twin.
- **Stale numbers:** the island colours in the table above predate the fix and need
  re-measuring.

Same wall time as before (about 5:10 to 5:20) and the same peak RSS (51.5 GiB).

## Memory: instances and curve spans inline

Measured 2026-10-01 for `slim-instance-and-curve-storage`, against its parent
`a50b1a8`, which already stores each triangle once (`compact-triangle-storage`).
- **Setup:** `shotCam`, `-s 1 --stats`, Ptex streamed
  (`CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIPSPACE=file`, so that Ptex is not the
  variable), under an RSS guard that kills the process at 56 GiB on this 61 GiB machine.
- **What it targets:** what that change left as its Deferred item 3. On the island, an
  instance was a 64-byte `PrimNode` slot plus a 240-byte box, and a cubic curve span a
  slot plus a 96-byte box. They are now 96 B each, inline, in arrays of their own.

| level 0 | before | after | |
|---|---|---|---|
| kernel memory | 20.02 GiB | **13.52 GiB** | **−6.50 GiB** |
| · primitive nodes | 2.80 GiB | 0 | 47 M slots gone (the island has no analytic primitives) |
| · boxed primitives → instances + cubic spans | 7.90 GiB | 2.47 + 1.73 GiB | 27.6 M × 96 B, 19.3 M × 96 B |
| · everything triangle-side | 9.32 GiB | 9.32 GiB | unchanged |
| peak RSS | 30.28 GiB | **23.78 GiB** | −6.50 GiB |

The image is bit-identical: the frame diffs to 0 pixels against the parent, as do
ALab's, every checked-in sample, and both Kitchen_set variants.

Speed does not pay for the memory.
- **Island:** `bench_ab.sh`, alternating the two binaries at `-s 4` (2 reps each),
  timed `Render` at 1.064 → 0.932 s minimum (−12.4%) and 1.242 → 0.948 s mean
  (−23.7%). The mean is noisy, because one base run was slow.
- **Default scenes:** within ±1%.
- **callgrind:** −0.7% instructions on cornellbox and −0.6% on nested_instancing.
  `Scene::occluded` drops 4.8% and 3.4%, because a shadow ray entering an instance no
  longer chases a box.

**Level 1 now fits.** `--subdiv-level 1`, with Ptex streamed:

| level 1 | before | after |
|---|---|---|
| outcome | killed at the 56 GiB guard after 8:05, in `Commit` | **completes in 8:17** |
| kernel memory | — | 36.59 GiB (143 B per triangle, 359 M packet lanes) |
| peak RSS | > 56 GiB | **51.14 GiB** (`Traverse prims` 41.4 GiB, `Commit` peak) |

The headroom is real but thin: 51 GiB of 61. The commit's transient, 41.4 → 51.1 GiB,
is now the largest single step. `compact-triangle-storage`'s Deferred item 4, a builder
that never materialises the binary tree, is the lever for it.

ALab (level 0, same flags) went from 4.38 to 3.82 GiB of kernel memory and from 25.74
to 25.00 GiB peak. Its peak is dominated by what the kernel does not own.

## Benchmark: level 0 and level 1, both packet layouts

Measured 2026-10-01 with the code at `cd11376`. The only
kernel commit since the previous section is `24f9458`, which computes the extents
diagnostic on request instead of at every commit. The machine was idle (load average
under 0.5 for level 0, 3–5 on 72 cores for level 1).
- **Setup:** `shotCam`, Ptex streamed (`CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIPSPACE=file`),
  `--stats`. Every A/B alternates its two sides run by run and keeps each run's whole
  report, so time, phases and memory come from the same runs (`bench_ab.sh` keeps one
  phase). Level 1 runs under the 56 GiB RSS guard.
- **Images:** every pair below diffs to 0 pixels: `d06daaa` against `cd11376`, and
  gathered against indexed in all three level-1 pairs.

**Level 0, `d06daaa` against `cd11376`,** `-s 4`, two runs each:

| phase | `d06daaa` min / mean | `cd11376` min / mean | Δ min / mean |
|---|---|---|---|
| Render (4 spp) | 0.981 / 0.993 s | 0.961 / 0.980 s | −2.0% / −1.3% |
| Parse USD stage | 3:26.3 / 3:28.1 | 3:20.5 / 3:22.3 | −2.8% / −2.8% |
| · Traverse prims | 2:42.6 / 2:44.2 | 2:36.7 / 2:37.9 | −3.6% / −3.8% |
| · Load assets (Ptex) | 25.3 / 25.6 s | 25.4 / 25.4 s | flat |
| · Commit | 17.9 / 18.0 s | 18.2 / 18.7 s | +1.8% / +3.8% |

Kernel memory is 13.52 GiB on both (60.9 M triangles), and peak RSS is 23.5–23.9 GiB on
both, moving ~0.4 GiB between runs of one binary. The render is within noise. The
`Traverse prims` gain fits `24f9458`, since prototype scenes are committed during the
traverse and no longer pay for the diagnostic, but `Commit` moved the other way by as
much, and two runs cannot resolve 3%. It would take callgrind to prove it.

**Level 1, memory** (`--subdiv-level 1 -s 1`, one run per layout):

| | gathered (default) | `CRUST_TRI_PACKETS=indexed` | Δ |
|---|---|---|---|
| triangles in memory | 274 707 194 | same | 4.5× level 0 |
| kernel memory | 36.59 GiB | **28.22 GiB** | −8.37 GiB (−23%) |
| · triangle packets | 16.07 GiB | 7.70 GiB | the only row that moves |
| · records / BVH nodes / vertices + normals | 6.14 / 5.18 / 3.18 GiB | same | |
| · instances / cubic spans / leaves | 2.47 / 1.73 / 1.34 GiB | same | |
| bytes per triangle | 143.0 | 110.3 | |
| lanes filled | 80.4% (289 M of 359 M) | same | |
| `Traverse prims` RSS | 41.27 GiB | 34.85 GiB | −6.42 GiB |
| peak RSS | 51.07 GiB | **42.47 GiB** | −8.60 GiB |
| headroom on 61 GiB | ~10 GiB | ~18.5 GiB | |

The gathered row reproduces the previous section's level-1 record (36.59 GiB, 51.14
GiB peak, 8:17 against 8:19.5 now). Three quarters of the indexed saving appears during
`Traverse prims`, not at the final commit: the island's prototypes are committed while
the stage is read, and their packets shrink too. The commit's transient, 41.3 → 51.1
GiB gathered, is still the largest single step.

**Level 1, speed** (`--subdiv-level 1 -s 16`, 15.08 M ray queries, gathered and indexed
alternating, two runs each):

| | gathered min / mean | indexed min / mean | Δ min / mean |
|---|---|---|---|
| **Render** | 3.759 / 3.869 s | 4.074 / 4.228 s | **+8.4% / +9.3%** |
| throughput | 4.01 / 3.90 Mray/s | 3.70 / 3.57 Mray/s | |
| · Traverse prims | 6:29.0 / 6:31.0 | 6:14.6 / 6:17.8 | −3.7% / −3.4% |
| · Commit | 63.0 / 63.2 s | 60.3 / 61.5 s | −4.3% / −2.7% |
| **total run** | 8:05.0 / 8:05.1 | 7:46.6 / 7:50.6 | **−3.8% / −3.0%** |
| peak RSS | 51.20 / 51.25 GiB | 42.35 / 42.41 GiB | −8.8 GiB |

- **Render:** 8–9% slower indexed, consistent across both reps (the slowest gathered
  run beat the fastest indexed one). That falls between the in-cache 1–8% and the
  out-of-cache 30% of `ray_throughput` (`openspec/specs/intersection-kernel/design.md`).
- **Import:** 3–4% faster indexed in both reps and both phases. Writing 8.4 GiB less
  packet data is the plausible cause. It is near this machine's noise at two reps, so
  it is probable, not proven.
- **Whole run:** indexed costs ~0.022 s of render per sample per pixel and saves ~14 s of
  import, so on these numbers it is faster end to end below roughly 600 spp. At the
  island's default 128 spp that is ~3 s of render for ~14 s of import and 8.8 GiB.

So on a scene this size, where the import is the run, `indexed` does not lose overall.
`auto` is still `gathered`: the "no threshold" decision compared render throughput,
which indexed loses at every size, and one scene at two reps is not a threshold. A
whole-run criterion would need its own proposal.

## Adaptive subdivision

Measured 2026-10-01 for `adaptive-subdivision`, `shotCam`, Ptex streamed, `-s 4`, under
the 56 GiB RSS guard, against the uniform runs above.

| | triangles | kernel | peak RSS | Traverse prims |
|---|---|---|---|---|
| uniform level 0 | 60.9 M | 13.52 GiB | 23.79 GiB | 2:36.7 |
| `--subdiv-edge-length 2` (ceiling 3), `4` (ceiling 3), `2 --subdiv-level 2` | | | killed > 56 GiB | |
| `--subdiv-edge-length 2 --subdiv-level 1` | 187.2 M | 27.75 GiB | **38.47 GiB** | 6:01.4 |
| uniform level 1 | 274.7 M | 36.59 GiB | 51.07 GiB | 6:41.2 |

- **Capped at 1** it holds 32% fewer triangles than uniform level 1 and peaks 12.6 GiB
  lower. Only 5 285 of 188 959 mesh reads are refined (`subdivision levels  L0 183 674 ·
  L1 5 285`), with 544 prototype versions (509 rate-dependent), yet those meshes hold most
  of the extra triangles.
- **At ceiling 2 or 3 it does not fit.** A level is per mesh, chosen at the mesh's
  nearest point, and the island's terrain and beach meshes are kilometres wide while
  passing close to `shotCam`: each is refined in full to the ceiling. Per-face refinement
  would fix it and is out of scope (`openspec/specs/usd-scene-import/design.md`, "Known
  gaps: adaptive subdivision").
- **The image** at 640×360, 4 spp, is indistinguishable from level 0 by eye. relmse 0.18
  against level 0 is sampling divergence (two seeds of one binary differ by 1.40).

ALab, by contrast, is what the design was for: at `--subdiv-edge-length 2` and the
default ceiling of 3 it holds 74.2 M triangles against uniform level 1's 81.8 M, 9.28
against 10.47 GiB of kernel and peaks at 35.67 against 36.86 GiB, while 240 meshes near
the camera get level 3.

### Per-face tessellation

Measured 2026-10-02 for `per-face-adaptive-tessellation` on opensubdiv-rs 0.5.0, same
setup. Each cage edge of *unshared* geometry is cut by its own size on screen; prototypes
placed more than once stay at the uniform level; faces whose edges all need one segment,
and faces out of view, keep their cage; patches are built only for the refined faces.

| `--subdiv-edge-length 2` | triangles | kernel | peak RSS | Traverse prims |
|---|---|---|---|---|
| per-mesh, ceiling 3 or 2 | | | killed > 56 GiB | |
| per-mesh, ceiling 1 | 187.2 M | 27.75 GiB | 38.47 GiB | 6:01 |
| **per-face, ceiling 3 (default)** | **63.6 M** | **13.83 GiB** | **24.70 GiB** | **3:25** |

- **Within 1 GiB of uniform level 0's peak**, with 32 589 meshes cut per face (edges at
  rate 1: 48.7 M · 2: 89 k · 3–4: 59 k · 5–8: 63 k) and 86 682 shared vegetation meshes
  at level 0.
- **What it took**, each found by the run before: shared prototypes at the uniform level
  (the terrain elements are `instanceable` but placed once, so they count as unshared);
  isolation depth 1 (the ocean is an all-triangle cage, irregular everywhere: 19.9 GiB
  at depth 3); a frustum term (`ocean_geo1` 31.2 M → 4.1 M triangles); compact patch
  storage and face-varying patches (opensubdiv-rs 0.4.0); and patches for refined faces
  only (0.5.0) — `ocean_geo`, about 14.9 M triangles, would otherwise need about 45 M
  Gregory patches.
- **Speed**, both at `--subdiv-edge-length 2 --subdiv-level 1 -s 16`, alternating, two
  runs each: per-face Render 3.631 / 3.736 s (min / mean) against per-mesh 3.752 /
  3.789 s, within noise; Traverse 4:27 against 5:20; peak 30.9 against 39.0 GiB.
- **Triangle shapes** of the 3.02 M refined triangles (`4√3·area / Σ edge²`): stitched
  rings 19% ≥ 0.9, 69% ≥ 0.5, 10.3% ≥ 0.1, 1.2% ≥ 0.01, 6 below; the interior grids
  21%, 71%, 7.3%, 0.8%, none below. Stitching costs little shape.
- **ALab** at the same settings: 22.8 M triangles, 3.97 GiB of kernel, 29.46 GiB peak,
  Traverse 2:58 — under uniform level 1's 81.8 M / 10.47 / 36.86, with all 5 242 unshared
  meshes cut per face (their face-varying charts evaluated on the patches).

## Tooling added for this

- `traversal-stats` now also counts descents per top-level instance
  (`crust_rt::traversal_stats::note_descent` / `top_level_descents`), shadow
  rays included. With `--stats`, the renderer prints how concentrated they are (the 50/90/99% lines)
  and the 40 hottest instances with bounds, inner primitive count and how many
  top-level instances share the inner scene (`Scene::describe_instances`).
- The importer's `Instance … uses prototype` and `Imported PointInstancer`
  DEBUG lines carry `geom ids a..b`, so a hot `geom_id` maps back to a prim.
- One caution about the feature: its per-level node counters are still global
  atomics. On this scene a 1 spp render takes 27 min against ~1.4 min without
  the feature, because every one of ~60 k node visits per ray contends one line.
  The counts are right, but budget for the wait. The descent counts are
  deliberately *not* collected in an ordinary build. Each one is a hash-map
  update on the hottest path in the kernel, and on this scene there were
  tens of thousands per ray. The always-on `--stats` counters are integer
  bumps per ray or per shading point, and cost nothing measurable.

## Reproducing

```bash
ISLAND=~/Workspace/samples/island/usd/island.usda
# profile
cargo run --release -- -i $ISLAND --camera /island/cam/shotCam -s 4 --profile
# traversal counts + per-instance attribution (slow; see above)
cargo build --release -p crust-render --features traversal-stats --target-dir target/tstats
target/tstats/release/crust-render -i $ISLAND --camera /island/cam/shotCam -s 1 --stats -l debug > island.log
# the knockout: a layer that sublayers island.usda and adds
#   over "island" { over "isDunesB" ( active = false ) { } }
# the before/after timing, interleaved (binaries built from one Cargo.lock)
scripts/bench_ab.sh -a bin_before -b bin_after -n 2 -p Render \
    -x "--camera /island/cam/shotCam -s 4" $ISLAND
# level 1 in either packet layout, Ptex streamed (peak ~51 GiB gathered, ~42 indexed)
CRUST_TRI_PACKETS=indexed CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIPSPACE=file \
    target/release/crust-render -i $ISLAND --camera /island/cam/shotCam \
    --subdiv-level 1 -s 16 --stats
```
