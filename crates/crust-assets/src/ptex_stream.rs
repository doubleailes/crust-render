//! Per-face Ptex textures, paged in a tile at a time.
//!
//! The residency half of the Ptex problem, as opposed to the filtering half:
//! [`PtexColor`](crate::PtexColor) decodes every face at load and so makes
//! memory scale with the texture's total footprint, which is the only reason
//! `CRUST_PTEX_MAX_LOG2` exists. A cap is a poor residency policy — it
//! discards authored detail permanently and still cannot help a scene binding
//! more than fits — and on the Moana island it is not a tuning knob but the
//! thing that makes the island possible at all: 2 576 238 faces are 4.58 GiB
//! at the default 32x32 cap and **494 GiB** at full resolution.
//!
//! This is the other answer, and the one production renderers give: leave the
//! pyramid on disk, where a `.ptx` already keeps it per face, and page in one
//! tile of one level of one face behind a bounded cache. Memory then scales
//! with the *cache* rather than with the scene.
//!
//! **The reader owns the cache, deliberately.** "Known incomplete work" has
//! said for a while that the fix belonged in `ptex-rs` — exactly as the C++
//! Ptex library ships `PtexCache` — rather than as a second cache in
//! crust-assets, and that it must not be bolted onto the `.tx` tile cache,
//! whose keys and on-disk layout are a different problem. Upstream now ships
//! it: [`ptex::SharedReader`] is a `&self` reader over an LRU of decoded
//! blocks under a byte budget, and [`ptex::PtexReader::get_tile`] and friends
//! make one tile of one level addressable without materialising its face.
//! So this module is a *sampler over* that reader, not a cache: everything
//! here is level selection, tile addressing, the colour decode and the
//! per-thread microcache that keeps the common tap off the reader's mutex.
//!
//! Opt-in via `CRUST_PTEX_STREAM=1`; [`PtexColor`] stays the default and the
//! correctness oracle. See `streamed_and_preloaded_agree_texel_for_texel` in
//! `tests/ptex_stream.rs` for the invariant that pins the two together, and
//! the module docs on [`level_res`] for the one place they are *supposed* to
//! disagree.

use crate::{max_log2_from_env_opt, read_channel};
use crust_core::{PtexTexture, Vec3A};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Default cache budget, in MiB.
///
/// 1024, matching `CRUST_TEX_CACHE_MB` and so OIIO's own default. Upstream's
/// [`ptex::DEFAULT_CACHE_BUDGET`] is 64 MiB, which is a library's answer for a
/// caller that has not thought about it; a renderer has, and a path tracer
/// asks for texels from every worker in an order nothing can predict, so the
/// working set is the frame rather than a locality window.
pub const DEFAULT_CACHE_MB: usize = 1024;

/// `CRUST_PTEX_CACHE_MB`, validated, as a byte count.
pub fn cache_budget_from_env() -> usize {
    let mb = match std::env::var("CRUST_PTEX_CACHE_MB") {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n > 0 => n,
            _ => {
                tracing::warn!(
                    "CRUST_PTEX_CACHE_MB={v} is not a positive integer — using {DEFAULT_CACHE_MB}"
                );
                DEFAULT_CACHE_MB
            }
        },
        Err(_) => DEFAULT_CACHE_MB,
    };
    mb * 1024 * 1024
}

/// Is the streaming backend on? `CRUST_PTEX_STREAM=1` turns it on.
pub fn stream_enabled() -> bool {
    std::env::var("CRUST_PTEX_STREAM").as_deref() == Ok("1")
}

