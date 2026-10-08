## ADDED Requirements

### Requirement: Trials that change the picture are never gains

Every tier-1 trial, the combined one included, SHALL be checked on each crop for a
change of the picture before it is judged on efficiency. The check compares the
trial's crop with the baseline's crop rendered in the same pair, with the same seed:

- The **luminance shift** SHALL be the relative difference of the two crops' mean
  luminance, `mean_T / mean_B − 1`. It SHALL be measured over the crop's pixels,
  excluding the 1% whose two values differ most, so that a few firefly pixels
  cannot produce it.
- The shift's **z** SHALL be the shift divided by its standard error, estimated
  from both images' own per-pixel variances.
- A crop SHALL be `biased` when the absolute shift exceeds 0.02 **and** the
  absolute z exceeds 4, in every pair.

The overall verdict SHALL be `biased` when any crop is `biased`, whatever the
efficiency. A `biased` trial:

- SHALL NOT become a suggestion;
- SHALL NOT be a factor's winner for the combined trial;
- SHALL NOT contribute an image to any crop's reference;
- SHALL be reported with its per-crop shifts, its ΔEff, and its verdict.

Each per-crop result of every trial SHALL report both sides' mean luminance, the
luminance shift and its z, whether or not the trial is `biased`.

#### Scenario: A setting that darkens the image

- **WHEN** a trial renders a crop whose mean luminance is 39% of the paired
  baseline's, well beyond the noise, and its ΔEff is 14
- **THEN** its verdict is `biased`, it is not suggested, its images are not
  part of the crop's reference, and the report shows both mean luminances and
  the shift of −0.61

#### Scenario: An unbiased setting on a scene with fireflies

- **WHEN** a trial changes only how lights are picked, and the two images of a
  pair differ by a handful of firefly pixels
- **THEN** those pixels are excluded from the shift, the trial is not
  `biased`, and its verdict is decided by efficiency alone

#### Scenario: A shift within the noise

- **WHEN** a trial's luminance shift is 0.03 with a z of 1.5
- **THEN** the crop is not `biased`

### Requirement: Picture findings

After the baseline, the diagnostic SHALL report what makes the scene's picture
fragile, as static findings, each with numeric evidence:

- `clamp_bias` (kind `correctness`): the authored indirect clamp would remove at
  least 5% of the baseline's luminance. Evidence: the limit, the share of
  luminance removed, and the share of pixels touched.
- `firefly_energy` (kind `noise`): the brightest 0.1% of the baseline's pixels
  hold at least 20% of its luminance, not counting light seen directly by the
  camera or in a single glossy or mirror reflection (the `emission` and
  `direct_glossy` rows). Evidence: that share, the baseline's spp, and the
  noise-breakdown row holding the most of those pixels' luminance.
- `light_sampling_misses` (kind `noise`): tier 3's light-sampling reach is below
  0.9 on some crop, with a z beyond 4. Evidence: the lowest reach, its z, and
  its crop.
- `visualization_strategy` (kind `correctness`): the authored sampling strategy
  is `light` or `bsdf`, which do not converge to the same image as MIS on every
  scene. Action: `--strategy power`, `crust:samplingStrategy = power`.

`clamp_bias`, `firefly_energy` and `light_sampling_misses` SHALL carry the action
`none`, with the reason that no crust setting makes these paths reachable by
light sampling. They SHALL NOT block `converged`.

#### Scenario: A clamp that removes most of the image

- **WHEN** the default clamp would remove 66% of the baseline's luminance,
  touching 7.6% of its pixels
- **THEN** a `clamp_bias` correctness finding reports both shares and the
  limit, with action `none`

#### Scenario: Energy only BSDF sampling finds

- **WHEN** light sampling alone reaches 39% of a crop's energy
- **THEN** a `light_sampling_misses` noise finding reports the reach, and the
  Verdict's top-noise-source line cites it

#### Scenario: A scene without fireflies

