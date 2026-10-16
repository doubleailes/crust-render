# Spec Delta

## MODIFIED Requirements

### Requirement: Diagnose without changing anything

`crust diagnostic -i <scene>` SHALL import the stage once and measure it.
It SHALL NOT modify the stage or any file beside it, and it SHALL write no
image unless `--adaptive-maps` asks for one. Its outputs SHALL be:

- the Markdown report on stdout, and nothing else on stdout;
- the JSON report at `--json PATH` (default `crust-diagnostic.json` in the
  working directory);
- the adaptive maps EXR at `--adaptive-maps PATH`, only when that flag is given;
- the log on stderr.

#### Scenario: Outputs of a run

- **WHEN** the user runs `crust diagnostic -i samples/cornellbox.usda`
- **THEN** stdout holds the Markdown report, `crust-diagnostic.json` is
  written, no `.exr` or `.png` is written, and the stage file is unchanged

#### Scenario: Asking for the adaptive maps

- **WHEN** the user runs
  `crust diagnostic -i samples/cornellbox.usda --adaptive-maps maps.exr`
- **THEN** `maps.exr` is written as well as the JSON report, no `.png` is written,
  and the stage file is unchanged

### Requirement: Unbiased probe conditions

Every probe and trial image used for an efficiency comparison SHALL be
rendered:

- with the indirect clamp off;
- with adaptive sampling off;
- at a fixed sample count;
- at the scene's resolution.

The report SHALL state these conditions. The only exceptions are tier 2's
verifying adaptive renders and tier 3's depth trial. Each of those SHALL
state what it changed. Tier 2's recording render keeps every condition
above: it is a fixed-count render that also keeps each sample's luminance.

#### Scenario: An authored clamp does not reach the comparison

- **WHEN** the stage authors `crust:indirectClamp = 2`
- **THEN** every tier-1 image is rendered without a clamp, and the clamp
  appears only under picture-changing settings

#### Scenario: The recording render is not adaptive

- **WHEN** the stage authors `crust:varianceThreshold = 0.02`
- **THEN** tier 2's recording render of each crop takes the same number of
  samples in every pixel, and only its verifying render stops pixels early

### Requirement: Sample budget estimates

Tier 2 SHALL report:

