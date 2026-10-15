# Design

## Context

See proposal.md for the motivation. The facts this design is built on:

- **`trace_path` (`tracer/path.rs`) is one loop.** It is about 1,100 lines with
  five const-generic parameters (`S`, `PROFILE`, `AOV`, `MEDIA`, `TWINS`) and
  `#[inline(always)]`, inlined into the per-pixel sample loop in `tracer/mod.rs`.
  Its comments record instruction costs of 0.2–1.4% (callgrind, cornellbox at
  2 spp) for each structural choice: the const generics, flag-beside-slot
  instead of `Option<WalkExit>`, scratch buffers instead of allocation, cold-side
  patching of the hit.
- **The path's state is already mostly explicit.**
  - Locals carried between iterations: `ray`, `remaining`, `prev: Option<PrevVertex>`,
    `beta`, `terminal`, `sss_pending`, `enclosure`, `carry`, `crossings`, and the
    path's sampler domain.
  - Per-path buffers in `PathScratch`: `records`, `first`, `route`, `sss_exit`,
    `clamp_removed`.
  - Buffers used only within one vertex, also in `PathScratch`: `thin`,
    `nee_split`, `bounce_split`.
  - The estimate is rebuilt after the walk by a backward gather over
    `records: Vec<VertexRec>`. That gather also emits guiding training samples
    into the work unit's `samples`.
- **Random draws are keyed, not sequenced.**
  - Every draw comes from `Sampler::new(i, j, seed, sample).new_domain(tile)`,
    then `K_PATH`, then the vertex index plus `crossings`, then the event key
    (`K_NEE`, `K_BSDF`, …).
  - A path's numbers therefore do not depend on when it runs. This is what makes a
    bitwise pin possible at all.
- **The extension ray is the vertex loop's one closest-hit query** with no
  exceptions: `world.intersect(&ray, TRACE_T_MIN, ∞)` at the top of each vertex,
  plus one at the depth-exhausted exit. Every other query is a follow-up that
  depends on that hit: cutout and thin-wall chains (`pass_cutouts`), medium
  boundary re-hits (`hit_past`), shadow rays (`occluded`), and walk exits that
  skip the query.
- **The per-pixel sample loop** (`tracer/mod.rs`, after the `PathContext`) folds
  each sample into `PixelState` (`sum`, `weight_sum`, `lum_sum`, `lum_sq`), AOV
  planes, the inspection store and the clamp counter, in sample order. `f64` and
  `Vec3A` sums are not associative, so this order is part of the image's bits.
- **Scheduling pins already exist.** Tiles ↔ scanlines and staged ↔ unstaged
  (`a_staged_sweep_renders_the_unstaged_one_bit_for_bit` in `tracer/tests.rs`).
  This change adds a third pin, batched ↔ one at a time.

## Goals / Non-Goals

**Goals:**
- A step function that advances one path between extension-ray queries. The
  one-path-at-a-time render becomes "step until done" with no measurable cost.
- A batched driver that is bitwise equal to that render in everything it writes.
- Numbers: where time goes today (Phase 0), and what holding a batch costs before
  any exploit (Phase 2's gate).

**Non-Goals:**
- Any sorting (by material, texture, direction), ray streams into `crust-rt`, SIMD
  across paths, or batched inference. Each is a follow-up change with its own A/B.
- Batching shadow rays or any query other than the extension ray.
- Changing the default schedule. `CRUST_WAVEFRONT` stays `0`.
- Refilling a batch's dead slots with new camera samples mid-batch (see Open
  Questions).

## Decisions

### D1. Yield only at the extension ray

The step runs from a hit to the next extension ray the path needs, so one call
covers:
- emission and MIS at the hit,
- material resolve,
- NEE with its shadow rays,
- cutout, thin-wall and boundary chains,
- subsurface walks,
- the bounce sample and roulette.

A walk exit needs no query (`sss_pending`). The step carries on through it
without yielding, exactly as today's loop does.

*Alternatives.*
- **Yield at every kernel query.** That means splitting NEE, the cutout chains and
  the boundary chains into resumable pieces: most of the 1,100 lines turned
  inside out before anything is measured.
