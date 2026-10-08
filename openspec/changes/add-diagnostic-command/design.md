## Context

See `proposal.md` for the motivation. This section describes the code the
design builds on.

**Efficiency is already measured once, for guiding** (`tracer/mod.rs`,
`render_guided`):

- `PassStats { variance, var_map, rays }` per pass;
- `mean_relative_error(var_map, ref_lum)`, the mean of `var / max(lum², 1e-4)`;
- `E = 1/(wall-clock · MRSE)`. Variance scales as 1/spp and cost as spp, so
  E is comparable across budgets.
- `ΔEff = E_pg+/E_pg− < 1` renders the final pass unguided.

**What a render can already explain:**

- `RayStats`:
  - mean path length and RR kill rate;
  - `ended_depth` (paths cut by `max_depth`);
  - shadow rays per vertex;
  - the adaptive early-stop share and samples per pixel;
- `--profile` sections: Trace / EvalBsdfs / Texture / SurfaceLighting;
- texture and Ptex cache hit rates;
- per-phase time and memory;
- the scene inventory: lights, primitives, subdivision, displacement.

**What blocks a trial loop today:**

- `Renderer::new(camera, world: World, lights, settings)` takes the world
  by value and builds light selection inside (`lights.select_by`, plus a
  training pre-pass for `learned`). Every trial would have to re-import.
- No crop rendering and no per-path variance; these come from
  `add-render-region` and `add-lpe-variance`.
- The CLI has `render` and `ls`; render flags live in one `RenderArgs`.

