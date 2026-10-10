//! Host-side asset decoders for Crust Render.
//!
//! `crust-core` decodes nothing — its [`crust_core::AssetLoader`] seam asks the
//! host for an environment map, a Ptex sampler or a UV texture and takes
//! whatever comes back. This crate is the host side of that seam for a program
//! that reads files: [`FileAssets`] implements the trait over `exr`, `image`
//! and `ptex-rs`, and the decoders behind it are public so the probe examples
//! can decode a texture *exactly* the way the renderer does instead of
//! carrying their own copies (which is what they used to do — `read_channel`
//! existed three times).
//!
//! Everything that knows a file format lives here, so a tool that wants to
//! render without any of these formats links crust-core alone.
//!
//! Two A/B switches for separating a texture problem from a material or
//! lighting one: `CRUST_PTEX=0` declines every Ptex file and `CRUST_TEX=0`
//! every UV texture, so the same scene renders on its constant inputs.
//! `CRUST_PTEX_MAX_LOG2` and `CRUST_TEX_MAX` cap the decoded resolutions —
//! see the respective modules for why those caps are what make production
//! assets loadable at all.
//! `forbid(unsafe_code)`: this crate contains no `unsafe`, and the attribute
//! is what keeps the README's "100% safe Rust" true rather than aspirational.
//! It is load-bearing here — the tile cache was hand-rolled rather than taking
//! a concurrent-cache dependency precisely so this would still hold.
#![forbid(unsafe_code)]

mod environment;
mod error;
mod exr_planes;
mod ies;
mod image_file;
mod mip_filter;
mod ptex_stream;
mod ptex_texture;
mod texture_cache;
mod tiled;
mod uv_texture;

pub use environment::{load_exr_environment, load_image_environment, read_exr_rgb, read_rgb_image};
pub use error::AssetError;
pub use exr_planes::read_exr_planes;
pub use ies::{load_ies, parse_ies};
pub use ptex_stream::{
    DEFAULT_CACHE_MB as PTEX_DEFAULT_CACHE_MB, DEFAULT_STREAM_MIN_MB as PTEX_DEFAULT_STREAM_MIN_MB,
    MICRO_SLOTS as PTEX_MICRO_SLOTS, MipSpace as PtexMipSpace, PtexStream,
    StreamStats as PtexStreamStats, micro_reserve as ptex_micro_reserve,
    micro_retained_bytes as ptex_micro_retained_bytes, micro_slot_max as ptex_micro_slot_max,
    micro_thread_bytes as ptex_micro_thread_bytes, micro_threads as ptex_micro_threads,
};
pub use ptex_texture::{
    DEFAULT_MAX_LOG2, PtexColor, max_log2_from_env, max_log2_from_env_opt, read_channel,
};
/// Offline `.tx` conversion: what `maketx` and `--auto-tx` write.
pub use tiled::{MadeTx, TxFormat, make_tx, make_tx_atomic};
pub use uv_texture::{DEFAULT_MAX_EDGE, UvTexture};

/// The files a texture path names: every `<UDIM>` / `<UVTILE>` tile on
/// disk (the 10x10 UDIM sweep every texture reader shares), or the one image
/// when it exists.
pub fn texture_files(path: &Path) -> Vec<std::path::PathBuf> {
    let name = path.to_string_lossy();
    if name.contains("<UDIM>") || name.contains("<UVTILE>") {
        uv_texture::existing_tiles(&name)
            .into_iter()
            .map(|t| t.path)
            .collect()
    } else if path.exists() {
        vec![path.to_path_buf()]
    } else {
        Vec::new()
    }
}

use crust_core::{
    AssetLoader, ColorSpace, EnvironmentMap, IesProfile, LightTexture, PtexTexture,
    ResolvedColorSpace, Texture2D,
};
use crust_core::{cause_warning, warning};
use std::path::Path;
use std::time::Instant;
use tracing::{debug, info, warn};

/// The tables a resolved colour space's curve is applied through.
///
/// The curves themselves are OCIO's ([`crust_core::color`]); a texture stored
/// as bytes meets them only through these two tables, built once per open, so
/// no texel lookup and no mip reduction runs an OCIO processor.
pub trait TransferCurve: Copy {
    /// The 256-entry decode table.
    ///
    /// The files are 8-bit, so every possible stored value is one of 256 —
    /// the transfer function is evaluated once per level at load rather than
    /// per texel fetch, and nothing recovers precision that was never in the
    /// file.
    ///
    /// The three curves are deliberately distinct. MaterialX's `g22_rec709`
    /// and `g18_rec709` are pure power laws; sRGB's EOTF is piecewise, with a
    /// linear toe that keeps near-black values well above the power law (up
    /// to 19x at 0.01 — `docs/color_management.md` tabulates it). Collapsing
    /// them into one curve is wrong in the shadows for 2.2 and wrong
    /// everywhere for 1.8.
    fn to_linear_table(self) -> [f32; 256];

    /// The linear value at which each stored byte begins: entry `k` is the
    /// decode of `(k + 0.5) / 255`, the boundary between bytes `k` and
    /// `k + 1`.
    ///
    /// The re-encode of a mip level, as a table. Rounding `encode(mean)` to
    /// the nearest byte picks the `k` whose interval holds it, and since the
    /// curve is monotone that is the number of boundaries at or below `mean`
    /// — [`quantize`] — found by bisection rather than by running the
    /// inverse curve on every texel of every coarser level.
    fn code_steps(self) -> [f32; 255];
}

impl TransferCurve for ResolvedColorSpace {
    fn to_linear_table(self) -> [f32; 256] {
        let mut table = [0.0f32; 256];
        for (i, v) in table.iter_mut().enumerate() {
            *v = i as f32 / 255.0;
        }
        self.decode_curve_slice(&mut table);
        table
    }