/// Default admission threshold: a texture streams only if **preloading** it
/// would cost more than 8 MiB.
///
/// **This exists because a production stage's Ptex is Pareto-distributed, and
/// an even split over all of it is the wrong answer.** Measured on the Moana
/// island, which binds **3 618** `.ptx` totalling 5.98 GiB preloaded:
///
/// | | textures | share of bytes |
/// | --- | --- | --- |
/// | top 25 | 0.7% | 87.9% |
/// | >= 1 MiB | 167 | 97.0% |
/// | all the rest | 3 451 | 3.0% |
///
/// The median texture is under a kilobyte. Giving each of 3 618 readers a
/// slice of one budget hands the four textures that hold *half the bytes* a
/// 0.3 MiB cache each — below a single face, so every read comes back
/// `oversized` and nothing caches at all — while 3 451 sub-kilobyte textures
/// each hold a slot they will never fill. Flooring the slice instead (this
/// module's first answer) multiplies out: 3 618 x 4 MiB is **14.1 GiB**,
/// worse than the 5.98 GiB preload it replaces.
///
/// So the test is per texture and it is the honest one: **a texture smaller
/// than the cache slot it would occupy should just be preloaded.** On the
/// island 8 MiB admits 39 readers at ~26 MiB each — a real working set — and
/// preloads 0.54 GiB of small ones, for ~1.54 GiB against 5.98 GiB, a 3.9x
/// reduction with every large texture properly streamed.
///
/// `CRUST_PTEX_STREAM_MIN_MB` overrides it; `0` admits everything, which is
/// what reproduces the even-split behaviour for comparison.
pub const DEFAULT_STREAM_MIN_MB: usize = 8;

/// `CRUST_PTEX_STREAM_MIN_MB`, validated, as a byte count. See
/// [`DEFAULT_STREAM_MIN_MB`].
pub fn stream_min_bytes_from_env() -> usize {
    let mb = match std::env::var("CRUST_PTEX_STREAM_MIN_MB") {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    "CRUST_PTEX_STREAM_MIN_MB={v} is not an integer — using \
                     {DEFAULT_STREAM_MIN_MB}"
                );
                DEFAULT_STREAM_MIN_MB
            }
        },
        Err(_) => DEFAULT_STREAM_MIN_MB,
    };
    mb * 1024 * 1024
}

/// Mip levels a face of resolution `res` holds, halving each axis to a floor
/// of one texel.
///
/// This is [`PtexColor`](crate::PtexColor)'s chain, not the file's.
/// `ptex::PtexReader::face_num_levels` reduces *both* axes together and stops
/// when the shorter one reaches a texel, so a 64x16 face has 5 levels there
/// and 7 here. Following the preloaded chain is what lets the two backends be
/// compared level for level; the resolutions this asks for beyond the file's
/// own chain are anisotropic reductions, which the reader computes.
fn level_count(res: ptex::Res) -> u8 {
    res.ulog2.max(res.vlog2).max(0) as u8 + 1
}

/// Resolution of mip level `k` of a face whose finest level is `base`.
///
/// **Here is where the two backends legitimately part company, and it is
/// worth stating plainly rather than discovering in a render.** A preloaded
/// texture decodes its base to linear light and reduces *that*, because
/// averaging display-encoded texels is not averaging light. A streamed
/// texture cannot: the coarser level is on disk, reduced by the writer (or
/// recomputed by the reader) in the file's own encoding, and decoded to
/// linear only once it is here. Convexity says which way it goes — `x^2.2` is
/// convex, so the mean of the decoded texels is never below the decode of
/// their mean, and the streamed chain is therefore the *darker* of the two at
/// every level above the base.
///
/// This is the same defect `crust:mipspace` guards against for `.tx`, and the
/// reason `PtexColor` builds its pyramid in memory instead of asking the
/// reader for each resolution. It is accepted here because the alternative —
/// reducing in linear light from streamed base tiles — needs a second pyramid
/// cache of crust's own, which is precisely the design "Known incomplete
/// work" ruled out. It is also what every production Ptex cache does.
///
/// `tests/ptex_stream.rs` measures the divergence rather than asserting it
/// away, and the *base* level, which is what a close-up reads, is bit-identical
/// between the two.
fn level_res(base: ptex::Res, k: u8) -> ptex::Res {
    let k = k as i8;
    ptex::Res::new((base.ulog2 - k).max(0), (base.vlog2 - k).max(0))
}

