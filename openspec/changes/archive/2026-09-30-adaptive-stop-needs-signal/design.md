## Context

`render_pixel` (`crates/crust-core/src/tracer/mod.rs`) runs a pixel's whole
sample loop inside its work unit (a 16×16 tile or a row). It accumulates
`sum`, `weight_sum`, `lum_sum` and `lum_sq`, and every 4th sample past
`min_spp` it stops when `sqrt(var_of_mean) / max(mean, 1e-4) < threshold`.
Three properties constrain the change:

- Each pixel decides alone, so nothing can see a neighbour's state:
  a neighbour comparison is impossible in the current structure.
- A pixel's samples depend only on `(x, y, frame, sample index)`, never on
  the schedule, which is why tiles and scanlines are bit-identical.
- When every sample is 0, the variance is 0, the ratio is 0, and the pixel
  stops.

See proposal.md for the ALab measurements, the literature, and the
Guerilla-style convergence index this change adopts.

## Goals / Non-Goals

**Goals:**

- Remove the all-zero stop, floor the minimum at √spp, and compare each
  pixel's convergence index with its cross neighbours within a tolerance.
- With a negative tolerance, every pixel takes exactly today's samples (up
  to the two new gates), so the rounds can be A/B'd as scheduling only.
- Tiles and scanlines stay bit-identical with the comparison on.

**Non-Goals:**

