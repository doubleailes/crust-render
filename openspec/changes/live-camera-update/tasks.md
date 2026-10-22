## 1. Baseline

- [ ] 1.1 Build the current binary and record goldens: `scripts/check_images.sh
      record <dir>`, at 16 spp with `--indirect-clamp 0`. Keep the binary for the
      A/B in 4.2. Done when the golden directory holds an EXR for every sample.

## 2. The dicing camera in the import (crust-core)

- [ ] 2.1 Read `rel crust:dicingCamera` from the `RenderSettings` prim in
      `usd_import/settings.rs`, with the same pass resolution as
      `crust:subdivisionEdgeLength`. Add `WarningCode::CameraDicingNotCamera`
      (`camera.dicing_not_a_camera`) and its row in
      `site/content/docs/reference/warnings.md`. Done when the warnings
      vocabulary test passes.
- [ ] 2.2 Split `read_camera_ahead` so that the projection comes from the
      dicing camera and the shutter from the render camera (design D2). Pass
      the dicing camera to `subdiv_policy`, and warn
      `subdiv.adaptive_needs_camera` only when neither camera resolves. Done
      when `cargo test -p crust-core` passes unchanged, which shows that
      unauthored stages are untouched.
- [ ] 2.3 Add tests to `crates/crust-core/tests/usd_scene.rs` for each scenario
      of "The dicing camera":
      - **unauthored:** tessellation identical to before;
      - **diced for `/B`, seen through `/A`:** triangle counts and vertex
        positions equal to the `/B` render's;
      - **a dicing camera alone enables adaptive tessellation**, with no
        warning;
      - **a non-camera target:** the warning, then render-camera dicing;
      - **products do not override it.**
      Done when all pass.
- [ ] 2.4 Document `crust:dicingCamera` in
      `site/content/docs/usd/render-settings.md` (what it dices, the default,
      the use case of a Hydra viewport or a camera fly-through), and in
      `openspec/specs/usd-scene-import/design.md` § adaptive subdivision. Done
      when `zola build` in `site/` passes its link check.

## 3. In-place camera moves (crust-core `live`, crust-render `mcp`)

- [ ] 3.1 Add `dicing: Option<String>` to `StageFacts`, recorded by the import.
      `refresh_settings` refuses when the resolved dicing path changed (design
      D3). Done when a `tests/live.rs` test shows that retargeting
      `crust:dicingCamera` returns `Structural`.
- [ ] 3.2 Add `Provenance::any_below(path)`, true when a material, light or
      geometry prim imported by the scene lies under `path`. Done when a unit
      test covers a mesh under a camera, a sibling and the camera itself.
- [ ] 3.3 In `crust-render/src/mcp/delta.rs` `classify`, put `xformOp:*` and
      `xformOpOrder` values on the render camera in `settings` unless
      `any_below` (design D4). Done when the `mcp/tests.rs` classification
      tests cover the four new spec scenarios, and the existing "a transform
      is not confined" test still passes.
- [ ] 3.4 Add bit-identity tests that render in place against a `full`
      application at 16 spp, for a translate and a rotate of the render camera
      on:
      - cornellbox;
      - an adaptive scene with a dicing camera;
      - a scene with on-screen implicit shapes and a dicing camera;
      - a scene with `domeLightCameraVisibility` off.
      Done when every pair is bitwise equal, and the adaptive scene with no
      dicing camera reports `full`.
- [ ] 3.5 Update `openspec/specs/mcp-session/design.md`: narrow the "Transforms,
      topology and namespace edits re-import" known gap, and record the time of
      an in-place camera move on cornellbox and on ALab frame 1004 when it is
      available. Done when the record states both figures, or states that ALab
      was not available.

## 4. Integration checks

- [ ] 4.1 Run `cargo fmt --all -- --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast` and
      `cargo deny --locked check`. Done when all four pass.
- [ ] 4.2 Run `scripts/check_images.sh check <dir>` against the 1.1 goldens.
      Done when every sample reports `identical`.

## Workflow follow-up

- Archive after review. `hydra-render-delegate` builds on this change.
