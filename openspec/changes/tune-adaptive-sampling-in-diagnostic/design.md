# Design

## Context

See `proposal.md` for the motivation and `specs/` for the behaviour. This section
gives only the state of the code the approach relies on.

- **The stop rule is a pure function of per-pixel prefix sums.** A pixel of an
  adaptive pass accumulates `lum_sum`, `lum_sq` (f64) and `taken`. After each round,
  `PixelState::finish_round(spp, threshold)` derives `converged` and the f32 index
  `e = rel / threshold` from those three numbers alone. The neighbour rule
  (`held_by_neighbour`) reads a frozen image-order buffer of indices and active
  flags. The round boundaries come from `adaptive_check_points(spp, authored_min)`
  and `batch_schedule(spp, first_check)`, both pure functions.
- **Samples do not depend on the schedule.** `rendering/design.md` § Adaptive
  sampling pins "the same sample indices whatever the schedule". Sample `k` of a
  pixel is the same in a fixed render and in an adaptive one. So a pixel's first
  `n` samples in a fixed render are exactly what it would have accumulated
  adaptively by `taken = n`.
- **A diagnostic crop is a region render.** A neighbour outside the region does not
  exist for the rule, as for `--region`. A replay over the crop's own grid is
  therefore the crop's adaptive render, not an approximation of the full frame's.
- **Tier 2 today** (`diagnostic/mod.rs` ~L1250–1330): per crop, one adaptive render
  at `spp_a = clamp(4 · min_spp, 64, 1024)` with the authored threshold and minimum,
  against a fixed time *estimated* from the tier-1 renders' per-sample time
  (`time_fixed_estimate_s`). Reported as `AdaptiveCrop` rows.
- `Instruments` already offers opt-in per-pixel planes (`variance`) and per-tile
  times (`tile_times`), each costing a branch only while on.

## Goals / Non-Goals

**Goals:**

- A replay that is **bit-exact** in its sample-count decisions, so the verification
  can demand pixel-for-pixel equality rather than a tolerance.
- The sweep costs no renders beyond the one recording and one verification per crop.
- The report's adaptive answer survives the CLAUDE.md measurement traps. The ranking
  rests on the recorded render's per-tile cost, not on single sequential timings.

**Non-Goals:**

- Tuning adaptive sampling over the full frame. Crops stay the unit, as for every
  other tier.
- Changing the stop rule itself. Neither the error measure nor the batch growth
  changes.
- Tuning guiding's training passes, which are never adaptive.
- A per-pixel *time* AOV. The predicted time comes from per-tile cost.

## Decisions

### D1. Replay a recording instead of rendering each setting

Each setting of the grid is decided by replaying the stop rule over one recorded
fixed-spp render, not by rendering it.

The new instrument `Instruments::luminance_trace` makes the final pass keep, per
pixel of the region, the `f64` luminance it adds to `lum_sum` for every sample,
in sample order. That is the exact value from `self.lights.luma().of(color)` in the
accumulation. The replay walks a setting's schedule. At each round boundary it
forms `lum_sum` and `lum_sq` by adding the trace in the same order the renderer
adds them, so the f64 sums are bit-identical. It then calls the same
`finish_round` and `held_by_neighbour`.

The replay itself lives in `tracer/replay.rs`, a child of the tracer module. There it
uses the private `PixelState` directly. The round loop's decision step is factored into
`keeps_sampling`, and the sums' update into `PixelState::add_luminance`. The pass and
the replay call both, along with `finish_round`, `adaptive_check_points` and
`batch_schedule`. That keeps the pair "render ↔ replay" structural rather than a copy
kept in step by a test. `diagnostic/adaptive.rs` holds what is the diagnostic's own: the
grid, the predictions, the front and the pick.

*As built:* the pin test (`a_replay_takes_the_samples_an_adaptive_render_takes`,
`tracer/tests.rs`) holds bitwise on `cornellbox` and `veach_mis`, full frame and
region, at six settings: `sampleCount` in every pixel, the `variance` bits, and the
early-stopped and held counters.

