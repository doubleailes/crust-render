# Tasks

## 0. Prerequisite

- [x] 0.1 Archive `render-pass-matte-and-camera-visibility` (#340) once its tasks
  3.4 and 5.1 are closed, so `openspec/specs/render-passes/spec.md` exists and
  `scene-check` / `cli` carry its requirements. Verify with
  `openspec validate render-visibility-and-all-passes`, which must resolve every
  MODIFIED header in this change's deltas.

## 1. Spikes that decide the approach

- [x] 1.1 Check whether BVH construction in crust-rt reads masks: SBVH splits,
  leaf packing, `Tri4` / `Tri4i` grouping (design D2). Verify by committing one
  scene twice with two different mask tables and comparing the node, packet
  and record layouts. They must be identical apart from mask fields. If they
  are not, make packing mask-independent first and verify
  `scripts/check_images.sh check` and `scripts/test_simd_matrix.sh -p crust-rt`
  report no change. Record the result in the design.
- [ ] 1.2 Check that a `def RenderSettings "sharp" (inherits = </Render/settings>)`
  that authors only `disableDepthOfField = 1` composes the base's camera,
  resolution, products and `crust:` settings through every settings reader. Verify
  with a test in `crust-core/tests/render_passes.rs` on a pass whose
  `renderSource` targets it, and repeat with `references` and `specializes`. A
  failure is an openusd gap: report it upstream and record it as a known gap
  rather than working around it. (A manual run before this change found all
  three arcs compose camera, resolution and `crust:samplesPerPixel` in
  `crust check`, and the inherited `disableDepthOfField` stamps
  `crust:depthOfField = 0`. The test pins that.)

## 2. Mask rewrite in crust-rt

- [x] 2.1 Add `Scene::remask` rewriting the top-level mask copies (triangle
  records, leaf `LaneMasks` with `mask_and` / `mask_or`, prim nodes, instance
  prims) and never a prototype scene (design D2). Verify with a unit test over a
  scene holding every geometry kind and an instance: for random mask tables `a`
  and `b`, `remask(commit(a), b)` equals `commit(b)` field for field. Also verify
  `scripts/test_simd_matrix.sh -p crust-rt` passes.
- [x] 2.2 Pin the pair in `docs/architecture.md` § Invariants (`remask` ↔ commit),
  and add `remask` to the intersection-kernel design record. Verify that
  `cargo doc -p crust-rt --no-deps` builds.

## 3. One pass state, one function (crust-core)

- [ ] 3.1 Make `LightLinks::resolve` write masks, light classes and the remap into
  a mask table instead of `WorldBuilder` (design D1). The single-pass import
  applies that table to the builder before commit. Verify that every test in
  `light_linking.rs`, `render_passes.rs` and `holdouts.rs` passes unchanged,
  `scripts/check_images.sh check` reports no difference, and the zero-AOV
  instruction-count test passes.
- [ ] 3.2 Make `ActivePass` judge `renderVisibility` beside `matte` and
  `cameraVisibility`, record a three-bit membership per geom-id range, volume
  range and light, and apply precedence (design D7). Then build the pass state
  with `apply_pass(base, pass)`: masks, holdouts, lights outside
  `renderVisibility` removed before links, links resolved. Narrow
  `render_pass.collection_not_read` to `prune`, and extend
  `render_pass.volume_ignored` to `renderVisibility`. Verify with tests in
  `crust-core/tests/render_passes.rs`:
  - a plate alone, where render-hidden geometry casts no shadow;
  - lights off: no illumination and no dome to the camera;
  - a render-hidden `RectLight` that no shadow ray hits;
  - matte plus render-hidden is not a holdout;
  - a light linked only to hidden geometry lights nothing;
  - a pass with no `renderVisibility` is bitwise unchanged;
  - a render-hidden volume warns.

  Also update the rows in `site/content/docs/reference/warnings.md` and verify the
  warnings-page test.
- [ ] 3.3 Add `World::apply_pass(&PassState)`: `remask`, the holdout table, the
  light classes, transparent emitters, volume masks. Verify with a test that a
  world committed for pass A, after `apply_pass(B)`, renders `render_passes.usda`
  bitwise like a world committed for B.
- [ ] 3.4 Add the render-hidden count to `PassSummary` / `PassInfo` and to
  `crust check`'s pass line and JSON. Verify with the check-report test for a pass
  whose `renderVisibility` leaves out two meshes.

## 4. Import groups (crust-core)

- [ ] 4.1 Extract `refresh_settings`' refusal checks into `ImportKey::of` (design
  D3), and have `refresh_settings` compare keys. Verify that every MCP test in
  `crust-render/src/mcp/tests.rs` passes unchanged, and add a unit test that
  passes differing only in products share a key while `crust:subdivisionLevel`
  splits them.
