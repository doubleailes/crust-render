# Tasks

Prerequisite: `add-diagnostic-command` is archived, so that
`openspec/specs/diagnostics/spec.md` holds the requirements this change modifies.

## 1. Tier 1 strategy factor (design D1)

- [x] 1.1 Make the `strategy` factor try only the other MIS heuristic. Unit test in
      `diagnostic/tests.rs`:
      - `power` yields `[balance]`;
      - `balance` yields `[power]`;
      - an authored `light` or `bsdf` yields `[power, balance]`;
      - no factor ever yields `light` or `bsdf`.
- [x] 1.2 Add the `visualization_strategy` correctness finding to `checks.rs`:
      - it fires when the authored strategy is `light` or `bsdf`;
      - its action is `--strategy power` / `crust:samplingStrategy = power`;
      - unit test, and `actions_name_only_real_settings` still passes.

## 2. A seed per repeat (design D7)

- [x] 2.1 Render pair *i* with `with_frame(frame + i · 0x9E37_79B9)` (wrapping) on
      both sides; pair 0 keeps the scene's seed. Record the seeds in `run.seeds`.
      Tests:
      - pair 0's images are bitwise today's;
      - two runs give the same seeds and images;
      - pairs *i* ≠ *j* of an unguided baseline differ.
- [x] 2.2 Keep every pair's images, not only the first. Build each crop's reference
      from the baseline's image of each seed, once per seed, plus every image of every
      trial that is not `biased` (task 3.3).

## 3. The bias guard (design D2, D3)

- [x] 3.1 Add `trials::luminance_shift(baseline, trial) -> (shift, z)`: trim the 1%
      of pixels with the largest `|d|` (at least one, never all), then take the
      shift and its z over the kept pixels with `se = sqrt(Σ(var_T + var_B)) / Σ lum_B`.
      Unit tests:
      - identical images give `(0, 0)`;
      - a uniform darkening to 39% gives a shift of −0.61 and a z far beyond 4;
      - one image with a few firefly pixels added is not `biased`;
      - a 3% shift at z 1.5 is not `biased`.
- [x] 3.2 Add `Verdict::Biased` (`"biased"`): a crop is `biased` when every pair's
      `|shift| > 0.02` and `|z| > 4`, whatever the ΔEff, and the overall verdict is
      `biased` when any crop is. Unit tests for the spec's three bias scenarios.
- [x] 3.3 Reorder `run`: compute every pair's shift, then build the references
      (task 2.2), then judge.
      - Combined-trial winners come only from trials that are not `biased`.
      - The combined trial is guarded too.
      - Test with a synthetic `Running` holding a biased trial: it is absent from
        the reference blend, from the winners and from `suggestions`, and its
        report shows its ΔEff and its shifts.

## 4. Trimmed MRSE, the noise floor, and the verdict order (design D7, D8)

- [x] 4.1 Add `trials::mrse_trimmed(b, t, reference)`. It excludes the pixels in the
      top 0.1% (at least one) of either side's `var / ref²`, the same set on both
      sides, and returns both sides' trimmed and untrimmed MRSE. Unit tests:
      - one firefly pixel on one side leaves the trimmed pair ratio at 1;
      - with no outlier, trimmed and untrimmed are within 0.1%.
- [x] 4.2 Add the noise floor: `max / min` of the baseline's own-trimmed MRSE over
      its seeds per crop, and `null` (taken as 1) when R = 1.
- [x] 4.3 Rewrite `crop_verdict` and `overall` to the spec's order: `biased`, then
      `better` / `worse` (every pair beyond both the ±5% band and the noise floor,
      and the untrimmed median ΔEff on the same side of 1), then
      `insufficient_samples` (noise floor > 1.10), then `inconclusive`. Unit tests
      for every spec scenario of the requirement, including:
      - "The error moves between seeds" (floor 2.26 → `insufficient_samples`);
      - "Trimming withholds, never creates";
      - "Within the noise" (floor 1.04 → `inconclusive`).
- [x] 4.4 `converged` is false while any trial is `insufficient_samples`. Unit test.

## 5. Render time, and setup at the target (design D9)

- [x] 5.1 Compute pair ΔEff from `render_s` only. Keep `setup_trial_s`.
- [x] 5.2 Compute each trial's full-frame setup:
      - `learned`: as measured;
      - guiding: the crop's training time × frame / crop pixels, median over crops;
      - combined: the sum of its factors';
      - baseline: P1's own setup.
- [x] 5.3 Compute `delta_eff_at_target = (setup_B + R_B) / (setup_T + R_B / ΔEff)`,
      with `R_B` from tier 2's target, or the scene's spp without one. Then:
      - suggestions need `better`, an overall ΔEff ≥ 1.10 and an at-target ΔEff ≥ 1.10;
      - tier 2 reports `projected_setup_s`.

      Unit tests:
      - with no setup, the at-target ΔEff equals the overall ΔEff exactly;
      - the spec's two setup scenarios: one amortised, one vetoed.

## 6. The budget reservation (design D10)

- [x] 6.1 In `schedule.rs`, replace `TIER_SHARES` with reserves from estimates:
      - tier 2's adaptive renders;
      - tier 3's half-depth pairs, reach renders and missing baselines.

      `trial_spp` picks the largest power of two at which tier 1's trials and tier 3's
      spp-dependent renders fit together with tier 2's fixed cost. Fake-clock tests:
      - the spec's "Later tiers need little" scenario (97 of 100 s to tier 1);
      - roll-forward is unchanged.
