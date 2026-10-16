# Tasks

## 1. Adaptive sampling flags (cli)

- [x] 1.1 Add `--variance-threshold` (≥ 0), `--min-samples` (≥ 1) and `--neighbour-tolerance` to `SceneArgs` in `crust-render/src/main.rs`, applied as overrides of `crust:varianceThreshold` / `crust:minSamplesPerPixel` / `crust:adaptiveNeighbourTolerance`. Verify with parse tests: shared by `render`, `diagnostic`, `check` and `light-paths`; `--variance-threshold -1` is a usage error.
- [x] 1.2 Add the `adaptive_neighbour_tolerance` row to the diagnostic's and `check`'s effective settings, and give the threshold and minimum rows their new flags. Verify with the cli spec's scenario "The adaptive flags are shared" as a test (`crust diagnostic --variance-threshold 0.02 --min-samples 16` → effective settings).
- [x] 1.3 Document the three flags in `site/content/docs/reference/command-line.md` § Sampling and time, linking to their `render-settings.md` attributes, and mention them in each attribute's section. Verify that `zola build` in `site/` passes its link and anchor check.

## 2. Luminance trace and the shared decision step (rendering)

- [x] 2.1 Make `adaptive_check_points`, `batch_schedule`, `held_by_neighbour` and `PixelState::finish_round` reachable from `diagnostic/`. Factor the round loop's "freeze indices → decide stops" step into one `pub(crate)` function that the pass calls. Verify that `cargo test -p crust-core` passes unchanged and `scripts/check_images.sh check` reports no image change.
- [x] 2.2 Add `Instruments::luminance_trace`: the final pass keeps, per region pixel, each sample's f64 luminance in sample order, returned on `Measured`. Verify with a test that the trace's in-order f64 sums equal the pass's `lum_sum` / `lum_sq` bitwise, and that a render without the instrument is bit-identical to before (callgrind instruction count on cornellbox at `-s 2` within noise, as CLAUDE.md describes).

## 3. Replay and sweep (diagnostics)

- [x] 3.1 Create the replay, `replay(trace, setting) -> (sample_count, var_of_mean)`, built on the step from 2.1 (as built: in `tracer/replay.rs`, beside the private `PixelState`; `diagnostic/adaptive.rs` holds the grid and predictions). Verify with the bitwise pin from design D1: on `cornellbox` and `veach_mis` crops, at tolerances −1 and 1 and two thresholds, the replay's counts equal a real adaptive region render's `sampleCount` AOV in every pixel.
- [x] 3.2 Build the grid (design D3) including the authored setting and the target-derived threshold, and predict each setting's MRSE against the crop reference and its time from the recording's tile costs (design D2). Run the replays in parallel over settings. Verify with unit tests: the authored setting is always on the grid; with an authored threshold of 0 the grid uses the fixed thresholds; the predicted time of a fixed setting equals the recording's summed tile time.
- [x] 3.3 Compute the per-crop Pareto front and `best` (least summed predicted time with MRSE ≤ target on every crop, else the lowest MRSE with `target_reached = false`). Verify with unit tests on synthetic predictions covering the spec scenarios "A lower threshold with a higher cap" and "Nothing reaches the target".
- [x] 3.4 Apply the memory bound (design D4): one crop's trace alive at a time, and `spp_r` halves until the trace fits 256 MiB, reported. Verify with a test on a forced-small bound that `spp_r` halves and the report states it.

## 4. Tier 2 wiring, verification and suggestion (diagnostics)

- [x] 4.1 Replace tier 2's per-crop adaptive render with record → sweep → verify in `diagnostic/mod.rs`. The verifying render uses `best` with the `sampleCount` and `variance` AOVs; the result records `sample_count_matches`, `mismatched_pixels`, and measured beside predicted time. Update `schedule::plan`'s tier-2 reserve and admissions to cost both renders; one that does not fit is `not_tried` (`budget`). Update the existing tests for these schedules. Verify with the spec scenarios "The prediction reproduces" and "The budget cannot verify" as tests.
- [x] 4.2 Replace `AdaptiveCrop` in `report.rs` with the `sample_budget.adaptive` object: `grid`, `authored`, `front`, `best`, `target_reached`, `verification`, `spp_recorded`. Make `--baseline` treat an array-shaped previous `adaptive` as absent. Render a short adaptive section in `markdown.rs` from the same data. Verify with the structure test ("Stable structure": same keys in the same order on two runs) and a test that a report with the old shape loads as a `--baseline`.
- [x] 4.3 Turn a verified `best` into the `adaptive_sampling` suggestion when its ΔEff at the target is ≥ 1.10 (design D5). It names the attributes and flags of the changed values, `-s` included, and cites the sweep and the verifications. Emit them in `suggested_command` (design D6). Verify with the spec scenarios "A verified gain", "Within the bar" and "The prediction does not reproduce" as tests, and that `converged` stays false while such a suggestion exists.
- [ ] 4.4 Update the `diagnostics` docs: `site/content/docs/help/diagnosing-a-render.md` (the adaptive section, how to read the front, the shape change of `sample_budget.adaptive`); `openspec/specs/diagnostics/design.md` (replace the tier-2 adaptive paragraph with the replay, state the measured replay cost and memory, and update the Known gaps: tier 2's budget, grid coarseness, guided MRSE is final-pass only). Verify that `zola build` passes.

## 5. Adaptive maps (cli + diagnostics)

- [x] 5.1 Return the four planes per crop on the `Report`'s unserialised side data. Write them in `crust-render` with `--adaptive-maps PATH` as one EXR at the scene's resolution with layers `authored.sampleCount`, `authored.variance`, `best.sampleCount`, `best.variance` (0 outside the crops). Name the file and its layers in the JSON. Verify with a CLI test: with the flag the EXR has those layers and non-zero pixels only inside the crops; without it no `.exr` / `.png` is written ("Outputs of a run"); with tier 2 not run no file is written and the report says why.
- [x] 5.2 Document `--adaptive-maps` under `## diagnostic` in `command-line.md`, and add a step to `.claude/skills/debug-noise/SKILL.md` that reads the adaptive front and the maps when adaptive sampling is on. Verify that `zola build` passes.

## 6. Integration

- [ ] 6.1 Run CI locally: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`. Verify all are clean.
- [ ] 6.2 Run `crust diagnostic --budget 2m --adaptive-maps maps.exr` on `samples/cornellbox.usda` and `veach_mis`. Record in `diagnostics/design.md`: each crop's verification matched; the replay's wall time; tier 2's share of the budget against before; `best` against the authored point. Then render the full frame with `suggested_command` and with the authored settings at `--indirect-clamp 0`, compare against an adaptive-off reference with `crust diff`, and confirm that the suggested setting's relMSE is at or below the target while its time is lower (`bench_ab.sh`-style interleaved timing).

## Workflow follow-up

- Archive the change with `/opsx:archive` once reviewed, which syncs the `diagnostics` and `cli` deltas into the main specs.