- **Split each vertex into shade, NEE and bounce stages.** That is what sorting
  needs, but it only pays once sorting exists.

Both are follow-ups that this split makes local edits rather than rewrites. The
extension ray is the query with the most to gain from coherence later (camera
rays and first bounces), and the only one every vertex issues.

### D2. Path state and step

All of the loop's carried locals move into one `PathState<'w>`, along with the
per-path buffers `records`, `first`, `route`, `sss_exit` and `clamp_removed`.
`PrevVertex<'a>` already borrows from the world, so the state borrows for the
render's lifetime. The per-vertex buffers (`thin`, `nee_split`, `bounce_split`) are
written and read within one step, so they stay per worker and are shared across a
batch's slots.

The interface is three functions:
- `begin`: camera ray in, state out.
- `step`: state plus the extension hit; returns `Extend(ray)` or `Done`.
- `finish`: the backward gather. Takes the state and the slot's training-sample
  buffer, returns the radiance.

All three keep the five const generics.

The depth-exhausted exit's intersect becomes an `Extend` whose answer `step` treats
as the terminal query. The state knows it asked a terminal query (`remaining <= 0`),
so no new variant is needed.

*Alternative.* A generator or coroutine (unstable `Coroutine` trait, or an `async`
state machine). Not on stable Rust 2024 for this purpose, and an `async` lowering
would hide state layout from the instruction budget.

### D3. Phase 1 must cost nothing measurable

The one-path driver keeps the `PathState` in a local, calls `step` in a loop, and
everything stays `#[inline(always)]`. LLVM's SROA should put the state back in
registers.

The budget is **≤ 0.3% more instructions** in `advance_pixel` (inclusive, callgrind,
2 spp, `RAYON_NUM_THREADS=1`) on `cornellbox`, `materialx_basic` and
`openpbr_showcase`, and `check_images.sh check` identical. The path's comments
already cite costs of 0.2–1.4%, and 0.3% is the noise floor those comments treat
as real.

If SROA fails, the fallback is to keep hot locals (`ray`, `beta`, `remaining`) as
`step` arguments and returns rather than state fields. That gets measured before
being chosen.

### D4. A batch is whole pixels of one unit's stage, in pixel order

The sweep already advances a work unit one stage, or one adaptive round, at a
time. Pixels whose stage target is above `taken` contribute `target − taken`
samples each.

A batch takes **whole pixels**, in the unit's pixel order, until adding the next
pixel would exceed the cap `K`. A single pixel larger than `K` gets a batch alone.
Slot order is therefore (pixel, sample), exactly the scalar loop's order.

`K` is an internal constant, not a user switch. It starts at 1024 paths:
- at depth 32, about 32 × ~190 B of `VertexRec` per path (~6 MiB per worker) at
  full depth;
- in practice paths are much shorter;
- `Vec` capacity is reused across batches as `PathScratch` does today.

Phase 2 measures `K ∈ {256, 1024, 4096}` and records the result.

*Alternative.* Batches that span units or stages. That breaks the progress,
snapshot and cancellation granularity, the staging pin and the adaptive pin, so
it is rejected.

### D5. The round loop

```
begin every slot          (camera ray, filter weight, primary cone: the scalar
                           loop's preamble, per slot)
loop:
  for live slots in slot order:  hit[s] = world.intersect(ext[s])   // scalar queries
  for live slots in slot order:  step(state[s], hit[s]) -> Extend | Done
  if no slot is live: break
for slots in slot order: finish(state[s]) -> fold into pixel, AOV, inspection, clamp
```

- The intersect loop is still a scalar `world.intersect` per slot. A
  stream-query API is follow-up work, and Phase 2 measures only the cost of the
  structure.
- `RayStats` counters are integer sums, so where they are incremented does not
  matter.
- Profile sections nest as before: `Trace` around the query loop, everything
  else inside `step`.

### D6. Accumulate in slot order

Nothing is folded into a pixel until the batch is done. `finish` and the fold
then run in slot order, so each pixel sees its samples in increasing sample
index. The adaptive test runs after the batch, as it runs after the scalar loop
today.