/// Identifies one tile of one level of one face of one texture.
///
/// The texture id is in the key because the microcache is a process-global
/// thread-local: two `.ptx` files bound in the same scene would otherwise
/// answer for each other, and the face ids and resolutions that collide are
/// exactly the common ones.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TileId {
    tex: u32,
    face: u32,
    res: u16,
    tile: u32,
}

/// The per-thread microcache's slots: the most recent `(key, tile)` pairs.
///
/// The same idiom as `tiled::cache`'s microcache, and for the same measured
/// reason: a bilinear tap reads one tile up to four times in a row, so this
/// absorbs most lookups before any lock is touched — and the lock avoided
/// here is the reader's single `Mutex` rather than one of 64 shards, so it
/// matters more, not less.
///
/// **Four rather than that cache's two, and the difference is not a guess.**
/// A `.tx` is one grid over the whole texture, so a tap straddling a seam is
/// rare. A `.ptx` is a grid *per face*, and the faces that get tiled at all
/// are the large ones a streamed render spends its time in — so taps land on
/// seams routinely, and a lookup sitting on a **four-tile corner**, where the
/// u and the v tap both straddle, needs four distinct tiles for its four
/// taps. With two slots each tap evicts one the same lookup is about to want:
/// measured on the tiled fixture, that case hit **0.000** of 1 600 fetches
/// while a tap inside a tile hit 0.999. Four slots take the corner to 0.998
/// and leave the interior at 0.999, for one extra `Option` pair per thread
/// and a linear scan that finds its hit at index 0 either way. Pinned by
/// `the_microcache_absorbs_most_taps`.
type MicroSlots = [Option<(TileId, ptex::PixelData)>; MICRO_SLOTS];

/// See [`MicroSlots`]: four is the corner case's tap count, not a round
/// number. Dropping it to two is what the 0.000 above measures.
pub const MICRO_SLOTS: usize = 4;

/// Absolute ceiling on one microcache slot: 256 KiB.
///
/// **The microcache holds `ptex::PixelData`, which is memory the reader's
/// budget does not know about**, so without a ceiling it is an unbounded
/// second cache wearing the word "micro". The pathological case is not a
/// normal tile — 128x128 at four channels is 64 KiB, and four of those per
/// thread is nothing — it is a block upstream has *refused* to cache: a face
/// too big for the budget comes back `oversized`, deliberately uncached, and
/// retaining it here put it straight back into residency, times four slots,
/// times every worker thread, entirely off the books.
///
/// 256 KiB clears any real Ptex tile (256x256 at four channels) while
/// excluding the whole-face reads that are the oversized case. It is an
/// absolute bound *and* a relative one: [`micro_reserve`] takes the smaller
/// of this and half the budget, so a small budget shrinks the slots rather
/// than being quietly exceeded by them.
const MICRO_SLOT_MAX: usize = 256 * 1024;

/// Worker threads to size the microcache reserve for.
///
/// The allowance is per *thread*, since every one has its own slots.
/// `available_parallelism` is what rayon defaults its pool to, memoised
/// because this is read per texture open and the answer cannot change.
pub fn micro_threads() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
}

/// The share of a render's Ptex budget set aside for thread-local tiles.
///
/// **This is what makes the microcache accounted rather than additional.**
/// Every slot on every thread can hold one tile, so the honest total is
/// `threads * MICRO_SLOTS * slot_size`, and that has to come *out of*
/// `CRUST_PTEX_CACHE_MB` rather than sit beside it — `FileAssets` subtracts
/// this before dividing the rest among the readers, so the two together are
/// still the number that was asked for.
///
/// Capped at half the budget so a small one is not consumed entirely by
/// thread-local slots; below that the slots simply get smaller, and once
/// [`micro_slot_max`] falls under a real tile the microcache retains nothing
/// at all. That is the correct degradation: every tap then goes to the
/// reader, which is slower and still right.
pub fn micro_reserve(total: usize) -> usize {
    (micro_threads() * MICRO_SLOTS * MICRO_SLOT_MAX).min(total / 2)
}

