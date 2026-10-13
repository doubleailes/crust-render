## Context

The project's goals, in order: **performance**, then **a Hydra render delegate**, then
**compile time**. The first is a constraint on everything else: no refactor may make
a render slower. Wall-clock time is the metric that counts. Instruction counts serve
only as a cheap way to *prove* nothing changed (D0).

What the code looks like today (2026-10-13):

```
crust-core (~70k lines)
├─ scene/usd_import/   17k   every file uses openusd; reading prims and building
│                            engine data are interleaved (emit_mesh(&Prim, ...))
├─ scene/subdiv/, scene/displace.rs   engine work; subdiv uses openusd only for gf::Vec3f
├─ live.rs + usd_import/live.rs       in-place MCP updates (Signature, Update, Provenance)
├─ diagnostic/          6.7k  uses Renderer, Scene, Buffer, PixelRect, SamplingStrategy,
│                             LightSelection, tracer::{Instruments, Measured,
│                             mean_relative_error}, lpe::validate, profile, memory probes.
│                             Nothing in the engine calls into it.
├─ tracer/, material/, light/, ...    the render loop: untouched by this change
└─ stats, report, check, compare      reporting shared by several hosts
```

The import's ambient state:

- `usd_import/time.rs`: `EVAL_TIME`, a thread-local read by ~40 attribute-read
  sites, scoped by the `!Send` `EvalTimeScope`.
- `warnings.rs`: `COLLECTOR`, a thread-local scoped by a `!Send` guard. A recording
  macro on a rayon worker during an import panics in debug builds
  (`recording_from_a_rayon_worker_during_an_import_panics`). The macro is also used
  by `crust-assets` (texture and tile caches) and by `crust-render`.
- `material/materialx.rs` `SLOTS` and `material/closure` are render-time scratch
  pools. They are per-thread by design and **out of scope**.

The release profile is `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`.

## Goals / Non-Goals

**Goals**

- The engine can be fed plain data by a translator other than USD (a Hydra delegate)
  without going through `openusd`.
- The engine compiles without `openusd`, `openusd-schemas` or the diagnostic.
- No import state is tied to the importing thread.
- Bit-identical images and no measurable render, import-time or import-memory
  regression, shown at every step.

**Non-Goals**

- The Hydra delegate itself (FFI, `unsafe`, Pixar USD as a build dependency: a
  project decision of its own).
- A parallel USD import. This change makes one possible; it does not build it.
- Any change to `tracer/`, `trace_path`, materials' `resolve` / `eval`, lights'
  sampling or the kernel.
- Faster `--release` builds. Fat LTO dominates them whatever the crate layout.

## Decisions

### D0. The performance gate, applied to every step

Each step's PR records, against a baseline binary built from the commit it starts from:

1. **Images:** `scripts/check_images.sh check` over goldens recorded from the
   baseline, at 16 spp with `--indirect-clamp 0`. It must report zero changed pixels.
2. **Codegen proof (cheap, preferred):** callgrind on `cornellbox.usda` and
   `materialx_basic.usda` at `-s 2`, `RAYON_NUM_THREADS=1`. The inclusive count of
   each hot function (`Bvh::hit`, `render_pixel`, `trace_path`, `scatter_resolved`,
   `eval_all`, `pdf_all`) must match the baseline exactly, as the 2026-09-27 pass
   showed. Identical generated code means identical speed, so step 1 and step 2's
   render side stop here.
3. **Wall clock (when 2 differs, and always for step 3):** `scripts/bench_ab.sh -a
   <baseline> -b <candidate>` across the `bench_scenes.sh` set. Report min and mean.
   The bar is "no scene's min regresses beyond the run-to-run spread". A suspected
   regression is chased with callgrind to a cause, not argued away as noise.
4. **Import (step 3, and step 2 where it touches import code):** `--stats` import
   phase times and peak memory on the largest scenes available. ALab and Moana when
   present; otherwise the largest checked-in samples. The bench set is extended if
   none of them stresses the mesh path.

Why not wall-clock alone: its run-to-run spread reaches 15%, so a 2% regression is
invisible to it. Why not callgrind alone: it cannot see cache behaviour, and step 3
changes how import data is laid out.

### D1. Step 1: `crust-diagnostic` takes `diagnostic/` and nothing else