**The measurement rules this design implements** (CLAUDE.md, "Measuring a
change"):

- interleave A and B, never compare sequential wall-clock runs;
- 1–5% differences are below the timing noise floor;
- `--indirect-clamp 0` for unbiased measurements;
- a fixed spp, because adaptive sampling couples time to variance.

## Goals / Non-Goals

**Goals:**

- In a fixed time budget, measure which *unbiased* settings make this scene
  more efficient, with honest confidence.
- Explain where time and noise go, in terms that map to settings.
- Emit a report an LLM can parse, act on, and compare across iterations.

**Non-Goals:**

- Changing the scene or writing settings (diagnose only).
- Ranking settings that change the picture (clamp, depth, subdivision):
  those are measured and handed to the reader's judgement.
- Search over combinations beyond one-factor-at-a-time plus one combined
  check.
- Per-object attribution (needs object IDs; future).
- Replacing `bench_ab.sh`. That remains the tool for code changes; this one
  is for scene settings.

## Decisions

### D1. Engine in `crust-core::diagnostic`, a thin subcommand in `crust-render`

- `crust_core::diagnostic::run(scene, options) -> Report`.
- `Report: Serialize`, plus `Report::to_markdown()`.
- The CLI parses, calls `run`, prints the Markdown, writes the JSON, and maps
  the exit status. This keeps `crust-render` "drives a render, writes
  files", and lets the logic be unit-tested without a process.
- The JSON is the source of truth; the Markdown is rendered *from the
  `Report` value*, so the two cannot disagree.

### D2. One import; `Renderer::reconfigure`

`Renderer::reconfigure(&mut self, settings)` keeps the camera, world,
volumes and asset caches. It rebuilds what depends on the settings:

- `LightList::select_by`, including the `learned` pre-pass;
- light-sample counts, strategy, clamp, spp, adaptive and guiding settings.

`Renderer::new` becomes `new` + `reconfigure`, so the two cannot drift. A
test pins that `new(s)` and `new(s0).reconfigure(s)` render bitwise-equal
images for every setting the diagnostic varies.

### D3. Probe conditions

Every probe and trial runs with:

- `indirect_clamp = None`;
- `min_samples_per_pixel = spp`, `variance_threshold = 0` (adaptive off);
- a fixed spp;
- the scene's own resolution, so crops are bit-identical sub-images
  (`add-render-region`).

Tier 2 is the one place adaptive sampling is switched back on, to measure
it.

P1's spp comes from a calibration pass:

- render 1 spp full frame, timed;
- `spp_P1 = clamp(⌊0.25 · budget / t₁⌋, 4, 64)`.

P1 is mandatory. If even 4 spp overruns the budget, it still runs, and the
report says `budget_exceeded_in: P1` (exit 3).

### D4. The efficiency metric and its reference

- `E = 1 / (t_render · MRSE)`, with MRSE from `mean_relative_error`.
- The **reference** for a crop is the inverse-variance blend (as in
  `render_guided`) of the baseline and every tier-1 image of that crop. All
  are unbiased images of the same pixels, so their blend is the best
  estimate available. It is computed once tier 1 ends, and every trial's
  MRSE is computed against it. Storing each trial's `var_map` and luminance
  for a crop is small.
- **Time is split into `setup_s` and `render_s`:**
  - setup is the `learned` pre-pass and guiding training;
  - render is the sampling.

  E uses their sum on the crop. Projections to the full frame scale only
  `render_s`, because setup does not grow with pixel count the same way.
  The report shows both.

### D5. Repeats, interleaving, verdicts

- A trial is R pairs (default 3, `--repeats`), interleaved on one crop:
  `B T B T B T`. Load lands on both sides, as in `bench_ab.sh`.
- Sampling is deterministic (openqmc), so a configuration's image, and its
  MRSE, are identical across repeats. Only time varies. Task 3.2 verifies
  that assumption; if it fails, MRSE is averaged across repeats.
- Per pair: `ΔEff_i = (t_B · MRSE_B) / (t_T · MRSE_T)`.
- Verdict, with ε = 0.05:
  - `better`: every `ΔEff_i > 1 + ε`;
  - `worse`: every `ΔEff_i < 1 − ε`;
  - otherwise `inconclusive`.
- Reported per crop: median, min and max.
- **Overall ΔEff** is the geometric mean of the per-crop medians.
- **Disagreement**: a crop with `better` while another has `worse`. It is
  reported explicitly, and the overall verdict is `mixed`.
- Only `better` trials are suggested. `inconclusive` is reported as such,
  so the loop does not chase noise.

### D6. Crop selection

- P1 records, per 16×16 tile:
  - the summed relative variance `var / max(lum², 1e-4)`;
  - wall-clock time: one `Instant` per work unit, in a per-tile timer that
    is off outside the diagnostic.
- **Crop side** `S = max(128, 16 · ⌈√(4 · threads)⌉)`, clipped to the
  frame. A crop then holds at least 4 tiles per worker thread. Smaller
  crops measure thread-pool ramp-up, not the scene.
- **Candidates** are tile-aligned S×S windows, scored by a summed-area
  table:
  - A: maximum relative variance;
  - B: maximum time;
  - C: the window whose (relative variance, time) ranks are closest to the
    median of both, skipping windows whose mean luminance is 0 (pure
    background).
- A candidate overlapping an already chosen crop by IoU > 0.5 is dropped,
  and the next best for its criterion is taken. If none remains, there are
  fewer crops, and the budget goes to more repeats.
- With `--region`, that region is the only crop. Too small for the thread
  count gives a warning, not a refusal.

### D7. The noise breakdown, and how it orders tier 1

P1 requests an engine-built `AovRequest` of value and variance vars
(`add-lpe-variance`) for a fixed partition:

| key | expression | meaning |
|---|---|---|
| `emission` | `C[LO]` | seen directly |
| `direct_diffuse` | `C<RD>[LO]` | |
| `indirect_diffuse` | `C<RD>.+[LO]` | |
| `direct_glossy` | `C<R[GS]>[LO]` | |
| `indirect_glossy` | `C<R[GS]>.+[LO]` | includes caustic-like paths |
| `transmission` | `C<T.>.*[LO]` | |
| `volume` | `C<V.>.*[LO]` | |
| `unlit_emitters` | `C.*O` | emission reached only by BSDF sampling |

This is the same partition the `aovs` spec already tests to sum to the
beauty. Each row reports the component's own relative error
(`var / max(mean², ε)`, averaged over the crop) and its relative error
against the beauty's mean. Rows are never reported as shares of the total
(`add-lpe-variance` D5).

**Light groups:**

- lights authoring `crust:light:lpeTag` give one `C.*<L.'tag'>` row each;
- if no light authors one and there are at most 8 lights, the diagnostic
  labels its own in-memory copy of the light list per light, by prim path.
  Labels affect only LPE routing, never the image. Task 3.3 verifies this
  bitwise.

**Ordering rules** (data, in one table in the module, so they are testable
and documented):

- direct rows dominate → `light-selection`, `light-samples` first;
- indirect, glossy or volume rows dominate → `guiding`,
  `light-samples-indirect` first;
- more than 8 lights → `light-selection` first.

The ordering only matters when the budget cannot fit the whole tier.

### D8. The trial set

- **Tier 1** (unbiased; one factor at a time from the baseline):
  - `strategy`: every other value of `power|balance|light|bsdf`;
  - `light-selection`: every other value of `uniform|power|learned`;
  - `light-samples`: 2 and 4, if the current value is 1;
  - `light-samples-indirect`: 2;
  - `guiding`: toggled.

  Then one **combined** trial of every `better` winner (best per factor),
  because factors interact (e.g. learned selection and more light samples).
  The suggestion is the combined trial if it is `better` than the best
  single trial, else the best single.
- **Tier 2** (no new trial images except adaptive):
  - `spp_to_target`: `spp_P1 · MRSE_best / MRSE_target`, with
    `MRSE_target = varianceThreshold²` (a 5% relative standard error per
    pixel ⇒ 0.0025), or `--target-mrse`;
  - the projected full-frame render time at that spp, labelled an
    estimate;
  - one adaptive trial per crop, at the authored threshold with the best
    tier-1 settings: the early-stop share, mean spp, and time saved.
- **Tier 3** (measured, not ranked):
  - **clamp**: a counter, active only when the diagnostic asks, measures
    during P1 the luminance the authored clamp would remove and the share of
    pixels it would touch, without applying it. Its "off" side is today's
    code.
  - **max_depth**: `ended_depth / paths` from P1, plus one crop trial at
    `max_depth / 2` reporting the time saved and the mean luminance
    change.
  - **subdivision**: from stats only (triangles, memory, build time), with
    no trial: changing it means re-importing.

### D9. Static checks (P0)

Each check is a function `(&Scene, &Stats) -> Option<Finding>`. A finding
has:

- `id`, and a `kind` (`time` | `noise` | `memory` | `correctness`);
- `evidence`: numbers;
- `action`:
  - `{ flag, usd_attribute, value }`;
  - or `{ none: "<why>" }`.

The first set:

- UV textures without a sibling `.tx`, with `--auto-tx` off;
- texture or Ptex cache hit rate below 90% (from P1);
- emissive materials outside the light list (`unlit_emitters` > 0) →
  `action: none` unless crust has a supported way to make them lights;
- more than 8 lights with `uniform` selection (feeds tier-1 ordering);
- guiding authored on, while the scene has no indirect-dominant rows;
- peak memory against the machine's total.

New checks are added by adding a function and its test.

### D10. Budget scheduling

- `--budget` takes a duration (`90s`, `5m`; default `120s`). It covers
  everything after import; import time is reported separately.
- **Trial cost estimate:**
  `(render_s per pixel per spp from P1) · crop pixels · spp_trial · 2R`,
  plus the setup cost from the last trial of that kind.
- The scheduler never starts a trial whose estimate would overrun. It
  records it in `not_tried` with `reason: budget`, and continues to cheaper
  ones in the same tier.
- Trial spp is the largest power of two such that each crop's tier-1 block
  fits in the tier's share. Tier 1 gets 70% of what remains after P1, and
  tier 2 and tier 3 get 15% each; unspent share rolls forward.

### D11. The report: `crust-diagnostic/1`

JSON top level, in this order:

- `format`, `crust_version`;
- `scene` (path, frame, camera, resolution, region);
- `effective_settings` (every setting the diagnosis ran with, flag and
  attribute names);
- `run` (budget_s, used_s, import_s, phases, `exit`);
- `static_findings`;
- `baseline` (spp, time_s, mrse, rays_per_s, top profile sections,
  path stats, cache hit rates);
- `noise_breakdown`;
- `crops`;
- `trials` (tier, change, per crop, overall, verdict);
- `sample_budget`;
- `picture_changing`;
- `not_tried`;
- `suggestions` (ordered, each with flag + attribute + expected ΔEff +
  evidence ids);
- `converged`;
- `suggested_command`;
- `deltas` (only with `--baseline`).

Rules:

- Keys are snake_case, with units in names (`time_s`, `mem_bytes`).
- Floats carry 4 significant digits; arrays are in a fixed order.
- The only non-deterministic values are times. A test pins the key
  order.
- The Markdown has the same sections in the same order, with a
  `verdict` block of a few lines first (top time sink, top noise source,
  best change, converged).

`converged = true` when no tier-1 trial (combined included) is `better`
with overall ΔEff ≥ 1.10, and P0 has no finding of kind `time` or `noise`
with an action.

### D12. `--baseline PREV.json`

- Refused, with a "not comparable" note, when the format version, scene
  path, frame, camera, resolution or region differ.
- Otherwise `deltas` lists:
  - the baseline's time_s and MRSE change;
  - `effective_settings` entries that differ (what the agent changed);
  - findings resolved and findings new;
  - suggestions that went away.
- This lets the agent see whether its last action did what the previous
  report predicted.

### D13. Exit status and logging

- Exit status:
  - `0`: tier 1 completed, whether or not later tiers fit;
  - `3`: the budget ran out before tier 1 completed (P1 overrun included);
    the report is still written;
  - `1`: error;
  - `2`: clap usage error.
- `INFO` is bounded: one line per phase.
- Per-trial lines are `DEBUG`.
- stdout holds only the Markdown, as `ls` keeps stdout for its listing.

### D14. Shared flags

- The render flags that shape the scene or its settings move to a clap
  `#[command(flatten)] SceneArgs`, used by both `render` and
  `diagnostic`:
  - `-i`, `-f`, `--camera`, `--region`;
  - `--strategy`, `--light-selection`, `--light-samples[-indirect]`;
  - `--indirect-clamp`, `--filter[-radius]`;
  - `--subdiv-*`, `--auto-tx`.
- `-s` is not shared: the diagnostic picks its own spp. `diagnostic`
  refuses `-s`, `-o` and the colour flags as unknown arguments.
- The baseline is the scene plus these overrides, so the agent can apply
  a suggestion as a flag and re-diagnose immediately.

## Risks / Trade-offs

- **[Crops are not the frame]**
  → Three crops chosen for different reasons, per-crop verdicts with
  disagreement surfaced, and full-frame projections labelled estimates.
- **[Timing noise on a busy machine]**
  → Interleaving, an ε of 5%, and all-pairs agreement for a verdict. On a
  very noisy machine everything reads `inconclusive`, which is correct and
  tells the agent to raise `--budget` or `--repeats`.
- **[A small reference blend makes MRSE noisy]**
  → The blend uses every tier-1 image (≥ 5 per crop in practice). The
  report includes the reference's own estimated MRSE, so a reader can see
  when it is too noisy to separate trials.
- **[Fixed setup costs dominate small crops]** (learned pre-pass, guiding
  training) → `setup_s` is reported separately and excluded from
  full-frame projections.
- **[The clamp counter adds a branch in the integrator]**
  → It is behind a setting checked once per camera sample, off by default.
  The callgrind pin on the zero-AOV render must stay within noise.
- **[`reconfigure` drifting from `new`]**
  → `new` is implemented via `reconfigure`, with a bitwise test per varied
  setting.
- **[An LLM over-trusting the projection]**
  → Projections live under `sample_budget` and are labelled estimates.
  Only measured ΔEff produces `suggestions`.

## Migration Plan

None for existing users. `render` gains no new behaviour; its flags move
into `SceneArgs` with identical names and defaults. CLI tests pin
`crust render --help` content except ordering.

## Open Questions

- Should the JSON go to stdout with `--json -`, for agents that prefer a
  single stream? This is easy to add later; the default stays a file.
- Should `ε` and R be scene-adaptive (raised automatically when baseline
  pairs disagree by more than ε)? Start fixed; revisit with data from real
  loops.
