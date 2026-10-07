# Design

## Context

See proposal.md (Why) for the failure itself. The current shape of the code:

- **Opens at bind time do not leak.** `TiledFile::open` reads the header and the
  level geometry, then drops its handle. Both `TiffFile::open` and `ExrFile::open`
  build a decoder and let it go out of scope.
- **The pools are the problem.** Each `FileSlot` in `TileCache` has a
  `readers: Mutex<Vec<TileReader>>`. `page_in` takes a reader, or opens a fresh one
  when the pool is empty, reads one tile, and always pushes the reader back
  (`cache.rs:531-556`). Each pooled reader is one open descriptor
  (`BufReader<File>` or a `tiff::Decoder<BufReader<File>>`). Nothing ever shrinks a
  pool.
- **Failures are quiet.** A failed open or read becomes `tracing::debug!` plus
  `stats.errors += 1`, and the lookup returns `None`, which is the fallback colour.
  The cache's no-panic contract stays: nothing here may abort a worker.
- **The cache lives until the end.** `FileAssets` owns it, and `main.rs` keeps
  `assets` alive through `write_product` / `write_png`.
- **Cache philosophy** (header of `cache.rs`): no render thread ever blocks to make
  room. The clock sweep `try_lock`s and walks away. The descriptor cap must follow
  the same rule.

## Goals / Non-Goals

**Goals:**

- Peak descriptors held by the `.tx` cache ≤ `cap + threads`, independent of how many
  files the scene binds.
- No image change at any cap (scheduling only, like tiles vs scanlines).
- No measurable cost when a scene touches fewer files than the cap.
- Make a failed tile read visible at WARN, bounded per file and per render.

**Non-Goals:**

- Raising `RLIMIT_NOFILE`. That needs `libc` or `rustix` (`setrlimit`), a new
  dependency carrying `unsafe`, which CLAUDE.md makes a project decision. The cap
  makes the soft limit irrelevant to the cache instead.
- Ptex streaming descriptors. `ptex::SharedReader` holds one `File` per streamed
  `.ptx`, so it scales with files, not files × threads. Bounding it belongs upstream
  in `ptex-rs`, next to its cache: the design record already says not to grow a
  second Ptex cache here. It is recorded as a gap.
- Single-flight on a miss, sharing the cache between renders, or anything else in
  "Known gaps: texture residency" that this change does not need.

## Decisions

### 1. One global idle-reader pool, ordered by last use

The per-file `readers` vectors are replaced by one `ReaderPool` on `TileCache`, behind
a single `Mutex`. It holds:

- `idle: HashMap<u32, Vec<TileReader>>`: idle readers, keyed by file id;
- an LRU order over the files that have idle readers: a monotonic stamp per file, and
  a `BTreeMap<stamp, file>` to find the oldest one;
- `open: usize`: every reader that exists, idle or checked out.

The mutex is held only to pop, push or pick a victim. It is never held across `open`,
a tile read or a `close`. A victim is moved out under the lock and dropped after
it is released.

*Alternative: keep the per-file pools and add a global atomic count.* The count is
easy. Eviction is not: when the cap is hit, the cache must find an idle reader on a
*cold* file, which needs a global order anyway. Without one, the cheap policy
("over the cap → don't pool the returning reader") lets dead files keep their
descriptors forever, while every *live* file pays a reopen per miss. Rejected.

*Contention:* the lock is taken twice per **miss**, never per hit or per texel. A miss
already costs a seek, a decompress and a shard insert (tens of µs), against ~100 ns
under this lock. This is checked on ALab's miss rate with `--profile` (TextureLoad)
rather than assumed (tasks 4.x).

### 2. The cap never blocks; it bounds idle readers

On a miss:

1. Pop an idle reader for this file if one exists.
2. Otherwise, if `open >= cap`, evict the oldest idle reader of *another* file.
3. Open a fresh reader either way. `open += 1`.

If there was nothing idle to evict (every reader is checked out by another thread),
the open still happens. So `open` can exceed the cap by at most the number of threads
inside `page_in` at once, which is the thread count. On return, if `open > cap`, the
reader is closed instead of pooled (`open -= 1`). That brings the cache back under the
cap as soon as the burst ends.

