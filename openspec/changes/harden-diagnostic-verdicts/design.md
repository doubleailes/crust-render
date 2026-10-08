## Context

See `proposal.md` for the motivation (the ALab run). This section describes the code
the design changes, all of it in `crust-core/src/diagnostic/` as `add-diagnostic-command`
leaves it.

**How a trial is judged today** (`mod.rs::judge`, `trials.rs`):

- `shoot` renders a crop and keeps a `CropImage { lum, var }`: per-pixel luminance, and
  the variance of each pixel's mean.
- `trials::mrse(var, reference.lum)` is the *trial's own* variance over the
  reference's squared luminance, floored at 1e-4. No term compares the trial's
  luminance with anything. A trial that removes high-variance energy lowers its
  MRSE and is never charged for the missing energy.
- `references()` blends, per crop, the first baseline image and *every* trial's
  first image, weighted by `1 / mean(var)`.
- Repeats render the same image for an unguided configuration: same settings, same
  frame seed (`the_baseline_is_deterministic`). Pairs therefore differ in time,
  not in error.
- A pair's ΔEff uses `time = setup + render`. Setup is one of two things:
  - guiding's training passes, rendered on the crop at 2, 2, 4, 8… spp;
  - the `learned` pre-pass, which trains over the full frame whatever the region
    (`light_cache::train` takes the full width and height).

**How the budget is spent today** (`schedule.rs`, `mod.rs`):

- `TIER_SHARES` splits what the baseline leaves 70/15/15. Tier ends are cumulative,
  so leftovers roll *forward*, never back.
- Tier 2's adaptive trial and tier 3's half-depth trial run only when `exceeded` is
  unset. Once tier 1 overruns, both are skipped without a `not_tried` entry. ALab
  showed `adaptive: []` and `half_depth: null`, with nothing listed.
- The sampler seed is `RenderSettings::with_frame`. The stage's time samples were
  resolved at import, so changing it changes only sample patterns. Guiding already
  decorrelates its passes with `base_seed + (k + 1) · 0x9E37_79B9`.

**What ALab measured** (frame 1004, 640×360, 46 lights, `--budget 2m`, trial spp 4):

| trial | MRSE, baseline → trial (crops a, b, c) | median ΔEff |
|---|---|---|
| strategy=light | 2.74→0.339, 7.11→0.403, 10.09→0.501 | 8.2, 18.0, 20.0 |
| strategy=balance | 2.74→2.45, 7.11→6.65, 10.09→9.32 | 1.11, 1.11, 1.09 |
| strategy=bsdf | 2.74→4.03, 7.11→19.7, 10.09→23.3 | 0.77, 0.39, 0.46 |

Light-only's error was 8–20× lower than every other image's, so it carried most of
each crop's reference. Full-frame means (32 spp, clamp 0):

| strategy | beauty | `diffuse_indirect` |
|---|---|---|
| power | 0.937 | 0.543 |
| balance | 0.936 | 0.542 |
| bsdf | 0.918 | 0.545 |
| light | 0.361 | 0.068 |

Three things follow:

- Light-only is not an unbiased swap on this scene.
- Power is right: BSDF-only and balance agree with it.
- The energy light-only misses is the energy the fireflies carry. In the power
  render, the top 0.1% of pixels hold 41% of the beauty and 62% of
  `diffuse_indirect`. The default clamp removes 66% of the luminance.

The other trials of that run (setup and render medians in seconds; baseline render
0.35–0.39 s per crop):

| trial | MRSE, baseline → trial (crops a, b, c) | setup | render |
|---|---|---|---|
| light_samples_indirect=2 | 2.74→11.8, 7.11→40.8, 10.09→26.3 | 0.03 | 0.34–0.37 |
| light_selection=learned | 2.74→16.0, 7.11→155, 10.09→100 | 0.47–0.79 | 0.42–0.53 |
| guiding=true | 2.74→8.84, 7.11→35.3, 10.09→51.3 | 1.36–1.43 | 0.32–0.35 |

