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
mod ptex_stream;
mod ptex_texture;
pub mod tiled;
mod uv_texture;

pub use environment::{load_exr_environment, load_image_environment, read_exr_rgb};
pub use ptex_stream::{
    DEFAULT_CACHE_MB as PTEX_DEFAULT_CACHE_MB, DEFAULT_STREAM_MIN_MB as PTEX_DEFAULT_STREAM_MIN_MB,
    PtexStream, StreamStats as PtexStreamStats,
    cache_budget_from_env as ptex_cache_budget_from_env, stream_enabled as ptex_stream_enabled,
    stream_min_bytes_from_env as ptex_stream_min_bytes_from_env,
};
pub use ptex_texture::{
    DEFAULT_MAX_LOG2, PtexColor, max_log2_from_env, max_log2_from_env_opt, read_channel,
};
pub use uv_texture::{DEFAULT_MAX_EDGE, UvTexture};

use crust_core::{AssetLoader, ColorSpace, EnvironmentMap, PtexTexture, Texture2D};
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
    /// Every Ptex texture opened, so the render can be reported on and — for
    /// the streamed ones — re-budgeted as more arrive. See [`PtexHandle`].
    ptex: std::sync::Mutex<Vec<PtexHandle>>,
}

/// Floor on a streamed Ptex texture's share of the budget: 1 MiB.
///
/// A zero budget in `ptex::CacheOptions` disables caching outright — every
/// texel fetch back to a seek and an inflate — so overshooting the total beats
/// silently turning the cache off, and the report shows the overshoot.
///
/// **It is a backstop and must not be the policy**, which is the lesson the
/// island taught: at 3 618 admitted readers a 4 MiB floor asked for 14.1 GiB,
/// worse than the 5.98 GiB preload it replaced. Admission
/// (`DEFAULT_STREAM_MIN_MB`) is what keeps the count small enough that this
/// rarely fires at all — raising it is treating the symptom.
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
    Preloaded { faces: usize, bytes: usize },
    Streamed(std::sync::Arc<PtexStream>),
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
        if ptex_streaming {
            info!(
                "Streaming Ptex with a {:.0} MiB cache",
                ptex_stream::cache_budget_from_env() as f64 / (1024.0 * 1024.0)
            );
        }
        FileAssets {
            cache: std::sync::Arc::new(tiled::TileCache::new(budget)),
            streaming,
            ptex_streaming,
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
        // A backstop, not the policy. Admission (`DEFAULT_STREAM_MIN_MB`) is
        // what keeps this count small enough for the shares to be usable —
        // 39 readers on the island rather than 3 618. The floor only catches
        // a stage that still manages to admit more readers than the budget
        // has megabytes, where a share of zero would disable caching outright
        // and re-read every texel.
        let share = (ptex_stream::cache_budget_from_env() / streams.len()).max(MIN_PTEX_SHARE);
        for s in &streams {
            s.set_budget(share);
        }
    }

    /// Ptex residency and cache counters, in the shape `--stats` reports.
    ///
    /// Reports for **both** backends, because the first question the report
    /// has to answer is which one ran. Pushed into `RenderStats` by the host
    /// exactly as `texture_cache_stats` is.
    pub fn ptex_stats(&self) -> crust_core::PtexCacheStats {
        let opened = self.ptex.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = crust_core::PtexCacheStats::default();
        for h in opened.iter() {
            out.textures += 1;
            match h {
                PtexHandle::Preloaded { faces, bytes } => {
                    out.faces += *faces as u64;
                    out.preloaded_bytes += *bytes as u64;
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
            info!(
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
            Some(map) => info!(
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
        let started = Instant::now();
        let loaded = UvTexture::open(path, space)?;
        info!(
            "Loaded texture {} ({} tile(s), {}x{} each, {:.1} MiB resident, {:?}) in {:?}",
            path.display(),
            loaded.tile_count(),
            loaded.tile_size().0,
            loaded.tile_size().1,
            loaded.bytes() as f64 / (1024.0 * 1024.0),
            space,
            started.elapsed()
        );
        Some(std::sync::Arc::new(loaded))
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
        // Streaming first when it is on, preloading when it is not or when
        // the file declines it. The fallback matters for the same reason the
        // `.tx` path's does: turning residency on can make a render slower
        // but must never break one, so a `.ptx` this cannot open tile-wise
        // still renders — just resident.
        if self.ptex_streaming {
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
                        debug!(
                            "Ptex {} would preload in {:.2} MiB, under the {:.0} MiB                              streaming floor — preloading it instead",
                            path.display(),
                            would as f64 / (1024.0 * 1024.0),
                            floor as f64 / (1024.0 * 1024.0),
                        );
                    } else {
                        let tex = std::sync::Arc::new(tex);
                        let mut opened = self.ptex.lock().unwrap_or_else(|e| e.into_inner());
                        opened.push(PtexHandle::Streamed(tex.clone()));
                        // Every sibling's share shrinks as this one joins, so the
                        // total stays what was asked for rather than growing with
                        // the texture count.
                        self.rebudget_ptex(&opened);
                        info!(
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
                    error!(
                        "Could not stream Ptex {}: {e} — preloading instead",
                        path.display()
                    );
                }
            }
        }
        match PtexColor::open(path) {
            Ok(tex) => {
                info!(
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
