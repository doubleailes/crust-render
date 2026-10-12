## 1. Assets survive edits

- [x] 1.1 Make `import_checked` (`crust-render/src/main.rs`) take an
      `Arc<FileAssets>` instead of building one. `crust render`, `check` and
      `diagnostic` pass a fresh one, so they behave as before. Done when every
      checked-in sample renders bit-identical (`scripts/check_images.sh check`).
- [x] 1.2 Add `FileAssets::revalidate()` (`crust-assets`): stat every loaded file and
      evict the entries whose length or mtime changed, across textures, the tile cache
      and Ptex. Done when a unit test rewrites a PNG between two loads and gets the
      new texels, and an untouched file is not decoded twice (count decodes).
- [x] 1.3 The MCP session keeps one `Arc<FileAssets>` for its lifetime and calls
      `revalidate()` before each import. Done when the spec's two "Assets persist
      across edits" scenarios pass as session tests.

## 2. Provenance

- [x] 2.1 Add `Scene::provenance` (`scene.rs`): material path → slot indices (by
      `Arc` pointer, every epoch's `Arc`), light path → `LightList` index plus the
      `geom_id`s sharing its `Emissive`, and the `LightLinks` runs kept instead of
      dropped. Done when a test over `samples/cornellbox.usda` and an instanced sample
      maps every material and light slot to its prim path, and a streamed import with
      several epochs maps every `Arc` of a shared material.
- [ ] 2.2 Check the cost: a callgrind instruction count of `crust render` on the
      cornell box at `-s 2`, before and after, differs only by the table's
      construction. `--stats` peak memory on ALab and Moana is recorded in the
      change's notes.
      *Callgrind done (+169 869 instructions, 0.004%, all import-time; `notes.md`).
      ALab and Moana peak memory not measured: neither stage is in this checkout.*

## 3. In-place entry points (crust-core)

- [x] 3.1 Hold `World`'s committed `crust_rt::Scene` in an `Arc`. Add
      `World::replace_materials`, which recomputes the derived flags and returns
      `Err(Structural)` when any of them would change. Done when unit tests cover a
      colour change (accepted), an opacity change to a cutout (refused) and an
      emission change from 0 (refused).
      *Not held in an `Arc`: the session takes its renderer apart and moves the
      world into the updated scene, so nothing shares it. The check is
      `World::check_materials` (per-slot `live::Signature`), the swap
      `World::replace_materials`.*
- [x] 3.2 Add `LightList::replace(index, LightKind)` and a rebuild of the selection
      that goes through the same `select_by` / learned training as a fresh
      `Renderer::new`. An area light's `Emissive` replaces `AreaLight.material` and its
      provenance slots in one call. Done when a test shows `radiance_toward` agrees
      between the light and the material after the replacement.
- [x] 3.3 In `usd_import`, expose `rebuild_material(stage, path, …)` and
      `rebuild_light(stage, path, …)`. They go through the same functions the
      traversal calls, under `EvalTimeScope`, with empty import caches and the
      session's assets. Settings and camera reuse their import functions.

## 4. Classification and the session

- [x] 4.1 Diff the root layer's `AbstractData` before and after into
      `(spec path, field)` changes (`mcp/edit.rs`, next to `check_against`).
      Unit tests: a set value, an added spec, a removed spec, a time sample, a
      relationship.
- [x] 4.2 Classify against the allow-list in design D1, reading composed prim types
      from the session's stage. Anything else is `full`. Unit tests for each table row,
      and for a mixed batch taking its most expensive class.
      *Through a real session (`edits_in_place_render_as_a_full_import`,
      `unconfined_edits_are_full`), since the classifier reads the import's
      provenance.*
- [x] 4.3 `Open::edit` and `undo` apply by class: build a new `Scene` from the old one
      plus the rebuilt pieces, wrap it in a new `Renderer`, and on `Structural` or any
      rebuild error fall back to `full`. Report `applied` in the result. Cancelling
      the in-flight render and the undo stack stay as they are.
- [x] 4.4 Add `CRUST_SCENE_DELTAS` to `crust_core::Config` (default on; `0` gives
      `full` and a fresh `FileAssets` per import, the old behaviour).

## 5. Bit-identity pair and measurement

- [x] 5.1 For each class, a session test edits a sample, renders at 16 spp, and
      compares bitwise (beauty, every AOV, the warnings list) with a session that
      imports the same saved layer as `full`. Cover: an exposure change (`settings`); a
      `RectLight`'s intensity and colour, and a dome light's texture file (`lights`); a
      base colour, a roughness and a texture-file input (`materials`); and an undo of
      each.
- [ ] 5.2 Measure the edit latency (`import_s`) for one edit of each class on the
      cornell box, the DPEL teapot and ALab, with `CRUST_SCENE_DELTAS` on and off, and
      record min and mean in the change's notes.
      *Measured on a synthetic textured stage, `usdpreview_textured` and
      `PointInstancedMedCity` instead (`notes.md`); the DPEL teapot's asset and ALab
      are not in this checkout.*

## 6. Documentation and CI

- [x] 6.1 `docs/architecture.md`: the switch row, and the in place ↔ full re-import
      pair under § Invariants.
- [x] 6.2 `site/`: `reference/environment-variables.md` (`CRUST_SCENE_DELTAS`) and
      `help/claude-desktop.md` (what applies in place, the `applied` field). Done when
      `zola build` (0.21) passes.
- [x] 6.3 Design records: write the mcp-session's long-form record as
      `openspec/specs/mcp-session/design.md` (today it lives only in the archived
      change), with this change's classification table, traps and measurements. Add
      the provenance table and rebuild entry points to `usd-scene-import/design.md`,
      and list a Hydra delegate, subtree re-traversal and BVH refit under its Known
      gaps.
- [x] 6.4 CI locally: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`, and
      `scripts/check_images.sh check` against goldens recorded before the change.
      *All pass; the 37 checked-in samples are bit-identical (Kitchen_set is not in
      the checkout). The nightly leg was not run.*