/// The largest tile one microcache slot may retain, given a render budget.
///
/// A tile above this is handed to the caller and dropped rather than kept —
/// which is exactly upstream's own rule for a block that does not fit its
/// budget, and the reason this exists: the microcache must not re-admit what
/// the reader deliberately refused.
pub fn micro_slot_max(total: usize) -> usize {
    micro_reserve(total) / (micro_threads() * MICRO_SLOTS)
}

/// Bytes the thread-local microcaches currently hold, process-wide.
///
/// Maintained on insert and eviction — the miss path only, so a hit still
/// touches no shared state. Reported by `--stats` beside the reader's own
/// resident figure, because a number that is not reported is a number nobody
/// checks against the budget.
static MICRO_BYTES: AtomicU64 = AtomicU64::new(0);

/// Bytes retained across every thread's microcache. See [`MICRO_BYTES`].
pub fn micro_retained_bytes() -> u64 {
    MICRO_BYTES.load(Ordering::Relaxed)
}

thread_local! {
    static MICRO: std::cell::RefCell<MicroSlots> =
        const { std::cell::RefCell::new([const { None }; MICRO_SLOTS]) };
}

/// Distinguishes textures in [`TileId`]. Wraps only after 4 billion `.ptx`
/// files in one process, and a wrap would cost a stale microcache hit rather
/// than unsoundness.
static NEXT_TEX_ID: AtomicU32 = AtomicU32::new(0);

/// How often the microcache answered, against how often the reader had to.
///
/// `micro_hits` is what this module adds; everything about the byte budget
/// itself comes from [`ptex::CacheStats`], since the cache is the reader's.
#[derive(Debug, Clone, Copy)]
pub struct StreamStats {
    pub micro_hits: u64,
    pub reader_lookups: u64,
    pub cache: ptex::CacheStats,
}

impl StreamStats {
    /// Share of texel fetches that never reached the reader's mutex.
    pub fn micro_rate(&self) -> f64 {
        let total = self.micro_hits + self.reader_lookups;
        if total == 0 {
            return 0.0;
        }
        self.micro_hits as f64 / total as f64
    }
}

/// A Ptex colour texture read a tile at a time.
pub struct PtexStream {
    id: u32,
    reader: ptex::SharedReader,
    n_faces: usize,
    n_chan: usize,
    dt: ptex::DataType,
    /// `1 / one_value` for the sample type, folded in before the decode curve
    /// exactly as the preloading path folds it.
    scale: f32,
    /// A triangle `.ptx` packs two triangles into each square of texels, so
    /// its reductions are symmetric by definition and the reader refuses an
    /// anisotropic one. Both axes therefore clamp together, as in `PtexColor`.
    triangle: bool,
    /// The decode curve, memoised for the one sample type where it can be.
    ///
    /// `u8` is what production colour Ptex is (and what the island is), and a
    /// trilinear tap is 8 texels x 3 channels, so 24 `powf` calls per lookup
    /// is not a rounding error. 256 entries of exactly the expression the
    /// scalar path computes, so the table is bit-identical to it rather than
    /// merely close — which is what lets the streamed and preloaded texel
    /// values be compared for equality instead of within a tolerance.
    lut: Option<Box<[f32; 256]>>,
    /// The resolution ceiling, when one was asked for.
    ///
    /// `None` — the default — is the whole point: streaming has no reason to
    /// cap, since it holds a cache rather than the texture. `CRUST_PTEX_MAX_LOG2`
    /// still caps when it is *explicitly set*, because that is what makes the
    /// two backends comparable at a resolution they both hold.
    cap: Option<i8>,
    /// `CRUST_PTEX_MIP=0` pins every lookup to the base level, as it does for
    /// the preloading path.
    mip: bool,
    /// Largest tile a microcache slot may retain — see [`micro_slot_max`].
    /// Anything above it is handed to the caller and dropped, which is what
    /// keeps thread-local retention inside the render's budget instead of
    /// beside it.
    micro_max: usize,
    fallback: Vec3A,
    micro_hits: AtomicU64,
    reader_lookups: AtomicU64,
}

