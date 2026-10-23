# Proposal

## Why

On a production frame most of the render time goes where adaptive sampling decides
it goes, and today `crust diagnostic` can only say how the *authored* adaptive
settings behave: tier 2 renders each crop once at `crust:varianceThreshold`, the
authored minimum and the default neighbour tolerance, and reports the share of
pixels stopped early, the mean spp and the time saved. It never asks whether another
threshold, minimum, tolerance or sample cap would reach the same error sooner. Its
evidence is three numbers per crop, not the per-pixel maps (`sampleCount`,
`variance`) that show where the samples went and why.

Tuning those four settings by hand means a grid of full renders, each of which costs
what the frame costs. That is too slow to do, and with sequential timings too noisy to
trust.

## What Changes

- **Tier 2 tunes adaptive sampling.** It no longer runs one adaptive render per crop.
  Instead it records one fixed-spp render per crop with each pixel's luminance
  sample by sample. Then it **replays** the adaptive stop rule offline over a grid
  of settings: variance threshold, effective minimum, neighbour tolerance and spp
  cap. Each replay predicts every pixel's `sampleCount` and the variance of its
  mean. Combined with the crop's measured per-tile cost, that gives each setting's
  predicted error and time. The renderer already uses the same sample indices
  whatever the stop schedule, so a replay takes exactly the decisions a real
  adaptive render would. A replay costs microseconds per pixel, not a render.
- **The best setting is verified, not trusted.** The setting that reaches the target
  MRSE in the least predicted time is rendered once for real, per crop, with the
  `sampleCount` and `variance` AOVs on. The report states whether the real
  `sampleCount` map matches the replay pixel for pixel. If it does not, the
  prediction is reported as not reproduced and nothing is suggested from it.
- **Report.** `sample_budget.adaptive` changes from a per-crop list into an object.
  It holds the settings tried, the Pareto front of predicted time against predicted
  MRSE, the authored setting's predicted point, the best setting at the target and
  its verification. The Markdown report gains a short adaptive section. The report
  stays `crust-diagnostic/1`, and the change to the `adaptive` key is documented.
  **BREAKING** for readers of `sample_budget.adaptive[]`.
- **A suggestion when verified.** A verified best setting that beats the authored
  one at the target by the suggestion bar becomes a suggestion. It names its
  `crust:*` attributes and its flags, and its ΔEff at the target. This relaxes
  "projections SHALL NOT produce suggestions" for this one verified case only.
- **`--adaptive-maps PATH`** (opt-in) writes one EXR with the authored and the
  best setting's `sampleCount` and `variance` planes for every crop. These are the
  two AOVs as images, to see where each setting spends its samples. Without the flag
  the diagnostic still writes no image.
- **Three adaptive flags** so a suggestion can be applied from the command line:
  `--variance-threshold`, `--min-samples` and `--neighbour-tolerance`. They override
  `crust:varianceThreshold`, `crust:minSamplesPerPixel` and
  `crust:adaptiveNeighbourTolerance`, are accepted by `render` and shared with
  `diagnostic`, `check` and `light-paths`, and are emitted in `suggested_command`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `diagnostics`: tier 2's adaptive measurement becomes a replayed sweep with one
  verifying render per crop. The `sample_budget.adaptive` report shape changes.
  Suggestions may now come from a verified adaptive setting. `--adaptive-maps` is
  the one opt-in exception to "writes no image". The probe-conditions exception
  names the recording and the verifying renders.
- `cli`: the three adaptive flags join `render`'s flags and the set shared with
  `diagnostic`, `check` and `light-paths`.

## Impact

- `crust-core/src/tracer/mod.rs`: a new opt-in `Instruments` recording, the
  per-sample luminance of the final pass in pixel order. Off by default, and the
  render without it is unchanged; its cost is a branch per work unit. The stop
  rule's pure parts (`PixelState::finish_round`, `held_by_neighbour`,
  `batch_schedule`, `adaptive_check_points`) are reused by the replay, not
  copied.
- `crust-core/src/diagnostic/`: a new `adaptive.rs` (grid, replay, Pareto front,
  pick); `mod.rs` tier 2; `schedule.rs` reserve; `report.rs` and `markdown.rs`;
  `tests.rs`.
- `crust-render/src/main.rs`: the three flags, `--adaptive-maps` and its EXR writer
  (the engine still writes nothing).
- Memory: the recording keeps 8 bytes per sample per crop pixel. At the default
  three crops of up to 128×128 at 1024 spp that is up to 128 MiB per crop, freed
  after its replay. The sweep is bounded (design D4).
- Budget: tier 2 costs one fixed render plus one adaptive render per crop, where it
  cost one adaptive render. The reserve grows to match. This is the known gap
  "Tier 2's adaptive test can take much of a short budget", made somewhat worse in
  exchange for an answer instead of a measurement.
- Docs: `site/content/docs/help/diagnosing-a-render.md`,
  `site/content/docs/reference/command-line.md` (three flags, `--adaptive-maps`),
  `openspec/specs/diagnostics/design.md`, `docs/architecture.md` if the instrument
  list is there.