Every one of these MRSEs is a single draw: all pairs reuse one seed. Adding shadow
rays cannot make an unbiased estimator 2.6–5.7× noisier. That number measures which
pixels caught a firefly with that sample pattern.

**Facts the design relies on:**

- The pixel filter is applied by filter importance sampling (`filter.rs`): no
  sample is splatted into a neighbour. Pixel estimates are therefore independent,
  and the variance of a crop's mean is `Σ var_q / n²`.
- Sampling is keyed by dimension (`K_*` in `tracer/path.rs`). A trial that changes
  only how a light is picked, or how MIS weighs it, keeps the baseline's BSDF
  directions. The two images of a pair share their paths and their fireflies.
  Guiding changes the directions, so its pairs share neither.
- `LightOnly` drops bounce-hit emission "for lights NEE could have sampled". Its
  mean equals MIS's exactly when every such path is also reachable by a shadow
  ray. That fails behind transmissive surfaces, and anywhere a shadow ray is
  blocked where a bounce ray is not.
- The baseline already renders every noise row per pixel (`noise::measure` reads
  the P1 `AovFilm`), and already counts what the clamp would remove
  (`p1.clamp`).

## Goals / Non-Goals

**Goals:**

- No trial that moves the picture can be suggested, or can bias another trial's
  judgement through the reference.
- The report states, in numbers and before the best change, when the scene's
  picture depends on rare paths.
- Each per-crop result carries the numbers that answer "did this change the
  picture?", so a reader checks it in the report, not with extra renders.
- When the probe cannot resolve a gain, the verdict says so
  (`insufficient_samples`), instead of reporting one draw of the lottery as a
  result.
- Efficiency is judged on what scales with samples. Setup is charged where the
  final render pays it.
- The budget is spent on measurements, not on reserves nothing uses.

**Non-Goals:**

- Choosing the probe's spp from the scene's noise ahead of time. The noise floor
  measures afterwards whether the spp sufficed; a larger `--budget` buys more.
- Judging whether a bias is *wanted*. `biased` means "changes the picture relative
  to this baseline", not "is wrong". Tier 3 settings stay the reader's call.
- Finding *which* path or light carries the missing energy. That needs per-object or
  per-light attribution: `add-identity-aovs-openexrid`, and light groups on large
  rigs.

## Decisions

### D1. `light` and `bsdf` leave tier 1, not just fail a guard

Both are documented as visualization modes, and each is biased by construction on
common scenes:

- light-only, wherever energy reaches a light only through a surface a shadow ray
  cannot cross;
- BSDF-only, wherever a light cannot be hit: point, spot and zero-angle distant
  lights.

Trying them cost 2 of ALab's 7 tier-1 trials, about 28% of tier 1. At best they would
then be flagged. At worst a bias under the guard's tolerance would carry a large
ΔEff into the suggestions.

The strategy factor becomes "the other MIS heuristic": `balance` from `power`, and
`power` from `balance`.

*Alternatives considered:*

- **Keep both, behind the guard.** Rejected: it costs budget, and a 1.5% bias with a
  ΔEff of 3 would pass.
- **Keep `bsdf` when the stage has no delta light.** Rejected: on a large-area-light
  scene the gain is at most the shadow rays' cost, and the rule adds a factor that
  depends on the light inventory.

When the *authored* strategy is `light` or `bsdf`, the baseline is the visualization
mode. Tier 1 still tries `power` and `balance`, and the guard will rightly flag them
`biased`, since they change the picture. The `visualization_strategy` finding then
explains this, with the action `--strategy power`. Nothing special-cases the guard.

### D2. A paired, trimmed luminance-shift test

Per crop and per pair, from the two `CropImage`s:

1. `d_q = lum_T,q − lum_B,q`. Drop the 1% of pixels with the largest `|d_q|`; at
   least one pixel, never all.
2. Over the kept pixels:
   - `shift = Σ lum_T / Σ lum_B − 1`;
   - `se = sqrt(Σ (var_T + var_B)) / Σ lum_B`;
   - `z = shift / se`.