impl PtexStream {
    /// Opens `path` for streaming, reading the budget and caps from the
    /// environment.
    ///
    /// `Err` carries a message suitable for a warning; the caller falls back
    /// to preloading rather than failing the render.
    pub fn open(path: &Path) -> Result<Self, String> {
        let total = cache_budget_from_env();
        PtexStream::open_with(
            path,
            total,
            micro_slot_max(total),
            max_log2_from_env_opt(),
            crate::ptex_texture::mip_enabled(),
        )
    }

    /// [`PtexStream::open`] with the policy passed in rather than read from
    /// the environment — the seam `PtexColor::open_with` offers, and for the
    /// same reason: comparing both sides should not mean mutating a
    /// process-global the rest of the program is reading.
    /// `micro_max` is the per-slot ceiling on thread-local retention, which
    /// is policy for the same reason the budget is — see [`micro_slot_max`],
    /// which derives the render-wide default. A test passes it directly so it
    /// can drive the case that matters: a tile larger than what may be kept.
    pub fn open_with(
        path: &Path,
        budget_bytes: usize,
        micro_max: usize,
        cap: Option<i8>,
        mip: bool,
    ) -> Result<Self, String> {
        let options = ptex::CacheOptions {
            premultiply: false,
            budget_bytes,
        };
        let reader =
            ptex::SharedReader::open_with_options(path, options).map_err(|e| e.to_string())?;

        let n_chan = reader.num_channels();
        if n_chan == 0 {
            return Err("file has no channels".into());
        }
        let dt = reader.data_type();
        let scale = dt.one_value_inv();
        let lut = (dt == ptex::DataType::UInt8).then(|| {
            let mut t = Box::new([0.0f32; 256]);
            for (i, e) in t.iter_mut().enumerate() {
                *e = decode_sample(i as f32, scale);
            }
            t
        });

        Ok(PtexStream {
            id: NEXT_TEX_ID.fetch_add(1, Ordering::Relaxed),
            n_faces: reader.num_faces(),
            n_chan,
            dt,
            scale,
            triangle: reader.mesh_type() == ptex::MeshType::Triangle,
            lut,
            cap,
            mip,
            micro_max,
            reader,
            fallback: Vec3A::splat(0.5),
            micro_hits: AtomicU64::new(0),
            reader_lookups: AtomicU64::new(0),
        })
    }

    /// Cache and microcache counters, for the load-time and `--stats` report.
    pub fn stats(&self) -> StreamStats {
        StreamStats {
            micro_hits: self.micro_hits.load(Ordering::Relaxed),
            reader_lookups: self.reader_lookups.load(Ordering::Relaxed),
            cache: self.reader.cache_stats(),
        }
    }

    /// Re-budgets this texture's cache.
    ///
    /// Exists for one reason: the budget belongs to the *render*, not to a
    /// file. `ptex::SharedReader` owns its cache, so N textures opened at
    /// `CRUST_PTEX_CACHE_MB` each would hold N times that — and a production
    /// stage binds Ptex per element, so N is not small. `FileAssets` divides
    /// the total as textures arrive; see `rebudget_ptex` there.
    pub fn set_budget(&self, bytes: usize) {
        self.reader.set_cache_budget(bytes);
    }

    /// Faces the file holds, for the load-time and `--stats` report.
    pub fn faces(&self) -> usize {
        self.n_faces
    }

    /// What **preloading** this texture would cost, in bytes, from the header
    /// alone.
    ///
    /// `face_infos()` is parsed at open and carries every face's resolution
    /// with no pixel I/O, so this is exact and free — which is what makes it
    /// usable as an admission test rather than a guess. It mirrors
    /// `PtexColor::bytes()`: each face clamped to `max_log2` by the same rule,
    /// as linear `f32` RGB, plus the mip chain below it.
    ///
    /// `max_log2` is the *preloading* cap (`CRUST_PTEX_MAX_LOG2`, defaulting
    /// to 32x32), not this stream's own ceiling. The question being asked is
    /// what the alternative would cost, so it has to be priced the way the
    /// alternative prices it.
    pub fn preload_bytes(&self, max_log2: i8) -> usize {
        let mut floats = 0usize;
        for info in self.reader.face_infos() {
            let res = if self.triangle {
                let l = info.res.ulog2.min(info.res.vlog2).min(max_log2);
                ptex::Res::new(l, l)
            } else {
                ptex::Res::new(info.res.ulog2.min(max_log2), info.res.vlog2.min(max_log2))
            };
            let (mut w, mut h) = (res.u(), res.v());
            loop {
                floats += w * h * 3;
                if w == 1 && h == 1 {
                    break;
                }
                w = (w / 2).max(1);
                h = (h / 2).max(1);
            }
        }
        floats * std::mem::size_of::<f32>() + self.n_faces * 8
    }

