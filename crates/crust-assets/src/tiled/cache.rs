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
//! 1. a per-thread set-associative microcache ([`with_tile`]), the same
//!    `thread_local!` idiom the MaterialX evaluator already uses for its value
//!    stack;
//! 2. a sharded map, one `Mutex` per shard;
//! 3. a miss — take a reader from the pool, read, insert.
//!
//! **Open files are bounded too.** A miss needs an open reader, and readers are
//! pooled rather than reopened per miss. The pool is one for the whole cache,
//! capped by `CRUST_TEX_MAX_OPEN_FILES` and ordered by last use, so a render
//! that touches thousands of files holds at most the cap plus one reader per
//! thread — see [`ReaderPool`].
//!
//! **Nothing here may panic.** `Texture2D::eval`'s contract says so and
//! `panic = "abort"` makes a violation fatal to the process rather than to a
//! worker, so every lock is taken with poison recovery and every failure
//! returns `None` for the caller to turn into a fallback colour.

use super::{TileReader, TiledFile};
use crate::texture_cache::{Ways, mib_to_bytes};
use half::f16;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Shards the tile map is split across.
///
/// OIIO uses 128. 64 is chosen here because crust's per-thread microcache
/// absorbs the repeated taps that make OIIO's shard traffic heavy, so the
/// residual contention is lower; it is a power of two so the shard index is a
/// mask rather than a modulo.
const SHARDS: usize = 64;

/// Counter stripes: one cache line each, so each thread increments its own.
/// A power of two above any core count crust has run on (72), so two threads
/// share a stripe only past 128.
const STRIPES: usize = 128;

/// One stripe, alone on its cache line. 128 rather than 64 bytes because the
/// adjacent-line prefetcher on Intel parts pulls lines in pairs, so two stripes
/// on neighbouring 64-byte lines would still bounce together.
#[repr(align(128))]
#[derive(Debug, Default)]
struct Stripe(AtomicU64);

/// A counter bumped from every render thread on the texel path.
///
/// **Why it is striped.** Profiling ALab (`docs/alab_profile.md`) found one
/// plain `AtomicU64` here costing more than the lookups it counted: at 3.3 G
/// lookups a frame on 72 threads, every increment moved the same cache line
/// between cores, and 72 threads rendered only ~1.25x faster than 8. Each
/// thread now increments its own line, and the reader sums them. The count is
/// still exact; only reading it costs more, and that happens once per report.
#[derive(Debug)]
pub struct StripedCounter {
    stripes: Box<[Stripe]>,
}

impl Default for StripedCounter {
    fn default() -> Self {
        StripedCounter {
            stripes: (0..STRIPES).map(|_| Stripe::default()).collect(),
        }
    }
}

impl StripedCounter {
    #[inline]
    pub fn add(&self, n: u64) {
        self.stripes[thread_stripe()]
            .0
            .fetch_add(n, Ordering::Relaxed);
    }

    /// Takes `n` back off this thread's stripe. Sound for a quantity each
    /// thread only ever takes back what it added itself — a thread-local
    /// cache's bytes — so no stripe goes below zero; `load` is then the
    /// current total.
    #[inline]
    pub fn sub(&self, n: u64) {
        self.stripes[thread_stripe()]
            .0
            .fetch_sub(n, Ordering::Relaxed);
    }

    /// The exact total. Not a snapshot while threads are still counting, like
    /// any relaxed counter, but reports read it after the render.
    pub fn load(&self) -> u64 {
        self.stripes
            .iter()
            .map(|s| s.0.load(Ordering::Relaxed))
            .sum()
    }
}

/// This thread's stripe, assigned round-robin on first use.
#[inline]
fn thread_stripe() -> usize {
    thread_local! {
        static STRIPE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    }
    STRIPE.with(|s| {
        if let Some(v) = s.get() {
            return v;
        }
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let v = NEXT.fetch_add(1, Ordering::Relaxed) % STRIPES;
        s.set(Some(v));
        v
    })
}

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

/// How a tile's bytes are to be read.
///
/// **The payload is bytes either way, deliberately.** An `enum` holding
/// `Vec<u8>` or `Vec<f16>` is the obvious modelling, and it puts a tag check on
/// the hottest path in a textured render — where it cost 25 instructions a
/// texel and ~10% of wall clock on an 8-bit render that gained nothing from HDR
/// existing (measured; see `StreamingTexture::texel`). Which kind a tile holds
/// is a property of its *file*, so the sampler already knows it before it asks,
/// and the flag here is what that knowledge is checked against rather than what
/// it is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileKind {
    /// Three `u8` a texel, in the file's own encoding; the sampler decodes
    /// through its colour-space table.
    U8,
    /// Three little-endian `f16` a texel, already **linear**; no decode.
    Half,
}

/// A tile's texels: interleaved RGB, as bytes, plus how to read them.
///
/// The split is by the source's *sample type*, not its container — 8- and
/// 16-bit TIFF samples are `U8`, float samples from either backing are `Half`.
/// So an 8-bit texture pays nothing for HDR existing, and an HDR one is not
/// silently clipped to fit an 8-bit cache. `half` rather than `f32` because it
/// is what every streaming texture format stores and what keeps an HDR tile at
/// twice a `u8` tile rather than four times: a texture is shading input, not a
/// render target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileData {
    pub bytes: Vec<u8>,
    pub kind: TileKind,
}

impl TileData {
    /// An 8-bit payload, as decoded.
    pub fn u8(bytes: Vec<u8>) -> TileData {
        TileData {
            bytes,
            kind: TileKind::U8,
        }
    }

    /// A `half` payload from samples already in memory. The EXR reader builds
    /// its bytes directly and does not come through here.
    pub fn half(samples: &[f16]) -> TileData {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_bits().to_le_bytes());
        }
        TileData {
            bytes,
            kind: TileKind::Half,
        }
    }

    /// A `half` payload whose bytes were assembled by the caller.
    pub fn half_bytes(bytes: Vec<u8>) -> TileData {
        TileData {
            bytes,
            kind: TileKind::Half,
        }
    }

    /// Components (not bytes) — three per texel either way.
    pub fn len(&self) -> usize {
        match self.kind {
            TileKind::U8 => self.bytes.len(),
            TileKind::Half => self.bytes.len() / 2,
        }
    }
}

#[cfg(test)]
impl TileData {
    /// The `u8` payload, for tests that compare a tile against the source
    /// bytes it was cut from.
    pub fn expect_u8(&self) -> &[u8] {
        assert_eq!(self.kind, TileKind::U8, "expected an 8-bit tile");
        &self.bytes
    }

    /// The `half` payload, for tests that check a float source survived.
    pub fn expect_half(&self) -> Vec<f16> {
        assert_eq!(self.kind, TileKind::Half, "expected a half tile");
        self.bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f16::from_bits(u16::from_le_bytes(*b)))
            .collect()
    }
}

/// A decoded tile, clipped to its level's bounds.
///
/// `width` is the tile's *own* width, which for an edge tile is less than the
/// file's tile edge. Indexing it by the nominal edge instead shears every
/// texture whose size is not a multiple of it.
#[derive(Debug)]
pub struct Tile {
    pub data: TileData,
    pub width: usize,
    pub height: usize,
    /// Set when the shard map evicts this tile while a thread still holds it:
    /// the count its bytes were added to, and which its drop takes them back
    /// from. See [`Held`].
    evicted: std::sync::OnceLock<Arc<Held>>,
}