- [ ] 4.2 Let `UsdImportOptions` take a group of passes. Judge each in the one
  traversal, mask every chunk to every pass prim, and return the shared `Scene`
  plus one `PassScene` per pass (settings, camera, aovs, settings prim, summary,
  that pass's warnings) re-read from the kept stage (design D4). Verify with a
  test that each `PassScene` equals what a `--pass` import of that pass returns
  for those fields.

## 5. `crust render --all-passes` (crust-render)

- [ ] 5.1 Add `--all-passes` to `render` and `check`. It is refused with `--pass`,
  `-o`, `--stats-json`, or without `-i`. Verify with clap tests beside
  `cli_pass_is_a_scene_argument`, and document the flag in
  `site/content/docs/reference/command-line.md`.
- [ ] 5.2 Split `render_and_write` so one pass renders from parts and gives the
  world and lights back (design D5). Verify that a plain render and `--pass`
  render are bitwise unchanged (`scripts/check_images.sh check`) and that the
  `render_final` MCP test still passes.
- [ ] 5.3 Implement the loop:
  - list the passes and group them;
  - compute every product file and refuse collisions before any import;
  - import once per group, then `apply_pass`, render and write per pass;
  - check the interrupt between passes;
  - log one INFO line per group and per pass.

  Verify with tests in `crust-render/tests/`:
  - `render_passes.usda --all-passes -s 16` writes both files, each identical to
    `--pass` under `crust diff`;
  - the log shows one import;
  - a stage with no pass fails;
  - two passes with no products fail before rendering, naming both;
  - a `crust:subdivisionLevel` split imports twice;
  - an interrupt after the first pass leaves the third unwritten.
- [ ] 5.4 Add a plate-and-lights-off pass and a sharp utility pass (inheriting
  the base settings with `disableDepthOfField`) to `samples/render_passes.usda`.
  Verify that `crust render -i samples/render_passes.usda --all-passes -s 16`
  writes every pass, and that `crust ls pass` lists them in order.

## 6. `crust check --all-passes` (crust-core + crust-render)

- [ ] 6.1 Build each pass entry from `check_scene`'s pieces over its `PassScene`,
  with the diff against the no-pass entry and the always-printed depth of field
  and motion blur (design D6). Add the `passes` key (`null` without the flag) and
  the text section after the render described. Verify with check-report tests:
  - a leaked depth of field shows "on" with only the product different;
  - an inherited `disableDepthOfField` shows on → off;
  - `passes` is `null` without the flag;
  - the JSON lists `/Render/Passes/char` first with `/Render/charSettings`;
  - a stage with no pass exits 0 with an empty list.
- [ ] 6.2 Raise `passes_share_output` from the collision function `render` uses.
  Verify with a test where two passes have no products: one finding naming the
  default beauty file and both passes, exit status 0. Add the finding to the
  diagnostics findings list in the user docs.

## 7. User documentation and design records

- [ ] 7.1 Update `site/content/docs/usd/render-passes.md`:
  - `renderVisibility` (geometry, lights off, precedence);
  - the settings-override recipe with `inherits`;
  - `--all-passes` and the shared import;
  - `check --all-passes`;
  - known gaps: drop `renderVisibility` and "one render renders one pass"; add
    "hidden geometry still costs traversal".

  Describe the shared import on the `site/` architecture page. Verify that
  `zola build` (0.21) in `site/` passes its link and anchor check.
- [ ] 7.2 Add to `docs/architecture.md` § Invariants: `--pass` ↔ the one-pass case
  of `--all-passes` (one `apply_pass`), and `ImportKey` shared by MCP and
  grouping. Verify `cargo doc -p crust-core --no-deps` builds without broken
  intra-doc links.

## 8. Integration

- [ ] 8.1 Measure the shared import on `samples/render_passes.usda` and one
  production-sized stage. Record import seconds for N passes, `--all-passes`
  against N × `--pass`, using `scripts/bench_ab.sh` style interleaving, in the
  render-passes design record.
- [ ] 8.2 Run the CI set and verify each is clean:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --no-fail-fast`
  - `cargo deny --locked check`

## Workflow follow-up

- On archive, merge this change's design and measurements into
  `openspec/specs/render-passes/design.md` and update its "Known gaps" section.
- Close issue #348 with a link to the merged change.