Guiding training samples are produced inside `finish`, into the unit's `samples`
in slot order. That matches the scalar order, so the field is trained identically.

*Alternative.* Fold each path as it retires and sort later. Not bitwise: the sums
would differ.

### D7. The Phase 2 gate

The gate is measured against `CRUST_WAVEFRONT=0` on the default
`scripts/bench_scenes.sh` scenes, and on ALab and Moana when available:
- **callgrind:** ≤ 3% more instructions;
- **`bench_ab.sh`:** min and mean wall-clock ≤ 5% slower, reported per scene;
- **`--stats`:** the peak resident memory increase.

**Pass:** follow-up exploit changes build on this driver.

**Fail:** the record names the cost and its cause, for example cache misses on
the state arrays. A struct-of-arrays layout of the hot fields is the first
candidate fix, before any exploit work.

Either way the switch stays opt-in. Only an exploit change can argue for making
it the default.

### D8. Phase 0 is a reading of existing instruments

- `--profile` already splits `Trace`, `Occlusion`, `EvalBsdfs`, `RunShader`,
  `Texture`, `TextureLoad` and `SurfaceLighting`.
- callgrind attributes interpreter dispatch (`Program::apply_op`).

The table goes into the `rendering` design record as "Where a path's time goes",
one row per scene. The proposal's ranking of exploits (shading dispatch, texture
coherence, ray coherence) is then a reading of that table, not an assumption. No
new instrument is built.

### D9. The switch

`CRUST_WAVEFRONT` is a `Config` field in `crust-core/src/config.rs`, parsed once.
A bad value warns once and falls back to `0`. The change also adds:
- a row in `docs/architecture.md` § Environment switches;
- a section on the user page.

The off side is today's driver, so the A/B is honest. It applies to every render that goes through
the sweep (`advance_pixel`), `crust light-paths`' inspection store included. The
`ray_color*` entry points (tests and probes) keep the one-path driver, which runs the
same step code.

## Risks / Trade-offs

- **[SROA does not recover the state, and Phase 1 costs more than 0.3%.]** → Use D3's
  fallback (hot locals stay arguments). Measure the struct layout before
  splitting it. Never trade the budget for a cleaner signature.
- **[`trace_path` keeps evolving under in-flight changes]**
  (`neural-visibility-light-selection`, `learned-light-selection-by-default`).
  → The split moves the loop body without
  changing what it does, and NEE and the bounce stay inside `step`. A change that
  lands after this one edits `step` where it would have edited the loop.
- **[A subtle reorder breaks a bitwise pin]**, for example an AOV `planes.add`
  that read worker-shared scratch now overwritten by the next slot. →
  - Per-slot buffers are named in D2.
  - The pins run on every sample scene, tiles × scanlines × staged × unstaged,
    plus AOV, inspection and guided renders.
  - A cap test runs `K = 1` and a non-divisor `K = 7` against the default.
- **[A batch that keeps few paths live wastes rounds]** (one long subsurface or
  glass path keeps a batch alive). → It is harmless to correctness and measured
  by the gate. Refilling dead slots is the follow-up fix (Open Questions).
- **[Cancellation latency grows]** from one pixel advance to one batch per worker.
  → At `K` = 1024 and 1 spp that is one tile's worth of samples, the latency the
  tiled stage-1 sweep already has. The `rendering` delta states it.
- **[Memory per worker]** grows with `K` × the live path depth. → The cap, `Vec`
  reuse, and the gate's peak-RSS row.

## Migration Plan

There is nothing to migrate.
- Phase 1 lands as a refactor pinned by the existing goldens and tests.
- Phase 2 lands off by default.
- Rollback is reverting the commit, or leaving the switch at `0`.

## Open Questions

- **Refilling slots mid-batch.** Starting the next pixel's samples in dead slots
  keeps the batch full. It is order-safe only if the fold still runs in
  (pixel, sample) order, which a per-slot result buffer allows. It is worth doing
  once an exploit makes a full batch valuable; it does not change this change's
  pins or tasks.
- **Whether `K` should follow the work unit** (a 16×16 tile at 1 spp is 256 paths).
  Measured in Phase 2. It stays an internal constant either way.
