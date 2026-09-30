## Context

`render_pixel` (`crates/crust-core/src/tracer/mod.rs`) accumulates `lum_sum`
and `lum_sq` for each filter-weighted sample. Every 4th sample past `min_spp`,
it stops when `sqrt(var_of_mean) / max(mean, 1e-4) < threshold`. The `1e-4`
floor lets dark pixels stop on an absolute error, but when every sample is 0
the variance is 0 too, the ratio is 0, and the pixel stops. See proposal.md
for the ALab measurements.

## Goals / Non-Goals

**Goals:**
- Remove the degenerate stop on an all-zero history, and nothing else. Every
  pixel with a non-zero sample follows exactly the path it follows today.

**Non-Goals:**
- A better adaptive estimator (a variance prior, confidence bounds, per-tile
  error). Cases with sparse but non-zero signal are already safe; see D1.
- Changing the `1e-4` mean floor or the default `min_spp`.

## Decisions

**D1: gate on "any non-zero sample", not a larger minimum.** Only the
all-zero case is degenerate. With `k` equal non-zero hits among `n` samples,
the relative standard error is about `sqrt((1 - k/n) / k)`. A single hit
reads as about 1, so the stop needs roughly `1/threshold²` hits before it
fires, which is the intended behaviour. Raising `min_spp` would only shrink
the window in which the all-zero case happens, and would charge every pixel
for it.

*Alternative, rule of three:* let an all-zero pixel stop after `3/p` samples
for some acceptable miss probability `p`. Rejected: it adds a second tuning
constant with no connection to `varianceThreshold`, and it still writes black
for signals rarer than `p`.

**D2: test `lum_sq > 0.0`, not `lum_sum > 0.0`.** Filter weights `wx * wy`
can be negative (Mitchell lobes), so a pixel that has seen light can still
have `lum_sum <= 0`. `lum_sq` is a sum of squares, so it is zero exactly when
every sample's luminance is zero. It is already accumulated, so the check
needs no new state. Luminance uses positive Rec.709 weights, so a
non-negative colour with any non-zero channel has non-zero luminance.

**D3: the regression test pins the rule, not the ALab image.** Render a
scene that returns no light at all, with adaptive sampling on and a
threshold above zero, and assert that no pixel stops early and every pixel
takes the full budget (`early_stopped == 0`, `spp_min == spp_max == spp`).
The test fails on today's code (every pixel stops at `min_spp`) and does not
depend on how QMC samples happen to land. The existing
`adaptive_sampling_takes_fewer_camera_rays_on_a_flat_image` keeps covering
the other side of the rule: a pixel with signal still stops.

## Risks / Trade-offs

- [Fully black regions now run the full budget] → On ALab, zero pixels were
  0.6% of the frame and the 256 spp A/B took 117 s against 120 s. A scene
  that is mostly void and renders with a large budget will slow down. That
  is the cost of an unbiased stop, and the design record will state it.
- [A pixel whose one non-zero sample is tiny can still stop almost at once] →
  Not new, and not black: the pixel keeps the signal it found. It is covered
  by the existing threshold semantics.