- the samples per pixel needed to reach a target MRSE (default: the square
  of the scene's adaptive threshold; `--target-mrse` overrides it) with the
  best tier-1 settings;
- that sample count's projected full-frame render time;
- the best settings' setup at full-frame scale, apart from the render time;
- the adaptive sampling sweep described under "Adaptive sampling sweep".

Projections SHALL be labelled estimates. The only projection that MAY
produce a suggestion is a verified adaptive setting (see "A verified adaptive
setting may be suggested").

#### Scenario: Target from the scene

- **WHEN** the scene's adaptive threshold is 0.05 and no `--target-mrse` is
  given
- **THEN** the target MRSE is 0.0025, and the projected spp and time are
  reported as estimates

#### Scenario: Projected setup

- **WHEN** the best settings use `learned` light selection
- **THEN** tier 2 reports the pre-pass's full-frame time apart from the
  projected render time

## ADDED Requirements

### Requirement: Adaptive sampling sweep

Per crop, tier 2 SHALL render once at a fixed sample count with the best tier-1
settings. It SHALL then predict, for every setting of a grid of variance
threshold, minimum samples, neighbour tolerance and sample cap, each pixel's
sample count and the variance of its mean. From these it SHALL derive the
setting's predicted MRSE against the crop's reference and its predicted time
from the crop's measured cost.

#### Scenario: The authored setting is always on the grid

- **WHEN** the stage authors `crust:varianceThreshold = 0.03`,
  `crust:minSamplesPerPixel = 8` and `crust:adaptiveNeighbourTolerance = 1`
- **THEN** `sample_budget.adaptive.authored` holds that setting's predicted
  mean spp, early-stopped share, MRSE and time for every crop

#### Scenario: The sweep's settings are reported

- **WHEN** tier 2's sweep runs
- **THEN** `sample_budget.adaptive.grid` lists every threshold, minimum,
  tolerance and cap it tried, and `sample_budget.adaptive.front` lists, per
  crop, the settings no other setting beats on both predicted time and
  predicted MRSE

#### Scenario: Adaptive sampling off in the stage

- **WHEN** the stage authors `crust:varianceThreshold = 0`
- **THEN** the sweep still runs, the authored point is the fixed-count render,
  and the report says that adaptive sampling is off as authored

### Requirement: The best adaptive setting at the target

Tier 2 SHALL name, as `sample_budget.adaptive.best`, the grid setting whose
predicted MRSE is at or below the target on every crop with the least summed
predicted time. When no setting reaches the target, it SHALL name the one with
the lowest predicted MRSE and say that the target was not reached.

#### Scenario: A lower threshold with a higher cap

- **WHEN** the authored threshold misses the target on one crop and a lower
  threshold with a higher cap reaches it on every crop
- **THEN** `best` is the cheapest such setting, and the authored point is
  reported as missing the target on that crop

#### Scenario: Nothing reaches the target

- **WHEN** the target MRSE is below what the largest cap reaches on a crop
- **THEN** `best` is the lowest-MRSE setting, and `target_reached` is false

### Requirement: The best adaptive setting is verified

Each crop SHALL be rendered once with `best` and adaptive sampling on, with
the `sampleCount` and `variance` AOVs. The report SHALL say, per crop, whether
the rendered `sampleCount` matches the prediction in every pixel. It SHALL
also give the measured render time beside the predicted one, labelled a
single run.

#### Scenario: The prediction reproduces

- **WHEN** the verifying render of a crop takes in every pixel the samples its
  replay predicted
- **THEN** that crop's `verification.sample_count_matches` is true, with
  `mismatched_pixels` 0

#### Scenario: The prediction does not reproduce

- **WHEN** a verifying render's `sampleCount` differs from the prediction in
  any pixel
- **THEN** that crop's `verification.sample_count_matches` is false, the count
  of mismatched pixels is given, and `best` is not suggested

#### Scenario: The budget cannot verify

- **WHEN** the budget left cannot fit a crop's verifying render
- **THEN** that verification is listed under `not_tried` with reason `budget`,
  and `best` is not suggested

### Requirement: A verified adaptive setting may be suggested

`best` SHALL become a suggestion only when every crop's verification
matched, and its ΔEff at the target over the authored setting is at least
1.10. ΔEff is predicted time × predicted MRSE, each summed over crops. The
suggestion SHALL name the changed values' `crust:*` attributes and flags, and
cite the sweep and the verifications as evidence.

#### Scenario: A verified gain

- **WHEN** every crop's verification matched and `best` reaches the target in
  0.7× the authored setting's predicted time at no higher MRSE
- **THEN** a suggestion `adaptive_sampling` gives the changed
  `--variance-threshold`, `--min-samples`, `--neighbour-tolerance` and `-s`
  values, with their attributes, and `suggested_command` applies them

#### Scenario: Within the bar

- **WHEN** `best` is verified and its ΔEff at the target is 1.05
- **THEN** it is reported, and not suggested

### Requirement: Adaptive maps

With `--adaptive-maps PATH`, the diagnostic SHALL write one EXR. It SHALL hold,
for every crop of tier 2, four planes: the predicted `sampleCount` and
`variance` of the authored setting, and the rendered `sampleCount` and
`variance` of `best`. Pixels outside every crop SHALL hold 0. The JSON report
SHALL name the file and its layers.

#### Scenario: Layers of the maps

- **WHEN** tier 2 swept and verified three crops with `--adaptive-maps maps.exr`
- **THEN** `maps.exr` is at the scene's resolution with layers
  `authored.sampleCount`, `authored.variance`, `best.sampleCount` and
  `best.variance`, each non-zero only inside the three crops

#### Scenario: Tier 2 did not run

- **WHEN** `--adaptive-maps maps.exr` is given and the budget ran out before
  tier 2
- **THEN** no maps file is written, and the report says why
