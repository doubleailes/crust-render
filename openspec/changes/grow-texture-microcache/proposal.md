## Why

On ALab at 72 threads, a streamed texture call is 1.91× slower than at 8 threads,
against the 1.6× this VM costs any thread doing the same work at 72-way occupancy
(`docs/alab_profile.md` § Thread scaling, measured 2026-10-08). The 1.19× left over
is 6.7% of the 72-thread render's thread capacity, and it is the only render section
with contention of its own.

The source is the per-thread microcache in front of the `.tx` tile cache. It is
16 sets × 4 ways, and the set is chosen by file alone. It misses on 15.6% of 972 M
lookups, about once per `eval`. Every miss writes cache lines that other threads read:
- it locks a shard `Mutex`;
- it clones the tile's `Arc`;
- it later drops the `Arc` it evicts.

The tiles a thread holds also sit outside `CRUST_TEX_CACHE_MB`: 64 per thread, up to
108 MiB at 72 threads. So the cache cannot simply be made bigger without growing
memory nobody accounts for.

## What Changes

- **Measure the misses before choosing a shape.** Variant builds of the microcache
  are read through the counters that do not vary between runs (microcache hits, shard
  hits per `eval`) on ALab at 72 threads:
  - more sets;
  - more ways;
  - the set chosen from the whole tile id rather than the file;
  - a second per-thread level that receives evicted tiles.

  Only the shape that pays ships. If none removes enough shard hits to show in
  `bench_ab.sh`, the change stops at the measurement, and the design record keeps the
  numbers.
- **The per-thread cache grows** to the measured shape. If the second level wins, an
  evicted tile moves into it instead of being dropped, and a hit there moves it back.
  Moving an `Arc` touches no reference count, so only a true shard hit writes shared
  memory.
- **Tiles held by threads count against `CRUST_TEX_CACHE_MB`.**
  - Each thread's capacity is capped at startup, so that all threads together can hold
    at most half the budget.
  - A tile the shared cache evicts while a thread still holds it stays counted until
    the last holder drops it.
  - The sweep keeps shared plus still-held bytes within the budget.

  Tiles that are in both places are counted once, so a scene that evicts nothing
  (ALab, at 537 MiB of 1 GiB) loses no shared capacity.
- **`--stats` reports the bytes still held by threads after eviction** (peak), beside
  the shared cache's `peak resident / budget`.
- **Images do not change.** A cache never changes a texel value, so renders at `-s 16`
  are bit-identical before and after.
- **No new environment switch.** The A/B is two binaries through `bench_ab.sh`, as for
  the 8 → 16 set change that preceded this one.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `textures`: a new requirement. Streamed UV tiles held by render threads count
  against `CRUST_TEX_CACHE_MB` and are reported by `--stats`. Today they sit outside
  it, which is documented in the design record but not in the spec.

## Impact

- **`crates/crust-assets/src/tiled/cache.rs`:**
  - the microcache's shape and set index (`MICRO_SETS`, `MICRO_WAYS`, `micro_set`);
  - the optional second level;
  - the startup capacity cap;
  - accounting for evicted-but-held tiles (a flag on the tile and a `Drop` that
    releases its bytes);
  - `make_room`;
  - `CacheStats`.

  `texture_cache.rs` (`Ways`) and the stats plumbing in `crust-assets/src/lib.rs` and
  `crust-core/src/stats.rs` follow. Everything stays safe Rust, with no new dependency.
- **Performance:**
  - **Expected gain, ALab:** a ceiling of about 7–9% of the render, if every shard hit
    disappeared. A plausible 3–4%, extrapolated from the 8 → 16 set step (81.2% →
    84.9% hits, Texture 1.76 → 1.57 µs per `eval`).
  - **Must not regress:** the streamed alias scene
    (`scripts/gen_texture_alias_scene.py`) and the per-lookup instruction count on a
    textured scene.
  - **Unaffected:** preloaded scenes (cornellbox, `materialx_basic`).
- **Memory:** bounded by the budget instead of sitting beside it. Under eviction
  pressure, the shared cache gets less room by exactly the bytes threads still hold.
- **Docs:**
  - `openspec/specs/textures/design.md`, the streaming section's microcache paragraph;
  - `docs/alab_profile.md`, the thread-scaling numbers;
  - the `CRUST_TEX_CACHE_MB` row in `docs/architecture.md`;
  - `site/content/docs/reference/environment-variables.md` (the budget now includes
    thread-held tiles).
- **Out of scope:** Ptex streaming. It has its own four-slot microcache and its own
  reserve (`micro_reserve`), and `stream-ptex-by-default` is changing that path. The
  same contention applies there behind a single reader `Mutex`, and is noted as a
  follow-up.