/// Bytes of tiles the shard map has evicted that some thread still holds —
/// in its microcache, or in hand during the lookup that read it: memory the
/// map's `resident` no longer counts, but that is alive all the same. (A sweep
/// can evict the very tile the inserting lookup is about to return.)
///
/// Counted at the eviction rather than reserved up front (as the Ptex
/// stream's `micro_reserve` does). A thread's tiles are nearly always in the
/// map too, so a reserve would take the worst case out of the budget for
/// nothing: at 512 tiles a thread that is up to 890 MiB of a 1 GiB budget at
/// 72 threads, while ALab, which evicts nothing, holds no byte beyond the map.
/// Written on eviction and on a marked tile's final drop, both rare; a lookup
/// never touches it.
#[derive(Debug, Default)]
struct Held {
    now: AtomicU64,
    peak: AtomicU64,
}

impl Drop for Tile {
    fn drop(&mut self) {
        if let Some(held) = self.evicted.get() {
            let bytes = self.bytes();
            let was = held.now.fetch_sub(bytes, Ordering::Relaxed);
            debug_assert!(was >= bytes, "held bytes went below zero");
        }
    }
}

impl Tile {
    fn new(data: TileData, width: usize, height: usize) -> Tile {
        Tile {
            data,
            width,
            height,
            evicted: std::sync::OnceLock::new(),
        }
    }

    /// One texel of an **8-bit** tile, decoded through `to_linear`.
    ///
    /// `x` and `y` must already be inside the tile — the caller checks, because
    /// it has a fallback colour to return and this does not. The payload is
    /// exactly `width · height · 3` bytes by construction (see
    /// [`Tile::well_formed`]), so the index cannot escape it.
    ///
    /// Deliberately the same shape the sampler had before there was a second
    /// payload, down to indexing rather than `get`ting and to not taking the
    /// fallback as an argument: passing a `[f32; 3]` in cost it a
    /// materialisation on every lookup, including the overwhelming majority
    /// that never need it.
    #[inline]
    pub fn rgb_u8(&self, x: usize, y: usize, to_linear: &[f32; 256]) -> [f32; 3] {
        let o = (y * self.width + x) * 3;
        let v = &self.data.bytes;
        [
            to_linear[v[o] as usize],
            to_linear[v[o + 1] as usize],
            to_linear[v[o + 2] as usize],
        ]
    }

    /// One texel of a **half** tile. No decode: a float file carries
    /// scene-referred values, and putting a transfer curve on them would be
    /// inventing one. Same contract as [`Tile::rgb_u8`] on bounds.
    #[inline]
    pub fn rgb_half(&self, x: usize, y: usize) -> [f32; 3] {
        let o = (y * self.width + x) * 6;
        let v = &self.data.bytes;
        let at = |k: usize| f16::from_bits(u16::from_le_bytes([v[k], v[k + 1]])).to_f32();
        [at(o), at(o + 2), at(o + 4)]
    }

    /// Whether the payload really is `width · height` texels — the invariant
    /// the two accessors above rely on instead of checking per texel.
    ///
    /// Asserted where tiles are made rather than where they are read: a backend
    /// returns a tile clipped to its level, so this is a statement about the
    /// readers, and it is worth failing a test over rather than a texel.
    pub fn well_formed(&self) -> bool {
        self.data.len() == self.width * self.height * 3
    }

    fn bytes(&self) -> u64 {
        // The `Vec`'s own header and the `Arc` count are deliberately included:
        // a budget that only counts payload under-reports by ~10% on small
        // tiles, and the whole point of the number is that it bounds RSS.
        self.data.bytes.len() as u64 + std::mem::size_of::<Tile>() as u64 + 16
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
    /// The three bumped per lookup are striped (see [`StripedCounter`]); the
    /// rest move only on a decode or an eviction, which is rare enough that a
    /// shared atomic costs nothing.
    pub micro_hits: StripedCounter,
    pub hits: StripedCounter,
    pub misses: StripedCounter,
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
    /// Readers opened, and of those the ones on a file that had a reader
    /// closed before — the reopens the cap costs. A second reader opened
    /// while the first is still in use is an open, not a reopen.
    pub opens: AtomicU64,
    pub reopens: AtomicU64,
    /// The most readers open at once, counted from successful opens and
    /// actual closes: open file descriptors, which is what the cap bounds.
    pub peak_open: AtomicU64,
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
    /// The most bytes of evicted tiles that threads' microcaches kept alive
    /// at once, beside `peak_bytes`: together they are what the budget bounds.
    pub held_peak_bytes: u64,
    pub errors: u64,
    pub budget_bytes: u64,
    /// Readers opened over the render, and of those the ones on a file whose
    /// reader had been closed: the cost of the open-file cap.
    pub opens: u64,
    pub reopens: u64,
    /// The most readers — open files — that existed at once.
    pub peak_open: u64,
    /// `CRUST_TEX_MAX_OPEN_FILES`; `0` is unbounded.
    pub max_open_files: u64,
    /// Files registered with the cache.
    pub files: u64,
    /// Tiles decoded from disk, first reads and re-reads alike (Guerilla's
    /// "loaded tiles"; its "unloaded tiles" is `evictions`).
    pub loaded_tiles: u64,
    /// Bytes the cache holds now, at the end of the render.
    pub resident_bytes: u64,
    /// What every level of every registered file would hold if it were all
    /// resident — the figure a preloading renderer would have paid.
    pub total_bytes: u64,
}

/// One file's geometry and what the cache has learned about it.
struct FileSlot {
    file: TiledFile,
    /// Whether this file's first failed read was already reported at WARN.
    /// The rest go to DEBUG: one line per broken file, never per tile.
    warned: AtomicBool,
    /// Tiles this file has ever paged in, so a second page-in of the same tile
    /// can be recognised as thrashing rather than as a first read.
    seen: Mutex<std::collections::HashSet<(u8, u32)>>,
}

/// Payload bytes of a file's whole mip chain, at the width its tiles page in
/// as: RGB `u8` for an integer file, RGB `half` for a float one.
fn full_chain_bytes(file: &TiledFile) -> u64 {
    let texel = if file.is_linear() { 6 } else { 3 };
    file.levels()
        .iter()
        .map(|l| (l.width * l.height * texel) as u64)
        .sum()
}

/// Idle readers across every file, and how many readers exist.
///
/// Every `tiff` read takes `&mut self`, so a reader cannot be shared: a miss
/// takes one, reads one tile and gives it back. Each reader is an open file.
///
/// **One pool, not one per file.** Per-file pools grew to (files touched) x
/// (threads that missed on a file together) and were freed only when the
/// cache dropped, which took ALab's 6 832 `.tx` past the 1 024-descriptor soft
/// limit and failed the frame's EXR write after 22 minutes. A global count
/// alone would not fix it: when the cap is reached the cache has to find an
/// idle reader on a *cold* file to close, which needs one order over all files.
/// Without it, dead files keep their descriptors and every live file pays a
/// reopen per miss.
///
/// **The cap never blocks.** It bounds *idle* readers. A miss with nothing to
/// reuse closes the least recently returned idle reader of another file when
/// at the cap, and opens either way, so `open` can pass the cap by the threads
/// inside a miss at once. A reader returned while over the cap is closed
/// instead of pooled. The bound is the cap plus the thread count, and no
/// render thread ever waits for a reader — the rule the clock sweep follows
/// too.
///
/// The mutex is held for map operations only, twice per **miss**, never per
/// hit: never across an open, a read or a close. Readers leaving the pool are
/// moved out under it and dropped after it is released.
#[derive(Default)]
struct ReaderPool {
    idle: HashMap<u32, Vec<TileReader>>,
    /// When each file in `idle` last had a reader returned, and the same
    /// inverted so the oldest is the first key.
    stamps: HashMap<u32, u64>,
    lru: BTreeMap<u64, u32>,
    clock: u64,
    /// Readers that exist or are being opened, idle or checked out: what the
    /// cap is checked against.
    open: usize,
    /// Files that have had a reader closed, so an open on one is a reopen.
    closed: HashSet<u32>,
}

impl ReaderPool {
    /// One of `file`'s idle readers.
    fn take(&mut self, file: u32) -> Option<TileReader> {
        let readers = self.idle.get_mut(&file)?;
        let reader = readers.pop();
        if readers.is_empty() {
            self.idle.remove(&file);
            if let Some(stamp) = self.stamps.remove(&file) {
                self.lru.remove(&stamp);
            }
        }
        reader
    }