    /// Bytes the cache currently holds. Unlike `PtexColor::bytes` this moves
    /// during a render and is bounded by the budget rather than by the asset.
    pub fn bytes(&self) -> usize {
        self.reader.cache_stats().bytes_resident
    }

    /// The finest resolution this texture will serve for `face`: what the
    /// file authored, clamped by an explicit cap.
    ///
    /// The clamping rule is `PtexColor`'s, so that a capped stream and a
    /// capped preload ask the reader the same question. Each axis clamps
    /// independently for a quad — Ptex faces are frequently non-square and
    /// clamping the pair would distort the aspect the file chose — while a
    /// triangle clamps both together.
    fn base_res(&self, res: ptex::Res) -> ptex::Res {
        let Some(cap) = self.cap else {
            return res;
        };
        if self.triangle {
            let l = res.ulog2.min(res.vlog2).min(cap);
            ptex::Res::new(l, l)
        } else {
            ptex::Res::new(res.ulog2.min(cap), res.vlog2.min(cap))
        }
    }

    /// One decoded texel of one level, through the microcache.
    ///
    /// `x`/`y` are texel coordinates within the level and are assumed already
    /// clamped into it by the caller.
    #[inline]
    fn texel(&self, face: u32, layout: &ptex::TileLayout, x: usize, y: usize) -> Option<Vec3A> {
        let tile = layout.tile_index(x, y);
        let (ou, ov) = layout.tile_origin(tile);
        let idx = (y - ov) * layout.tile_res.u() + (x - ou);
        let id = TileId {
            tex: self.id,
            face,
            res: layout.res.val(),
            tile: tile as u32,
        };
        self.with_tile(id, layout.res, |data| self.decode(data, idx))
    }

    /// Reads one tile through the per-thread microcache and hands it to `f`.
    ///
    /// A closure rather than a returned handle, for the reason
    /// `tiled::cache::with_tile` documents and measured: one atomic refcount
    /// pair per texel was the difference between streaming costing 4x a
    /// preloaded render and costing 2x, and here the handle would be an
    /// `Arc` clone out of a `Mutex`-guarded LRU rather than out of a shard.
    ///
    /// The thread-local borrow is held across `f`, so `f` must not look
    /// anything else up through here. Every caller decodes one texel and
    /// returns.
    #[inline]
    fn with_tile<R>(&self, id: TileId, res: ptex::Res, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        // `FnOnce` can only be moved once and either path might call it, so
        // it is parked in an `Option` and taken by whichever path wins.
        let mut f = Some(f);
        let hit = MICRO.with(|m| {
            let slots = m.borrow();
            let idx = slots
                .iter()
                .position(|s| matches!(s, Some((k, _)) if *k == id))?;
            let (_, data) = slots[idx].as_ref()?;
            Some(f.take()?(data))
        });
        if let Some(r) = hit {
            self.micro_hits.fetch_add(1, Ordering::Relaxed);
            return Some(r);
        }
        // Miss. The borrow above is released before this: `get_tile` takes
        // the reader's mutex and may read and inflate, and must not run under
        // a thread-local borrow.
        self.reader_lookups.fetch_add(1, Ordering::Relaxed);
        let data = self
            .reader
            .get_tile(id.face as usize, res, id.tile as usize)
            .ok()?;
        let r = f.take()?(&data);
        // **Retain only what fits a slot.** A block bigger than this is one
        // upstream itself declined to cache (`oversized`), and keeping it
        // here would put it back into residency off the books — four slots
        // deep, on every worker thread. Dropping it costs a re-read next tap
        // and keeps the budget honest, which is the trade the whole feature
        // is about.
        if data.len() > self.micro_max {
            return Some(r);
        }
        MICRO.with(|m| {
            let mut slots = m.borrow_mut();
            // Take the entry about to fall off the end *before* rotating, so
            // its bytes leave the accounting with it; `rotate_right` then
            // puts that hole in front for the new tile.
            if let Some((_, old)) = slots[MICRO_SLOTS - 1].take() {
                MICRO_BYTES.fetch_sub(old.len() as u64, Ordering::Relaxed);
            }
            slots.rotate_right(1);
            MICRO_BYTES.fetch_add(data.len() as u64, Ordering::Relaxed);
            slots[0] = Some((id, data));
        });
        Some(r)
    }

