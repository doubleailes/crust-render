# Tasks

## 1. Physical lens diameter

- [x] 1.1 Find how openusd exposes the stage's `metersPerUnit`: a
      `Stage::meters_per_unit()` beside `up_axis()`, else the root layer's
      metadata. Write a private reader in `usd_import/camera.rs` that returns
      `Option<f64>`, and check with a unit test that it returns `Some(1.0)` on an
      inline stage authoring `metersPerUnit = 1` and `None` on a stage authoring
      nothing.
- [x] 1.2 Add the `camera.invalid_meters_per_unit` code to `warnings.rs`
      (Refused, Each) and a row under "Camera" in
      `site/content/docs/reference/warnings.md`. Verify that the warnings-table
      test in `warnings.rs` passes.
- [x] 1.3 In `lens_camera`, compute the aperture as
      `focal / fStop × 0.001 / metersPerUnit`, reading 0.01 when `metersPerUnit`
      is unauthored. Read 0.01 and raise `camera.invalid_meters_per_unit` when it
      is not finite or not positive, and do the check only when `fStop > 0`.
      Update the `debug!` line to print the diameter in scene units. Verify with
      `cargo test -p crust-core --test usd_inline` (the existing
      `f_stop_gives_the_camera_a_lens` still passes).
- [x] 1.4 Add `usd_inline.rs` tests for the spec's "Camera depth of field" and
      "An unusable metersPerUnit is refused" scenarios. Cover a cm stage
      (diameter 2.5: the origin offset from lens samples `[0, 0.5]` and
      `[1, 0.5]` is 2.5 apart), an m stage (0.025), unauthored (2.5), the
      pinhole case, an identical centre direction across units, a zero
      `metersPerUnit` with a lens (one warning, 2.5) and without one (no warning).
      Verify that they pass.
- [x] 1.5 Remove `fStop = 1000` from `samples/cornellbox_guided.usda`. In
      `samples/openpbr_showcase.usda`, change `fStop = 1000` to `fStop = 100`
      and update its comment to give the new reading
      (`50 / 100 × 0.001 / 0.01 = 0.05` units). Record goldens with
      `scripts/check_images.sh record` on the parent commit and run `check`
      after this group. Verify that only these two move:
      `cornellbox_guided`'s difference falls as 1/sqrt(N) between `-s 16` and
      `-s 64` (no bias, just the old near-pinhole lens gone), and
      `openpbr_showcase`'s `crust diff` relmse is at the float-rounding level,
      not a loss of blur.
- [x] 1.6 Document the lens in a "Camera depth of field" section of
      `site/content/docs/usd/render-settings.md`, beside "Camera exposure". Give
      the formula, the millimetre reading, the `metersPerUnit` fallback, the
      factor older stages change by (10x cm, 1000x m) and where it departs from
      Hydra. Update the camera row of `docs/architecture.md` if it describes the
      lens. Verify with `zola build` in `site/` (Zola 0.21).

## 2. disableDepthOfField

- [x] 2.1 Add `depth_of_field: bool` (default `true`) and `with_depth_of_field`
      to `RenderSettings` in `tracer/settings.rs`, beside `motion_blur`. Verify
      that `cargo build -p crust-core` succeeds.
- [x] 2.2 In `products.rs`, resolve `disableDepthOfField` in `Base` (product over
      settings, through `custom_bool` or a schema accessor if one exists), carry
      `depth_of_field` on `RenderProducts`, and remove it from
      `warn_unhonoured`. In `mod.rs`, set it on the settings next to
      `with_motion_blur`. Verify with a test in `products.rs`'s tests that the
      product's value beats the settings'.
- [x] 2.3 Add `product.depth_of_field_mismatch` (Approximated, Each) to
      `warnings.rs` and `warnings.md`, and raise it for a later product whose
      effective value differs, as `product.motion_blur_mismatch` is raised.
      Verify with the warnings-table test and a products test.
- [x] 2.4 Make `lens_camera` build a pinhole (aperture 0) when
      `settings.depth_of_field` is false, before the `metersPerUnit` check.
      Verify with `usd_inline.rs` tests for the spec's "Render settings can
      disable depth of field" scenarios: a sharp render with no
      `product.unhonoured_attribute` naming it, product over settings, the
      default, products that disagree.
- [x] 2.5 Pin the live-edit pair: an MCP-style edit that authors
      `disableDepthOfField = true` on the settings prim applies in place and
      gives the same camera rays as a full re-import of the same layer. Verify
      with a test beside the existing live-update tests in
      `crust-core/src/scene/usd_import/live.rs` (or `crust-render/src/mcp/tests.rs`).
      Also cover a stage with `metersPerUnit = 0` and `fStop = 2`, where an edit
      sets `disableDepthOfField = true`. The in-place update's warnings must
      equal the re-import's, with no stale `camera.invalid_meters_per_unit`.
- [x] 2.6 Document `disableDepthOfField` under "Standard attributes" in
      `site/content/docs/usd/render-settings.md` (after `disableMotionBlur`).
      Remove it from the not-honoured list in `site/content/docs/usd/aovs.md`.
      Update the honoured-attributes notes in `openspec/specs/aovs/design.md`
      and `openspec/specs/usd-scene-import/design.md`. Verify with `zola build`.

## 3. MCP sessions carry the input's stage metadata

- [x] 3.1 Make `open_session` (`crust-render/src/mcp/session.rs`) read
      `metersPerUnit` and `upAxis` from the input's root layer (as `place.rs`
      `up_axis` reads it). Have `new_layer_text` (`mcp/layer.rs`) write
      whichever ones the input authors into the new layer's metadata. Verify
      with a `layer.rs` unit test: an input authoring both gives a layer with
      both and `subLayers`, an input authoring neither gives the layer as before.
- [x] 3.2 On resume, add whichever of the input's authored `metersPerUnit` /
      `upAxis` the override layer does not author, keeping every opinion and
      any value the layer already authors. Verify with an
      `mcp/tests.rs` test that resumes a layer written by the old
      `new_layer_text` and finds the metadata added and the edits intact.
- [x] 3.3 Add an `mcp/tests.rs` test on the `metersPerUnit = 1` look-dev stage
      with a camera at `fStop = 2`. The session's camera rays and `render_final`'s
      output must match `crust render` of the input at `-s 16`, checked with
      `crust diff`. Add a second test: a Z-up input's session reads `upAxis = Z`
      from its own stage.
- [x] 3.4 Retire the "override layer carries no `upAxis`" gap in
      `openspec/specs/mcp-session/design.md`, recording the "the copy wins"
      trade-off instead. If `site/content/docs/help/claude-desktop.md`
      describes the layer's content, update it there too. Verify with
      `zola build`.

## 4. Integration

- [x] 4.1 Run CI's checks locally: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`. Verify that all three pass.
- [x] 4.2 Render `samples/cornellbox.usda` with `fStop = 2` added through a
      layer: once as authored, once with `disableDepthOfField = true`. Check with
      `crust diff` that the second matches the unmodified sample bit for bit at
      `-s 16`, and that the first differs only off the focus plane.

## Workflow follow-up

- Sync the deltas into `openspec/specs/usd-scene-import/spec.md` and
  `openspec/specs/mcp-session/spec.md`, then archive
  the change once it is reviewed and merged.