    /// An idle reader of the least recently used file, to be closed. Called
    /// only after `take` found none for the file missing, so the oldest is
    /// always another file's.
    fn evict_oldest(&mut self) -> Option<TileReader> {
        let (_, &file) = self.lru.first_key_value()?;
        let reader = self.take(file)?;
        self.open = self.open.saturating_sub(1);
        self.closed.insert(file);
        Some(reader)
    }

    /// Pools `reader`, or hands it back to be closed when over `cap`.
    fn give_back(&mut self, file: u32, reader: TileReader, cap: usize) -> Option<TileReader> {
        if cap != 0 && self.open > cap {
            self.open = self.open.saturating_sub(1);
            self.closed.insert(file);
            return Some(reader);
        }
        self.idle.entry(file).or_default().push(reader);
        self.clock += 1;
        if let Some(old) = self.stamps.insert(file, self.clock) {
            self.lru.remove(&old);
        }
        self.lru.insert(self.clock, file);
        None
    }

    /// Every idle reader, to be closed.
    fn drain(&mut self) -> Vec<TileReader> {
        self.closed.extend(self.idle.keys().copied());
        let all: Vec<TileReader> = self.idle.drain().flat_map(|(_, r)| r).collect();
        self.stamps.clear();
        self.lru.clear();
        self.open = self.open.saturating_sub(all.len());
        all
    }
}

/// Whether an open failed for want of a file descriptor (`EMFILE`, `ENFILE`;
/// the same numbers on Linux and macOS). Only then is closing idle readers
/// worth trying: a missing or corrupt file would otherwise empty the pool on
/// every one of its misses.
fn out_of_descriptors(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(e.raw_os_error(), Some(23 | 24))
    }
    #[cfg(not(unix))]
    {
        let _ = e;
        false
    }
}

/// The cache. One per render, shared by every streaming texture in it.
pub struct TileCache {
    /// Process-unique identity, part of every microcache key. A [`TileId`]'s
    /// file index is only unique *within* one cache, while the microcache is
    /// per thread and outlives any one cache — so without this, a second
    /// cache in the same process (a second `FileAssets`: a test, a host
    /// rendering twice) was handed the first one's tiles on the same thread.
    id: u32,
    shards: Vec<Mutex<HashMap<TileId, Entry>>>,
    files: Mutex<Vec<Arc<FileSlot>>>,
    readers: Mutex<ReaderPool>,
    /// Readers actually open: up on a successful open, down after a close.
    /// The pool's `open` also counts opens still being tried, so the peak is
    /// taken from this one.
    live: AtomicU64,
    /// `CRUST_TEX_MAX_OPEN_FILES`: idle readers kept; `0` keeps every one.
    max_open: usize,
    /// The OS error the next open fails with, `0` for none: how a test makes
    /// the process run out of descriptors.
    #[cfg(test)]
    inject_open_error: std::sync::atomic::AtomicI32,
    resident: AtomicU64,
    budget: u64,
    /// Evicted tiles still alive in some thread's microcache. The budget
    /// bounds `resident + held`.
    held: Arc<Held>,
    /// The bytes of this cache's tiles one thread's microcache may retain:
    /// half the budget, split across the threads, so that `held` cannot pass
    /// half the budget even if every thread held only evicted tiles.
    micro_share: u64,
    /// Held by whichever thread is currently making room. `try_lock`ed, never
    /// blocked on: a thread that finds a sweep in progress carries on and lets
    /// the other one do the work.
    sweeping: Mutex<usize>,
    pub stats: CacheStats,
}

impl TileCache {
    /// A cache of `budget_bytes` that keeps at most `max_open_files` idle
    /// readers open (`0`: no cap).
    pub fn new(budget_bytes: u64, max_open_files: usize) -> TileCache {
        static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let budget = budget_bytes.max(mib_to_bytes(1));
        TileCache {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
            files: Mutex::new(Vec::new()),
            readers: Mutex::new(ReaderPool::default()),
            live: AtomicU64::new(0),
            max_open: max_open_files,
            #[cfg(test)]
            inject_open_error: std::sync::atomic::AtomicI32::new(0),
            resident: AtomicU64::new(0),
            budget,
            held: Arc::default(),
            // The thread count the Ptex stream sizes its slots by: what rayon
            // defaults its pool to, an overestimate under a smaller
            // `RAYON_NUM_THREADS`, which errs on the safe side.
            micro_share: budget / 2 / crate::ptex_stream::micro_threads() as u64,
            sweeping: Mutex::new(0),
            stats: CacheStats::default(),
        }
    }

    /// The same cache with each thread's share set outright, so a test does
    /// not depend on how many cores the machine has.
    #[cfg(test)]
    fn with_micro_share(mut self, bytes: u64) -> TileCache {
        self.micro_share = bytes;
        self
    }

    /// The tile cache budget `config` asks for (`CRUST_TEX_CACHE_MB`), in
    /// bytes.
    pub fn budget_of(config: &crust_core::Config) -> u64 {
        mib_to_bytes(config.tex_cache_mb.get())
    }

    pub fn resident(&self) -> u64 {
        self.resident.load(Ordering::Relaxed)
    }

    /// Bytes of evicted tiles still alive in some thread's microcache.
    pub fn held(&self) -> u64 {
        self.held.now.load(Ordering::Relaxed)
    }

    /// Registers a file and returns the index a [`TileId`] names it by.
    pub fn intern(&self, file: TiledFile) -> Option<u32> {
        let mut files = lock(&self.files)?;
        let id = files.len() as u32;
        files.push(Arc::new(FileSlot {
            file,
            warned: AtomicBool::new(false),
            seen: Mutex::new(std::collections::HashSet::new()),
        }));
        Some(id)
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
        let (files, total_bytes) = lock(&self.files)
            .map(|files| {
                let total = files.iter().map(|f| full_chain_bytes(&f.file)).sum();
                (files.len() as u64, total)
            })
            .unwrap_or((0, 0));
        CacheCounters {
            files,
            loaded_tiles: s.decoded.load(Ordering::Relaxed),
            resident_bytes: self.resident(),
            total_bytes,
            micro_hits: s.micro_hits.load(),
            hits: s.hits.load(),
            misses: s.misses.load(),
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
            held_peak_bytes: self.held.peak.load(Ordering::Relaxed),
            errors: s.errors.load(Ordering::Relaxed),
            budget_bytes: self.budget,
            opens: s.opens.load(Ordering::Relaxed),
            reopens: s.reopens.load(Ordering::Relaxed),
            peak_open: s.peak_open.load(Ordering::Relaxed),
            max_open_files: self.max_open as u64,
        }
    }