The crate takes the whole `diagnostic/` directory (run, report, markdown, noise,
crops, schedule, trials, checks, compare) and its test. The engine keeps
`Renderer::render_measured`, `Instruments`, `Measured` and `ClampMeasure`. Those are
render-time code, and the clamp counter lives inside `trace_path`. They change
visibility from `pub(crate)` to `pub`, and so do `tracer::mean_relative_error`,
`tracer::GUIDING_PASS_SEED_STEP`, `lpe::validate`, `peak_memory_bytes` and
`machine_memory_bytes`. Visibility does not change codegen under LTO.

`check.rs`, `compare.rs`, `report.rs` and `stats.rs` stay in the engine for now.
`report.rs` is the JSON envelope that `stats` (`crust-stats/1`), `check`
(`crust-check/2`) and the diagnostic all share, and `crust diff` / `crust check` are
not diagnostic features. So `serde` / `serde_json` stay `crust-core` dependencies.
Moving the reporting group out is listed under open questions, not done here.

The compile-time gain is the 6.7k lines that no longer rebuild on every engine edit.

*Alternative rejected:* a `diagnostic` cargo feature on `crust-core`. It hides the
code but keeps the coupling, and a feature unifies across the workspace, so it would
be on in every build that includes `crust-render`.

### D2. Step 2: the evaluation time is a value, not a thread-local

`load_scene` builds an `ImportContext { time: Option<f64>, … }` and every read site
receives it. The ~40 sites today read `EVAL_TIME` from helpers that take only a
`Prim` or `Attribute`. Those helpers gain a `&ImportContext` (or a `TimeCode`)
parameter. `EvalTimeScope` and `EVAL_TIME` are deleted. The comment at
`time.rs:7` names this exact change as the condition for reading attributes off the
importing thread.

The context is the one place import-wide inputs live. It holds the time, the
`ImportCaches` epoch, the asset loader and the warning scope handle (D3). It is
passed by shared reference, so it costs nothing at a call site beyond the argument.

*Alternative rejected:* keeping the thread-local and re-entering it on each worker.
It works for one value, but every future ambient input would repeat the pattern, and
a Hydra sync calls into the engine from threads the engine did not create.

### D3. Step 2: warnings are scoped and mergeable, not thread-bound

The `crust_core::warning!` / `cause_warning!` macros keep their call syntax, because
`crust-assets` and `crust-render` use them and the `AssetLoader` seam passes no
context. What changes is the collector:

- A `WarningScope` can be opened on **any** thread, rayon workers included. It owns
  a `Collector` and returns it on `finish`.
- `Collector::merge(other, order_key)` folds a worker's records into the parent in a
  deterministic order (by the prim's traversal index), so `Scene::warnings` is the
  same whichever thread raised what. "One record per code, in the order first fired"
  is kept by defining the order on the traversal key, not on wall-clock arrival.
- The debug panic for a rayon recording *outside* any scope during an import stays.
  A recording inside a worker's own scope is legal.

For the single-threaded import of this change, every warning is still raised in one
scope and in today's order, so `Scene::warnings` is unchanged (pinned by the
existing warning tests and the `crust check` JSON of every sample).

### D4. Step 3: the builder takes plain data, in the vocabulary the engine already has

A new module `crust_core::build` exposes a `SceneBuild`. It wraps today's
`WorldBuilder`, the light list under construction, the material table, `MeshArena`
and the provenance table. It accepts:

| descriptor | carries | notes |
|---|---|---|
| `MeshDesc` | points, face counts, face indices, optional normals, `UvSource`, subdivision scheme and level, displacement, motion samples of the transform, `MaterialId`, ray-visibility flags, Ptex face ids | everything `mesh_source` / `emit_mesh` compute from the prim today |
| `PrototypeDesc` / `InstanceDesc` | a prototype handle and per-instance transforms | handles, not prim paths. Epoch scoping stays a USD-side concern in `crust-usd` |
| `LightDesc` | the `LightKind` inputs after UsdLux unit conversion | UsdLux units (`lux.rs`) stay in the engine; Hydra lights carry the same inputs |
| `MaterialDesc` | a `crust-mtlx` document / node graph, `OpenPBR` parameters or `PreviewSurface` parameters | `crust-mtlx`'s types are adopted, not wrapped (CLAUDE.md rule) |
| `CameraDesc`, `RenderSettings`, `AovRequest`, volumes | as today | already plain data |