- **WHEN** the Cornell box is diagnosed
- **THEN** none of `clamp_bias`, `firefly_energy` or `light_sampling_misses`
  is reported

## MODIFIED Requirements

### Requirement: Time budget

`--budget DURATION` (e.g. `90s`, `5m`; default `120s`) SHALL bound the
time spent after import. Import time SHALL be reported but not counted.

- Static checks and the full-frame baseline SHALL always run.
- The tiers SHALL then run in order (unbiased swaps, sample budget,
  picture-changing) for as long as the budget allows.
- Once the baseline has run, the diagnostic SHALL reserve for tiers 2 and 3
  their estimated render cost, not a fixed share of the budget. Tier 1 SHALL be
  allowed to spend everything else. The report SHALL state both reservations.
- A trial or measurement of any tier whose estimated cost would overrun the
  budget SHALL NOT be started. It SHALL be listed under `not_tried` with reason
  `budget`.

#### Scenario: A short budget

- **WHEN** the user passes `--budget 10s` on a scene whose baseline alone
  takes 8 s
- **THEN** the report contains the static findings and the baseline, lists
  the tier-1 trials it could not start under `not_tried`, and the exit
  status is 3

#### Scenario: Later tiers need little

- **WHEN** 100 s remain after the baseline and tiers 2 and 3 are estimated
  at 3 s together
- **THEN** tier 1 may spend 97 s, and the report states the two reservations

#### Scenario: Tier 1 overruns

- **WHEN** tier 1 runs until the budget is gone
- **THEN** every tier-2 and tier-3 measurement that did not run is listed
  under `not_tried` with reason `budget`

### Requirement: Efficiency trials and verdicts

Each tier-1 trial SHALL change one setting from the baseline. The trials
SHALL cover:

- the other MIS heuristic (`power` or `balance`);
- each other light-selection mode;
- light samples per camera vertex of 2 and 4 (when the current value is 1);
- light samples per indirect vertex of 2;
- path guiding toggled.

Tier 1 SHALL NOT try the light-only or BSDF-only strategies. They are
visualization modes, and do not converge to the same image as MIS on every scene.

After those, one combined trial SHALL apply the best `better` value of
every factor.

Measurement:

- A trial SHALL be measured on each crop as R interleaved pairs of
  baseline and trial renders (default R = 3, `--repeats`).
- **Seeds.** Both renders of pair *i* SHALL use the same sampler seed, and each
  pair a different one. Pair 0 SHALL use the scene's own seed. Seeds SHALL be a
  fixed function of the scene's seed and *i*, so two runs render the same
  images. The report SHALL list them.
- **MRSE.** An image's MRSE SHALL be the mean, over the crop's pixels, of its
  per-pixel variance over the reference's squared luminance. Within a pair, it
  SHALL exclude the pixels in the top 0.1% of either image by that ratio: the
  same pixels on both sides. The untrimmed MRSE SHALL be reported beside it.