    /// Closes every idle reader. A later miss reopens, so this is safe at any
    /// time; the host calls it when the render is done, so writing the outputs
    /// never competes with texture files for descriptors, whatever the cap.
    pub fn release_readers(&self) {
        self.close(self.drain_idle());
    }

    /// Closes `readers`, outside the pool's lock, and only then counts them
    /// closed: the live count may briefly over-report, never under-report.
    fn close(&self, readers: impl IntoIterator<Item = TileReader>) {
        let n = readers.into_iter().count() as u64;
        if n > 0 {
            self.live.fetch_sub(n, Ordering::Relaxed);
        }
    }

    /// Takes every idle reader out of the pool; the caller drops them after
    /// the lock is released.
    fn drain_idle(&self) -> Vec<TileReader> {
        lock(&self.readers)
            .map(|mut p| p.drain())
            .unwrap_or_default()
    }

    /// Readers that exist right now, pooled or checked out — one open file
    /// descriptor each.
    #[cfg(test)]
    pub(crate) fn open_readers(&self) -> usize {
        lock(&self.readers).map(|p| p.open).unwrap_or(0)
    }

    /// Readers in the pool, waiting for a miss.
    #[cfg(test)]
    pub(crate) fn idle_readers(&self) -> usize {
        lock(&self.readers)
            .map(|p| p.idle.values().map(Vec::len).sum())
            .unwrap_or(0)
    }

    /// A reader for `file`: an idle one when there is one, else a fresh open.
    ///
    /// At the cap, the least recently used other file's idle reader is closed
    /// first. If the open runs out of descriptors, every idle reader is closed
    /// and the open tried once more before the miss counts as failed.
    fn checkout(&self, file: u32, slot: &FileSlot) -> Option<TileReader> {
        let (victim, reopen) = {
            let mut pool = lock(&self.readers)?;
            if let Some(reader) = pool.take(file) {
                return Some(reader);
            }
            let victim = if self.max_open != 0 && pool.open >= self.max_open {
                pool.evict_oldest()
            } else {
                None
            };
            pool.open += 1;
            (victim, pool.closed.contains(&file))
        };
        self.close(victim);
        let opened = match self.open_reader(slot) {
            Err(e) if out_of_descriptors(&e) => {
                self.close(self.drain_idle());
                self.open_reader(slot)
            }
            other => other,
        };
        match opened {
            Ok(reader) => {
                self.stats.opens.fetch_add(1, Ordering::Relaxed);
                if reopen {
                    self.stats.reopens.fetch_add(1, Ordering::Relaxed);
                }
                let live = self.live.fetch_add(1, Ordering::Relaxed) + 1;
                self.stats.peak_open.fetch_max(live, Ordering::Relaxed);
                Some(reader)
            }
            Err(e) => {
                if let Some(mut pool) = lock(&self.readers) {
                    pool.open = pool.open.saturating_sub(1);
                }
                self.read_failed(slot, &e);
                None
            }
        }
    }

    /// Returns a reader taken by [`TileCache::checkout`], closing it instead
    /// when the pool is over the cap.
    fn check_in(&self, file: u32, reader: TileReader) {
        let excess = match lock(&self.readers) {
            Some(mut pool) => pool.give_back(file, reader, self.max_open),
            None => Some(reader),
        };
        self.close(excess);
    }

    fn open_reader(&self, slot: &FileSlot) -> io::Result<TileReader> {
        #[cfg(test)]
        {
            let code = self.inject_open_error.swap(0, Ordering::Relaxed);
            if code != 0 {
                return Err(io::Error::from_raw_os_error(code));
            }
        }
        slot.file.reader()
    }

