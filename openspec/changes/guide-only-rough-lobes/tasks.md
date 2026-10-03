# Tasks

## 1. Baseline

- [ ] 1.1 Branch from `claude/unbiased-guided-pass-blend` (or `main`, once it has
      merged). Build its release binary as `bin_before`. Verify
      `samples/caustic_guided.usda` and `guided_caustic_is_not_darkened` are present.
- [ ] 1.2 Record baselines with `bin_before`, `--indirect-clamp 0`, 4 seeds
      (`-f 1..4`), 256²:
      - `caustic_guided.usda` at 1024 spp, guided and unguided, plus the existing 32k
        reference;
      - a `specularRoughness 0.2` variant of it;
      - `openpbr_showcase.usda` and `materialx_showcase.usda` with
        `crust:pathGuiding = true`, guided and unguided, each against an 8k-spp
        unguided reference.
      Keep the scene copies and numbers in the scratchpad. Verify every run has a
      relMSE and a mean-energy figure.

## 2. The guidable-fraction query

- [ ] 2.1 Add `ResolvedOpenPBR::guidable_fraction(threshold)`, also reachable for a
      plain `OpenPBR`. Build it from `LobePmf::from_params` and the lobe alphas:
      - Diffuse and Fuzz always count.
      - Specular and Coat count if `(ax·ay)^(1/4) ≥ threshold`, from their
        roughness and anisotropy.
      - Continuous Transmission counts if `transmission_alphas` passes the same
        test.
      - Thin-walled transmission never counts but stays in the total.
      Unit-test it in `openpbr/`: a pure diffuse material gives ≥ 0.99; smooth thick
      glass (roughness 0.05) gives < 0.01; a diffuse base with a smooth coat gives
      its diffuse pmf share; at roughness exactly equal to the threshold the lobe
      counts. Verify with `cargo test -p crust-core guidable_fraction`.
- [ ] 2.2 Add the closure equivalent over `ResolvedClosure::leaves()`:
      - Diffuse, Sheen and Translucent count.
      - Specular counts by the same alpha test, whatever its R/T/RT mode, unless it
        is thin-walled transmit, which is delta.
      - Subsurface never counts.
      Unit-test it with closures built as the existing closure tests build them.
      Verify with `cargo test -p crust-core guidable_fraction`.
- [ ] 2.3 Add `Material::guidable_fraction` (default 1.0) and
      `ShadingPoint::guidable_fraction(threshold)`, dispatching over `Resolved`. Add
      the snapping in one place: below 0.01 → 0, above 0.99 → 1. Threshold 0 returns
      1 without evaluating anything. Verify with a unit test that covers the snaps
      and the threshold-0 short cut.

## 3. Setting

- [ ] 3.1 Add `guiding_roughness_threshold` to `RenderSettings` (default 0.1, builder
      `with_guiding_roughness_threshold`). Read `float crust:guidingRoughnessThreshold`
      in `scene/usd_import/settings.rs`, clamping to 0–1 with a WARN, and print it in
      the existing guiding debug line. Extend the settings import tests: authored,
      absent (0.1) and out of range (warning, clamped). Verify with
      `cargo test -p crust-core --test usd_scene` (or wherever the settings tests
      live).
- [ ] 3.2 Document `crust:guidingRoughnessThreshold` in
      `site/content/docs/usd/render-settings.md` § Path guiding, including that 0
      turns the scaling off. Add it to the settings table in
      `site/content/docs/usd/overview.md`. State `crust:guidingProb`'s existing
      0.1–0.9 clamp. Verify with `zola build` (Zola 0.21) in `site/`.

## 4. Integrator

- [ ] 4.1 In `trace_path`, compute `f` once per surface vertex when guiding is on and
      the vertex is guided (`guiding_here`) or training. Pass `α' = α·f` (0 when the
      field is untrained there) into `sample_bounce_direction` and the NEE
      `bounce_pdf`, and use `1/(1−α')` for delta compensation. Gate `vrec.train` on
      `f > 0`. Rewrite the misplaced doc comment so it sits on
      `sample_bounce_direction` (today it is attached to `ray_cones_enabled`), and
      describe `α'` there.
