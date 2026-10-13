Every step ends with the performance gate (design D0). "Gate" below means: images
bit-identical (`check_images.sh check`), the hot-function callgrind counts equal the
baseline's, or `bench_ab.sh` shows no scene's min regressing when they are not, and,
where noted, `--stats` import time and peak memory no worse. Record the numbers in
`notes.md` beside this file.

## 0. Baseline

- [ ] 0.1 Build the baseline binary from the commit the step starts from and keep it
      (`target/baseline/crust`). Record goldens with it
      (`scripts/check_images.sh record`, which pins 16 spp; add `--indirect-clamp 0`
      on both sides).
- [ ] 0.2 Record the baseline callgrind inclusive counts for `Bvh::hit`,
      `render_pixel`, `trace_path`, `scatter_resolved`, `eval_all` and `pdf_all` on
      `cornellbox.usda` and `materialx_basic.usda` at `-s 2`, `RAYON_NUM_THREADS=1`.
- [ ] 0.3 Record `--stats` import phase times and peak memory for the largest scenes
      available (ALab / Moana if present; otherwise name the samples used and why).

## 1. `crust-diagnostic` crate

- [ ] 1.1 Make the hooks the diagnostic uses public: `Renderer::render_measured`,
      `Instruments`, `Measured`, `ClampMeasure`, `tracer::mean_relative_error`,
      `tracer::GUIDING_PASS_SEED_STEP`, `lpe::validate`, `peak_memory_bytes`,
      `machine_memory_bytes`. Done when `crust-core` builds with no other change.
- [ ] 1.2 Create `crates/crust-diagnostic` (`forbid(unsafe_code)`, workspace lints),
      move `crust-core/src/diagnostic/` into it, and rewrite `crate::` paths to
      `crust_core::`. Move `crust-core/tests/diagnostic.rs` and the
      `diagnostic::tests::fixture` helper with it.
- [ ] 1.3 Point `crust-render` (`main.rs`: `checks`, `Facts`, `SceneInfo`,
      `SceneFlags`, `effective_settings`, `Options`, `run`) and the MCP session at
      `crust_diagnostic`. Fix the doc links in `tracer/mod.rs` and `tracer/path.rs`
      that name `crate::diagnostic`.
- [ ] 1.4 Gate. Also: `crust diagnostic --budget 30s` on `cornellbox.usda` gives a
      `crust-diagnostic.json` structurally equal to the baseline's (the same keys,
      verdicts and suggestions; timings excepted).
- [ ] 1.5 Docs: `CLAUDE.md` § Workspace, `docs/architecture.md` (crate graph,
      ownership table, module map), `openspec/config.yaml`,
      `site/content/docs/architecture/overview.md`, and
      `openspec/specs/diagnostics/design.md` naming the crate. Run `zola build`.

## 2. `ImportContext`

- [ ] 2.1 Introduce `ImportContext` in `scene/usd_import/mod.rs`, built by
      `load_scene` and holding the evaluation time. Pass `&ImportContext` (or a
      `TimeCode`) to every helper that reads an attribute, replacing each
      `EVAL_TIME` read. Delete `EVAL_TIME` and `EvalTimeScope` (`time.rs`). Done
      when nothing in the crate references either name and the time-sampled tests
      (`usd_scene.rs` frame tests, motion blur, `camera_exposure.rs`) pass.
- [ ] 2.2 Move the import-wide inputs that are currently passed separately into the
      context where it simplifies signatures (the `ImportCaches` epoch, the asset
      loader). Do not touch render-time code.
- [ ] 2.3 Warnings (`warnings.rs`): let a `WarningScope` be opened on any thread and
      returned by `finish`, and add a merge into a parent collector keyed by
      traversal order. Keep the macros' syntax and the `crust-assets` call sites as
      they are. Keep the debug panic for a recording outside any scope during an
      import; add a test that a recording inside a worker's own scope is merged at
      its traversal position.
- [ ] 2.4 Gate, including import time and memory (0.3), and the `crust check --json`
      output of every sample equal to the baseline's.

## 3. Scene-building API, then `crust-usd`

- [ ] 3.1 Survey before designing types: list every field `emit_mesh`,
      `mesh_source`, `flush_meshes`, the instancing code, `emit_light` and the
      material resolution read from the stage. Find where `openusd::gf::Vec3f` points
      are converted to the engine's storage type, and what `Vec3f` is. Write the
      `MeshDesc` / `LightDesc` / `MaterialDesc` / `InstanceDesc` field lists into
      `notes.md` and settle design D5's element type there.
- [ ] 3.2 Replace `openusd::gf::Vec3f` in `scene/subdiv/` with the type chosen in
      3.1. Gate (images and import memory).
- [ ] 3.3 Add `crust_core::build` (`SceneBuild` and the descriptors) wrapping
      `WorldBuilder`, `MeshArena`, the light list, the material table and
      `Provenance`, with source keys. Arrays are moved in, never cloned (D5). No
      caller yet; unit tests build each descriptor kind by hand.
- [ ] 3.4 Re-cut `usd_import` onto the builder **in place, still inside
      `crust-core`**: each `emit_*` reads the prim into a descriptor and hands it
      over; the build work moves behind `SceneBuild`. The USD side of `live` calls
      the same functions. Gate, with import time and peak memory as hard criteria.
      The in-place ↔ full re-import tests (`live.rs`, MCP session tests) must pass
      unchanged.
- [ ] 3.5 Pure move: create `crates/crust-usd` and move `scene/usd_import/`,
      `Scene::from_usd*`, `Scene::list_usd`, `UsdImportOptions`, the openusd-facing
      parts of `error.rs` and `material/materialx.rs`'s UsdShade loading. Drop
      `openusd` / `openusd-schemas` from `crust-core/Cargo.toml`. Done when
      `cargo tree -p crust-core` lists neither.
- [ ] 3.6 Tests: move every `crust-core/tests/*.rs` that loads a stage to
      `crust-usd/tests/`, with `tests/data` as needed. Add a `crust-core` test that
      builds the scene-building spec's "host with no USD" scene. Update the
      `integrator` bench and the `crust-render` examples. Done when `cargo test
      --workspace` runs the same number of tests as before plus the new ones.
- [ ] 3.7 Gate (full D0: images, callgrind, `bench_ab.sh` over the bench set, import
      time and memory). Also check `cargo check` / `cargo test` incremental times
      after touching a file in `tracer/`, before and after, and record them as the
      compile-time result.
- [ ] 3.8 Docs: `CLAUDE.md` (workspace, the import rules that now name `crust-usd`,
      the `stage_builder` path), `docs/architecture.md` (crate graph, seams: add
      `SceneBuild`; a render end to end; module maps), `openspec/config.yaml`,
      `site/content/docs/architecture/`, and `openspec/specs/usd-scene-import/design.md`.
      Sync the `scene-building` and `scene-warnings` spec deltas.