- *Alternative: render each setting.* At around 100 settings × 3 crops that is
  hundreds of renders. It does not fit any budget the diagnostic uses.
- *Alternative: model the stop from the final variance map alone.* That assumes
  the relative error falls as 1/√n, and it cannot reproduce the neighbour rule or
  the zero-signal gate. Its predictions could not be verified exactly.

### D2. Predicted error and time

For a setting, each pixel's stop count `n_p` and prefix mean `μ_p(n_p)` come from
the trace. The pixel's predicted variance is `var_of_mean` at `n_p`.

The predicted MRSE uses the crop's existing reference (the inverse-variance blend of
its tier-1 baselines; `design.md` § The reference). It is the same relative
squared error tier 1 uses, applied to the luminance of `μ_p(n_p)`. Luminance is
used rather than RGB because the stop rule sees luminance only, and the trace
stays at 8 B per sample.

The predicted time is `Σ_tiles c_t · Σ_{p∈t} n_p`. Here `c_t` is tile `t`'s share
of the recording's wall-clock `render_s`, by its measured seconds (`tile_times`), over
its samples. A glass tile therefore costs what it costs. *As built:* the tiles' own
seconds add up the workers' time, about N × the wall on N threads. Used directly, the
first version predicted 6.8 s for a crop that rendered in 1.9 s on 4 threads. Scaled to
`render_s`, the predictions on `cornellbox` and `veach_mis` came within 6–15% of each
single verifying run. This ignores the per-round fork/join tail, which the design
record measured as a few percent at 16 rounds. The verification's single-run time
is reported beside the prediction so a gap is visible.

### D3. The grid

The grid is the product of:

- thresholds `{¼, ½, 1, 2}` × the authored threshold, plus the threshold whose
  square is the target, de-duplicated (with an authored 0: `{0.01, 0.02, 0.05,
  0.1}`);
- minimums `{authored, 8, 16, 32, 64}`, those at or below the cap;
- tolerances `{−1, 0, 1, 2}` plus the authored value;
- caps `{spp_r/4, spp_r/2, spp_r}`, where `spp_r` is the recording's spp.

That is at most 6 × 6 × 5 × 3 = 540 settings. One replay is `O(pixels × rounds)`
(rounds ≤ ~20). On a 128×128 crop the whole grid is about 1.8 × 10⁸ simple
operations, under a second on one thread; replays run in parallel with rayon over
settings.

*As built:* `spp_r` is the authored sample count within 64..=1024, so the authored
point can be replayed as authored. It is halved, down to 64, while a recording and a
verification of every crop would take more than half of what the budget has left after
the baseline. It is halved again, down to 4, while the traces would exceed the memory
bound (D4). An authored count above `spp_r` is replayed at `spp_r`, and
`authored_spp_capped` says so. The grid's caps include the authored count. When
`--target-mrse` asks for more than `spp_r` reaches, `target_reached` says so (spec).

Without a target (adaptive sampling off as authored, and no `--target-mrse`), each
crop's bar is the authored point's own MRSE (`target_from: "authored"`). A fixed count
is the cleanest the grid holds, so such a stage keeps its authored point: pass
`--target-mrse` to tune it.

`front` keeps each predicted outcome once. Many tolerances, or thresholds no pixel
passes, predict the same time and MRSE. The Markdown lists six points per crop; the
JSON has them all.

### D4. Memory bound

The trace takes `pixels × spp_r × 8 B`, up to 128 MiB for a 128×128 crop at 1024
spp. Only one crop's trace is alive at a time: record, replay, verify, drop.

*As built:* `best` is picked across all crops before any crop is verified, and
verifying a crop needs its trace again. So every crop's trace stays alive until the
verifications, under one bound of 512 MiB across the crops (`adaptive::TRACE_BYTES`).
The default three 128×128 crops at 1024 spp take 384 MiB. `spp_r` halves until the
traces fit, and the report gives `spp_recorded`. `f32` storage was rejected because
the replay must reproduce f64 sums bit for bit.

### D5. Verification and the suggestion