    /// Counts a failed open or read; names the file at WARN the first time.
    fn read_failed(&self, slot: &FileSlot, e: &io::Error) {
        self.stats.errors.fetch_add(1, Ordering::Relaxed);
        if slot.warned.swap(true, Ordering::Relaxed) {
            tracing::debug!("{}: {e}", slot.file.path().display());
        } else {
            tracing::warn!(
                "{}: {e} — its unreadable tiles use the texture's fallback colour",
                slot.file.path().display()
            );
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
                // Only when clear: a hit on a hot tile is otherwise a store
                // to memory every thread reading that tile has cached.
                if !e.used {
                    e.used = true;
                }
                e.tile.clone()
            })
        }) {
            self.stats.hits.add(1);
            return Some(shard);
        }
        self.stats.misses.add(1);
        self.page_in(id)
    }

    fn page_in(&self, id: TileId) -> Option<Arc<Tile>> {
        let _p = crust_core::profile::scope(crust_core::profile::Section::TextureLoad);
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

        let mut dec = self.checkout(id.file, &slot)?;
        let read = slot
            .file
            .read_tile(&mut dec, id.level as usize, id.tile)
            .map_err(|e| self.read_failed(&slot, &e))
            .ok();
        // The cursor goes back whatever happened — a decode error leaves it
        // usable, and closing it here would cost the next miss a reopen.
        self.check_in(id.file, dec);

        let data = read?;
        let (width, height) = slot
            .file
            .level(id.level as usize)
            .tile_size(id.tile, slot.file.tile_edge());
        let tile = Arc::new(Tile::new(data, width, height));
        // The accessors index without checking, so this is where the size is
        // established. A backend that returned the wrong shape would be a bug
        // in the backend, and one a test should catch rather than a render.
        debug_assert!(
            tile.well_formed(),
            "{}: level {} tile {} is {}x{} but holds {} components",
            slot.file.path().display(),
            id.level,
            id.tile,
            width,
            height,
            tile.data.len()
        );
        let bytes = tile.bytes();
        self.stats.bytes_read.fetch_add(bytes, Ordering::Relaxed);

        // The shard guard is scoped tightly and dropped before `make_room`.
        // `std::sync::Mutex` is not reentrant and the sweep walks *every*
        // shard, so holding this one across the call deadlocks the thread
        // against itself the first time a texture exceeds the budget — which
        // is to say, on every render the cache is actually for.
        //
        // The bytes are counted *under* the guard, though: a sweep needs this
        // shard's lock to evict the entry, so counting after the drop lets
        // another thread evict it and subtract its bytes first, wrapping
        // `resident` below zero.
        let resident = match lock(self.shard(&id)) {
            Some(mut m) => m
                .insert(
                    id,
                    Entry {
                        tile: tile.clone(),
                        used: true,
                    },
                )
                .is_none()
                .then(|| self.resident.fetch_add(bytes, Ordering::Relaxed) + bytes),
            None => None,
        };
        self.stats.decoded.fetch_add(1, Ordering::Relaxed);
        if let Some(now) = resident {
            self.stats.peak_bytes.fetch_max(now, Ordering::Relaxed);
            if now + self.held() > self.budget {
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
                        return true;
                    }
                    let bytes = e.tile.bytes();
                    freed += bytes;
                    // A thread still holds it, in its microcache or in hand:
                    // the bytes stay alive past this eviction, so they move to
                    // `held` until the last holder drops the tile
                    // (`impl Drop for Tile`).
                    // Under the shard lock no thread can take a new reference,
                    // so a count of one means the map's is the last and the
                    // tile dies right here, unmarked. Marked before the map
                    // lets go, so its final drop always sees the mark.
                    if Arc::strong_count(&e.tile) > 1
                        && e.tile.evicted.set(self.held.clone()).is_ok()
                    {
                        let now = self.held.now.fetch_add(bytes, Ordering::Relaxed) + bytes;
                        self.held.peak.fetch_max(now, Ordering::Relaxed);
                    }
                    false
                });
                if freed > 0 {
                    self.resident.fetch_sub(freed, Ordering::Relaxed);
                    self.stats.evictions.fetch_add(1, Ordering::Relaxed);
                }
            }
            if self.resident.load(Ordering::Relaxed) + self.held() <= self.budget {
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

/// Sets in the per-thread microcache, and ways per set: 512 tiles a thread.
///
/// Capacity is what pays. Measured on ALab frame 1004 at 72 threads and
/// 32 spp (`grow-texture-microcache`), lookups that missed the microcache
/// for a shard fell from 152.0 M at 16 x 4 to 104.2 M at 64 x 4, 78.8 M at
/// 64 x 4 indexed by the whole tile id, and 50.3 M here. Texture went 1.71 ->
/// 1.28 us per `eval` and the render 14.6% faster; its 8 -> 72 thread
/// slowdown is now the machine's own 1.6x, so the shard contention is gone.
/// Earlier, 8 -> 16 sets had taken hits from 81.2% to 84.9%.
const MICRO_SETS: usize = 64;
/// Ways per set: a trilinear tap reads two levels, and a footprint straddling
/// a tile edge doubles that.
const MICRO_WAYS: usize = 8;
const _: () = assert!(MICRO_SETS.is_power_of_two());

type MicroSet = Ways<(u32, TileId), Arc<Tile>, MICRO_WAYS>;
/// The per-thread microcache: `MICRO_SETS` sets of `MICRO_WAYS` `(key, tile)`
/// pairs, newest first within a set, keyed by the owning cache's
/// [`TileCache::id`] as well as the tile.
type MicroSlots = [MicroSet; MICRO_SETS];

thread_local! {
    /// The most recently used tiles, per thread.
    ///
    /// The highest-leverage part of the whole cache and the cheapest: a
    /// bilinear tap reads one tile up to four times in a row and trilinear
    /// doubles that, so this absorbs most lookups before any lock is touched.
    /// A miss is what costs: it locks a shard and clones the tile's `Arc`,
    /// and the entry it pushes out drops one — shared lines every thread
    /// reading that tile writes.
    ///
    /// **A production material interleaves several textures.** The two slots
    /// this started as (OIIO's number, right for one texture per shading
    /// point: 98.6% hits on the alias plane) found the *previous* texture's
    /// tiles on each `eval`, a 25% miss rate on ALab (`docs/alab_profile.md`).
    /// What decides the rate is how many recent tiles a thread keeps across
    /// materials, which is why this is 512 of them (see [`MICRO_SETS`]).
    ///
    /// **In the budget.** What a thread keeps of one cache is bounded by that
    /// cache's `micro_share` ([`RETAINED`]), and an evicted tile a thread
    /// still holds is counted in the cache's `held` until it is dropped.
    static MICRO: std::cell::RefCell<MicroSlots> =
        const { std::cell::RefCell::new([MicroSet::EMPTY; MICRO_SETS]) };

    /// Bytes of tiles this thread's microcache holds, whichever cache they
    /// came from. Read and written on the miss path only.
    static RETAINED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The microcache set a key lives in: a one-multiply (Fibonacci) hash of the
/// whole key, top bits.
///
/// **The whole tile id, not the file.** A file index kept each texture's tiles
/// in one set, which a trilinear tap at a tile corner — up to eight tiles of
/// one texture — overflows. Measured at 64 x 4, the file index sent 104.2 M
/// lookups to the shards and this 78.8 M. Indexing by (file, level) instead
/// matched this one's hits at 64 x 8 but scanned deeper: 2.5% more
/// instructions on the alias scene against 1.4%. The cost of any tile-dependent
/// index is the hash per tap, where the file index was hoisted out of the
/// taps. On the alias scene, whose microcache already hits 99.9%, that is the
/// whole 1.4% (`grow-texture-microcache`, a trade taken for ALab's 14.6%).
#[inline]
fn micro_set(key: &(u32, TileId)) -> usize {
    let x = key.1.tile as u64
        ^ ((key.1.file as u64) << 32)
        ^ ((key.1.level as u64) << 24)
        ^ ((key.0 as u64) << 56);
    (x.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - MICRO_SETS.trailing_zeros())) as usize
}

/// Puts `tile` in front of its set, unless that would take this thread past
/// `cache`'s share; the oldest way falls off the end (see `Ways`).
///
/// A tile left out was still used for the lookup that read it: only the next
/// one pays a shard. With a share below one tile, which a small budget on many
/// threads gives, the microcache keeps nothing and every lookup takes the
/// shard path — slower, and still right.
fn retain(cache: &TileCache, set: usize, key: (u32, TileId), tile: Arc<Tile>) {
    let evicted = MICRO.with(|m| {
        let mut slots = m.borrow_mut();
        let out = slots[set].oldest_if_full().map_or(0, |t| t.bytes());
        let now = RETAINED.get() - out + tile.bytes();
        if now > cache.micro_share {
            return None;
        }
        RETAINED.set(now);
        slots[set].push(key, tile)
    });
    // After the borrow ends, as before: the last reference to a tile the map
    // evicted gives its bytes back to `held` as it drops.
    drop(evicted);
}

/// Reads `id` through the per-thread microcache and hands the tile to `f`.
///
/// Takes a closure rather than returning the `Arc` on purpose. A texel fetch
/// is the hottest thing in a textured render — 8.7 M of them in a 640x360
/// frame at 4 spp — and returning a handle means an atomic refcount increment
/// and decrement on *every one*, even the 98.6% that hit this thread's own
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
    let key = (cache.id, id);
    let set = micro_set(&key);
    let hit = MICRO.with(|m| {
        let slots = m.borrow();
        let tile = slots[set].get(&key)?;
        Some(f.take()?(tile))
    });
    if let Some(r) = hit {
        cache.stats.micro_hits.add(1);
        return Some(r);
    }
    // Miss: the borrow above is released before this, because `cache.get` can
    // decode and must not run under a thread-local borrow.
    let tile = cache.get(id)?;
    let r = f.take()?(&tile);
    retain(cache, set, key, tile);
    Some(r)
}