- [ ] 4.2 Verify threshold 0 is the old behaviour: render `caustic_guided.usda`,
      `cornellbox_guided.usda` and the guided `openpbr_showcase` copy with
      `crust:guidingRoughnessThreshold = 0` at `-s 16`, and diff against `bin_before`
      with `exr_diff`. Zero differing pixels.
- [ ] 4.3 Verify the default keeps diffuse scenes bit-identical: `cornellbox_guided.usda`
      at the default threshold against `bin_before`, `-s 16`. Zero differing pixels.
- [ ] 4.4 Add an `#[ignore]`d MIS consistency test to `crates/crust-core/tests/guiding.rs`.
      Use a small scene with a partly guidable material: a diffuse base under a
      smooth coat, lit indirectly. Check that the guided and unguided mean luminance
      agree within noise under `power`, `balance` and `light` strategies. Show it
      fails when the NEE side is deliberately left on the unscaled `α` (scratch
      edit, not committed), then passes. Verify with
      `cargo test -p crust-core --release --test guiding -- --ignored`.
- [ ] 4.5 Run `guided_tiles_and_rows_are_bit_identical` and the rest of
      `cargo test -p crust-core --test guiding` (plus `-- --ignored`). Verify all
      pass, `guided_caustic_is_not_darkened` included.

## 5. Measure and choose the default

- [ ] 5.1 Sweep the threshold over 0.05, 0.1, 0.2 and 0.3 on the 1.2 scenes, 4
      seeds, `--indirect-clamp 0`. Report shadow/image relMSE, energy and time
      against `bin_before` guided and unguided.
      - Verify the spec scenario: at the chosen default, `caustic_guided.usda`'s
        shadow relMSE is within the seed spread of unguided (exploration: 0.41
        against 0.42).
      - Verify no scene gets clearly worse than `bin_before` guided.
- [ ] 5.2 If the sweep favours a default other than 0.1, update `RenderSettings`, both
      delta specs, the docs from 3.2 and design.md together. Verify
      `openspec validate guide-only-rough-lobes --strict` passes afterwards.
- [ ] 5.3 Add a `#[ignore]`d noise test, `guided_sharp_caustic_is_not_noisier`. It
      renders `caustic_guided.usda` guided and unguided, compares shadow-region
      variance across 8 seeds, and uses the same small resolution as
      `guided_caustic_is_not_darkened`. Calibrate so it fails with threshold 0 and
      passes at the default. Verify both outcomes and record them for the PR
      description.

## 6. Documentation

- [ ] 6.1 Update `openspec/specs/rendering/design.md`:
      - the path-guiding paragraph: `α'`, the guidable share, snapping, threshold 0;
      - replace the "Narrow lobes are guided anyway" known gap with what remains
        (anisotropic lobes, no product sampling, no learned `α`), quoting the 5.1
        numbers;
      - in the "`ΔEff` cannot see rare bright paths" gap, re-measure what the check
        now decides on `caustic_guided.usda`.
      Verify the record has no stale numbers.
- [ ] 6.2 Update `site/content/docs/architecture/limitations.md`: retire or reword
      "Sharp glass gets noisier" to match 5.1, and add the anisotropic caveat.
      Update `README.md`'s guiding paragraph if its "Every continuous lobe is guided"
      sentence no longer holds. Verify with `zola build`.

## 7. Integration checks

- [ ] 7.1 Run the CI gates on the pinned toolchain (`cargo +1.98.1`): `fmt --check`,
      `clippy --workspace --all-targets -D warnings`, `test --workspace
      --no-fail-fast`. Verify all pass.
- [ ] 7.2 Run `scripts/check_images.sh check` against goldens recorded with
      `bin_before`. Verify every unguided sample is identical and that
      `cornellbox_guided` is identical too; only `caustic_guided` may differ.
- [ ] 7.3 Run `openspec validate guide-only-rough-lobes --strict` and verify it
      passes.
