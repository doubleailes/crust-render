## 1. Import the scale

- [ ] 1.1 In `usd_import/camera.rs`, compute the linear exposure scale from
      `exposure`, `exposure:time`, `exposure:iso`, `exposure:fStop` and
      `exposure:responsivity` at `eval_time()`, in `f32` and in C++'s order
      (`time × iso × 2^exposure × responsivity / (100 × fStop × fStop)`). Done when
      unit tests give exactly 1 with nothing authored, 4 at `exposure = 2`, 1.5 for the
      full set in the spec, and 1 with only `fStop = 4` authored.
- [ ] 1.2 Add `camera.invalid_exposure` (`Refused`, `Each`) to `warnings.rs` and its
      row to `site/content/docs/reference/warnings.md`; raise it for a non-finite or
      non-positive scale and keep 1. Done when the warnings test for
      `exposure:fStop = 0` passes and the codes-in-step test passes.
- [ ] 1.3 Add `RenderSettings::exposure_scale` (default 1) and set it from the render
      camera during import. Done when a stage with `exposure = 2` imports with a scale
      of 4, a time-sampled exposure follows `-f`, and the procedural fallback keeps 1.

## 2. Apply it to the film

- [ ] 2.1 Add `AovSource::exposure_power()` as an exhaustive match (1 for `Color` and
      `Lpe`, 2 for `Variance`, 0 otherwise).
- [ ] 2.2 At the end of `render_impl`, multiply the beauty buffer and every film plane
      by `scale^power`, skipping the whole step when the scale is 1. Done when a render
      at `exposure = 1` has beauty and LPE channels exactly twice, variance exactly
      four times, and depth, normal and sample count identical to the `exposure = 0`
      render (16 spp).
- [ ] 2.3 Scale the estimates the snapshot publish writes. Done when a controlled
      render's last snapshot equals its final beauty at a scale other than 1.
- [ ] 2.4 Check that the samples taken do not depend on the exposure: the same
      adaptive render at `exposure = 0` and `exposure = 3` reports the same
      `spp_taken` and per-pixel sample counts.

## 3. Record and compare

- [ ] 3.1 Add `exposure_scale` to `SamplingStamp`, written as `crust:exposureScale`;
      pin it in the stamp's attribute-order test and in a written product's header.
- [ ] 3.2 Report `exposure_scale` in `crust check`'s effective settings (flag `null`,
      attribute `exposure`), and in `crust diagnostic`'s, which uses the same shape.
- [ ] 3.3 In `compare::comparability`, warn naming `exposureScale` when both stamps
      carry it and the values differ; read a missing key as 1. Done when the
      spec's two scenarios pass as unit tests.

## 4. Verify and document

- [ ] 4.1 `scripts/check_images.sh check` against goldens recorded before the change:
      every sample unchanged (no sample authors an exposure).
- [ ] 4.2 Render the Sponza overlay's camera 1 (4.5 stops) and record the brightness
      change and the stamp in the change's notes.
- [ ] 4.3 Docs: the camera section of the user docs (the five attributes, the formula,
      what scales and what does not, the stamp), and the `usd-scene-import` and
      `aovs` design records. Done when `zola build` passes.
- [ ] 4.4 CI locally: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`, and
      `openspec validate camera-exposure --strict`.
