//! File-based dome-light textures.
//!
//! The capi plays the `AssetLoader` role for the Hydra delegate that the
//! CLI plays for batch renders, so the decode semantics here mirror
//! `crust-render/src/main.rs`'s `CliAssets` exactly: `.exr` via the `exr`
//! crate (values already linear), `.hdr` via the `image` crate
//! (passthrough), any other `image`-supported format decoded then pushed
//! through the piecewise sRGB EOTF so an LDR sky lights the scene in
//! linear light. Row order is file order — row 0 at the top (+Y pole),
//! `EnvironmentMap`'s convention.

use crust_core::{EnvironmentMap, Vec3A};
use exr::prelude::read_first_rgba_layer_from_file;
use std::path::Path;

pub(crate) fn load_environment(path: &Path) -> Option<EnvironmentMap> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("exr") => load_exr(path),
        _ => load_image(path),
    }
}

fn load_exr(path: &Path) -> Option<EnvironmentMap> {
    let image = read_first_rgba_layer_from_file(
        path,
        |resolution, _| {
            let (w, h) = (resolution.width(), resolution.height());
            (w, h, vec![Vec3A::ZERO; w * h])
        },
        |(w, _h, pixels): &mut (usize, usize, Vec<Vec3A>),
         pos,
         (r, g, b, _a): (f32, f32, f32, f32)| {
            pixels[pos.y() * *w + pos.x()] = Vec3A::new(r, g, b);
        },
    )
    .ok()?;
    let (w, h, pixels) = image.layer_data.channel_data.pixels;
    EnvironmentMap::new(w, h, pixels)
}

fn load_image(path: &Path) -> Option<EnvironmentMap> {
    // The default 512 MiB decode-allocation limit is well below a
    // production panorama (a 16k HDRI); this is a trusted, locally-authored
    // asset, so lift it — same reasoning as the CLI.
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    reader.no_limits();
    let decoded = reader.decode().ok()?;
    let rgb = decoded.to_rgb32f();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    // `to_rgb32f` keeps HDR values as authored but rescales integer formats
    // to 0..1 *without* removing their sRGB transfer curve — undo it for
    // those.
    let is_hdr = matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("hdr")
    );
    let to_linear = |c: f32| {
        if is_hdr {
            c
        } else if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let pixels = rgb
        .pixels()
        .map(|p| Vec3A::new(to_linear(p[0]), to_linear(p[1]), to_linear(p[2])))
        .collect();
    EnvironmentMap::new(w, h, pixels)
}
