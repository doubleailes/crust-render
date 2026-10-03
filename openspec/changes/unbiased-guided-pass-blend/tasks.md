# Tasks

## 1. Reproduction and baseline

- [x] 1.1 Add `samples/caustic_guided.usda`: a glass ball (`crust:openpbr`,
      `transmissionWeight` 1, `specularRoughness` 0.05, IOR 1.5) on a white floor in
      front of a white back wall, a `SphereLight` of radius 0.04, and `crust:pathGuiding`
      on with 4 training iterations. Add a doc string saying what it reproduces. Verify
      it loads and renders (`cargo run --release -- -i samples/caustic_guided.usda -s 64
      --indirect-clamp 0`) with a visible caustic inside the ball's shadow.
- [x] 1.2 Build the parent commit's release binary (`bin_before`) and keep it for the
      comparisons in 2.5 and 3.1.

## 2. Budget-weighted blend

- [x] 2.1 Add a pure `pass_weights(spp: &[u32]) -> Vec<f64>` in `tracer/mod.rs`
      (each pass's spp over the total). Unit-test it in `tracer/tests.rs`: for
      `[2, 2, 4, 8, 256]` the weights are `spp / 272`, they sum to 1, and an empty or
      all-zero input is handled without NaN. Verify with
      `cargo test -p crust-core pass_weights`.
- [x] 2.2 Make `render_guided` record each pass's configured spp next to its buffer,
      and make `blend_passes` and `blend_luminance` use `pass_weights` instead of
      `1 / variance`. Drop `PassStats::variance` if nothing but the debug log reads it,
      and log the weight shares as before. Verify that
      `guided_tiles_and_rows_are_bit_identical` and the rest of
      `cargo test -p crust-core --test guiding` pass.
- [x] 2.3 Rewrite the `render_guided` and `blend_passes` doc comments: why weights are
      fixed in advance, and the configured budget rather than samples taken. Verify by
      reading them against design.md.
- [x] 2.4 Add the `#[ignore]`d end-to-end test `guided_caustic_is_not_darkened` to
      `crates/crust-core/tests/guiding.rs`. It renders `caustic_guided.usda` guided and
      unguided at indirect clamp 0 and a fixed seed, and compares the shadow-region
      mean luminance. Calibrate resolution, spp and tolerance so that it **fails with
      the old blend** (check out `blend_passes` from the parent commit, or use
      `bin_before`'s numbers) and passes with the new one, in seconds under
      `cargo test -p crust-core --release --test guiding -- --ignored`. Record both
      outcomes in the PR description.
- [x] 2.5 Measure the fix on `caustic_guided.usda`: 4 seeds (`-f 1..4`) at 1024 spp,
      `--indirect-clamp 0`, against a 32k-spp unguided reference, for `bin_before`,
      the new binary and unguided. Report shadow and caustic energy (mean over
      reference) and relMSE. Verify the new binary's energy is within noise of unguided
      (the exploration measured 0.84 / 0.69 before).

## 3. No regression where guiding helps

- [x] 3.1 On `cornellbox_guided.usda`, render a high-spp unguided reference and compare
      relMSE for `bin_before` and the new binary over 4 seeds, at equal spp and
      `--indirect-clamp 0`. Verify the new blend is not clearly worse (within the
      seed-to-seed spread). If it is, stop and revisit design.md's "Weight each pass
      by its sample budget" before going on.
- [x] 3.2 Use the 3.1 numbers to re-measure the "guiding cuts the error by about 20%"
      claim (guided vs unguided at equal spp). Update it in
      `site/content/docs/architecture/design-choices.md` and in the design record.
      Verify `zola build` in `site/` (Zola 0.21) succeeds.

## 4. Documentation

- [x] 4.1 Update `openspec/specs/rendering/design.md`:
      - the path-guiding paragraph: budget weights, why inverse estimated variance
        was biased, and the measurement from 2.5;
      - in the `ΔEff` paragraph, the reference image is now budget-weighted;
      - "Known gaps: path guiding": `ΔEff` still relies on 2–8-spp variance estimates,
        unreliable under heavy tails (the caustic scene measured `ΔEff` 0.51 and fell
        back to an unguided final pass).
      Verify the record no longer says "weighted by inverse variance" anywhere.
- [x] 4.2 Add one sentence to `site/content/docs/usd/render-settings.md` § Path guiding:
      training passes are kept and averaged into the image in proportion to their
      samples. Verify `zola build` succeeds.

## 5. Integration checks

- [x] 5.1 Run the CI gates: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`. Verify all pass.
- [x] 5.2 Run `scripts/check_images.sh check <dir>` against goldens recorded with
      `bin_before`. Verify there are zero differences: unguided images must be
      untouched.
- [x] 5.3 Run `openspec validate unbiased-guided-pass-blend --strict` and verify it
      passes.
