//! The shared tile cache: bounded memory, many readers, no blocking.
//!
//! This is the piece that turns "textures are resident" into "textures are
//! streamed". Everything else in the module exists to feed it.
//!
//! **Why not `moka` or `quick_cache`.** Both are good and both carry internal
//! `unsafe`, which `CLAUDE.md` treats as a project decision rather than an
//! optimisation. They are also solving a harder problem than this one, because
//! **OIIO's ImageCache is not an LRU**: its `check_max_mem` runs a clock hand
//! over the shards, gives each entry one second chance, and — the part that
//! matters — `try_lock`s the sweep and *returns immediately* if another thread
//! already holds it, so no render thread ever blocks to make room. That
//! algorithm is a few hundred lines of `std::sync`, needs no dependency, and is
//! what this implements. If measured shard contention ever says otherwise, a
//! dependency is the answer then, with numbers behind it.
//!
//! **Three tiers, cheapest first.** A bilinear tap reads the same tile up to
//! four times running and trilinear doubles that, so the great majority of
//! lookups should never reach a lock at all:
//!
//! 1. a per-thread two-entry microcache ([`with_tile`]), the same
//!    `thread_local!` idiom the MaterialX evaluator already uses for its value
//!    stack;
//! 2. a sharded map, one `Mutex` per shard;
//! 3. a miss — pop a decoder from the file's pool, read, insert.
//!
//! **Nothing here may panic.** `Texture2D::eval`'s contract says so and
//! `panic = "abort"` makes a violation fatal to the process rather than to a
//! worker, so every lock is taken with poison recovery and every failure
//! returns `None` for the caller to turn into a fallback colour.

use super::read::{TileReader, TiledFile};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Shards the tile map is split across.
///
/// OIIO uses 128. 64 is chosen here because crust's per-thread microcache
/// absorbs the repeated taps that make OIIO's shard traffic heavy, so the
/// residual contention is lower; it is a power of two so the shard index is a
/// mask rather than a modulo.
const SHARDS: usize = 64;

/// Default cache budget, matching OIIO's own 1 GB.
pub const DEFAULT_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;

/// Which tile, of which level, of which file.
///
/// The file is an interned index, not a path: a key is compared and hashed on
/// every lookup, and comparing strings there would cost more than the decode it
/// is trying to avoid. Unlike OIIO's `TileID` there is no channel range —
/// crust always wants RGB, so the generality would be a wider key for nothing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileId {
    pub file: u32,
    pub level: u8,
    pub tile: u32,
}

/// A decoded tile: `u8` RGB, clipped to its level's bounds.
///
/// `width` is the tile's *own* width, which for an edge tile is less than the
/// file's tile edge. Indexing by the nominal edge instead shears every texture
/// whose size is not a multiple of it.
#[derive(Debug)]
pub struct Tile {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl Tile {
    fn bytes(&self) -> u64 {
        // The `Vec`'s own header and the `Arc` count are deliberately included:
        // a budget that only counts payload under-reports by ~10% on small
        // tiles, and the whole point of the number is that it bounds RSS.
        self.pixels.len() as u64 + std::mem::size_of::<Tile>() as u64 + 16
    }
}

struct Entry {
    tile: Arc<Tile>,
    /// Second-chance bit. Set on every hit, cleared by the sweep; an entry
    /// found already clear is evicted. This is what makes the policy
    /// scan-resistant without keeping an ordering structure.
    used: bool,
}

/// Counters, read once at the end of a render.
///
/// Three tiers are counted separately because they answer different questions:
/// microcache hits say whether the sampler's access pattern is coherent, shard
/// hits say whether the budget is big enough, and `redundant` — tiles fetched
/// more than once over the render — is the direct measure of thrashing. OIIO
/// reports the same three for the same reason.
#[derive(Debug, Default)]
pub struct CacheStats {
    pub micro_hits: AtomicU64,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    /// Successful tile decodes. Every one is either a tile's first, a
    /// re-read after eviction, or a concurrent double fill; `counters()`
    /// separates the three by subtraction rather than by counting them
    /// independently, because the three events are observed under different
    /// locks and a thread can be the second to mark a tile seen while being
    /// the first to insert it.
    pub decoded: AtomicU64,
    pub raced: AtomicU64,
    pub evictions: AtomicU64,
    pub bytes_read: AtomicU64,
    pub peak_bytes: AtomicU64,
    pub errors: AtomicU64,
}

/// A plain snapshot of [`CacheStats`], for reporting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheCounters {
    pub micro_hits: u64,
    pub hits: u64,
    pub misses: u64,
    pub redundant: u64,
    pub raced: u64,
    pub evictions: u64,
    pub bytes_read: u64,
    pub peak_bytes: u64,
    pub errors: u64,
    pub budget_bytes: u64,
}

