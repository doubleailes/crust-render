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
//! On by default for files large enough to be worth it (see
//! [`DEFAULT_STREAM_MIN_MB`]); [`PtexColor`] stays the correctness oracle.
//! See `streamed_and_preloaded_agree_texel_for_texel` in `tests/ptex_stream.rs`
//! for the invariant that pins the two together.
//!
//! **The mip chain is where they could part company**, and [`MipSpace`] says
//! how each policy answers that. The default, `capped`, makes the two agree
//! wherever the preload holds texels at all: the preload's own base is the
//! file's level at the cap, so the stream reads the file at and above the cap
//! and *derives* the levels below it from the cap level, through the
//! preload's own decode and reduction (`CappedLevels`), held in the reader's
//! cache under its budget.

use crate::error::AssetError;
use crate::mip_filter::{MipSource, Taps, trilinear};
use crate::ptex_texture::{
    LevelReduction, capped_res, decode_face, level_reduction, ptex_space, reduce_level,
};
use crate::read_channel;
use crust_core::{ColorSpace, PtexTexture, Vec3A};
use std::path::Path;

use crate::texture_cache::{Ways, mib_to_bytes};
use std::sync::atomic::{AtomicU32, Ordering};

/// Default cache budget, in MiB.
///
/// 1024, matching `CRUST_TEX_CACHE_MB` and so OIIO's own default. Upstream's
/// [`ptex::DEFAULT_CACHE_BUDGET`] is 64 MiB, which is a library's answer for a
/// caller that has not thought about it; a renderer has, and a path tracer
/// asks for texels from every worker in an order nothing can predict, so the
/// working set is the frame rather than a locality window.
pub const DEFAULT_CACHE_MB: usize = crust_core::config::DEFAULT_CACHE_MB;

/// The render's whole Ptex budget `config` asks for (`CRUST_PTEX_CACHE_MB`),
/// in bytes.
pub fn budget_bytes(config: &crust_core::Config) -> usize {
    mib_to_bytes(config.ptex_cache_mb.get() as u64) as usize
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
pub const DEFAULT_STREAM_MIN_MB: usize = crust_core::config::DEFAULT_PTEX_STREAM_MIN_MB;

/// The admission threshold `config` asks for (`CRUST_PTEX_STREAM_MIN_MB`), in
/// bytes. See [`DEFAULT_STREAM_MIN_MB`].
pub(crate) fn stream_min_bytes(config: &crust_core::Config) -> usize {
    mib_to_bytes(config.ptex_stream_min_mb as u64) as usize
}

/// Which mip chain a streamed texture is allowed to read — the reasoning is
/// on [`crust_core::PtexMipSpace`], which `CRUST_PTEX_STREAM_MIPSPACE` parses
/// into.
pub use crust_core::PtexMipSpace as MipSpace;

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
/// **The level this names comes off disk already reduced, and that is the
/// one place the two backends cannot be made to agree.** A preloaded texture
/// decodes its base to linear light and reduces *that*, because averaging
/// display-encoded texels is not averaging light. A streamed texture cannot:
/// the coarser level was reduced by the writer (or is recomputed by the
/// reader) in the file's own encoding, and is decoded to linear only once it
/// is here. Convexity says which way it goes — `x^2.2` is convex, so the mean
/// of the decoded texels is never below the decode of their mean, and the
/// streamed chain is therefore the *darker* of the two at every level above
/// the base.
///
/// This is the same defect `crust:mipspace` guards against for `.tx`, and
/// there it is **refused** rather than described — a mismatched chain reads
/// as perfectly correct at level 0 and wrong only under minification, which
/// by eye is indistinguishable from a filtering bug. So it is refused here
/// too: see [`MipSpace`], which declines to stream a texture whose chain
/// would be read this way and preloads it instead. This function is reached
/// only under `CRUST_PTEX_STREAM_MIPSPACE=file`, the explicit opt-in that
/// takes the file's chain and the residency that comes with it.
///
/// `tests/ptex_stream.rs` measures the divergence rather than asserting it
/// away, and the *base* level, which is what the refusal preserves and what a
/// close-up reads, is bit-identical between the two.
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

/// The [`TileId::tile`] of a derived block (`capped`), which is a whole level
/// of a face rather than a tile of one. A level's resolution names it within
/// the face, and no file tile index reaches this.
const DERIVED: u32 = u32::MAX;

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
type MicroSlots = Ways<TileId, ptex::PixelData, MICRO_SLOTS>;

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

/// Threads to size the per-thread microcaches' allowances for: this one and
/// the `.tx` cache's `micro_share`.
///
/// The allowance is per *thread*, since every one has its own slots, so the
/// count has to cover every thread that looks a texture up, or their slots
/// together pass the bound they were sized to. That is the render pool —
/// `RAYON_NUM_THREADS` when it is set, which may exceed the cores — or the
/// cores if more, plus one for the thread that drives the pool. Memoised:
/// this is read per texture open, and the pool is built once.
pub fn micro_threads() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        cores.max(rayon::current_num_threads()) + 1
    })
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
///
/// Striped, like the hit counters, rather than one atomic every thread
/// writes on every insert and eviction: each thread adds and removes only its
/// own microcache's bytes, so every stripe stays non-negative and their sum
/// is the total.
static MICRO_BYTES: std::sync::LazyLock<crate::tiled::StripedCounter> =
    std::sync::LazyLock::new(Default::default);

