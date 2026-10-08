# diagnostics — design record

> Design record for the **diagnostics** capability (`crust diagnostic`): the
> reasoning, measurements and history behind the behaviour `spec.md` states.
> Introduced by the `add-diagnostic-command` change and hardened by
> `harden-diagnostic-verdicts`; their designs are carried here with what
> implementation and calibration found. `docs/architecture.md` is the map; the user's
> view is `site/content/docs/help/diagnosing-a-render.md`.

## Why

Making a render faster or cleaner was expert work: render with `--stats`, guess
which setting matters, A/B it with the tooling `CLAUDE.md` describes, and avoid
its traps — sequential timings that lie by 15%, a biased clamp that "removes
noise", adaptive sampling coupling time to variance. `crust diagnostic` runs
that loop for every unbiased *setting* within a budget and reports the evidence
for a machine reader, so an agent can run
`diagnose → change a setting → diagnose again → stop when converged`.

It is not `bench_ab.sh`: that compares two *binaries* on fixed settings and
stays the tool for code changes.

## Shape

- **Engine in `crust-core/src/diagnostic/`, a thin subcommand in
  `crust-render`.** `diagnostic::run(scene, &Options) -> Report`; the CLI
  parses, calls it, prints `Report::to_markdown()` on stdout, writes
  `Report::to_json()`, maps the exit status. The Markdown is rendered from the
  `Report` value, so it cannot disagree with the JSON. Cache counters come from
  the host through `Options::cache_stats` (a closure over `FileAssets`):
  crust-core decodes no assets and owns no cache.
- **One import.** Every render reuses the imported scene through
  `Renderer::reconfigure(settings)`, which rebuilds only what depends on the
  settings — the light selection and the `learned` pre-pass. `Renderer::new`
  *is* construct + `reconfigure`; `reconfigure_renders_what_new_renders`
  (`tracer/tests.rs`) pins, bitwise on the Cornell box and on `veach_mis` (four
  lights, so `learned` really trains), every setting the diagnostic varies.