*Alternative: wait on a condvar for a free reader.* That gives a strict bound, but a
render thread would block, which the cache's header rules out. With 1024 descriptors
and a 256 default, `cap + threads` already has the headroom. Rejected.

### 3. `CRUST_TEX_MAX_OPEN_FILES`, default 256, `0` = unbounded

- A new `Config` field, `tex_max_open_files: usize`, read in `config.rs` with the
  existing parser helpers.
- `0` keeps every reader, which is today's behaviour, so the A/B is honest.
- Why 256: it leaves `1024 − 256 − threads` for Ptex streaming, output, logs and the
  USD stage on the default soft limit (≈ 640 at 128 threads). It is above OIIO's
  `max_open_files` default of 100 because crust has no per-file handle sharing:
  one hot file can hold several readers.
- The ALab measurement (task 4.3) may move the number. The spec names the switch,
  not the value, apart from stating the default.

### 4. Retry once on descriptor exhaustion, only on `EMFILE` / `ENFILE`

If an open fails with `raw_os_error()` 24 (`EMFILE`) or 23 (`ENFILE`):

1. Drain *all* idle readers, dropped outside the lock.
2. Retry the open once.

The codes are the same on Linux and macOS. The check sits behind `cfg(unix)`, and
other targets don't retry. Other errors are not retried. A missing or corrupt file
would otherwise empty every pool on each of its misses, and the existing
"file disappearing mid-render is survivable" test must keep its meaning.

### 5. WARN once per failing file, once per render

- `FileSlot` gains `warned: AtomicBool`. The first failed open or read on a file
  emits a WARN naming the path and the error. Later failures stay at DEBUG.
- `main.rs` reads `stats.textures.errors`, already snapshotted after the render.
  If it is non-zero, it logs one WARN: "N tile reads failed; those lookups used
  their fallback". This is printed whether or not `--stats` is on.

That fits the logging rule: WARN means something authored was skipped, the per-file
line is bounded by the number of failing files, and nothing is per tile.

### 6. Release readers before writing outputs

- `TileCache::release_readers()` drains the pool.
- `FileAssets` exposes it as `release_texture_files()`.
- `main.rs` calls it right after the stats snapshot, before `write_product`.

A reader opened afterwards (none are expected) would simply reopen. This also covers
the `cap = 0` A/B leg, so the "off" side can no longer fail the write for this reason.

### 7. Counters

`CacheStats` gains:

- `opens`: every reader opened;
- `peak_open`: a `fetch_max` on `open`;
- `reopens`: `opens − files with ≥1 open`, derived at report time the way
  `redundant` already is.

`CacheCounters` / `crust_core::stats` print them in the textures block, with the cap.

## Risks / Trade-offs

- [A cap below a scene's live working set turns misses into reopens. A TIFF reopen
  re-parses IFD0 and walks the IFD chain to the level.] → `reopens` makes it visible
  in `--stats`. Measure on ALab against `cap=0` with `bench_ab.sh`. Raise the default
  if the cost shows.
- [`open` exceeds the cap by up to the thread count] → it is documented as
  `cap + threads` in the spec, and the default leaves room for it.
- [The global lock becomes a hot spot on miss-heavy renders] → it is held only for
  O(log n) map operations, never across I/O. If `--profile` shows it, shard the pool
  by file id. The LRU then becomes per-shard, which is still a bound.
- [Bit-identity] → readers are interchangeable cursors, and tile bytes do not depend
  on which reader decoded them. A test renders a multi-`.tx` scene at `cap=1` vs `0`
  and compares bitwise, and a cache-level test hammers `cap=1` from many threads.
- [WARN noise on a genuinely broken asset library] → it is bounded by failing files
  and points at real lost texels, which is the intent.

## Migration Plan

No migration: the change is additive behind a switch whose "off" value is the old
behaviour. Rollback is `CRUST_TEX_MAX_OPEN_FILES=0`. The release (task 6) still
applies there.
