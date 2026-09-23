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
mod ies;
mod ptex_stream;
mod ptex_texture;
pub mod tiled;
mod uv_texture;

pub use environment::{load_exr_environment, load_image_environment, read_exr_rgb, read_rgb_image};
pub use ies::{load_ies, parse_ies};
pub use ptex_stream::{
    DEFAULT_CACHE_MB as PTEX_DEFAULT_CACHE_MB, DEFAULT_STREAM_MIN_MB as PTEX_DEFAULT_STREAM_MIN_MB,
    MICRO_SLOTS as PTEX_MICRO_SLOTS, MipSpace as PtexMipSpace, PtexStream,
    StreamStats as PtexStreamStats, cache_budget_from_env as ptex_cache_budget_from_env,
    micro_reserve as ptex_micro_reserve, micro_retained_bytes as ptex_micro_retained_bytes,
    micro_slot_max as ptex_micro_slot_max, micro_threads as ptex_micro_threads,
    mip_space_from_env as ptex_mip_space_from_env, stream_enabled as ptex_stream_enabled,
    stream_min_bytes_from_env as ptex_stream_min_bytes_from_env,
};
pub use ptex_texture::{
    DEFAULT_MAX_LOG2, PtexColor, max_log2_from_env, max_log2_from_env_opt, read_channel,
};
pub use uv_texture::{DEFAULT_MAX_EDGE, UvTexture};

use crust_core::{
    AssetLoader, ColorSpace, EnvironmentMap, IesProfile, LightTexture, PtexTexture, Texture2D,
};
use std::path::Path;
use std::time::Instant;
use tracing::{debug, error, info};

/// The piecewise sRGB EOTF: display-encoded `[0, 1]` to linear.
///
/// One definition, used by both the LDR environment decoder and the UV
/// texture's lookup table, so the two cannot drift apart. `f32` throughout,
/// which is what both callers computed before it was shared — the results
/// are bit-identical.
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// One encoded sample, in `space`, as linear light.
///
/// The scalar form of the 256-entry table the texture decoders build. It exists
/// for the one caller that has samples rather than bytes — the `.tx` converter,
/// which must linearise a display-encoded source *once* before writing it to a
/// float file that has no transfer curve of its own. Defined here so that
/// caller cannot invent a second sRGB curve.
#[inline]
pub fn to_linear(space: ColorSpace, encoded: f32) -> f32 {
    match space.gamma() {
        Some(g) => encoded.max(0.0).powf(g),
        None => match space {
            ColorSpace::Srgb => srgb_to_linear(encoded),
            _ => encoded,
        },
    }
}

/// The inverse of [`srgb_to_linear`]: linear `[0, 1]` back to display-encoded.
///
/// Needed only to re-encode a mip level after averaging its parents in linear
/// light. Levels are stored `u8` like the file they came from, so the
/// averaging has to round-trip through the transfer function — and doing the
/// averaging in linear is the whole point, since summing display-encoded
/// values is not summing light.
#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// The host side of `crust_core::AssetLoader`, reading from the filesystem.
///
/// The engine asks for pixels, this decodes them: OpenEXR through `exr`,
/// Radiance `.hdr` and LDR images through `image`, per-face Ptex through
/// `ptex-rs`. LDR pixels are un-gamma'd to linear, since the renderer works
/// in linear light and an sRGB-encoded sky would be noticeably wrong.
pub struct FileAssets {
    /// The tile cache every streaming texture shares. Present whether or not
    /// streaming is on, so the counters can be reported either way — an empty
    /// one costs 64 empty maps.
    cache: std::sync::Arc<tiled::TileCache>,
    /// Read once at construction, not per `load_texture`: the switch decides
    /// which backend every texture in the render uses, and reading the
    /// environment per call would let it change mid-import.
    streaming: bool,
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
    /// efficiency one, and under the default policy the reason *most*
    /// mipmapped textures preload, so it is reported on its own line.
    MipSpace,
    StreamFailed,
}

impl Default for FileAssets {
    fn default() -> Self {
        FileAssets::new()
    }
}