/// Bytes retained across every thread's microcache. See [`MICRO_BYTES`].
pub fn micro_retained_bytes() -> u64 {
    MICRO_BYTES.load()
}

/// Bytes the *calling thread's* microcache holds.
///
/// [`micro_retained_bytes`] is process-wide, so anything else rendering at the
/// same time — another test in the same binary, for one — moves it too. A
/// check on what one sequence of lookups retained has to read this instead.
pub fn micro_thread_bytes() -> u64 {
    MICRO.with(|m| m.borrow().values().map(|data| data.len() as u64).sum())
}

thread_local! {
    static MICRO: std::cell::RefCell<MicroSlots> =
        const { std::cell::RefCell::new(MicroSlots::EMPTY) };
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
    /// How the stored samples decode — gamma 2.2 for colour, raw for data.
    space: ColorSpace,
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
    /// The change of primaries a table-decoded texel takes into the working
    /// space; `None` on the working primaries.
    gamut: Option<crust_core::Mat3A>,
    /// The whole conversion a texel of any other sample type takes, resolved
    /// once here rather than per tap — a lookup in the process-wide
    /// conversion cache is a lock and a hash per texel. `None` when it is the
    /// identity (raw data, a displacement map), which then costs nothing.
    conversion: Option<std::sync::Arc<crust_core::color::Conversion>>,
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
    /// Under [`MipSpace::Capped`], the preload's cap: the resolution the
    /// derived chain starts at, and the boundary between file levels (finer)
    /// and derived ones (at and coarser). `None` reads the file's chain.
    capped: Option<i8>,
    /// Largest tile a microcache slot may retain — see [`micro_slot_max`].
    /// Anything above it is handed to the caller and dropped, which is what
    /// keeps thread-local retention inside the render's budget instead of
    /// beside it.
    micro_max: usize,
    fallback: Vec3A,
    /// Striped for the reason the `.tx` cache's are (see
    /// [`crate::tiled::StripedCounter`]): bumped on every texel fetch from
    /// every thread, so a plain atomic is one cache line all of them contend
    /// on.
    micro_hits: crate::tiled::StripedCounter,
    reader_lookups: crate::tiled::StripedCounter,
}

impl PtexStream {
    /// Opens `path` for streaming, with the budget and caps of
    /// [`crust_core::config()`].
    ///
    /// `Err` says why, for a warning; the caller falls back to preloading
    /// rather than failing the render.
    pub fn open(path: &Path) -> Result<Self, AssetError> {
        PtexStream::open_config(path, crust_core::config())
    }

    /// [`PtexStream::open`] with the budget and caps of `config`.
    pub fn open_config(path: &Path, config: &crust_core::Config) -> Result<Self, AssetError> {
        PtexStream::open_config_in(path, ColorSpace::GAMMA22, config)
    }

    /// [`PtexStream::open_config`], decoding the stored samples as `space`,
    /// with the mip chain `CRUST_PTEX_STREAM_MIPSPACE` names.
    pub fn open_config_in(
        path: &Path,
        space: ColorSpace,
        config: &crust_core::Config,
    ) -> Result<Self, AssetError> {
        let total = budget_bytes(config);
        let tex = PtexStream::open_in(
            path,
            space,
            total,
            micro_slot_max(total),
            config.ptex_max_log2,
            config.ptex_mip,
        )?;
        match config.ptex_mip_space {
            MipSpace::Capped => tex.capped_at(
                path,
                config
                    .ptex_max_log2
                    .unwrap_or(crate::ptex_texture::DEFAULT_MAX_LOG2),
            ),
            MipSpace::Linear | MipSpace::File => Ok(tex),
        }
    }

