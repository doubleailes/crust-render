## Context

The streamed `.tx` path today (`crates/crust-assets/src/tiled/cache.rs`, textures design
record § "Streaming"):

- **Three tiers.**
  - A per-thread microcache (`MICRO`, `thread_local!`): 16 sets × 4 ways, FIFO within a
    set, keyed by `(cache id, TileId)`. The set is `(file + 5 · cache id) mod 16`, so
    every tile of every mip level of one texture lands in one set.
  - 64 shards, each a `Mutex<HashMap<TileId, Entry>>`.
  - A decode from a pooled reader.
- **`with_tile` hands the tile to a closure.** A microcache hit therefore touches no
  reference count and no shared memory.
- **A miss writes shared memory three times:**
  - it locks the shard;
  - it clones the tile's `Arc` to put it in front of its set;
  - it later drops the `Arc` that falls off the end of the set.

  On hot tiles, which every thread reads, these are cache lines moving between cores.
- **Budget.** `resident` counts the bytes in the shards against `CRUST_TEX_CACHE_MB`.
  When an insert takes it over, one thread `try_lock`s the clock sweep (`make_room`),
  and no thread ever blocks on it. The microcache's tiles are not counted: 64 per
  thread, up to 108 MiB at 72 threads.
- **One `TileCache` per `FileAssets`.** The microcache is per thread and process-wide,
  shared by every cache in the process, which is why the cache id is in the key.
- **Payloads.** A tile is at most 64×64 texels of `u8` or `f16` RGB: 12 or 24 KiB plus
  a small header (`Tile::bytes`).
- **The measurement this starts from** (`docs/alab_profile.md` § Thread scaling, ALab
  frame 1004, 32 spp):

  | | 8 threads | 72 threads |
  |---|---|---|
  | Texture per `eval` | 895 ns | 1.71 µs |
  | microcache hits | 84.4% | 84.4% |
  | lookups reaching a shard | 152 M | 152 M |
  | texture `eval`s | 147 M | 147 M |

  The machine's own 8 → 72 factor is 1.6×.
- **The previous step.** Going from 8 to 16 sets moved hits from 81.2% to 84.9%, and
  Texture time from 1.76 to 1.57 µs per `eval`, bit-identical. It added no environment
  switch.

## Goals / Non-Goals

**Goals:**
- Fewer shard hits per `eval` on ALab, chosen by measurement rather than by guess.
- No write to shared memory on any per-thread hit, at either level.
- Thread-held tiles bounded by, and reported against, `CRUST_TEX_CACHE_MB` (the spec's
  new requirement).
- Bit-identical images, no slower lookups on single-texture scenes, preloaded scenes
  untouched.

**Non-Goals:**
- **Making the shard map lock-free.** That needs `unsafe` or a dependency that carries
  it, which `CLAUDE.md` makes a project decision. The lever here is how often the shard
  map is reached, not what reaching it costs.
- **The Ptex stream's microcache.** It is four slots, behind one reader `Mutex`, with its
  own `micro_reserve`, and `stream-ptex-by-default` is changing that path. The lessons
  here apply there, as a follow-up.
- **The end-of-pass idle time**, the other 4.9% of the 72-thread gap. That is a
  scheduling change, not a texture one.

## Decisions

### D1. Measure which misses dominate, then choose the shape

Three kinds of miss call for three different fixes:

| kind of miss | why it happens | the fix |
|---|---|---|
| two textures in one set | 5,722 files on 16 sets, with the next shading point's material landing on the last one's | more sets |
| one texture needing more than 4 tiles at once | trilinear reads two levels; at a tile edge or corner each needs 2–4 | more ways, or the set from the whole tile id |
| plain capacity | the tiles used over many shading points exceed 64 | more of both |

Nothing measured so far separates them. The spike builds the variants below and reads
`--stats` on ALab frame 1004 at 32 spp and 72 threads. One run per variant is enough:
the hit counts depend on which thread renders which tile, but barely (the 8- and
72-thread runs differ by 1,500 hits in 820 M).