    fn code_steps(self) -> [f32; 255] {
        let mut steps = [0.0f32; 255];
        for (k, v) in steps.iter_mut().enumerate() {
            *v = (k as f32 + 0.5) / 255.0;
        }
        self.decode_curve_slice(&mut steps);
        steps
    }
}

/// A filtered RGBA lookup brought to the working space: the change of
/// primaries a texture stored on its own primaries applies after filtering
/// ([`crust_core::color::apply_gamut`]); alpha is untouched. One function for
/// the preloaded and the streamed sampler, so the two stay bit-identical.
#[inline]
pub(crate) fn to_working(gamut: Option<&crust_core::Mat3A>, rgba: [f32; 4]) -> [f32; 4] {
    match gamut {
        None => rgba,
        Some(_) => {
            let [r, g, b, a] = rgba;
            let c = crust_core::color::apply_gamut(gamut, crust_core::Vec3A::new(r, g, b));
            [c.x, c.y, c.z, a]
        }
    }
}

/// A linear value as the nearest stored byte, given its space's
/// [`TransferCurve::code_steps`]. Below the first boundary (or NaN) is 0, past
/// the last is 255.
#[inline]
pub(crate) fn quantize(steps: &[f32; 255], linear: f32) -> u8 {
    steps.partition_point(|&s| s <= linear) as u8
}

/// An 8-bit alpha as the coverage it stores: `a / 255`.
///
/// Alpha is never colour-managed. A colour space's curve and primaries apply
/// to RGB alone, so an sRGB texture's alpha is not sRGB-decoded — as a GPU's
/// `SRGB8_ALPHA8` format leaves it linear. One table, shared by the preloaded
/// sampler, the streamed one and the mip reduction, so their alphas are the
/// same bits. It is [`TransferCurve::to_linear_table`] of `raw`.
pub(crate) const ALPHA_U8: [f32; 256] = {
    let mut table = [0.0f32; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = i as f32 / 255.0;
        i += 1;
    }
    table
};

/// [`TransferCurve::code_steps`] for alpha: the boundaries `(k + 0.5) / 255`
/// [`quantize`] re-encodes an averaged alpha through.
pub(crate) const ALPHA_STEPS: [f32; 255] = {
    let mut steps = [0.0f32; 255];
    let mut k = 0;
    while k < 255 {
        steps[k] = (k as f32 + 0.5) / 255.0;
        k += 1;
    }
    steps
};

/// Interleaved RGBA as RGB when its alpha is `opaque` at every texel, and
/// whether an alpha was kept.
///
/// An image saved with an alpha channel it never uses — most RGBA PNGs a DCC
/// exports — is stored as the RGB it is: a fourth channel that reads 1.0
/// everywhere costs a third more memory and changes no lookup, since a texture
/// without alpha reads 1.0 too. Only an alpha that cuts something is kept.
pub(crate) fn drop_opaque_alpha<T: Copy + PartialEq>(rgba: Vec<T>, opaque: T) -> (Vec<T>, bool) {
    let (texels, _) = rgba.as_chunks::<4>();
    if texels.iter().any(|t| t[3] != opaque) {
        return (rgba, true);
    }
    (
        texels.iter().flat_map(|t| [t[0], t[1], t[2]]).collect(),
        false,
    )
}

/// Applies `decode` — a colour space's curve, or curve and primaries — to the
/// RGB of interleaved RGBA, leaving alpha as it is. The RGB values pass
/// through `decode` in the same order and grouping as an RGB image's would,
/// so the colour of an RGBA texel is decoded exactly as an RGB one.
pub(crate) fn decode_rgb_of_rgba(rgba: &mut [f32], decode: impl FnOnce(&mut [f32])) {
    let mut rgb: Vec<f32> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|t| [t[0], t[1], t[2]])
        .collect();
    decode(&mut rgb);
    for (texel, c) in rgba
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(rgb.as_chunks::<3>().0)
    {
        texel[..3].copy_from_slice(c);
    }
}

/// The host side of `crust_core::AssetLoader`, reading from the filesystem.
///
/// The engine asks for pixels, this decodes them: OpenEXR through `exr`,
/// Radiance `.hdr` and LDR images through `image`, per-face Ptex through
/// `ptex-rs`. LDR pixels are un-gamma'd to linear, since the renderer works
/// in linear light and an sRGB-encoded sky would be noticeably wrong.
pub struct FileAssets {
    /// The `CRUST_*` switches this loader obeys: [`crust_core::config()`]
    /// unless constructed [`FileAssets::with_config`]. Every texture-policy
    /// decision below reads it, never the environment.
    config: crust_core::Config,
    /// The tile cache every streaming texture shares. Present whether or not
    /// streaming is on, so the counters can be reported either way — an empty
    /// one costs 64 empty maps.
    cache: std::sync::Arc<tiled::TileCache>,
    /// Whether a `.tx` beside a texture is looked for and streamed. On by
    /// default — a `.tx` exists only because someone converted the texture
    /// for streaming — with `CRUST_TEX_STREAM=0` to force every texture onto
    /// the preload path for an A/B. Read once at construction, not per
    /// `load_texture`: it decides the backend of every texture in the render,
    /// and reading the environment per call would let it change mid-import.
    streaming: bool,
    /// `--auto-tx`: convert a texture whose `.tx` is stale
    /// ([`tiled::tx_staleness`]) before looking for it. See
    /// [`FileAssets::with_auto_tx`].
    auto_tx: bool,
    /// Conversions `--auto-tx` performed and failed, and the time they took,
    /// for the one summary line the CLI prints (see [`FileAssets::tx_report`]).
    tx_converted: std::sync::atomic::AtomicUsize,
    tx_failed: std::sync::atomic::AtomicUsize,
    tx_nanos: std::sync::atomic::AtomicU64,
    /// The same, for Ptex. A separate switch rather than a shared one because
    /// the two answer different questions: `CRUST_TEX_STREAM` needs a `.tx`
    /// converted beside the asset and silently declines without one, while
    /// `CRUST_PTEX_STREAM` needs nothing — a `.ptx` is already a tiled per-face
    /// pyramid, which is the whole reason Ptex was the format waiting on a
    /// reader-side cache rather than on a conversion step.
    ptex_streaming: bool,
    /// Which mip chain a streamed Ptex may read, from
    /// `CRUST_PTEX_STREAM_MIPSPACE`. Read once for the same reason
    /// `ptex_streaming` is: it decides admission for every texture in the
    /// render, and a value that changed mid-import would give one stage
    /// chunk's textures a different backend from the next's.
    ptex_mip_space: ptex_stream::MipSpace,
    /// Every Ptex texture opened, so the render can be reported on and — for
    /// the streamed ones — re-budgeted as more arrive. See [`PtexHandle`].
    ptex: std::sync::Mutex<Vec<PtexHandle>>,
    /// UV textures that took the preload path, and the bytes they hold —
    /// the half of texture residency the tile cache cannot see.
    preloaded_textures: std::sync::atomic::AtomicU64,
    preloaded_texture_bytes: std::sync::atomic::AtomicU64,
}

