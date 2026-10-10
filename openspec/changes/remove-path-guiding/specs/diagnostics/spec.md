## MODIFIED Requirements

### Requirement: Baseline and noise breakdown

The baseline SHALL render the full frame at a low fixed sample count. It
SHALL report:

- render time and rays per second;
- MRSE (the mean over pixels of variance over squared luminance);
- the profile's largest sections;
- path statistics (mean length, Russian-roulette kill rate, share ended by
  `max_depth`);
- texture and Ptex cache hit rates.

It SHALL break noise down by light transport, giving each component's own
relative error:

- emission seen directly;
- direct diffuse;
- indirect diffuse;
- direct glossy;
- indirect glossy;
- transmission;
- volume;
- emission from non-light emitters.

It SHALL also break noise down by light group:

- one group per `crust:light:lpeTag`;
- with no tags and at most 8 lights, one group per light.

Components SHALL NOT be reported as shares of the beauty's variance.
Labelling lights for the breakdown SHALL NOT change any rendered value.

#### Scenario: Noise from indirect light

- **WHEN** a scene lit mostly by bounce light is diagnosed
- **THEN** the breakdown's indirect diffuse row has the largest relative
  error, and tier 1 runs its indirect light-sample trial before its strategy trials

### Requirement: Efficiency trials and verdicts

Each tier-1 trial SHALL change one setting from the baseline. The trials
SHALL cover:

- the other MIS heuristic (`power` or `balance`);
- each other light-selection mode;
- light samples per camera vertex of 2 and 4 (when the current value is 1);
- light samples per indirect vertex of 2.

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
  Setup (the `learned` pre-pass) SHALL be reported
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

- **WHEN** the `learned` light selection is `better` on the high-variance crop and `worse` on the
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
