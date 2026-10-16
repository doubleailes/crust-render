# Tasks

## 1. Strict binding resolution upstream (design D5)

- [ ] 1.1 Open a PR on openusd (`openusd-schemas`, `shade/binding.rs`) exposing the
  private `bound_material_for_single_purpose` as a public `MaterialBindingAPI` method
  (per-purpose, no all-purpose fallback), with a test mirroring
  `purpose_binding_with_strength`; verify with `cargo test -p openusd-schemas` in the
  openusd checkout
- [ ] 1.2 Once merged, move the workspace to that openusd `main` commit
  (`cargo update -p openusd -p openusd-schemas`); verify `cargo test -p crust-core`
  passes and `cargo deny --locked check` is clean. If it has not landed when group 3
  starts, take the fallback in design Risks (lists limited to `[p]`, `[p, all]`,
  `[all]`) and record it as a known gap in group 3's docs

## 2. Purposes from the render settings (crust-core)

- [ ] 2.1 Add `purposes`, `binding_purposes` and `payloads` to `UsdImportOptions` (all
  defaulting to today's behaviour), with the `PurposeSet`, `BindingPurpose` and
  `LoadSet` types (design D2); verify `cargo build --workspace` and that every existing
  `UsdImportOptions { .. }` site still compiles unchanged through `Default`
- [ ] 2.2 Read `includedPurposes` and `materialBindingPurposes` off the settings prim in
  `read_index`, apply the overrides, and record the effective values in `StageFacts`;
  raise `settings.unknown_purpose` (new `WarningCode`, kind `refused`) for unknown
  tokens. Add its row to `site/content/docs/reference/warnings.md`, and verify the test
  that keeps the code list and the page in step passes
- [ ] 2.3 Thread a `PurposeFilter` through `prune_reason` and all four walks
  (`traverse_into`, `count_placements`, `prototype_prunes`, `listing.rs` with the final
  filter). Verify with new tests in `crust-core/tests/usd_scene.rs` covering:
  - the default stage (`GEO` drawn, `GEO_PROXY` not);
  - `--purposes proxy`;
  - `default` always drawn;
  - an ancestor's purpose winning;
  - an unknown token warning.
- [ ] 2.4 Make an in-place settings update refuse a change of the effective purposes or
  binding purposes (`crust_core::live`, `Structural`); verify with a test in
  `crust-core/tests/live.rs` that an `includedPurposes` edit is refused in place and a
  full re-import shows the proxies
- [ ] 2.5 Verify the default import is unchanged:
  - `scripts/check_images.sh check` against goldens recorded before the change reports
    no difference on every sample;
  - callgrind on `cornellbox.usda -s 2` (`RAYON_NUM_THREADS=1`) shows the instruction
    count of `prune_reason` and the total within noise; record the numbers in the PR.
- [ ] 2.6 Document purposes: the `purpose` row in `site/content/docs/usd/geometry.md`,
  `includedPurposes` in `site/content/docs/usd/render-settings.md`; verify `zola build`
  (0.21) in `site/` succeeds

## 3. Binding purposes as an ordered list (crust-core)

- [ ] 3.1 Resolve `bound_material` over the effective binding-purpose list, in order,
  with `""`, `allPurpose` and `all` as the all-purpose binding (design D5). Verify with
  tests in `crust-core/tests/usd_scene.rs` beside
  `bindings_inherit_from_ancestors_and_prefer_the_full_purpose`, covering:
  - the fallback order;
  - `preview,all`;
  - the all-purpose fallback;
  - no listed purpose binding the prim (grey);
  - Houdini's `allPurpose` with no warning;
  - `["full", "preview", ""]` (or the fallback's warning, per 1.2).
- [ ] 3.2 Check that `crust-core/tests/resolve.rs` still passes unchanged: resolution
  per material does not depend on which binding chose it
- [ ] 3.3 Document `materialBindingPurposes` in
  `site/content/docs/usd/render-settings.md` and the binding order in
  `site/content/docs/usd/materials.md`; verify `zola build` succeeds

## 4. The payload load set (crust-core)

- [ ] 4.1 Add a test fixture under `crates/crust-core/tests/data/payloads/` with:
  - two payloaded component assets (render `GEO` inside the payload, `GEO_PROXY` and
    `extentsHint` outside it, `full` and `preview` bindings);
  - a nested payload inside one of them;
  - a light inside a payload;
  - a shot camera brought in through a payload;
  - a referenced asset with no payload;
  - two instanceable placements of one payloaded asset.

  Verify it opens in `crust check` with no warning.
- [ ] 4.2 Apply the load set in `open_stage`: the `LoadAll` path is untouched for the
  default set; otherwise open with `LoadNone` and call `set_load_rules` before the first
  query (design D3). Pass the set from every caller (single stage, chunks, the camera
  projection stage, `live.rs` rebuilds; the default set from `listing.rs`). Verify with
  tests for the spec's load scenarios: nothing loaded, one asset, a nested payload,
  everything but one, loading inside an unloaded asset, order independence, a light
  inside an unloaded payload.
- [ ] 4.3 Add `load_without_descendants(camera)` for a named camera, and the error for a
  `--load` / `--unload` prim that does not compose (new `crate::Error` variant naming the
  prim). Verify with tests: the payloaded shot camera renders with `--unloaded` and no
  other payload loads; `/World/nothing` fails the import.
- [ ] 4.4 Implement the proxy rule. When `render` and `proxy` are both drawn, a
  proxy-purpose prim is pruned iff `is_loaded()`. In the prototype walk, the instance
  prim's state decides (design D4). Verify with tests on the fixture:
  - one asset loaded among unloaded ones;
  - an asset without a payload;
  - proxies alone;
  - the two instanceable placements, one loaded: one draws render geometry, the other
    its proxy.
- [ ] 4.5 If 4.4's instancing test shows openusd sharing one prototype between a loaded
  and an unloaded instance, key the prototype's parts by (prototype, loaded) under the
  same epoch; verify the same test passes and `cargo test -p crust-core --test
  usd_scene` stays green
- [ ] 4.6 Verify that streaming does not change a view: on a fixture with at least
  `MIN_STREAM_CHUNKS` subtrees, a render with `--unloaded --load ...` is bit-identical
  with `CRUST_STREAM_IMPORT=0` and without it, at `-s 16`
- [ ] 4.7 Write the "Payloads, purposes and the view" section of
  `openspec/specs/usd-scene-import/design.md` (the open path, the camera rule, the proxy
  rule and its prototype trap, why not a live stage). Update the render-flow lines of
  `docs/architecture.md`. Verify the paths and names it cites exist (`rg` each symbol).

## 5. CLI flags (crust-render)

- [ ] 5.1 Add `--purposes`, `--binding-purposes`, `--unloaded`, `--load` and `--unload`
  to `SceneArgs`, with parsers that reject unknown tokens, non-absolute prim paths and a
  path given to both `--load` and `--unload` (usage errors naming the value). Map them
  into `UsdImportOptions` in `load_scene`, and add the `warn!` for a `--load` that
  changes nothing. Verify with argument-parsing unit tests in `main.rs` beside
  `cli_checkpoint_is_a_positive_number_of_seconds`, and that `crust check`,
  `diagnostic` and `light-paths` accept the flags.
- [ ] 5.2 Add the effective-setting rows (`purposes`, `binding_purposes`, `payloads`,
  each with its flag and attribute) and the reproducing command-line parts in
  `crust-tools/src/diagnostic/mod.rs`; verify in `crust-render/tests/check.rs` that
  `crust check --unloaded --purposes proxy --json -` reports them, and that a
  diagnostic's printed command line carries the view flags
- [ ] 5.3 Add an end-to-end test in `crust-render/tests/` rendering the fixture with
  `--unloaded --purposes proxy --binding-purposes preview,all` and with
  `--unloaded --load /World/a --purposes render,proxy`; verify the images differ where
  the spec says they do, and that `crust diff` against the same command run twice is
  identical
- [ ] 5.4 Document the five flags in `site/content/docs/reference/command-line.md` (with
  the decision-image recipe), and the decision workflow in
  `site/content/docs/architecture/design-choices.md`; verify `zola build` succeeds

## 6. The session's view (crust-render mcp)

- [ ] 6.1 Add `View` (preset, `LoadSet`, the two optional lists), `View::flags()`, and
  `check_args(layer, &view)` parsing `crust check -i <layer> <flags>`. Verify with unit
  tests: `final` prints no flags, the decision view prints
  `--unloaded --purposes render,proxy --binding-purposes preview,all`, and a
  `pcp::LoadRules` → flags → `LoadSet` round trip is equal.
- [ ] 6.2 Give `open_session` an optional `view` argument, defaulting to `decision`, and
  report the view in its result. Update the existing session tests that compare with
  the CLI to open with `view = "final"`. Verify `cargo test -p crust-render` (`mcp`
  tests) passes.
- [ ] 6.3 Add the `load`, `unload` and `set_view` tools: validation, re-import under the
  new view, cancelling a running render, and leaving the view and scene untouched on
  failure. Verify with tests in `crust-render/src/mcp/tests.rs` for the spec scenarios:
  - judging the hero;
  - putting it back (bit-identical);
  - loading what a probe hit;
  - a mistyped prim;
  - judging a loaded asset's look;
  - back to the final view.
- [ ] 6.4 Carry the view (with `flags`) in the results of `load`, `unload`, `set_view`,
  `render`, `snapshot` and `check`; a render reports the view it was made under. Verify
  with a test: a decision-view render at 16 spp is bit-identical (`crust diff`) to
  `crust render <output> -s 16 <reported flags>`.
- [ ] 6.5 Report authored purposes that the view overrides in an edit's result. Verify
  the two scenarios of "Authored purposes re-import": `applied = full` in the `final`
  view, an unchanged render with the override noted in the `decision` view, and
  `render_final` showing the proxies.
- [ ] 6.6 Verify the view's lifetime with tests: `undo` leaves the view alone, a resume
  starts in the `decision` view, `render_final` ignores the view (it equals
  `crust render <output>`), and the view never changes the layer's bytes
- [ ] 6.7 Update the server instructions and the tool list (the `Listing tools` test).
  Document views, `load` / `unload` / `set_view` and the decision workflow in
  `site/content/docs/help/claude-desktop.md`. Add a "The session's view" section to
  `openspec/specs/mcp-session/design.md`. Verify `zola build` succeeds and the
  `Listing tools` test passes.

## 7. Integration

- [ ] 7.1 Run the CI gates locally and verify each is clean:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --no-fail-fast`
  - `cargo deny --locked check`
  - `cargo +nightly-2026-09-26 clippy --workspace --all-targets -- -D warnings`
- [ ] 7.2 Measure ALab frame 1004 (local copy; `samples/ALab` is gitignored) with
  `--stats`: the default render, and the decision view
  (`--unloaded --purposes render,proxy --binding-purposes preview,all --camera <shot
  camera>`). Record in the usd-scene-import design record:
  - Parse USD stage time and peak memory;
  - geometry counts;
  - whether the fur is drawn;
  - which material the proxies resolve.
  Verify the decision view's Parse USD stage time is a small fraction of the default's,
  or record why not.
- [ ] 7.3 Retire the `includedPurposes` / `materialBindingPurposes` item from the aovs
  design record's follow-ups and the "Known gaps" it names. Update
  `site/content/docs/architecture/limitations.md`. Verify `rg -n includedPurposes
  openspec/specs site/content` shows no stale "not supported" text.

## Workflow follow-up

- Archive only after `live-stage-scene-deltas`, `live-session-source-reload` and
  `add-light-path-inspector` are archived: this change's `mcp-session` delta is written
  over their text.
- Open the phase-2 change (in-place view changes, design "Phase 2") once 7.2's ALab
  numbers show whether re-import latency in the session is the next bottleneck.