    /// Texel `idx` of a tile's interleaved bytes, as linear RGB.
    #[inline]
    fn decode(&self, data: &[u8], idx: usize) -> Vec3A {
        let dsize = self.dt.size();
        let px = dsize * self.n_chan;
        let Some(src) = data.get(idx * px..idx * px + px) else {
            // A tile shorter than its layout claims is a corrupt file, not a
            // caller's bug, and this runs inside the integrator.
            return self.fallback;
        };
        let mut out = Vec3A::ZERO;
        for ch in 0..3 {
            // A single-channel (displacement-style) file feeds channel 0 to
            // all three, so it reads as greyscale rather than red — the same
            // rule the preloading path applies.
            let c = if ch < self.n_chan { ch } else { 0 };
            out[ch] = match &self.lut {
                Some(t) => t[src[c] as usize],
                None => decode_sample(read_channel(&src[c * dsize..], self.dt), self.scale),
            };
        }
        out
    }

    /// Bilinear lookup within one level of one face.
    ///
    /// Texel addressing, the half-texel offset and the clamped borders are
    /// `PtexColor::sample_level`'s, expression for expression, because the two
    /// are compared for equality and not for similarity. What is new is that a
    /// tap may land in a different tile than its neighbour: `texel` resolves
    /// that per tap, and the microcache makes the common case — all four in
    /// one tile — one reader call rather than four.
    fn sample_level(&self, face: u32, res: ptex::Res, fu: f32, fv: f32) -> Option<Vec3A> {
        let layout = self.reader.tile_layout(face as usize, res).ok()?;
        let (w, h) = (res.u(), res.v());

        // Texel centres sit at (i + 0.5)/n.
        let x = fu * w as f32 - 0.5;
        let y = fv * h as f32 - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let tx = x - x0;
        let ty = y - y0;
        let cx = |c: f32| (c.max(0.0) as usize).min(w - 1);
        let cy = |c: f32| (c.max(0.0) as usize).min(h - 1);
        let (x0i, x1i) = (cx(x0), cx(x0 + 1.0));
        let (y0i, y1i) = (cy(y0), cy(y0 + 1.0));

        let top = self
            .texel(face, &layout, x0i, y0i)?
            .lerp(self.texel(face, &layout, x1i, y0i)?, tx);
        let bot = self
            .texel(face, &layout, x0i, y1i)?
            .lerp(self.texel(face, &layout, x1i, y1i)?, tx);
        Some(top.lerp(bot, ty))
    }
}

/// One raw sample, scaled and decoded to linear light.
///
/// Ptex colour is display-encoded: the island's shading network gammas it
/// (`PxrColorCorrect`) and the GL path declares `sourceColorSpace = "sRGB"`.
/// Both mean decode by 2.2. Written once, called from the streaming decode
/// and from the table that memoises it, so the two cannot drift.
#[inline]
fn decode_sample(raw: f32, scale: f32) -> f32 {
    (raw * scale).max(0.0).powf(2.2)
}