- An unbiased stop (Kirk & Arvo's discarded pilot pass). It costs samples,
  and production renderers do not pay it.
- A different error estimator (Cycles' half-buffer error, a variance prior,
  a perceptual metric). The index is today's relative error, rescaled.
- Writing the index buffer out as an EXR layer. It would be a useful
  diagnostic and could follow, but it needs an output-side decision.
- A CLI flag for the tolerance (see proposal.md, Impact).
- Changing the `1e-4` mean floor or the unauthored default minimum (32).

## Decisions

**D1: gate on "any non-zero sample", not a larger minimum.** Only the
all-zero case is degenerate. With `k` equal non-zero hits among `n` samples,
the relative standard error is about `sqrt((1 - k/n) / k)`. A single hit
reads as about 1, so the stop needs roughly `1/threshold²` hits before it
fires. The √spp floor (D5) and the neighbour comparison (D4) address the
sparse-but-non-zero case. The gate is the only one of the three that
removes the black pixels outright. In index terms: an all-zero pixel has
`e = +∞`.

*Alternative, rule of three:* let an all-zero pixel stop after `3/p` samples.
Rejected: it adds a tuning constant unrelated to `varianceThreshold`, and it
still writes black for signals rarer than `p`.

**D2: test `lum_sq > 0.0`, not `lum_sum > 0.0`.** Filter weights `wx * wy`
can be negative (Mitchell lobes), so a pixel that has seen light can still
have `lum_sum <= 0`. `lum_sq` is a sum of squares, so it is zero exactly when
every sample's luminance is zero, and it is already accumulated. Luminance
uses positive Rec.709 weights, so a non-negative colour with any non-zero
channel has non-zero luminance.

**D3: the adaptive pass runs in global rounds over an index buffer.**
Per-pixel state moves out of `render_pixel` into full-frame arrays:

- the accumulators (`sum`, `weight_sum`, `lum_sum`, `lum_sq`, `taken`);
- a `stopped` flag;
- the convergence index `e: Vec<f32>`.

The pass then runs in three steps:

1. A first sweep brings every pixel to the first check point: the smallest
   multiple of 4 that is at least the effective minimum, capped at the
   budget.
2. Each round then does the following, and repeats until no pixel is
   active or the budget is spent:
   - recompute `e` for every unstopped pixel;
   - decide, against the frozen buffer, which pixels stop (D4);
   - trace 4 more samples for every pixel still active.
3. Work units (tiles or rows) are the parallel grain within a round, exactly
   as today.

The index is computed in f64 from the accumulators and stored as f32. The
stop test reads the same f32, so every decision is a function of one value
per pixel. The decision step reads a buffer that no one writes during it,
so no pixel's decision depends on schedule order, and tiles and scanlines
stay bit-identical. Each pixel draws the same sample indices and checks at
the same `taken` values as the per-pixel loop.

*Keeping a negative tolerance bit-identical to today's code:* the own-pixel
test must stay exactly today's f64 comparison
`sqrt(var_of_mean) / mean < threshold`. It must not become `e < 1` on the
rounded f32, which could flip a pixel sitting at the boundary. The f32 index
is used only for the comparison between neighbours.

*Alternative, per-tile comparison with a halo:* compare within a tile using
the state of the pixels around it. Rejected: the halo belongs to other work
units, so either tiles would wait on each other or the result would depend
on schedule order, which breaks the tiles ↔ scanlines bit-identity pair.

*Batch size:* the first implementation used batches of 4 throughout, so
that the check points stayed where the per-pixel loop had them and a
negative tolerance could be proved bit-identical to the pre-change binary
(it was: cornellbox 64 spp, `exr_diff` all zeros). Measured, the rounds then
cost **+8.1% min / +8.9% mean** wall-clock on that same bit-identical render
(`bench_ab.sh`), with callgrind counting 0.5% *fewer* instructions — the
cost is the fork/join tail of `(spp − first_check) / 4` rounds and colder
caches, not work. D8 replaces the fixed batch.

**D8: batches grow 25% a round.** After the first sweep, round `r` traces
`batch = max(4, taken / 4)` samples per active pixel, capped at the budget,
where `taken` is the count every active pixel shares at the start of the
round. From 32 at 1024 spp that is 16 rounds instead of 248. The cost is
overshoot: a pixel that would have stopped at the next multiple of 4 now
stops at the end of its batch, at most 25% past what it had taken (Cycles'
doubling schedule overshoots by up to 100%; 25% keeps the samples spent on
already-converged pixels small next to the barrier savings). The schedule
is a pure function of `(spp, first_check)`, computed once up front, so the
number of rounds — and the progress total (D6) — is known before the pass
starts. What stays pinned: every pixel draws the same sample indices
whatever the schedule; a negative tolerance still means "exactly the
samples the pixel would take alone", since alone it would follow the same
schedule; tiles and scanlines stay bit-identical. What is retired: the
bit-identity of `t < 0` with the pre-change per-pixel loop above the first
check point. It held at batch 4, which was its purpose (task 5.2); it is not
a standing invariant.

*Alternative, finer work units in late rounds:* shortens each barrier's
tail without moving the check points. Set aside: it keeps the same number
of barriers, and the tail is per barrier.

**D4: the cross-neighbour rule.** Pixel `p` stops in a round when all of the
following hold:

- it has passed the effective minimum;
- it passes its own test (D3), which with D1 implies `e_p < 1`;
- for each of its up, down, left and right neighbours `q` that is inside
  the image and not stopped, `e_q − e_p ≤ t`.

A few details:

- **One-sided.** A neighbour that is *more* converged never holds `p`. The
  comparison is there to stop `p` from quitting while the pixels beside it
  are still noisy, not to keep the frame evenly sampled.
- **Absolute, in index units.** Both indices are already normalised by the
  threshold, so `t = 1` means "no neighbour may be more than one threshold
  of relative error behind me". A relative tolerance was considered and set
  aside, because the index is already relative to the pixel's own mean.
- **Stopped neighbours never hold.** A neighbour that stopped is either
  converged or out of budget, and holding `p` for it spends samples with no
  prospect of changing the outcome. This bounds the cost at the edge of a
  truly black region: its `+∞` pixels hold their lit border only while they
  are still sampling.
- **No chaining.** A held pixel keeps its own `e`, so holding never
  propagates. `p`'s neighbours compare themselves with `e_p`, not with the
  neighbour that holds `p`. No neighbour-of-neighbour pass is needed.
- **`+∞` arithmetic.** `+∞ − finite = +∞ > t` holds `p`. Two all-zero
  pixels never reach the neighbour test, because each fails its own.
- **Default `t = 1`. Off is `t < 0`.** A negative tolerance skips the
  comparison entirely (not `e_q − e_p ≤ −1`), so off is the per-pixel stop
  with no dependence on neighbour values.
- **Cross, not square.** Up, down, left and right only, following Guerilla.
  Diagonal noise reaches `p` through the shared cross neighbours in a later
  round, if at all. There is no radius parameter.

*Alternative, binary square-window dilation that scales the threshold (the
previous revision of this change):* replaced. It could not tell a neighbour
at `e = 1.01` from one at `e = 50`, and it needed two parameters where the
index needs one.

**D5: √spp is a floor on the authored minimum, computed per pass.** The
effective minimum is `max(min_samples_per_pixel, ⌈√N⌉, 2)`, where `N` is the
pass's budget (`cfg.spp`, which after the `-s` override is the final pass's
budget). The unauthored default stays 32. Cycles instead uses √spp only when
the minimum is unauthored, and trusts an authored value. Rejected here for
two reasons: ALab's authored 8 would stay 8, and a √spp default would drop
the minimum to 4 at 16 spp, making the goldens early-stop and cascade (the
`check_images.sh` / CLAUDE.md rule).