impl CacheCounters {
    pub fn is_empty(&self) -> bool {
        self.micro_hits == 0 && self.hits == 0 && self.misses == 0
    }

    /// Fraction of lookups answered without touching the disk.
    pub fn hit_rate(&self) -> f64 {
        let total = self.micro_hits + self.hits + self.misses;
        if total == 0 {
            return 0.0;
        }
        (self.micro_hits + self.hits) as f64 / total as f64
    }
}

/// One file's geometry plus the decoders that read it.
struct FileSlot {
    file: TiledFile,
    /// Idle cursors. Every `tiff` read takes `&mut self`, so a decoder cannot
    /// be shared; a miss takes one, uses it, and puts it back. The pool grows
    /// to the number of threads that ever miss concurrently and no further.
    readers: Mutex<Vec<TileReader>>,
    /// Tiles this file has ever paged in, so a second page-in of the same tile
    /// can be recognised as thrashing rather than as a first read.
    seen: Mutex<std::collections::HashSet<(u8, u32)>>,
}

/// The cache. One per render, shared by every streaming texture in it.
pub struct TileCache {
    shards: Vec<Mutex<HashMap<TileId, Entry>>>,
    files: Mutex<Vec<Arc<FileSlot>>>,
    resident: AtomicU64,
    budget: u64,
    /// Held by whichever thread is currently making room. `try_lock`ed, never
    /// blocked on: a thread that finds a sweep in progress carries on and lets
    /// the other one do the work.
    sweeping: Mutex<usize>,
    pub stats: CacheStats,
}

impl TileCache {
    pub fn new(budget_bytes: u64) -> TileCache {
        TileCache {
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
            files: Mutex::new(Vec::new()),
            resident: AtomicU64::new(0),
            budget: budget_bytes.max(1024 * 1024),
            sweeping: Mutex::new(0),
            stats: CacheStats::default(),
        }
    }

