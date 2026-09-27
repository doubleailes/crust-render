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
`lights`, `materials`, `preview`, `volume`, `camera`, `xform`, `settings`, `attrs` and
`time`; the table in `mod.rs`'s module doc says which reads what. Siblings expose what
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
- **Subdivision surfaces** (`scene/subdiv.rs`, via the pure-Rust
  [`opensubdiv-rs`](https://github.com/doubleailes/OpenSubdiv-rs) port of OpenSubdiv's
  Far/Sdc layers — zero dependencies, `forbid(unsafe_code)`, pinned by git tag). A mesh
  prim authoring `crust:subdivisionLevel` (int, default 0, clamped to 6) is uniformly
  refined that many times and **snapped to the limit surface**, with smooth per-vertex
  shading normals (the kernel's `TriangleMesh.normals`, interpolated by barycentrics).
  Deliberately **opt-in per prim** rather than triggered by `subdivisionScheme`: USD's
  fallback scheme is `catmullClark`, so honouring the scheme alone would subdivide
  virtually every mesh ever authored (all of the Moana island included), and USD has no
  standard per-prim refinement level — Hydra treats refinement as a render setting. The
  scheme still picks the algorithm once a level asks: unauthored/`catmullClark` →
  Catmark, `bilinear` → Bilinear, `loop` → Loop (all-triangle cages only), `none` →
  warn and render the cage. `creaseIndices`/`creaseLengths`/`creaseSharpnesses`
  (per-run or per-edge sharpness, 10 = infinite), `cornerIndices`/`cornerSharpnesses`
  and `interpolateBoundary` are honoured; `holeIndices` and
  `faceVaryingLinearInterpolation` are not. Refinement happens in `mesh_source`,
  *before* interning, so every path (direct bake, deferred instance-vs-bake,
  prototypes) sees it exactly once and `MeshKey` dedupes on the refined arrays. A
  malformed cage or refiner error warns and degrades to the cage. **Ptex keeps
  indexing the base cage**: refined triangles carry explicit corner UVs
  (`FaceMap.uvs`) mapping them back into their cage face's unit square (a synthetic
  face-varying channel refined with linear-everywhere interpolation), and
  `check_face_count` compares the texture against the *authored* face count. Baked
  placements push normals through the inverse transpose (`bake_normals`), matching the
  kernel's instance path exactly — mirrors included. Sample scene:
  `samples/subdivision.usda` (levels 0–3 plus a fully edge-creased cube that stays a
  cube); `CRUST_SUBDIV=0` is the kill switch.
  **Memory, measured** (the refiner retains every level 0..L, a ×4/3 geometric series
  over the last level): `subdivide()` transiently allocates **~313 B per refined face**,
  ~556–592 B when the material needs Ptex sub-face UVs (the synthetic fvar channel is a
  full parallel hierarchy); the returned mesh holds 48 B/face (84 with UVs). Pinned by
  the allocation-counting probe `cargo test -p crust-core --lib
  subdivision_memory_probe -- --ignored --nocapture --test-threads=1` (deterministic
  requested-byte ceilings ~25% above those numbers). End to end
  (`scripts/gen_subdiv_stress.py`, 1.18 M refined quads at level 3, A/B'd with
  `CRUST_SUBDIV=0`): traverse-phase peak +310 MiB — within 6% of the model — and
  kernel-resident memory scaling exactly ×4 per level (34.67 MiB at level 1 → 2.17 GiB
  at level 4). The whole-process peak is **not** opensubdiv: at level 4 traversal peaks
  at 1.16 GiB while the SBVH build over the baked result peaks at 2.19 GiB — the
  pre-existing build transient (see "Known gaps: geometry and acceleration" in
  `openspec/specs/intersection-kernel/design.md`), which subdivision merely
  feeds 4^L× more triangles.
- `UsdGeomBasisCurves` → an instanced `rt::Geometry::RoundCurves` batch: `linear` curves
  directly, `cubic` (bezier | bspline | catmullRom) as one `CubicCurveSegment` per span,
  converted to Bézier control points and subdivided per ray query by the kernel's cubic
  intersector rather than flattened; widths
  (USD diameters) resolve per-vertex / per-curve / constant by array length.
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
    nests to arbitrary depth. A nested *native* instance is skipped (upstream bug, below).
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
  (pinned pixel-identical on the samples). openusd's own xformable composition (the
  `compose_xform_ops` fallback) has no default arm and keeps its historical 0.0. A frame
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
  `crust:varianceThreshold`, `crust:frame`, `crust:samplingStrategy` token = `power` |
  `balance` | `light` | `bsdf`, `crust:lightSelection` token = `uniform` | `power` | `learned`,
  `crust:pixelFilter` token = `box` | `triangle` |
  `gaussian` | `blackman` | `mitchell` + `crust:pixelFilterRadius` float,
  `crust:indirectClamp` float). Missing attrs
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
Measured at 640x360 / 8 spp against the same build preloading, one sequential run
each rather than an interleaved `bench_ab.sh` A/B — the memory and residency figures
are deterministic, but treat the timings as indicative (the `Render` +1.2% is within
noise, and the decode and load deltas are large enough to survive it): Ptex residency **5.98 ->
0.61 GiB** (39 textures streamed, 3 579 preloaded under the size threshold), Ptex decode
**84.8 s -> 14.0 s** so `Load assets` falls 01:40.7 -> 27.3 s, `Traverse prims` RSS
**47.34 -> 41.48 GiB**, peak RSS **51.28 -> 47.08 GiB**, and `Render` costs **+1.2%** —
nothing like the 2.8x the deliberately texture-bound sample scene shows, because the
island is traversal-bound. The whole run finished 69 s sooner. Peak falls by less than
residency does because peak lands at `Commit acceleration structure`, the SBVH build
transient; the figure this feature moves is the traverse RSS.
The cache held **3.28 MiB of a 2 GiB budget with zero evictions**: at this framing the
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
33.92 GiB and peak RSS from 51.5 to 46.2 GiB.

