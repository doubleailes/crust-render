# Proposal

## Why

The MaterialX subsurface random walk (`subsurface.rs`, Typhoon's and Cycles' walk)
is half of what `samples/materialx_subsurface.usda` costs — 50% of its instructions
and 58% of its ray queries — and none of that showed in `--profile`, which folded the
walk into `MainLoop`'s local time. Measuring it (`docs/subsurface_walk.md`) found two
free reductions: walks keep stepping after every channel has gone dim, and each step
evaluates nine exponentials where six determine the other three.

## What Changes

- **In-walk Russian roulette.** A scatter whose throughput has fallen under 0.05 in
  every channel survives with probability `peak / 0.05` (never under 0.05) and is
  reweighted on survival. Unbiased; 10–15% fewer steps for a chromatic medium such
  as skin, none for a grey one, no measurable variance.
- **One exponential fewer per channel per step.** The backward-stretched
  transmittance of the Dwivedi mixture pdf is `exp(−σt)² / exp(−σ(1 − c/ν)t)`, a
  multiply and a divide, with the exponential kept for flights long enough for the
  quotient to underflow. Images move at the last ulp only.
- **A `Subsurface` profile section** around the walk, opened only when a walk runs,
  so `--profile` reports the walk (34% of the fixture's thread time) instead of
  hiding it.
- **`docs/subsurface_walk.md`**: the cost breakdown, the walk-level experiments
  (albedo mapping × entry, the 256-step cap's energy loss on thick objects, the
  variants that did not pay) and the literature roadmap; the materials, rendering
  and CLI design records point to it.

Nothing on the per-vertex path changes: cornellbox, which never walks, executes
+0.0002% instructions.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `materials`: adds a requirement on how a subsurface walk ends — exit, absorption,
  roulette, cap — and that the roulette leaves the expected radiance unchanged.
  (The walk itself is specified by the unarchived change
  `add-mtlx-random-walk-subsurface`; this delta stands beside it.)
- `cli`: adds a requirement that `--profile` reports subsurface walks as their own
  `Subsurface` section.

## Impact

- `crates/crust-core/src/subsurface.rs`: the roulette and the transmittance identity
  inside `random_walk`; two new constants.
- `crates/crust-core/src/profile.rs`, `crates/crust-core/src/tracer/path.rs`:
  `Section::Subsurface` and its scope around `walk_subsurface`.
- Renders with a live `subsurface_bsdf` change at the noise level (unbiased both
  before and after); every other scene is bit-identical.
- `docs/subsurface_walk.md`, `docs/architecture.md`, the `materials`, `rendering` and
  `cli` design records.
- Implemented on branch `claude/subsurface-walk-roulette` (commit `6860b07`).
