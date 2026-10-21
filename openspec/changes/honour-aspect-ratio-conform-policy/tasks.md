# Tasks

## 1. Resolve the policy with the product base

- [ ] 1.1 In `usd_import/products.rs`, add the conform policy to `Base`. `Base::unauthored` sets `expandAperture`. `Base::resolve` reads `aspect_ratio_conform_policy_attr()` through `value_at` and `AspectRatioConformPolicy::try_from`, and falls back per attribute as for `dataWindowNDC`. Expose the first product's resolved policy beside its `resolution` (as `Framing`, design Decision 2). Verify with a unit test: settings-only, product-overrides-settings, and unauthored → `expandAperture`.
- [ ] 1.2 Add `product.invalid_conform_policy` (Refused, Each) and `product.framing_mismatch` (Approximated, Each) to `crates/crust-core/src/warnings.rs`, with rows in `site/content/docs/reference/warnings.md`. Verify that the test keeping the two in step passes.
- [ ] 1.3 Raise `product.invalid_conform_policy` for an authored token outside the five, at the prim that authors it, and resolve as unauthored. Verify with a test: `"adjustApertureHieght"` on the settings gives one warning and `expandAperture`.
- [ ] 1.4 In `resolve_products`, compare each later product's resolved policy to the first's, next to `product.region_mismatch`, and raise `product.framing_mismatch`. Do not add it to `same_render`. Verify with a test: two products `expandAperture` / `adjustApertureHeight` give both files and one warning naming the second.

## 2. Conform the camera frame

- [ ] 2.1 Thread `Framing` from `read_index` / the import context to `CameraFrame::read`'s callers (`build_camera`, `screen_projection`, `camera_lens`, and `live.rs`'s `refresh_settings`). Verify with `cargo build --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] 2.2 In `CameraFrame::read`, after the unauthored-vertical default, call `openusd_schemas::render::conform_aperture(policy, [W, H], 1.0, [h, v])`, keeping both the authored and the conformed pair. `tan_half_vfov` and `f_px` read the conformed vertical. Add the `debug_assert` that conformed `h / v` matches `W / H` (design Decision 3). Verify with a test over a 36×24 aperture at 1:1, 2:1 and 1:2 for all five policies, against the apertures in the spec's scenarios (48×24, 36×36, 36×18, 24×24) computed by hand, not by calling `conform_aperture`.
- [ ] 2.3 For `adjustPixelAspectRatio` with a mismatched aperture, conform as `expandAperture` and raise `product.unhonoured_attribute` naming the needed pixel aspect ratio. A matching aperture raises nothing. Verify with a test: 36×24 at 200×100 gives one warning naming 0.75 and the `expandAperture` vfov.
- [ ] 2.4 Write a render-level regression for #339, with the policy on `RenderSettings` over `samples/dome_backdrop.usda` at `-s 16`. With `adjustApertureHeight` the image differs from the image without the policy, and without the policy it is bit-identical to the pre-change binary (`crust diff` exit 0). Add a second case with the policy authored on the camera prim only, which must have no effect.
- [ ] 2.5 Audit the test stages that author `verticalAperture` (`light_linking.rs`, `hidden_lights.rs`, `usd_listing.rs`) for an aperture wider than their resolution, and update any expectation the new `expandAperture` framing legitimately moves, saying so in the commit. Verify that `cargo test --workspace --no-fail-fast` is green.
- [ ] 2.6 Run `scripts/check_images.sh record` on `main`, then `check` on the change. Verify zero diffs: every sample's conform is a no-op (the four samples authoring `verticalAperture` have apertures no wider than their image).

## 3. Readers of the framing

- [ ] 3.1 Add `conformed_aperture_mm` to `CameraRecord` in `listing.rs` and the `crust-ls/1` output. Verify with a `usd_listing.rs` test: a 36×24 camera at 200×100 with `adjustApertureHeight` reports `aperture_mm = [36, 24]` and `conformed_aperture_mm = [36, 18]`.
- [ ] 3.2 MCP live edits: verify with a test that changing only `aspectRatioConformPolicy` on a stage without camera-dependent geometry applies in place and is bit-identical to a full import of the same layer, and that on a stage with adaptive subdivision it reports the structural "another camera projection" re-import.

## 4. Documentation

- [ ] 4.1 `site/content/docs/usd/render-settings.md`: add an `aspectRatioConformPolicy` section covering the five policies with the 36×24 examples, where it is authored (settings or product, not the camera), the `adjustPixelAspectRatio` fallback, and a "Stages written for older versions" note (wide apertures now expand; author `adjustApertureWidth` to keep the old crop). Verify that `zola build` (0.21) in `site/` passes the link and anchor checks.
- [ ] 4.2 `site/content/docs/architecture/limitations.md`: record non-square pixels (`pixelAspectRatio`, `adjustPixelAspectRatio`) and the unread aperture offsets. Document `conformed_aperture_mm` where the `ls --json` fields are listed (`reference/command-line.md`, if it lists them). Verify with `zola build`.
- [ ] 4.3 `openspec/specs/usd-scene-import/design.md`: in the camera section, describe the conform in `CameraFrame::read`, why it precedes every reader, and the follow-up note on non-square pixels (design Decision 4). Move `pixelAspectRatio` there from the "unhonoured" paragraph's sole entry. Verify by reading the diff against the change's design.md.

## 5. Integration

- [ ] 5.1 Run the CI set locally: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`. All three are clean.
- [ ] 5.2 Validate the change: `openspec validate honour-aspect-ratio-conform-policy --strict` passes.

## Workflow follow-up

- Comment on #339 that `aspectRatioConformPolicy` belongs on `RenderSettings` / `RenderProduct`, with the corrected repro, and link the PR.
- Open a separate issue for `horizontalApertureOffset` / `verticalApertureOffset`.
- Archive the change after review and merge.