- **`Renderer::render_measured(request, Instruments) -> Measured`** is the
  diagnostic's only way to render (crate-private). `Instruments` asks for the
  per-tile timer, the clamp counter, the per-pixel variance map (for a guided
  render, of the blend: `Σ (wₖ/W)² varₖ`, which needs every pass's map kept, so
  it is opt-in), and `quiet` — a guided render's once-per-render INFO line goes
  to DEBUG, since the diagnostic renders many. `Measured` splits time into
  `setup_s` (guiding's training passes) and `render_s`; `reconfigure` returns
  the light selection's setup.

## Probe conditions (D3)

Every efficiency comparison renders with the clamp off, adaptive sampling off
(`with_adaptive_sampling(spp, 0)`), a fixed spp, at the scene's resolution —
crops are sub-images of the frame, bit for bit (`add-render-region`). The
exceptions say what they changed: tier 2's adaptive trial, tier 3's half-depth
trial. The baseline's spp comes from a timed 1 spp calibration:
`clamp(⌊0.25 · budget / t₁⌋, 4, 64)`. The baseline always runs; past the budget
it says `budget_exceeded_in: P1` and the exit status is 3.

## A trap already fallen into: an error measure that cannot see lost energy

The first version tried `--strategy light` and `bsdf` as tier-1 swaps. On ALab
(`renders/alab/alab_aovs.usda -f 1004`, 640×360, 46 lights, `--budget 2m`,
trial spp 4) it named light-only its best change — overall ΔEff 14.31, `better`
on every crop — and wrote it into `suggested_command`:

| trial | MRSE, baseline → trial (crops a, b, c) | median ΔEff |
|---|---|---|
| strategy=light | 2.74→0.339, 7.11→0.403, 10.09→0.501 | 8.2, 18.0, 20.0 |
| strategy=balance | 2.74→2.45, 7.11→6.65, 10.09→9.32 | 1.11, 1.11, 1.09 |
| strategy=bsdf | 2.74→4.03, 7.11→19.7, 10.09→23.3 | 0.77, 0.39, 0.46 |

Full-frame means at 32 spp, clamp off:

| strategy | beauty | `diffuse_indirect` |
|---|---|---|
| power | 0.937 | 0.543 |
| balance | 0.936 | 0.542 |
| bsdf | 0.918 | 0.545 |
| light | 0.361 | 0.068 |

The suggestion rendered 61% darker. Three things hid it. A trial's MRSE is its
*own* variance over the reference's squared luminance, so removing energy is
never charged. Light-only's error was 8–20× below every other image's, so it
carried most of each crop's inverse-variance reference and biased every other
trial's measurement too. And the explanation sat in tier 3, unread: the default
clamp removes 66% of the luminance, and the top 0.1% of pixels hold 41% of the
beauty — the energy light-only misses is the energy the fireflies carry. The
same run showed the efficiency itself fragile: every pair reused one seed, so a
single 4-spp draw decided all of them (`light_samples_indirect=2` read 2.6–5.7×
*noisier*, which an unbiased change adding shadow rays cannot be); setup was
charged to a 4-spp crop (`learned`'s pre-pass 1.3–2.1× the crop's render,
guiding's training about 4×); and the run exited 3 with 34 s unspent, tiers 2
and 3 skipped without a `not_tried` entry. What follows is the design that
answers each.

## Efficiency and its reference (D4, D5; hardened D2, D3, D7, D8, D9)

`E = 1/(render time · MRSE)`, MRSE from the tracer's `mean_relative_error` — the
one estimator guiding's own ΔEff uses. Per pair,
`ΔEff = (render_B · MRSE_B) / (render_T · MRSE_T)`: render time only (below).

**Repeats are interleaved** (`B T B T …`, `--repeats`, default 3), so load lands
on both sides, and **each pair has its own seed**: pair *i* renders both sides
with `with_frame(frame + i · 0x85EB_CA6B)` (wrapping), pair 0 the scene's own,
so it renders exactly the first version's images and two runs render the same
ones (`each_pair_has_its_own_fixed_seed`; `run.seeds`). The step is *not*
guiding's pass step (`GUIDING_PASS_SEED_STEP`, 0x9E37_79B9), which the first
implementation used: a guided render adds `(k + 1) · step` to the pair's seed
for training pass `k`, so pass `k` of pair *i* drew the samples of pair
*i + k + 1*'s final passes — guided pairs, and every pair of a scene authoring
guiding, were correlated with later pairs (found in review). With MurmurHash3's
constant no pair or pass seed meets another over 64 pairs and 32 training
iterations, modulo 2³² as the tracer seeds (`no_pair_or_pass_shares_a_seed`;
the old step gave 2016 collisions). Both sides of a pair share the seed — the common random numbers
the picture check needs — and the pairs are independent draws, so "every pair
agrees" is evidence about the error, not only the time. The seed changes only
sample patterns: the stage's time samples are resolved at import.

The first version assumed a configuration's image identical across repeats.
Implementation found that false for guiding: `render_guided` decides whether
its final pass is guided from a ΔEff measured in *wall-clock* time, so two
guided renders of the same settings can differ (`veach_mis` with three training
iterations did, in the `reconfigure` test). Every pair's MRSE is measured on
that pair's own images; for an unguided configuration a seed renders the same
image every time (`the_baseline_is_deterministic`).

**The picture check comes first** (`trials::luminance_shift`). Per crop and per
pair: `d_q = lum_T − lum_B`; drop the 1% of pixels with the largest `|d_q|` (at
least one, never all); over the rest `shift = Σ lum_T / Σ lum_B − 1`,
`se = √Σ(var_T + var_B) / Σ lum_B`, `z = shift / se`. A crop is `biased` when
`|shift| > 0.02` and `|z| > 4` in **every** pair. Paired against the baseline,
not the reference: the reference is what bias contaminates, and the pair shares
its seed, the most precise comparison there is. `var_T + var_B` ignores the
covariance, which common random numbers make positive, so the error is
overestimated: fewer false flags. The 1% trim keeps fireflies — the pixels
whose 4-sample variance estimate is least trustworthy — from producing a shift;
guiding's pairs share no fireflies, and the trim is what protects them. Both
thresholds, because z alone flags tiny real differences and grows with the
pixel count, and the tolerance alone flags noisy crops; 2% is under 0.03 stops.
Every remaining tier-1 factor is unbiased by design, so a `biased` verdict on
one of them is a renderer bug — a NEE ↔ bounce pair out of step — never a
reason to loosen a threshold. A combined trial that is `biased` while its parts
are not is warned about (the factors interact) and not suggested.

**The reference** of a crop is the inverse-variance blend of the baseline's
image of every seed (once per seed: an unguided baseline renders the same image
for every trial, so the first trial's stand for all) and of every image of
every trial that is not `biased` on that crop (`references`). The picture check
therefore runs before the reference exists. Single trials are judged once
against the singles' blend, to pick the combined trial's winners; then every
trial, the combined one included, is re-judged against the final blend. Each
crop reports its reference's own estimated MRSE.

**MRSE is trimmed** (`trials::mrse_pair`): within a pair, both sides leave out
the pixels in the top 0.1% (at least one) of *either* side's `var / ref²` — the
same set, so the pair compares the same pixels. On a firefly-heavy crop the
MRSE is decided by the few pixels a firefly happened to land on: the lottery
`exr_diff`'s trimmed 0.1% relMSE already answers on ALab. The untrimmed MRSEs
and ΔEff are reported beside. Trimming withholds, never creates: `better` also
needs the untrimmed median ΔEff above 1, `worse` below 1.

**The noise floor** of a crop is `max / min` of the baseline's MRSE across its
seeds, each trimmed on its own top 0.1% (`noise_floors`): how far the error
estimate moves when only the seed changes, at this spp. A gain must beat it.
It is conservative — the two sides of a pair share a seed, so their ratio moves
less than either side — and a no-op trial gives exactly 1. With one repeat it
is `null`, taken as 1. It costs nothing: the renders exist anyway.

**Setup is charged at the target**, not to the crop. A pair's ΔEff is on
`render_s`: setup does not scale like sampling, and on a small crop at a few
samples it measures the probe, not the render. Each trial reports
`delta_eff_at_target = (setup_B + R_B) / (setup_T + R_B / ΔEff_overall)`, with
`R_B` the baseline's projected full-frame render time to reach tier 2's target
(at the baseline's settings: `render_s(P1) · MRSE(P1) / target`), or its render
at the scene's spp without one. Setups are at full-frame scale
(`full_frame_setup`): the `learned` pre-pass as measured (it trains over the
full frame whatever the crop), guiding's training × frame / crop pixels, median
over crops — so a combined trial's is the sum of its factors' — and the
baseline P1's own. A selection other than `learned` counts as no setup at all
(`setup_parts`), so with none on either side the at-target value *is* the
overall ΔEff, exactly. It can only veto a suggestion.

## Verdicts (D5; hardened D7)

ε = 0.05. Per crop, the first that applies (`trials::crop_verdict`): `biased`
when the picture check failed; `better` when every pair exceeds both 1 + ε and
the noise floor, and the untrimmed median exceeds 1; `worse` when every pair is
below both 1 − ε and the floor's reciprocal, and the untrimmed median is below
1; `insufficient_samples` when the floor exceeds 1.10; `inconclusive` otherwise
(and with any non-finite pair). Overall ΔEff is the geometric mean of the
per-crop medians. The overall verdict is `biased` when any crop is, whatever
the efficiency; `mixed` when one crop is better and another worse; `better`
when at least one crop is better, none is worse, *and* the overall ΔEff clears
1 + ε (one better crop must not carry a geometric mean that is noise); `worse`
symmetrically; `insufficient_samples` when no crop is better or worse and one
is `insufficient_samples`; else `inconclusive`. All of it is `trials.rs`,
unit-tested on the spec's scenarios.

`inconclusive` says "this setting does not matter here"; `insufficient_samples`
says "the probe cannot tell" — an agent's next step differs (move on, or raise
`--budget`, or `--region` the crop). 1.10 is `SUGGEST_ABOVE`: a floor above it
means the probe could not have resolved a gain worth suggesting.

A trial is suggested only when it is `better` with an overall ΔEff ≥ 1.10 *and*
a ΔEff at the target ≥ 1.10 (`trials::meets_bar`), the same bar `converged`
uses — otherwise a better trial at 1.07 would be suggested while the report
says converged. A `biased` trial is never `better`, so never a winner for the
combined trial, never suggested. The suggestion is, of the trials that clear
the bar, the combined one when it beats the best single one (`best`).
`converged` also needs no trial `insufficient_samples`: a scene is not
converged because the probe could not see.

## Crops (D6)

Side `S = max(128, 16 · ⌈√(4 · threads)⌉)`, clipped to the frame: four tiles per
worker, or the crop measures the pool's ramp-up. Candidates are S×S windows on
the 16-pixel grid (plus the last start, so the far edges are candidates),
scored by summed-area tables of per-pixel relative variance and time: A the
highest variance, B the highest time, C the ranks closest to the median of
both, skipping windows with no luminance. A candidate with IoU > 0.5 against a
chosen crop gives way to the next best for its criterion. `--region` (or a
stage's own data window) is the only crop; one smaller than S² is warned about,
not refused.

Per-tile time is each work unit's time on its worker, so a crop's
`baseline_thread_s` is thread-seconds — a share of the work, not wall-clock
(on 72 threads a 0.15 s baseline showed a 3.6 s crop).

## Noise breakdown (D7)

The baseline renders a value and a variance var per row (`add-lpe-variance`):

| key | expression |
|---|---|
| `emission` | `C[LO]` |
| `direct_diffuse` | `C<RD>[LO]` |
| `indirect_diffuse` | `C<RD>.+[LO]` |
| `direct_glossy` | `C<R[GS]>[LO]` |
| `indirect_glossy` | `C<R[GS]>.+[LO]` |
| `transmission` | `C<T.>.*[LO]` |
| `volume` | `C<V.>.*[LO]` |
| `unlit_emitters` | `C.*O` |

The parser accepts every form, `<R[GS]>` included (`every_expression_parses`).
Correction to the design: the table is not a partition. The first seven rows
are (the `aovs` spec's partition with glossy and singular reflection merged;
`the_transport_rows_sum_to_the_beauty`), and `unlit_emitters` overlaps them —
every `[LO]` row already ends on `O` too. Rows report their own relative error
and their error against the beauty; never shares of the beauty's variance.

Light groups: one `C.*<L.'tag'>` per authored `crust:light:lpeTag`; with none
and at most 8 lights, the diagnostic labels its own copy of the light list by
prim path. `LightList` now keeps each light's name (`set_name` / `name`, set by
the importer's `tag_last`) beside its tag. Labels route only;
`labelling_lights_changes_no_value` pins the beauty bitwise. A tag a label
cannot hold (a quote ends it) gets no group, with one warning: one expression
that does not compile disables the routing of *every* expression, which left
the whole breakdown black (`an_unwritable_tag_is_left_out_not_the_breakdown`).

The ordering rules are data (`noise::RULES`): more than 8 lights → light
selection first; direct rows dominant → light selection, light samples; indirect,
glossy or volume rows dominant → guiding, indirect light samples. They matter
only when the budget cannot fit tier 1.

## Trials, budget, tiers (D8, D10; hardened D1, D4, D10)

Tier 1: the other MIS heuristic (`power` ↔ `balance`; both, when the stage
authors a single strategy); every other light selection (not applicable with
fewer than two lights); light samples 2 and 4 when at 1; indirect light samples
2; guiding toggled. `light` and `bsdf` were trials at first and are not (see the
trap above): they are modes that show what MIS balances between, each biased by
construction on common scenes — light-only wherever energy reaches a light only
through a surface a shadow ray cannot cross, BSDF-only wherever a light cannot
be hit. They also cost 2 of ALab's 7 trials. Kept behind the picture check, a
1.5% bias with a ΔEff of 3 would still pass (`tier_one_never_tries_a_single_strategy`).
A stage that authors one gets the `visualization_strategy` finding, and both
MIS heuristics are tried — and rightly read `biased` against that baseline. A
factor that cannot change anything is `not_applicable` with its reason. Then
one combined trial of the best `better` value of each factor, when at least two
factors have one.

The budget covers everything after import. Once the crops are chosen, tiers 2
and 3 **reserve** their estimated cost (`schedule::plan`) and tier 1 may spend
the rest; tier ends are cumulative, so unspent time rolls forward. Tier 2's is
the adaptive render of every crop at `spp_a`; tier 3's the half-depth pairs (2R
renders of crop 0) and one light-only render per crop, at the trial spp. Each
render's setup counts too, in the reserves and in every tier-2 and tier-3
admission (`schedule::render_cost_s`): the `learned` pre-pass, which covers
the full frame whatever the crop, and guiding's training, scaled to the crop.
The first version priced those renders by their samples alone, so a `learned`
stage could start a measurement past the budget. Tier 2 is admitted at the
best settings' setup as tier 1 measured it, and reserved at the authored
settings' (the best are not known yet). Trial
spp is the largest power of two in 2..=256 at which every tier-1 trial (combined
included), tier 3's renders and tier 2's cost fit together. The reserves are
those estimates with no margin: the estimates already err long, because the
baseline they come from is profiled. When even 2 spp does not fit, the tiers
keep their priority — tier 1 keeps what its trials need, tier 2 reserves what
it can of the rest, then tier 3. Every tier-2 or tier-3 measurement that does
not fit is listed under `not_tried` (`budget`), whatever ran out first: the
first version skipped tiers 2 and 3 silently once tier 1 overran
(`a_budget_the_baseline_exhausts_lists_every_later_measurement`). `run` reports
both reserves. The first version split what the baseline left 70/15/15;
reserves nothing used are why ALab exited with 34 s unspent.

A trial's estimate is the baseline's sampling seconds per pixel per spp × crop
pixels × spp × 2R, plus setup: the baseline's own per render, and the trial's —
what its factor last cost, else measured before admission (one `learned`
pre-pass, which covers the full frame whatever the crop) or estimated (guiding's
training passes, `guiding_training_spp`: 2 + 2 + 4 + 8 samples of the crops at
four iterations). The first version priced an unseen factor's setup at zero, so
a first `learned` or guided trial could overrun unseen. A trial that would
overrun is listed under `not_tried` (`budget`) and the scheduler moves on to
cheaper ones; one that runs out of budget mid-way is abandoned after its
current crop; and a tier 1 that ends past the whole budget did not complete
(exit 3). `schedule.rs` is pure bookkeeping over seconds, tested on a fake
clock.

Tier 2: `spp_to_target = spp_P1 · MRSE_P1 · r / target`, `r` the best trial's
geometric-mean MRSE ratio; target `threshold²` or `--target-mrse`; projected
time scales `render_s` only, by the best trial's render-time ratio, and the best
settings' full-frame setup is reported apart (`projected_setup_s`). Labelled
estimates, never suggestions. The adaptive trial runs each crop at
`clamp(4 · min_spp, 64, 1024)` spp, where the early stop can act, against a
fixed-spp time *estimated* from the crop's tier-1 renders
(`time_fixed_estimate_s`).

Tier 3: the clamp counter's result, `ended_depth / camera rays`, one half-depth
crop trial, subdivision counts, and the **light-sampling reach**: per crop, one
light-only render at the trial spp against the crop's pair-0 baseline image
(same seed and spp; rendered in tier 3 when tier 1 did not), `reach = Σ lum_L /
Σ lum_B`, with a z as the picture check's but **untrimmed** — the missing energy
*is* the bright tail. With common random numbers the two renders share every
path, so `1 − reach` is the share only bounce-hit emission brings: what MIS has
no partner strategy for. It needs no integrator change, unlike a per-sample
"found by BSDF only" counter, which would cost what the clamp counter did
(below). One render per crop replaces the 2 trials × crops × 2R the dropped
strategy trials cost: 3 renders against 36 on ALab. Not applicable without a
light-list entry, or on a single-strategy baseline. Never ranked.

## Picture findings (D5, D6)

`clamp_bias` (`correctness`, ≥ 5% of the luminance: about 0.07 stops),
`firefly_energy` (`noise`, ≥ 20% of the luminance in the top 0.1% of pixels) and
`light_sampling_misses` (`noise`, a reach below 0.9 with |z| > 4) carry
`action: none`: no crust setting moves energy from BSDF-only paths onto
light-sampled ones, and the clamp is a trade, not a gain. They never block
`converged`. `visualization_strategy` (`correctness`) has the action
`--strategy power`. The findings are assembled when the run ends, since the
reach comes from tier 3, and reported first; the Markdown verdict's **Picture**
line names every correctness finding and every `biased` trial with its number,
and the top-noise-source line cites the firefly numbers.

`firefly_energy` leaves out light seen directly *or in one glossy or mirror
reflection* — the `emission` and `direct_glossy` rows (`noise::SEEN_ROWS`). The
design left out only `emission` ("the brightest thing in a frame without being
noise"); calibration found a light seen in a mirror is the same case:
`veach_mis`'s lights in its glossy plates put 35% of the image in 0.1% of the
pixels at 64 spp, every bit of it `direct_glossy` (a row whose relative error
was 0.0098), and the finding fired on highlights — with an action reason
("no setting makes these paths reachable by light sampling") that is false for
direct light (`highlights_are_not_fireflies`).

## Calibration (harden-diagnostic-verdicts)

On a 72-thread machine, otherwise idle; every threshold of the design held, and
one finding's scope moved. These runs used the first pair-seed step (guiding's,
since replaced — see "Efficiency and its reference"); pair 0, which the reach
and the half-depth trial use, is unchanged.

- **Cornell box** (`--budget 2m`): trials at 128 spp; no trial `biased` (every
  |z| ≤ 1); noise floors 1.002; reach 99.96–100% (|z| ≤ 0.5); none of the picture
  findings. 70 s of the 120 used: 256 spp would not have fit.
- **`veach_mis`** (`--budget 2m`): trials at 64 spp; no trial `biased` (|z| ≤
  1.8). `firefly_energy` fired on highlights — the scope change above. The reach
  of crop_c read 0.754 at z −3.4, just inside the guard: light-only is unbiased
  there, but heavy-tailed on the sharp plates (Known gaps). The suggestion:
  `learned` + 4 light samples + guiding, combined ΔEff 4.44 and 2.78 at the
  target; guiding alone read 1.085 overall but 0.99 at the target.
- **ALab, `--budget 2m`**: P1 at 64 spp in 17 s; 32.8 s reserved for tier 2 and
  3.1 s for tier 3; tier 1 ran 6 trials at **4 spp** (`light_samples=4` did not
  fit: 6.2 s needed, 3.8 s left); 115.6 s of 120 used (the first version left
  34 s), exit 3. `clamp_bias` 66.03% of the luminance on 7.64% of the pixels;
  `firefly_energy` 28.7% (74% of it `indirect_diffuse`); reach 0.37–0.40.
  Floors 1.39–2.04: `light_samples_indirect=2`, `strategy=balance` and
  `light_samples=2` were `insufficient_samples`. Light-only's trimmed shift was
  only −4% to −8% at |z| 2–3: at 4 spp the picture check would not have caught
  it — almost all the energy it misses sits in the 1% of pixels it trims.
- **ALab, `--budget 10m`**: tier 1 ran all 7 trials at **32 spp** in 357 s;
  429 s of 600 used, exit 0. Floors 1.41, 1.35 and 20.2 (one seed of crop_c
  caught a firefly worth twentyfold the error). `light_samples_indirect=2` and
  `light_samples=2` resolved, both `worse`; `strategy=balance` stayed
  `insufficient_samples` at ΔEff 1.03 — a true tie. Reach 0.40–0.45 at z ≈ −10;
  light-only's trimmed shift −8.7% to −12% at |z| 8.5–10.7, caught on every crop.
  Every unbiased-by-construction trial (the light selections, the light samples,
  the other heuristic) moved by under 2.5% at |z| ≤ 0.3.
- **Guiding is `biased` on ALab, and it is**: at 4 spp by +8.2%, +13.0%, +7.1%
  (z 7–13), at 32 spp by −5.5%, −4.3%, −12.5% (z −4.9, −4.6, −15.3; untrimmed
  −6.0%, −2.1%, −17% at |z| ≤ 0.9). A direct test — crop_c rendered at 64 spp,
  clamp and adaptive sampling off, 40 seeds each, untrimmed means with the
  standard error across seeds — gave 0.4611 ± 0.0046 unguided against
  0.4294 ± 0.0055 guided: **6.9% darker, z −4.4**, the guided seeds below the
  unguided mean 33 times in 40. That is the design's premise working: a
  `biased` verdict on an unbiased factor is a renderer problem (`rendering`'s
  Known gaps; [#244](https://github.com/doubleailes/crust-render/issues/244)). It
  is not fixed here.

Two lessons. On a firefly-dominated crop at a few samples, the trimmed shift
says *that* a trial moved energy, not by how much or which way: the same
baseline images' trimmed mean on crop_a ranged 0.20–0.67 at 4 spp, depending on
which 1% each trial's differences trimmed, and guiding's shift had the wrong
sign there. And the check must stay trimmed: requiring the untrimmed shift to
agree would have hidden the guiding bias, whose untrimmed z never passed 1.

## The clamp counter, and what it cost

The counter measures what the authored clamp would remove while the baseline
runs unclamped: per sample, the continuation's `indirect − clamp_indirect(indirect)`,
weighted as the sample is, summed per pixel and divided by the pixel's weight —
the beauty's own estimator, so it matches the clamped render's luminance loss
(`the_clamp_counter_measures_what_the_clamp_removes`, within 1e-4).

Placement was measured with callgrind (cornellbox, `-s 2`, one thread, the
zero-AOV render; parent 4 241 414 429 instructions):

| placement | instructions | Δ |
|---|---|---|
| a per-record check before the clamp, a per-sample branch in `advance_pixel`, a per-pixel gather | 4 253 042 584 | +0.27% |
| the measurement inside the clamp's branch (a copy of the unclamped expression), the gather moved out | 4 255 193 820 | +0.32% |
| everything behind `PROFILE` (the instrumented instantiation) | 4 239 357 618 | −0.05% |

The direct cost of the per-sample branch was only ~2 instructions per sample;
the rest (~25) was register pressure spread across the inlined integrator. So
the counter lives in the `PROFILE` instantiation, which a measuring pass always
takes (`render_pass`: `profiling = enabled || measure_clamp`); its sections
record nothing while profiling is off, and every other render compiles the
counter away. The integrator side is a copy of the ordinary expression inside
the clamp's branch — a pair to keep in step (`docs/architecture.md`,
"Invariants"). The per-tile timer is a branch per work unit.

## Report (D11, D12)

`crust-diagnostic/1`: serde writes a struct's fields as declared, so each
struct in `report.rs` is its object's key order; `the_json_keys_are_in_the_spec_order`
reads the keys off the text (a `serde_json::Value` would sort them). Floats are
`Num`, written with four significant digits and as `null` when not finite.
`deltas` is omitted without `--baseline`.

`harden-diagnostic-verdicts` kept the format `crust-diagnostic/1`: no release
had carried it, and a bump would only have marked a draft (were `/1` shipped
first, the change would have bumped to `/2`, and `--baseline` refuses another
version). Its new keys sit at the end of their objects, so the existing order
holds, and are all `#[serde(default)]`: per crop, both mean luminances, the
shift and its z, the untrimmed MRSEs and ΔEff, the noise floor; per trial and
suggestion, the ΔEff at the target; `run.seeds` and the two reserves;
`sample_budget.projected_setup_s`; `picture_changing.light_sampling_reach`. The
per-crop `time_*_s` became `render_*_s` (setup excluded), read under their old
names through serde aliases — a key that kept its name while changing its
meaning would mislead a reader comparing two runs. `--baseline` still reads a
report written before (`a_report_from_before_the_hardening_still_compares`,
against `snapshots/before_hardening.json`); one naming `strategy=light` shows it
under "no longer made", the right delta.

`--baseline` parses the previous file as a `Value` first, to refuse another
format before reading it as a `Report`; then compares scene path, frame,
camera, resolution and region. Its baseline-time change compares two runs made
minutes apart, which `CLAUDE.md` ("Measuring a change") says lies: it is
labelled indicative, and the evidence for a gain stays each run's interleaved
trials. Each run's calibration picks its own baseline spp, and MRSE scales as
1/spp, so `deltas` carries the spp change beside the MRSE change.

The Ptex hit rate is the reader cache's own, `hits / (hits + misses)`: ptex-rs
counts a hit per cache operation, and one reader lookup of a tiled face can
make several, so dividing by `PtexCacheStats::lookups` could pass 100%.
Report strings reach the Markdown escaped (`|` in table cells, a code span
fenced past any backticks): a light's tag or path is authored text.

Every action names a flag or attribute crust has (`actions_name_only_real_settings`)
or is `none` with the reason. Guiding has no `crust render` flag, so its
suggestions carry the attribute only, and `suggested_command` names them in a
trailing shell comment.

## CLI (D13, D14)

The scene-shaping flags moved into a flattened `SceneArgs` shared by `render`
and `diagnostic`; `RenderArgs` derefs to it so render code and its tests keep
`cli.strategy`. `crust render --help` lists the same 26 flags as before.
`diagnostic` refuses `-s`, `-o`, the colour and statistics flags, `--scanline`
and `--log-file` as unknown (exit 2), requires `-i` (exit 2), and exits 1 when
the stage, `--baseline` or the region cannot be used. Its log goes to stderr, as
`ls`'s does. INFO: one line for the import, one per phase.

## Known gaps

- **Crops are not the frame.** Projections are estimates; disagreement is
  `mixed`.
- **Guided renders are not repeatable**, so guiding trials' pairs vary in error
  as well as time (see above). Fixing it would mean deciding the final pass
  from something other than wall-clock.
- **No guiding flag.** A guiding suggestion cannot be applied by
  `suggested_command`; it has to be authored.
- **Subdivision build time is not recorded** by the importer (`build_s: null`).
- **The baseline's MRSE is against its own image**, the only reference P1 has;
  it is comparable between runs at the same spp, which the calibration may not
  pick twice on a loaded machine.
- **`--auto-tx` writes beside the stage** (the `.tx` files), the one exception
  to "changes no file beside it", as a render would.
- **The baseline is profiled** (for `profile_top`, and because the clamp
  counter lives in that instantiation), so its time carries the profiler's
  overhead; trials are not profiled, and trial estimates from it err long.
- **Per-object noise attribution** waits for object IDs
  (`add-identity-aovs-openexrid`).
- `ε` and R are fixed; the noise floor says when R pairs at this spp cannot
  decide (`insufficient_samples`), but the diagnostic does not pick its spp from
  the scene's noise ahead of time: a larger `--budget` buys more.
- **The picture check trims 1%.** A bias only a few pixels show (a caustic one
  setting drops) can pass it; the light-sampling reach and `firefly_energy`,
  which count every pixel, report that energy instead.
- **A biased trial's shift is a sign, not a measure, at a few samples.** On a
  firefly-dominated crop the trimmed means depend on which pixels each pair
  trims (ALab, 4 spp: guiding read +8–13% where its bias is −7%). The verdict
  held; the number did not. More budget makes it a measure.
- **Tier 2's adaptive test can take much of a short budget.** On ALab at
  `--budget 2m` it reserved 32.8 s (three crops at 128 spp) to report 0.6–4%
  saved, and tier 1 stayed at 4 spp.
- **The reach mixes bias with a heavy tail.** Light-only is unbiased wherever
  every light is reachable by a shadow ray, yet at a few samples its estimate of
  a sharp glossy reflection of a large light is mostly low, with rare large
  samples its own variance estimate misses: `veach_mis`'s crop_c read a reach of
  0.754 at z −3.4 at 64 spp, just inside the |z| > 4 guard. Below 1 then means
  "light sampling rarely finds this energy at this spp", not "never".
- **A reserve can still come up short.** It assumes the authored settings'
  render rate and setup, and that tier 1 renders every crop; slower best
  settings in tier 2, or a tier 1 that rendered nothing, need more. The
  measurement is then not started and is listed under `not_tried` (`budget`),
  and a tier 1 that rendered nothing left its whole share to roll forward.
- **No per-object or per-light attribution** of the missing energy: which path
  or light carries it waits for `add-identity-aovs-openexrid`, and for light
  groups on large rigs.
- **Report details** (found on ALab, not yet fixed): `scene.camera` is `null`
  when the RenderSettings camera is used — it should be the resolved path;
  `suggested_command` restates defaults and omits tier 2's projected `-s`; a rig
  of more than 8 lights with no `crust:light:lpeTag` gets no light-group rows,
  and no hint to author tags.
