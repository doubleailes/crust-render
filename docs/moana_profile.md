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

Same wall time as before (about 5:10 to 5:20) and the same peak RSS (51.5 GiB).

## Memory: storing resident geometry once

Measured 2026-10-01 for `compact-geometry-storage`.
- **Setup:** the island at `bb53ff2` against the change, `shotCam`, `-s 1 --stats`,
  Ptex streamed (`CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIPSPACE=file`) so that Ptex is
  not the dominant variable, under an RSS guard that kills the process at 56 GiB (on a
  61 GiB machine).
- **The starting point:** with Ptex preloaded (the default), level 0 peaks at
  **43.1 GiB**, 6.02 GiB of which is Ptex. Streaming takes that to 37.3 GiB.

| level 0, Ptex streamed | before | after | |
|---|---|---|---|
| kernel memory | 26.58 GiB | **13.85 GiB** | **−12.73 GiB** |
| · triangle records (was: primitive nodes) | 8.04 GiB | 1.36 GiB | 80 → 24 B per triangle, and instances and curves no longer take a slot |
| · instances + cubic spans (was: boxed primitives) | 7.90 GiB | 2.47 + 1.73 GiB | 320 → 96 B per instance, 176 → 96 B per span |
| · vertex normals | 2.72 GiB | 0.38 GiB | per vertex, not per corner |
| · BVH nodes, packets, leaves, indices | 7.95 GiB | 7.95 GiB | unchanged |
| peak RSS | 37.29 GiB | **26.06 GiB** | −11.23 GiB |

**Speed is not paid for the memory; it gets better.** `bench_ab.sh`, alternating the
two binaries on the island at `-s 4` (2 reps each), timed `Render`:

| | min | mean |
|---|---|---|
| before | 1.390 s | 1.418 s |
| after | 0.951 s | 0.965 s |
| Δ | −31.6% | −31.9% |

callgrind cannot see this gain. On cornellbox, which fits in cache, the change is
+0.15% instructions, and the default `bench_ab.sh` scenes move within ±2.5% (noise).
The island's gain is cache behaviour: a closest hit reads a 24-byte record instead of
an 80-byte one, and an instance descent reads a 96-byte inline payload instead of
chasing a box.

The kernel figure lands within 2% of the design's estimate (about 13.6 GiB, struct
sizes × counts). The render is bit-identical: the island and ALab frames above diff to
0 pixels, and so does every checked-in sample, and both Kitchen_set variants, at 16 spp.

**Level 1 still does not fit.** `--subdiv-level 1`, with Ptex streamed, was killed at
the 56 GiB guard both times, still in `Traverse prims`. Before the change it was
killed at 6:27; after it, at 8:17. The change moves the wall, but the level-1 import
does not reach `Commit` on this machine. What is left at that point is not only kernel
memory:
- the importer's per-triangle side tables (`FaceMap` / `UvMap`: corner UVs,
  densities, Ptex face ids, and 24 B of sub-face UVs per refined triangle);
- the refiner's transient (about 313 B per refined face);
- the build transient.

`compact-triangle-layout` (no packets) and `adaptive-subdivision` are the proposed next
steps. Counting the non-kernel memory in `--stats` would show which of these comes
first.

ALab (level 0, same flags) went from 6.87 to 4.28 GiB of kernel memory and from 28.65
to 26.34 GiB peak. Its peak is dominated by what the kernel does not own.

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
```
