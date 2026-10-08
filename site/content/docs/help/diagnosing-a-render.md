+++
title = "Diagnosing a render"
description = "Read crust diagnostic's report, trust its verdicts, and use it in a loop."
date = 2026-10-08T08:00:00+00:00
updated = 2026-10-08T08:00:00+00:00
draft = false
weight = 20
sort_by = "weight"
template = "docs/page.html"

[extra]
lead = '<code>crust diagnostic</code> measures which settings make a stage render faster or cleaner, and says how sure it is. This page explains its report, the rules behind each verdict, and the diagnose → change → diagnose loop it is built for.'
toc = true
top = false
+++

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
2. **Static findings**, from the import and the baseline: textures without a `.tx`, a
   cache that misses, emissive geometry no shadow ray can find, many lights picked
   uniformly, guiding on where direct light dominates, memory near the machine's.
3. **Crops.** Up to three windows of the frame: the noisiest, the slowest, and one
   typical of both (never pure background). Each is at least 128 pixels square and gives
   every worker thread four tiles. `--region` makes that region the only crop.
4. **Tier 1, unbiased swaps.** Each setting that changes only noise and time is tried
   on its own: the other MIS heuristic (`power` ↔ `balance`), every other light
   selection, 2 and 4 light samples at the camera vertex, 2 at the indirect ones,
   guiding on or off. Then the winners of different settings are tried together.
   `--strategy light` and `--strategy bsdf` are never tried: they show what MIS
   balances between, and on a scene with light only one of them reaches they render
   darker while looking less noisy.
5. **Tier 2, sample budget.** The samples per pixel the best settings need to reach a
   target error, the projected render time, and adaptive sampling measured on each crop.
6. **Tier 3, picture-changing settings.** What the clamp removes, how many paths
   `max_depth` cuts and what half the depth saves, what subdivision costs. These change
   the picture, so they are measured and never ranked.

Tier 1 gets 70% of what the baseline leaves, tiers 2 and 3 15% each; what one tier
leaves unspent passes to the next. A trial whose estimated cost would overrun is not
started, and is listed under `not_tried` with the reason `budget`.

## Efficiency, and how it is measured

A trial is judged by **efficiency**, `E = 1 / (time × MRSE)`, where MRSE is the mean
over pixels of the variance of the pixel over its squared luminance. Error falls as
1/spp while time grows as spp, so `E` does not depend on the sample count: a setting
that is twice as slow but has a quarter of the error is twice as efficient. The report
gives `ΔEff = E_trial / E_baseline`: above 1 the trial is better.

Three rules keep the measurement honest:

- **Interleaved pairs.** On each crop, the baseline and the trial are rendered
  alternately, `--repeats` times (B T B T B T), so background load lands on both sides.
  Each pair gives one ΔEff.
- **One reference per crop.** Every MRSE is measured against the same image: the
  inverse-variance blend of every unbiased image of that crop. Measuring each image
  against its own noisy mean would bias the comparison.
- **Unbiased conditions.** The clamp is off and adaptive sampling is off, so no trial
  can "win" by removing energy or by stopping where it happens to look converged.

Time includes **setup** — the `learned` light selection's pre-pass and guiding's
training — because a render pays it. Setup does not grow with the image the way sampling
does, so it is reported separately, and full-frame projections leave it out.

## Verdicts and their thresholds

| verdict | rule |
|---------|------|
| `better` | on a crop: **every** pair above 1.05 |
| `worse` | on a crop: **every** pair below 0.95 |
| `inconclusive` | anything else — within ±5% is noise, never a gain |
| `mixed` | overall: one crop `better` and another `worse` |

The **overall ΔEff** is the geometric mean of the per-crop medians. A trial is `better`
overall when at least one crop is better, none is worse, and the overall ΔEff itself
clears 1.05. Pairs of 1.03, 1.08 and 0.98 are `inconclusive`, and not suggested.

A trial becomes a **suggestion** only when it is `better` with an overall ΔEff of at
least **1.10**. The combined trial is suggested when it beats the best single one.

The scene is **converged** when no trial is `better` by 1.10 or more and no `time` or
`noise` finding has an action left to take.

## Reading the report

The Markdown on stdout and the JSON at `--json` hold the same data in the same order;
the Markdown starts with a short **Verdict**: the top time sink (the largest profile
section), the top noise source, the best change, and whether the scene has converged.

| section | holds |
|---------|-------|
| `scene` | the stage, frame, camera, resolution and region — what makes two reports comparable |
| `effective_settings` | every setting the diagnosis ran with, each with the flag and the `crust:*` attribute that set it |
| `run` | budget, time used, import time, threads, repeats, the probe conditions, each phase's time, where the budget ran out, the exit status |
| `static_findings` | each with an id, a kind (`time`, `noise`, `memory`, `correctness`), numbers, and an action: a flag and/or attribute with a value, or `none` with the reason crust has no setting for it |
| `baseline` | its spp and time, MRSE, rays per second, path statistics, the largest profile sections, cache hit rates, peak memory |
| `noise_breakdown` | the relative error of each kind of light path, and of each light group |
| `crops` | each crop's rectangle (image pixels, top-left origin), why it was chosen, its share of the baseline's work, and its reference's own error |
| `trials` | per crop: every pair's ΔEff, the median, min and max, both sides' error and time, the verdict; then the overall ΔEff and verdict |
| `sample_budget` | estimates only: spp and time to a target error; adaptive sampling measured per crop |
| `picture_changing` | the clamp, `max_depth` and subdivision numbers |
| `not_tried` | every trial that did not run, and why: `budget` or `not_applicable` |
| `suggestions` | each with its flag, attribute, value, expected overall ΔEff and the trials that are its evidence |
| `converged`, `suggested_command` | the verdict, and a `crust render` line applying every suggestion |
| `deltas` | with `--baseline` only: see below |

Keys are snake_case with their units in the name (`time_s`, `mem_bytes`); numbers carry
four significant digits; a number that could not be measured is `null`. Two runs on the
same scene differ only in times and what is derived from them.

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
reliably help here. If everything reads `inconclusive` on a busy machine, give it more
`--budget` or `--repeats`.

## Limitations

- **Crops are not the frame.** Three crops chosen for different reasons cover the usual
  cases, and disagreement between them is reported as `mixed`; full-frame projections
  are labelled estimates and never produce suggestions.
- **Guided renders are not repeatable.** Whether a guided render's last pass is guided
  depends on an efficiency measured in wall-clock time, so two guided renders of the
  same settings can differ. Each pair's error is therefore measured on its own images.
- **Guiding has no flag.** A guiding suggestion names `crust:pathGuiding` only; the
  suggested command lists it in a trailing comment, to be authored on the stage.
- **Small crops measure the thread pool.** A frame smaller than the crop size is one
  crop, the whole frame, and a `--region` smaller than it is used as given, with a
  warning: either way, on many threads, part of what is timed is the pool's ramp-up.
- **Subdivision's build time is not recorded**, so that number is `null`.
- **Thread time, not wall time.** A crop's baseline time is the time its tiles took on
  their workers, summed: a share of the work.