    /// [`PtexStream::open_in`] under the `capped` chain, with `preload_max_log2`
    /// the preload's cap: file levels above it, derived levels at and below.
    #[allow(clippy::too_many_arguments)]
    pub fn open_capped(
        path: &Path,
        space: ColorSpace,
        budget_bytes: usize,
        micro_max: usize,
        cap: Option<i8>,
        preload_max_log2: i8,
        mip: bool,
    ) -> Result<Self, AssetError> {
        PtexStream::open_in(path, space, budget_bytes, micro_max, cap, mip)?
            .capped_at(path, preload_max_log2)
    }

    /// Installs the `capped` chain: [`CappedLevels`] on the reader, so the
    /// derived levels live in its cache and under its budget.
    fn capped_at(mut self, path: &Path, preload_max_log2: i8) -> Result<Self, AssetError> {
        let levels = CappedLevels {
            max_log2: preload_max_log2,
            triangle: self.triangle,
            dt: self.dt,
            n_chan: self.n_chan,
            space: self.space,
            reduce: level_reduction(self.triangle),
        };
        self.reader = self
            .reader
            .with_derived(std::sync::Arc::new(levels))
            .map_err(AssetError::ptex(path))?;
        self.capped = Some(preload_max_log2);
        Ok(self)
    }

    /// Is this texture read under the `capped` chain?
    pub fn is_capped(&self) -> bool {
        self.capped.is_some()
    }

    /// [`PtexStream::open`] with the policy passed in rather than read from
    /// the environment — the seam `PtexColor::open_with` offers, and for the
    /// same reason: comparing both sides should not mean mutating a
    /// process-global the rest of the program is reading.
    /// `micro_max` is the per-slot ceiling on thread-local retention, which
    /// is policy for the same reason the budget is — see [`micro_slot_max`],
    /// which derives the render-wide default. A test passes it directly so it
    /// can drive the case that matters: a tile larger than what may be kept.
    ///
    /// Decodes as colour (gamma 2.2); [`PtexStream::open_in`] takes the space.
    pub fn open_with(
        path: &Path,
        budget_bytes: usize,
        micro_max: usize,
        cap: Option<i8>,
        mip: bool,
    ) -> Result<Self, AssetError> {
        PtexStream::open_in(path, ColorSpace::GAMMA22, budget_bytes, micro_max, cap, mip)
    }

    /// [`PtexStream::open_with`], decoding the stored samples as `space`.
    pub fn open_in(
        path: &Path,
        space: ColorSpace,
        budget_bytes: usize,
        micro_max: usize,
        cap: Option<i8>,
        mip: bool,
    ) -> Result<Self, AssetError> {
        let options = ptex::CacheOptions {
            premultiply: false,
            budget_bytes,
        };
        let reader =
            ptex::SharedReader::open_with_options(path, options).map_err(AssetError::ptex(path))?;

        let n_chan = reader.num_channels();
        if n_chan == 0 {
            return Err(AssetError::unusable(path, "Ptex file has no channels"));
        }
        let dt = reader.data_type();
        let scale = dt.one_value_inv();
        let lut = (dt == ptex::DataType::UInt8).then(|| {
            let mut t = Box::new([0.0f32; 256]);
            for (i, e) in t.iter_mut().enumerate() {
                *e = decode_sample(i as f32, scale, space);
            }
            t
        });

        Ok(PtexStream {
            id: NEXT_TEX_ID.fetch_add(1, Ordering::Relaxed),
            n_faces: reader.num_faces(),
            n_chan,
            dt,
            scale,
            space,
            triangle: reader.mesh_type() == ptex::MeshType::Triangle,
            lut,
            gamut: ptex_space(space).gamut(),
            conversion: Some(ptex_space(space).conversion()).filter(|c| !c.is_identity()),
            cap,
            mip,
            capped: None,
            micro_max,
            reader,
            fallback: Vec3A::splat(0.5),
            micro_hits: Default::default(),
            reader_lookups: Default::default(),
        })
    }

