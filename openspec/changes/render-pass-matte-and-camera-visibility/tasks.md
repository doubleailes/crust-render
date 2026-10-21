# Tasks

## 1. Choosing the pass and its settings (crust-core import)

- [ ] 1.1 Add `pass: Option<String>` to `UsdImportOptions` and resolve it on the
  index stage before the traversal: a path that is not a `RenderPass` (or does not
  exist) is an import error naming the path (design D1). Verify with tests in
  `crust-core/tests/usd_inline.rs`: a named pass is found; an `Xform` path and a
  missing path both fail before any geometry is imported.
- [ ] 1.2 Change `render_settings_path(stage)` to `render_settings_path(stage, pass)`
  so a `renderSource` that targets a `RenderSettings` prim wins (design D2). Raise
  the new `render_pass.render_source_invalid` `WarningCode` for any other target.
  Thread the pass through every caller and the MCP provenance's settings prim.
  Verify with a test that resolution, camera, products and `crust:samplesPerPixel`
  all come from `renderSource`, a test that an unauthored `renderSource` reads
  `/Render/settings`, and the warnings-page test after adding its row to
  `site/content/docs/reference/warnings.md`.
- [ ] 1.3 Warn `render_pass.collection_not_read` once per collection when the
  active pass's `prune` or `renderVisibility` restricts anything. Verify with a
  test (`prune` includes `/World/Set`: one warning, the set still renders) and the
  warnings page row.

## 2. Per-geometry holdout flag and camera bit (crust-core world)

- [ ] 2.1 Add `holdouts` to `WorldBuilder` / `World` with `has_holdouts()` and
  `is_holdout(geom_id)`, shaped like `transparent_emitters` and empty unless set
  (design D4). Verify with unit tests in `rt_world.rs`: unset ids answer false, and
  a world that never sets one reports `has_holdouts() == false`.
- [ ] 2.2 Build the pass's `matte` and `cameraVisibility` membership queries once
  per chunk from that chunk's stage. Skip a collection that restricts nothing.
  At the `LightLinks::saw` dispatch site, set the holdout flag and clear
  `MASK_CAMERA` (never set a bit) over the prim's geom-id range (design D3). Warn
  `render_pass.target_in_instance` (shared `inside_instance`) and
  `render_pass.volume_ignored`. Verify with tests in `crust-core/tests/`: nearest
  path decides; `explicitOnly`; a `membershipExpression` with a predicate on a mesh
  inside a payload; a PointInstancer judged on the instancer; a prototype target
  warns once; a held-out volume warns and is unchanged; `crust:rayMask = 6` geometry
  included in `cameraVisibility` stays camera-hidden.
- [ ] 2.3 Apply the pass to lights when they are added: clear the camera bit of an
  area light's emitter geometry without touching its transparent-emitter flag, and
  clear `MASK_CAMERA` from an infinite light's escape mask when it is outside
  `cameraVisibility` or inside `matte`. Verify with tests: a hidden-by-default
  `SphereLight` in `cameraVisibility` stays hidden; a `crust:light:cameraVisible`
  `RectLight` excluded by the pass is invisible to the camera and still occludes;
  a matte `DomeLight` shows nothing to the camera and lights the scene unchanged.
- [ ] 2.4 Pin the ordering against light linking. Verify with a test where a
  `DomeLight` linked to nothing and excluded from `cameraVisibility` is dropped
  rather than kept as a backdrop (escaping camera rays see 0).

## 3. Ending camera rays at holdouts (crust-core integrator and AOVs)

- [ ] 3.1 After `pass_cutouts` on the camera segment only, end the path at a
  holdout behind `world.has_holdouts()`. Record `FirstHit::Escaped` and add no
  emission, NEE or scatter (design D5, D6). Verify with tests in
  `crust-core/tests/`:
  - a full-frame opaque holdout before an emissive wall renders exactly 0, with
    alpha 0;
  - an opacity-0.5 holdout converges to 0.5 beauty and 0.5 alpha;
  - a holdout sphere's floor shadow is bitwise the no-pass render's shadow region;
  - a mirror reflects the holdout as shaded;
  - fog in front of a holdout wall keeps its in-scatter.