/// Which streaming candidate [`FileAssets::open_streaming`] may open.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Candidates {
    /// The texture's own path — a `.tx`, or a source that is already a tiled
    /// mip file.
    Source,
    /// The `.tx` sibling, once `prepare_tx` has found (or made) it complete.
    Sibling,
}

/// The smallest share of the budget worth giving a streamed reader: 1 MiB.
///
/// **This bounds the reader count; it is not a floor on the share.** Those are
/// opposite designs and the difference is the whole bug this replaced. A floor
/// — `max(budget / n, 1 MiB)` — silently multiplies: at a `CRUST_PTEX_CACHE_MB`
/// of 8 with 39 admitted readers it hands out 39 MiB against a budget of 8,
/// and the setting that exists to bound residency stops bounding it. The
/// smaller the budget, the worse the overshoot, which is exactly backwards.
///
/// So this is read as a *capacity*: at most `budget / MIN_PTEX_SHARE` readers
/// may stream, and the budget is then divided **exactly** among them. Both
/// properties hold by construction — every admitted reader gets at least
/// 1 MiB, and `n * (budget / n) <= budget` because integer division floors.
/// A texture arriving past that cap is preloaded
/// ([`PreloadReason::BudgetFull`]), which is the honest answer: there is no
/// cache left to give it, and a reader with a share too small to hold one
/// block caches nothing anyway (upstream returns such a read `oversized`).
///
/// It costs the default path nothing: 1 GiB admits 1 024 readers and the
/// island wants 39. It only engages when the budget is genuinely small, which
/// is precisely when honouring it matters.
const MIN_PTEX_SHARE: usize = 1024 * 1024;

/// One opened Ptex texture, as `FileAssets` remembers it.
///
/// Held for two reasons that both only show up on a real stage. A preloaded
/// texture is remembered so `--stats` can report *which backend ran* and what
/// it cost — without that the report is silent about Ptex and an operator
/// cannot tell a streamed island from a preloaded one. A streamed one is
/// remembered because its budget has to be revised downward as siblings
/// arrive; see [`FileAssets::rebudget_ptex`].
enum PtexHandle {
    Preloaded {
        faces: usize,
        bytes: usize,
        why: PreloadReason,
    },
    Streamed(std::sync::Arc<PtexStream>),
}

/// Why a texture ended up preloaded. Reported separately because the three
/// mean different things: the default is that streaming is off, `TooSmall` is
/// the admission rule working as designed on 95% of a production stage's
/// files, and `StreamFailed` is the one that wants looking at.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PreloadReason {
    NotStreaming,
    TooSmall,
    /// The budget has no room for another reader — see [`MIN_PTEX_SHARE`].
    BudgetFull,
    /// Streaming it would have read a mip chain reduced in the file's own
    /// encoding, which `PtexColor` builds in linear light — see
    /// [`ptex_stream::MipSpace`]. A correctness refusal rather than an
    /// efficiency one, made only under `CRUST_PTEX_STREAM_MIPSPACE=linear`,
    /// where it is the reason *most* mipmapped textures preload, so it is
    /// reported on its own line.
    MipSpace,
    StreamFailed,
}

impl Default for FileAssets {
    fn default() -> Self {
        FileAssets::new()
    }
}

impl FileAssets {
    /// A loader under the process's switches ([`crust_core::config()`]).
    pub fn new() -> FileAssets {
        FileAssets::with_config(crust_core::config().clone())
    }

