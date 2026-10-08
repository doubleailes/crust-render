# Tasks

## 1. Baseline

- [x] 1.1 Build the parent commit's release binary (`bin_before`). Record goldens
      with `scripts/check_images.sh record <dir>` using `--indirect-clamp 0`. Verify:
      the goldens exist for every scene the script covers.
- [x] 1.2 Add a test in `tiled/cache.rs` that reads tiles from N > 8 `.tx` files from
      many threads, and counts open readers through a test hook (`open_readers()`).
      Verify: it shows the pool growing to (files × concurrent misses) on today's code.
      This is the regression the cap fixes.

## 2. Config switch

- [x] 2.1 Add `tex_max_open_files: usize` to `crust_core::Config`, read from
      `CRUST_TEX_MAX_OPEN_FILES`. Default 256, `0` = unbounded. Verify: a `config.rs`
      unit test covers unset, `0`, a number and a malformed value (the malformed one
      falls back to the default, with the same warning as the other switches).
- [x] 2.2 Document the switch:
      - a row in `docs/architecture.md` § Environment switches;
      - a table row and a `### CRUST_TEX_MAX_OPEN_FILES` section in
        `site/content/docs/reference/environment-variables.md`.

      Verify: `zola build` (0.21) in `site/` passes its link and anchor check.

## 3. Bounded reader pool in the tile cache

- [x] 3.1 Replace the per-`FileSlot` `readers` vectors with the global `ReaderPool`
      from design §1. It holds the idle map, the LRU stamps and the `open` count, and
      `TileCache::new` takes a cap. Thread the cap from `Config` through `FileAssets`.
      Verify: `cargo test -p crust-assets` passes unchanged.
- [x] 3.2 Implement the miss path from design §2:
      - pop this file's idle reader, else evict the oldest other file's idle reader
        when at the cap, then open;
      - on return, close instead of pooling while `open > cap`;
      - drop victims outside the lock.

      Verify: the 1.2 test, at `cap = 4`, shows open readers never above
      `4 + threads` and idle readers never above 4 after the run.
- [x] 3.3 Add the `EMFILE` / `ENFILE` retry (design §4, `cfg(unix)`). Verify: a unit
      test injects the error through a test-only open hook, and the read succeeds after
      the idle pool is drained. The existing "file disappearing mid-render" test still
      passes, with no pool drain on `NotFound`.
- [x] 3.4 Add `release_readers()` on `TileCache` and `release_texture_files()` on
      `FileAssets`. Verify: a unit test shows zero open readers after the call, and a
      later `get` still reads the tile by reopening.
- [x] 3.5 Add a bit-identity test: the same tile sequence read through `cap = 1` and
      `cap = 0` caches, from many threads, yields byte-identical tiles. Verify:
      `cargo test -p crust-assets` passes, and so does
      `scripts/test_simd_matrix.sh -p crust-assets`.
- [x] 3.6 Update `openspec/specs/textures/design.md` § Streaming textures with the
      pool, the cap, the never-block rule and why per-file pools were replaced. Verify:
      the record names the switch and the `cap + threads` bound.

## 4. Reporting

- [x] 4.1 WARN once per failing file (a `warned: AtomicBool` on `FileSlot`). Later
      failures stay at DEBUG. Verify: a test with a `.tx` deleted after binding sees
      exactly one WARN for that path across many failed reads (`tracing-test` or the
      repo's existing log-capture helper).
- [x] 4.2 Add `opens`, `peak_open` and the derived `reopens` to
      `CacheStats` / `CacheCounters` / `crust_core::TextureCacheStats`, printed in the
      `--stats` textures block with the cap. Verify: a `stats.rs` formatting test shows
      the lines; `cargo run --release -- render -i <a .tx scene> --stats` prints them.
- [x] 4.3 In `main.rs`, after the stats snapshot:
      - log one WARN when `stats.textures.errors > 0`;
      - call `assets.release_texture_files()` before `write_product`.

      Verify: on a scene with one unreadable `.tx`, the render ends with one summary
      WARN and the outputs are written.
- [x] 4.4 Update `site/` (the `--stats` description, if it lists the textures block)
      and the `cli` design record § Logging if it enumerates WARN sources. Verify:
      `zola build` passes.

## 5. Integration and measurement

- [x] 5.1 Run `scripts/check_images.sh check <dir>` against the 1.1 goldens with the
      new binary, at the default cap and at `CRUST_TEX_MAX_OPEN_FILES=1`. Verify:
      zero differences.
- [x] 5.2 Run callgrind on `samples/cornellbox.usda` and on a `.tx`-textured sample,
      `bin_before` vs new. Verify: a zero or negligible instruction delta on scenes
      under the cap.
- [x] 5.3 Render ALab frame 1004 with `--stats` and `ulimit -n 1024`, at the default
      cap and at `0`:
      - record peak open, reopens and tile read errors;
      - record the `--profile` TextureLoad share for each;
      - compare the timing of the two with `bench_ab.sh`.

      Verify: the default-cap run writes its EXR with 0 tile read errors. Adjust the
      default if reopens cost more than the noise floor, and record the numbers in
      `openspec/specs/textures/design.md`.
- [x] 5.4 Render the Moana island (`usd/island.usda`, `--camera /island/cam/shotCam`,
      the command in `docs/moana_profile.md`) with `--stats` and `ulimit -n 1024`,
      `bin_before` against the new binary. The island binds only Ptex, so no `.tx`
      streams and the cap must never engage. Sample the peak descriptor count from
      `/proc/<pid>/fd` once a second, in two runs:
      - **Control:** the default Ptex residency. Verify: images are bit-identical at
        `-s 16`, the textures block reports 0 opens, and `bench_ab.sh` shows no timing
        change beyond the noise floor (min and mean).
      - **The Ptex gap:** `CRUST_PTEX_STREAM=1 CRUST_PTEX_STREAM_MIPSPACE=file
        CRUST_PTEX_STREAM_MIN_MB=0`, which streams all 3,618 files. Verify: the peak
        descriptor count is recorded, and whether the render and its output write
        survive the 1024 limit. This is the figure 5.5 cites.
- [x] 5.5 Add the Ptex descriptor gap to `openspec/specs/textures/design.md` § Known
      gaps: texture residency: one descriptor per streamed `.ptx`, bounded by files and
      not by threads, owned upstream by `ptex-rs`. Include the island figure from 5.4.
      If the 5.4 run failed on descriptors, also record it in `docs/moana_profile.md`,
      and note it on `stream-ptex-by-default`, which makes streaming the default. Verify:
      the gap names the switch it does *not* cover and carries the measured peak.
- [x] 5.6 Run the CI set: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast` and `cargo deny --locked check`.
      Verify: all are green.

## Workflow follow-up

- Sync the delta into `openspec/specs/textures/spec.md` and archive the change once
  merged.
