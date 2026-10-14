## 0. Prerequisite

- [x] 0.1 Archive `live-stage-scene-deltas` (`/opsx:archive`), so its "Edits apply in
      place when they are confined" requirement is in
      `openspec/specs/mcp-session/spec.md` for this change's delta to modify.
      `openspec validate` checks the delta alone and passes either way; archiving
      this change is what would fail. Done when the requirement is in the main spec.

## 1. Settings on camera-tessellated scenes (design D1)

- [x] 1.1 Record in `StageFacts` the adaptive `ScreenRate` (`eye`, `f_px`, the
      frustum's planes when it has one, `target`, `max`) and, when an implicit
      shape's count came from it, the projection `ImplicitPolicy` used, as bit keys
      (`ScreenKey`), replacing the `adaptive` / `on_screen_shapes` booleans. The key
      is what the tessellation is handed (design D1's trap).
- [x] 1.2 In `refresh_settings`, drop the two refusals and compare the recorded values
      with those `read_index` returns, by `to_bits`. Refuse with a reason naming which
      changed. Unit tests: equal projections pass; each field changed by one ulp
      refuses.
- [x] 1.3 Session tests on `samples/subdivision_adaptive.usda`: `exposure` on the
      render camera → `settings`, bit-identical to a full import at 16 spp (beauty,
      AOVs, warnings), and its undo; `focalLength` → `full`. The same exposure case on
      a written stage with an unshared implicit sphere sized on screen.

## 2. Lights on linked scenes (design D2)

- [x] 2.1 `LightLinks::resolve` returns its index remap (emission index → index after
      demotion, or `None`). Unit test: two demotions, one before and one after a kept
      light.
- [x] 2.2 `load_scene` applies the remap to `light_origins` before building the
      `Provenance`, and drops demoted lights. Test: on `samples/light_linking.usda`
      and on a stage with a demoted light ahead of others, every provenance index
      names the light whose prim path it is keyed by (`LightList::names`).
- [x] 2.3 Remove the `linked` refusal from `Rebuilding::light` and the links/backdrops
      refusal from `LightList::check_replace`. Confirm (read, then pin in the test of
      2.4) that `emit_light` puts no link state on the `LightKind` or the emitter's
      mask.
- [x] 2.4 Session tests: `inputs:intensity` and `inputs:color` on `Key`, `Fill` and
      `Rim` of `light_linking.usda` → `lights`, bit-identical to a full import, and
      their undo; on the written stage, the demoted light → `full`, the light after it
      → `lights` and bit-identical.

## 3. Materials behind payloads (design D3)

- [x] 3.1 `owning_material` (`mcp/delta.rs`): when the authoring stage does not
      compose the prim, take the nearest provenance material on its path and mark the
      prim as to be verified in the `Change`. Keep the `Shader` check where the
      authoring stage composes the prim.
- [x] 3.2 `Provenance::rebuild`: on the payload-loaded stage masked to the material,
      refuse (`Structural`) a to-be-verified prim that is not a `Shader` in that
      material's network.
- [x] 3.3 Session tests on a written stage whose material lives in a layer brought in
      by a payload: `inputs:roughness` on its shader → `materials`, bit-identical to a
      full import, and its undo; an input on a `NodeGraph` under the material →
      `full`.

## 4. Measurement

- [x] 4.1 Re-run the ALab latency measurement of `live-stage-scene-deltas` (the
      stdio MCP driver; frame 1004 wrapper `renders/alab/alab.usda`; the camera's
      `exposure`, `lgt_fill_top`'s `inputs:intensity`, the stoat `sweater_full`
      shader's `inputs:roughness`, a translate on `/root/stoat`; switch on and off,
      3 rounds each, never two sessions at once). Record min / mean in this change's
      `notes.md`. Done when each confined class reports its own `applied`, or the
      remaining refusal is named from a `-l debug` session.
- [x] 4.2 A callgrind instruction count of `crust render` on the cornell box at `-s 2`
      and on `light_linking.usda`, before and after: unchanged except import-time
      bookkeeping.

## 5. Documentation and CI

- [x] 5.1 `openspec/specs/mcp-session/design.md`: rewrite the settings and lights
      refusal lists in "Rebuilds go through the import's own code" and
      "Applying the rebuilt parts"; retire or narrow the Known gap "ALab applies
      nothing in place" from 4.1's result.
- [x] 5.2 `site/content/docs/help/claude-desktop.md`: what applies in place on
      adaptive, linked and payload-delivered scenes. `zola build` (0.21) passes.
- [x] 5.3 CI locally: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`, `cargo deny --locked check`, and
      `scripts/check_images.sh check` against goldens recorded before the change
      (every checked-in sample bit-identical: `crust render` must not move).