3. A crop is `biased` when `|shift| > 0.02` and `|z| > 4` in every pair.

Why each part:

- **Paired, against the baseline, not the reference.** The test must run *before*
  the reference exists (D3). The pair also shares its seed, which makes it the most
  precise comparison available.
- **`var_T + var_B` ignores the covariance.** Common random numbers make the
  covariance positive, so the true variance of `d` is smaller than this sum. The
  standard error is overestimated: fewer false flags, slightly less power.
  Estimating the covariance would need per-sample pairs, which no image keeps.
- **Trimming 1%.** A bias worth catching moves most of a crop. Fireflies move a few
  pixels, the ones whose 4-sample variance estimate is least trustworthy, because a
  firefly that wasn't sampled leaves the variance underestimated. Guiding's pairs
  share no fireflies (Context), and trimming is what protects them. This mirrors the
  0.1% trimmed relMSE the repo already trusts on ALab (`exr_diff`). The guard trims
  more (1%) because a crop holds about 74k pixels, not a full frame.
- **Both thresholds.**
  - z alone flags tiny, real, irrelevant differences, and z grows with the pixel
    count.
  - The tolerance alone flags noisy 4 spp crops.
  - A 2% exposure difference is under 0.03 stops: below what lighting review notices,
    and far above float-ordering effects.
  - At z > 4 a Gaussian false alarm is 6·10⁻⁵ per test. A run makes about 20 tests
    (trials × crops). Heavy tails make the Gaussian figure optimistic; trimming and
    the conservative standard error pay for that.
- **Every pair.** Each pair has its own seed (D7), so every pair is an independent
  check, for guided and unguided trials alike. One unlucky seed cannot flag a
  trial.

On ALab, light-only's missing energy is concentrated in the brightest pixels, so
trimming removes part of the signal. Power's top 0.1% hold 41% of its beauty against
18% for light-only. The two trimmed means should still differ by tens of percent,
far beyond 2%. The calibration task (tasks §5) measures this, and records it here
when the change is archived.

**The guard doubles as a consistency check.** Every remaining tier-1 factor is
unbiased by design. A `biased` verdict on one of them, say `light_selection=learned`
on ALab's shadow-linked rig, is a renderer bug: a NEE ↔ bounce pair out of step
(CLAUDE.md, "Pairs that must change together"). Calibration that flags an unbiased
factor opens an issue. It never loosens a threshold.

*Alternatives considered:*

- **Compare against the reference, not the pair.** Circular: the reference is what
  bias contaminates.
- **Add bias² to the MRSE, as a true error.** At 4 spp the bias of a 61%-dark image
  dominates anyway, but a 3% bias would only shave the ΔEff and could still leave a
  "better". The picture question deserves its own verdict, not a weight in the
  efficiency.
- **A rank or sign test over pixels.** Robust, but it tests whether *pixels* move,
  not whether the crop's *energy* does, and it has no natural tolerance in stops.

### D3. Bias is judged before the reference is built

The order in `run` becomes:

1. Render every trial's pairs (unchanged).
2. Compute every pair's shift (D2).
3. Build each crop's reference from:
   - the baseline's image of every seed (D7), once per seed, since an unguided
     baseline renders the same image for every trial;
   - every image, of every seed, of each trial that is not `biased` on that crop.
4. Judge efficiency against those references (D8, D9).

Combined-trial winners are drawn only from trials that are not `biased`. The
combined trial is itself guarded. If it is `biased` while its parts are not, the
factors interact, which is a renderer bug as in D2. It is then reported and not
suggested.

### D4. Light-sampling reach, measured in tier 3

On each crop:

- one light-only render at the tier-1 spp;
- its mean luminance divided by that of the crop's tier-1 baseline image of pair 0,
  which has the same seed (the scene's) and spp;
- `reach = Σ lum_L / Σ lum_B`;
- `z` as in D2, but **untrimmed**: the energy light sampling misses *is* the bright
  tail, and trimming would hide the number this exists to show.

No timing is involved, so no pairs and no repeats are needed. An unguided baseline
image is deterministic.

When tier 1 did not render a crop (budget), tier 3 renders that crop's baseline too.
When the baseline is guided, pair 0's baseline is used, since it is an image of the
same seed.

*Why light-only, and not a counter in the integrator:*

- It needs no integrator change. CLAUDE.md's pairs and the zero-AOV instruction
  count stay untouched.
- With common random numbers the two renders share every path, so `1 − reach` is
  exactly the share of energy that only bounce-hit emission brings. That is the
  share MIS has no partner strategy for, and it is where the fireflies come from.
- A per-sample "found by BSDF only" counter would cost what the clamp counter cost
  (`diagnostics/design.md`, +0.3% instructions outside `PROFILE`), for one number.

**Cost.** One crop render per crop, against the 2 trials × crops × 2R that `light`
and `bsdf` cost in tier 1: 3 renders against 36 on ALab.

### D5. Picture findings: thresholds and placement

Placement:

- `clamp_bias` and `firefly_energy` are computed after the baseline and join the
  existing post-baseline checks (`checks::Facts` gains the fields).
- `light_sampling_misses` needs tier 3, so the findings list is completed after tier
  3. The report order is unchanged: `static_findings` comes first in the report, but
  its content is assembled when the run ends.

Thresholds are constants in `checks.rs`, beside `MIN_HIT_RATE`:

- **`clamp_bias` at 5%.** `removed_luminance_share` is the clamp counter's, already
  verified to within 1e-4 of the clamped render's loss. 5% is about 0.07 stops, the
  smallest exposure error worth a correctness line.
- **`firefly_energy` at 20% of the luminance in the top 0.1% of pixels, light seen
  directly or in one reflection aside.**
  - It is computed in `noise.rs` (`top_pixels`), from the film the baseline already
    renders, which has every row per pixel: the beauty minus the `emission` and
    `direct_glossy` rows.
  - A light seen directly is the brightest thing in a frame without being noise,
    which is why direct emission is excluded. A light seen in a glossy or mirror
    reflection is the same case. Calibration found it: `veach_mis`'s lights in its
    glossy plates put 35% of the image in 0.1% of the pixels at 64 spp, every bit of
    it `direct_glossy`, and the finding fired on highlights. Its action reason ("no
    setting makes these paths reachable by light sampling") is also wrong for direct
    light, which NEE reaches. So `direct_glossy` is excluded too.
  - 0.1% is the repo's existing trim.
  - For scale: in a converged, smooth image, 0.1% of the pixels hold of the order
    of 0.1–1% of the energy. ALab held 41%.
  - The share depends on spp, since more samples spread a firefly's energy over more
    pixels, so the evidence carries the baseline's spp.
  - The evidence also names the noise row holding most of those pixels' luminance:
    on ALab, `indirect_diffuse`.
- **`light_sampling_misses` below 0.9, with |z| > 4.** 10% of the energy behind
  paths only BSDF sampling finds is already a visible firefly source at production
  spp.

Calibration (tasks §5) must show none of the three fires on the Cornell box, and that
`veach_mis` is understood. Its small bright lights in glossy plates may
legitimately concentrate energy, and the design record then says why the finding is
right or adjusts the scope.

All three carry `action: none`. No crust setting moves energy from BSDF-only paths
onto light-sampled ones. The clamp *is* a setting, but no clamp value is a gain: it
trades this bias for those fireflies. The `none` reason says so, and names
`--indirect-clamp` / `crust:indirectClamp` as the trade-off's knob without proposing
a value.

### D6. The Verdict block

The verdict block gains two things:

- **Picture.** Every `correctness` finding and every `biased` trial, each with its
  headline number, for example "`clamp_bias`: the clamp removes 66% of the
  luminance". It reads `none` otherwise.
- **The top-noise-source line cites the firefly findings.** When `firefly_energy`
  or `light_sampling_misses` is reported, the line names the row and adds its
  numbers, for example "indirect_diffuse — 41% of the energy in 0.1% of the pixels;
  light sampling reaches 39%".

The block stays five lines. The scene's story is read before the best change. The
converged line also says why it is `no` when a trial is `insufficient_samples`, for
example "no — 3 trials need more samples: raise `--budget`".

### D7. A seed per repeat, the noise floor, and `insufficient_samples`

**Seeds.**

- Pair *i* renders both sides with `seed_i = frame + i · 0x9E37_79B9` (wrapping,
  through `with_frame`), the constant guiding already uses between passes.
- `seed_0` is the scene's seed, so pair 0 renders exactly today's images.
- The seeds are listed in `run.seeds`.
- Both sides of a pair share a seed (the common random numbers D2 relies on), and
  the pairs are independent draws.

Today, "every pair above 1.05" can only filter timing noise, since the error is
identical in every pair. With distinct seeds, agreement across pairs is evidence
about the error too.

**Noise floor.**

- `NF = max_i MRSE(B_i) / min_i MRSE(B_i)` over the baseline's R images, each
  trimmed on its own top 0.1% (D8). It is how far the error estimate moves when only
  the seed changes, at this spp, on this crop.
- A gain must beat it: `better` needs every pair above `max(1.05, NF)`, and `worse`
  every pair below `min(0.95, 1/NF)`.
- This is conservative. The two sides of a pair share a seed, so their ratio moves
  less than either side alone. A no-op trial gives exactly 1.

**`insufficient_samples`.**

- When a crop is neither `better` nor `worse` and `NF > 1.10` (`SUGGEST_ABOVE`), the
  measurement noise exceeds the smallest gain the diagnostic would suggest.
- `inconclusive` says "this setting does not matter here". `insufficient_samples`
  says "the probe cannot tell". An agent's next step differs: move on, or raise
  `--budget` (or `--region` the crop).
- So `converged` is false while any trial is `insufficient_samples`. A scene is not
  converged because the probe could not see.
- With `--repeats 1` there is no floor: NF is `null`, taken as 1, which is today's
  behaviour. The user page says that `--repeats 1` cannot report
  `insufficient_samples`.

**Cost.** None: the number of renders is unchanged. `learned` retrains per seed,
since its pre-pass reads the seed, but `shoot` already reconfigures per render.
Runs stay deterministic, and `the_baseline_is_deterministic` holds per seed.

*Alternatives considered:*

- **A fixed spp floor.** Wrong both ways: a smooth scene resolves at 4 spp, and
  ALab may not at 64. NF measures it.
- **Bootstrapping MRSE over pixels.** At 4 spp the pixels' variance estimates are
  the problem itself, and resampling them cannot recover the fireflies they missed.
  New seeds can.
- **More repeats by default.** Costs budget linearly, and NF from R = 3 comes free
  from renders that exist anyway.

### D8. Trimmed MRSE, over the same pixels on both sides

- Per pair, the trim set is the pixels in the top 0.1% (at least one) of either
  side's `var_q / ref_q²`. Both sides exclude the same set, so a pair compares the
  same pixels.
- The untrimmed MRSE and ΔEff are reported beside the trimmed ones.

**Why trim once seeds vary.** The noise floor says *whether* the probe resolves a
gain; trimming makes it resolve more often. On a firefly-heavy crop, MRSE is
dominated by the few pixels where a firefly happened to land. That is the lottery
the repo already documents, and `exr_diff`'s trimmed 0.1% relMSE is the value it
trusts on ALab. On a 272² crop, 0.1% is 74 pixels.

**Why the union.** Trimming each side on its own top would compare different pixels.

**Trimming withholds, never creates.** A configuration that produces more fireflies,
but in fewer than 0.1% of the pixels, looks better trimmed than it is. So:

- `better` also needs the untrimmed median ΔEff above 1;
- `worse` needs it below 1.

Trimming can resolve a lottery only in the direction the untrimmed numbers already
lean.

*Alternative considered:* **the median of per-pixel relative variance.** Robust,
but blind to tails altogether. It also departs from the estimator guiding's own ΔEff
uses (`mean_relative_error`), which the trimmed MRSE stays a restriction of.

### D9. ΔEff on render time; setup charged at the target

**Pairs are judged on `render_s` alone.** A probe crop is small and its spp low,
while setup does not scale like sampling:

- on ALab, `learned`'s pre-pass was 1.3–2.1× the crop's render, and guiding's
  training about 4×;
- tier 2 projected the full frame at 264 spp (70 s).

Charging setup to a 4-spp crop measures the probe, not the render.

**At the target.** Let `R_B` be the baseline's projected full-frame render time to
reach the target: tier 2's `spp_to_target` at the baseline's settings, times
`P1.render_s / spp_P1`. Then:

```
t*_B = setup_B + R_B
t*_T = setup_T + R_B / ΔEff_overall
ΔEff_target = t*_B / t*_T
```

- With no setup on either side, `ΔEff_target = ΔEff_overall` exactly, so setup-free
  trials are unaffected.
- Setups are at full-frame scale:
  - `learned`: as measured, since its pre-pass trains over the full frame whatever
    the crop;
  - guiding: the crop's training time × frame pixels / crop pixels, median over
    crops, since its passes render the region;
  - combined: the sum of its factors';
  - baseline: P1's own setup, already full frame.
- Without a target (threshold 0 and no `--target-mrse`), `R_B` is the baseline's
  render at the scene's samples per pixel.

**The suggestion bar** becomes `better`, an overall ΔEff ≥ 1.10, and
`ΔEff_target ≥ 1.10`. The at-target value can only veto a suggestion, never create
one. Tier 2 adds `projected_setup_s`, the best settings' full-frame setup.

**On ALab this alone would not have rescued `learned` or guiding.** Their MRSE was
itself 6–22× and 3–5× worse at 4 spp, and guiding trains on 2-sample passes there.
That is D7's case: those trials should read `insufficient_samples` or `worse`, not
lose to their setup.

*Alternative considered:* **report both ΔEffs and let the reader choose.** Rejected:
a suggestion needs one rule. Both numbers are reported anyway.

### D10. The budget reserves what tiers 2 and 3 need

Once the crops are chosen, the diagnostic estimates tiers 2 and 3 with the existing
per-pixel-per-spp cost:

- tier 2: the adaptive trial per crop at `spp_a`, when the threshold is above 0;
- tier 3:
  - the half-depth trial: 2R renders of crop 0;
  - the reach: one light-only render per crop, plus any baseline tier 1 will not
    have rendered.

Tier 3 renders at the trial spp, which itself depends on what tier 1 gets. So
`trial_spp` picks the largest power of two at which tier 1's trials, tier 3's
renders and tier 2's fixed cost all fit in what the baseline left.

- The two reserves are that estimate, with no margin: the estimates already err
  long, because the baseline they come from is profiled (a known gap).
- Tier 1 may spend everything but the reserves. Roll-forward stays.
- `run.tier2_reserve_s` and `run.tier3_reserve_s` report them.
- Every tier-2 or tier-3 measurement that does not run for time is listed under
  `not_tried` with reason `budget`, including when tier 1 overran. That fixes
  today's silent skip.
- Exit 3 keeps its meaning: tier 1 did not complete.

*Alternative considered:* **keep the shares, and run the skipped tier-1 trials after
tier 3.** Rejected: tier 2's projection and the combined trial are built on the best
tier-1 result, and a late trial could change it after the fact.

### D11. Report format and `--baseline`

- **New keys, all at the end of their objects so the existing order holds:**
  - per crop trial: `mean_luminance_baseline`, `mean_luminance_trial`,
    `luminance_shift`, `luminance_shift_z`, `mrse_baseline_untrimmed`,
    `mrse_trial_untrimmed`, `delta_eff_untrimmed`, `noise_floor`;
  - per trial: `delta_eff_at_target`;
  - per suggestion: `expected_delta_eff_at_target`;
  - `run.seeds`, `run.tier2_reserve_s`, `run.tier3_reserve_s`;
  - `sample_budget.projected_setup_s`;
  - `picture_changing.light_sampling_reach`: per crop, `{crop, reach, z}`, or
    `null`.
- **Renamed keys.** Per crop trial, `time_baseline_s` / `time_trial_s` become
  `render_baseline_s` / `render_trial_s`: render time, setup excluded.
  `setup_trial_s` stays. A key that kept its name while changing its meaning would
  mislead a reader comparing two runs. A serde alias reads the old names.
- **New verdict values** `biased` and `insufficient_samples`, and the four new
  finding ids.
- **The format stays `crust-diagnostic/1`.** `add-diagnostic-command` is not
  archived, and no release has carried `/1`. A version bump now would only mark a
  draft. If `/1` ships before this change lands, the change bumps to
  `crust-diagnostic/2`, and `--baseline` then refuses a `/1` report as
  `not comparable`, as it does for any other version.
- **Reading an older `/1` report.** Every new field is
  `#[serde(default)]`, so `--baseline` still reads a `/1` report written before
  this change. A report naming `strategy=light` as a suggestion then shows that
  suggestion under "no longer made", which is the right delta.

## Risks / Trade-offs

- **A heavy-tailed crop flags an unbiased trial.** Mitigations: trimming, the
  conservative standard error, both thresholds, and every pair. Calibration on three
  scenes. A false flag costs a missed suggestion, not a wrong one: the guard fails
  safe.
- **Trimming hides a bias concentrated in a few pixels.** That is a bias only a few
  pixels show, for example a caustic one setting drops. At 1% the loss is bounded by
  what those pixels carry. `light_sampling_reach` (untrimmed) and `firefly_energy`
  report exactly that energy, so it is not silent.
- **The reach is noisy at 4 spp on its own.** Common random numbers make the ratio
  far tighter than either mean, and the finding requires |z| > 4. With fewer samples
  the reach is reported, and the finding stays quiet.
- **Dropping `bsdf` loses a legitimate, rare win.** Accepted: on a scene lit
  only by large area lights, BSDF-only saves at most the shadow rays, and no
  production scene the repo measures is such a scene.
- **Findings assembled after tier 3.** If the budget stops before tier 3, there is
  no `light_sampling_misses` finding, and `not_tried` lists the reach with reason
  `budget`. That is the existing contract for tier 3.
- **The noise floor makes verdicts rarer on noisy scenes.** That is the point.
  `insufficient_samples` says what to do, and tier 1 now runs fewer trials (D1) on
  more of the budget (D10), so its spp rises. On ALab at `--budget 2m`, expect several
  `insufficient_samples`. Calibration records how much budget resolves them.
- **An agent loops on `insufficient_samples`.** Each larger budget raises the spp,
  but a scene may need more than the user will spend. The user page says to stop
  at the budget you accept, and to `--region` the crop that matters.
- **The at-target projection is wrong.** It is labelled an estimate, and it can only
  veto a suggestion.
- **The reservation is wrong.**
  - Too large: tier 1 loses some spp.
  - Too small: tiers 2 and 3 skip measurements, now always listed under `not_tried`.

## Migration Plan

Internal to an unreleased command:

- `add-diagnostic-command` is archived first, which creates
  `openspec/specs/diagnostics/spec.md`.
- This change then modifies five of its requirements and adds two.
- The user page and the design record are updated in the same change.
- Rollback is reverting the change. No file format other than the report, and no
  render, is affected.

## Follow-ups (not in this change)

From the same ALab run, recorded so they are not lost:

- **Report details.**
  - `scene.camera` is null when the RenderSettings camera is used; it should be the
    resolved path.
  - `suggested_command` restates defaults and omits tier 2's projected `-s`.
  - Rigs with more than 8 lights and no `crust:light:lpeTag` get no light-group rows,
    and no hint to author tags.
