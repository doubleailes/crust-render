# Design

## Context

`Renderer::render_pass` (`crust-core/src/tracer/mod.rs`) already runs a final pass in
two phases. First, a **first sweep** takes every pixel to `first_check` (the smallest
multiple of 4 ≥ `min_spp`, 32 by default). Then come **adaptive rounds**: freeze each
pixel's convergence index and active flag, decide stops against that frozen
buffer, and trace the next batch (`batch_schedule`, +25% a round). Per-pixel state
lives in `Unit`s (16×16 tiles or rows) that rayon mutates through `par_iter_mut`.
The image is assembled only in the gather at the end. Two properties make this
change cheap:

- `advance_pixel` is documented to be step-invariant. A sample depends only on
  `(i, j, seed, sample index)`, so advancing a pixel to `t₁` and then to `t₂` gives
  the same result as advancing it straight to `t₂`.
- Decisions read frozen buffers, never live ones, so unit order never matters.
  That is the same argument that makes tiles and scanlines bit-identical.

Training passes are the exception. Each pixel's `SampleData` is stored
contiguously in its unit's buffer (`PixelState::samples_end`), and the gather
replays them in scanline order. Interleaving a training pass's samples across
stages would break that.

See proposal.md for motivation, and the `rendering`, `cli` and `image-output` deltas
for the requirements.

## Goals / Non-Goals

**Goals:**

- A completed render is bit-identical to today's: images, AOVs, counters, goldens.
- Cancellation is prompt. The bound is one pixel advance in flight per worker,
  not one round.
- The API is shaped so a Hydra render delegate can sit on it later without
  reworking the engine. That means a caller-owned control, readable from any
  thread, with a generation counter and an outcome.
- No measurable cost to a render that nobody watches: callgrind instruction count
  on cornellbox within noise, `bench_ab.sh` within noise.

**Non-Goals:**

- Hydra, a C ABI, `cbindgen`, or any FFI crate.
- Scene or camera edits, and restarting a render. Today the caller cancels and
  calls again.
- Progressive AOV snapshots. Only the beauty is published while the render runs.
  A cancelled render still returns its AOVs, from the normal gather.
- A coarse preview below 1 spp (pixel skipping or upscaling).
- A per-session rayon pool. A caller that wants one wraps the call in
  `pool.install`, which already works.
- The diagnostic using cancellation for its time budget. That is a natural
  follow-up.

## Decisions

### D1. Staging is scheduling inside `render_pass`, always on for final passes