    /// Cache and microcache counters, for the load-time and `--stats` report.
    pub fn stats(&self) -> StreamStats {
        StreamStats {
            micro_hits: self.micro_hits.load(),
            reader_lookups: self.reader_lookups.load(),
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

    /// Can every lookup this texture will serve be answered from level 0?
    ///
    /// The admission test for [`MipSpace::Linear`], and the reason it is a
    /// question about the *texture* rather than a flat refusal: the chain is
    /// only a problem if a coarse level can ever be read. Two cases where
    /// none can, and both are real —
    ///
    /// - `CRUST_PTEX_MIP=0` pins every lookup to the base level, so there is
    ///   no chain to be reduced in the wrong space. This is the streaming
    ///   configuration that is exact *and* unbounded, and the one the
    ///   `streamed_and_preloaded_agree_texel_for_texel` invariant covers.
    /// - Every face is a single texel on both axes once the cap is applied,
    ///   so `level_count` is 1 throughout. Degenerate, but it costs one pass
    ///   over headers already parsed to say so rather than preload a texture
    ///   that has nothing to get wrong.
    /// - The texture is read raw (a displacement map): the file's encoding
    ///   *is* the linear value, so its own reduction is the right one.
    ///
    /// - The texture reads the `capped` chain, whose levels below the cap are
    ///   the preload's own.
    ///
    /// Anything else reads a level the file reduced in its own encoding, and
    /// under `linear` is preloaded instead — see [`MipSpace`].
    pub fn chain_is_exact(&self) -> bool {
        // The `capped` chain is the preload's below the cap by construction.
        if self.capped.is_some()
            || !self.mip
            || matches!(self.space, ColorSpace::RAW | ColorSpace::AUTO)
        {
            return true;
        }
        self.reader
            .face_infos()
            .iter()
            .all(|i| level_count(self.base_res(i.res)) == 1)
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
            let res = capped_res(info.res, max_log2, self.triangle);
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
        match self.cap {
            Some(cap) => capped_res(res, cap, self.triangle),
            None => res,
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
        let fetch = || self.reader.get_tile(face as usize, layout.res, tile).ok();
        self.with_block(id, fetch, |data| self.decode(data, idx))
    }

    /// Bilinear lookup within derived level `k` (resolution `res`) of a face:
    /// [`PtexColor::sample_level`] over the same linear `f32` texels, through
    /// the microcache like a tile. A derived block is a whole level, so all
    /// four taps read one block and the microcache is touched once.
    fn sample_derived(
        &self,
        face: u32,
        k: usize,
        res: ptex::Res,
        fu: f32,
        fv: f32,
    ) -> Option<Vec3A> {
        let id = TileId {
            tex: self.id,
            face,
            res: res.val(),
            tile: DERIVED,
        };
        let (w, h) = (res.u(), res.v());
        let t = Taps::new(fu * w as f32 - 0.5, fv * h as f32 - 0.5, w, h);
        let fetch = || self.reader.get_derived(face as usize, k).ok();
        self.with_block(id, fetch, |data| {
            let at = |x: usize, y: usize| {
                let i = (y * w + x) * 12;
                let f = |o: usize| {
                    let b = data.get(i + o..i + o + 4)?;
                    Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                };
                Some(Vec3A::new(f(0)?, f(4)?, f(8)?))
            };
            let top = at(t.x0, t.y0)?.lerp(at(t.x1, t.y0)?, t.fx);
            let bot = at(t.x0, t.y1)?.lerp(at(t.x1, t.y1)?, t.fx);
            Some(top.lerp(bot, t.fy))
        })
        .flatten()
    }

    /// Reads one block — a file tile, or a derived level — through the
    /// per-thread microcache, `fetch`ing it from the reader on a miss, and
    /// hands it to `f`.
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
    fn with_block<R>(
        &self,
        id: TileId,
        fetch: impl FnOnce() -> Option<ptex::PixelData>,
        f: impl FnOnce(&[u8]) -> R,
    ) -> Option<R> {
        // `FnOnce` can only be moved once and either path might call it, so
        // it is parked in an `Option` and taken by whichever path wins.
        let mut f = Some(f);
        let hit = MICRO.with(|m| {
            let slots = m.borrow();
            let data = slots.get(&id)?;
            Some(f.take()?(data))
        });
        if let Some(r) = hit {
            self.micro_hits.add(1);
            return Some(r);
        }
        // Miss. The borrow above is released before this: `fetch` takes the
        // reader's mutex and may read, inflate or derive, and must not run
        // under a thread-local borrow.
        self.reader_lookups.add(1);
        let data = fetch()?;
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
        // The entry that falls off the end leaves the accounting with it.
        MICRO_BYTES.add(data.len() as u64);
        if let Some((_, old)) = MICRO.with(|m| m.borrow_mut().push(id, data)) {
            MICRO_BYTES.sub(old.len() as u64);
        }
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
        // A single-channel (displacement-style) file feeds channel 0 to all
        // three, so it reads as greyscale rather than red — the same rule the
        // preloading path applies.
        let c = |ch: usize| if ch < self.n_chan { ch } else { 0 };
        match &self.lut {
            Some(t) => crust_core::color::apply_gamut(
                self.gamut.as_ref(),
                Vec3A::new(
                    t[src[c(0)] as usize],
                    t[src[c(1)] as usize],
                    t[src[c(2)] as usize],
                ),
            ),
            // One decode per texel, all three channels together: the curve
            // is an OCIO processor, whose per-call cost is per pixel.
            None => {
                let raw = |ch: usize| read_channel(&src[c(ch) * dsize..], self.dt);
                let v = Vec3A::new(raw(0), raw(1), raw(2)) * self.scale;
                match &self.conversion {
                    Some(conversion) => conversion.convert(v),
                    None => v,
                }
            }
        }
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
        let t = Taps::new(fu * w as f32 - 0.5, fv * h as f32 - 0.5, w, h);
        let top = self
            .texel(face, &layout, t.x0, t.y0)?
            .lerp(self.texel(face, &layout, t.x1, t.y0)?, t.fx);
        let bot = self
            .texel(face, &layout, t.x0, t.y1)?
            .lerp(self.texel(face, &layout, t.x1, t.y1)?, t.fx);
        Some(top.lerp(bot, t.fy))
    }
}

/// One streamed face as a [`MipSource`]: `base` is its level 0 after the
/// cap, `levels` how many the pyramid holds (1 without mips).
struct StreamFace<'a> {
    tex: &'a PtexStream,
    face: u32,
    base: ptex::Res,
    levels: usize,
}

impl MipSource for StreamFace<'_> {
    type Texel = Vec3A;

    #[inline(always)]
    fn level_count(&self) -> usize {
        self.levels
    }

    /// `width` is a fraction of the face, so it converts to texels by the
    /// base resolution, measured against the denser axis — an isotropic
    /// footprint over a 64x16 face is minified most where the texels are.
    #[inline(always)]
    fn texels_across(&self) -> f32 {
        self.base.u().max(self.base.v()) as f32
    }

    #[inline(always)]
    fn bilinear(&self, level: usize, u: f32, v: f32) -> Option<Vec3A> {
        // Level 0 is `base` itself; asking `level_res` for it would only
        // rebuild the same resolution.
        let res = if level == 0 {
            self.base
        } else {
            level_res(self.base, level as u8)
        };
        self.tex.sample_level(self.face, res, u, v)
    }

    #[inline(always)]
    fn blend(a: Vec3A, b: Vec3A, t: f32) -> Vec3A {
        a.lerp(b, t)
    }
}

/// One face under the `capped` chain, at and below the preload's cap: `base`
/// is the cap level, and the chain is [`PtexColor`]'s exactly — the same
/// levels, the same texels, the same `texels_across`, so the shared
/// [`trilinear`] selects and blends as it does there and the result is
/// bit-identical.
///
/// **Level 0 is read as file tiles; only the coarser levels are derived
/// blocks.** The cap level holds the file's own texels, which a tile read
/// decodes bit-identically to the preload already (the invariant
/// `streamed_and_preloaded_agree_texel_for_texel` pins), and a derived block
/// is a whole level: at an explicit cap of 8 the cap level would be a 768 KiB
/// `f32` block, past a microcache slot and possibly past a reader's share, so
/// every tap would decode the whole face again.
struct CappedFace<'a> {
    tex: &'a PtexStream,
    face: u32,
    base: ptex::Res,
    levels: usize,
}

