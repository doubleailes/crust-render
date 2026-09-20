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
    DEFAULT_CACHE_MB as PTEX_DEFAULT_CACHE_MB, PtexStream, StreamStats as PtexStreamStats,
    cache_budget_from_env as ptex_cache_budget_from_env, stream_enabled as ptex_stream_enabled,
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
        }
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
                    info!(
                        "Streaming Ptex {} ({} faces, {:.0} MiB budget) opened in {:?}",
                        path.display(),
                        PtexTexture::num_faces(&tex),
                        ptex_stream::cache_budget_from_env() as f64 / (1024.0 * 1024.0),
                        started.elapsed()
                    );
                    return Some(std::sync::Arc::new(tex));
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
                Some(std::sync::Arc::new(tex))
            }
            Err(e) => {
                error!("Could not load Ptex {}: {e}", path.display());
                None
            }
        }
    }
}