| variant | sets × ways | set index | tiles per thread | what it isolates |
|---|---|---|---|---|
| A (today) | 16 × 4 | file | 64 | baseline |
| B | 64 × 4 | file | 256 | textures sharing a set |
| C | 16 × 8 | file | 128 | one texture's corner and trilinear taps |
| D | 16 × 4 | whole tile id | 64 | the same, at no extra memory |
| E | 64 × 8 | whole tile id | 512 | capacity, against B to D |
| F | A + a 192-tile second level | file | 256 | the move-not-clone second level (D3), against B at equal capacity |

**The gate.** A shape ships only if all of these hold:
1. shard hits per `eval` fall at least 25% (so Texture's 6.7% has room to show);
2. ALab's render is faster by `bench_ab.sh` at both min and mean;
3. the generated alias scene's render is no slower;
4. its single-threaded instruction count grows less than 1% (callgrind).

**Relaxed after the spike (2026-10-09): tests 3 and 4 accept up to 1.5%.** E2 misses
them by about half a point (+1.4% instructions; +0.9% min / +1.5% mean in time). The
cause is inherent to the gain: choosing the set from the tile costs about two
instructions per tap, where a file-only index was computed once per lookup. The alias
scene's microcache already hits 99.9%, so it pays that cost with nothing to gain, while
ALab renders 14.6% faster. The trade was taken deliberately, and the design record keeps
it.

**The shipped binary, measured against its parent (2026-10-09).**

| check | result | against the relaxed gate |
|---|---|---|
| ALab, `bench_ab.sh -n 3` | 7.28 / 7.34 s → 6.12 / 6.17 s (**−15.9%**, min and mean) | passes |
| alias scene, instructions | **+1.34%** | inside 1.5% |
| alias scene, time (`-n 16`, 256 spp) | **+2.3% / +2.1%** (min / mean) | **over 1.5%** |

The time is over the relaxed limit while the instructions are inside it. The
difference is cache behaviour that callgrind does not count: the per-thread array is
12 KiB, where it was 1.5 KiB. This is reported rather than relaxed again; whether
it stands is the reviewer's call.

**Spike results (2026-10-08).**
- **Setup:** ALab frame 1004, 32 spp, 72 threads, `--stats`, 147.3 M texture `eval`s.
  Render times are single runs, some beside a build, so indicative only.
- **The alias scene** is the one generated with
  `scripts/gen_texture_alias_scene.py --udim=8 --size=4096` (13.9 M lookups, 171
  tiles). Its instruction count is at one thread and `-s 2`; before the change it is
  2,040.4 M.
- **The variants added after the first round:**
  - *E capped:* E with D5's share enforced.
  - *G:* 64 × 4, set from the whole tile id.
  - *E2:* E with a one-multiply (Fibonacci) hash and `Ways::get` forced inline.
  - *H:* E2's shape, set from (file, level).

| variant | shard hits | vs A | shard hits per `eval` | Render | alias instructions |
|---|---|---|---|---|---|
| A (today) | 152.0 M | — | 1.03 | 7.42 s | 2,040.4 M |
| B 64×4 by file | 104.2 M | −31% | 0.71 | 6.82 s | |
| C 16×8 by file | 117.0 M | −23% | 0.79 | 7.19 s | |
| D 16×4 by tile id | 138.6 M | −9% | 0.94 | 7.32 s | |
| E 64×8 by tile id | 49.4 M | −68% | 0.34 | 6.34 s | |
| F 16×4 + 192 second level | 87.5 M | −42% | 0.59 | 6.77 s | |
| E capped | 49.4 M | −68% | 0.34 | 6.26 s | 2,104.2 M (+3.1%) |
| G 64×4 by tile id | 78.8 M | −48% | 0.54 | 6.57 s | |
| **E2** | **50.3 M** | **−67%** | **0.34** | **6.16 s** | **2,069.4 M (+1.4%)** |
| H 64×8 by (file, level) | 50.6 M | −67% | 0.34 | 6.25 s | 2,091.4 M (+2.5%) |

- **Capacity is what pays.** Textures sharing a set (B against C) cost more than one
  texture's corner taps (C, D), and at equal capacity a full tile-id index (G) beats
  both the file index (B) and the second level (F).
- **D5's share does not bind on ALab.** Its tiles are mostly `u8`: 542 MiB over 48,010
  tiles is about 11 KiB each. So 512 tiles fit in the 7.1 MiB share, and E capped
  equals E.
- **E's extra instructions** were `Ways::get` falling out of line at 8 ways (40.8 M) and
  the four-multiply hash (+24.0 M in `texel`). E2 fixes the first, and halves the
  second. H keeps the hash out of the per-tap path, but its sets hold a whole level's
  tiles, so hits sit deeper in the scan and it costs more.
- **E capped against A, measured properly:**
  - **ALab, `bench_ab.sh -n 3`:** 7.324 / 7.367 s → 6.252 / 6.284 s (min / mean),
    **−14.6% / −14.7%**.
  - **ALab, profiled pair:** Texture 895 ns → 788 ns at 8 threads, 1.71 → 1.28 µs at
    72. The 8 → 72 slowdown is now **1.62×**, the machine's own factor. The render
    goes 41.38 → 38.69 s at 8 threads and 8.02 → 6.93 s at 72: **5.58×** over 8
    threads, against 5.16×.
- **E2 against A, alias scene at `-s 256`, `bench_ab.sh -n 8`:** +0.9% / +1.5%
  (min / mean). That is gate tests 3 and 4 missed by about 1%, on a scene whose
  microcache already hits 99.9% and so has nothing to gain.

Otherwise the change stops at the numbers. The design record keeps them, and the
accounting below is not built for a cache that did not grow.

*Alternative considered:* adopt 64 × 4 without measuring, extrapolating the last step.
Rejected: the last step was one data point, and B, C and D cost different things
(memory, scan length, isolation). The spike is about 40 minutes of builds and runs.

### D2. The set index may change from the file to the whole tile id

Today the set is chosen by file, so that "each texture's recent tiles stay out of the
others' way". Hashing the whole `TileId` (file, level, tile) instead spreads one
texture's taps over several sets. That fixes a corner or trilinear lookup needing more
tiles than one set has ways, at the price of letting every texture compete everywhere.

Variants D and E measure that trade, and it is kept only if it wins. The hash is a few
multiplies, like `shard()`'s, inlined into the lookup.

### D3. A second level, if it wins, moves tiles and never clones them

Variant F keeps today's 64-tile first level and adds a larger per-thread second level
behind it:
- a tile falling off the end of a first-level set moves into the second level instead
  of being dropped;
- a second-level hit moves the tile back to the first level, and the first level's
  evictee takes its place.

Moving an `Arc<Tile>` touches no reference count, so tiles shuffling between the two
levels never write shared memory. Only a true shard hit (one clone) and a final eviction
off the second level (one drop) do.

*Alternative:* a single larger level (B). Same capacity, one probe, but every eviction
drops an `Arc`. F is worth its second probe only if the drops are what costs, and F
against B at equal capacity is that measurement.

### D4. Thread-held tiles are counted when the shared map evicts them, not reserved up front

The Ptex stream's precedent, `micro_reserve`, sets aside the worst case (threads × slots
× slot size) out of the budget before anything is cached. For the `.tx` cache at 256
tiles a thread, that reserve would be up to 432 MiB at 72 threads. ALab at 1 GiB would
lose that much shared capacity, while in fact it holds nothing beyond the shared map:
every tile a thread holds is also in a shard.