impl MipSource for CappedFace<'_> {
    type Texel = Vec3A;

    #[inline(always)]
    fn level_count(&self) -> usize {
        self.levels
    }

    #[inline(always)]
    fn texels_across(&self) -> f32 {
        self.base.u().max(self.base.v()) as f32
    }

    #[inline(always)]
    fn bilinear(&self, level: usize, u: f32, v: f32) -> Option<Vec3A> {
        if level == 0 {
            return self.tex.sample_level(self.face, self.base, u, v);
        }
        let res = level_res(self.base, level as u8);
        self.tex.sample_derived(self.face, level, res, u, v)
    }

    #[inline(always)]
    fn blend(a: Vec3A, b: Vec3A, t: f32) -> Vec3A {
        a.lerp(b, t)
    }
}

/// One face under the `capped` chain, finer than the preload's cap: the
/// file's own levels from the authored resolution `fine`, then the cap level
/// `base`, which is this chain's last level and the [`CappedFace`]'s first.
///
/// Levels `0..steps` are the `file` chain's — `fine` halved on both axes,
/// read as tiles — and level `steps` is `base`. The two agree on the widest
/// axis there, which is what [`trilinear`]'s level selection counts. On a
/// square face they are the same resolution; on a non-square one the halved
/// level has already lost short-axis texels the cap level keeps (1024x512
/// halves to 32x16 where the cap holds 32x32). Stepping the short axis more
/// slowly instead would mean levels the file does not store, which the
/// reader reduces from the full-resolution face — the read streaming exists
/// to avoid.
struct FinerFace<'a> {
    tex: &'a PtexStream,
    face: u32,
    fine: ptex::Res,
    base: ptex::Res,
    /// Levels from `fine` to `base`: the widest axis's log2 difference.
    steps: usize,
    levels: usize,
}