    /// The budget from `CRUST_TEX_CACHE_MB`, or [`DEFAULT_BUDGET_BYTES`].
    pub fn budget_from_env() -> u64 {
        match std::env::var("CRUST_TEX_CACHE_MB") {
            Ok(v) => match v.parse::<u64>() {
                Ok(mb) if mb >= 1 => mb * 1024 * 1024,
                _ => {
                    tracing::warn!(
                        "CRUST_TEX_CACHE_MB={v} is not a positive integer — using {} MiB",
                        DEFAULT_BUDGET_BYTES / (1024 * 1024)
                    );
                    DEFAULT_BUDGET_BYTES
                }
            },
            Err(_) => DEFAULT_BUDGET_BYTES,
        }
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    pub fn resident(&self) -> u64 {
        self.resident.load(Ordering::Relaxed)
    }

    /// Registers a file and returns the index a [`TileId`] names it by.
    pub fn intern(&self, file: TiledFile) -> Option<u32> {
        let mut files = lock(&self.files)?;
        let id = files.len() as u32;
        files.push(Arc::new(FileSlot {
            file,
            readers: Mutex::new(Vec::new()),
            seen: Mutex::new(std::collections::HashSet::new()),
        }));
        Some(id)
    }

    pub fn file(&self, id: u32) -> Option<TiledFile> {
        let files = lock(&self.files)?;
        files.get(id as usize).map(|s| s.file.clone())
    }

    pub fn counters(&self) -> CacheCounters {
        let s = &self.stats;
        // Distinct tiles ever decoded, across every file.
        let distinct = lock(&self.files)
            .map(|files| {
                files
                    .iter()
                    .map(|f| lock(&f.seen).map(|s| s.len() as u64).unwrap_or(0))
                    .sum::<u64>()
            })
            .unwrap_or(0);
        CacheCounters {
            micro_hits: s.micro_hits.load(Ordering::Relaxed),
            hits: s.hits.load(Ordering::Relaxed),
            misses: s.misses.load(Ordering::Relaxed),
            // Exact, and free of the race an independently counted version
            // had: every successful decode is a first read, a re-read after
            // eviction, or a double fill, and the other two are counted
            // unambiguously — `distinct` under the file's own lock, `raced`
            // under the shard lock that decided it.
            redundant: s
                .decoded
                .load(Ordering::Relaxed)
                .saturating_sub(distinct)
                .saturating_sub(s.raced.load(Ordering::Relaxed)),
            raced: s.raced.load(Ordering::Relaxed),
            evictions: s.evictions.load(Ordering::Relaxed),
            bytes_read: s.bytes_read.load(Ordering::Relaxed),
            peak_bytes: s.peak_bytes.load(Ordering::Relaxed),
            errors: s.errors.load(Ordering::Relaxed),
            budget_bytes: self.budget,
        }
    }

    #[inline]
    fn shard(&self, id: &TileId) -> &Mutex<HashMap<TileId, Entry>> {
        // The three fields are mixed rather than hashed: a `TileId` is three
        // small integers and a real hash costs more than the collisions it
        // avoids at this size. Multiplying by odd constants spreads the tile
        // index — which varies fastest — across all the shards.
        let h = (id.file as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add((id.tile as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
            .wrapping_add((id.level as u64).wrapping_mul(0x1656_67B1_9E37_79F9));
        &self.shards[(h >> 32) as usize & (SHARDS - 1)]
    }

    /// The tile, from the shard map or from disk.
    ///
    /// `None` on any failure — a poisoned lock, an unknown file, a decode
    /// error. The caller turns that into the texture's fallback colour; it must
    /// never become a panic.
    pub fn get(&self, id: TileId) -> Option<Arc<Tile>> {
        if let Some(shard) = lock(self.shard(&id)).and_then(|mut m| {
            m.get_mut(&id).map(|e| {
                e.used = true;
                e.tile.clone()
            })
        }) {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            return Some(shard);
        }
        self.stats.misses.fetch_add(1, Ordering::Relaxed);
        self.page_in(id)
    }

    fn page_in(&self, id: TileId) -> Option<Arc<Tile>> {
        let slot = {
            let files = lock(&self.files)?;
            files.get(id.file as usize)?.clone()
        };

        // Which distinct tiles this file has ever yielded. Only the *count*
        // is used, at report time, to separate a re-read after eviction from a
        // double fill — see `CacheStats::decoded`.
        if let Some(mut seen) = lock(&slot.seen) {
            seen.insert((id.level, id.tile));
        }

        let mut dec = match lock(&slot.readers).and_then(|mut p| p.pop()) {
            Some(d) => d,
            None => match slot.file.reader() {
                Ok(d) => d,
                Err(e) => {
                    tracing::debug!("{}: {e}", slot.file.path().display());
                    self.stats.errors.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
            },
        };

        let read = slot
            .file
            .read_tile(&mut dec, id.level as usize, id.tile)
            .map_err(|e| {
                tracing::debug!("{}: {e}", slot.file.path().display());
                self.stats.errors.fetch_add(1, Ordering::Relaxed);
            })
            .ok();

        // The cursor goes back whatever happened — a decode error leaves it
        // usable, and dropping it would quietly shrink the pool under load.
        if let Some(mut pool) = lock(&slot.readers) {
            pool.push(dec);
        }

        let pixels = read?;
        let (width, height) = slot
            .file
            .level(id.level as usize)
            .tile_size(id.tile, slot.file.tile_edge());
        let tile = Arc::new(Tile {
            pixels,
            width,
            height,
        });
        let bytes = tile.bytes();
        self.stats.bytes_read.fetch_add(bytes, Ordering::Relaxed);

        // The shard guard is scoped tightly and dropped before `make_room`.
        // `std::sync::Mutex` is not reentrant and the sweep walks *every*
        // shard, so holding this one across the call deadlocks the thread
        // against itself the first time a texture exceeds the budget — which
        // is to say, on every render the cache is actually for.
        let inserted = match lock(self.shard(&id)) {
            Some(mut m) => m
                .insert(
                    id,
                    Entry {
                        tile: tile.clone(),
                        used: true,
                    },
                )
                .is_none(),
            None => false,
        };
        self.stats.decoded.fetch_add(1, Ordering::Relaxed);
        if inserted {
            let now = self.resident.fetch_add(bytes, Ordering::Relaxed) + bytes;
            self.stats.peak_bytes.fetch_max(now, Ordering::Relaxed);
            if now > self.budget {
                self.make_room();
            }
        } else {
            // The slot was already full, so another thread decoded the same
            // tile while this one was decoding it. Wasted work, but bounded by
            // the thread count and no reason to touch the budget — the two
            // would be indistinguishable if they shared a counter.
            self.stats.raced.fetch_add(1, Ordering::Relaxed);
        }
        Some(tile)
    }

    /// Clock sweep: one pass giving each entry a second chance, dropping the
    /// ones that did not use theirs.
    ///
    /// Returns immediately if another thread is already sweeping. That is the
    /// property worth preserving above all: a render thread that finds the
    /// cache full should keep rendering, not queue behind an eviction. The
    /// budget is a target, not an invariant, and it is briefly exceeded by
    /// however much the other threads insert while one sweeps.
    fn make_room(&self) {
        let Ok(mut hand) = self.sweeping.try_lock() else {
            return;
        };
        // Two passes at most. The first clears `used` on anything touched
        // since the last sweep and evicts the rest; if that was not enough —
        // every entry was hot — the second takes them anyway, because looping
        // until the budget is met against a live working set does not
        // terminate.
        for _ in 0..2 {
            for _ in 0..SHARDS {
                *hand = (*hand + 1) & (SHARDS - 1);
                let Some(mut m) = lock(&self.shards[*hand]) else {
                    continue;
                };
                let mut freed = 0u64;
                m.retain(|_, e| {
                    if e.used {
                        e.used = false;
                        true
                    } else {
                        freed += e.tile.bytes();
                        false
                    }
                });
                if freed > 0 {
                    self.resident.fetch_sub(freed, Ordering::Relaxed);
                    self.stats.evictions.fetch_add(1, Ordering::Relaxed);
                }
            }
            if self.resident.load(Ordering::Relaxed) <= self.budget {
                return;
            }
        }
    }
}

/// Takes a lock, recovering from poisoning rather than propagating it.
///
/// A poisoned mutex means some other thread panicked while holding it. With
/// `panic = "abort"` that cannot actually happen in a release build, but in a
/// test build it can — and `unwrap()`ing here would turn one thread's failure
/// into every other thread's. The data behind these locks is a cache: the worst
/// a torn update can do is a wrong tile, and every write is a whole `Arc`.
fn lock<T>(m: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match m.lock() {
        Ok(g) => Some(g),
        Err(poisoned) => Some(poisoned.into_inner()),
    }
}

/// The per-thread microcache's slots: the two most recent `(key, tile)` pairs.
type MicroSlots = [Option<(TileId, Arc<Tile>)>; 2];

thread_local! {
    /// The two most recently used tiles, per thread.
    ///
    /// The highest-leverage part of the whole cache and the cheapest: a
    /// bilinear tap reads one tile up to four times in a row and trilinear
    /// doubles that, so this absorbs most lookups before any lock is touched.
    /// OIIO keeps exactly two for the same reason and tracks its miss rate
    /// separately; so does [`CacheStats::micro_hits`].
    ///
    /// Two rather than one because trilinear alternates between two levels,
    /// and one entry would thrash on every other tap.
    static MICRO: std::cell::RefCell<MicroSlots> =
        const { std::cell::RefCell::new([None, None]) };
}

/// Reads `id` through the per-thread microcache and hands the tile to `f`.
///
/// Takes a closure rather than returning the `Arc` on purpose. A texel fetch
/// is the hottest thing in a textured render — 8.7 M of them in a 640x360
/// frame at 4 spp — and returning a handle means an atomic refcount increment
/// and decrement on *every one*, even the 98.6% that hit this thread's own two
/// slots and never touch a lock. Measured, that was the difference between a
/// streamed render costing 4x a preloaded one and costing 1.3x: the cache
/// itself was never the bottleneck, the `Arc` traffic was.
///
/// The borrow is held across `f`, so `f` must not look anything else up
/// through the microcache. Every caller reads one texel and returns, which is
/// why this is a closure and not a guard.
pub fn with_tile<R>(cache: &TileCache, id: TileId, f: impl FnOnce(&Tile) -> R) -> Option<R> {
    // `FnOnce` can only be moved once, and it might be called on either path,
    // so it is parked in an `Option` and taken by whichever path wins. The
    // alternative — `Fn` — would forbid callers that move anything in.
    let mut f = Some(f);

    // Fast path: this thread already holds the tile. No lock, no refcount.
    // The index is found first so the closure is called exactly once, which
    // is what lets this stay `FnOnce` and stay safe.
    let hit = MICRO.with(|m| {
        let slots = m.borrow();
        let idx = slots
            .iter()
            .position(|s| matches!(s, Some((k, _)) if *k == id))?;
        let (_, tile) = slots[idx].as_ref()?;
        Some(f.take()?(tile))
    });
    if let Some(r) = hit {
        cache.stats.micro_hits.fetch_add(1, Ordering::Relaxed);
        return Some(r);
    }
    // Miss: the borrow above is released before this, because `cache.get` can
    // decode and must not run under a thread-local borrow.
    let tile = cache.get(id)?;
    let r = f.take()?(&tile);
    MICRO.with(|m| {
        let mut slots = m.borrow_mut();
        // Newest in front; the displaced entry becomes the second slot. Two
        // entries make this a swap rather than a policy.
        slots[1] = slots[0].take();
        slots[0] = Some((id, tile));
    });
    Some(r)
}

/// Empties every thread's microcache.
///
/// Needed only by tests: the entries hold `Arc<Tile>`s from a cache that a test
/// is about to drop, and a stale hit would answer from the wrong cache.
#[cfg(test)]
pub fn clear_microcache() {
    MICRO.with(|m| *m.borrow_mut() = [None, None]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiled::{TILE_EDGE, write_tx};
    use std::path::PathBuf;

    /// Writes a `.tx` whose texel values encode their own coordinates, so a
    /// tile read back can be checked against where it claims to be from.
    fn fixture(name: &str, w: usize, h: usize) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("crust_tilecache_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("t.tx");
        let src: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                [(x % 251) as u8, (y % 251) as u8, ((x * 3 + y) % 251) as u8]
            })
            .collect();
        write_tx(&path, &src, w, h, crust_core::ColorSpace::Raw).expect("write");
        path
    }

    #[test]
    fn a_tile_read_through_the_cache_matches_a_direct_read() {
        clear_microcache();
        let path = fixture("direct", 300, 200);
        let tf = TiledFile::open(&path).expect("open");
        let cache = TileCache::new(64 * 1024 * 1024);
        let id = cache.intern(tf.clone()).expect("intern");

        let mut dec = tf.reader().expect("reader");
        for level in 0..tf.level_count() {
            let li = tf.level(level);
            for tile in 0..(li.across * li.down) as u32 {
                let direct = tf.read_tile(&mut dec, level, tile).expect("direct");
                let via = cache
                    .get(TileId {
                        file: id,
                        level: level as u8,
                        tile,
                    })
                    .expect("cached");
                assert_eq!(via.pixels, direct, "level {level} tile {tile}");
                let (tw, th) = li.tile_size(tile, TILE_EDGE);
                assert_eq!((via.width, via.height), (tw, th));
            }
        }
        // Everything was a miss the first time and a hit the second.
        let before = cache.counters();
        let _ = cache.get(TileId {
            file: id,
            level: 0,
            tile: 0,
        });
        assert_eq!(cache.counters().hits, before.hits + 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The budget is a bound on residency, not a suggestion.
    ///
    /// Sized so the working set is several times the budget, which forces the
    /// sweep to run repeatedly while reads continue. The assertion allows
    /// overshoot — the sweep is deliberately non-blocking, so other threads
    /// keep inserting while one makes room — but it must not grow without
    /// limit, which is what an eviction bug looks like.
    #[test]
    fn residency_stays_near_the_budget_under_pressure() {
        clear_microcache();
        let path = fixture("budget", 1536, 1536);
        let tf = TiledFile::open(&path).expect("open");
        let tile_bytes = (TILE_EDGE * TILE_EDGE * 3) as u64;
        // The floor `TileCache::new` clamps to, so the working set below is
        // ~7 MiB against it — several sweeps' worth.
        let budget = 1024 * 1024;
        let cache = TileCache::new(budget);
        let id = cache.intern(tf.clone()).expect("intern");

        let l0 = tf.level(0);
        let tiles = (l0.across * l0.down) as u32;
        assert!(
            tiles as u64 * tile_bytes > 4 * budget,
            "the fixture must exceed the budget several times over"
        );
        for tile in 0..tiles {
            assert!(
                cache
                    .get(TileId {
                        file: id,
                        level: 0,
                        tile
                    })
                    .is_some()
            );
        }
        let c = cache.counters();
        assert!(c.evictions > 0, "nothing was ever evicted");
        assert!(
            cache.resident() <= budget * 2,
            "resident {} against a {budget}-byte budget",
            cache.resident()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Many threads, one cache, overlapping keys — the shape a render has.
    ///
    /// Checks the two things that matter: no deadlock (the sweep is
    /// `try_lock`ed from every thread at once), and every tile that comes back
    /// is the right tile. A torn read or a shard-index bug shows up as the
    /// wrong pixels, not as a crash, so the values are verified rather than
    /// just the count.
    #[test]
    fn concurrent_readers_get_correct_tiles_without_deadlocking() {
        clear_microcache();
        let path = fixture("threads", 512, 512);
        let tf = TiledFile::open(&path).expect("open");
        let cache = Arc::new(TileCache::new(512 * 1024));
        let id = cache.intern(tf.clone()).expect("intern");

        // The expected contents, computed once, single-threaded.
        let mut dec = tf.reader().expect("reader");
        let l0 = tf.level(0);
        let tiles = (l0.across * l0.down) as u32;
        let want: Vec<Vec<u8>> = (0..tiles)
            .map(|t| tf.read_tile(&mut dec, 0, t).expect("direct"))
            .collect();
        let want = Arc::new(want);

        let threads: Vec<_> = (0..8)
            .map(|k| {
                let cache = cache.clone();
                let want = want.clone();
                std::thread::spawn(move || {
                    clear_microcache();
                    for round in 0..40u32 {
                        // Overlapping but offset sweeps, so threads collide on
                        // some keys and race on others.
                        let t = (round * 7 + k * 3) % tiles;
                        let got = cache
                            .get(TileId {
                                file: id,
                                level: 0,
                                tile: t,
                            })
                            .expect("tile");
                        assert_eq!(got.pixels, want[t as usize], "tile {t} on thread {k}");
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("worker panicked");
        }
        assert_eq!(cache.counters().errors, 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The microcache answers repeats without reaching a shard, and still
    /// answers correctly when the two slots alternate — which is what
    /// trilinear filtering does across two levels.
    #[test]
    fn the_microcache_absorbs_repeats_and_alternation() {
        clear_microcache();
        let path = fixture("micro", 256, 256);
        let tf = TiledFile::open(&path).expect("open");
        let cache = TileCache::new(64 * 1024 * 1024);
        let id = cache.intern(tf).expect("intern");

        let a = TileId {
            file: id,
            level: 0,
            tile: 0,
        };
        let b = TileId {
            file: id,
            level: 1,
            tile: 0,
        };
        let first = with_tile(&cache, a, |t| t.pixels.clone()).expect("a");
        let second = with_tile(&cache, b, |t| t.pixels.clone()).expect("b");
        let base = cache.counters();

        // Four alternating taps — the trilinear pattern — must all be
        // microcache hits, because two slots hold both levels at once.
        for _ in 0..2 {
            assert_eq!(
                with_tile(&cache, a, |t| t.pixels.clone()).expect("a"),
                first
            );
            assert_eq!(
                with_tile(&cache, b, |t| t.pixels.clone()).expect("b"),
                second
            );
        }
        let now = cache.counters();
        assert_eq!(now.micro_hits, base.micro_hits + 4);
        assert_eq!(now.hits, base.hits, "no lookup should have reached a shard");
        assert_eq!(now.misses, base.misses);

        clear_microcache();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Nothing about a broken file may panic — `Texture2D::eval` forbids it and
    /// `panic = "abort"` makes it fatal to the process.
    #[test]
    fn failures_return_none_rather_than_panicking() {
        clear_microcache();
        let path = fixture("broken", 128, 128);
        let tf = TiledFile::open(&path).expect("open");
        let cache = TileCache::new(1024 * 1024);
        let id = cache.intern(tf).expect("intern");

        // A tile index past the end of the level.
        assert!(
            cache
                .get(TileId {
                    file: id,
                    level: 0,
                    tile: 9999
                })
                .is_none()
        );
        // A file index that was never interned.
        assert!(
            cache
                .get(TileId {
                    file: 77,
                    level: 0,
                    tile: 0
                })
                .is_none()
        );
        // A level past the pyramid clamps rather than failing.
        assert!(
            cache
                .get(TileId {
                    file: id,
                    level: 200,
                    tile: 0
                })
                .is_some()
        );
        assert!(cache.counters().errors > 0, "the failures were counted");

        // And the file disappearing mid-render is survivable: the pooled
        // reader still holds a handle, so this exercises a *fresh* open.
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let missing = TiledFile::open(&path);
        assert!(missing.is_err());
    }
}