The cost that needs bounding is only a tile the shared map has **evicted** while a
thread still holds it. So:
- **When the sweep evicts an entry,** it marks the tile with its cache's held-bytes
  counter and adds the tile's bytes to that counter, while the shard's own `Arc` still
  keeps the tile alive.
- **`impl Drop for Tile`** subtracts the bytes if the tile was marked. If no thread held
  it, the drop happens inside the same sweep, and the two cancel.
- **The sweep's target becomes `resident + held ≤ budget`.** It can only evict shard
  entries, so while threads hold evicted tiles it keeps fewer tiles in the shards.
- **The writes happen on eviction and on a marked tile's final drop**, both rare. A hit
  at either level writes nothing.
- **The counter's peak is what `--stats` reports**, as the bytes held by threads after
  eviction.

Marking before the shard drops its `Arc` is what makes this race-free. The tile cannot
reach its final drop unmarked once the sweep has chosen it, so no interleaving leaves
bytes added and never removed, or removed twice. The mark is set once (a `OnceLock`
holding a handle to the counter).

*Alternative:* the Ptex-style reserve. It is simpler, but pessimistic by the whole
reserve on every scene, and it would put ALab's working set (537–598 MiB across
versions) right at the edge of a 592 MiB shared map. Kept for Ptex, whose reserve is
small.