impl MipSource for FinerFace<'_> {
    type Texel = Vec3A;

    #[inline(always)]
    fn level_count(&self) -> usize {
        self.levels
    }

    #[inline(always)]
    fn texels_across(&self) -> f32 {
        self.fine.u().max(self.fine.v()) as f32
    }

    #[inline(always)]
    fn bilinear(&self, level: usize, u: f32, v: f32) -> Option<Vec3A> {
        if level >= self.steps {
            // The cap level, read as tiles as [`CappedFace`] reads it.
            self.tex.sample_level(self.face, self.base, u, v)
        } else {
            let res = level_res(self.fine, level as u8);
            self.tex.sample_level(self.face, res, u, v)
        }
    }

    #[inline(always)]
    fn blend(a: Vec3A, b: Vec3A, t: f32) -> Vec3A {
        a.lerp(b, t)
    }
}

/// The `capped` chain's levels at and below the preload's cap, as the reader
/// derives and caches them (`ptex::DerivedLevels`).
///
/// Level 0 is the file's face at the cap — [`capped_res`], the resolution
/// [`PtexColor`] fetches — through [`decode_face`], the preload's own decode;
/// each further level is [`reduce_level`] of its parent, the preload's own
/// reduction. Interleaved linear `f32` RGB, little-endian, exactly the
/// texels the preloaded arena holds for that face and level. Lookups ask for
/// levels 1 and coarser only (level 0 is read as tiles, see [`CappedFace`]),
/// so level 0 is produced as the parent of a derivation and not kept.
struct CappedLevels {
    max_log2: i8,
    triangle: bool,
    dt: ptex::DataType,
    n_chan: usize,
    space: ColorSpace,
    reduce: LevelReduction,
}

impl ptex::DerivedLevels for CappedLevels {
    fn base_res(&self, _faceid: usize, res: ptex::Res) -> ptex::Res {
        capped_res(res, self.max_log2, self.triangle)
    }

    fn decode(&self, _faceid: usize, res: ptex::Res, pixels: &[u8]) -> Vec<u8> {
        let mut texels = vec![0.0f32; res.size() * 3];
        if pixels.len() >= res.size() * self.dt.size() * self.n_chan {
            decode_face(pixels, &mut texels, self.dt, self.n_chan, self.space);
        }
        to_bytes(&texels)
    }

    fn derive(&self, _faceid: usize, _k: u8, parent_res: ptex::Res, parent: &[u8]) -> Vec<u8> {
        let parent: Vec<f32> = parent
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let (sw, sh) = (parent_res.u(), parent_res.v());
        if parent.len() != sw * sh * 3 {
            return to_bytes(&vec![0.0; (sw / 2).max(1) * (sh / 2).max(1) * 3]);
        }
        to_bytes(&reduce_level(&parent, sw, sh, self.reduce))
    }
}

