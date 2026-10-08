## ADDED Requirements

### Requirement: Diagnose without changing anything

`crust diagnostic -i <scene>` SHALL import the stage once and measure it.
It SHALL write no image and SHALL NOT modify the stage or any file beside
it. Its outputs SHALL be:

- the Markdown report on stdout, and nothing else on stdout;
- the JSON report at `--json PATH` (default `crust-diagnostic.json` in the
  working directory);
- the log on stderr.

#### Scenario: Outputs of a run

- **WHEN** the user runs `crust diagnostic -i samples/cornellbox.usda`
- **THEN** stdout holds the Markdown report, `crust-diagnostic.json` is
  written, no `.exr` or `.png` is written, and the stage file is unchanged

### Requirement: Unbiased probe conditions

Every probe and trial image used for an efficiency comparison SHALL be
rendered:

- with the indirect clamp off;
- with adaptive sampling off;
- at a fixed sample count;
- at the scene's resolution.

The report SHALL state these conditions. The only exceptions are tier 2's
adaptive-sampling measurement and tier 3's depth trial. Each of those SHALL
state what it changed.

#### Scenario: An authored clamp does not reach the comparison

- **WHEN** the stage authors `crust:indirectClamp = 2`
- **THEN** every tier-1 image is rendered without a clamp, and the clamp
  appears only under picture-changing settings

### Requirement: Time budget

`--budget DURATION` (e.g. `90s`, `5m`; default `120s`) SHALL bound the
time spent after import. Import time SHALL be reported but not counted.

- Static checks and the full-frame baseline SHALL always run.
- The tiers SHALL then run in order (unbiased swaps, sample budget,
  picture-changing) for as long as the budget allows.
- A trial whose estimated cost would overrun the budget SHALL NOT be
  started. It SHALL be listed under `not_tried` with reason `budget`.

#### Scenario: A short budget

- **WHEN** the user passes `--budget 10s` on a scene whose baseline alone
  takes 8 s
- **THEN** the report contains the static findings and the baseline, lists
  the tier-1 trials it could not start under `not_tried`, and the exit
  status is 3

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
  error, and tier 1 runs its guiding trial before its strategy trials

### Requirement: Representative crops

After the baseline, the diagnostic SHALL choose up to three crops of the
frame from the baseline's per-tile data:

- the highest relative variance;
- the highest render time;
- the tile closest to the median of both, excluding pure background.

Rules:

- A crop overlapping an already chosen crop by more than half its area
  SHALL be replaced by the next best for its criterion, or dropped.
- Crops SHALL be at least 128 pixels square, and large enough to give every
  worker thread at least four tiles, clipped to the frame.
- With `--region`, that region SHALL be the only crop.
- Each crop SHALL be reported with its pixel rectangle and the reason it was
  chosen.

#### Scenario: Region given

- **WHEN** the user passes `--region 0,0,256,256`
- **THEN** every trial renders that region, and the report lists exactly
  one crop with reason `region`

### Requirement: Efficiency trials and verdicts

Each tier-1 trial SHALL change one setting from the baseline. The trials
SHALL cover:

- each other MIS strategy;
- each other light-selection mode;
- light samples per camera vertex of 2 and 4 (when the current value is 1);
- light samples per indirect vertex of 2;
- path guiding toggled.

After those, one combined trial SHALL apply the best `better` value of
every factor.

Measurement:

- A trial SHALL be measured on each crop as R interleaved pairs of
  baseline and trial renders (default R = 3, `--repeats`).
- Each pair's `ΔEff` SHALL be `(time_B · MRSE_B) / (time_T · MRSE_T)`.
- MRSE SHALL be measured against one reference per crop: the
  inverse-variance blend of every unbiased image of that crop.

Per crop, the verdict SHALL be:

- `better` when every pair exceeds 1.05;
- `worse` when every pair is below 0.95;
- `inconclusive` otherwise.

Overall:

- the overall ΔEff SHALL be the geometric mean of the per-crop medians;
- the overall verdict SHALL be `mixed` when one crop is `better` and another
  is `worse`.

Only `better` trials SHALL become suggestions.

#### Scenario: Within the noise

- **WHEN** a trial's per-pair ΔEff values are 1.03, 1.08 and 0.98 on every
  crop
- **THEN** its verdict is `inconclusive`, and it is not suggested

#### Scenario: Crops disagree

