//! Lat-long environment maps: OpenEXR, Radiance `.hdr`, and LDR images.

use crust_core::{EnvironmentMap, Vec3A};
use exr::prelude::*;
use std::path::Path;
use tracing::error;

pub fn load_exr_environment(path: &Path) -> Option<EnvironmentMap> {
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
    .map_err(|e| error!("EXR decode failed for {}: {e}", path.display()))
    .ok()?;
    let (w, h, pixels) = image.layer_data.channel_data.pixels;
    EnvironmentMap::new(w, h, pixels)
}

/// An EXR's RGB samples, interleaved, row-major, linear — nothing else.
///
/// The same decode [`load_exr_environment`] does, without the importance-
/// sampling structure an [`EnvironmentMap`] builds on top. It exists for the
/// `.tx` converter, which needs the pixels and none of the rest, and it lives
/// here so there is still exactly one place in the workspace that knows how to
/// read an EXR.
pub fn read_exr_rgb(path: &Path) -> Option<(Vec<f32>, usize, usize)> {
    let image = read_first_rgba_layer_from_file(
        path,
        |resolution, _| {
            let (w, h) = (resolution.width(), resolution.height());
            (w, h, vec![0.0f32; w * h * 3])
        },
        |(w, _h, pixels): &mut (usize, usize, Vec<f32>),
         pos,
         (r, g, b, _a): (f32, f32, f32, f32)| {
            let o = (pos.y() * *w + pos.x()) * 3;
            pixels[o] = r;
            pixels[o + 1] = g;
            pixels[o + 2] = b;
        },
    )
    .map_err(|e| error!("EXR decode failed for {}: {e}", path.display()))
    .ok()?;
    let (w, h, pixels) = image.layer_data.channel_data.pixels;
    Some((pixels, w, h))
}

pub fn load_image_environment(path: &Path) -> Option<EnvironmentMap> {
    // `image::open`'s default 512MiB decode-allocation limit is well below a
    // production-scale panorama (e.g. a 16k HDRI): lift it for this trusted,
    // locally-authored asset rather than have large dome lights fail to load.
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| error!("Image decode failed for {}: {e}", path.display()))
        .ok()?
        .with_guessed_format()
        .map_err(|e| error!("Image decode failed for {}: {e}", path.display()))
        .ok()?;
    reader.no_limits();
    let decoded = reader
        .decode()
        .map_err(|e| error!("Image decode failed for {}: {e}", path.display()))
        .ok()?;
    let rgb = decoded.to_rgb32f();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    // `to_rgb32f` keeps HDR values as authored, but rescales integer
    // formats to 0..1 *without* removing their sRGB transfer curve. Undo it
    // for those, so an LDR sky lights the scene in linear light.
    let is_hdr = matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("hdr")
    );
    let to_linear = |c: f32| if is_hdr { c } else { crate::srgb_to_linear(c) };
    let pixels = rgb
        .pixels()
        .map(|p| Vec3A::new(to_linear(p[0]), to_linear(p[1]), to_linear(p[2])))
        .collect();
    EnvironmentMap::new(w, h, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host side of the asset seam: an EXR written to disk must come
    /// back as pixels the engine can build a map from, with the geometry
    /// and values intact. `crust-core` cannot test this — it has no
    /// decoder, which is the whole point of the split.
    #[test]
    fn exr_environment_round_trips() {
        let dir = std::env::temp_dir().join("crust_env_round_trip");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("env.exr");

        let (w, h) = (8usize, 4usize);
        // A value per texel that is unmistakable and not 0..1, so an
        // accidental LDR clamp or gamma would show up.
        let value = |x: usize, y: usize| (x as f32 + 10.0 * y as f32, 2.0, 0.5);
        exr::prelude::write_rgb_file(&path, w, h, value).expect("write exr");

        let map = load_exr_environment(&path).expect("decode the EXR we just wrote");
        assert_eq!((map.width(), map.height()), (w, h));

        // Row 0 is the +Y pole by convention, so straight up must read the
        // first row. Sampling nearest-texel, +Y lands in column 0.
        let up = map.radiance(Vec3A::Y);
        assert!(
            (up.x - value(0, 0).0).abs() < 1e-4 && (up.y - 2.0).abs() < 1e-4,
            "top-row lookup returned {up:?}"
        );

        // And a high dynamic range value survives unclamped.
        let low = map.radiance(-Vec3A::Y);
        assert!(low.x > 20.0, "bottom row was clamped or gamma'd: {low:?}");

        let _ = std::fs::remove_file(&path);
    }

    /// LDR images are sRGB-encoded; the renderer works in linear light, so
    /// the loader must undo the transfer curve or an image-based sky is
    /// noticeably wrong.
    #[test]
    fn ldr_images_are_converted_to_linear() {
        let dir = std::env::temp_dir().join("crust_env_round_trip");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("env.png");

        // Mid-grey in sRGB (188/255 ~ 0.7373) is ~0.5 in linear light.
        let mut img = image::RgbImage::new(4, 2);
        for p in img.pixels_mut() {
            *p = image::Rgb([188, 188, 188]);
        }
        img.save(&path).expect("write png");

        let map = load_image_environment(&path).expect("decode the PNG we just wrote");
        let c = map.radiance(Vec3A::Y);
        assert!(
            (c.x - 0.5).abs() < 0.02,
            "sRGB 188 should be ~0.5 linear, got {}",
            c.x
        );

        let _ = std::fs::remove_file(&path);
    }
}
