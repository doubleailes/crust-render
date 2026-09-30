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

## What Changes

- A pixel that has not yet recorded a single non-zero sample SHALL NOT stop
  early: an all-zero history carries no estimate of the error, so it is not
  evidence of convergence. Such a pixel keeps sampling until it sees light or
  exhausts `samplesPerPixel`.
- Pixels that have seen any signal stop exactly as before, including a
  constant non-zero image, which still stops at the first check past the
  minimum.
- A regression test renders a scene that returns no light and asserts that
  no pixel stops early.
- Truly black pixels (a void, an unlit interior) now take the full budget.
  This is the price of the fix; on ALab it is under 1% of pixels, and the
  256 spp A/B rendered in the same time (117 s against 120 s).

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `rendering`: the "Adaptive sampling stops pixels early" requirement gains
  the condition that the pixel has recorded some signal, plus a scenario for
  an all-zero pixel.

## Impact

- `crates/crust-core/src/tracer/mod.rs`: the early-stop condition in
  `render_pixel`.
- `crates/crust-core/tests/render_smoke.rs`: new regression test.
- `openspec/specs/rendering/design.md`: records the trap under adaptive
  sampling.
- Output: renders with adaptive sampling on change wherever a pixel used to
  stop at black. Image goldens (`check_images.sh`, `-s 16` with the default
  minimum of 32) never early-stop, so they are unchanged.