impl FileAssets {
    pub fn new() -> FileAssets {
        let streaming = std::env::var("CRUST_TEX_STREAM").as_deref() == Ok("1");
        let budget = tiled::TileCache::budget_from_env();
        if streaming {
            info!(
                "Streaming textures from .tx with a {:.0} MiB tile cache",
                budget as f64 / (1024.0 * 1024.0)
            );
        }
        let ptex_streaming = ptex_stream::stream_enabled();
        let ptex_mip_space = ptex_stream::mip_space_from_env();
        if ptex_streaming {
            info!(
                "Streaming Ptex with a {:.0} MiB cache",
                ptex_stream::cache_budget_from_env() as f64 / (1024.0 * 1024.0)
            );
            // Said at construction rather than per texture, because under
            // the default policy it is the line that explains a render where
            // streaming was asked for and nothing streamed.
            match ptex_mip_space {
                ptex_stream::MipSpace::Linear => info!(
                    "Ptex mip chains must be reduced in linear light, so a mipmapped .ptx \
                     preloads — CRUST_PTEX_STREAM_MIPSPACE=file takes the file's own chain \
                     instead (darker minified texture; see docs/ptex_streaming.md)"
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
            uv_texture::max_edge_from_env(),
            if ptex_streaming {
                "streaming"
            } else {
                "preloaded"
            },
            max_log2_from_env(),
        );
        FileAssets {
            cache: std::sync::Arc::new(tiled::TileCache::new(budget)),
            streaming,
            ptex_streaming,
            ptex_mip_space,
            ptex: std::sync::Mutex::new(Vec::new()),
        }
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
        let share = Self::ptex_reader_total() / streams.len();
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
    fn max_streams() -> usize {
        (Self::ptex_reader_total() / MIN_PTEX_SHARE).max(1)
    }

    /// The render's Ptex budget less the thread-local microcaches' share.
    ///
    /// What is left for the readers, and the number every division below is
    /// against — so the two halves of Ptex residency sum to the configured
    /// total rather than the readers alone matching it.
    fn ptex_reader_total() -> usize {
        let total = ptex_stream::cache_budget_from_env();
        total - ptex_stream::micro_reserve(total)
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
            micro_reserve_bytes: ptex_stream::micro_reserve(ptex_stream::cache_budget_from_env())
                as u64,
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
        let c = self.cache.counters();
        crust_core::TextureCacheStats {
            micro_hits: c.micro_hits,
            hits: c.hits,
            misses: c.misses,
            redundant: c.redundant,
            raced: c.raced,
            evictions: c.evictions,
            bytes_read: c.bytes_read,
            peak_bytes: c.peak_bytes,
            errors: c.errors,
            budget_bytes: c.budget_bytes,
        }
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
    ) -> Option<std::sync::Arc<dyn Texture2D>> {
        for candidate in Self::stream_candidates(path) {
            let started = Instant::now();
            let Some(tex) =
                tiled::StreamingTexture::open(&candidate, space, self.cache.clone(), |u, v| {
                    let name = candidate.to_string_lossy();
                    uv_texture::expand_token(&name, u, v).map(std::path::PathBuf::from)
                })
            else {
                continue;
            };
            let (w, h) = tex.size();
            debug!(
                "Streaming texture {} ({} chart(s), {}x{} level 0, {} level(s), {} tiles, {:?}) \
                 in {:?}",
                candidate.display(),
                tex.chart_count(),
                w,
                h,
                tex.level_count(),
                if tex.is_linear() { "half" } else { "8-bit" },
                space,
                started.elapsed()
            );
            return Some(std::sync::Arc::new(tex));
        }
        None
    }
}

impl AssetLoader for FileAssets {
    fn load_environment(&self, path: &Path) -> Option<EnvironmentMap> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let started = Instant::now();
        let loaded = match ext.as_str() {
            "exr" => load_exr_environment(path),
            _ => load_image_environment(path),
        };
        match &loaded {
            Some(map) => debug!(
                "Loaded environment {} ({}x{}) in {:?}",
                path.display(),
                map.width(),
                map.height(),
                started.elapsed()
            ),
            None => error!("Could not load environment {}", path.display()),
        }
        loaded
    }

    fn load_texture(
        &self,
        path: &Path,
        space: ColorSpace,
    ) -> Option<std::sync::Arc<dyn Texture2D>> {
        // Same A/B switch as CRUST_PTEX, for the same reason: with every
        // texture declined a MaterialX surface renders on its constant
        // inputs, which is how you tell a wrong chart from a wrong material.
        if std::env::var("CRUST_TEX").as_deref() == Ok("0") {
            debug!("CRUST_TEX=0: ignoring {}", path.display());
            return None;
        }
        // Streaming first, preloading as the fallback. Anything the streaming
        // path declines — no `.tx` beside the asset, a mip chain reduced in
        // another colour space, a file it cannot read — lands here, so turning
        // the switch on can make a render slower but never break it.
        if self.streaming
            && let Some(streamed) = self.open_streaming(path, space)
        {
            return Some(streamed);
        }
        if self.streaming {
            debug!(
                "No .tx backing for {} — preloading it instead",
                path.display()
            );
        }
        let started = Instant::now();
        let loaded = UvTexture::open(path, space)?;
        // The space *resolved* against the file, not the one asked for: under
        // UsdUVTexture's `auto` the two differ, and the resolved one is what
        // answers "why is this map darker than expected".
        debug!(
            "Loaded texture {} ({} tile(s), {}x{} each, {}, {:.1} MiB resident, {:?}) in {:?}",
            path.display(),
            loaded.tile_count(),
            loaded.tile_size().0,
            loaded.tile_size().1,
            if loaded.is_float() { "f32" } else { "8-bit" },
            loaded.bytes() as f64 / (1024.0 * 1024.0),
            loaded.color_space(),
            started.elapsed()
        );
        Some(std::sync::Arc::new(loaded))
    }

    fn load_light_texture(&self, path: &Path) -> Option<std::sync::Arc<LightTexture>> {
        let started = Instant::now();
        let loaded = read_rgb_image(path).and_then(|(w, h, px)| LightTexture::new(w, h, px));
        match &loaded {
            Some(t) => debug!(
                "Loaded light texture {} ({}x{}) in {:?}",
                path.display(),
                t.width(),
                t.height(),
                started.elapsed()
            ),
            None => error!("Could not load light texture {}", path.display()),
        }
        loaded.map(std::sync::Arc::new)
    }

    fn load_ies(&self, path: &Path) -> Option<std::sync::Arc<IesProfile>> {
        let loaded = load_ies(path);
        match &loaded {
            Some(p) => debug!(
                "Loaded IES profile {} (power {:.3})",
                path.display(),
                p.power()
            ),
            None => error!("Could not load IES profile {}", path.display()),
        }
        loaded.map(std::sync::Arc::new)
    }

    fn load_ptex(&self, path: &Path) -> Option<std::sync::Arc<dyn PtexTexture>> {
        // A/B switch, in the spirit of CRUST_MESH_BAKE: decline every texture
        // so the same scene renders on its constant `baseColor` fallback. That
        // is how you tell "the Ptex lookup is wrong" from "the material or the
        // lighting is wrong", since both show up as an off-colour surface.
        if std::env::var("CRUST_PTEX").as_deref() == Ok("0") {
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
            Self::streamed_count(&opened) < Self::max_streams()
        };
        if self.ptex_streaming && !room {
            why = PreloadReason::BudgetFull;
            debug!(
                "Ptex {}: the {:.0} MiB budget already has {} readers, its most \
                 at {:.0} MiB each — preloading this one instead",
                path.display(),
                ptex_stream::cache_budget_from_env() as f64 / (1024.0 * 1024.0),
                Self::max_streams(),
                MIN_PTEX_SHARE as f64 / (1024.0 * 1024.0),
            );
        }
        if self.ptex_streaming && room {
            match PtexStream::open(path) {
                Ok(tex) => {
                    // Admission. Opening read headers only, so this costs a
                    // seek and answers exactly: a texture that would preload
                    // for less than the cache slot it is about to occupy is
                    // cheaper resident than streamed. See
                    // `DEFAULT_STREAM_MIN_MB` for the island distribution that
                    // makes this necessary rather than tidy.
                    let would = tex.preload_bytes(max_log2_from_env());
                    let floor = ptex_stream::stream_min_bytes_from_env();
                    if would < floor {
                        why = PreloadReason::TooSmall;
                        debug!(
                            "Ptex {} would preload in {:.2} MiB, under the {:.0} MiB                              streaming floor — preloading it instead",
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
                             (CRUST_PTEX_STREAM_MIPSPACE=file to stream it anyway)",
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
                            ptex_stream::cache_budget_from_env() as f64 / (1024.0 * 1024.0),
                        );
                        return Some(tex);
                    }
                }
                Err(e) => {
                    why = PreloadReason::StreamFailed;
                    error!(
                        "Could not stream Ptex {}: {e} — preloading instead",
                        path.display()
                    );
                }
            }
        }
        match PtexColor::open(path) {
            Ok(tex) => {
                debug!(
                    "Loaded Ptex {} ({} faces, {:.1} MiB resident) in {:?}",
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
                error!("Could not load Ptex {}: {e}", path.display());
                None
            }
        }
    }
}