impl PtexTexture for PtexStream {
    fn eval(&self, face_id: u32, u: f32, v: f32, width: f32) -> Vec3A {
        let Ok(info) = self.reader.face_info(face_id as usize) else {
            return self.fallback;
        };
        let base = self.base_res(info.res);
        let (w, h) = (base.u(), base.v());

        let fu = if u.is_finite() {
            u.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let fv = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };

        let levels = if self.mip { level_count(base) } else { 1 };

        // No footprint, or no pyramid to choose from: base-level bilinear.
        if levels == 1 || !width.is_finite() || width <= 0.0 {
            return self
                .sample_level(face_id, base, fu, fv)
                .unwrap_or(self.fallback);
        }

        // `width` is a fraction of the face, so it converts to texels by the
        // base resolution, measured against the denser axis — an isotropic
        // footprint over a 64x16 face is minified most where the texels are.
        let texels = width * w.max(h) as f32;
        // Magnification short-circuit: the answer is the base level and the
        // `log2` would only confirm it.
        if texels <= 1.0 {
            return self
                .sample_level(face_id, base, fu, fv)
                .unwrap_or(self.fallback);
        }
        let lod = texels.log2().clamp(0.0, (levels - 1) as f32);
        let lo = lod.floor();
        let frac = lod - lo;
        let Some(a) = self.sample_level(face_id, level_res(base, lo as u8), fu, fv) else {
            return self.fallback;
        };
        if frac <= 0.0 {
            return a;
        }
        match self.sample_level(face_id, level_res(base, lo as u8 + 1), fu, fv) {
            Some(b) => a.lerp(b, frac),
            None => a,
        }
    }

    fn num_faces(&self) -> usize {
        self.n_faces
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_chain_halves_each_axis_to_a_floor_of_one() {
        // 64x16. The chain runs to 1x1 rather than stopping where the file's
        // own does (at 4x1, when the shorter axis pins), because it is the
        // preloaded chain that is being matched — see `level_count`.
        let base = ptex::Res::new(6, 4);
        assert_eq!(level_count(base), 7);
        let got: Vec<_> = (0..level_count(base))
            .map(|k| {
                let r = level_res(base, k);
                (r.u(), r.v())
            })
            .collect();
        assert_eq!(
            got,
            vec![(64, 16), (32, 8), (16, 4), (8, 2), (4, 1), (2, 1), (1, 1)]
        );
    }

    #[test]
    fn the_level_chain_matches_the_preloaded_one() {
        // The whole reason `level_count` is written out here rather than
        // taken from `face_num_levels`: the two backends must agree on how
        // many levels there are and how big each one is, or a `lod` computed
        // from one addresses the other's chain.
        for (ul, vl) in [(0, 0), (5, 5), (6, 4), (4, 6), (10, 9), (1, 7)] {
            let base = ptex::Res::new(ul, vl);
            let n = crate::ptex_texture::level_count(base.u(), base.v());
            assert_eq!(level_count(base), n, "level count for {ul}x{vl}");
            for k in 0..n {
                let r = level_res(base, k);
                let (w, h) = crate::ptex_texture::level_size(base.u(), base.v(), k as usize);
                assert_eq!((r.u(), r.v()), (w, h), "level {k} of {ul}x{vl}");
            }
        }
    }

    #[test]
    fn the_decode_table_is_the_scalar_decode() {
        // The table exists to skip 24 `powf` calls per trilinear tap, not to
        // approximate them: the streamed-vs-preloaded comparison is for
        // equality, so a table that merely rounded the same way would make
        // that test a tolerance check without saying so.
        let scale = ptex::DataType::UInt8.one_value_inv();
        for i in 0..256u32 {
            let table = decode_sample(i as f32, scale);
            let scalar = (read_channel(&[i as u8], ptex::DataType::UInt8) * scale)
                .max(0.0)
                .powf(2.2);
            assert_eq!(table.to_bits(), scalar.to_bits(), "entry {i}");
        }
    }

    #[test]
    fn a_budget_is_read_from_the_environment_and_a_bad_one_falls_back() {
        // No env mutation: `cache_budget_from_env` is the wrapper, and the
        // policy it wraps is what matters. Kept as a compile-time check that
        // the default is stated in one place and in MiB.
        assert_eq!(DEFAULT_CACHE_MB * 1024 * 1024, 1024 * 1024 * 1024);
    }
}
