## Why

On ALab (`renders/alab/alab_aovs.usda -f 1004`, `--budget 2m`), `crust diagnostic`
named `--strategy light` its best change (overall ΔEff 14.31, `better` on every crop)
and wrote it into `suggested_command`. Full-frame renders at 32 spp with the clamp off
give these beauty means:

| strategy | beauty mean |
|---|---|
| power | 0.937 |
| balance | 0.936 |
| bsdf | 0.918 |
| light | 0.361 |

So the suggestion makes the image 61% darker. An agent that trusts the Verdict would
ship that render. The diagnostic cannot see the problem for three reasons:

- A trial's MRSE is its *own* variance over the reference's squared luminance, so
  removing energy is never penalised.
- `light` and `bsdf` are tried as unbiased swaps, though `--help` describes them as
  modes "to visualize what MIS balances between".
- The light-only images have the lowest variance, so they dominate the
  inverse-variance reference blend: on ALab their MRSE was 0.34–0.50 against the
  baseline's 2.7–10.1. That biases the reference of every crop.

The report did hold the explanation, but buried it in tier 3. The default clamp
removes **66%** of the luminance, and the top 0.1% of pixels hold 41% of the beauty's
energy. Most of this scene's light reaches the camera on paths that only BSDF sampling
finds. That fact explains the noise and makes the default render far too dark, yet it
is neither a finding nor in the Verdict.

The same run showed that the efficiency measurement is fragile even when no trial is
biased:

- **The error is one lottery draw.** Repeats reuse the seed, so the three pairs differ
  only in time. A single 4-spp error estimate on a firefly-heavy crop decides every
  pair. `light_samples_indirect=2` read 2.6–5.7× *noisier* than the baseline (MRSE
  2.74→11.8), which is not physical for an unbiased change that adds shadow rays.
  Yet every pair agreed, so the verdict looked certain.
- **Setup decides learning trials.** ΔEff charges setup to a 4-spp crop:
  - `learned` setup was 0.47–0.79 s against a 0.36 s crop render;
  - guiding training was 1.4 s;
  - tier 2 then projects a 264-spp render, where that setup is a small share.
- **Budget is left unspent.**
  - The run exited 3 with 34 s of its 120 s unspent.
  - `light_samples=2` and `=4` were skipped for "budget". Tiers 2 and 3 kept a fixed
    30% share, then were skipped silently once tier 1 overran: no entry in
    `not_tried`.

## What Changes

- **A bias guard on every tier-1 trial.**
  - On each crop, the trial's mean luminance is compared with the paired baseline's.
    The two share their seed, so an unbiased change moves the mean by little.
  - A trial that moves it beyond both a relative tolerance and the noise gets a new
    verdict, `biased`.
  - A `biased` trial is never suggested, never wins a factor for the combined trial,
    and never enters a crop's reference. The reference is built only from images the
    guard passed.
  - Every per-crop result reports both sides' mean luminance and the measured shift,
    so a reader can check "did this change the picture?" in the report itself.
- **`--strategy light` and `--strategy bsdf` leave tier 1.**
  - Tier 1 keeps the other MIS heuristic (`power` ↔ `balance`).
  - Light-only moves to tier 3 as a measurement: the share of the energy that light
    sampling alone reaches on each crop (`light_sampling_reach`). Below 1 means some
    light arrives only on paths BSDF sampling finds. That is where MIS has no partner
    strategy and fireflies come from.
  - BSDF-only is no longer run.
  - A stage that authors `light` or `bsdf` gets a `visualization_strategy` correctness
    finding, with the action `--strategy power`.
- **Fireflies become findings and reach the Verdict.**
  - `clamp_bias` (`correctness`): the authored clamp would remove at least 5% of the
    baseline's luminance.
  - `firefly_energy` (`noise`): the top 0.1% of the baseline's pixels hold at least 20%
    of its non-emission luminance.
  - `light_sampling_misses` (`noise`): light sampling reaches less than 90% of the
    energy on the measured crop.
  - None of the three has a setting that fixes it, so each carries `action: none` with
    its reason, and none blocks `converged`.
  - The Markdown Verdict gains a **Picture** line naming them, and any `biased` trial.
