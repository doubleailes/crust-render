# Proposal

## Why

The `.tx` tile cache never closes a file during a render. Every streamed file keeps a
pool of open readers, one per thread that ever missed on it at the same time. The pools
are freed only when the cache drops, after the output is written. So the number of open
descriptors grows as roughly (files touched) × (threads that missed together), with no
bound. ALab binds 6,832 `.tx`-ready textures, so it goes past the usual 1024 soft limit.

That limit was hit on frame 1004 (2026-10-07). The render ran for 22 minutes and then
failed to write its EXR: `Error writing image: Too many open files (os error 24)`.
Worse, the same limit is reached *during* the render. A tile read whose open fails
logs at DEBUG and returns the texture's fallback colour. So the frame could lose
textures without a single WARN, and the only sign is a `tile read errors` count
under `--stats`.

## What Changes

- **The tile cache holds a bounded number of open files.** A new switch,
  `CRUST_TEX_MAX_OPEN_FILES`, caps the idle readers the cache keeps across all
  files. The default is 256.
  - When a new open would exceed the cap, the cache first closes idle readers of
    the least-recently-used files.
  - A render thread never blocks on the cap. A reader opened while every pooled one
    is checked out is closed when it is returned, instead of being pooled. So the
    peak is at most the cap plus the thread count.
  - `0` restores today's unbounded pools. That is the "off" side of the A/B.
- **A failed open is retried once after the idle pools are emptied.** This happens
  when the open fails with `EMFILE` / `ENFILE`, so descriptors held by the cache are
  given back before a texel is lost.
- **A tile read that still fails is never silent.**
  - A file whose tiles cannot be read is reported once, at WARN, with the OS error.
  - At the end of the render, if any tile reads failed, one WARN gives the count
    and says those lookups used their fallback colour.
- **Pooled readers are released when the render finishes**, before any output is
  written. A long render's final write never competes with texture handles, whatever
  the cap.
- **`--stats`** reports peak open texture files, the cap, and reopens (opens beyond
  the first per file). That makes the cost of a tight cap measurable.
- Images are unchanged: the cap decides which file handles stay open, never which
  tiles are read or what they contain. A bit-identity test pins this.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `textures`: new requirement. Streaming holds a bounded number of open files, and it
  never fails a tile read silently. This sits beside "Streaming is an A/B of
  preloading", whose text is unchanged.

## Impact

- **`crates/crust-assets/src/tiled/cache.rs`**:
  - a global open-reader count and an LRU of files with idle readers on `TileCache`;
  - the cap check, the eviction and the retry in `page_in`;
  - a `release_readers()`;
  - new counters in `CacheStats` / `CacheCounters`.
- **`crates/crust-assets/src/lib.rs`**: passes the cap from `Config` into the cache,
  and exposes the release to the host.
- **`crates/crust-core/src/config.rs`**: `tex_max_open_files: usize`, read from
  `CRUST_TEX_MAX_OPEN_FILES`.
- **`crates/crust-core/src/stats.rs`**: the new `--stats` lines.
- **`crates/crust-render/src/main.rs`**: releases readers before writing products,
  and the end-of-render WARN.
- **Docs** in the same change:
  - `docs/architecture.md` § Environment switches;
  - `site/content/docs/reference/environment-variables.md`;
  - `openspec/specs/textures/design.md` (§ Streaming textures and § Known gaps:
    texture residency).
- **No new dependency and no `unsafe`.** Raising `RLIMIT_NOFILE` at startup would need
  `libc` / `rustix`, which is a project decision. It is left out (see design.md).
- **Not covered:** Ptex streaming (`ptex-rs` `SharedReader`) holds one descriptor per
  streamed `.ptx` for the render. That is bounded by the file count rather than by
  files × threads, and it stays a documented gap. The Moana island measures it: its
  3,618 `.ptx`, all streamed, would need about 3,600 descriptors.
- **Performance:** none at the default on scenes under the cap; every checked-in
  sample is. Two production scenes are benchmarked:
  - **ALab** (6,832 `.tx`) is the case the cap exists for. Its reopens are measured
    with `bench_ab.sh` and callgrind, and the default is tuned from those numbers.
  - **The Moana island** (Ptex only) is the control: the cap never engages there, so
    its image and timing must not move.