    /// A loader under `config` rather than the process environment — how a
    /// test or probe compares both sides of a switch without mutating a
    /// process-global the rest of the program is reading.
    pub fn with_config(config: crust_core::Config) -> FileAssets {
        let streaming = config.tex_stream;
        let budget = tiled::TileCache::budget_of(&config);
        let max_open_files = config.tex_max_open_files;
        // DEBUG, not INFO: this is the default now, and a default render's
        // INFO lines are the four that do not scale with anything.
        if streaming {
            debug!(
                "Textures stream from a .tx beside them when one exists, through a {:.0} MiB \
                 tile cache (CRUST_TEX_STREAM=0 preloads everything)",
                texture_cache::bytes_to_mib(budget)
            );
        }
        let ptex_streaming = config.ptex_stream;
        let ptex_mip_space = config.ptex_mip_space;
        if ptex_streaming {
            // DEBUG, not INFO: streaming is the default, and a default
            // render's INFO lines are the four that do not scale with
            // anything. A non-default chain is said at INFO below.
            debug!(
                "Large Ptex files stream through a {:.0} MiB cache \
                 (CRUST_PTEX_STREAM=0 preloads everything)",
                texture_cache::bytes_to_mib(ptex_stream::budget_bytes(&config) as u64)
            );
            // Said at construction rather than per texture, because under
            // `linear` it is the line that explains a render where streaming
            // was on and nothing streamed.
            match ptex_mip_space {
                ptex_stream::MipSpace::Capped => {}
                ptex_stream::MipSpace::Linear => info!(
                    "CRUST_PTEX_STREAM_MIPSPACE=linear: a mipmapped .ptx preloads — the \
                     default `capped` streams it with the preloaded pyramid below the cap \
                     (see docs/ptex_streaming.md)"
                ),
                ptex_stream::MipSpace::File => info!(
                    "CRUST_PTEX_STREAM_MIPSPACE=file: streaming the .ptx's own mip chain, \
                     which is reduced in the file's encoding and so darker under \
                     minification than the preloaded pyramid"
                ),
            }
        }
        // The residency policy in one line, both switches off included:
        // "why is this texture only 32x32" is a question the caps answer and
        // nothing else records.
        debug!(
            "Texture residency: UV {} (CRUST_TEX_MAX={}), Ptex {} (CRUST_PTEX_MAX_LOG2={})",
            if streaming { "streaming" } else { "preloaded" },
            config.tex_max,
            if ptex_streaming {
                "streaming"
            } else {
                "preloaded"
            },
            config.ptex_max_log2.unwrap_or(DEFAULT_MAX_LOG2),
        );
        FileAssets {
            config,
            cache: std::sync::Arc::new(tiled::TileCache::new(budget, max_open_files)),
            streaming,
            auto_tx: false,
            tx_converted: std::sync::atomic::AtomicUsize::new(0),
            tx_failed: std::sync::atomic::AtomicUsize::new(0),
            tx_nanos: std::sync::atomic::AtomicU64::new(0),
            ptex_streaming,
            ptex_mip_space,
            ptex: std::sync::Mutex::new(Vec::new()),
            preloaded_textures: std::sync::atomic::AtomicU64::new(0),
            preloaded_texture_bytes: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Converts textures on first use: before a UV texture is opened, every
    /// tile whose `.tx` sibling is missing, older than its source, or written
    /// by an older crust without the alpha its source has
    /// ([`tiled::tx_staleness`]) is converted beside it
    /// (`foo.1001.exr` → `foo.1001.tx`), the way Arnold's `autotx` does. The
    /// next render finds them current and converts nothing.
    ///
    /// The conversion is [`tiled::make_tx_atomic`] with
    /// [`tiled::TxFormat::FromSampleType`]: float sources keep `half` tiles,
    /// 8-bit ones take `u8`, and the colour space recorded is the one the
    /// material binds with (`auto` resolved against the file). A tile that
    /// fails to convert — a read-only asset library, a full disk — sends that
    /// texture down the preload path rather than streaming a set with a hole
    /// in it.
    pub fn with_auto_tx(mut self, on: bool) -> FileAssets {
        if on && !self.streaming {
            warn!("--auto-tx has no effect with CRUST_TEX_STREAM=0: no .tx is ever read");
        } else if on {
            info!("--auto-tx: converting textures to .tx beside their sources on first use");
        }
        self.auto_tx = on && self.streaming;
        self
    }

    /// `(converted, failed, seconds)` for the `--auto-tx` conversions so far.
    pub fn tx_report(&self) -> (usize, usize, f64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.tx_converted.load(Relaxed),
            self.tx_failed.load(Relaxed),
            self.tx_nanos.load(Relaxed) as f64 * 1e-9,
        )
    }

    /// Whether a complete set of `.tx` siblings stands beside `path`'s tiles,
    /// converting the missing and stale ones first under `--auto-tx`.
    ///
    /// **Complete** is the point: the streaming texture opens whichever tiles
    /// have a `.tx` and knows nothing of the ones that do not, so a half-
    /// converted UDIM set would stream with black holes where the missing
    /// tiles are. Such a set preloads instead. Without `--auto-tx` a `.tx`
    /// older than its source is still used — it is what was converted — but
    /// warned about, since the image then shows the old texture. A `.tx` an
    /// older crust wrote without the alpha its source declares
    /// ([`tiled::TxStaleness::PredatesAlpha`]) is not: its cutouts would be
    /// opaque, which is no texture anybody converted, so the set preloads,
    /// with a warning, as a `.tx` with the wrong mip space does.
    fn prepare_tx(&self, path: &Path, space: ColorSpace) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        let is_tx = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("tx"));
        if is_tx {
            return true;
        }
        let sources = texture_files(path);
        if sources.is_empty() {
            return false;
        }
        let staleness: Vec<_> = sources
            .iter()
            .filter_map(|s| tiled::tx_staleness(s, &tiled::tx_sibling(s)).map(|why| (s, why)))
            .collect();
        let stale: Vec<_> = staleness.iter().map(|(s, _)| (*s).clone()).collect();
        if stale.is_empty() {
            return true;
        }
        if !self.auto_tx {
            let count =
                |want: tiled::TxStaleness| staleness.iter().filter(|(_, why)| *why == want).count();
            let missing = count(tiled::TxStaleness::Missing);
            if missing > 0 {
                if missing < sources.len() {
                    debug!(
                        "{}: {missing} of {} tile(s) have no .tx — preloading the set \
                         (--auto-tx converts the rest)",
                        path.display(),
                        sources.len()
                    );
                }
                return false;
            }
            let predates_alpha = count(tiled::TxStaleness::PredatesAlpha);
            if predates_alpha > 0 {
                warning!(
                    TextureStreamFallback,
                    "{}: {predates_alpha} .tx tile(s) written by an older crust without \
                     the alpha their source has — preloading the texture instead; rerun \
                     with --auto-tx to reconvert",
                    path.display()
                );
                return false;
            }
            warning!(
                TextureTxStale,
                "{}: {} .tx tile(s) older than their source, used anyway — rerun with \
                 --auto-tx to reconvert",
                path.display(),
                stale.len()
            );
            return true;
        }

