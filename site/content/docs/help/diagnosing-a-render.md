+++
title = "Diagnosing a render"
description = "Read crust diagnostic's report, trust its verdicts, and use it in a loop."
date = 2026-10-08T08:00:00+00:00
updated = 2026-10-08T18:00:00+00:00
draft = false
weight = 20
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = '<code>crust diagnostic</code> measures which settings make a stage render faster or cleaner, and says how sure it is. This page explains its report, the rules behind each verdict, and the diagnose → change → diagnose loop it is built for.'
toc = true
top = false
+++

## Check before you render

A diagnostic spends a budget rendering. Before that, or before any long render,
`crust check` answers in the time of one import what the render would do, as text or as
JSON (`--json -`):

```bash
crust check -i shot.usda
```

It imports the stage exactly as the render would, renders nothing, and reports the
camera, resolution and files the render would use, its effective settings, what the
import cost, the diagnostic's findings that need no render (textures without a `.tx`,
many lights picked uniformly, a visualisation strategy, peak memory), and every
[warning](@/docs/reference/warnings.md) the import raised: what crust refused,
approximated or skipped. In a pipeline, `--deny refused,skipped` makes it exit `3`
when the stage raised a warning of those kinds, so a broken stage stops before it is
rendered. See [`crust check`](@/docs/reference/command-line.md#check).

## What it does

```bash
crust diagnostic -i shot.usda --budget 2m
```

The stage is imported once. Then, until the budget is spent:

1. **Baseline.** A 1 spp render times the scene, and sizes a full-frame render at 4 to 64
   samples per pixel (a quarter of the budget). It runs with the indirect clamp **off**,
   adaptive sampling **off** and a **fixed** sample count — the conditions every
   comparison below uses. It measures the noise of each kind of light path, the time
   spent in each 16×16 tile, what the clamp would remove, path statistics and cache hit
   rates.
2. **Findings**, from the import, the baseline and tier 3: textures without a `.tx`, a
   cache that misses, emissive geometry no shadow ray can find (`unlit_emitters`, which
   names the prims and the remedy below), many lights picked
   uniformly, guiding on where direct light dominates, memory near the machine's — and
   what makes the picture fragile: a clamp that removes a visible share of the image, a
   few pixels holding much of the energy, light that only BSDF sampling finds, a
   visualization strategy authored (see [The picture](#the-picture)).
3. **Crops.** Up to three windows of the frame: the noisiest, the slowest, and one
   typical of both (never pure background). Each is at least 128 pixels square and gives
   every worker thread four tiles. `--region` makes that region the only crop.
4. **Tier 1, unbiased swaps.** Each setting that changes only noise and time is tried
   on its own: the other MIS heuristic (`power` ↔ `balance`), every other light
   selection, 2 and 4 light samples at the camera vertex, 2 at the indirect ones,
   guiding on or off. Then the winners of different settings are tried together.
   `--strategy light` and `--strategy bsdf` are never tried: they show what MIS
   balances between, and on a scene with light only one of them reaches they render
   darker while looking less noisy. Every trial is checked for a change of the picture
   before its efficiency counts.
5. **Tier 2, sample budget.** The samples per pixel the best settings need to reach a
   target error, the projected render time and setup, and the
   [adaptive sampling sweep](#tuning-adaptive-sampling): the variance threshold,
   minimum, neighbour tolerance and sample count that reach the target soonest.
6. **Tier 3, picture-changing settings.** What the clamp removes, how many paths
   `max_depth` cuts and what half the depth saves, what subdivision costs, and how much
   of each crop's energy light sampling alone reaches. These change the picture, so
   they are measured and never ranked.

Once the baseline has run, tiers 2 and 3 **reserve** their estimated render cost, and
tier 1 may spend everything else: the trials' sample count is the largest power of two
at which all of tier 1 and the later tiers fit. What one tier leaves unspent passes to
the next. The report states both reserves (`run.tier2_reserve_s`, `run.tier3_reserve_s`).
A trial or measurement whose estimated cost would overrun is not started, and is listed
under `not_tried` with the reason `budget` — in every tier, including when tier 1 ran
long.

## Efficiency, and how it is measured

A trial is judged by **efficiency**, `E = 1 / (render time × MRSE)`, where MRSE is the
mean over pixels of the variance of the pixel over its squared luminance. Error falls
as 1/spp while time grows as spp, so `E` does not depend on the sample count: a setting
that is twice as slow but has a quarter of the error is twice as efficient. The report
gives `ΔEff = E_trial / E_baseline`: above 1 the trial is better.

These rules keep the measurement honest:

- **Interleaved pairs, one seed each.** On each crop, the baseline and the trial are
  rendered alternately, `--repeats` times (B T B T B T), so background load lands on
  both sides. Both renders of a pair use the same sampler seed, and each pair a
  different one: pair 0 the scene's own (`crust:frame`, or `-f`), the next ones a fixed
  step from it, so two runs render the same images. `run.seeds` lists them. Each pair
  gives one ΔEff, and an independent draw of the error.
- **The picture first.** Before its efficiency counts, every trial is checked for a
  change of the picture against the paired baseline (see
  [The picture check](#the-picture-check)).
- **One reference per crop.** Every MRSE is measured against the same image: the
  inverse-variance blend of the baseline's image of every seed and of every trial image
  that passed the picture check. Measuring each image against its own noisy mean would
  bias the comparison; letting a darker image in would bias it too.
- **Trimmed error.** Within a pair, the MRSE leaves out the 0.1% of pixels with the
  largest relative variance on either side — the same pixels on both — so one firefly
  that happened to land on one side does not decide the pair. The untrimmed values are
  reported beside the trimmed ones, and a crop is only `better` (or `worse`) when its
  untrimmed median ΔEff leans the same way: trimming can withhold a verdict, never
  create one.
- **Unbiased conditions.** The clamp is off and adaptive sampling is off, so no trial
  can "win" by removing energy or by stopping where it happens to look converged.

**Setup** — the `learned` and `neural` light selections' pre-pass, the `tree` selection's build and guiding's training — is
reported per pair (`setup_trial_s`) but left out of a pair's ΔEff: on a small crop at a
few samples it would weigh far more than in the render it stands for. It is charged
where a render pays it, in the **ΔEff at the target** (`delta_eff_at_target`): the
baseline's projected full-frame time to reach tier 2's target error, setup included,
over the trial's. The pre-pass counts as measured (it trains over the whole frame
whatever the crop), guiding's training scaled from the crop to the frame. Without
setup on either side it equals the overall ΔEff. It is an estimate, and it can only
veto a suggestion.

## Verdicts and their thresholds

A crop's **noise floor** is how far its error estimate moves when only the seed
changes: the largest over the smallest of the baseline's MRSEs across seeds (each
trimmed on its own top 0.1%). A gain has to beat it.

On each crop, the verdict is the first that applies:

| verdict | rule |
|---------|------|
| `biased` | the trial changed the picture (see below), whatever its ΔEff |
| `better` | **every** pair above both 1.05 and the noise floor, and the untrimmed median above 1 |
| `worse` | **every** pair below both 0.95 and the floor's reciprocal, and the untrimmed median below 1 |
| `insufficient_samples` | the noise floor is above 1.10: the probe could not have resolved a gain worth suggesting |
| `inconclusive` | anything else — the setting does not matter here |

The **overall ΔEff** is the geometric mean of the per-crop medians. Overall, a trial is
`biased` when any crop is; otherwise `mixed` when one crop is `better` and another
`worse`; `better` when at least one crop is better, none is worse, and the overall ΔEff
itself clears 1.05 (`worse` symmetrically); `insufficient_samples` when no crop is
better or worse and one is `insufficient_samples`; `inconclusive` otherwise. Pairs of
1.03, 1.08 and 0.98 under a floor of 1.04 are `inconclusive`, and not suggested; a
baseline whose MRSE reads 2.7, 6.1 and 3.4 across its seeds has a floor of 2.26, and
its crop is `insufficient_samples` whatever the trial's pairs say.

A trial becomes a **suggestion** only when it is `better` with both an overall ΔEff and
a ΔEff at the target of at least **1.10**. A `biased` trial is never suggested, never
one of the winners the combined trial is built from, and never part of a reference.
The combined trial is suggested when it beats the best single one.

The scene is **converged** when no trial clears the suggestion bar, none is
`insufficient_samples`, and no `time` or `noise` finding has an action left to take.

### The picture check

Every tier-1 trial, the combined one included, is checked on each crop against the
baseline rendered in the same pair, with the same seed. The **luminance shift** is
the relative difference of the two crops' mean luminance, `mean_T / mean_B − 1`,
over the crop's pixels except the 1% whose two values differ most — so a handful of
fireflies cannot produce it. Its **z** is the shift over its standard error, from both
images' own per-pixel variance. A crop is `biased` when the shift is beyond **2%**
(under 0.03 stops: below what a lighting review notices) **and** z is beyond **4**, in
**every** pair. Each per-crop result reports both mean luminances, the shift and its z,
so you can read "did this change the picture?" in the report itself.

Why it exists: on ALab (frame 1004), before the check, the diagnostic ranked
`--strategy light` its best change — ΔEff 14.3, `better` on every crop — and the
light-only image is **61% darker** (beauty mean 0.361 against 0.937 with MIS). Its
error was the lowest of every image, because the energy it lost is the energy that
carried the noise; it then dominated each crop's reference and biased every other
trial's measurement too. The light-only and BSDF-only strategies are no longer tried,
and the check guards the trials that remain: every one of them is unbiased by design, so
a `biased` verdict on one of them points at a renderer bug, not at the setting.

### When the verdict is `insufficient_samples`

`inconclusive` says "this setting does not matter here"; `insufficient_samples` says
"the probe cannot tell". The trials ran at too few samples for this scene's noise —
typically one where rare, bright paths carry the picture, which the
[picture findings](#the-picture) then name. A larger `--budget` buys more samples per
trial; `--region` on the crop that matters spends them where they count. Each larger
budget raises the trial spp, but a scene may need more than you will spend: stop at
the budget you accept, and treat what is still `insufficient_samples` then as not worth
changing at that budget.

With `--repeats 1` there is one seed and no noise floor (`noise_floor: null`, taken as
1): such a run cannot report `insufficient_samples`.

## Reading the report

The Markdown on stdout and the JSON at `--json` hold the same data in the same order;
the Markdown starts with a short **Verdict**: the top time sink (the largest profile
section), the top noise source (with the firefly numbers when they are findings), the
**picture** (every correctness finding and every `biased` trial, each with its number,
or `none`), the best change, and whether the scene has converged — or that more budget
is needed to decide.

| section | holds |
|---------|-------|
| `scene` | the stage, frame, camera, resolution and region — what makes two reports comparable |
| `effective_settings` | every setting the diagnosis ran with, each with the flag and the `crust:*` attribute that set it |
| `run` | budget, time used, import time, threads, repeats, the probe conditions, each phase's time, where the budget ran out, the exit status, each pair's seed, and the reserves held for tiers 2 and 3 |
| `static_findings` | each with an id, a kind (`time`, `noise`, `memory`, `correctness`), numbers, and an action: a flag and/or attribute with a value, or `none` with the reason crust has no setting for it. Assembled when the run ends, reported first |
| `baseline` | its spp and time, MRSE, rays per second, path statistics, the largest profile sections, cache hit rates, peak memory |
| `noise_breakdown` | the relative error of each kind of light path, and of each light group |
| `crops` | each crop's rectangle (image pixels, top-left origin), why it was chosen, its share of the baseline's work, and its reference's own error |
| `trials` | per crop: every pair's ΔEff (trimmed, and `delta_eff_untrimmed`), the median, min and max, both sides' error (trimmed and untrimmed), render time (`render_baseline_s`, `render_trial_s`) and setup, the verdict, both sides' mean luminance, the luminance shift and its z, the noise floor; then the overall ΔEff, the verdict and the ΔEff at the target |
| `sample_budget` | estimates: spp, render time and setup to a target error; and `adaptive`, the [adaptive sampling sweep](#tuning-adaptive-sampling) |
| `picture_changing` | the clamp, `max_depth` and subdivision numbers, and the light-sampling reach per crop |
| `not_tried` | every trial or measurement that did not run, and why: `budget` or `not_applicable` |
| `suggestions` | each with its flag, attribute, value, expected overall ΔEff and ΔEff at the target, and the trials that are its evidence |
| `converged`, `suggested_command` | the verdict, and a `crust render` line applying every suggestion |
| `deltas` | with `--baseline` only: see below |

Keys are snake_case with their units in the name (`time_s`, `mem_bytes`); numbers carry
four significant digits; a number that could not be measured is `null`. Two runs on the
same scene differ only in times and what is derived from them.

## Tuning adaptive sampling

Most of a production render's time goes where adaptive sampling sends it, so tier 2
looks for the adaptive settings that reach the target error soonest. It searches over
[`--variance-threshold`](@/docs/reference/command-line.md#variance-threshold),
[`--min-samples`](@/docs/reference/command-line.md#min-samples),
[`--neighbour-tolerance`](@/docs/reference/command-line.md#neighbour-tolerance) and the
sample count [`-s`](@/docs/reference/command-line.md#samples), with the best tier-1
settings. The search costs three renders per crop, however many settings it tries:

1. **Record.** Each crop is rendered once at a fixed sample count, the authored one
   within 64 to 1024 (`spp_recorded`). Every pixel keeps the brightness of each of its
   samples.
2. **Replay.** A pixel's samples are the same whether or not adaptive sampling stops
   it early. So the stop rule can be run again over the recording for any setting, and
   it stops exactly the pixels a real render with that setting would. The grid is:
   - ¼, ½, 1 and 2 × the authored threshold, plus the threshold whose square is the
     target;
   - minimums of 8, 16, 32, 64 and the authored one;
   - tolerances of −1, 0, 1, 2 and the authored one;
   - a quarter, half and all of `spp_recorded`, and the authored count.

   For each setting, every pixel's sample count gives the setting's render time, from
   the recording's time per tile. Its error comes from step 3. A few hundred settings
   take well under a second to replay.
3. **Measure the error.** Each crop is rendered a second time at the same count, with
   another seed. A pixel's error is how far its stopped value is from the second
   render, less the second render's own error. That error is measured too: it is half
   the squared difference between the two full renders, which differ only in their
   seed. A pixel's own noise estimate at its stop can't be used: a pixel stops when
   that estimate happens to be low, so it reads optimistic. The 0.1% of pixels where
   the two renders disagree most, fireflies, are left out for every setting. When
   the budget can't fit the second render, a crop falls back on each pixel's own
   estimate, and `error_from` says `variance`.
4. **Pick.** `best` is the setting that reaches the target error on every crop in the
   least predicted time. If none reaches it, `best` is the one with the lowest error and
   `target_reached` is `false`. `front` lists, per crop, every setting that no other
   beats on both time and error: where it bends is what more time buys.
5. **Verify.** `best` is rendered once per crop with the `sampleCount` and `variance`
   AOVs. The report says whether the rendered `sampleCount` matches the replay in every
   pixel (`verification`). The measured time sits beside the predicted one; it is a
   single run, so read it as a sign only.

`best` becomes a suggestion, one per value it changes (`adaptive_sampling.*`), only when
every crop was recorded and verified, every verification matched, and it is at least
1.10 more efficient than the authored point. Efficiency here is time × error, per crop,
combined across the crops by their geometric mean. A crop the budget skipped means no
suggestion. So does a stage authored above the recording's sample count when `best`
changes the count, since that count was never measured.

The suggested command carries the adaptive values the diagnosis ran with, including any
you passed to it, with the suggested ones in their place. Without a target (adaptive
sampling off as authored, and no
[`--target-mrse`](@/docs/reference/command-line.md#diagnostic)), `best` must be at least
as clean as the authored point on every crop. A fixed count is already the cleanest, so
pass `--target-mrse` to tune such a stage.

With `--adaptive-maps maps.exr`, the evidence is written as images: one EXR at the
scene's resolution, 0 outside the crops, with the layers:

- `authored.sampleCount` and `authored.variance`, as the replay predicts them for the
  authored setting;
- `best.sampleCount` and `best.variance`, as the verifying render wrote them.

Where `sampleCount` is high, the samples went there. Where `variance` is still high
beside it, they were not enough.

{% alert(icon="⚠️") %}
`sample_budget.adaptive` used to be a list of one measurement per crop. It is now the
object described here, in the same `crust-diagnostic/1` format. A report with the old
list still loads as `--baseline`, with `adaptive` read as `null`.
{% end %}

### The noise breakdown

| row | light path expression | the light that… |
|-----|-----------------------|-----------------|
| `emission` | `C[LO]` | the camera sees directly |
| `direct_diffuse` | `C<RD>[LO]` | one diffuse bounce brings from a source |
| `indirect_diffuse` | `C<RD>.+[LO]` | a diffuse bounce brings after more bounces |
| `direct_glossy` | `C<R[GS]>[LO]` | one glossy or mirror bounce brings from a source |
| `indirect_glossy` | `C<R[GS]>.+[LO]` | a glossy bounce brings after more (caustic-like paths too) |
| `transmission` | `C<T.>.*[LO]` | first goes through a surface |
| `volume` | `C<V.>.*[LO]` | first scatters in a volume |
| `unlit_emitters` | `C.*O` | ends on emissive geometry that is not a light — overlaps the rows above |

When `unlit_emitters` lights the scene, the finding of the same name lists up to five
of the emissive prims outside the light list (`prims` counts them all). No render
setting turns them into lights: apply
[`MeshLightAPI`](@/docs/usd/lights.md#mesh-lights) to an emitting mesh so light sampling
finds it, or author a UsdLux light in its place. A mesh that applies `MeshLightAPI` but
could not be made a light is listed with the warning that says why (for example
`light.mesh_light_varying_emission` for a textured glow).

The first seven rows add up to the image. Each row gives its **own** relative error
(`var / mean²`) and its error against the image (`var / image²`), never a share of the
image's variance: the rows' variances do not add up to it. The row with the largest
error against the image is the noise's **dominant** source, and orders the trials when
the budget cannot fit them all: direct rows first try light selection and light samples,
indirect rows first try guiding and indirect light samples, and more than eight lights
first try light selection.

**Light groups** add one row per `crust:light:lpeTag`, or, with no tags and at most
eight lights, one per light, named by its prim path. The labels only route light to the
rows; the image is unchanged by them.

### The picture

Some findings describe the scene rather than a setting: what makes its picture depend
on rare paths. They lead the verdict block, before the best change.

| finding | kind | fires when | evidence |
|---------|------|------------|----------|
| `clamp_bias` | `correctness` | the authored indirect clamp would remove at least **5%** of the baseline's luminance | the limit, the share of luminance removed, the share of pixels touched |
| `firefly_energy` | `noise` | the brightest **0.1%** of the baseline's pixels hold at least **20%** of its luminance, not counting light seen directly or in one glossy or mirror reflection (the `emission` and `direct_glossy` rows: the brightest thing in a pixel without being noise) | that share, the baseline's spp (more samples spread a firefly over more pixels), the row holding most of it |
| `light_sampling_misses` | `noise` | light sampling alone reaches less than **90%** of a crop's energy, with \|z\| beyond 4 | the lowest reach, its z, its crop |
| `visualization_strategy` | `correctness` | the stage authors `--strategy light` or `bsdf` | action: `--strategy power` / `crust:samplingStrategy = power` |

The **light-sampling reach** (`picture_changing.light_sampling_reach`) is one light-only
render of each crop over the crop's baseline image of the same seed and samples, every
pixel counted. Both render the same paths, so `1 − reach` is exactly the share of the
energy that only bounce-hit emission brings: the light MIS has no partner strategy for,
where fireflies come from. It is `null`, and listed as `not_applicable`, when the scene
has no light or the stage already authors a single strategy.

The first three have no setting that fixes them: no crust setting makes these paths
reachable by light sampling, and no clamp value is a gain — the clamp trades this energy
for fireflies, and `--indirect-clamp` / `crust:indirectClamp` is that trade's knob. So
their action is `none`, and they never keep the scene from converging. On ALab the
default clamp removes 66% of the luminance, and the top 0.1% of pixels hold 41% of the
beauty: the scene's noise *is* its picture.

## A worked loop

The report is built for an agent — or a person — running:

```text
diagnose → change one setting → diagnose again with --baseline → stop when converged
```

```bash
# 1. Diagnose.
crust diagnostic -i shot.usda --budget 3m --json r1.json > r1.md
```

Say `r1.md` finds `--light-selection learned` `better` on every crop, overall ΔEff 1.42,
and names it the best change. The suggested command applies it as a flag, so nothing in the
stage has to change to try it:

```bash
# 2. Apply the suggestion as a flag, and compare with the first run.
crust diagnostic -i shot.usda --light-selection learned --budget 3m \
    --baseline r1.json --json r2.json > r2.md
```

`r2.json`'s `deltas` lists `light_selection: power → learned`, the baseline's MRSE
before and after (with each run's baseline spp: error falls as 1/spp), and which
findings and suggestions went away — so you can check the change did what `r1`
predicted. It also gives the baseline's time before and after, but that compares two
runs made minutes apart, under whatever load each met: read it as indicative only.
The evidence that a setting is faster is the trials' interleaved ΔEff. Repeat until `converged` is `true`, then render with
the flags of the last `suggested_command` (adding `-s` and `-o`), or author the same
values on the stage: every suggestion names its `crust:*` attribute.

`--baseline` refuses a report of another scene, frame, camera, resolution, region or
format version: the deltas then say `not comparable` and hold nothing else.

A `mixed` or `inconclusive` verdict is an answer, not a failure: the setting does not
reliably help here. `insufficient_samples` is the probe saying it needs a larger
`--budget` to answer (see above). A `biased` verdict on a tier-1 setting is worth a bug
report: every one of them is meant to change only noise and time.

## Limitations

- **Crops are not the frame.** Three crops chosen for different reasons cover the usual
  cases, and disagreement between them is reported as `mixed`. Full-frame projections
  are labelled estimates and never produce suggestions. The one exception is an
  adaptive setting whose replay a real render has verified on every crop.
- **Guided renders are not repeatable.** Whether a guided render's last pass is guided
  depends on an efficiency measured in wall-clock time, so two guided renders of the
  same settings can differ. Each pair's error is therefore measured on its own images.
- **The adaptive sweep runs unguided.** A guided render's last pass depends on a
  wall-clock measurement, and its image blends every pass, so no replay can say what a
  guided render takes. When the best tier-1 settings are guided, the sweep records and
  verifies them without guiding, and says so (`unguided`).
- **The adaptive sweep's error leaves fireflies out.** It ignores the 0.1% of pixels
  where two renders disagree most. Elsewhere it is unbiased, but measured from one
  pair of renders, so it is noisy on a crop that fireflies dominate. On `veach_mis` its
  predicted gain matched the full frame: 3.94 predicted, 3.95 measured, 5.6× faster
  for 1.4× the error.
- **The adaptive grid is coarse.** Thresholds step by factors of two. To refine around
  `best`, diagnose again with `--variance-threshold` near it.
- **Guiding has no flag.** A guiding suggestion names `crust:pathGuiding` only; the
  suggested command lists it in a trailing comment, to be authored on the stage.
- **Small crops measure the thread pool.** A frame smaller than the crop size is one
  crop, the whole frame, and a `--region` smaller than it is used as given, with a
  warning: either way, on many threads, part of what is timed is the pool's ramp-up.
- **Subdivision's build time is not recorded**, so that number is `null`.
- **The picture check trims.** A bias only a few pixels show — a caustic one setting
  drops — can pass it; the light-sampling reach and `firefly_energy`, which count every
  pixel, report that energy instead.
- **At a few samples, a biased trial's shift is a sign, not a measure.** Where
  fireflies carry the image, the shift depends on which pixels each pair leaves out:
  on ALab at 4 spp guiding read +8–13% brighter, where it is in fact about 7% darker
  (at 32 spp the check read −4% to −13%). Trust the verdict; read the number at a
  larger `--budget`.
- **Thread time, not wall time.** A crop's baseline time is the time its tiles took on
  their workers, summed: a share of the work.
