# Tasks

Starts after `structured-warnings` task group 1 has landed (`Scene::warnings` exists).

## 1. Share the diagnostic's pieces

- [ ] 1.1 Split `diagnostic::effective()` into the part that depends on `RenderSettings` plus the shared scene flags, and the diagnostic-only rows. Make `Setting` and `SceneInfo` reachable from outside `diagnostic` with unchanged serde names (D3). Verify that `cargo test -p crust-core diagnostic` passes, with the snapshot tests unchanged.
- [ ] 1.2 Add `Facts::from_import(&Scene, auto_tx, import_peak)`, which leaves every baseline fact `None`. Reword the finding summaries that mention trials so they read correctly in both reports. Verify with a test that, for a scene with textures without `.tx` and more than 8 uniformly selected lights, `checks::run(&Facts::from_import(..))` returns exactly `textures_without_tx` and `many_lights_uniform`, with the same evidence and actions the diagnostic's facts produce. Review any snapshot diff as text-only (ids, evidence and actions unchanged).

## 2. The report

- [ ] 2.1 Add the `crust-check/1` report type in crust-core: `scene`, `products`, `effective_settings`, `import`, `counts`, `findings`, `warnings`, `denied`, in that order, through `report.rs` (D4), with `import` and `counts` reusing `crust-stats/1`'s phase and `scene` serialisers. Verify with a unit test that asserts the key order and key paths (in the style of the stats key-path test) and that a non-finite or unavailable number serialises as `null`.
- [ ] 2.2 Factor the default beauty output path out of `render` into one function, and resolve products for `check` through `refuse_shared_paths` and `product_channels` (D2). Verify with a test that a stage with `color`, `albedo` and `normal` vars lists one product with all their channels, and that a stage with no products lists the default beauty path `render` writes.

## 3. The subcommand

- [ ] 3.1 Add `Command::Check` with flattened `SceneArgs`, `-i` required, `--json PATH|-` and `--deny` as a `ValueEnum` list (D1, D5). Verify with CLI tests: `crust check` and `--deny fatal` and `-o out.exr` exit 2; `crust check -i missing.usda` exits 1 and writes no report.
- [ ] 3.2 Build the report from `load_scene`'s `Scene`, write the JSON via `write_json` / `is_stdout`, and decide the exit status after writing (D5, D6). Verify with CLI tests:
  - `crust check -i samples/cornellbox.usda --json -` exits 0, stdout parses with `format = crust-check/1`, and the log is on stderr;
  - no EXR or PNG appears in the working directory;
  - the light count equals the `scene.lights` from `render --stats-json -` on the same stage;
  - a test stage with two zero-width `RectLight`s and `--deny skipped` exits 3 with `denied = ["light.degenerate_shape"]`;
  - `--deny refused` on that stage exits 0;
  - a stage whose only issue is a `textures_without_tx` finding exits 0 with `--deny all`.
- [ ] 3.3 Write the text report in the spec's section order, ending with the closing totals line. Verify with a CLI test that a clean stage prints "no findings" and "no warnings", and that with `--json check.json` the text still goes to stdout while the file parses.

## 4. Documentation

- [ ] 4.1 Document `crust check` (flags, sections, `--deny`, exit codes) in `site/content/docs/reference/command-line.md`, add a "check before you render" section to `site/content/docs/help/diagnosing-a-render.md`, and add `check` to the CLAUDE.md command block. Verify that `zola build` in `site/` succeeds (Zola 0.21) and that each documented command runs as written on `samples/cornellbox.usda`.
- [ ] 4.2 Add the cookbook entry and the `crust-check/1` key list to `openspec/specs/cli/design.md` (§ Command cookbook, § Machine-readable reports), and record the `refuse_shared_paths` known gap. Verify that the key list matches the 2.1 test's expected key paths.

## 5. Integration

- [ ] 5.1 Run `crust check --json -` on every `samples/*.usda`. Verify that each parses, that two runs give equal `warnings` and `findings`, and that the `findings` for each sample are the import-only subset of `crust diagnostic --budget 10s` on it. Then run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` clean.

## Workflow follow-up

- Follow-ups: denying on findings; a check-only import that skips BVH builds (measured); coded CLI-side warnings for `refuse_shared_paths`; multi-frame checks.
- Archive after `structured-warnings` is archived, syncing `scene-check` and the `cli` delta into `openspec/specs/`.