### D5. Each thread retains at most `budget / 2 / threads` bytes of a cache's tiles, checked on a miss

D4 counts held bytes, but the sweep cannot evict them. If every thread held a full
cache of evicted tiles, `held` alone could exceed the budget. So each thread's share is
capped in bytes:

```text
bytes a thread retains ≤ (budget / 2) / threads
```

- **Checked when a tile is put in the microcache, on the miss path only.** Each thread
  keeps its retained bytes in a thread-local count. An insert that would take the count
  over the inserting cache's share is skipped: the tile is used for this lookup and
  dropped. A hit, at any level, reads no shape and writes nothing, so the lookup path
  is exactly what it was.
- **Per cache, not process-wide.** The share comes from the inserting cache's own
  budget. A render has one cache; the tests build several, some at 1 MiB, whose share
  is zero.
- **In bytes, not tiles.** `u8` tiles are half the size of `f16` ones, so a byte share
  holds twice as many of them. At zero, every lookup goes to the shard: slower, still
  right, and the spec's third scenario.
- **The thread count** is the one the Ptex stream already uses (`micro_threads`,
  `available_parallelism`, memoised). It overestimates when `RAYON_NUM_THREADS` is
  lower, which errs on the safe side.

At the default 1 GiB on 72 threads, the share is 7.1 MiB, about 300 of ALab's `f16`
tiles. B and F (256 tiles) fit, and E (512) does not. The spike measures E with the
share enforced.

*Rejected while building the spike:* shrinking the sets, then the ways, at run time,
with the strictest cache setting a process-wide shape.
- **It costs every lookup.** The lookup would read the shape each time: a global
  load and a mask on the hottest path, about 2% more instructions on the alias scene,
  against the gate's 1%.
- **It breaks unrelated caches.** A process-wide value lets one 1 MiB test cache switch
  the microcache off for every other cache in the process.

### D6. No environment switch

The A/B is the two binaries through `bench_ab.sh`, as for the 8 → 16 step. A cache
shape fixed at compile time cannot be switched at run time without paying for the
switch on every lookup. `CRUST_TEX_CACHE_MB` keeps its name and default. Only its
meaning tightens, from "the shards" to "the shards and the tiles threads still hold
after eviction", and the docs say so.

## Risks / Trade-offs

- **[A bigger thread-local array costs more than its hits save]** 64 × 4 ways at 24 B
  each is 6 KiB; 64 × 8 is 12 KiB, against a 32 KiB L1 data cache.
  → The gate's callgrind check catches extra instructions. Texture time per `eval` at
  8 threads, where contention is small, must not rise.
- **[Hashing the tile id loses per-texture isolation]** → Measured in D and E, and
  kept only if it wins.
- **[An accounting bug leaks or double-counts held bytes]** → Unit tests:
  - evict a tile no thread holds → `held` returns to 0 within the sweep;
  - evict a tile a thread holds → `held` = its bytes, and 0 after that thread's cache
    drops it;
  - two caches in one process count separately.

  A debug assertion that `held` never wraps below zero.
- **[Under pressure the shared map holds less]** By exactly the evicted tiles threads
  still hold. That is the point of the bound, and those tiles are still being used.
  → The spec's small-budget scenarios. Images must stay bit-identical, and a re-read
  decodes the same bytes.
- **[The gain is below what wall-clock timing can resolve]** → Gate on the counters,
  which do not vary between runs, and on `bench_ab.sh` (ALab is about 2.5 min of import
  per run, so `-n 3` is about 20 min).
- **[The magnitude is this VM's]** Cross-socket line transfers make every shared write
  dear here. On a single-socket machine the contention is smaller. Fewer shard hits
  still remove their uncontended cost (a lock, a hash probe, a clone), which the
  8-thread time per `eval` shows.

## Migration Plan

None for scenes or flags. Rollback is reverting the commit. The meaning of
`CRUST_TEX_CACHE_MB` tightens in the same commit as its documentation
(`docs/architecture.md` § Environment switches,
`site/content/docs/reference/environment-variables.md`).

## Open Questions

- Whether the spike's winner is also the best shape at 8 threads. Its decision is
  taken at 72 (the problem being solved). If the 8-thread run disagrees, the design
  record notes it, and the shape does not change.
