# Tasks

## 1. Collector and macros

- [ ] 1.1 Add `crust-core/src/warnings.rs` with: the vocabulary macro (D2), generating `WarningCode`, `as_str`, `kind`, `log_policy`, `doc` and `ALL`, seeded with a few codes; `WarningKind` (`refused` / `approximated` / `skipped`); and `Warning` (code, count, prims, message). Verify that a unit test asserts every code string matches `^[a-z_]+\.[a-z_]+$` and is unique.
- [ ] 1.2 Implement `WarningScope` (`!Send` guard, restores the previous collector on drop) and the `warning!` / `record_warning!` / `cause_warning!` macros, including the `[code] ` prefix, the `Each`/`Once` policy (thread-local flags outside a scope) and the 16-prim cap (D1, D4, D5). Verify with unit tests: count and prim cap, first-fire order, `Once` logs once but counts all, a nested scope restores the outer one, a scope dropped by an early return leaves no stale collector, and `cause_warning!` sets the message without counting.
- [ ] 1.3 Add a debug assertion that the recording macros are not called from a rayon worker while a scope is active, and verify it with a `#[should_panic]` debug-only test.
- [ ] 1.4 Add `warnings: Vec<Warning>` to `Scene`, filled by `load_scene` from its scope (empty for hand-assembled scenes), and re-export the types from `lib.rs`. Verify that `cargo test -p crust-core loads_cornellbox_usda` still passes and that a new test finds an empty `scene.warnings` for the cornell box.

## 2. Vocabulary

- [ ] 2.1 Draft the full code table, one row per cause across every import-time `warn!` in `scene/usd_import/`, `scene.rs`, `color.rs` and crust-assets, with each row's kind per D3's rule, as a reviewable list in the PR description. Get it reviewed before migrating sites. Verify that every import-time `warn!` call site maps to a row (grep count of unmapped sites = 0).
- [ ] 2.2 Write `site/content/docs/reference/warnings.md` (each code, its kind, what it means and what to do), link it from `reference/_index.md`, and add the doc-sync test (D2) comparing the page against `WarningCode::ALL`. Verify the test passes, that it fails when a code is removed from the page, and that `zola build` in `site/` succeeds (Zola 0.21).

## 3. Migrate the import sites

- [ ] 3.1 Migrate `settings.rs`, `products.rs`, `attrs.rs`, `xform.rs`, `mod.rs` (camera, time, instanceable) and `color.rs` to `warning!`. Verify with `cargo test --workspace` and a test stage under `crates/crust-core/tests/` that authors one cause per code here and asserts the records (code, kind, count, prims).
- [ ] 3.2 Migrate `mesh.rs` (replacing `cage_warned`, `legacy_warned` and `ptex_cage_warned` with `Once` codes), `shapes.rs`, `volume.rs` and `instancing.rs`. Verify with a test stage of three meshes displaced at their cage that asserts one log line and `count` 3 with three prims, plus PointInstancer/curves/volume cases asserting their records.
- [ ] 3.3 Migrate `lights.rs`, `light_links.rs`, `materials.rs` and `preview.rs`, including `material.fallback_default` across its four sites. Verify with test stages for a zero-width `RectLight` (`skipped`), a non-finite intensity (`refused`) and two differently broken materials sharing `material.fallback_default`.
- [ ] 3.4 Asset failures (D6): give the core asset helpers the referencing prim and record on every `None`; switch the crust-assets cause lines to `cause_warning!`; code the loader-only causes (`texture.udim_tile_missing`, `texture.tx_stale`, `texture.tx_convert_failed`) and the `NoAssets` defaults (`asset.unsupported_by_host`). Verify with the spec's scenarios: three materials sharing one missing texture give count 3 with three prims and one log line naming the file, and a UDIM set with a missing tile records `texture.udim_tile_missing`.
- [ ] 3.5 Confirm that nothing import-time is left uncoded: grep `scene/usd_import/`, `scene.rs`, `color.rs` and crust-assets for bare `warn!` and verify that only environment / non-import sites remain, each listed in the PR description. Run `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` clean.
- [ ] 3.6 Verify that rendering is unchanged: `scripts/check_images.sh check` against goldens recorded before the change reports no difference, and a callgrind run of `samples/cornellbox.usda -s 2` shows no instruction-count change in the render phase.

## 4. `crust check`

- [ ] 4.1 Add the `crust-check/1` report type beside the other reports (shared opening via `report.rs`, scene counts with `crust-stats/1`'s `scene` keys, `warnings` in the record shape). Verify with a unit test that asserts the key paths (in the style of the existing stats key-path test) and that a non-finite value becomes `null`.
- [ ] 4.2 Add the `check` subcommand: flattened `SceneArgs` with `-i` required, reuse of `load_scene`, the text report on stdout, `--json PATH|-` (the log goes to stderr with `-`), `--deny <kinds>|all`, and exits 0/1/2/3 (D7). Verify with CLI tests: `check -i samples/cornellbox.usda` exits 0 and writes no image; `--json -` parses with `format = crust-check/1`; a stage with a skipped light and `--deny skipped` exits 3 with the report written; `--deny fatal`, `-o out.exr` and a missing `-i` exit 2; a missing file exits 1.
- [ ] 4.3 Document `check` in `site/content/docs/reference/command-line.md` and its JSON in `openspec/specs/cli/design.md` § Machine-readable reports, and add a `check` line to the CLAUDE.md command block. Verify that `zola build` succeeds and that the documented commands run as written.

## 5. Records and integration

- [ ] 5.1 Update `openspec/specs/cli/design.md` § Logging (coded WARN lines, `Once` policy) and add the invariants to `docs/architecture.md` § Invariants: a new import warning is a new code in the table and on the reference page, and import warnings must be raised on the importing thread. Verify the links resolve in `zola build` and that the documents name the doc-sync test.
- [ ] 5.2 End-to-end: run `crust check --json -` on `samples/*.usda` and verify each parses, that two runs give equal `warnings` arrays (determinism), and that every code appearing exists on the reference page.

## Workflow follow-up

- Follow-up changes: warnings in a `crust-render/1` render summary and in the diagnostic report; collecting render-time warnings; an optional `detail` object per record.
- Archive the change after review, syncing `scene-warnings` and the `cli` delta into `openspec/specs/`.