        let started = Instant::now();
        let workers = std::thread::available_parallelism()
            .map_or(4, |n| n.get())
            .min(stale.len());
        let chunk = stale.len().div_ceil(workers.max(1));
        // Failures are reported after the workers join, on this thread: a
        // coded warning raised on a worker would be logged but not recorded
        // in the import's warnings. Sorted by source, so which one gives the
        // record its message does not depend on which worker finished first.
        let failures = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for part in stale.chunks(chunk.max(1)) {
                let failures = &failures;
                scope.spawn(move || {
                    for src in part {
                        match tiled::make_tx_atomic(src, space, tiled::TxFormat::FromSampleType) {
                            Ok(m) => debug!(
                                "--auto-tx: {} -> {} [{}, {:?}]",
                                src.display(),
                                m.dst.display(),
                                m.kind,
                                m.space
                            ),
                            Err(e) => failures
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push((src, e.to_string())),
                        }
                    }
                });
            }
        });
        let mut failures = failures.into_inner().unwrap_or_else(|e| e.into_inner());
        failures.sort();
        for (src, e) in &failures {
            warning!(
                TextureTxConvertFailed,
                "--auto-tx: could not convert {}: {e}",
                src.display()
            );
        }
        let failed = failures.len();
        self.tx_converted.fetch_add(stale.len() - failed, Relaxed);
        self.tx_failed.fetch_add(failed, Relaxed);
        self.tx_nanos
            .fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
        failed == 0
    }

    /// Splits the Ptex budget evenly over every streamed texture opened so far.
    ///
    /// **`CRUST_PTEX_CACHE_MB` is the render's budget, not a file's**, and
    /// keeping that true takes this. The `.tx` path gets it for free: every
    /// streaming texture there shares one `TileCache`, so the total is the
    /// budget by construction. `ptex::SharedReader` owns its cache instead —
    /// which is the right shape for a *library*, since a `.ptx` is a
    /// self-contained pyramid — so N textures opened at the full budget would
    /// hold N times it. That is not a rounding error on a production stage:
    /// the Moana island binds Ptex per element, so the default 1 GiB would
    /// become tens of GiB, and the feature whose entire purpose is to bound
    /// residency would be unbounded in the number of textures.
    ///
    /// An even split rather than a demand-driven one. It is not what OIIO
    /// would do — a shared pool serves whichever texture is hot — but it is
    /// what the reader's API allows without a second cache here, and the
    /// property that matters (the total is what was asked for) holds either
    /// way. Re-dividing on each open rather than once at the end because the
    /// count is only known when the import is done, and a texture must be
    /// usable the moment it is opened.
    ///
    /// **The division is exact, with no floor under the share.** A floor is
    /// what breaks the bound — see [`MIN_PTEX_SHARE`], which caps how many
    /// readers may stream instead, so that dividing exactly still leaves each
    /// one something usable. `max_streams` enforces that cap at admission, so
    /// by the time this runs `streams.len()` is already small enough.
    fn rebudget_ptex(&self, opened: &[PtexHandle]) {
        let streams: Vec<_> = opened
            .iter()
            .filter_map(|h| match h {
                PtexHandle::Streamed(s) => Some(s),
                PtexHandle::Preloaded { .. } => None,
            })
            .collect();
        if streams.is_empty() {
            return;
        }
        // Exact, and over what is left after the microcaches take theirs.
        // Thread-local tiles are real residency, so their allowance comes
        // *out of* the budget rather than sitting beside it — see
        // `ptex_stream::micro_reserve`. `n * (x / n) <= x` because integer
        // division floors, so readers plus microcaches are still at or under
        // what was asked for, whatever the count.
        let share = self.ptex_reader_total() / streams.len();
        for s in &streams {
            s.set_budget(share);
        }
    }

    /// How many textures may stream at once, given the budget.
    ///
    /// `budget / MIN_PTEX_SHARE`, and at least one — a single reader holding
    /// the whole budget is still within it, and refusing to stream anything
    /// at all would make a small budget mean "no streaming" rather than "a
    /// small cache".
    fn max_streams(&self) -> usize {
        (self.ptex_reader_total() / MIN_PTEX_SHARE).max(1)
    }

    /// The render's Ptex budget less the thread-local microcaches' share.
    ///
    /// What is left for the readers, and the number every division below is
    /// against — so the two halves of Ptex residency sum to the configured
    /// total rather than the readers alone matching it.
    fn ptex_reader_total(&self) -> usize {
        let total = self.ptex_budget();
        total - ptex_stream::micro_reserve(total)
    }

    /// The preloading backend's per-face cap: `CRUST_PTEX_MAX_LOG2`, or
    /// [`DEFAULT_MAX_LOG2`] when it is unset.
    fn preload_max_log2(&self) -> i8 {
        self.config.ptex_max_log2.unwrap_or(DEFAULT_MAX_LOG2)
    }

    /// The render's whole Ptex budget, `CRUST_PTEX_CACHE_MB`, in bytes.
    fn ptex_budget(&self) -> usize {
        ptex_stream::budget_bytes(&self.config)
    }

    /// Streamed textures opened so far. Callers hold the lock.
    fn streamed_count(opened: &[PtexHandle]) -> usize {
        opened
            .iter()
            .filter(|h| matches!(h, PtexHandle::Streamed(_)))
            .count()
    }

    /// Ptex residency and cache counters, in the shape `--stats` reports.
    ///
    /// Reports for **both** backends, because the first question the report
    /// has to answer is which one ran. Pushed into `RenderStats` by the host
    /// exactly as `texture_cache_stats` is.
    pub fn ptex_stats(&self) -> crust_core::PtexCacheStats {
        let opened = self.ptex.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = crust_core::PtexCacheStats {
            // Process-wide rather than per texture: one set of slots per
            // thread serves every stream, keyed by texture id.
            micro_retained_bytes: ptex_stream::micro_retained_bytes(),
            micro_reserve_bytes: ptex_stream::micro_reserve(self.ptex_budget()) as u64,
            ..Default::default()
        };
        for h in opened.iter() {
            out.textures += 1;
            match h {
                PtexHandle::Preloaded { faces, bytes, why } => {
                    out.faces += *faces as u64;
                    out.preloaded_bytes += *bytes as u64;
                    match why {
                        PreloadReason::TooSmall => out.below_threshold += 1,
                        PreloadReason::BudgetFull => out.budget_full += 1,
                        PreloadReason::MipSpace => out.mip_space += 1,
                        PreloadReason::StreamFailed => out.open_failed += 1,
                        PreloadReason::NotStreaming => {}
                    }
                }
                PtexHandle::Streamed(s) => {
                    let st = s.stats();
                    out.streamed += 1;
                    out.faces += s.faces() as u64;
                    out.micro_hits += st.micro_hits;
                    out.reader_lookups += st.reader_lookups;
                    out.cache_hits += st.cache.hits;
                    out.cache_misses += st.cache.misses;
                    out.evictions += st.cache.evictions;
                    out.resident_bytes += st.cache.bytes_resident as u64;
                    out.budget_bytes += st.cache.bytes_budget as u64;
                    out.capped += u32::from(s.is_capped());
                    out.derived_blocks += st.cache.derived_blocks as u64;
                    out.derived_bytes += st.cache.derived_bytes as u64;
                    out.derives += st.cache.derives;
                }
            }
        }
        out
    }

    /// The tile cache's counters, in the shape `--stats` reports.
    ///
    /// Converted here rather than in `crust-core` because the dependency runs
    /// that way: `crust-assets` knows about `crust-core`, not the reverse, so
    /// the host pushes its numbers in — exactly as `main.rs` already assigns
    /// `stats.rays` from what the renderer handed back.
    pub fn texture_cache_stats(&self) -> crust_core::TextureCacheStats {
        use std::sync::atomic::Ordering::Relaxed;
        let c = self.cache.counters();
        crust_core::TextureCacheStats {
            files: c.files,
            loaded_tiles: c.loaded_tiles,
            resident_bytes: c.resident_bytes,
            total_bytes: c.total_bytes,
            preloaded: self.preloaded_textures.load(Relaxed),
            preloaded_bytes: self.preloaded_texture_bytes.load(Relaxed),
            micro_hits: c.micro_hits,
            hits: c.hits,
            misses: c.misses,
            redundant: c.redundant,
            raced: c.raced,
            evictions: c.evictions,
            bytes_read: c.bytes_read,
            peak_bytes: c.peak_bytes,
            held_peak_bytes: c.held_peak_bytes,
            errors: c.errors,
            budget_bytes: c.budget_bytes,
            opens: c.opens,
            reopens: c.reopens,
            peak_open: c.peak_open,
            max_open_files: c.max_open_files,
        }
    }

    /// Closes every `.tx` file the tile cache holds open between misses.
    ///
    /// For the host to call once the render is done and before it writes its
    /// outputs, so the write never competes with texture files for
    /// descriptors. Textures still work afterwards: a miss reopens.
    pub fn release_texture_files(&self) {
        self.cache.release_readers();
    }

    /// Where a streamable backing for `path` might be, best candidate first.
    ///
    /// A scene names its textures as the artist authored them — `.png`,
    /// `.exr` — so the streaming path looks for a converted sibling rather
    /// than demanding the USD be rewritten. `examples/maketx` produces them;
    /// converting on first use instead is a deliberate follow-up, because a
    /// renderer that silently writes multi-gigabyte files next to a read-only
    /// asset library is a surprise nobody asked for.
    ///
    /// An asset already named `.tx` is taken as-is, and so is one named
    /// `.exr`: a tiled, mip-mapped EXR *is* the streaming format for V-Ray and
    /// Karma, so a stage that names one directly should stream it rather than
    /// hunt for a sibling. Both candidates are only *tried* — an ordinary
    /// scanline EXR declines at open and falls through to the sibling, and then
    /// to preloading.
    fn stream_candidates(path: &Path) -> Vec<std::path::PathBuf> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let sibling = path.with_extension("tx");
        match ext.as_str() {
            "tx" => vec![path.to_path_buf()],
            "exr" => vec![path.to_path_buf(), sibling],
            _ => vec![sibling],
        }
    }

    fn open_streaming(
        &self,
        path: &Path,
        space: ColorSpace,
        which: Candidates,
    ) -> Option<std::sync::Arc<dyn Texture2D>> {
        let candidates = Self::stream_candidates(path)
            .into_iter()
            .filter(|c| (c.as_path() == path) == (which == Candidates::Source));
        for candidate in candidates {
            let started = Instant::now();
            let Some(tex) = tiled::StreamingTexture::open(&candidate, space, self.cache.clone())
            else {
                continue;
            };
            let (w, h) = tex.size();
            debug!(
                "Streaming texture {} ({} chart(s), {}x{} level 0, {} level(s), {} {} tiles, \
                 {:?}) in {:?}",
                candidate.display(),
                tex.chart_count(),
                w,
                h,
                tex.level_count(),
                if tex.is_linear() { "half" } else { "8-bit" },
                if tex.has_alpha() { "RGBA" } else { "RGB" },
                tex.color_space(),
                started.elapsed()
            );
            return Some(std::sync::Arc::new(tex));
        }
        None
    }
}

