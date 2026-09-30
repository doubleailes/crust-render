## Why

Adaptive sampling stops a pixel once the relative standard error of its mean
falls below `crust:varianceThreshold`. A pixel whose first
`crust:minSamplesPerPixel` samples are all exactly zero has a measured
variance of zero, so it passes that test at once and is written as black,
whatever the requested budget. On ALab frame 1004 (1024 spp, minimum 8) this
left 3,124 pixels at exactly 0.0, speckled over the glassware, where most
paths legitimately carry nothing (shadow rays cannot see lights through a
dielectric). Turning adaptive sampling off makes the speckles fall away as
1/spp (803 zero pixels at 32 spp, 136 at 128). They are a bias of the stop
rule, not noise of the integrator.

The all-zero pixel is the extreme case of a known bias: when the samples
that decide whether to stop are also averaged into the image, early samples
that happen to agree stop the pixel too soon (Kirk & Arvo, SIGGRAPH '91;
Tamstorf & Jensen, EGWR '97, who find it worst under indirect light).
Production renderers bound it with two guards that crust lacks: a minimum
that grows with the budget (Cycles uses √spp), and a comparison with
neighbouring pixels, so that an isolated pixel cannot declare itself done
while the pixels around it are still noisy. Guerilla Render keeps an f32
convergence index per pixel and compares it with the pixel's cross
neighbours (up, down, left, right) within a tolerance. This change follows
that model.

## What Changes

- **Zero-signal gate.** A pixel that has not yet recorded a single non-zero
  sample SHALL NOT stop early. It keeps sampling until it sees light or
  exhausts its budget.
- **√spp floor.** The effective minimum becomes
  `max(crust:minSamplesPerPixel, ⌈√spp⌉)`, so a scene-authored minimum of 8
  at 1024 spp takes at least 32. The unauthored default stays 32, so renders
  at 16 spp (the goldens) still never stop early.
- **Convergence index and cross-neighbour tolerance.** Each pixel of the
  adaptive pass carries an f32 convergence index, `e = relative error /
  threshold`, which is below 1 when the pixel passes its own test and +∞
  before it has seen light. A pixel stops only if, in addition, none of its
  four cross neighbours that is still sampling has an index more than the
  tolerance above its own. One new render setting,
  `crust:adaptiveNeighbourTolerance` (index units, default 1), controls
  this. A negative tolerance is the per-pixel stop, which is the A/B side.
- The final pass samples in rounds, with the full-frame index buffer updated
  between rounds, instead of each pixel running its whole loop alone.
  Scheduling only: with a negative tolerance, every pixel takes exactly the
  samples it takes today.
- Regression tests: a scene with no light never stops early; the √spp floor;
  a less converged cross neighbour holds a pixel and a diagonal one does not;
  a negative tolerance matches the per-pixel stop; tiles and scanlines stay
  bit-identical with the comparison on.
- Truly black pixels, and pixels next to noisy ones, now take more samples.
  That is the cost of the fix. On ALab the zero gate alone rendered the
  256 spp A/B in the same time (117 s against 120 s); the rounds and the
  neighbour comparison need their own measurement (tasks).

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `rendering`: "Adaptive sampling stops pixels early" gains the zero-signal
  gate, the √spp floor and the cross-neighbour comparison. A new requirement
  covers the tolerance setting.

## Impact

- `crates/crust-core/src/tracer/mod.rs`: `render_pass` / `render_pixel`
  restructured so the adaptive pass runs in rounds, plus the stop test.
- `crates/crust-core/src/tracer/settings.rs`: a new `RenderSettings`
  field and builder.
- `crates/crust-core/src/scene/usd_import/settings.rs`: reads the
  tolerance.
- `crates/crust-core/src/stats.rs` and `--stats`: counts the pixels a
  neighbour held back.
- `crates/crust-core/tests/render_smoke.rs`: new regression tests.
- `openspec/specs/rendering/design.md`, `docs/architecture.md`: the trap and
  the new setting.
- Output: adaptive renders change wherever a pixel used to stop at black,
  stop under the √spp floor, or sit next to a less converged pixel. Image
  goldens (`-s 16`, default minimum 32) never early-stop, so they are
  unchanged.
- Not in scope: a CLI flag for the tolerance. It would need a `cli`
  spec delta; for now, A/B through a shot-layer override.