/// Empties every thread's microcache.
///
/// Needed only by tests: the entries hold `Arc<Tile>`s from a cache that a test
/// is about to drop, and a stale hit would answer from the wrong cache.
#[cfg(test)]
pub fn clear_microcache() {
    let old =
        MICRO.with(|m| std::mem::replace(&mut *m.borrow_mut(), [MicroSet::EMPTY; MICRO_SETS]));
    drop(old);
    RETAINED.set(0);
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
        write_tx(&path, &src, w, h, crust_core::ResolvedColorSpace::RAW).expect("write");
        path
    }

    #[test]
    fn a_tile_read_through_the_cache_matches_a_direct_read() {
        clear_microcache();
        let path = fixture("direct", 300, 200);
        let tf = TiledFile::open(&path).expect("open");
        let cache = TileCache::new(64 * 1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
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
                assert_eq!(via.data, direct, "level {level} tile {tile}");
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

    /// The cache is indifferent to what backs a file, and its byte budget is
    /// not.
    ///
    /// A `half` tile is six bytes a texel against a `u8` tile's three, so the
    /// same budget holds half as many of them. That is the honest cost of HDR
    /// and it has to show up in the accounting rather than in a surprise at
    /// render time — a budget that counted payloads it does not hold would
    /// bound nothing.
    #[test]
    fn half_tiles_are_budgeted_at_their_real_size() {
        clear_microcache();
        let dir = std::env::temp_dir().join("crust_tilecache_half");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("t.tx");
        let (w, h) = (512usize, 512usize);
        let src: Vec<f32> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                [x * 0.125, y * 0.0625, 12.5]
            })
            .collect();
        crate::tiled::write_tx_exr(&path, &src, w, h, crust_core::ResolvedColorSpace::RAW)
            .expect("write");

        let tf = TiledFile::open(&path).expect("open");
        let budget = 1024 * 1024;
        let cache = TileCache::new(budget, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
        let id = cache.intern(tf.clone()).expect("intern");
        let mut dec = tf.reader().expect("reader");

        let l0 = tf.level(0);
        let tiles = (l0.across * l0.down) as u32;
        for tile in 0..tiles {
            let direct = tf.read_tile(&mut dec, 0, tile).expect("direct");
            let via = cache
                .get(TileId {
                    file: id,
                    level: 0,
                    tile,
                })
                .expect("cached");
            assert_eq!(via.data, direct, "tile {tile}");
        }

        let c = cache.counters();
        // Six bytes a texel, plus the per-tile header the budget deliberately
        // counts. A `u8` file of the same size would report half of this.
        let texels = (w * h) as u64;
        assert!(
            c.bytes_read >= texels * 6 && c.bytes_read < texels * 6 + tiles as u64 * 128,
            "{} bytes for {texels} texels",
            c.bytes_read
        );
        assert!(c.evictions > 0, "a 1 MiB budget must have swept");
        assert!(
            cache.resident() <= budget * 2,
            "resident {} against a {budget}-byte budget",
            cache.resident()
        );

        let _ = std::fs::remove_dir_all(&dir);
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
        let cache = TileCache::new(budget, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
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
        let cache = Arc::new(TileCache::new(
            512 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));
        let id = cache.intern(tf.clone()).expect("intern");

        // The expected contents, computed once, single-threaded.
        let mut dec = tf.reader().expect("reader");
        let l0 = tf.level(0);
        let tiles = (l0.across * l0.down) as u32;
        let want: Vec<Vec<u8>> = (0..tiles)
            .map(|t| {
                tf.read_tile(&mut dec, 0, t)
                    .expect("direct")
                    .expect_u8()
                    .to_vec()
            })
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
                        assert_eq!(
                            got.data.expect_u8(),
                            want[t as usize],
                            "tile {t} on thread {k}"
                        );
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
        let cache = TileCache::new(64 * 1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
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
        let first = with_tile(&cache, a, |t| t.data.clone()).expect("a");
        let second = with_tile(&cache, b, |t| t.data.clone()).expect("b");
        let base = cache.counters();

        // Four alternating taps — the trilinear pattern — must all be
        // microcache hits, because two slots hold both levels at once.
        for _ in 0..2 {
            assert_eq!(with_tile(&cache, a, |t| t.data.clone()).expect("a"), first);
            assert_eq!(with_tile(&cache, b, |t| t.data.clone()).expect("b"), second);
        }
        let now = cache.counters();
        assert_eq!(now.micro_hits, base.micro_hits + 4);
        assert_eq!(now.hits, base.hits, "no lookup should have reached a shard");
        assert_eq!(now.misses, base.misses);

        clear_microcache();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A production material samples several textures per shading point, one
    /// after another, each at two levels. With the old two slots shared by all
    /// textures, every texture's first tap found the previous texture's tiles
    /// and missed — 25% of lookups on ALab. Each texture must keep its own.
    #[test]
    fn interleaved_textures_do_not_evict_each_other() {
        clear_microcache();
        let path = fixture("micro_interleave", 256, 256);
        let cache = TileCache::new(64 * 1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
        // Five textures, as an ALab material has.
        let files: Vec<u32> = (0..5)
            .map(|_| {
                cache
                    .intern(TiledFile::open(&path).expect("open"))
                    .expect("intern")
            })
            .collect();
        let taps = |cache: &TileCache| {
            for &file in &files {
                for level in 0..2 {
                    let id = TileId {
                        file,
                        level,
                        tile: 0,
                    };
                    with_tile(cache, id, |t| t.width).expect("tile");
                }
            }
        };
        taps(&cache); // warm: every tile once
        let base = cache.counters();
        for _ in 0..3 {
            taps(&cache);
        }
        let now = cache.counters();
        assert_eq!(
            now.micro_hits - base.micro_hits,
            3 * 5 * 2,
            "every tap a microcache hit"
        );
        assert_eq!(now.hits, base.hits, "no lookup reached a shard");

        clear_microcache();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A trilinear tap at a tile corner reads four tiles at each of two
    /// levels, all of one texture. A file-indexed set of four ways could not
    /// hold them; every one must be a microcache hit the second time.
    #[test]
    fn a_trilinear_corner_stays_in_the_microcache() {
        clear_microcache();
        let path = fixture("micro_corner", 512, 512);
        let tf = TiledFile::open(&path).expect("open");
        let cache = TileCache::new(64 * 1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
        let id = cache.intern(tf.clone()).expect("intern");
        let corner = |level: u8| {
            let across = tf.level(level as usize).across as u32;
            [0, 1, across, across + 1].map(|tile| TileId {
                file: id,
                level,
                tile,
            })
        };
        let taps: Vec<TileId> = corner(0).into_iter().chain(corner(1)).collect();
        for &t in &taps {
            with_tile(&cache, t, |t| t.width).expect("tile");
        }
        let base = cache.counters();
        for &t in &taps {
            with_tile(&cache, t, |t| t.width).expect("tile");
        }
        let now = cache.counters();
        assert_eq!(now.micro_hits - base.micro_hits, taps.len() as u64);
        assert_eq!(now.hits, base.hits, "no tap reached a shard");

        clear_microcache();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Floods `cache` with every level-0 tile of `file` through the shard map
    /// alone, until `until` holds or four passes have run.
    fn flood(cache: &TileCache, file: u32, tiles: u32, until: impl Fn(&TileCache) -> bool) {
        for _ in 0..4 {
            for tile in 0..tiles {
                cache.get(TileId {
                    file,
                    level: 0,
                    tile,
                });
            }
            if until(cache) {
                return;
            }
        }
    }

    /// What a full `u8` tile counts for in the budget.
    fn full_u8_tile_bytes() -> u64 {
        (TILE_EDGE * TILE_EDGE * 3) as u64 + std::mem::size_of::<Tile>() as u64 + 16
    }

    /// Evicted tiles no thread keeps are released as soon as the lookup that
    /// read them lets go. The sweep can evict the very tile the inserting
    /// lookup still has in hand, so the peak may reach that one tile, never
    /// more on one thread.
    #[test]
    fn evicting_unheld_tiles_holds_nothing() {
        clear_microcache();
        let path = fixture("held_none", 1536, 1536);
        let tf = TiledFile::open(&path).expect("open");
        let cache = TileCache::new(1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
        let id = cache.intern(tf.clone()).expect("intern");
        let l0 = tf.level(0);
        flood(&cache, id, (l0.across * l0.down) as u32, |c| {
            c.counters().evictions > 0
        });
        assert!(cache.counters().evictions > 0, "the flood must have swept");
        assert_eq!(cache.held(), 0);
        assert!(cache.counters().held_peak_bytes <= full_u8_tile_bytes());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A tile the map evicts while this thread's microcache holds it stays
    /// counted, against its own cache only, until the microcache lets go.
    #[test]
    fn an_evicted_tile_a_thread_holds_counts_until_dropped() {
        clear_microcache();
        let path = fixture("held_one", 2048, 2048);
        let tf = TiledFile::open(&path).expect("open");
        // Large enough that one thread's share holds a tile on any core
        // count, small enough that the flood below must evict.
        let cache = TileCache::new(8 * 1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
        let other = TileCache::new(64 * 1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
        let id = cache.intern(tf.clone()).expect("intern");
        let other_id = other.intern(tf.clone()).expect("intern");
        let held_tile = TileId {
            file: id,
            level: 1,
            tile: 0,
        };
        let bytes = with_tile(&cache, held_tile, |t| t.bytes()).expect("tile");
        // The other cache's tile is held too, and never evicted.
        with_tile(
            &other,
            TileId {
                file: other_id,
                level: 1,
                tile: 0,
            },
            |t| t.width,
        )
        .expect("tile");

        let l0 = tf.level(0);
        flood(&cache, id, (l0.across * l0.down) as u32, |c| c.held() > 0);
        assert_eq!(cache.held(), bytes, "the held tile is counted once");
        // Plus, at most, the tile the flood's own lookup had in hand.
        let peak = cache.counters().held_peak_bytes;
        assert!(
            (bytes..=bytes + full_u8_tile_bytes()).contains(&peak),
            "peak {peak}"
        );
        assert_eq!(other.held(), 0, "another cache's count is its own");

        clear_microcache();
        assert_eq!(cache.held(), 0, "dropping the last holder releases it");
        assert_eq!(cache.counters().held_peak_bytes, peak, "the peak stays");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The share is half the budget split across the threads.
    #[test]
    fn a_threads_share_is_half_the_budget_split_across_threads() {
        let budget = 64 * 1024 * 1024;
        let cache = TileCache::new(budget, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
        let threads = crate::ptex_stream::micro_threads() as u64;
        assert_eq!(cache.micro_share, budget / 2 / threads);
    }

    /// A share below one tile keeps nothing: every lookup takes the shard
    /// path and still returns the right tile. Another cache's larger share
    /// is not limited by it.
    #[test]
    fn a_share_below_one_tile_retains_nothing() {
        clear_microcache();
        let path = fixture("held_share", 256, 256);
        let tf = TiledFile::open(&path).expect("open");
        let small = TileCache::new(64 * 1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES)
            .with_micro_share(0);
        let large = TileCache::new(64 * 1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
        let s = small.intern(tf.clone()).expect("intern");
        let l = large.intern(tf.clone()).expect("intern");
        let at = |file| TileId {
            file,
            level: 0,
            tile: 1,
        };
        let mut dec = tf.reader().expect("reader");
        let want = tf.read_tile(&mut dec, 0, 1).expect("direct");

        for _ in 0..3 {
            let got = with_tile(&small, at(s), |t| t.data.clone()).expect("tile");
            assert_eq!(got, want);
        }
        let c = small.counters();
        assert_eq!(c.micro_hits, 0, "nothing was retained");
        assert_eq!(c.hits + c.misses, 3);
        assert_eq!(RETAINED.get(), 0);

        for _ in 0..3 {
            with_tile(&large, at(l), |t| t.width).expect("tile");
        }
        assert_eq!(large.counters().micro_hits, 2, "the large share retains");

        clear_microcache();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Striping must not lose counts: each thread increments its own line and
    /// the reader sums them, so the total is exact however many threads ran —
    /// including more threads than stripes, which share one.
    #[test]
    fn striped_counter_is_exact_across_threads() {
        let c = StripedCounter::default();
        let threads = STRIPES + 7;
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    for _ in 0..1000 {
                        c.add(1);
                    }
                });
            }
        });
        assert_eq!(c.load(), threads as u64 * 1000);
    }

    /// Writes `n` small `.tx` files in one directory, for tests that care how
    /// many files a render touches rather than what is in them.
    fn many_fixtures(name: &str, n: usize) -> Vec<PathBuf> {
        let dir = std::env::temp_dir().join(format!("crust_tilecache_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        (0..n)
            .map(|k| {
                let (w, h) = (192, 128);
                let path = dir.join(format!("t{k}.tx"));
                let src: Vec<u8> = (0..w * h)
                    .flat_map(|i| [(i % 251) as u8, k as u8, ((i / w) % 251) as u8])
                    .collect();
                write_tx(&path, &src, w, h, crust_core::ResolvedColorSpace::RAW).expect("write");
                path
            })
            .collect()
    }

    /// Every level-0 tile of every file, from `threads` threads at once, each
    /// starting at a different file so their misses overlap.
    fn read_everything(cache: &TileCache, files: &[u32], tiles: u32, threads: u32) {
        std::thread::scope(|s| {
            for k in 0..threads {
                s.spawn(move || {
                    clear_microcache();
                    for round in 0..4u32 {
                        for f in 0..files.len() as u32 {
                            let file = files[((f + k + round) as usize) % files.len()];
                            for tile in 0..tiles {
                                let id = TileId {
                                    file,
                                    level: 0,
                                    tile,
                                };
                                assert!(cache.get(id).is_some(), "file {file} tile {tile}");
                            }
                        }
                    }
                });
            }
        });
    }

    /// The regression the descriptor cap fixes: with no cap, pooled readers
    /// are never closed, so the cache ends a render holding at least one open
    /// file per file it ever read — ALab's 6 832 `.tx` past a 1 024 soft limit.
    #[test]
    fn without_a_cap_every_file_read_keeps_a_reader_open() {
        clear_microcache();
        let paths = many_fixtures("uncapped", 24);
        let cache = TileCache::new(1024 * 1024, 0);
        let files: Vec<u32> = paths
            .iter()
            .map(|p| {
                cache
                    .intern(TiledFile::open(p).expect("open"))
                    .expect("intern")
            })
            .collect();
        read_everything(&cache, &files, 6, 8);
        assert!(
            cache.open_readers() >= files.len(),
            "{} open readers for {} files",
            cache.open_readers(),
            files.len()
        );
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    /// Interns every path in `paths`, in order.
    fn intern_all(cache: &TileCache, paths: &[PathBuf]) -> Vec<u32> {
        paths
            .iter()
            .map(|p| {
                cache
                    .intern(TiledFile::open(p).expect("open"))
                    .expect("intern")
            })
            .collect()
    }

    /// The cap bounds open files by itself plus one reader per thread in a
    /// miss, and leaves at most itself open once the misses are over.
    #[test]
    fn a_cap_bounds_open_readers_by_itself_plus_the_threads() {
        clear_microcache();
        let paths = many_fixtures("capped", 24);
        let (cap, threads) = (4, 8);
        let cache = TileCache::new(1024 * 1024, cap);
        let files = intern_all(&cache, &paths);
        read_everything(&cache, &files, 6, threads as u32);
        let c = cache.counters();
        assert!(
            c.peak_open as usize <= cap + threads,
            "peak {} open against a cap of {cap} on {threads} threads",
            c.peak_open
        );
        assert!(cache.idle_readers() <= cap, "{} idle", cache.idle_readers());
        assert_eq!(
            cache.open_readers(),
            cache.idle_readers(),
            "a reader leaked"
        );
        assert!(c.reopens > 0, "24 files under a cap of 4 must reopen");
        assert_eq!(c.max_open_files, cap as u64);
        assert_eq!(c.errors, 0);
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    /// A reopen is an open on a file that had a reader closed. Threads opening
    /// a second reader on a file whose first is in use closed nothing, so
    /// under a cap the scene never reaches, nothing counts as a reopen.
    #[test]
    fn concurrent_first_reads_are_not_reopens() {
        clear_microcache();
        let paths = many_fixtures("no_reopen", 1);
        let cache = TileCache::new(1024 * 1024, 256);
        let files = intern_all(&cache, &paths);
        read_everything(&cache, &files, 6, 8);
        let c = cache.counters();
        assert!(c.opens >= 1);
        assert_eq!(c.reopens, 0, "{} opens, nothing closed", c.opens);
        // Closing and reading again is a reopen.
        cache.release_readers();
        let id = TileId {
            file: files[0],
            level: 1,
            tile: 0,
        };
        assert!(cache.get(id).is_some());
        assert_eq!(cache.counters().reopens, 1);
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    /// The reported peak counts files that were open, not opens that were
    /// tried and failed.
    #[test]
    fn failed_opens_do_not_raise_the_peak() {
        clear_microcache();
        let paths = many_fixtures("peak_failed", 3);
        let cache = TileCache::new(1024 * 1024, 256);
        let files = intern_all(&cache, &paths);
        let tile0 = |file| TileId {
            file,
            level: 0,
            tile: 0,
        };
        for &f in &files[..2] {
            assert!(cache.get(tile0(f)).is_some());
        }
        assert_eq!(cache.counters().peak_open, 2);
        cache.inject_open_error.store(2, Ordering::Relaxed); // ENOENT
        assert!(cache.get(tile0(files[2])).is_none());
        assert_eq!(cache.counters().peak_open, 2, "a failed open was counted");
        // Closing and opening again does not grow the peak either.
        cache.release_readers();
        assert!(cache.get(tile0(files[2])).is_some());
        assert_eq!(cache.counters().peak_open, 2);
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    /// Readers are interchangeable cursors: which one decoded a tile, and how
    /// often files were closed and reopened, cannot change its bytes.
    #[test]
    fn the_cap_does_not_change_a_single_byte() {
        clear_microcache();
        let paths = many_fixtures("cap_identity", 16);
        let capped = TileCache::new(1024 * 1024, 1);
        let unbounded = TileCache::new(1024 * 1024, 0);
        let files = intern_all(&capped, &paths);
        assert_eq!(intern_all(&unbounded, &paths), files);
        let tiles = 6;
        read_everything(&capped, &files, tiles, 8);
        read_everything(&unbounded, &files, tiles, 8);
        assert!(capped.counters().reopens > 0, "the cap never engaged");
        for &file in &files {
            for level in 0..TiledFile::open(&paths[file as usize])
                .expect("open")
                .level_count()
            {
                for tile in 0..tiles {
                    let id = TileId {
                        file,
                        level: level as u8,
                        tile,
                    };
                    let a = capped.get(id).map(|t| t.data.clone());
                    let b = unbounded.get(id).map(|t| t.data.clone());
                    assert_eq!(a, b, "file {file} level {level} tile {tile}");
                }
            }
        }
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    /// Out of descriptors, the cache gives its idle ones back and tries again,
    /// so the tile is read rather than lost. Any other failure is a property
    /// of the file, and must not empty the pool.
    #[test]
    fn running_out_of_descriptors_drains_the_pool_and_retries() {
        clear_microcache();
        let paths = many_fixtures("emfile", 4);
        let cache = TileCache::new(1024 * 1024, 256);
        let files = intern_all(&cache, &paths);
        let tile0 = |file| TileId {
            file,
            level: 0,
            tile: 0,
        };
        for &f in &files[..3] {
            assert!(cache.get(tile0(f)).is_some());
        }
        assert_eq!(cache.idle_readers(), 3);

        // Not a descriptor problem: no drain, and the miss fails.
        cache.inject_open_error.store(2, Ordering::Relaxed); // ENOENT
        assert!(cache.get(tile0(files[3])).is_none());
        assert_eq!(cache.idle_readers(), 3, "a missing file emptied the pool");
        assert_eq!(cache.counters().errors, 1);

        #[cfg(unix)]
        {
            cache.inject_open_error.store(24, Ordering::Relaxed); // EMFILE
            let tile1 = TileId {
                tile: 1,
                ..tile0(files[3])
            };
            assert!(
                cache.get(tile1).is_some(),
                "the retry did not read the tile"
            );
            assert_eq!(cache.idle_readers(), 1, "only the retried reader is left");
            assert_eq!(cache.open_readers(), 1);
            assert_eq!(cache.counters().errors, 1, "a retried open is not an error");
        }
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    /// After a release nothing is open, and the cache still works.
    #[test]
    fn releasing_readers_closes_every_file_and_reopens_on_demand() {
        clear_microcache();
        let paths = many_fixtures("release", 3);
        let cache = TileCache::new(1024 * 1024, 256);
        let files = intern_all(&cache, &paths);
        read_everything(&cache, &files, 6, 4);
        assert!(cache.open_readers() > 0);
        cache.release_readers();
        assert_eq!(cache.open_readers(), 0);
        assert_eq!(cache.idle_readers(), 0);
        // Level 1 was never read, so this is a miss and needs a fresh open.
        let id = TileId {
            file: files[0],
            level: 1,
            tile: 0,
        };
        assert!(cache.get(id).is_some());
        assert_eq!(cache.open_readers(), 1);
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
    }

    /// Counts WARN events on the thread it is the default for.
    struct CountWarnings(Arc<std::sync::atomic::AtomicUsize>);

    impl tracing::Subscriber for CountWarnings {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() == tracing::Level::WARN {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// A file that becomes unreadable is named once at WARN, however many of
    /// its tiles then fail; the failures are all counted.
    #[test]
    fn an_unreadable_file_is_warned_about_once() {
        clear_microcache();
        let paths = many_fixtures("warn_once", 1);
        let cache = TileCache::new(1024 * 1024, 256);
        let file = intern_all(&cache, &paths)[0];
        let id = |tile| TileId {
            file,
            level: 0,
            tile,
        };
        assert!(cache.get(id(0)).is_some());
        cache.release_readers();
        let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());

        let warnings = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        tracing::subscriber::with_default(CountWarnings(warnings.clone()), || {
            for tile in 1..6 {
                assert!(
                    cache.get(id(tile)).is_none(),
                    "tile {tile} of a deleted file"
                );
            }
        });
        assert_eq!(warnings.load(Ordering::Relaxed), 1);
        assert_eq!(cache.counters().errors, 5);
    }

    /// Nothing about a broken file may panic — `Texture2D::eval` forbids it and
    /// `panic = "abort"` makes it fatal to the process.
    #[test]
    fn failures_return_none_rather_than_panicking() {
        clear_microcache();
        let path = fixture("broken", 128, 128);
        let tf = TiledFile::open(&path).expect("open");
        let cache = TileCache::new(1024 * 1024, crust_core::DEFAULT_TEX_MAX_OPEN_FILES);
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