impl AssetLoader for FileAssets {
    fn load_environment(&self, path: &Path, space: ColorSpace) -> Option<EnvironmentMap> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let started = Instant::now();
        match environment::try_load_environment(path, ext == "exr", space) {
            Ok(map) => {
                debug!(
                    "Loaded environment {} ({}x{}) in {:?}",
                    path.display(),
                    map.width(),
                    map.height(),
                    started.elapsed()
                );
                Some(map)
            }
            Err(e) => {
                cause_warning!(LightMapUnreadable, "{e} — the dome renders without its map");
                None
            }
        }
    }

    fn load_texture(
        &self,
        path: &Path,
        space: ColorSpace,
    ) -> Option<std::sync::Arc<dyn Texture2D>> {
        // Same A/B switch as CRUST_PTEX, for the same reason: with every
        // texture declined a MaterialX surface renders on its constant
        // inputs, which is how you tell a wrong chart from a wrong material.
        if !self.config.tex {
            debug!("CRUST_TEX=0: ignoring {}", path.display());
            return None;
        }
        // Streaming first, preloading as the fallback. Anything the streaming
        // path declines — no `.tx` beside the asset, a mip chain reduced in
        // another colour space, a file it cannot read — lands here, so turning
        // the switch on can make a render slower but never break it.
        // Ptex never takes the `.tx` route, even when one arrives here as a
        // UV texture: it is already a tiled mip pyramid, so there is nothing to
        // convert, and a `foo.tx` beside it is not its converted form.
        if self.streaming && !tiled::is_ptex(path) {
            // The source first: a file that is itself tiled and mip-mapped (an
            // OIIO `.tx`, or a tiled mip EXR — every ALab texture is one)
            // streams as it is, and converting it would only write a copy.
            if let Some(streamed) = self.open_streaming(path, space, Candidates::Source) {
                return Some(streamed);
            }
            if self.prepare_tx(path, space)
                && let Some(streamed) = self.open_streaming(path, space, Candidates::Sibling)
            {
                return Some(streamed);
            }
        }
        if self.streaming {
            debug!(
                "No .tx backing for {} — preloading it instead",
                path.display()
            );
        }
        let started = Instant::now();
        let (mip, max_edge) = (self.config.tex_mip, self.config.tex_max);
        let loaded = match UvTexture::try_open_capped(path, space, mip, max_edge) {
            Ok(t) => t,
            Err(e) => {
                // The one place a texture failure becomes the seam's `None`,
                // so the one place it is logged.
                cause_warning!(TextureUnreadable, "{e} — the input reads its fallback");
                return None;
            }
        };
        {
            use std::sync::atomic::Ordering::Relaxed;
            self.preloaded_textures.fetch_add(1, Relaxed);
            self.preloaded_texture_bytes
                .fetch_add(loaded.bytes() as u64, Relaxed);
        }
        // The space *resolved* against the file, not the one asked for: under
        // UsdUVTexture's `auto` the two differ, and the resolved one is what
        // answers "why is this map darker than expected".
        debug!(
            "Loaded texture {} ({} tile(s), {}x{} each, {} {}, {:.1} MiB resident, {:?}) in {:?}",
            path.display(),
            loaded.tile_count(),
            loaded.tile_size().0,
            loaded.tile_size().1,
            if loaded.is_float() { "f32" } else { "8-bit" },
            if loaded.has_alpha() { "RGBA" } else { "RGB" },
            loaded.bytes() as f64 / (1024.0 * 1024.0),
            loaded.color_space(),
            started.elapsed()
        );
        Some(std::sync::Arc::new(loaded))
    }

    fn load_light_texture(
        &self,
        path: &Path,
        space: ColorSpace,
    ) -> Option<std::sync::Arc<LightTexture>> {
        let started = Instant::now();
        let loaded = environment::try_read_rgb_image(path, space).and_then(|(w, h, px)| {
            LightTexture::new(w, h, px)
                .ok_or_else(|| AssetError::unusable(path, "not a usable light texture (empty)"))
        });
        match loaded {
            Ok(t) => {
                debug!(
                    "Loaded light texture {} ({}x{}) in {:?}",
                    path.display(),
                    t.width(),
                    t.height(),
                    started.elapsed()
                );
                Some(std::sync::Arc::new(t))
            }
            Err(e) => {
                cause_warning!(LightMapUnreadable, "{e} — the light renders untextured");
                None
            }
        }
    }

    fn load_ies(&self, path: &Path) -> Option<std::sync::Arc<IesProfile>> {
        let loaded = load_ies(path);
        match &loaded {
            Some(p) => debug!(
                "Loaded IES profile {} (power {:.3})",
                path.display(),
                p.power()
            ),
            None => cause_warning!(
                IesUnreadable,
                "Could not load IES profile {}",
                path.display()
            ),
        }
        loaded.map(std::sync::Arc::new)
    }

    fn load_ptex(
        &self,
        path: &Path,
        space: crust_core::ColorSpace,
    ) -> Option<std::sync::Arc<dyn PtexTexture>> {
        // A/B switch, in the spirit of CRUST_MESH_BAKE: decline every texture
        // so the same scene renders on its constant `baseColor` fallback. That
        // is how you tell "the Ptex lookup is wrong" from "the material or the
        // lighting is wrong", since both show up as an off-colour surface.
        if !self.config.ptex {
            debug!("CRUST_PTEX=0: ignoring {}", path.display());
            return None;
        }
        let started = Instant::now();
        // Why this texture ends up preloaded, if it does — reported apart,
        // since "declined by policy" and "streaming broke" read very
        // differently in the stats block.
        let mut why = PreloadReason::NotStreaming;
        // Streaming first when it is on, preloading when it is not or when
        // the file declines it. The fallback matters for the same reason the
        // `.tx` path's does: turning residency on can make a render slower
        // but must never break one, so a `.ptx` this cannot open tile-wise
        // still renders — just resident.
        // Is there budget left for another reader? Checked before the open,
        // because it needs no file I/O at all — and because a reader admitted
        // past the cap would either push the total over the budget (the old
        // floor's bug) or get a share too small to hold one block, which
        // caches nothing. Preloading is the honest answer to both.
        let room = {
            let opened = self.ptex.lock().unwrap_or_else(|e| e.into_inner());
            Self::streamed_count(&opened) < self.max_streams()
        };
        if self.ptex_streaming && !room {
            why = PreloadReason::BudgetFull;
            debug!(
                "Ptex {}: the {:.0} MiB budget already has {} readers, its most \
                 at {:.0} MiB each — preloading this one instead",
                path.display(),
                self.ptex_budget() as f64 / (1024.0 * 1024.0),
                self.max_streams(),
                MIN_PTEX_SHARE as f64 / (1024.0 * 1024.0),
            );
        }
        if self.ptex_streaming && room {
            match PtexStream::open_config_in(path, space, &self.config) {
                Ok(tex) => {
                    // Admission. Opening read headers only, so this costs a
                    // seek and answers exactly: a texture that would preload
                    // for less than the cache slot it is about to occupy is
                    // cheaper resident than streamed. See
                    // `DEFAULT_STREAM_MIN_MB` for the island distribution that
                    // makes this necessary rather than tidy.
                    let would = tex.preload_bytes(self.preload_max_log2());
                    let floor = ptex_stream::stream_min_bytes(&self.config);
                    if would < floor {
                        why = PreloadReason::TooSmall;
                        debug!(
                            "Ptex {} would preload in {:.2} MiB, under the {:.0} MiB \
                             streaming floor — preloading it instead",
                            path.display(),
                            would as f64 / (1024.0 * 1024.0),
                            floor as f64 / (1024.0 * 1024.0),
                        );
                    } else if self.ptex_mip_space == ptex_stream::MipSpace::Linear
                        && !tex.chain_is_exact()
                    {
                        // **The correctness gate, and the project's own
                        // standard for it.** A `.tx` whose levels were
                        // reduced in the wrong colour space is refused
                        // (`crust:mipspace`) rather than described, because
                        // the failure is invisible: level 0 stays right and
                        // every coarser level is wrong, which by eye is a
                        // filtering bug. A `.ptx`'s stored chain is reduced
                        // in the file's encoding while crust decodes Ptex by
                        // 2.2, so it is that same mismatch every time — and
                        // gets the same answer. Preloading rebuilds the
                        // pyramid in linear light from the decoded base.
                        //
                        // Checked after the size test on purpose: on a
                        // production stage the size rule accounts for 95% of
                        // preloads and is the boring reason, so leaving it
                        // first keeps this line meaning what it says.
                        why = PreloadReason::MipSpace;
                        debug!(
                            "Ptex {} has a mip chain reduced in the file's own encoding — \
                             preloading it so the pyramid is built in linear light \
                             (the default CRUST_PTEX_STREAM_MIPSPACE=capped streams it with \
                             that pyramid)",
                            path.display(),
                        );
                    } else {
                        let tex = std::sync::Arc::new(tex);
                        let mut opened = self.ptex.lock().unwrap_or_else(|e| e.into_inner());
                        opened.push(PtexHandle::Streamed(tex.clone()));
                        // Every sibling's share shrinks as this one joins, so the
                        // total stays what was asked for rather than growing with
                        // the texture count.
                        self.rebudget_ptex(&opened);
                        debug!(
                            "Streaming Ptex {} ({} faces) opened in {:?} — {} textures now sharing \
                         {:.0} MiB",
                            path.display(),
                            PtexTexture::num_faces(tex.as_ref()),
                            started.elapsed(),
                            opened.len(),
                            self.ptex_budget() as f64 / (1024.0 * 1024.0),
                        );
                        return Some(tex);
                    }
                }
                Err(e) => {
                    why = PreloadReason::StreamFailed;
                    warning!(
                        TextureStreamFallback,
                        "{e} — could not stream it, preloading instead"
                    );
                }
            }
        }
        match PtexColor::open_in(path, space, self.config.ptex_mip, self.preload_max_log2()) {
            Ok(tex) => {
                debug!(
                    "Loaded Ptex {} ({} faces, {:.1} MiB resident, {space:?}) in {:?}",
                    path.display(),
                    PtexTexture::num_faces(&tex),
                    tex.bytes() as f64 / (1024.0 * 1024.0),
                    started.elapsed()
                );
                self.ptex
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(PtexHandle::Preloaded {
                        faces: PtexTexture::num_faces(&tex),
                        bytes: tex.bytes(),
                        why,
                    });
                Some(std::sync::Arc::new(tex))
            }
            Err(e) => {
                cause_warning!(
                    TextureUnreadable,
                    "{e} — the surface uses its constant base colour"
                );
                None
            }
        }
    }
}
