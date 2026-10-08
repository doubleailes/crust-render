# diagnostics — design record

> Design record for the **diagnostics** capability (`crust diagnostic`): the
> reasoning, measurements and history behind the behaviour `spec.md` states.
> Introduced by the `add-diagnostic-command` change; its design is carried here
> with what implementation found. `docs/architecture.md` is the map; the user's
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

## Efficiency and its reference (D4, D5)

`E = 1/(time · MRSE)`, MRSE from the tracer's `mean_relative_error` — the one
estimator guiding's own ΔEff uses. Per pair, `ΔEff = (t_B · MRSE_B) / (t_T · MRSE_T)`
with `time = setup + render`.

The reference of a crop is the inverse-variance blend of every unbiased image of
it: the first baseline and each tier-1 trial's first image
(`trials::reference`). Single trials are judged once against the singles'
blend, to pick the combined trial's winners; then every trial, the combined one
included, is re-judged against the final blend. Each crop reports its
reference's own estimated MRSE.

**Repeats are interleaved** (`B T B T …`, `--repeats`, default 3), so load lands
on both sides. The design assumed a configuration's image is identical across
repeats, so only time varies. Implementation found that false for guiding:
`render_guided` decides whether its final pass is guided from a ΔEff measured
in *wall-clock* time, so two guided renders of the same settings can differ
(`veach_mis` with three training iterations did, in the `reconfigure` test).
Every pair's MRSE is therefore measured on that pair's own images; for an
unguided configuration this changes nothing (`the_baseline_is_deterministic`
pins two baselines, and the instruments, bitwise).

## Verdicts (D5)

ε = 0.05. Per crop: `better` when every pair exceeds 1 + ε, `worse` when every
pair is below 1 − ε, `inconclusive` otherwise (and with any non-finite pair).
Overall ΔEff is the geometric mean of the per-crop medians. The overall verdict
is `mixed` when one crop is better and another worse; `better` when at least
one crop is better, none is worse, *and* the overall ΔEff clears 1 + ε (one
better crop must not carry a geometric mean that is noise); `worse`
symmetrically; else `inconclusive`. All of it is `trials.rs`, unit-tested on
the spec's scenarios.

A trial is suggested only when `better` with an overall ΔEff ≥ 1.10
(`trials::SUGGEST_ABOVE`), the same bar `converged` uses — otherwise a better
trial at 1.07 would be suggested while the report says converged. The
suggestion is the combined trial when it beats the best single one.

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

## Trials, budget, tiers (D8, D10)

Tier 1: the other MIS heuristic (`power` ↔ `balance`; both, when the stage
authors a single strategy); every other light selection (not applicable with
fewer than two lights); light samples 2 and 4 when at 1; indirect light samples
2; guiding toggled. `light` and `bsdf` were trials at first and are not: they
are modes that show what MIS balances between, and on a scene whose light one
strategy alone cannot reach they lose energy while their variance falls. On
ALab (frame 1004) light-only was ranked `better` on every crop, ΔEff 14.31,
and renders 61% darker (beauty mean 0.361 against power's 0.937) — the error
measure never sees the lost energy (`tier_one_never_tries_a_single_strategy`;
the bias guard that would catch any such trial is `harden-diagnostic-verdicts`). A factor that cannot change anything is `not_applicable`
with its reason. Then one combined trial of the best `better` value of each
factor, when at least two factors have one.

The budget covers everything after import. Tier 1 gets 70% of what the
baseline leaves, tiers 2 and 3 15% each; tier ends are cumulative, so unspent
time rolls forward. A trial's estimate is the baseline's sampling seconds per
pixel per spp × crop pixels × spp × 2R, plus setup: the baseline's own per
render, and the trial's — what its factor last cost, else measured before
admission (one `learned` pre-pass, which covers the full frame whatever the
crop) or estimated (guiding's training passes, `guiding_training_spp`: 2 + 2 +
4 + 8 samples of the crops at four iterations). The first version priced an
unseen factor's setup at zero, so a first `learned` or guided trial could
overrun unseen. A trial that would overrun is listed under `not_tried`
(`budget`) and the scheduler moves on to cheaper ones; one that runs out of
budget mid-way is abandoned after its current crop; and a tier 1 that ends past
the whole budget did not complete (exit 3). Trial spp is the largest power of two in
2..=256 that fits every tier-1 trial (combined included) in the tier's share.
`schedule.rs` is pure bookkeeping over seconds, tested on a fake clock.

Tier 2: `spp_to_target = spp_P1 · MRSE_P1 · r / target`, `r` the best trial's
geometric-mean MRSE ratio; target `threshold²` or `--target-mrse`; projected
time scales `render_s` only. Labelled estimates, never suggestions. The
adaptive trial runs each crop at `clamp(4 · min_spp, 64, 1024)` spp, where the
early stop can act, against a fixed-spp time *estimated* from the crop's tier-1
renders (`time_fixed_estimate_s`).

Tier 3: the clamp counter's result, `ended_depth / camera rays`, one half-depth
crop trial, subdivision counts. Never ranked.

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
- `ε` and R are fixed; whether to raise them automatically when baseline pairs
  disagree is open.