`best` is rendered once per crop with adaptive sampling on, the `sampleCount` and
`variance` AOVs requested through the ordinary `AovRequest`, and the clamp off.
`sample_count_matches` compares the AOV's plane with the replay's `n_p`, as
integers.

The suggestion's ΔEff comes from the **predicted** time and MRSE of `best` against
the authored setting, both from the same recording. The single-run verification
time is not used, so both sides of the ratio share one measurement and the 15%
run-to-run spread cancels. The verification establishes that the prediction is the
renderer's behaviour, which is what licenses using it.

This is why tier 2's "projections never produce suggestions" can be relaxed for
this case alone: the projection is checked against a real render before anything
is suggested.

### D5b. Suggestions as built

There is one suggestion per value `best` changes, with id
`adaptive_sampling.<setting>` (`samples_per_pixel`, `variance_threshold`,
`min_samples_per_pixel`, `adaptive_neighbour_tolerance`). Each has its flag and
attribute, and the same ΔEff and evidence (`adaptive_sampling` and every
`adaptive_verify@<crop>`). The gate (`adaptive_suggestions_for`) is a pure function,
so its refusals are unit-tested directly: a mismatch, a crop not verified, a gain under
the bar, and `best` being the authored point. A mismatching replay cannot be forced in
a real render.

### D6. Where the suggestion's values go

`--variance-threshold`, `--min-samples` and `--neighbour-tolerance` are added to
the shared scene flags (`SceneArgs`), next to the existing overrides. The `-s` cap
is a `render`-only flag already. `command()` emits all four when the adaptive
suggestion is made. `effective_settings` gains an `adaptive_neighbour_tolerance`
row, and the threshold and minimum rows gain their flags.

### D7. The maps EXR is the CLI's

`run` returns the four planes per crop in the `Report`'s side data (not
serialised), and `crust-render` writes the EXR, as it writes the JSON. The engine
still writes nothing. Layers are `authored.*` (from the replay) and `best.*` (from
the verifying render), at the scene's resolution, 0 outside the crops. Both are
needed: the authored maps show where today's setting spends its samples, and the
best maps show the change.

## Risks / Trade-offs

- **[Guided scenes]** *Found in implementation; the plan's claim was wrong.* A
  guided render decides whether its final pass is guided from a ΔEff measured in
  wall-clock time, and its image blends every pass. On `cornellbox`, whose best
  tier-1 settings are guided, the verifying render's `sampleCount` differed from the
  replay in every pixel. → When the best tier-1 settings are guided, the sweep records
  and verifies them with guiding off, and the report says so (`unguided: true`). The
  adaptive values it finds are for the unguided render; this is listed in Known
  gaps.
- **[Single-sample-ordered sums]** If the accumulation ever reorders its additions
  (SIMD over samples, say), the replay's f64 sums drift by an ulp and the
  verification starts failing. → Failing loudly is the intended behaviour. A
  bitwise test pins replay against an adaptive render on `cornellbox` and
  `veach_mis`, and at a positive tolerance, so the pair cannot drift unnoticed.
- **[Budget]** Tier 2 now costs a fixed render plus an adaptive one per crop, where
  it cost one adaptive render. That is roughly 2× on a short budget, the known gap
  made worse. → The reserve accounts for it (`schedule::plan`). A verification
  that does not fit is `not_tried`, and the sweep is still reported unverified.
- **[Grid misses the optimum]** The grid is coarse (×2 steps). → The front shows
  where it bends, and a second run with `--variance-threshold` near `best` refines
  it. The coarseness is listed as a known gap rather than adding search iterations.
- **[Report shape change]** `sample_budget.adaptive` goes from an array to an
  object while the format stays `crust-diagnostic/1`. → `--baseline` treats an
  array-shaped previous `adaptive` as absent, not as an error. The breaking change
  is listed in the user docs. Nothing in the workspace reads the field outside
  `diagnostic/` (checked: no reader in `crust-render`, its MCP server or the
  `debug-noise` skill).

## Migration Plan

None beyond the report shape (above). No stage attribute changes meaning, and the
new flags default to the stage's values.
