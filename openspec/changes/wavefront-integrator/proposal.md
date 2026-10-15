## Why

crust traces one path at a time. `trace_path` (`tracer/path.rs`) follows a camera
sample from its first hit to its last vertex on one thread's stack, so every
kernel query, shader run, texture fetch and (in the in-flight neural changes)
network inference is issued for a single path, in path order. That has three
measured or recorded costs:
- the kernel's design record lists "rays are traced one at a time, so there is no
  coherent ray-packet tracing (that needs the integrator restructured, not just
  the kernel)" as a known gap;
- the checked-in scenes are shading-bound, and the MaterialX interpreter pays one
  `match` dispatch per instruction per hit (`docs/shading_performance.md`);
- ALab and Moana stream `.tx` tiles and Ptex faces in path order, the access
  pattern Hyperion's sorted deferred shading (Eisenacher et al., EGSR 2013) was
  built to fix.

`neural-visibility-light-selection` and `neural-bvh-experiment` both gate on
the cost of one inference. Run one path at a time, that is a 1×64
matrix-vector product: memory-bound on a CPU. A batch of live paths turns the
same query into a matrix product.

This change builds the foundation every one of those wins needs: an integrator
that can hold many paths in flight and advance them stage by stage. It is
pinned **bit for bit** to today's integrator, so it changes the schedule and
nothing else. Exploiting the batch (sorting by material or texture, ray
streams, batched inference) is left to follow-up changes, each measured on its
own.

## What Changes

- **Phase 0: a measurement, recorded whatever it shows.** `--profile` and
  callgrind numbers for the default bench scenes, plus ALab and the Moana island
  where the assets are available. They say how much of a render is shader
  dispatch, traversal, texture misses and shadow rays, which is how much any
  coherence could recover. The table goes into the `rendering` design record. It
  decides which exploit change comes first. It does not gate Phases 1 and 2.
- **Phase 1: a resumable path.** `trace_path`'s loop is split into
  - an explicit per-path state, and
  - a step function that advances one path from one extension-ray hit to the
    next extension ray it needs.

  The existing one-path-at-a-time render drives that step in a loop. Output is
  bit-identical to today: images, AOVs, light-path inspection records, guiding
  training samples, counters. The cost is held to an instruction-count budget
  measured with callgrind. No user-visible switch: this is a refactor.
- **Phase 2: a batched sweep, opt-in.** A new driver holds a batch of live paths
  per worker. The batch is one work unit's (tile's or row's) stage of the sweep:
  it already exists as a scheduling boundary that the staging and adaptive pins
  respect. The driver
  - issues each round's extension rays together,
  - steps every path to its next query,
  - retires finished paths, and
  - accumulates each sample's results in sample order, so every per-pixel sum
    matches the one-path-at-a-time render.

  Selected by a new environment switch, `CRUST_WAVEFRONT` (`0` | `1`, default
  `0`; the off side is today's driver). It is **bit-identical** to the default in
  every image, AOV, counter and guiding field, under tiles and scanlines, staged
  and unstaged, guided or not. Shadow rays, cutout and thin-wall chains, medium
  crossings and subsurface walks stay inside a step in this change; promoting
  them to batched stages is follow-up work.
- **A gate on Phase 2's overhead.** `bench_ab.sh` and callgrind measure the batched
  sweep against the default on the bench scenes. The batch costs memory traffic
  and gains nothing until an exploit lands, so the gate is only that this
  overhead is small enough to be won back (design.md, D7). The result is
  recorded either way. If the gate fails, the switch stays off and the record
  says why.
- Documentation:
  - the `rendering` design record (the measurement, the state and step split, the
    batch, the gate's result);
  - `docs/architecture.md` (the new switch row; the invariants list gains
    "batched ↔ one path at a time");
  - the user page for `CRUST_WAVEFRONT` in `site/content/docs/reference/environment-variables.md`;
  - the kernel record's packet gap, updated to point at this change.

No default changes. With `CRUST_WAVEFRONT` unset, every render is bit-identical
to before this change.

## Capabilities

### New Capabilities

None. Path scheduling belongs to `rendering`; the switch belongs to `cli`.

### Modified Capabilities

- `rendering`: new requirements:
  - a batched sweep produces bit-identical output to the one-path-at-a-time
    sweep;
  - each pixel accumulates its samples in sample order whatever the schedule;
  - a batch never spans a stage or a work unit.
- `cli`: a new requirement for the `CRUST_WAVEFRONT` scheduling switch: parsed
  once, warns once on a bad value, documented with its off side.

## Impact

- **Code:**
  - `crates/crust-core/src/tracer/path.rs`: `trace_path`'s body becomes the path
    state plus the step; the one-path entry points (`ray_color*`) keep their
    signatures.
  - `crates/crust-core/src/tracer/mod.rs`: the per-pixel sample loop gains a
    batched counterpart.
  - New module `tracer/wavefront.rs`: the batch, its slots and the round loop.
  - `crust-core/src/config.rs`: the `CRUST_WAVEFRONT` field.
- **Tests:** new bitwise pins, batched ↔ default, across the sample scenes, tiles
  and scanlines, staged and unstaged, AOV, inspection and guided renders. The
  existing goldens (`scripts/check_images.sh`) stay unchanged.
- **Performance:**
  - Phase 1 is bounded by its instruction budget; with the switch off, nothing
    else moves.
  - Phase 2 is opt-in. Its cost is measured and recorded, not hidden.
  - Memory: one path state per live path, `VertexRec` records included,
    bounded by the batch cap (design.md).
- **No new dependencies, no `unsafe`, no GPU.**
- **In-flight changes:** `neural-visibility-light-selection`, `neural-bvh-experiment`
  and path guiding touch the vertex loop. Whichever lands second rebases onto the
  other's step. The design keeps NEE and the bounce sample inside the step, so
  their seams do not move.