- **WHEN** guiding is `better` on the high-variance crop and `worse` on the
  median crop
- **THEN** the trial's overall verdict is `mixed`, and both per-crop
  results are reported

### Requirement: Sample budget estimates

Tier 2 SHALL report:

- the samples per pixel needed to reach a target MRSE (default: the square
  of the scene's adaptive threshold; `--target-mrse` overrides it) with the
  best tier-1 settings;
- that sample count's projected full-frame render time;
- the measured effect of adaptive sampling at the authored threshold on each
  crop: the share of pixels stopped early, the mean samples per pixel, and
  the time saved.

Projections SHALL be labelled estimates and SHALL NOT produce suggestions.

#### Scenario: Target from the scene

- **WHEN** the scene's adaptive threshold is 0.05 and no `--target-mrse` is
  given
- **THEN** the target MRSE is 0.0025, and the projected spp and time are
  reported as estimates

### Requirement: Picture-changing settings are measured, not ranked

Tier 3 SHALL report, after every other section:

- the luminance the authored indirect clamp would remove and the share of
  pixels it would touch, measured during the baseline without changing it;
- the share of paths ended by `max_depth`, and, on one crop, the time saved
  and mean luminance change at half the depth;
- the subdivision's triangle count, memory and build time.

These SHALL never be ranked by efficiency, nor suggested as a gain.

#### Scenario: Clamp measurement

- **WHEN** a scene with fireflies is diagnosed with the default clamp
- **THEN** the picture-changing section reports the energy the clamp
  removes and the share of pixels affected, and no suggestion proposes
  changing the clamp

### Requirement: Static findings

Before rendering, and after the baseline for findings that need it, the
diagnostic SHALL report findings. Each finding SHALL have an id, a kind
(`time`, `noise`, `memory` or `correctness`), numeric evidence, and an
action. The action SHALL be either:

- a CLI flag and its `crust:*` USD attribute with a value;
- or `none`, with the reason crust has no setting for it.

An action SHALL NOT name a flag or attribute crust does not have.

#### Scenario: Textures without `.tx`

- **WHEN** a scene's UV textures have no sibling `.tx` and `--auto-tx` is
  not given
- **THEN** a `time` finding lists their count, with the action `--auto-tx`

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
- the expected overall ΔEff;
- the ids of its evidence.

`suggested_command` SHALL be a `crust render` command line applying every
suggestion.

The Markdown report SHALL be rendered from the same data, in the same order,
preceded by a short verdict block:

- top time sink;
- top noise source;
- best change;
- converged.

Two runs on the same scene and settings SHALL produce reports that differ
only in times and values derived from them.

`converged` SHALL be true when no tier-1 trial is `better` with an overall
ΔEff of at least 1.10, and no `time` or `noise` finding has an action.

#### Scenario: Stable structure

- **WHEN** the diagnostic runs twice on the same scene
- **THEN** both JSON files have identical keys in identical order, and the
  same crops, trials and verdict labels unless a verdict sits on its
  threshold

#### Scenario: Converged scene

- **WHEN** every tier-1 trial is `inconclusive` or `worse`, and no
  actionable time or noise finding remains
- **THEN** `converged` is true, and `suggestions` is empty

### Requirement: Comparing with a previous run

`--baseline PREV.json` SHALL add a `deltas` section. It SHALL hold:

- the change in baseline time and MRSE;
- the effective settings that differ;
- the findings resolved and the findings new;
- the suggestions no longer made.

The comparison SHALL be refused, with a `not comparable` note and no
deltas, when any of these differ: the format version, scene path, frame,
camera, resolution or region.

#### Scenario: After applying a suggestion

- **WHEN** a run suggested `--light-selection learned`, and the next run
  passes that flag with `--baseline` set to the first run's JSON
- **THEN** `deltas` lists `light_selection: power → learned` and the
  baseline MRSE change

#### Scenario: Different camera

- **WHEN** `--baseline` names a report made through another camera
- **THEN** the report says `not comparable`, and holds no deltas

### Requirement: Exit status

`crust diagnostic` SHALL exit with:

- `0` when tier 1 completed;
- `3` when the budget ran out before tier 1 completed, including when the
  baseline overran (the report SHALL still be written);
- `1` on error (e.g. the stage cannot be opened);
- `2` on a usage error.

#### Scenario: Missing scene

- **WHEN** the input does not exist
- **THEN** an error is logged, no report is written, and the exit status is
  1