- [x] 6.2 Report `run.tier2_reserve_s` and `run.tier3_reserve_s`. List every tier-2
      and tier-3 measurement not run for time under `not_tried` (`budget`), including
      when tier 1 overran. Test: a budget that tier 1 exhausts lists the adaptive,
      half-depth and reach measurements.

## 7. Light-sampling reach (design D4)

- [x] 7.1 In tier 3, render one light-only image per crop at the trial spp and
      compare it, untrimmed, with that crop's pair-0 baseline image: report `reach`
      and `z`.
      - When tier 1 did not render a crop, render its baseline too.
      - `not_applicable` when there is no light-list entry, or when the authored
        strategy is `light` or `bsdf`.
      - Crops that do not fit go under `not_tried` (`budget`).
      - Log each crop's *trimmed* shift at DEBUG: the calibration evidence that D2
        would catch light-only.
- [x] 7.2 Add `picture_changing.light_sampling_reach` (per crop `{crop, reach, z}`,
      or `null`) and its Markdown line. Integration test in
      `crust-core/tests/diagnostic.rs`: on the Cornell box the reach is within noise
      of 1 (|z| < 4).

## 8. Picture findings and the Verdict (design D5, D6)

- [x] 8.1 In `noise::measure`, compute the share of non-emission luminance (the
      beauty minus the `emission` row) held by the brightest 0.1% of pixels, and
      the noise row holding most of those pixels' luminance.
- [x] 8.2 Add the `clamp_bias` (≥ 5%), `firefly_energy` (≥ 20%) and
      `light_sampling_misses` (< 0.9, |z| > 4) checks, with their thresholds as
      constants beside `MIN_HIT_RATE`.
      - Each carries `action: none` with the D5 reason.
      - Assemble the findings list after tier 3, keeping its place in the report.
      - Unit tests: each fires at its threshold and not below; none blocks
        `converged`.
- [x] 8.3 In the Markdown verdict block:
      - add the **Picture** line: correctness findings and `biased` trials with
        their numbers, else `none`;
      - make the top-noise-source line cite `firefly_energy` and
        `light_sampling_misses` when present;
      - make the converged line say "raise `--budget`" when a trial is
        `insufficient_samples`.

## 9. Report format (design D11)

- [x] 9.1 Add the new keys at the end of their objects, all `#[serde(default)]`,
      and rename `time_baseline_s` / `time_trial_s` to `render_baseline_s` /
      `render_trial_s`, with serde aliases. Show in the Markdown trials table:
      - the shift;
      - the noise floor;
      - the at-target ΔEff when it differs from the overall one.
- [x] 9.2 Update the key-order test and the Markdown snapshot. Test that
      `--baseline` still parses a report fixture written before this change.

## 10. Calibration on real scenes (design D2, D5, D7, D10)

- [x] 10.1 Run `crust diagnostic` on `samples/cornellbox.usda` and
      `samples/veach_mis.usda` at the default budget, and record every shift, z and
      noise floor.
      - No tier-1 trial is `biased`.
      - None of the three picture findings fires on the Cornell box.
      - On `veach_mis`, explain any `firefly_energy` in the design record, or adjust
        its scope there.
- [x] 10.2 Run it on ALab: `renders/alab/alab_aovs.usda -f 1004` at `--budget 2m`
      and `--budget 10m` (import ~3 min and about 30 GB each, so run them one after
      the other).
      - No tier-1 factor is `biased`. If one is, open an issue for the renderer pair
        it breaks, and do not loosen a threshold.
      - The reach is well below 1 (about 0.4 expected from the full-frame means).
      - The DEBUG trimmed shift of light-only is far beyond 2%.
      - `clamp_bias` reports about 66%, and `firefly_energy` fires.
      - At 2m, record which trials are `insufficient_samples`. At 10m, record which
        of them resolve, and at what spp.
      - Record tier 1's spp and the unspent budget against the parent change's run:
        4 spp, 34 s unspent.
- [x] 10.3 If calibration moves a threshold, update the spec delta and the design
      before archiving.

## 11. Documentation

- [x] 11.1 Update `site/content/docs/help/diagnosing-a-render.md`:
      - the tier-1 list (the other MIS heuristic only), and the budget reservation
        in place of the 70/15/15 split;
      - "Efficiency, and how it is measured": seeds per repeat, trimmed MRSE, render
        time, the at-target ΔEff;
      - the verdict table with `biased`, `insufficient_samples` and the noise floor;
      - what to do on `insufficient_samples`, including when to stop raising the
        budget;
      - the new keys, the three picture findings and the Picture line;
      - a paragraph on why the guard exists, citing the ALab numbers;
      - `--repeats 1` cannot report `insufficient_samples`.
- [x] 11.2 Change the example in `site/content/docs/reference/command-line.md` that
      runs `crust diagnostic --strategy bsdf --baseline …` to apply a setting that
      is not a visualization mode (e.g. `--light-selection learned`).
- [x] 11.3 Fold D1–D11 and the calibration numbers into
      `openspec/specs/diagnostics/design.md`:
      - rewrite "Efficiency and its reference" (seeds, trimming, the guard, the
        reference order), "Verdicts" and "Trials, budget, tiers";
      - add a "trap already fallen into" note with the ALab table;
      - move the follow-ups under Known gaps.

## 12. Verification

- [x] 12.1 Run `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings` and
      `cargo test --workspace`. Run `zola build` in `site/` to check links and
      anchors.
- [x] 12.2 Run `openspec validate harden-diagnostic-verdicts --strict`.