The first sweep's `sweep_to` becomes a stage list: `[1, 2, 4, …, < sweep_to,
sweep_to]`. Each stage is one `par_iter_mut` over the units, in which every pixel is
advanced to the stage's target. `finish_round` runs only after the last stage, so
convergence is tested at exactly the `taken` value it is today. For example,
`finish_round` at `taken = 2` could set `converged`, which no stop rule reads
before the first round, but keeping it out avoids the question entirely.

Staging is not tied to whether a control is present. It runs on every final pass.
A single code path means the bit-identity test exercises what users run. The
internal `PassConfig` keeps a `staged: bool` (default true) only so that the
bit-identity test can render the unstaged reference. It is not exposed and not an
environment switch, because this is not an optimisation to A/B.

*Alternatives:*

- Stage only when a control is attached. Rejected: two schedules to keep
  identical, and the CLI always attaches one (for Ctrl-C) anyway.
- Shrink the adaptive batches for finer updates. Rejected: it moves the decision
  points and changes the image.

### D2. Per-unit publish into a shared display buffer (not round-boundary snapshots)

Late adaptive rounds can be long (+25% of a 1024 spp budget is ~200 spp per pixel
over the full frame). Snapshotting only between rounds would leave the viewer
frozen for that long. Instead, when a unit finishes a stage or round, it computes
its pixels' estimates (`PixelState::estimate`, with the zero-sample rule of D5) and
copies them into the control's region-sized display `Buffer` under one `Mutex`.
It then bumps an `AtomicU64` generation.

The cost is one lock and about 256 pixel writes per unit per stage or round:
negligible next to the paths behind them, and contention-free in practice, since
units take milliseconds. A reader (`snapshot()`) clones the buffer under the same
lock. That is O(pixels), which is fine at checkpoint rates. A Hydra
`HdRenderBuffer::Map` that wants to avoid the clone can get a double buffer later
without an API change.

*Alternatives:*

- Atomic `f32` per channel: needs bit-casting and gives no consistent tile.
- Per-tile locks: more state than the single lock's contention justifies.

### D3. One control type, owned by the caller

```rust
pub struct RenderControl { /* cancel: AtomicBool, generation: AtomicU64, display: Mutex<Option<Buffer>> */ }
impl RenderControl {
    pub fn new() -> Self;
    pub fn without_snapshots() -> Self;                // cancels only; the render publishes nothing
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
    pub fn generation(&self) -> u64;
    pub fn snapshot(&self) -> Option<(u64, Buffer)>;   // None before the first publish
}
pub enum RenderOutcome { Completed, Cancelled }
```

A new entry point on `Renderer` takes `&RenderControl` together with the existing
`tiled`, `progress` and optional AOV request. It returns the buffer, the optional
film, `RayStats` and the `RenderOutcome`. The existing `render*` methods delegate
to it with no control and keep their signatures. The control is `Sync`, the
`Renderer` is `Sync`, and the call borrows both. The CLI uses `std::thread::scope`
for its checkpoint thread. A future Hydra thread will hold `Arc`s, which needs no
engine change.

The control is per render, so cancelling one render never leaks into the next.
A cancelled control stays cancelled. A restart takes a fresh control.

*Added while implementing:* `RenderControl::without_snapshots()`, a control that
only cancels. Each publish costs ~60 instructions a pixel (0.66% of cornellbox at
2 spp, callgrind), which a host that never shows the render in progress — the CLI
without `--checkpoint`, the diagnostic's time budget later — should not pay. The
CLI takes it unless `--checkpoint` is given.

*Alternatives:*

- A callback the engine calls with each tile. Rejected: it runs user code on
  rayon workers inside the hot loop, and Hydra wants to pull, not be pushed to.
- A render session that owns the `Renderer` and a thread. Rejected for now:
  ownership and threading are the host's policy, and this change should not pick
  one for Hydra yet.

### D4. Cancel checked before each pixel advance

`render_pass` reads the flag (`Relaxed`) once per pixel per stage or round, before
`advance`. A cancelled unit skips its remaining pixels. Between stages and rounds,
the driving thread stops scheduling. A unit is a whole row under `--scanline`, and
a round's pixel can carry ~200 samples, so a per-unit check would bound latency by
seconds on heavy scenes. The per-pixel load is about one instruction per several
thousand of path work. `Renderer::new`, `reconfigure` and the `learned` light
selection's pre-pass do not read the flag. They stay uncancellable (see Risks).

After cancellation the gather runs as usual on whatever the units hold. Pixels in
one unit may now differ in `taken`, which the per-pixel estimator handles. The
progress callback is **not** walked to the total on cancel. A cancelled render is
not complete, and the CLI bar shows where it stopped.

### D5. A pixel with no sample estimates to zero

`PixelState::estimate` divides by `weight_sum` or `taken`, which gives NaN at
`taken == 0`. Today that state is unreachable, because every pixel takes at least
`min_spp ≥ 2`. Cancellation makes it reachable. `estimate` returns `(Vec3A::ZERO,
0.0)` at `taken == 0`, and `AovFilm::store` leaves the plane at its clear value. In
a completed render, no pixel has `taken == 0`, so nothing changes there.

### D6. Guided renders: publish every pass, blend what is weightable on cancel

A guided render runs several `render_pass` calls (training passes unstaged, the
final pass staged), each publishing through the same control. The display
therefore shows the pass in progress over the previous pass's pixels. The final
pass's first stages are noisier than the last training pass, so the preview gets
noisier for a moment. That is accepted for now and recorded as a known gap in
the `rendering` design record.

On cancel, `render_guided` stops scheduling passes. It keeps the completed passes,
plus the interrupted one if every pixel in it has `taken ≥ 2`. A partial pass with
a pixel at 1 sample has no variance estimate for that pixel and would skew the
pass weight. With no completed pass, the interrupted one is returned alone. The
blend (`blend_passes`, AOV blend) then runs unchanged. The guiding field is never
consulted after a cancel, so a half-trained field cannot leak into anything.

### D7. CLI: `ctrlc` for SIGINT, a scoped checkpoint thread, exit 130

- **Signals.** `ctrlc` (MIT/Apache-2.0) installs one handler. A small state
  machine is shared with the handler through an `AtomicU8`:
  - *loading*: the handler calls `process::exit(130)`.
  - *rendering*: it calls `control.cancel()` and moves to *writing*.
  - *writing*: it calls `process::exit(130)`.

  `ctrlc` contains `unsafe` in its handler setup. It is not on the render path, so
  it does not cross the project's "unsafe on the hot path" line. `crust-render` stays
  `forbid(unsafe_code)`. `cargo deny --locked check` gates the licence and
  advisories.

  *Alternative:* `signal-hook` gives finer control at more code. Not needed for
  one signal.
- **Checkpoint.** `--checkpoint <SECONDS>` (a positive `f64`, parsed by clap's value
  parser). Inside a `thread::scope`, a thread sleeps in short steps, so it notices
  the render ending promptly. At each interval, if `generation()` moved, it writes
  the snapshot through the same `write_png` (with the same output colour) to the
  final PNG path. For a region render, the PNG is the region, as today. The final
  write overwrites the file.
- **Partial outputs.** On `Cancelled`, the CLI writes everything it would normally
  write, passing an "interrupted" flag to the EXR writers. It then logs `WARN` with
  the outcome and the min and max `taken` reached, and returns `ExitCode::from(130)`.
  `WARN` follows the logging rule: something authored (the spp budget) was not
  honoured.

### D8. The interrupted EXR attribute

`crust:renderStatus = "interrupted"` is a string attribute added only on
cancellation, both in `write_rgb_file` (the single beauty) and in
`products::write_product`. A completed render writes exactly the headers it writes
today, so the byte-identity requirement for completed renders holds trivially.

## Risks / Trade-offs

- [Staging perturbs the image through some state not keyed on the sample index:
  a cache, a learned light cache update, a guiding sample order] → The
  bit-identity test (staged vs. unstaged, tiles vs. scanlines, with AOVs and with a
  guided scene) plus `check_images.sh check` on every sample scene. The learned
  light cache is built before the passes and is read-only during them (verify
  while implementing). Texture caches affect timing, not values (already pinned by
  the streamed ↔ preloaded bitwise test).
- [More frame-wide sweeps cost locality: each stage re-touches every pixel's state
  and re-warms texture tiles] → For 6 stages to 32 spp, the extra state traffic is
  6 passes over about 64 B per pixel. Measure with `bench_ab.sh` on a texture-heavy
  scene as well as cornellbox. If it shows up, cap the stage list (e.g. start at
  4 spp), which is still bit-identical.
- [Progress counts change (units × stages + rounds)] → The contract (one report at
  a time, +1 per report) is unchanged. Update any test that pins the total.
- [Cancellation does not cover scene import, `Renderer::new` or the `learned`
  pre-pass] → The CLI exits at once on Ctrl-C before rendering. For a library
  caller, this is a stated limitation in the `rendering` design record.
- [The snapshot clone is O(pixels) under the lock workers publish through] → At
  4K this is ~100 MB/s of copying at 1 Hz, irrelevant. A Hydra delegate at 60 Hz
  will want the double buffer noted in D2.
- [A cancelled guided render discards an interrupted pass with a 1-sample pixel]
  → Accepted. The alternative is a biased-looking blend weight.

## Migration Plan

Additive API, with output unchanged for completed renders. Before merging: record
goldens on `main`, then run `check_images.sh check` on the branch, the callgrind
comparison on cornellbox at 2 spp, and `bench_ab.sh`. Rollback is a revert. No
data or file formats change for completed renders.

## Open Questions

- Should `--checkpoint` also rewrite the EXR (or products), for a farm that wants a
  usable partial frame if the job is killed with SIGKILL? That is deferrable: it
  adds a flag value, and does not change this design.
- The exact first stage. Starting at 1 spp gives the fastest first image, and the
  benchmark above may argue for 2 or 4. Either way it is bit-identical, so it can
  be tuned after measurement.