- **A robust error estimate.**
  - Each repeat renders with its own sampler seed. Pair 0 keeps the authored seed, and
    runs stay deterministic.
  - The baseline's own MRSE spread across those seeds is the crop's **noise floor**. A
    crop is `better` (or `worse`) only when every pair clears both the ±5% band and
    the noise floor.
  - A new verdict, `insufficient_samples`, applies when a crop could not have
    resolved a gain worth suggesting: neither `better` nor `worse`, with a noise floor
    above 1.10. Any such trial makes `converged` false, and the report says to raise
    `--budget`.
  - MRSE is trimmed: the top 0.1% of pixels by relative variance, the same pixel set
    on both sides of a pair. The untrimmed value is reported beside it. Trimming can
    withhold a verdict, but never create one.
- **Judge on render time.**
  - A pair's ΔEff uses render time only.
  - Setup is charged where a render pays it: each trial also reports its ΔEff **at
    the target**. This is the time to reach tier 2's target error on the full frame,
    setup included, scaled the way each setup scales.
  - A suggestion needs both an overall `better` and an at-target ΔEff of at least
    1.10.
  - Tier 2 reports the projected setup beside the projected render time.
- **The budget follows the work.**
  - The fixed 70/15/15 split goes. Tiers 2 and 3 reserve their estimated render cost,
    and tier 1 may use everything else.
  - A tier-2 or tier-3 measurement skipped for time is always listed under
    `not_tried`.
- **Report:**
  - new verdict values `biased` and `insufficient_samples`;
  - new per-crop keys:
    - `mean_luminance_baseline`, `mean_luminance_trial`, `luminance_shift`,
      `luminance_shift_z`;
    - the untrimmed MRSEs and ΔEff;
    - `noise_floor`;
  - `time_*_s` become `render_*_s` (setup excluded); `setup_trial_s` stays;
  - per trial `delta_eff_at_target`;
  - `run.seeds`, and `run.tier2_reserve_s` / `run.tier3_reserve_s`;
  - `sample_budget.projected_setup_s`;
  - `picture_changing.light_sampling_reach`;
  - the four new finding ids.
  - The format stays `crust-diagnostic/1`, because no release has shipped it (see
    design). `--baseline` still reads a report written before this change.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `diagnostics`:
  - tier 1 no longer tries the light-only and BSDF-only strategies;
  - every trial passes a bias guard (a new `biased` verdict), and references hold only
    the images it passed;
  - tier 3 measures light-sampling reach;
  - four new findings: clamp bias, firefly energy, light-sampling reach, and a
    visualization strategy authored;
  - the Verdict's **Picture** line;
  - a seed per repeat, trimmed MRSE, the noise floor and the `insufficient_samples`
    verdict, which `converged` now accounts for;
  - ΔEff on render time, the at-target ΔEff, and the suggestion bar;
  - tier 2's projected setup;
  - the budget reserves what tiers 2 and 3 need instead of fixed shares.

## Impact

- **Depends on `add-diagnostic-command`**, which introduces the `diagnostics`
  capability. That change is archived first, and this one modifies its requirements.
- `crust-core/src/diagnostic/`:
  - `trials.rs`:
    - the paired luminance test;
    - trimmed MRSE;
    - the noise floor;
    - the `biased` and `insufficient_samples` verdicts and their precedence;
    - the at-target ΔEff;
  - `mod.rs`:
    - drop `light` / `bsdf` from the strategy factor;
    - seed each repeat;
    - judge bias before building references;
    - the tier-3 reach trial;
    - pass the new facts to the checks;
    - the suggestion bar;
    - list every skipped tier-2/3 measurement;
  - `schedule.rs`: estimate-based reservation in place of `TIER_SHARES`;
  - `checks.rs`: the four findings;
  - `report.rs` / `markdown.rs`: the new keys, the verdict values and the Picture line;
  - `noise.rs`: the top-pixel share.
- The renderer and integrator are untouched. The seed is the existing
  `RenderSettings::with_frame`; the scene's time is resolved at import, so only
  sample patterns change. Every new number is computed from images the diagnostic
  already renders. The only new renders are tier 3's light-only renders, one per
  crop. They replace the 2 trials × crops × 2R renders the two dropped strategy
  trials cost: 36 renders on ALab become 3. Tier 1 gets cheaper, and with the
  reservation it gets more of the budget, so trials run at more samples.
- Docs:
  - `site/content/docs/help/diagnosing-a-render.md`:
    - the tier-1 list and the budget split;
    - efficiency on render time and at the target;
    - the verdict table;
    - the new findings, and the Picture line;
  - `openspec/specs/diagnostics/design.md`: the record, with the ALab measurement.
- Out of scope, recorded as follow-ups in the design: the small report issues found
  in the same run:
  - `camera: –`;
  - a `suggested_command` without the projected `-s`;
  - no hint to author light-group tags on large rigs.
