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

mod environment;
mod ptex_texture;
mod uv_texture;

pub use environment::{load_exr_environment, load_image_environment};
pub use ptex_texture::{DEFAULT_MAX_LOG2, PtexColor, max_log2_from_env, read_channel};
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

/// The host side of `crust_core::AssetLoader`, reading from the filesystem.
///
/// The engine asks for pixels, this decodes them: OpenEXR through `exr`,
/// Radiance `.hdr` and LDR images through `image`, per-face Ptex through
/// `ptex-rs`. LDR pixels are un-gamma'd to linear, since the renderer works
/// in linear light and an sRGB-encoded sky would be noticeably wrong.
pub struct FileAssets;

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