Each entry carries an optional source key (`Arc<str>`, the prim path for USD, the
`SdfPath` for Hydra), which feeds `Provenance` for in-place updates.

The engine keeps every piece of work a second translator would also need:
subdivision (`scene/subdiv/`, with `gf::Vec3f` replaced), displacement, the
bake-or-instance decision in `flush_meshes`, the BVH commit, light selection, and
`live`'s `Signature` / `Update::apply` / `World::check_materials` /
`replace_materials`. `crust-usd` keeps what only USD needs: stage opening and the
`stage_builder` schema registry, chunked masked-stage streaming, traversal and
pruning, attribute reading at a time code, UsdShade → `MaterialDesc`, light links
from collections, the RenderProduct resolution, `listing.rs`, and the USD side of
`live` (the layer diff's rebuild through `read_index`, `resolve_fresh`,
`emit_light`). Those keep calling the same functions, now through the builder.

The in-place ↔ full re-import bit-identity pair holds by construction as before:
both paths go through the same `crust-usd` functions into the same builder calls.

*Alternative rejected:* a trait `SceneSource` that the engine pulls from. Pulling
makes the engine own traversal order. Streaming chunks, prototype epochs and Hydra's
push-style sync all want the translator to own it.

### D5. Step 3: descriptors move data, never copy it

The streaming import exists to keep Moana's peak memory down. A descriptor that
copies a mesh's arrays doubles that mesh's footprint at its peak. So:

- Every array field is an owned `Vec` the translator *moves* in, or a borrowed slice
  for the duration of the call. Never a clone.
- Points cross in the element type the import already reads. If `openusd::gf::Vec3f`
  is not the type `subdiv` / `MeshArena` end up storing, the conversion happens
  exactly where the import converts today, not in an extra pass. Task 3.1 locates
  that point before the type is chosen.
- Peak import memory is a D0 gate for this step, not an afterthought.

## Risks / Trade-offs

- **Step 3 is wide.** ~17k lines move and the mesh path is re-cut at a new seam. To
  contain it: land the builder inside `crust-core` first, with `usd_import` calling
  it in place (tasks 3.2–3.4), gated by D0. Only then move the directory to
  `crust-usd` (3.5), which is a pure move.
- **Test migration.** Most of `crust-core/tests/` loads USD, so it moves to
  `crust-usd/tests/`. A dev-dependency from `crust-core` on `crust-usd` would build
  two `crust-core`s in the test binary and make the types mismatch. The engine keeps
  the tests that build scenes by hand. The builder's own tests (3.6) build every
  descriptor without USD, which is the coverage a Hydra delegate will rely on.
- **Collisions with active changes.** `live-stage-scene-deltas` has open tasks in
  `live.rs`. Step 3 waits for it or rebases over it. Steps 1 and 2 do not touch it.
- **Import-time regression from the extra seam.** The builder adds a call boundary
  per prim, which is noise next to an attribute read. Fat LTO inlines it either way.
  D0.4 measures it regardless.
- **Warning order (D3).** Defining "first fired" by traversal key equals today's
  arrival order only while the import is single-threaded. The parallel import that
  comes later inherits a definition that already holds.

## Migration Plan

Three PRs in order: step 1, step 2, step 3, each passing D0 and CI on its own.
Step 3 may be split further at the 3.4 / 3.5 boundary. Rollback is a revert per PR;
no on-disk format or CLI surface changes.

## Open Questions

- Should `report.rs`, `stats.rs`, `check.rs` and `compare.rs` leave the engine too
  (a `crust-report` crate), so the engine sheds `serde`? `RenderStats` is filled
  inside the engine, so it would need a split between counters and the JSON
  formatting.
- Is `openusd::gf::Vec3f` a `glam` type, a `[f32; 3]` newtype or something else?
  It decides whether D5's handover is free or needs one conversion pass (task 3.1).
- Will the Hydra delegate's material path hand `crust-mtlx` a parsed document or an
  `HdMaterialNetwork`-shaped graph? If the latter, `MaterialDesc` may want a
  graph-builder entry point in `crust-mtlx` rather than a document.