- [ ] 3.2 Verify AOVs at holdout samples. Test: a `color4f` beauty, `depth`
  (closest), `normal`, `P`, `albedo` and an LPE var at a pixel half covered by a
  holdout. Alpha and beauty are premultiplied 0.5, depth picks the visible
  surface, and a pixel fully held out has clear values. Also verify that
  `C.*[LO]` stays bitwise equal to the beauty with holdouts present.
- [ ] 3.3 Verify the cost of a world without holdouts. Check:
  - `scripts/check_images.sh check` reports no difference against goldens
    recorded before the change;
  - callgrind on `cornellbox.usda -s 2` (`RAYON_NUM_THREADS=1`) differs by at most
    one branch per camera ray;
  - the zero-AOV instruction-count test passes.

  Record the numbers in the rendering design record.
- [ ] 3.4 Verify the two-pass composite end to end: a character pass and a props
  pass from one sample stage, `over` composited, match the no-pass render within
  sampling noise (difference falls as 1/√N between 16 and 64 spp,
  `--indirect-clamp 0`). Add the sample stage under `samples/`.

## 4. CLI, check and MCP (crust-render)

- [ ] 4.1 Add `--pass <PRIM_PATH>` to `SceneArgs` (so `render`, `check`,
  `diagnostic` and `light-paths` all take it). It is passed into
  `UsdImportOptions` and warns when there is no `-i`. Verify with clap parsing
  tests beside `cli_subdiv_level_overrides_the_scene` and a run of
  `crust render -i samples/<passes sample> --pass /Render/Passes/char -s 16`.
  Document the flag in `site/content/docs/reference/command-line.md`.
- [ ] 4.2 Add `pass` to `crust ls` (`LsKind::Pass`, plural accepted), listing
  `RenderPass` prims not under an inactive ancestor in namespace order. Verify with
  a test on the passes sample, and update the `ls` section of `command-line.md`.
- [ ] 4.3 Report the active pass, the settings prim, and the holdout and
  camera-hidden geometry counts in `crust check` text and `crust-check/2` JSON.
  Verify with check-report tests for a pass with `renderSource` and for no pass
  (`pass: null`).
- [ ] 4.4 Accept an optional `pass` on MCP `open_session`, store it in the session,
  and append `--pass` to the argument lists of the session import and
  `render_final` (design D7). Verify with `mcp/tests.rs`: the session's
  `render_final` image is bitwise the CLI's with `--pass`; an edit adding a
  `collection:matte:includes` target re-imports and holds the target out; an
  unknown pass fails the call. Update `site/content/docs/help/claude-desktop.md`.

## 5. User documentation and design records

- [ ] 5.1 Write `site/content/docs/usd/render-passes.md`. It covers choosing a pass,
  `renderSource`, `matte` and `cameraVisibility` with a worked character/background
  example, compositing with `over`, the collections not read, and the known gaps
  from the `render-passes` spec. Link it from `usd/_index.md` and
  `usd/render-settings.md`. Verify `zola build` (0.21) in `site/` passes its
  link and anchor check.
- [ ] 5.2 Add the holdout pair to `docs/architecture.md` § Invariants:
  - the camera-vertex holdout check and `FirstHit::Escaped` recording change
    together;
  - pass camera visibility is applied before `LightLinks::resolve`.

  Add `crust-core/src/scene/usd_import/passes.rs` to the import module table in
  `usd_import/mod.rs`. Verify `cargo doc -p crust-core --no-deps` builds without
  broken intra-doc links.
- [ ] 5.3 Run the CI set and verify each is clean:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace --no-fail-fast`
  - `cargo deny --locked check`

## Workflow follow-up

- On archive, write `openspec/specs/render-passes/design.md`. Use this change's
  design.md plus the measurements from 3.3 and 3.4, and end it with its
  "Known gaps" section.
- Close issue #340 with a link to the merged change.
