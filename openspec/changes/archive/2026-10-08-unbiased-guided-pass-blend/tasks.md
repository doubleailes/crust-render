# Tasks

## 1. Baseline

- [x] 1.1 Build the parent commit's release binary (`bin_before`) and keep it for the
      comparisons below.
- [x] 1.2 Add the issue's probe as `crates/crust-render/examples/guided_bias_probe.rs`,
      check it compiles against `main`, and run it on ALab's crop
      (`renders/alab/alab_aovs.usda 1004 176 88 448 360 64 40`). Verify the shift
      reproduces: guided darker, |z| > 3 (measured −6.5%, z −4.1 unpaired, −4.7
      paired, darker on 36/40 seeds).

## 2. Budget-weighted blend

- [x] 2.1 Add a pure `pass_weights(spp: &[u32]) -> Vec<f64>` in `tracer/mod.rs`. Unit
      tests in `tracer/tests.rs`: for `[2, 2, 4, 8, 256]` the weights are `spp / 272`
      and sum to 1; an empty or all-zero input gives zeros, never NaN.
- [x] 2.2 Make `render_guided` record each pass's configured spp next to its buffer
      (the final pass's from its `PassConfig`, not the samples it took), compute the
      shares once, and hand them to `blend_passes`, `AovFilm::blend` (which now takes
      shares) and the blend's variance map; route `blend_luminance` through
      `pass_weights`. Remove `blend_weights` and `PassStats::variance` (its only other
      reader was the training pass's debug line; `render_pass` still logs the mean
      variance). Keep the weight-share debug line.
- [x] 2.3 Rewrite the `render_guided`, `blend_passes`, `blend_luminance` and
      `AovFilm::blend` doc comments, and the scanline-gather comment: why the weights
      are fixed in advance, and the configured budget rather than samples taken.
- [x] 2.4 Add `samples/caustic_guided.usda` and the ignored end-to-end test
      `guided_caustic_is_not_darkened` (shared `utils::luminance`, not inlined
      Rec.709 weights). Calibrate it to fail on the old blend (ratio 0.510 over 32
      seeds, run against the parent commit's tracer) and pass on the new (0.97).

## 3. ALab

- [x] 3.1 Re-run the probe with the new binary: −3.3%, z −2.2 / −2.7 — a shift
      survived the |z| < 2 bar.
- [x] 3.2 Test the second suspect, the guide mixture ↔ NEE pair: in a scratch build,
      log each pass's own crop mean and compare it with an unguided render at the same
      seed and spp, over 80 seeds. No pass is biased (design.md, Measurements).
- [x] 3.3 Replicate on 40 independent seeds at 256 spp: +0.3%, z +0.3 / +0.4. Fixed.
- [x] 3.4 Report the noise on both sides and the `ΔEff` decisions (design.md,
      Measurements; `rendering`'s Known gaps).
- [x] 3.5 Run `crust diagnostic` with `bin_before` and the new binary and check the
      `guiding=true` trial no longer reads `biased`. The issue's run (three crops,
      `--budget 10m`, 32 spp): `biased` before, `worse` after (ΔEff 0.14, |z| ≤ 3.1).
      The crop alone (`--region`, 64 spp) read `insufficient_samples` even before
      (−2.5%, z −3.0) and reads `worse` after (ΔEff 0.20). At `--budget 2m` (4 spp)
      guiding still reads `biased`, at +12–17%: a picture-check limit at a few
      samples, recorded in the `diagnostics` design record's Known gaps.

## 4. Documentation

- [x] 4.1 `openspec/specs/rendering/design.md`: the path-guiding paragraph (budget
      weights, why inverse estimated variance was biased, the ALab before/after
      numbers), the `ΔEff` reference, and "Known gaps: path guiding" (the ALab entry
      removed; `ΔEff`'s blindness to rare paths added).
- [x] 4.2 The other records: `aovs` (guided blend), `cli` (the probe in the
      cookbook), `diagnostics` (calibration: #244 fixed); `docs/light_sampling.md`;
      `crust-core/src/filter.rs`'s module doc (no more "inverse-variance pass
      blending"); `diagnostic/trials.rs`'s reference doc (it no longer mirrors
      `render_guided`).
- [x] 4.3 `site/`: the limitations page (the darkening removed, the noise added),
      `usd/render-settings.md` (how training passes are combined), the design-choices
      page and `README.md` (the training schedule; the "about 20% less error" claim
      withdrawn and replaced by a measurement: on `cornellbox_guided.usda` an
      unguided render with the same 320 spp has 13% less error in a sixth of the
      time); the diagnosing-a-render page's guiding example. Verify `zola build`.

## 5. Integration checks

- [x] 5.1 `cargo test -p crust-core --release --test guiding -- --ignored` passes,
      and `guided_tiles_and_rows_are_bit_identical` with the default suite.
- [x] 5.2 `scripts/check_images.sh check` against goldens recorded with `bin_before`:
      every unguided scene identical (36 samples and both Kitchen_set variants); the
      two that author `crust:pathGuiding`, `cornellbox_guided` and `caustic_guided`,
      differ, as they must.
- [x] 5.3 `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
      warnings`, `cargo test --workspace --no-fail-fast` (1655 passed, 4 ignored).
- [x] 5.4 `openspec validate --strict`.
