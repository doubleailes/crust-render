## Why

`crust-core` is about 70 000 lines holding four different things: the USD translator
(`scene/usd_import/`, 17k lines, all 20 files on `openusd`), the engine (integrator,
materials, lights, media, guiding), tools built on the engine (`diagnostic/`, 6.7k
lines, with its `serde` report), and the glue between translator and engine
(`live.rs`). This layout blocks two of the project's goals:

1. **A Hydra render delegate (goal 2).** Hydra hands a renderer plain data: mesh
   topology, primvar arrays, transforms, material networks, lights, and later
   dirty-bit updates. It never hands it a `UsdPrim`. Today the engine can only be fed
   *through* the USD import: `emit_mesh` takes `&Prim` and reads the prim,
   tessellates, displaces and decides bake-or-instance in one pass. Import state is
   also ambient to the thread (the time code in `EVAL_TIME`, the warning collector
   in `COLLECTOR`), so the import cannot run on several threads, which is how Hydra
   syncs prims.
2. **Compile time (goal 3).** Every engine edit recompiles the translator and the
   diagnostic, and the integrator is compiled against `openusd`.

**Performance (goal 1) is the constraint on all of it.** This change moves code; it
does not touch the render loop. Its acceptance bar: no measurable wall-clock
regression in rendering, and none in import time or peak import memory.

## What Changes

Three steps, each one landable and verifiable on its own:

- **Step 1: `crust-diagnostic` crate.** `crust-core/src/diagnostic/` moves to a new
  crate `crates/crust-diagnostic`, which depends on `crust-core`. The engine keeps the
  measurement hook (`Renderer::render_measured`, `Instruments`, `Measured`), which
  becomes public along with `tracer::mean_relative_error`, `lpe::validate` and the
  memory probes it already uses. `crust-render` calls `crust_diagnostic::run`.
  `crust-core` stops depending on `serde` / `serde_json` only if no other module
  still uses them (`stats.rs`, `check.rs`, `compare.rs`, `report.rs` and others do
  today, so they stay; see design D1).
- **Step 2: an explicit `ImportContext`.** No import state is tied to the importing
  thread any more. The evaluation time is a value carried by the import's context,
  not the `EVAL_TIME` thread-local. Warnings are collected per scope, and a scope can
  be opened on any thread and merged back in a deterministic order, so a warning
  raised on a rayon worker is recorded instead of panicking in debug. The import
  itself stays single-threaded in this change; making it parallel is a later change
  this one enables.
- **Step 3: a scene-building API in `crust-core`; the USD translator moves to
  `crust-usd`.** `crust-core` gains a builder that takes plain data (meshes as point
  / face arrays with their subdivision scheme and displacement, instances of
  prototypes by handle, lights, cameras, materials as `crust-mtlx` documents or
  OpenPBR / UsdPreviewSurface parameters, render settings). Tessellation,
  subdivision, displacement, the bake-or-instance decision and the BVH commit stay
  in the engine behind that builder, because a Hydra delegate needs them too.
  `scene/usd_import/` and the USD side of `live` move to a new crate
  `crates/crust-usd`, which reads the stage and calls the builder. `crust-core` no
  longer depends on `openusd` / `openusd-schemas`. `Scene::from_usd*` and
  `Scene::list_usd` move to `crust_usd`.
- **Bit-identity.** Every checked-in sample renders bit-identical before and after
  each step (`scripts/check_images.sh`). That includes the in-place ↔ full re-import
  pair the MCP session pins.
- **No observable behaviour changes.** The CLI, its flags, its outputs, the
  `crust-check/2`, `crust-stats/1` and `crust-diagnostic/1` formats, the warnings and
  the MCP tools all stay the same.

## Capabilities

### New Capabilities

- `scene-building`: building a renderable scene from plain data, with no scene
  format involved. It is the contract a Hydra delegate and the USD translator both
  build against.

### Modified Capabilities

None in behaviour. The design records move with their code:
`openspec/specs/diagnostics/design.md` and
`openspec/specs/usd-scene-import/design.md` name the new crates.

## Impact

- **Crates:** seven become nine (`crust-diagnostic`, `crust-usd`). Updates follow in
  `CLAUDE.md` § Workspace, `docs/architecture.md` (crate graph, ownership table,
  seams, module map), `openspec/config.yaml` and
  `site/content/docs/architecture/overview.md`.
- **Tests:** the integration tests in `crust-core/tests/` that load USD
  (`usd_*.rs`, `live.rs`, `lights.rs`, `aovs.rs` and most others) move to
  `crust-usd/tests/`. `crust-core` cannot dev-depend on `crust-usd` without getting
  two copies of its own types in the test build. `crust-core/tests/diagnostic.rs`
  moves to `crust-diagnostic`. The criterion bench `integrator` and the
  `crust-render` examples are updated to the new paths.
- **Public API:** `crust_core::Scene::from_usd*`, `crust_core::diagnostic::*` and
  the `usd_import` re-exports change paths. Every caller is in this workspace.
- **Ordering with active changes:** `live-stage-scene-deltas` still has open tasks
  and edits `live.rs` / `usd_import/live.rs`; step 3 lands after it, or is rebased
  over it. `neural-visibility-light-selection` also adds a crate, so its crate count
  needs reconciling.
- **Performance:** render-time code is not edited. Fat LTO with one codegen unit
  (`[profile.release]`) inlines across crates as it does within one, so a crate
  boundary alone moves no hot code. The risk is at import (step 3's plain-data
  types), and the design forbids copies there (D5).
- **Compile time:** the gain is in `cargo check`, `clippy`, tests and incremental
  dev builds. A `--release` build stays dominated by its fat-LTO link.

**Out of scope:** the Hydra delegate itself (C++ `HdRenderDelegate`, FFI, `unsafe`:
a project decision of its own), a parallel USD import, and any restructuring of
`tracer/path.rs` / `trace_path`.
