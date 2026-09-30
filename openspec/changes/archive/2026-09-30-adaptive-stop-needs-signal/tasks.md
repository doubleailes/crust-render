## 1. Settings

- [x] 1.1 Add `adaptive_neighbour_tolerance: f32` (default 1; negative
      turns it off) to `RenderSettings`, with a builder and an accessor.
- [x] 1.2 Read `crust:adaptiveNeighbourTolerance` in
      `scene/usd_import/settings.rs`. A non-finite value falls back to 1 with
      a `WARN` naming the authored value.
- [x] 1.3 Add it to `docs/architecture.md` wherever the render settings are
      listed.

## 2. Regression tests (written first, fail on today's code)

- [x] 2.1 No light: empty world, adaptive on. Assert `early_stopped == 0`,
      `spp_min == spp_max == spp`, and an all-black buffer.
- [x] 2.2 √spp floor: flat emissive image, 1024 spp, authored minimum 8.
      Assert `spp_min >= 32`.
- [x] 2.3 Neighbour rule, as a unit test of the decision function on a
      hand-built index buffer:
      - a cross neighbour at `e_p + t + ε` holds `p`, and one at `e_p + t`
        does not;
      - a diagonal neighbour at `+∞` does not hold `p`;
      - a stopped neighbour at `+∞` does not hold `p`;
      - a neighbour outside the image is ignored;
      - a negative `t` ignores every neighbour.
- [x] 2.4 End to end: a flat image with one noisy pixel. Its four cross
      neighbours take more samples at `t = 1` than at `t = -1`.
- [x] 2.5 Tiles ↔ scanlines are bit-identical with `t = 1`.
- [x] 2.6 Import: an unauthored tolerance reads as 1, and an authored `nan`
      reads as 1.
- [x] 2.7 Run all of them and confirm 2.1, 2.2 and 2.4 fail on today's
      code. 2.3 does not compile until the decision function exists.

## 3. Rounds

- [x] 3.1 Move per-pixel accumulation into full-frame state arrays
      (accumulators, `stopped`, `e: Vec<f32>`). Split `render_pixel` into
      "advance this pixel to `taken = k`" plus the own-pixel test.
      Non-adaptive (training) passes keep their single loop.
- [x] 3.2 First sweep to the first check point, then rounds of 4 samples:
      recompute `e` for unstopped pixels, decide stops against the frozen
      buffer with the cross rule, then trace the active pixels. Units are
      tiles or rows as today, with one `PathScratch` per rayon worker.
- [x] 3.3 Keep the own-pixel test as today's f64 comparison. The f32 index
      feeds only the comparison between neighbours.
- [x] 3.4 Gather the pass variance and the stats in scanline order, as
      today, so both strategies stay bit-identical.
- [x] 3.5 Progress: one tick per unit in the first sweep, then one per
      round, with the remaining ticks emitted on an early finish.

## 4. Stop rule

- [x] 4.1 Zero-signal gate `lum_sq > 0.0` (index `+∞`), with a comment
      explaining why an all-zero history is not convergence and why the
      gate is on `lum_sq`.
- [x] 4.2 Effective minimum `max(min_spp, ⌈√cfg.spp⌉, 2)`.
- [x] 4.3 `RayStats`: count the pixels a neighbour held at least once, and
      print that on the `--stats` adaptive line.

## 5. Verify

- [x] 5.1 `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`.
- [x] 5.2 Off is scheduling only: cornellbox at 64 spp with minimum 32 and
      `t = -1` (so neither gate fires) must match the pre-change binary
      exactly under `exr_diff`.
- [x] 5.3 `scripts/check_images.sh check` shows no change (16 spp never
      early-stops).
- [x] 5.4 `bench_ab.sh`, base binary against `t = -1`, on cornellbox and
      ALab: the cost of the rounds. Report min and mean.
- [x] 5.5 ALab probe (1024 spp, minimum 8, frame 1004) at `t = -1` and at
      `t = 1`: exact-zero pixel count, mean spp, held-pixel count, render
      time, and a look at the glass.

## 6. Docs

- [x] 6.1 `openspec/specs/rendering/design.md`, under adaptive sampling:
      the all-zero trap, the √spp floor and why it is a floor, the
      convergence index and the cross-neighbour tolerance, the round
      structure, the ALab numbers and the costs.

## 7. Growing batches (D8)

- [x] 7.1 Batch schedule: after the first sweep, round `r` traces
      `max(4, taken / 4)` samples per active pixel, capped at the budget.
      Compute the schedule once from `(spp, first_check)`; its length is
      the round count and the progress total.
- [x] 7.2 Test the schedule: deterministic, every batch ≥ 4, each ≤ 25% of
      the samples taken before it, the last ends exactly at `spp`; and
      `progress_callback_reaches_the_total` still holds with it.
- [x] 7.3 Re-measure: `bench_ab.sh`, base binary against `t = -1`, on
      cornellbox and ALab at 64 spp (min and mean), and the ALab probe
      (1024 spp, minimum 8, frame 1004) at `t = 1`: mean spp, held count,
      exact-zero count, render time — against the batch-4 numbers.
- [x] 7.4 `openspec/specs/rendering/design.md` § Adaptive sampling: the
      growth rule, the overshoot bound, the retired `t < 0` bit-identity,
      and the Costs table with the new column.
