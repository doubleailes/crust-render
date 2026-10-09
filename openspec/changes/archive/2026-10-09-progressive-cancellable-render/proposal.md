# Proposal

## Why

`crust-core` renders as one blocking call that only produces an image at the end,
and it cannot be stopped. The caller gets nothing but a `(done, total)` count.
Every interactive host needs the same two things from the engine: an image that
improves while the render runs, and a way to stop the render and keep what it has
done. That covers a Hydra render delegate (the longer-term goal), a viewport, and
even the CLI on a long render. This change adds both to the engine first, with the
CLI as the first consumer. That keeps the API honest before any FFI or Hydra code
exists.

## What Changes

- **Staged first sweep.** A final pass no longer takes every pixel straight to the
  first adaptive check point (≥ 32 spp by default) in one go. It sweeps the frame
  in stages of 1, 2, 4, 8, … samples per pixel up to that point, and then runs the
  existing adaptive rounds unchanged. Each pixel draws the same sample indices in
  the same order and is checked at the same `taken` values against the same
  frozen buffers. The image is therefore **bit-identical** to today's, as tiles and
  scanlines already are: a render mode stays scheduling only.
- **Live snapshots.** A caller-owned render control receives, as each work unit
  finishes a stage or a round, that unit's current per-pixel estimate. Any thread
  can read the latest full-frame beauty image at any time. A generation counter
  tells the reader whether anything changed.
- **Cooperative cancellation.** The same control carries a cancel flag. Workers
  check it before each pixel's advance. A cancelled render returns promptly with
  the image, the AOVs and the counters of the work it did, and an outcome that
  says it was cancelled. A pixel that received no sample is written as zero, never
  as NaN.
- **Path guiding:** training passes publish as they go. A cancelled guided render
  returns the blend of the passes it completed. It adds the interrupted pass only
  when that pass is the only one, or when every pixel in it has at least two
  samples, which is what its variance estimate needs.
- **CLI as first consumer:**
  - Ctrl-C stops the render and writes the partial outputs, with a warning and
    exit code 130. A second Ctrl-C exits at once without writing.
  - `--checkpoint <seconds>` rewrites the tone-mapped PNG preview from the latest
    snapshot while the render runs.
- **Interrupted EXRs say so:** an image written from a cancelled render records
  that in its header.
- The progress callback counts samples: each unit has one step per sample per pixel
  it is scheduled, capped at 64 and shared out in proportion past that, so the staged
  sweep's uneven stages still move the bar in step with the work (revised after
  review; it first counted unit-stages). Its contract (one report at a time,
  increasing by one) is unchanged.

## Capabilities

### New Capabilities

None. Progressive output and cancellation describe how a render runs, so they
extend `rendering`, not a new capability.

### Modified Capabilities

- `rendering`: new requirements for the staged final-pass sweep (bit-identical to
  the unstaged one), live snapshots of the beauty during a render, cooperative
  cancellation and what a cancelled render returns (guided renders included).
- `cli`: new requirements for interrupting a render with Ctrl-C and for the
  `--checkpoint` preview flag.
- `image-output`: a new requirement that EXRs written from an interrupted render
  carry a header attribute saying so.

## Impact

- **Code:**
  - `crust-core/src/tracer/mod.rs`: `render_pass` staging, per-unit publish, cancel
    checks, the zero-sample estimate, guided cancellation.
  - New public types re-exported from `crust_core`: the render control, the
    snapshot, the outcome.
  - `crust-render/src/main.rs`: signal handling, checkpoint thread, exit code.
  - `crust-render/src/products.rs` and the beauty writer: the EXR attribute.
- **API:** additive. The existing `render*` methods keep their signatures and
  behaviour, and a new entry point takes the control. `Renderer` is already
  `Sync`, so a caller can run the render on one thread and read snapshots or
  cancel from another.
- **Dependencies:** the CLI gains a signal-handling crate (`ctrlc`, MIT/Apache-2.0).
  The crate carries `unsafe` internally, but only in the signal handler, never on
  the render path. It must pass `cargo deny`.
- **Performance:** one relaxed atomic load per pixel per stage or round, a few more
  per-unit loop starts in the first sweep, and one copy of each unit's pixels into
  the shared display buffer per stage or round. Verify with callgrind on
  cornellbox (instruction count) and with `bench_ab.sh`.
- **Output:** unchanged for a render that completes. `check_images.sh check` must
  pass against goldens recorded before the change.
- **Docs:** `site/` command-line reference (`--checkpoint`, Ctrl-C), the
  `rendering` and `cli` design records, `docs/architecture.md` (Seams table, render
  flow). In `aovs/design.md`, the non-goal "Display drivers and progressive output"
  becomes "progressive AOV snapshots and display drivers".
- **Not in scope** (see design.md Non-Goals): Hydra, FFI, scene edits and
  restarts, progressive AOV snapshots, the diagnostic's use of cancellation.