- **ΔEff.** Each pair's `ΔEff` SHALL be
  `(render_B · MRSE_B) / (render_T · MRSE_T)`, with render time excluding setup.
  Setup (the `learned` pre-pass, guiding's training) SHALL be reported
  separately. The untrimmed ΔEff SHALL be reported beside it.
- **Reference.** MRSE SHALL be measured against one reference per crop: the
  inverse-variance blend of the baseline's images of every seed, and of every
  trial image of that crop that is not `biased`.
- **Noise floor.** A crop's noise floor SHALL be the ratio of the largest to the
  smallest MRSE among the baseline's images of different seeds, each trimmed on
  its own top 0.1%. With R = 1 it is unmeasured, taken as 1, and reported as
  `null`.

Per crop, the verdict SHALL be the first that applies:

- `biased` when the crop fails the picture check;
- `better` when every pair exceeds both 1.05 and the noise floor, and the
  untrimmed median ΔEff exceeds 1;
- `worse` when every pair is below both 0.95 and the noise floor's
  reciprocal, and the untrimmed median ΔEff is below 1;
- `insufficient_samples` when the noise floor exceeds 1.10, so the probe could
  not have resolved a gain worth suggesting;
- `inconclusive` otherwise.

Overall:

- the overall ΔEff SHALL be the geometric mean of the per-crop medians;
- the overall verdict SHALL be:
  - `biased` when any crop is `biased`;
  - otherwise `mixed` when one crop is `better` and another is `worse`;
  - `better` when at least one crop is `better`, none is `worse`, and the overall
    ΔEff exceeds 1.05; `worse` symmetrically;
  - `insufficient_samples` when no crop is `better` or `worse` and at least one is
    `insufficient_samples`;
  - `inconclusive` otherwise.

**At the target.** Every trial SHALL report its ΔEff at the target: the ratio of
the baseline's to the trial's projected full-frame time to reach the target MRSE
(tier 2's, or without one, the baseline's error at the scene's samples per
pixel). Each projected time SHALL be:

- the side's setup at full-frame scale;
- plus its render time per sample, times the samples it needs.

For a trial without setup it equals the overall ΔEff.

A trial SHALL become a suggestion only when it is overall `better`, with both an
overall ΔEff and a ΔEff at the target of at least 1.10.

#### Scenario: Within the noise

- **WHEN** a trial's per-pair ΔEff values are 1.03, 1.08 and 0.98 on every
  crop, and the noise floor is 1.04
- **THEN** its verdict is `inconclusive`, and it is not suggested

#### Scenario: Crops disagree

- **WHEN** guiding is `better` on the high-variance crop and `worse` on the
  median crop
- **THEN** the trial's overall verdict is `mixed`, and both per-crop
  results are reported

#### Scenario: Strategies tried

- **WHEN** a scene authored with the default `power` strategy is diagnosed
- **THEN** tier 1 tries `strategy=balance`, and no trial uses the `light` or
  `bsdf` strategy

#### Scenario: The error moves between seeds

- **WHEN** the baseline's MRSE on a crop is 2.7, 6.1 and 3.4 for its three
  seeds, and a trial's pairs give 1.3, 0.8 and 1.9
- **THEN** the noise floor is 2.26, the crop is `insufficient_samples`, and no
  pair is called a gain

#### Scenario: Trimming withholds, never creates

- **WHEN** a trial's trimmed pairs all exceed 1.05 and the noise floor, but
  its untrimmed median ΔEff is 0.7
- **THEN** the crop is not `better`

#### Scenario: Setup that the final render amortises

- **WHEN** `learned` renders a crop's samples 30% more efficiently but its
  pre-pass takes twice the crop's render time, and the projected full-frame
  render takes 70 s
- **THEN** the pairs are judged on render time, the trial can be `better`,
  and its ΔEff at the target counts its pre-pass once against 70 s

#### Scenario: Setup that the final render does not amortise

- **WHEN** a trial is `better` on render time with an overall ΔEff of 1.2, but
  its setup makes its ΔEff at the target 0.9
- **THEN** it is not suggested

### Requirement: Sample budget estimates

Tier 2 SHALL report:

- the samples per pixel needed to reach a target MRSE (default: the square
  of the scene's adaptive threshold; `--target-mrse` overrides it) with the
  best tier-1 settings;
- that sample count's projected full-frame render time;
- the best settings' setup at full-frame scale, apart from the render time;
- the measured effect of adaptive sampling at the authored threshold on each
  crop: the share of pixels stopped early, the mean samples per pixel, and the
  time saved.

Projections SHALL be labelled estimates and SHALL NOT produce suggestions.

#### Scenario: Target from the scene

- **WHEN** the scene's adaptive threshold is 0.05 and no `--target-mrse` is
  given
- **THEN** the target MRSE is 0.0025, and the projected spp and time are
  reported as estimates

#### Scenario: Projected setup

- **WHEN** the best settings use `learned` light selection
- **THEN** tier 2 reports the pre-pass's full-frame time apart from the
  projected render time

### Requirement: Picture-changing settings are measured, not ranked

Tier 3 SHALL report, after every other section:

- the luminance the authored indirect clamp would remove and the share of
  pixels it would touch, measured during the baseline without changing it;
- the share of paths ended by `max_depth`, and, on one crop, the time saved
  and mean luminance change at half the depth;
- the subdivision's triangle count, memory and build time;
- the **light-sampling reach** on each crop: the mean luminance of one
  light-only render of the crop over that of the crop's baseline image with the
  same seed and samples per pixel, with its z. Below 1, part of the energy
  arrives only on paths that BSDF sampling finds. Every pixel SHALL count: the
  missing energy is itself carried by the brightest samples. It SHALL be
  `not_applicable` when the scene has no light-list entry or the authored
  strategy is already `light` or `bsdf`. Crops it cannot fit SHALL be listed
  under `not_tried` with reason `budget`.

These SHALL never be ranked by efficiency, nor suggested as a gain.

#### Scenario: Clamp measurement

- **WHEN** a scene with fireflies is diagnosed with the default clamp
- **THEN** the picture-changing section reports the energy the clamp
  removes and the share of pixels affected, and no suggestion proposes
  changing the clamp

#### Scenario: Light-sampling reach

- **WHEN** ALab frame 1004 is diagnosed with a budget that fits tier 3
- **THEN** the picture-changing section reports a light-sampling reach well
  below 1 on its crops, and `--strategy light` appears in no suggestion

### Requirement: Machine-readable report

The JSON report SHALL carry `format: "crust-diagnostic/1"`. Its keys SHALL
be snake_case with units in their names, in this fixed order:

- `format`, `crust_version`;
- `scene`, `effective_settings`, `run`;
- `static_findings`, `baseline`, `noise_breakdown`, `crops`, `trials`;
- `sample_budget`, `picture_changing`, `not_tried`;
- `suggestions`, `converged`, `suggested_command`, `deltas`.

Each suggestion SHALL give:

- the CLI flag and the `crust:*` attribute;
- the expected overall ΔEff, and its ΔEff at the target;
- the ids of its evidence.

`suggested_command` SHALL be a `crust render` command line applying every
suggestion.

The Markdown report SHALL be rendered from the same data, in the same order,
preceded by a short verdict block:

- top time sink;
- top noise source, citing the `firefly_energy` and `light_sampling_misses`
  findings' numbers when they are reported;
- **picture**: every `correctness` finding and every `biased` trial, each with
  its number, or `none`;
- best change;
- converged, and when a trial is `insufficient_samples`, that a larger
  `--budget` is needed to decide it.

Two runs on the same scene and settings SHALL produce reports that differ
only in times and values derived from them.

`converged` SHALL be true when:

- no tier-1 trial meets the suggestion bar;
- no tier-1 trial is `insufficient_samples`;
- no `time` or `noise` finding has an action.

#### Scenario: Stable structure

- **WHEN** the diagnostic runs twice on the same scene
- **THEN** both JSON files have identical keys in identical order, and the
  same seeds, crops, trials and verdict labels unless a verdict sits on its
  threshold

#### Scenario: Converged scene

- **WHEN** every tier-1 trial is `inconclusive`, `worse` or `biased`, and no
  actionable time or noise finding remains
- **THEN** `converged` is true, and `suggestions` is empty

#### Scenario: Not enough samples to say

- **WHEN** no trial is `better`, and one is `insufficient_samples`
- **THEN** `converged` is false, and the verdict block says that more budget
  is needed

#### Scenario: The picture comes first

- **WHEN** the authored clamp would remove 66% of the luminance
- **THEN** the Markdown verdict block's picture line names `clamp_bias` and
  the 66%, before the best change
