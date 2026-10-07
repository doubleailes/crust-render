# usd-scene-import — design record

> Design record for the **usd-scene-import** capability: the reasoning, measurements and
> history behind the behaviour `spec.md` states. Moved out of `CLAUDE.md`, which
> now keeps only the rules and pointers. Section and path references such as
> "above" or "see X" may point to another capability's `design.md` —
> `openspec/specs/*/design.md` is the whole record; `docs/architecture.md` is the map.

## Overview, streaming import and import time

The only scene format. The module is split by schema family — `mod.rs` holds
`load_scene`, the streaming chunk loop, the traversal and the import-wide state
(`ImportCtx`, `ImportCaches`), and dispatches into `mesh`, `shapes`, `instancing`,
`lights`, `light_links`, `materials`, `preview`, `volume`, `camera`, `xform`,
`settings`, `attrs` and `time`; the table in `mod.rs`'s module doc says which reads what. Siblings expose what
they share as `pub(super)` and import each other explicitly — keep it that way (no
`use super::*`), so a file's `use` block stays its real dependency list.

`load_scene` opens the stage, imports `RenderSettings` first (the
camera needs the aspect ratio), then traverses prims with an explicit stack that bakes the
Xform hierarchy into world matrices.

**Streaming import.** A production stage is dominated by USD itself, not by the
renderer's own structures: composing all of Moana costs openusd 75.74 GiB and 6m19s,
against 1.10 GiB and 2.6s for a stage masked to one element. So `load_scene` opens a
cheap index stage (`InitialLoadSet::LoadNone`) for the settings and the list of
top-level subtrees, then composes, traverses and **drops one masked stage per subtree**
(`stream_roots` + `traverse_into`), bounding the composed set at about one element.
On the island: 117.10 GiB / 13:20 → 43.76 GiB / 09:19, output pixel-identical.
`MIN_STREAM_CHUNKS` keeps the single-stage path for scenes too small to repay the
re-opens; `CRUST_STREAM_IMPORT=0` forces it.
The subtlety is cache keying: prototype paths (`/__Prototype_N`) are **numbered per
composition**, so every masked stage has its own `/__Prototype_0`. Anything keyed on
such a path must be scoped per stage, or one chunk's data is silently handed to the
next — see `MaterialCache::key` and `ImportCaches::epoch`. Keying materials on the bare
path cost 5 835 258 triangles before it was caught, and *no single element reproduces
it*: it needs two chunks that both carry prototype-internal materials.

**Where import time goes** (ALab frame 1004, single-stage, stack-sampled with
`eu-stack` every 1.5 s — callgrind is hours on a 30 GiB import, and `perf` is not
installed here). Before the fixes below, `Traverse prims` was 3:24:
- **~35% is openusd composing the stage**, serially and lazily: the first query into a
  prototype namespace (`is_active` on `/__Prototype_0`) runs `discover_prototypes`,
  which walks and composes the whole populated stage — one call, 105 s. It runs
  **once** per population epoch, and the traversal reuses what it composed, so it is
  not duplicated work; C++ USD does the same composition in parallel at open. It is
  openusd's to parallelise (its stage is `Rc`-based), not crust's.
- **~23% was prototype BVH builds**, one mesh at a time as `collect_proto_parts` met
  them. Now each prototype's meshes are collected first and built together in
  parallel (`MeshArena::commit_slots`); a part holds `placeholder_scene()` until then.
  The parallel builds go in batches of at most `PARALLEL_COMMIT_TRIS` (2 M) triangles:
  a build's transient memory is proportional to its triangles, so an unbounded map
  would let a prototype of several large meshes hold all their transients at once.
- **~22% was dropping the composed stage** — openusd's index cache is millions of
  small allocations, 45 s of `free` before the render could start.
  `UsdImportOptions::skip_stage_teardown` (off by default, on in the CLI) leaves a
  **single-stage** import's stage allocated instead (`release_stage`). A streamed
  import drops every chunk, the last included: streaming exists for its memory bound,
  and its peak often comes after the traversal (the top-level BVH commit on the
  island), where a kept stage would stack on top of it. On ALab peak RSS is unchanged
  (~33 GiB) — the peak is reached during traversal, and glibc keeps a freed heap mapped
  anyway — while RSS after the traverse stays at the peak instead of falling. A host
  that loads several scenes in one process must leave the option off.
Together (`scripts/bench_ab.sh -n 2 -p "Parse USD stage" -x "-f 1004 --camera … -s 1"
samples/ALab/entry.usda`): 209.9 / 216.1 s → 163.2 / 164.1 s, **−22.2% min, −24.1%
mean**; 16 spp image identical. `bench_ab.sh -p` times any `--stats` phase (default
`Render`) and `-x` passes extra renderer arguments.

### Light links are decided after the last chunk

`light_links.rs` reads `collection:lightLink` and `collection:shadowLink` through
openusd's own `Collection::compute_membership_query`, so every `UsdCollectionAPI`
rule (nearest opinion, `includeRoot`, expansion rules, nested collections with
cycles broken) is the reference's. Two traps:

- **UsdLux's `includeRoot` fallback is true**, `UsdCollectionAPI`'s (and openusd's)
  false. When it is not authored the pseudo-root is added to the query's rule map
  (`link_query`), unless `/` already carries an opinion.
- **Ordering.** A light can be traversed before the receivers it links, and when
  streaming they sit in different chunks whose stages are gone by the end, so
  nothing is decided at the light. The traversal records, per prim that emits
  geometry (mesh, sphere, curves, native instance, `PointInstancer`, an area
  light's emitter) or a volume region, its first `geom_id` and its interned stage
  path; ids are handed out in traversal order, so that run covers every geometry
  the prim emits. Judging the instance or instancer prim, never a prototype's
  `/__Prototype_N` path, keeps the answer independent of instancing and streaming.
- **Two stages, each wrong somewhere.** A streamed chunk has every opinion,
  payloads included, but its population mask leaves out prims outside the
  subtree, so a nested collection in another scope resolves to nothing there.
  The index stage (payloads unloaded) has every subtree but no payload, so a link
  a payload authors on an existing light is missing there. So a light's
  collections are read on the index stage only when its link opinions
  (`includes`, `excludes`, `includeRoot`, `expansionRule`, `membershipExpression`)
  are the same there as on the chunk, and on the chunk otherwise, with a warning
  if it nests a collection the chunk does not compose. The index stage is
  dropped right after link resolution, before `flush_meshes` and the top-level
  commit that is the import's memory peak. Both production rigs (the island's,
  ALab's) come in through sublayers and references, not payloads.

After the last chunk, and before `flush_meshes` / `commit`, `LightLinks::resolve`
evaluates every distinct path against every linked light, and:
1. demotes the lights whose `lightLink` includes no receiver, highest index first
   through `LightList::remove` (the backdrop case, see the `lighting` record);
2. deduplicates the light-link answers into classes and fills the per-`geom_id`
   table (`WorldBuilder::set_light_classes`) from the runs;
3. encodes shadow classes into geometry and volume masks
   (`SceneBuilder::set_mask`, the mask twin of `set_geometry`), allocating bits by
   the number of geometries each class holds;
4. installs per-light illuminated sets, shadow masks and NEE-only flags
   (`LightList::set_links`).

The memory is one `(u32, u32)` per emitting prim plus one path per distinct one.
A `PointInstancer` counts once however many placements it has.

The render setting `domeLightCameraVisibility` (Hydra's name) or
`crust:domeLightCameraVisibility` is read off the index stage with the other
settings, and applied right after `resolve`.

## Geometry schema mapping

Schema mapping:

- `UsdGeomMesh` → **either** world-space triangles in the top-level BVH **or** a
  local-space committed `rt::Scene` placed by an `rt::Geometry::Instance`, decided by how
  many times the geometry is placed. Prims with identical points/topology/material are one
  *distinct mesh* (content hash + memoized material Arcs, so binding paths compare by
  pointer); a distinct mesh placed **exactly once** is baked flat, anything placed more
  than once keeps one shared kernel scene and an instance per placement. Instancing what
  is placed once buys no sharing and costs every entering ray a transform, a slab setup and
  a cold descent into a second tree — and presents the parent BVH a box-of-a-box that
  spatial splits cannot tighten. On cornellbox this took instance descents from 3.85 to
  0.13 per camera ray.
  The decision is deferred: `emit_mesh` interns the triangles and reserves a `geom_id`
  (`WorldBuilder::reserve_slot`), and `flush_meshes` fills the slots in after the last
  streamed chunk — placement counts are only final then, and deciding **per chunk** would
  both make results depend on stage layout and give geometry shared between two elements
  one resident copy each. `CRUST_MESH_BAKE=0` forces the all-instanced behaviour; with it
  set the output is bit-identical, which is what separates a deferral bug from a baking
  difference. Baking a mirrored (`det < 0`) placement **swaps two indices**: world-space
  vertices wind the opposite way, so without it `front_face` inverts.
  Non-invertible transforms still bake immediately, and a mesh authoring
  `crust:motion:translate` always instances (a baked mesh has no transform left to lerp).
  `UsdGeomSphere` → analytic `Sphere` geometry.
- **Subdivision surfaces** (`scene/subdiv/`, via the pure-Rust
  [`opensubdiv-rs`](https://github.com/doubleailes/OpenSubdiv-rs) port of OpenSubdiv's
  Far/Sdc layers — zero dependencies, `forbid(unsafe_code)`, pinned to its `0.5.0` release tag,
  whose commit the committed `Cargo.lock` records). Read from USD, never from a crust
  attribute on the prim: every mesh is a subdivision surface
  unless its `subdivisionScheme` is `none`, with an unauthored scheme taking the schema
  fallback, `catmullClark` → Catmark (`bilinear` → Bilinear, `loop` → Loop on
  all-triangle cages only, else warn and render the cage). It is uniformly refined to the
  load's one level, **snapped to the limit surface**, and shaded with smooth per-vertex
  normals (the kernel's `TriangleMesh.normals`, interpolated by barycentrics).
  - **Trap: the fallback is the signal.** A first cut refined only an *authored* scheme,
    on the belief that polygon exporters leave it unauthored. Production data says the
    opposite: ALab's and Kitchen_set's render meshes author neither a scheme nor
    normals — USD's way of saying "subdivision surface" — while ALab's polygonal display
    proxies author `none` *and* face-varying normals. ALab rendered its foam hand and
    glassware as faceted cages until the fallback was honoured. Polygon content has to
    say `none`; every checked-in sample does. (Checking an asset: crate files ≥ 0.4
    compress their token table, so `grep catmullClark` on a `.usd` finds nothing — read
    the attribute through openusd instead.)
  - **Level 0 is a smooth cage**: the cage's triangles with smooth per-vertex cage
    normals (`subdiv::smooth_cage_normals`), as Storm draws a subdivision surface at low
    complexity — so `--subdiv-level 0` renders a large subdivision scene smooth-shaded
    at its cage's memory. `none` stays faceted, and so does everything under
    `CRUST_SUBDIV=0` (`SubdivPolicy::enabled`), which is the behaviour this replaced.
  - The **level** is one per load, as Hydra's `refineLevel` is a render setting and
    USD has no per-prim level. It is resolved in `load_scene`: the host's
    `UsdImportOptions::subdivision_level` (the CLI's `--subdiv-level`), then
    `crust:subdivisionLevel` on the `RenderSettings` prim, then **0**; clamped to 6.
    `CRUST_SUBDIV=0` forces 0. It lives on `MeshArena` (`SubdivPolicy`) because every
    path that reads a mesh — direct prims, prototype parts, every streamed chunk —
    already holds the arena.
  - Why 0 by default: every production scene here is a subdivision scene — the Moana
    island authors `catmullClark` explicitly in 189 of its 213 mesh-bearing files, and
    ALab and Kitchen_set take the fallback — so any default refinement multiplies them.
    Level 1 costs ALab 32 → 49 GiB peak (measured below); level 2 would cost Moana 16×
    on 60.9 M triangles and 39 GiB of kernel memory. Refinement is asked for by the
    scene's RenderSettings (the DPEL teapot wrappers set 1) or by `--subdiv-level`.
  - **Adaptive level** (`usd_import/adaptive.rs`, opt-in): with a target edge length
    (`crust:subdivisionEdgeLength` on the `RenderSettings` prim, or the host's
    `UsdImportOptions::subdivision_edge_length`, the CLI's `--subdiv-edge-length`,
    which wins) each subdivision mesh gets its level *per placement*: the smallest `L`
    at which its mean cage edge `ē`, stretched by the placement and seen at the nearest
    point of its bounds, projects to at most the target. `p = ē · s · f_px / d`,
    `L = clamp(ceil(log2(p / t)), 0, max)`, with `f_px = height · focal / aperture` and
    `d` 0 (so `L = max`) inside the bounds. The level setting becomes the ceiling,
    `DEFAULT_ADAPTIVE_MAX_LEVEL` (3) when neither the host nor the stage sets one.
    Every term errs toward detail: the mean edge, `ceil`, the nearest point, and a
    stretch `s` that is the transform's exact spectral norm (`adaptive::stretch`).
    - **Trap: the largest column norm is not a stretch bound.** The design first used
      it; it equals the spectral norm only when the columns are orthogonal. A scale
      applied after a rotation (diag(2, 1, 1) · R45 has columns of norm 1.58) or a shear
      stretches some direction further than any column, so the level came out low.
      The spectral norm is also submultiplicative, which nesting relies on
      (`stretch_is_the_spectral_norm`).
    - **The camera comes first.** `subdiv_policy` reads the named camera (`--camera`
      or `RenderSettings.camera`) before the traversal, from the index stage, else
      from a stage masked to its path with payloads loaded (`shotCam` on the island);
      `camera::screen_projection` shares `lens` with `build_camera`, and a debug
      assertion checks the two agree. No named camera: one warning, uniform level.
      Orthographic does not arise: the camera importer builds only perspective.
    - **Direct meshes** are levelled in `mesh_source` from the prim's world transform
      and cage box (`MeshPlace::World`); `MeshKey` hashes the refined arrays, so equal
      levels still share and unequal ones cannot collide.
    - **Only unshared geometry is adaptive** (`per-face-adaptive-tessellation`):
      a direct mesh prim, a native instance whose prototype has no other placement in
      its top-level subtree, and a `PointInstancer` prototype placed once are rated
      through their world transform (`MeshPlace::World`, `ProtoPlace::Unshared`);
      every other prototype is *shared* and refined to the uniform level (the level
      setting when given, else 0), whatever its distance (`MeshPlace::Shared`). This is
      MoonRay's rule (`GeometryManager.cc` sets the adaptive error of shared
      primitives to 0); Cycles likewise dices only single-user meshes in world space.
      Placements are counted per top-level subtree in every import mode
      (`count_placements`, `subtree_roots`) because prototype paths are renumbered per
      streamed stage — a whole-stage count would let streaming change the result.
      `PointInstancer` targets are counted the same way, across every instancer of
      the subtree, so a prototype two instancers each place once is shared; and only
      placements that draw count, so a zero-scale "hidden" placement neither makes a
      prototype shared nor gets a version of its own.
      - **Trap: what was replaced.** `adaptive-subdivision` built one prototype version
        per rate bucket at the placement's distance. Every island element is
        `instanceable` with its geometry under a payload, so its kilometre-wide terrain
        arrived as prototypes placed once and was refined whole: killed past 56 GiB
        at 2 px. The buckets, the prototype survey and the `prototype versions` stat
        are gone; the shared caches are keyed `(epoch, path)` again.
    - **Per-face tessellation** (`scene/tessellate.rs`, `subdiv::tessellate_adaptive`,
      `CRUST_ADAPTIVE_PER_FACE`, default on): an unshared Catmull-Clark or bilinear mesh
      is cut per Ptex face, every cage edge into `clamp(ceil(ℓ · σ / t), 1, 2^max)`
      segments from its own cage chord and distance, decided once per edge so the two
      faces sharing it agree; interiors are gridded and stitched to their edges by the
      shorter diagonal; corners and edge points are evaluated once on the limit
      surface (patch table, `du × dv` normals) and shared. A face whose edges are all
      rated 1 is not refined: it renders its smooth cage, as level 0 does and as
      MoonRay does with a factor-0 face, and only faces with a finer edge are refined
      and patched (`refine_adaptive_selected`, `create_with_options_selected`,
      opensubdiv-rs 0.5.0). Ptex triangles carry explicit corners (`FaceMap::corners`);
      a face-varying chart is evaluated with face-varying patches, per triangle corner
      in its own Ptex face so seams keep each side. A `loop` mesh keeps the per-mesh
      level. Geometry wholly out of the camera's view is split once
      (`adaptive::Frustum`, `CRUST_ADAPTIVE_FRUSTUM`). Isolation depth 1
      (`subdiv::ADAPTIVE_ISOLATION`): exact down to it, Gregory patches below.
      - **Trap: an all-triangle cage is irregular everywhere.** Every split triangle's
        centre is a valence-3 vertex, so isolating to depth `d` refines every face `d`
        times: at depth 3 the island ocean's 684 416-triangle cage cost a 19.9 GiB
        transient (5.8 GiB at 1), and its patches were Gregory caps at 2.7 KB each
        until opensubdiv-rs 0.4.0 (1.0 KB).
      - **Trap: four side planes do not bound a view pyramid.** They all meet at the
        eye, so a box behind the camera passes each with a different corner; the
        frustum also tests the eye plane facing forward.
      - **Trap: the even rule cascades.** An `n`-gon's Ptex quads need its edges split
        at the midpoint; rounding every `n`-gon's edges up to even selects every face
        of an all-triangle cage. Only *selected* `n`-gons force it, and an unselected
        neighbour that gets such a midpoint is fanned from its centroid.
      - **Trap: guard the renderer, not its shell.** A memory guard that `pgrep`s the
        command line matched the wrapping shell, never fired, and let the kernel's
        OOM killer end a run silently; guard the PID the shell started (`$!`).
    - **Reported** by `--stats` (`adaptive subdivision`, `subdivision levels` per
      per-mesh read, `shared meshes N at level L`, `per-face meshes N (fallback: M)`,
      `edge rates`), one INFO line, a DEBUG line per refined mesh, and one DEBUG line
      of triangle shapes (`4√3·area / Σ edge²`, interior against stitched).
    - **Measured** (Ptex streamed, under a 56 GiB guard):

      | | triangles | kernel | peak | Traverse prims |
      |---|---|---|---|---|
      | island, uniform L0 | 60.9 M | 13.52 GiB | 23.79 GiB | 2:37 |
      | island, uniform L1 | 274.7 M | 36.59 GiB | 51.07 GiB | 6:41 |
      | island, per-mesh 2 px (max 3, 2) | — | — | killed > 56 GiB | — |
      | island, per-mesh 2 px, max 1 | 187.2 M | 27.75 GiB | 38.47 GiB | 6:01 |
      | **island, per-face 2 px, max 3** | **63.6 M** | **13.83 GiB** | **24.70 GiB** | **3:25** |
      | ALab 1004, uniform L1 | 81.8 M | 10.47 GiB | 36.86 GiB | 4:04 |
      | ALab 1004, per-mesh 2 px, max 3 | 74.2 M | 9.28 GiB | 35.67 GiB | 3:26 |
      | **ALab 1004, per-face 2 px, max 3** | **22.8 M** | **3.97 GiB** | **29.46 GiB** | **2:58** |

      The island: 32 589 per-face meshes (edge rates 1: 48.7 M · 2: 89 k · 3–4: 59 k ·
      5–8: 63 k), 86 682 shared meshes at level 0, within 1 GiB of uniform L0's peak.
      ALab: 5 242 per-face meshes, 381 shared, under uniform L1's memory with more
      detail near the camera than L1 gives anywhere.
      Render speed is unchanged: at `--subdiv-level 1` (where both fit), interleaved,
      per-face Render 3.631 / 3.736 s (min / mean) against per-mesh 3.752 / 3.789 s.
      Stitched triangles are shaped like the interior grids' (1.2% below a quality of
      0.1 against 0.8%), so the BVH sees no sliver population.
  - The per-prim `crust:subdivisionLevel` this replaced is read only to warn — once per
    load (`SubdivPolicy::legacy_warned`), since a per-prim warning would scale with
    the scene.
  - `creaseIndices` / `creaseLengths` / `creaseSharpnesses` (per-run or per-edge
    sharpness, 10 = infinite), `cornerIndices` / `cornerSharpnesses`,
    `interpolateBoundary` and `faceVaryingLinearInterpolation` are honoured;
    `holeIndices` is not. `cornersPlus2`'s concave-corner sharpening needs
    opensubdiv-rs ≥ 0.1.4 (0.1.3 implemented its junctions and darts only, so a
    concave UV corner came out smoothed; pinned by
    `corners_plus2_pins_a_concave_uv_corner`, which fails on 0.1.3). **Gap:** a
    `loop` mesh whose material reads Ptex is not refined, because Loop builds no
    face table and its refined triangles would be read as cage face ids (it renders
    its smooth cage, with a warning).
  - Refinement happens in `mesh_source`, *before* interning, so every path (direct
    bake, deferred instance-vs-bake, prototypes) sees it exactly once and `MeshKey`
    dedupes on the refined arrays, the refined chart included. **Trap:** at level 0
    a subdivision cage and a `none` cage share every array but not their shading,
    so `MeshKey` also carries whether the mesh has smooth normals — without it the
    first to intern shaded both (`a_smooth_cage_and_a_faceted_cage_do_not_share_a_mesh`). A malformed cage or
    refiner error warns and degrades to the cage.
  - **The UV chart is refined with the surface.** A `faceVarying` `primvars:st` (and
    `:indices`) becomes a real face-varying channel under the mesh's
    `faceVaryingLinearInterpolation` (USD fallback `cornersPlus1`, *not* OpenSubdiv's
    `cornersOnly`); a `vertex` chart is refined like the points. Both are snapped to
    the limit with the positions, so a texel stays on the limit point its vertex was
    snapped to. A chart whose indices do not resolve is dropped with a warning,
    because a refiner cannot skip a bad value the way a triangle lookup can. Pinned by
    `subdiv::tests` (affine charts reproduced exactly under all six rules, a UV seam
    keeps each side on its island, vertex charts) and by inline-USD tests reading the
    chart back through a hit (`a_subdivided_mesh_keeps_its_uv_chart`,
    `every_face_varying_rule_is_read`).
  - **Ptex keeps indexing the base cage**: refined triangles carry explicit corner UVs
    (`FaceMap.uvs`) mapping them back into their cage face's unit square, from a
    synthetic face-varying channel, and `check_face_count` compares the texture
    against the *authored* face count. **Trap:** the face-varying rule is one per
    refiner, not per channel, and the authored chart's rule wins. The Ptex channel
    comes out bit-identical under five rules (each value is private to its face, so
    every edge is a face-varying boundary, and the data is affine), but not under
    `none`, which smooths face-varying corners (0 → 0.125…). A mesh needing Ptex *and*
    a `none` chart is therefore refined twice, once chartless for the face table.
    `ptex_channel_is_invariant_under_every_fvar_rule` pins both paths.
  - Baked placements push normals through the inverse transpose (`bake_normals`),
    matching the kernel's instance path exactly — mirrors included.
  - Sample scene: `samples/subdivision.usda`, six identical cube cages authored with no
    scheme (the fallback), `none`, `bilinear`, `catmullClark`, a fully edge-creased `catmullClark`
    (stays a cube), and a UV-textured `catmullClark`, at a settings level of 2. Compare
    levels with `--subdiv-level`.
  - **Memory, measured** (the refiner retains every level 0..L, a ×4/3 geometric series
    over the last level): `subdivide()` transiently allocates **~313 B per refined face**;
    ~560–568 B with the Ptex sub-face channel and ~536 B with a shared UV chart (each
    face-varying channel is a full parallel hierarchy). The returned mesh holds 44 B/face;
    84 with the Ptex table, 68 with a refined chart (48 / 88 / 72 before
    `compact-triangle-storage` unpadded the normals). Pinned by the allocation-counting
    probe `cargo test --release -p crust-core --lib subdivision_memory_probe -- --ignored
    --nocapture --test-threads=1`, whose deterministic requested-byte ceilings sit ~20%
    above those numbers. The refiner is dropped before the result's copies are built;
    the probe counts requested bytes, not the live peak, so that shows in RSS rather
    than in its table.
  - End to end (`scripts/gen_subdiv_stress.py`, 1.18 M refined quads at level 3, A/B'd
    with `CRUST_SUBDIV=0`): traverse-phase peak +310 MiB, within 6% of the model.
    Kernel-resident memory scales exactly ×4 per level (34.67 MiB at level 1 →
    2.17 GiB at level 4). The whole-process peak is **not** opensubdiv: at level 4
    traversal peaks at 1.16 GiB while the SBVH build over the baked result peaks at
    2.19 GiB. That is the pre-existing build transient (see "Known gaps: geometry and
    acceleration" in `openspec/specs/intersection-kernel/design.md`), which
    subdivision merely feeds 4^L× more triangles. Those figures predate
    `compact-triangle-storage`; after it the same level-3 grid holds 202.86 MiB of
    kernel memory instead of 416.89 (95.5 bytes per refined triangle instead of 196),
    its traverse-phase RSS is 223 MiB instead of 284 (unpadded importer arrays) and its
    whole-process peak 508 MiB instead of 809.
  - The DPEL MaterialX teapot (three `catmullClark` cages, 32 504 quads, faceVarying
    `st` under `boundaries`), measured with `--stats`:

    | | cage | level 1 | level 2 |
    |---|---|---|---|
    | triangles | 64 930 | 259 870 | 1 039 462 |
    | kernel memory | 11.05 MiB | 54.15 MiB | 212.95 MiB |
    | peak RSS | 456 MiB | 582 MiB | 1.02 GiB |
    | traverse + commit | 0.09 s | 0.32 s | 1.33 s |

    Its 18 s import is texture decoding either way. The table predates
    `compact-triangle-storage`; after it (measured 2026-10-01 at `24f9458`, the
    baseline rebuilt at `616c53b` reproducing 212.95 MiB) level 2 holds 110.14 MiB of
    kernel (111 bytes per triangle, lanes 82.6 % filled), peaks at 744 MiB instead of
    1.02 GiB, and its importer keeps 25.6 bytes per refined triangle less (the
    `Traverse prims` RSS above the cage's: 206.1 → 182.3 MiB for 974 532 triangles); the
    cage holds 6.98 MiB.
  - ALab, frame 1004, `renderCam` (its render meshes take the fallback scheme), with
    `--stats`; level 0 at 640×360 × 8 spp, level 1 at 1920×1080 × 32 spp (parse and
    memory do not depend on either):

    | | level 0 (default: smooth cages) | level 1 |
    |---|---|---|
    | triangles in memory | 21 166 922 | 81 836 458 |
    | kernel memory | 6.87 GiB | 19.64 GiB |
    | peak RSS | 32.30 GiB | 49.01 GiB |
    | Parse USD stage | 2:54 | 4:07 |

    After `compact-triangle-storage` (2026-10-01, `24f9458`, 640×360 × 1 spp) level 1
    holds the same 81 836 458 triangles in 10.47 GiB of kernel instead of 19.64 (137
    bytes per triangle, lanes 83.2 % filled) and peaks at 36.86 GiB instead of 49.01;
    the parse takes 4:15.

    Level 1 fits a 61 GiB machine with ~24 GiB to spare (~12 before); level 2 would not.
- `UsdGeomBasisCurves` → an instanced `rt::Geometry::RoundCurves` batch: `linear` curves
  directly, `cubic` (bezier | bspline | catmullRom) as one `CubicCurveSegment` per span,
  converted to Bézier control points and subdivided per ray query by the kernel's cubic
  intersector rather than flattened; widths
  (USD diameters) resolve per-vertex / per-curve / constant by array length. A span's
  radius is linear between its two ends; per-vertex widths are evaluated there through
  the curve's basis, as USD's `vertex` interpolation specifies (`span_end_values`).
  Only a Bézier span ends on its end control points — B-spline and Catmull-Rom spans
  once read the widths of control points they never pass through, so a tapered strand
  under those bases was thicker or thinner than authored at every joint.
  A curve hit shades with the strand's direction as its tangent: the kernel reports
  `dpdu` through every placement (intersection-kernel record), and `World::intersect`
  takes it, normalised, wherever the geometry has no UV map. OpenPBR,
  `UsdPreviewSurface` and `Emissive` ignore the tangent, so curves with those materials
  render as before. A MaterialX closure's frame on a curve is now the strand's instead of
  `tangent_frame(n)`'s arbitrary one: an isotropic lobe changes only its sample pattern,
  and an anisotropic one stretches along the strand. `normals` (ribbons), `wrap` and
  curve primvars are not read.
- **Instancing** — both USD mechanisms reduce to the same thing, and share one code path
  (`collect_proto_parts` → `attach_proto_parts`): build a prototype's geometry *once*, then
  place it by transform. A prototype becomes a `Vec<ProtoPart>` — one part per bound leaf
  geometry, each a committed local-space `rt::Scene` plus its prototype-relative transform,
  ray mask and **slots** (material + Ptex/UV tables). `World` maps materials by top-level
  `geom_id`, so a hit must say which part it landed on. A leaf part has one slot. A
  **group** (`group_parts`) is a whole prototype in one scene, whose member instances
  label their hits `0..n` (`crust_rt::InstanceHitId`, the one-id version of Embree's
  `instID[]` stack). It is placed as *one* instance that takes `n` consecutive `geom_id`s,
  and forwards the hit's slot index on top of the first (`InstanceHitId::Offset`). Grouping
  is what keeps a many-part prototype one box in the BVH above it. Top-level placements
  group from `TOP_LEVEL_GROUP_MIN_PARTS` (64) parts, since grouping costs every entering
  ray one more transform; nested instancers always group. Prototypes (`protos`) and their
  groups (`groups`) are memoized by path in `ImportCaches`.
  - `UsdGeomPointInstancer` → one instance per entry of the per-instance arrays. The
    transform is USD's `translate ∘ orient ∘ scale`, under the instancer's own world
    matrix; `orientationsf` (quatf) wins over `orientations` (quath); `invisibleIds`
    prunes by `ids` (array index where `ids` is absent). The instancer's children are
    **not** traversed — prototypes are conventionally authored beneath it and are drawn
    only through it. Nested instancers and volumes inside prototypes warn and are skipped.
  - Native instancing (`instanceable = true` + a composition arc) → the prim's prototype
    (`/__Prototype_N`) is built once and shared by every instance; the instance's own proxy
    subtree is never descended into. Without this the importer re-read and re-hashed each
    instance's geometry (~30% of load time on a 2000-instance scene).
  - **Nesting.** A `PointInstancer` inside a prototype expands into real nested sub-scenes
    (`nested_instancer_parts`): *one* part, a scene holding each nested placement as an
    instance of its prototype's group, the prototypes' slots laid end to end. M placements
    of a K-part prototype are M boxes of one tree each. It used to be K parts of M
    instances each (one per material), and every such part spans the whole scatter: on
    the Moana island, isDunesB's 679 bay cedars of 16 181 parts each became 64 724
    identical boxes over the dune field, 99% of all instance descents and a 6 ms ray
    (`docs/moana_profile.md`). Flattening instead would multiply the outer instance count
    by the inner one — the blow-up instancing exists to prevent. The kernel
    nests to arbitrary depth. A nested *native* instance is a single placement, so it
    takes no extra level: `collect_proto_parts` splices its prototype's parts (built once
    through `prototype_parts`, cached by `(epoch, path)` like any prototype) into the
    outer prototype's list as clones placed by `this_local · inner.local`, sharing their
    kernel scenes and slots, and does not walk the instance's proxy subtree. Each part
    keeps its own mask, as at a top-level instance; pruning runs first, so an invisible
    nested instance contributes nothing; depth goes through the same
    `MAX_INSTANCE_NESTING` guard. The inner prototype is always the shared version
    (gap below).
    Sample scene: `samples/nested_instancing.usda`. `MAX_INSTANCE_NESTING` (8) is a
    backstop against a malformed stage describing an instancing cycle.
  - `class` prims are abstract and never drawn on their own — only reached through the
    prototypes that reference them. `collect_proto_parts` deliberately ignores that rule,
    since naming a class as a prototype is how "geometry that exists only to be instanced"
    is authored. Sample scene: `samples/instancing.usda`.
  - Non-invertible instance placements (a zero scale — a common "hide this" idiom) are
    skipped: `rt::Geometry::Instance` requires an invertible transform.
- Any geometry prim may author `crust:rayMask` (int; bit 0 camera, bit 1 shadow, bit 2
  indirect — default all, except **light** source geometry which defaults to shadow|indirect;
  `crust:light:cameraVisible = 1` on a light prim re-adds the camera bit, an authored
  `crust:rayMask` wins outright — sample: `samples/light_visibility.usda`) to hide from
  ray categories, and `crust:motion:translate`
  (float3, world-space) to streak through that translation over the shutter (transform
  motion blur; primary rays draw a `K_TIME` shutter sample and every secondary/shadow ray
  inherits the path's time). Sample scenes: `samples/motionblur.usda`, `samples/curves.usda`.

## Displacement

Scalar displacement is applied **at import, once per distinct mesh**, between
tessellation and the kernel (`scene/displace.rs`, called from `MeshArena::intern` on a key
miss and from the non-invertible bake path). The kernel, the integrator and per-triangle
render cost are untouched: a displaced mesh is an ordinary `TriangleMesh` whose points
moved. Production pre-tessellating renderers (Hyperion, Arnold, Manuka) displace at the
same point.

- **Where it comes from.** `resolve_bound` returns the material with an optional
  `Displacement` (materials design record § Material resolution), from `PxrDisplace`, a
  MaterialX `displacementshader`, or `UsdPreviewSurface.inputs:displacement`.
  `mesh_source` then asks for the charts the displacement reads (UVs, a Ptex face table)
  even when the surface does not.
- **The key is the undisplaced source plus the material.** The displaced result is a pure
  function of both, so prims sharing a mesh pay once and the instancing decision, which
  counts placements per key, is unchanged. Displacing at the end of `mesh_source` instead
  would run per prim, before deduplication.
- **One sample per unique vertex, from its owner corner**, the first face corner that
  references it in face order and carries every chart the displacement reads (falling
  back to the first corner when none does, e.g. a vertex only an unmapped `n`-gon
  reaches). Positions are shared and each moves once, so the displaced
  mesh has the undisplaced mesh's connectivity: watertight wherever that was, across UV
  seams and Ptex face boundaries (`a_uv_seam_stays_closed`,
  `a_ptex_face_boundary_stays_closed`, `a_per_face_mesh_at_mixed_rates_stays_closed`).
  The owner table is built from the *source's own face lists*, whatever tessellation
  produced them (cage, uniform refinement, per-face tessellation). That is one rule for
  every path, and it needs no change to the per-face tessellator, whose `emit` closure
  the change's design had planned to record charts in. Ptex corners come from the
  refinement's face table (`SubdivFaces`, `TessellatedFaces`), or on a cage from Ptex's
  quad and triangle conventions. Averaging all incident corners was rejected: it needs
  incidence lists, costs a sample per corner, and at a discontinuous seam produces a
  value neither side authored.
- **Direction and units.** Along the unit pre-displacement normal (limit, refined smooth or
  smooth cage normal), in **local** space before the placement transform, so a scaled
  placement scales the offset. A faceted source gets area-weighted smooth normals for the
  direction only. A face normal per face would send an edge's two sides two ways.
- **Footprint = dicing rate.** Each lookup's width is the longer chart edge at the owner
  corner (UV units, or face-local units for Ptex). A coarse tessellation reads a coarse mip
  level, a low-pass of the map, rather than a random subsample that flickers between
  rates. `samples/displacement.usda` shows it: at level 4 the ground's 64-texel rings
  peak at 0.154 of a possible 0.3, and at level 6 (one vertex per texel) at 0.3.
- **Normals are recomputed** from the displaced triangles (`smooth_normals`). A faceted
  source stays faceted. Limit-normal precision is lost on displaced per-face meshes only.
- **`none` meshes are diced bilinearly** when displaced, so a ground authored as one quad
  can show its map. Under `CRUST_SUBDIV=0` or `CRUST_DISPLACE=0` they stay faceted cages,
  and so does one whose displacement reads Ptex while it has a non-quad face (see Known
  gaps).
- **Deterministic and parallel.** The pass reads no USD, so it fans out over vertices in
  fixed rayon chunks without touching the import's thread-local time. Every vertex is
  independent, so the output is bit-identical at any thread count
  (`the_result_does_not_depend_on_the_thread_count`).
- **Adaptive dicing and the bound.** The per-mesh and per-segment frustum tests and
  nearest-point distances grow the **local** box by the bound before it is carried to
  world (`Cull::Pad`), so geometry displacement can bring closer is diced for the nearer
  distance. A displaced point lies within that box exactly,
  whatever the placement's scale, so no `max_axis_scale` factor is needed. The bound is
  `|c|` for a constant, else the mesh prim's `crust:displacementBound`, else its
  `primvars:displacementbound:sphere` (RenderMan's, which the island authors), else the
  material's `crust:displacementBound`. A displaced mesh with no bound turns its frustum
  test off (`Cull::Off`, counted as "frustum test skipped"). Skipping the frustum for
  every displaced mesh was rejected: it costs the island's out-of-view memory saving
  wholesale. An undisplaced mesh's `Cull::Pad(0)` is the old box bit for bit
  (`a_zero_pad_keeps_every_rate`). Offsets are never clamped to the bound, since the BVH
  is built from the displaced points either way. A sampled offset past an authored bound
  warns once per mesh.
- **`--stats`** reports meshes, vertices, time (inside "Traverse prims"), the largest
  `|offset|`, meshes displaced at cage resolution, and meshes whose frustum test was
  skipped, only when a mesh was displaced. A non-constant displacement at cage
  resolution warns once per load.

- **Measured** (2026-10-03, `--stats`). These are the deterministic figures — counts,
  kernel memory, peak RSS — from one run per side. No timing comparison is claimed: the
  runs were sequential, not interleaved with `bench_ab.sh`, and this change does not
  touch per-triangle render cost. The displacement pass's own time is its `--stats`
  counter, measured inside the import.
  - `samples/displacement.usda` (level 6): 3 meshes and 33 028 vertices displaced (pass
    ~10 ms); 65 536 triangles against 49 156 with `CRUST_DISPLACE=0` (the two one-quad
    grounds dice to 8 192 each); peak RSS 37.4 against 33.5 MiB.
  - ALab (`entry.usda`, `-s 1`, no displacement authored), against the binary before this
    change: identical geometry (13 302 geometries, 21 166 922 triangles), no displacement
    lines, peak RSS 25.07 against 25.14 GiB.
  - The Moana island at `--subdiv-edge-length 2` (ceiling 3), `shotCam`, Ptex streamed,
    `-s 4`, under a 56 GiB guard, `CRUST_DISPLACE` on against off on the same binary:
    47 meshes and 575 256 vertices displaced (pass ~0.2 s), max `|offset|` 6.25 (the
    largest `dispScale`), 4 at cage resolution, none frustum-skipped (every displaced mesh
    authors `primvars:displacementbound:sphere`); 63 603 431 against 63 600 590
    triangles (the bound padding brings a few more edges into view and nearer); kernel
    memory 13.83 GiB either way; 3 632 against 3 618 Ptex textures (the 14 displacement
    maps: 12 `displacementMap`s and isDunesA's two blended ones); peak RSS 24.66 against
    24.55 GiB — well inside the guard.

## Known gaps: displacement

- **Hard edges soften on displaced `none` meshes.** They are diced bilinearly and shade
  with smooth normals of the displaced surface. Keeping them sharp would need creases
  synthesised on every cage edge.
- **Rates ignore displacement.** Dicing is sized by the undisplaced cage on screen. A
  strongly displaced region gets the cage's rate, and there is no re-dicing after
  displacement. `--subdiv-edge-length` and `--subdiv-level` are the levers, and
  `--stats` reports the largest offset so the need shows.
- **A seam step.** Where a map is discontinuous across a UV seam, the one ring of triangles
  at the seam stretches between the two sides' values (one owner per vertex). This is
  inherent to the authoring.
- **Ptex displacement on refined non-quad faces.** Refinement splits a triangle or an
  `n`-gon into quads no Ptex face addresses here (the same gap Ptex colour has). A
  displaced `none` mesh with such a face therefore stays on its cage when its
  displacement reads Ptex (one warning per load). On an authored subdivision surface the
  children's own vertices read no map and stay undisplaced. Vertices they share with a
  Ptex quad take the quad's value, since the owner is the first *charted* corner.
- **No vector displacement** (MaterialX `vector3`, `PxrDisplace.dispVector` /
  `modelDispVector`): warned about and ignored.
- **RenderMan networks beyond `PxrPtexture`, or a `PxrBlend` multiply of two, `→
  [PxrDispTransform] → PxrDisplace`** are refused with a warning: any other `PxrBlend`
  operation, a blend whose inputs are not both `PxrPtexture`, and a UV `PxrTexture`.
  `operation = 18` is taken to be multiply from the island's own use of it (it masks a
  bump colour with it too), not from RenderMan's documentation.
- **No bump from sub-dicing detail**, no lazy or per-ray dicing, no displacement of
  `UsdGeomSphere`, and no displacement beyond the per-mesh level on Loop meshes.
- **MaterialX view-dependent nodes** have no meaningful value at a vertex (they see a
  head-on viewer, `view = −normal`).

## Volumes, frame, camera and render settings

- **Volumes**: any prim carrying `crust:volume:type` imports as a `VolumeRegion` (checked
  *first* in the dispatch, so it never becomes geometry — its bounds must not occlude
  shadow rays). The local box is `[-size/2, size/2]³` when the prim authors `size` (a
  `Cube`; USD's default size is 2), else the unit cube; placement/orientation/scale come
  from the composed prim transform. Attributes (all in `crust:volume:`, defaults in
  parentheses): `type` = `homogeneous` | `smoke` | `grid` (required); `densityScale` (1);
  `sigmaS` color3f (0.5 grey); `sigmaA` color3f (0); `emission` color3f (0);
  `anisotropy` (0, clamped ±0.99). Smoke adds `noiseScale`/`noiseOctaves`/`noiseGain`/
  `noiseLacunarity`/`noiseThreshold`/`noiseSeed` (4 / 4 / 0.5 / 2 / 0.3 / 0); grid needs
  `gridDims` int[3] + `gridData` float[] (x-fastest, length must equal nx·ny·nz — warns
  and skips otherwise). Sample scenes: `samples/fog.usda` (homogeneous god rays),
  `samples/smoke.usda` (noise plume + emissive ember + tiny explicit grid).
- **Frame / time code** (`-f/--frame`, `Scene::from_usd_at_frame`). Every attribute read in
  the importer goes through `eval_time()` — `Attribute::get_at` at the requested code —
  so transforms, points, camera, lights, instancer arrays and render settings all move
  together; an attribute with no time samples reads its default either way, and only
  animated ones change. The time lives in a scoped **thread-local** (`EvalTimeScope`)
  rather than a parameter, because ~40 read sites in helpers handed only a `Prim` would
  otherwise all carry a value none of them decide; that is sound only because the import
  is single-threaded — parallelising it means threading the time explicitly. **No
  frame means the attribute *default*, not frame 0**: a stage that authors only
  `timeSamples` reads its schema fallback, which is exactly the pre-`--frame` behaviour
  (pinned pixel-identical on the samples). The one exception is transforms: openusd
  0.7's `local_to_parent_transform`, which composes every prim's `xformOp` stack, has no
  default arm, so without a frame it evaluates at 0.0 (`xform_time()`). That differs from
  the default only for an op that authors both a default and time samples; it goes away
  with the openusd bump, where `None` means default time. A frame
  also sets the sampler's frame seed (its integer part, over `crust:frame`), so a
  sequence gets independent noise per frame, and a frame outside an authored
  `startTimeCode..endTimeCode` warns (USD holds the end samples; it is usually a typo).
  A non-finite frame is refused in `load_scene` with `Error::InvalidFrame` (and earlier
  by the CLI's `parse_frame`): `NaN` compares false against the range, so it would
  otherwise pass the check silently.
  Not time-aware: `UsdPreviewSurface` inputs (read by openusd-schemas'
  `read_preview_surface`, default only), and `crust:motion:translate` motion blur, which
  is still an authored offset rather than derived from the samples across the shutter.
  Sample: `samples/animation.usda`.
- **Camera choice** (`UsdImportOptions::camera`, the CLI's `--camera`): the named prim,
  else the stage's **`RenderSettings.camera`** relationship (read off the index stage, so
  the traversal knows it before meeting any camera), else the first `UsdGeomCamera` the
  traversal meets. A requested path that is malformed is refused before the stage opens
  (`Error::InvalidCameraPath`), and one that names no camera fails after traversal with
  `Error::CameraNotFound`, which **lists every camera the stage has** — a production
  camera's path is usually buried in a referenced binary cache, and passing a bogus path
  is the quickest way to discover the real one. A dangling `RenderSettings.camera`
  target warns and falls back to the first camera, since the stage, not the operator,
  made that mistake.
- `UsdRenderSettings` gives `resolution`; per-render params live as custom attrs in the
  `crust:` namespace (`crust:samplesPerPixel`, `crust:maxDepth`, `crust:minSamplesPerPixel`,
  `crust:varianceThreshold`, `crust:adaptiveNeighbourTolerance` float (index units,
  default 1, negative disables the cross-neighbour comparison, non-finite warns and
  keeps 1), `crust:frame`, `crust:samplingStrategy` token = `power` |
  `balance` | `light` | `bsdf`, `crust:lightSelection` token = `uniform` | `power` | `learned`,
  `crust:pixelFilter` token = `box` | `triangle` |
  `gaussian` | `blackman` | `mitchell` + `crust:pixelFilterRadius` float,
  `crust:indirectClamp` float, and `crust:domeLightCameraVisibility` / Hydra's
  un-namespaced `domeLightCameraVisibility` bool, default true). Missing attrs
  fall back to defaults (128 spp, depth 32, 640×360, power MIS, power light selection,
  triangle filter at radius 1.0, indirect clamp 10) defined as consts at the top of the file
  (the clamp's in `tracer/settings.rs`, `DEFAULT_INDIRECT_CLAMP`, since it is the engine's own default).
  **`crust:indirectClamp`** is the one biased setting: it caps each camera sample's
  *indirect* light (everything past the primary vertex's continuation) at the value in
  its largest channel, scaling the colour whole (`tracer/path.rs`, `clamp_indirect`). Direct
  light is never touched — the primary vertex's NEE, its emission, and what its bounce
  finds, an emitter *or an escape to a dome or sun* — because clamping one MIS half and
  not the other would bias the pair; the escape case is why the clamp only engages when
  the path has a second vertex (`indirect_clamp_never_touches_direct_light` pins it).
  **It is on by default at 10** (as in production renderers), so the default render is
  biased: on the checked-in samples at 16 spp it moves 0–0.17% of pixels (outliers
  only; cornellbox and veach_mis not at all). `--indirect-clamp 0` (or an authored
  `crust:indirectClamp = 0`) is the unbiased estimator and bit-identical to the renderer
  before the clamp existed — use it for any convergence or bias measurement
  (`relmse` against a reference, the 1/√N check, guiding's unbiasedness), and record
  goldens with the same setting on both sides of an A/B.

## openusd version and API history

Note: `openusd` is a hard dependency and USD is always compiled in — there is no `usd`
feature flag.

**`openusd` is tracked at `0.7`**, and **the typed schemas are a second crate**:
0.7 moved `UsdGeom` / `UsdLux` / `UsdShade` / `UsdRender` out of the core crate into
[`openusd-schemas`](https://docs.rs/openusd-schemas), versioned in lockstep and carrying
the `geom` / `lux` / `shade` / `render` feature flags the core crate used to. So
`openusd::schemas::geom` is now `openusd_schemas::geom`, and core `openusd` has no
features left but `serde`.

Three API changes came with it, all in `scene/usd_import/`:

- `Stage::prim` / `Stage::attribute` / `sdf::Layer::prim` take any path-like argument and
  therefore return a `Result` whose error is a *parse* failure. Every call here passes an
  already-parsed `sdf::Path`, so `prim_at()` wraps the unreachable arm once rather than
  scattering the same `expect` over a dozen sites.
- `StagePopulationMask::new` is fallible (a mask path must be an absolute prim path).
  `open_stage` reports it as an ordinary `Error::UsdOpen` — `stream_roots` only ever
  yields composed top-level prim paths, so a failure would be a bug, not bad input.
- `Material::compute_surface_source` takes an **ordered render-context list** and returns
  the whole resolved terminal (every source driving it) instead of one shader. 0.6 took no
  argument: universal terminal first, then every authored context alphabetically. The
  `SURFACE_RENDER_CONTEXTS` const restores that preference — `""` (the universal context)
  leads, `glslfx` is the only namespaced one crust decodes, and an `ri` surface is a
  PxrDisneyBsdf that `has_shader_id` already caught upstream of the call. Verified
  output-preserving: all 15 checked-in sample scenes render **pixel-identical** to the 0.6
  build at 16 spp (`exr_diff`, 0 differing pixels).

Earlier history worth knowing when reading old branches: 0.6.0 fixed two composition bugs
that made the Moana island unreadable (written up under `docs/issues/`), and renamed 0.5's
`prim_at` to `Stage::prim` and made `sdf::Value::Token` carry an interned `tf::Token`
rather than a `String`.

## Rendering the Moana island

**`usd/island.usda` imports directly**: 3 151 850 geometries, 21 904 388 top-level BVH
primitives, ~6:18 to parse (~4:45 of it traversal), ~47.6 GiB peak. It reads from its own
root layer with no preparation — the openusd bugs that used to prevent that are fixed in
0.6.0. The only geometry still lost is 35 empty xgen prototypes (beach shells, fibers,
seaweed, palm debris); the six `PointInstancer`s that used to vanish with them now import,
which is worth ~74 500 resident instances of bay cedar understory.

`renders/moana_island/island_root.usda` (gitignored) is a hand-assembled root layer that
references each element at **stage root** instead of under `/island`. It is no longer
needed, and is kept only as the workaround for openusd 0.5, where the nested-reference
bug otherwise yields almost no geometry. `/island` carries no transform, so the two are
geometrically equivalent; they differ ~0.06% in the *unique* (resident) triangle count,
which is prototype-sharing accounting rather than rendered geometry — the likely cause
being that `/island` is a single top-level subtree, so `MIN_STREAM_CHUNKS` keeps the
direct read single-stage while the 22-prim layer streams, giving the two different
`MaterialCache` epochs. That layer also authors a camera inline as a copy of `shotCam`,
from before `--camera` existed: without a named camera the importer takes the *first*
`UsdGeomCamera` it meets, and traversal order is unspecified, so reading `island.usda`
directly gets whichever of its seven cameras comes first. `--camera` names one instead.

Measured per element (Ptex declined, so geometry only): **~57.7 M top-level triangles**
across the 20 elements, the largest being `osOcean` (15.6 M), `isCoral` (14.5 M),
`isMountainA` (6.7 M) and `isMountainB` (6.4 M). Worst single-element openusd composition
peak is ~12.9 GiB (`isCoral`), which streaming keeps as a transient rather than a sum.
Ptex over the whole island is **3 618 textures / 2 564 203 faces**, and preloading them
costs **5.98 GiB** — the often-quoted 4.58 GiB is the 32x32 base *without* the mip
pyramid, which adds the expected 4/3. (1.84 GiB at a 16x16 base, 736 MiB at 8x8, and
**494 GiB** at full resolution, which is why the cap was not an optimisation but the
thing that made the island possible.)

**Streaming replaces that cap** (`CRUST_PTEX_STREAM=1` **plus
`CRUST_PTEX_STREAM_MIPSPACE=file`** — every island `.ptx` is mipmapped, so the default
policy declines them all and the switch alone reproduces the preloaded column exactly;
see `docs/ptex_streaming.md`).
Measured 2026-10-05 at 640x360 / 8 spp against the same build preloading, two runs
per side alternating (the memory and residency figures are deterministic): Ptex
residency **7.34 -> 0.61 GiB** (53 textures streamed, 3 579 preloaded under the size
threshold; the 14 displacement maps are among the 3 632), `Load assets` **1:33 ->
28 s**, `Traverse prims` RSS **28.7 -> 21.1 GiB**, peak RSS **31.6 -> 23.9 GiB**, and
`Render` +1.8% / +3.2% (min / mean), within noise — nothing like the 2.8x the
deliberately texture-bound sample scene shows, because the island is traversal-bound.
The whole run finishes 67 s sooner, a quarter of it. The commit's transient is now
small on either side, so peak falls by the full traverse saving. (The first
measurement, 2026-09-27, before the island's traversal was fixed: 5.98 -> 0.61 GiB,
`Load assets` 1:40.7 -> 27.3 s, peak 51.28 -> 47.08 GiB, `Render` +1.2%.)
The cache held **7.6 MiB of a 2 GiB budget with zero evictions**: at this framing the
ray cone asks for coarse levels, and a coarse level of a face is a few texels — reading
only the resolution the frame resolves is exactly what preloading cannot do. The budget
is a ceiling, not an allocation, so do not lower it on that number; a closer camera or a
4K frame walks the same faces at finer levels.

**Rendering it used to be traversal-bound because of one element** (`docs/moana_profile.md`).
At `shotCam`, 99.9% of render thread time was ray traversal, at 5.97 ms per closest-hit
query. isDunesB's `xgTreeFill` scatters 679 bay cedars of 16 181 parts each, and
nested instancers were grouped per (prototype, part). That gave 64 724 top-level
instances whose boxes all spanned the dune field, and a ray entered ~12 500 instances
per query. Grouping per prototype (see "Nesting" under instancing) took the 4 spp render
from **312.6 s to 1.195 s** (`bench_ab.sh`, min of 2; −99.6%), Trace from 5.97 ms to 22 µs, kernel memory from 39.31 to
33.92 GiB and peak RSS from 51.5 to 46.2 GiB. Re-profiled 2026-10-05 at the defaults
(128 spp, displacement on): the render takes 29.3 s of a 5:42 run, peaks at 31.6 GiB,
and is still 90% traversal, at 17.7 µs per closest hit.

Two costs specific to the full rig: `island.usda` authors *two* `DomeLight`s, and both
textures decode, since `islandsunVIS.png` is 16384x8192 and the pair peaks at ~11 GiB.
They do different jobs. `sky_dome_cam_llc` authors `collection:lightLink:excludes =
</island>`, so it is imported as a camera-only **backdrop** in front of the HDRI
`sky_dome_env_llc`. Before that was read, both lit the island (the sky was doubled,
part of the cool cast against the RenderMan reference) and the camera saw their sum.
Dropping `sky_dome_cam_llc` (`active = false`) is still the memory lever, at the cost
of the camera seeing the HDRI instead of the backdrop.

### Displacement on the island

How the island authors displacement, read off its `.usda` and confirmed by importing
`isLavaRocks`, `isPalmDead` and `isDunesA` at `-l debug` (2026-10-03):

- **The shared `BaseMaterial`** (`usd/materials/material.usda`), which nine elements'
  displaced materials reference: `outputs:ri:displacement → PxrDisplace`, whose
  `dispAmount` is connected to the interface's `inputs:dispScale`, and whose
  `dispScalar ← PxrDispTransform.resultF ← PxrPtexture.resultR`. The Ptex's `filename`
  is connected to the interface's `inputs:displacementMap`. The transform's
  `dispRemapMode` is 2 (interpolate depth and height), its `dispCenter` connected to
  `inputs:dispOffset` (0.5 everywhere it is authored, 0 once), and `dispDepth` /
  `dispHeight` are unauthored, so RenderMan's defaults of 1 apply. `displacementMap` is
  authored in the material layers of isBayCedarA1, isBeach, isDunesB, isGardeniaA,
  isLavaRocks, isMountainB, isPalmDead, isPalmRig and isPandanusA; `dispScale` ranges
  from 0.0625 to 6.25.
- **isDunesA's `soil`** authors its own network instead: `dispScalar` comes from a
  `PxrBlend` multiplying two Ptex maps (`smushy` × the `duneSide` mask) through a
  `PxrDispTransform` (mode 2, centre 0.5, depth and height 0.35, `dispAmount` 5). The
  reader accepts a `PxrBlend` with `operation = 18` (multiply) whose `topRGB` and
  `bottomRGB` are both `PxrPtexture`s, reading the product of their raw red channels
  at the vertex's owner face (`DisplacementValue::Ptex` holds a list of maps). On the
  isDunesA element at level 0 that displaces `topsoil0001_geo`'s 4 859 cage vertices,
  up to 1.75 (`5 × 0.35`). It was refused with a warning until 2026-10-03.
- **The bound** is `primvars:displacementbound:sphere` on each displaced mesh prim,
  equal to its `dispScale`. It is read as the mesh's bound when no
  `crust:displacementBound` is authored. That settles the `add-mesh-displacement`
  design's open question.
- No material authors `dispVector` / `modelDispVector`.

The displacement Ptex files are read raw and preloaded or streamed under the same policy
as colour Ptex. At the default level 0 every island mesh is displaced at its cage
resolution, which moves cage vertices only and warns once.

## Known gaps: instancing

- **Instancing caveats.** The kernel nests instances to arbitrary depth (transforms
  compose, normals map back through every level, masks gate per level — pinned by
  `instances_nest`, `nested_instances_compose_transforms_and_normals` and
  `nested_instances_respect_masks_at_each_level` in `crust-rt`), and the importer expands
  a `PointInstancer` inside a prototype into real nested sub-scenes, and splices a
  native instance inside a prototype into its parts. Volumes inside prototypes are skipped — they
  live outside the surface BVH by design and cannot ride an instance transform.
  `PointInstancer`
  `velocities` / `accelerations` / `angularVelocities` are ignored, so vectorized instances
  do not motion-blur (`crust:motion:translate` still works on ordinary prims). The
  per-instance arrays themselves (`protoIndices`, `positions`, `orientations`,
  `scales`, `ids`, `invisibleIds`) are evaluated at the requested frame like every
  other attribute, so an animated instancer does move between frames. Top-level `UsdGeomSphere` prims
  still bake their centre into world space and so ignore scale; spheres *inside* a
  prototype go through the instanced path and scale correctly.
- **A nested native instance's prototype is never adaptive.** `count_placements` does not
  descend into instances, so it has no count with which to call an inner placement
  unshared; the inner prototype always takes the uniform level, the answer any placement
  counted more than once already gets.

## Known gaps: adaptive subdivision

- **Shared geometry never gains detail.** A prototype placed more than once in its
  subtree takes the uniform level whatever its distance; `--subdiv-level` raises all of
  them (MoonRay's `mesh_resolution` trade).
- **Unrefined faces render their cage.** Far and out-of-view faces sit on their cage,
  not the limit surface, and a face between a refined and an unrefined neighbour mixes
  limit and cage corners; its chart is interpolated linearly.
- **Out-of-view geometry is coarse in reflections and shadows.** The frustum cuts it to
  its cage; `CRUST_ADAPTIVE_FRUSTUM=0` rates by distance alone.
- **No Loop tessellation.** A `loop` mesh keeps the per-mesh level.
- **Gregory patches below the isolation depth** approximate the limit next to
  extraordinary vertices (within 0.9% of an edge on the worst-case cube).
- **Cracks between meshes.** Two separate meshes meeting at a boundary can be refined
  differently, exactly as two separately refined meshes always could.
- **Level popping across frames.** Each frame imports at its own camera; there is no
  dicing reference camera.
- **The rate ignores motion.** It is chosen at the camera's and the placement's
  shutter-open transforms.

## Known gaps: openusd bugs and workarounds

- **Transforms are openusd's; two divergences from C++ USD remain until the bump.**
  `openusd` 0.5.0 composed multi-op `xformOpOrder` stacks in the wrong order, so the
  importer used to compose `xformOp`s itself (in `f32`, falling back to openusd for six
  prim types and to identity for every other). That composer is gone: on 18 stacks
  covering every op kind it decoded (translate, scale, every single-axis and three-axis
  rotation, `orient`, `transform`, `!invert!`, pivot suffixes, a leading
  `!resetXformStack!`, a time-sampled translate), openusd 0.7.0 matched C++ USD 26.8 on
  every matrix entry. `xform.rs` now hands every prim to openusd's `Xformable` through one
  type-independent view (`AnyXformable`), composing in `f64` and casting once, so
  `translateX`-style ops and a leading reset work on every prim type. An op kind outside
  the `UsdGeomXformOp` vocabulary still warns (openusd 0.7 reads it as identity silently;
  that list decides the message only). Regression tests:
  `cornellbox_transforms_compose_correctly`, the `xform.rs` unit tests and
  `single_axis_ops_place_non_xform_prims` / `a_leading_reset_drops_the_parent_on_a_light` /
  `a_pivot_stack_places_geometry_as_cpp_usd_does`. Remaining, both fixed on openusd main
  and retired by the bump (`openspec/changes/retire-openusd-workarounds`, phase 2):
  - **A `!resetXformStack!` after the first entry** makes 0.7 refuse the stack: the prim's
    local transform is identity, with a warning, and its parent's is still inherited.
    C++ keeps only the ops after the last reset.
  - **Ops on a prim that is not `Xformable`** (an untyped prim, a `Scope`) still apply to
    it and its descendants; C++ ignores them.

- **Fixed in openusd 0.6.0, keep in mind when reading old branches.** Two composition
  bugs used to make the Moana island import as almost nothing, and both failed silently —
  the data was readable, the API reported success, and the importer just saw less than was
  there. A prototype did not materialize when the `instanceable` prim arrived through a
  reference on a non-root prim (which is exactly `island.usda`'s shape), and a
  relationship's targets resolved to zero when its prim reached the prototype through a
  variant selection (worth six `PointInstancer`s, all isBayCedarA1's variant geometry).
  Written up with minimal reproductions under `docs/issues/`, kept because the symptoms
  are worth recognising, and because pinning to 0.5 brings both back. `examples/proto_probe`
  and `examples/rel_probe` are the diagnostics.

## Known gaps: ALab

- **ALab gaps** (Netflix Animation Studios' ALab 2.2, `samples/ALab/`, gitignored).
  Shot mk020_0281, frames 1004–1057. `entry.usda` sublayers the baked procedurals
  (fur and cloth as value-clipped `BasisCurves`), the trailer cameras and the shot. It
  imports and animates under `-f`. Frame 1004, measured 2026-10-05: 13 302 geometries,
  21.2 M triangles plus 9.4 M cubic fur spans in memory, 1 794 materials, 43 lights
  (the four `lgt_screenLights` link to no receiver and are dropped), ~2:57 to parse
  (see "Where import time goes"), 28.7 GiB peak, and 23.4 s to render at the importer
  defaults (640x360, 128 spp), so the import is 88% of the frame.
  **`docs/alab_profile.md` is the `--profile` of that render**: texture lookups are
  43% of render thread time and tracing 40%, and 72 threads render ~5.4x faster than 8.
  Its first profile (2026-09-27) found the lookups at 89% and 3:20 to render, from
  contention rather than work: a shared counter bumped on every lookup, and a 2-slot
  microcache that missed 25% once a material interleaved ~5 textures, so 72 threads
  rendered only ~1.25x faster than 8 (extrapolated from two `--profile` runs at
  different spp, not a `bench_ab.sh` A/B). Both causes are fixed; see "Streaming
  textures" in `openspec/specs/textures/design.md`. The earlier "~38 s at
  1280x720 / 64 spp" figure predates textured materials. Two download facts come
  first. The **Asset Structure** package ships every
  geometry, camera, layout and light-rig `.usd` under `fragment/` as a 213-byte
  placeholder layer. Without **techvar assets** merged *over* that tree, the stage
  composes to just the fur, at its rig origin. Nothing fails; every placeholder
  prototype just logs "contributed no geometry" (~1 250 of them), and there is no set,
  body, light or shot camera. The techvar zip extracts to `techvar_assets/fragment/…`,
  which nothing references. It has to be merged into `fragment/` (`cp -rlf
  techvar_assets/fragment/. fragment/` hard-links it at no disk cost; the 2 111
  replaced placeholders are in `placeholders_backup.tgz`). What crust still lacks,
  most visible first:
  - **Materials now render, and texture memory is the limit.** Four fixes made
    frame 1004 shade (1 752 `usd_full` preview surfaces, 1 738 of 1 739 UDIM
    sets loaded). With all four in place, only 29 prims are still unbound, all
    camera projection planes and a few set pieces:
    - bindings resolve for the `full` purpose, inherited from ancestors (`bound_material`);
    - `proxy`/`guide` subtrees are pruned (`non_render_purpose`, 1 459 `GEO_PROXY`
      duplicates);
    - unresolvable `<UDIM>` paths anchor on their authoring layer
      (`attribute_asset_path`);
    - single-channel `rgb.R` EXRs decode (`read_exr_rgb`).
    **Every ALab texture is already a `.tx` in all but name**: all 6 832 EXRs are
    64x64-tiled and mip-mapped (OIIO `maketx` output), so they stream straight from
    the source with no conversion — `--auto-tx` tries the source first and leaves such
    a file alone. What stopped that was the streaming reader's channel check
    (`resolve_rgb`), which matched `R`/`G`/`B` by full name and refused the 661
    three-channel sets written as `rgb.R`/`rgb.G`/`rgb.B`; they fell back to
    preloading at `f32`, 34 GiB on one frame. It matches by base name now, as the
    preload reader does. (Preloading everything at the default cap would be ~92 GiB
    against a ~35 GiB import on a 61 GiB machine, which is why this matters.) `occlusion` is
    not read, and the `usd_preview` materials are flat proxies and not worth decoding.
    **One map is missing from the dataset itself**, not mis-resolved:
    `tool_wrench_boxend03`'s `usd_full` connects `roughness` to
    `tool_wrench_boxend03_roughness.<UDIM>.exr`, which ALab does not ship (its folder
    has `ao`, `ior`, `metallic`, `ntu`, `surfaceColor`). The host logs one
    `no tiles found` WARN for it every render, and the wrench shades at the schema's
    roughness 0.5.
  - **The shot camera has to be named.** `entry.usda` carries 29 camera prims — 28
    under `/root/cameras` for the 27 trailer shots (`mk020_0110` has two) plus the
    shot's — and no
    `RenderSettings.camera`, and the first one traversed is trailer camera
    `mk020_0280`. Render the shot with
    `--camera /root/camera01/GEO/renderCam_hrc/renderCam_buffer/renderCam_srt/renderCam`
    (found by passing a bogus `--camera`, whose error lists every camera). Each trailer
    camera also carries a `projectionPlane_M_geo` mesh, which still imports as grey
    geometry; deactivating `/root/cameras` in a wrapper layer (`over "cameras" ( active
    = false )`) removes both.
  - The rig's **13 `CylinderLight`s** (oscilloscope and ham-radio button lights) now
    import as analytic cylinder lights; this used to be a gap.