fn to_bytes(texels: &[f32]) -> Vec<u8> {
    texels.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// One raw sample, scaled and put through `space`'s curve (see
/// [`crate::ptex_texture::decode_ptex_slice`]): an entry of the `u8` table.
/// The table is per channel, so it holds the curve alone; the change of
/// primaries follows per texel, as it does for the preloaded backend.
fn decode_sample(raw: f32, scale: f32, space: ColorSpace) -> f32 {
    ptex_space(space).decode_curve(raw * scale)
}

impl PtexStream {
    /// A lookup under the `capped` chain. `fine` is the finest level served
    /// (the authored face, or an explicit cap), `max_log2` the preload's cap.
    ///
    /// **Routed by the footprint against the cap level, not against `fine`**,
    /// because that is what makes the coarse half bit-identical rather than
    /// merely close: a footprint wider than one cap texel goes through
    /// [`CappedFace`], whose level selection is the preload's to the bit
    /// (selected against `fine`, `log2` would see a different argument and
    /// could round the blend weight differently). Only a footprint inside one
    /// cap texel — where the preload has nothing finer to give — reads the
    /// file's finer levels through [`FinerFace`].
    ///
    /// **Without a pyramid (`CRUST_PTEX_MIP=0`) the footprint is ignored**,
    /// as [`MipSource`] promises for a single level: every lookup reads
    /// `fine`, the uncapped authored face unless a cap was set. Routing by
    /// the footprint there would switch from the authored face to the cap
    /// face as a surface recedes, a jump in detail with distance.
    #[allow(clippy::too_many_arguments)]
    fn eval_capped(
        &self,
        face: u32,
        authored: ptex::Res,
        fine: ptex::Res,
        max_log2: i8,
        fu: f32,
        fv: f32,
        width: f32,
    ) -> Vec3A {
        if !self.mip {
            return self
                .sample_level(face, fine, fu, fv)
                .unwrap_or(self.fallback);
        }
        let base = capped_res(authored, max_log2, self.triangle);
        let coarse = CappedFace {
            tex: self,
            face,
            base,
            levels: level_count(base) as usize,
        };
        let wide = width.is_finite() && width > 0.0 && width * coarse.texels_across() > 1.0;
        // `fine` is never coarser than `base`: an explicit cap is the
        // preload's cap too, and without one `fine` is the authored face.
        if fine == base || wide {
            return trilinear(&coarse, fu, fv, width).unwrap_or(self.fallback);
        }
        let steps = (fine.ulog2.max(fine.vlog2) - base.ulog2.max(base.vlog2)) as usize;
        let finer = FinerFace {
            tex: self,
            face,
            fine,
            base,
            steps,
            levels: steps + 1,
        };
        trilinear(&finer, fu, fv, width).unwrap_or(self.fallback)
    }
}

impl PtexTexture for PtexStream {
    fn eval(&self, face_id: u32, u: f32, v: f32, width: f32) -> Vec3A {
        let _p = crust_core::profile::scope(crust_core::profile::Section::Texture);
        let Ok(info) = self.reader.face_info(face_id as usize) else {
            return self.fallback;
        };
        let base = self.base_res(info.res);

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

        if let Some(max_log2) = self.capped {
            return self.eval_capped(face_id, info.res, base, max_log2, fu, fv, width);
        }

        let levels = if self.mip { level_count(base) } else { 1 };
        // The shared trilinear filter, as the preloaded backend runs it; a
        // failed read of the finer level falls back, of the coarser one keeps
        // the finer.
        let source = StreamFace {
            tex: self,
            face: face_id,
            base,
            levels: levels as usize,
        };
        trilinear(&source, fu, fv, width).unwrap_or(self.fallback)
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
            let table = decode_sample(i as f32, scale, ColorSpace::GAMMA22);
            let scalar = (read_channel(&[i as u8], ptex::DataType::UInt8) * scale)
                .max(0.0)
                .powf(2.2);
            assert_eq!(table.to_bits(), scalar.to_bits(), "entry {i}");
            // Raw is the scaled sample itself: no curve, no clamp.
            let raw = decode_sample(i as f32, scale, ColorSpace::RAW);
            assert_eq!(raw.to_bits(), (i as f32 * scale).to_bits(), "raw entry {i}");
        }
    }

    #[test]
    fn the_default_budget_is_stated_once_and_in_mib() {
        // A compile-time check that the default is stated in one place and in
        // MiB.
        assert_eq!(DEFAULT_CACHE_MB * 1024 * 1024, 1024 * 1024 * 1024);
    }
}
