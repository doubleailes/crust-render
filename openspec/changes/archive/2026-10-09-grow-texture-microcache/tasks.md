## 1. Baseline

- [x] 1.1 Build the parent commit's release binary and copy it out of `target/` as
      `bin_before`. Every A/B binary gets its own `CARGO_TARGET_DIR`: worktrees sharing
      one silently run each other's builds. Record goldens with
      `scripts/check_images.sh record <dir>`.
- [x] 1.2 Generate the streamed alias scene:
      `scripts/gen_texture_alias_scene.py <dir> --udim=8 --size=4096`, then render
      once with `--auto-tx` to write its `.tx` files. With `bin_before`, record:
      - its `--stats` texture block;
      - a callgrind instruction count at `RAYON_NUM_THREADS=1`, `-s 2`.
- [x] 1.3 With `bin_before`, record ALab frame 1004 at 32 spp and 72 threads
      (`--stats`): microcache hits, shard hits, texture `eval`s, Render time. The
      reference values are in `docs/alab_profile.md` § Thread scaling. Re-measure only
      if the parent is not `3c8b861`.

## 2. Spike: which misses dominate (design D1–D3)

- [x] 2.1 In a throwaway worktree with its own target directory, build variants B–E
      from design D1 by changing `MICRO_SETS`, `MICRO_WAYS` and `micro_set` (D: hash
      the whole `TileId`, as `shard()` does). Copy each binary out as `crust_<variant>`.
- [x] 2.2 Build variant F at prototype quality: today's first level plus a 192-tile
      second level. An evicted tile moves into it, and a second-level hit moves it back
      with the first level's evictee taking its place (D3).
- [x] 2.3 Run every variant once on ALab frame 1004 (32 spp, 72 threads, `--stats`),
      one at a time. Tabulate microcache hit %, shard hits per `eval`, tiles per thread
      and Render time.
- [x] 2.4 Apply the gate's first test: shard hits per `eval` at least 25% below A.
      Choose the passing variant with the fewest shard hits per `eval`, breaking ties
      by less memory per thread. If none passes, write the table into the textures
      design record and `docs/alab_profile.md`, and stop the change at the measurement.
- [x] 2.5 For the chosen variant, run the rest of the gate against `bin_before`:
      - `scripts/bench_ab.sh -n 3` on ALab: Render faster at both min and mean;
      - `bench_ab.sh` on the alias scene: no slower;
      - callgrind on the alias scene: under 1% more instructions;
      - the profiled 72/8 pair on ALab: Texture per call against the machine's 1.6×.

      If any test fails, stop as in 2.4.

## 3. The chosen shape

- [x] 3.1 Implement the chosen shape in `crates/crust-assets/src/tiled/cache.rs`: the
      sets, ways and set index, and the second level if F won. Rewrite the `MICRO` and
      `micro_set` doc comments, including the "outside the budget" paragraph that
      section 4 retires.
- [x] 3.2 Unit tests for the chosen shape:
      - every variant: `the_microcache_absorbs_repeats_and_alternation` still passes,
        or is updated with the new numbers;
      - D or E: a trilinear lookup at a tile corner (up to 8 tiles of one texture) hits
        on its second pass;
      - F: a tile moving between the two levels leaves `Arc::strong_count` unchanged.

## 4. Thread-held tiles in the budget (design D4–D5)

- [x] 4.1 Add a held-bytes counter to `TileCache`, and a mark on `Tile` (a `OnceLock`
      holding a handle to that counter). `make_room` marks an entry and adds its bytes
      before the shard drops its `Arc`. `impl Drop for Tile` subtracts a marked tile's
      bytes.
- [x] 4.2 Change `make_room`'s target, and the insert-time check that triggers it, from
      `resident > budget` to `resident + held > budget`.
- [x] 4.3 Cap what each thread retains (design D5): a thread-local count of retained
      bytes, and a share of `(budget / 2) / micro_threads()` per cache. An insert on the
      miss path that would take the count over the inserting cache's share is skipped:
      the tile is used for this lookup and dropped. The hit path is unchanged.
- [x] 4.4 Unit tests:
      - evicting a tile no thread holds leaves `held` at 0 after the sweep;
      - evicting a tile this thread's microcache holds sets `held` to its bytes, and
        `clear_microcache` returns it to 0;
      - two caches in one process count separately;
      - a budget whose share is below one tile retains nothing, and lookups still
        return the right texels; a small cache's share does not limit another cache's;
      - a debug assertion that `held` never wraps below zero.
- [x] 4.5 Report the peak held bytes: add them to `CacheStats` and `counters()`, plumb
      them through `crust-assets/src/lib.rs` into `crust-core/src/stats.rs`, and add a
      `--stats` line beside `peak resident / budget`.

## 5. Verification

- [x] 5.1 `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`.
- [x] 5.2 `scripts/check_images.sh check <dir>` against 1.1: bit-identical.
- [x] 5.3 The spec's scenarios:
      - ALab at the default budget: nothing evicted, and no held bytes reported;
      - the alias scene with `CRUST_TEX_CACHE_MB` below its working set: shared resident
        plus held bytes within the budget, both reported;
      - the alias scene at `-s 16`, with the default budget and with one too small for
        any per-thread tile: bit-identical EXRs (`exr_diff`).
- [x] 5.4 Final measurements against `bin_before`:
      - `bench_ab.sh` on ALab and on the alias scene;
      - callgrind on the alias scene at one thread;
      - the profiled 72/8 pair on ALab.

## 6. Documentation

- [x] 6.1 `openspec/specs/textures/design.md`, streaming section: record
      - the microcache's new shape and why (the spike's table);
      - the held-bytes accounting, replacing "outside the budget";
      - the gate's measurements.
- [x] 6.2 `docs/alab_profile.md`: the thread-scaling per-call table, the "Where the
      72-thread render goes" breakdown, and "What is left" item 2, with the new numbers.
- [x] 6.3 Update `CRUST_TEX_CACHE_MB`'s meaning in the `docs/architecture.md`
      environment-switches row and in
      `site/content/docs/reference/environment-variables.md`. Run `zola build` in
      `site/`.
- [x] 6.4 Add to the textures design record's known gaps: the Ptex stream's microcache
      has the same contention behind one reader `Mutex`, and is a follow-up.