**D6: progress.** The first sweep reports once per work unit, as today.
Each later round reports one tick, and the total is units plus the length
of the batch schedule (D8). If the pass ends early, the remaining ticks are
emitted so that `completed` reaches `total`, still increasing by one per
report as the rendering spec requires. The counter advances whether or not
a callback is attached — the walk-to-total loop otherwise never ends on a
render without one (a trap already fallen into).

**D7: tests pin the rules, not the ALab image.** Each test:

- A scene with no light, adaptive on: `early_stopped == 0`, and
  `spp_min == spp_max == spp`.
- 1024 spp with an authored minimum of 8, on a flat emissive image that
  would otherwise stop at 8: `spp_min >= 32`.
- The neighbour rule, tested on the decision function over a hand-built
  index buffer rather than through a render, so the numbers are exact:
  - a cross neighbour at `e_p + t + ε` holds `p`, and one at `e_p + t` does
    not;
  - a diagonal neighbour at `+∞` does not hold `p`;
  - a stopped neighbour at `+∞` does not hold `p`;
  - a negative `t` ignores every neighbour.
- End to end: a flat image with one noisy pixel, where the noisy pixel's
  four cross neighbours take more samples at `t = 1` than at `t < 0`.
- A tiled render equals a scanline render, bit for bit, with `t = 1`.
- The batch schedule (D8): deterministic, every batch at least 4, each at
  most 25% of the samples taken before it, the last one ending exactly at
  the budget; and the round count it implies is the progress total.
- That a negative tolerance reproduced the pre-change code was proved once,
  outside the test suite, by `exr_diff` against the pre-change binary at
  batch 4 (task 5.2). With growing batches that comparison no longer holds
  and is not repeated.

## Risks / Trade-offs

- [Round overhead: measured at batch 4 as +8.1% min / +8.9% mean on
  cornellbox 64 spp and +6.6% / +13.6% on ALab 64 spp (`bench_ab.sh`, base
  against `t = −1`), a barrier-tail cost rather than instructions] → D8's
  growing batches cut the round count by an order of magnitude; re-measure
  the same A/B after it (tasks, section 7).
- [Overshoot from growing batches: a converged pixel takes up to 25% more
  samples than it needs] → Bounded by the growth rate; the ALab probe's mean
  spp at `t = 1` before (992.1) and after is the measurement. If it grows
  more than the barrier savings are worth, lower the rate.
- [Per-pixel state is now full-frame: about 48 B of accumulators plus 4 B of
  index per pixel, roughly 110 MB at 1920×1080] → Acceptable next to the
  scene; keep the fields that are already f64 as f64 so the off side stays
  bit-identical.
- [The comparison spends samples on converged pixels beside noise] → That is
  its purpose. The tolerance is the knob, and the `--stats` count of held
  pixels makes the cost visible.
- [Fully black regions run the full budget, and hold their lit cross
  neighbours while they do] → On ALab, zero pixels were 0.6% of the frame,
  and the 256 spp A/B of the gate alone took 117 s against 120 s. The border
  cost is part of task 5.5's measurement.
- [A PathScratch per work unit per round becomes an allocation per round]
  → Hold one per rayon worker (`map_init`) rather than one per unit.