Two costs specific to the full rig: `island.usda` authors *two* `DomeLight`s, and crust
has no per-light camera-visibility, so both light the scene (the sky is doubled) and both
textures decode — `islandsunVIS.png` is 16384x8192 and the pair peaks at ~11 GiB. Dropping
`sky_dome_cam_llc` (`active = false`) is the first lever if memory or exposure matters.

## Known gaps: instancing

- **Instancing caveats.** The kernel nests instances to arbitrary depth (transforms
  compose, normals map back through every level, masks gate per level — pinned by
  `instances_nest`, `nested_instances_compose_transforms_and_normals` and
  `nested_instances_respect_masks_at_each_level` in `crust-rt`), and the importer expands
  a `PointInstancer` inside a prototype into real nested sub-scenes. What is *not*
  supported is a natively-instanced (`instanceable`) prim inside another instance's
  prototype: `openusd` 0.5 cannot read its contents at all (see the upstream bug below),
  so the importer skips it with a warning. Volumes inside prototypes are skipped — they
  live outside the surface BVH by design and cannot ride an instance transform.
  `PointInstancer`
  `velocities` / `accelerations` / `angularVelocities` are ignored, so vectorized instances
  do not motion-blur (`crust:motion:translate` still works on ordinary prims). The
  per-instance arrays themselves (`protoIndices`, `positions`, `orientations`,
  `scales`, `ids`, `invisibleIds`) are evaluated at the requested frame like every
  other attribute, so an animated instancer does move between frames. Top-level `UsdGeomSphere` prims
  still bake their centre into world space and so ignore scale; spheres *inside* a
  prototype go through the instanced path and scale correctly.

## Known gaps: openusd bugs and workarounds

- **`openusd` xformOp bug, worked around locally; fixed upstream in 0.6.0.** `openusd`
  0.5.0 composed multi-op `xformOpOrder` stacks in the wrong order (the authored
  translate came back multiplied by the scale), which used to make
  `samples/cornellbox.usda` render as floating objects against sky. `usd_import/xform.rs`
  therefore composes the individual `xformOp:*` attributes itself
  (`compose_xform_ops`: translate/scale/rotateX·Y·Z/rotate-Euler-triples/orient/
  transform, `!invert!` prefixes, namespaced suffixes), falling back to openusd's
  composition — with a warning — only for op kinds it cannot decode. Regression test:
  `cornellbox_transforms_compose_correctly`.
  **On 0.6.0 the case that motivated it is fixed**: a translate+scale stack composes to the
  authored translation with the scale on the diagonal (`examples/xform_probe`). That is one
  case, not the whole surface — rotations, Euler triples, `orient`, `!invert!` prefixes and
  namespaced suffixes are unverified — so the local composer stays authoritative and the
  fallback stays in place. Retiring either means checking those kinds first.

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

- **Nested native instances are still skipped, but no longer have to be.** An
  `instanceable` prim *inside another instance's prototype* could not be read on
  `openusd` 0.5.0: resolving its prototype, or reading the type name of anything beneath
  it, tripped a `debug_assert!` in `pcp/instancing.rs::materialize_prototype` (debug builds
  aborted, release had it compiled out). So `collect_proto_parts` tests `is_instance()`
  **before** any schema lookup — a schema `get()` reads the type name, which is what
  aborted — and skips such prims with a warning. Regression test:
  `nested_native_instance_degrades_gracefully`.
  **On 0.6.0 that abort is gone**: a debug build resolves the nested prototype to a valid
  prim with its geometry (checked with `examples/proto_probe` on a four-line stage). The
  skip arm is therefore now conservative rather than necessary, and deleting it would
  recover this geometry — splice the inner prototype's parts in with composed transforms,
  since a native instance is a single placement and needs no extra level of kernel
  indirection. Not done yet, and it costs the Moana island nothing (that arm never fires
  there), so it is a correctness improvement for other stages rather than a fix for this
  one. The regression test would need rewriting to assert the geometry arrives instead of
  that it is skipped.

## Known gaps: ALab

- **ALab gaps** (Netflix Animation Studios' ALab 2.2, `samples/ALab/`, gitignored).
  Shot mk020_0281, frames 1004–1057. `entry.usda` sublayers the baked procedurals
  (fur and cloth as value-clipped `BasisCurves`), the trailer cameras and the shot. It
  imports and animates under `-f`. Frame 1004, measured 2026-09-27: 13 302 geometries,
  21.2 M triangles plus 9.4 M cubic fur spans in memory, 1 794 materials, 47 lights,
  ~3:00 to parse (see "Where import time goes"), 33.2 GiB peak, and 3:20 to render at
  the importer defaults (640x360, 128 spp). **`docs/alab_profile.md` is the
  `--profile` of that render**: streamed texture lookups take 89% of render thread
  time, and the cause was contention, not work. A shared counter was bumped on every
  lookup, and the 2-slot microcache missed 25% once a material interleaved ~5
  textures, so 72 threads rendered an estimated ~1.25x faster than 8 (extrapolated
  from two `--profile` runs at different spp, not a `bench_ab.sh` A/B). Both causes
  are fixed since; see "Streaming textures" in `openspec/specs/textures/design.md`. The earlier "~38 s at
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
