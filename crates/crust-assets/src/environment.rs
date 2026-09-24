//! Lat-long environment maps and light textures: OpenEXR, Radiance `.hdr`,
//! and LDR images, all decoded to linear float RGB by [`read_rgb_image`].

use crust_core::{EnvironmentMap, Vec3A};
use exr::prelude::*;
use std::path::Path;
use tracing::error;

/// A light's image as linear float RGB, row-major with row 0 at the top:
/// `(width, height, pixels)`. EXR by extension, everything else through
/// `image` (`.hdr` kept as authored, integer formats un-gamma'd from sRGB).
///
/// This is the decode dome lights have always had, shared with
/// `RectLight`'s `inputs:texture:file`. It is deliberately *not* the
/// UV-texture path: that one narrows to 8 bits when preloading, and a
/// light's texture is exactly where the range above 1.0 matters.
pub fn read_rgb_image(path: &Path) -> Option<(usize, usize, Vec<Vec3A>)> {
    let is_exr = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exr"));
    if is_exr {
        decode_exr_pixels(path)
    } else {
        decode_image_pixels(path)
    }
}

pub fn load_exr_environment(path: &Path) -> Option<EnvironmentMap> {
    let (w, h, pixels) = decode_exr_pixels(path)?;
    EnvironmentMap::new(w, h, pixels)
}

fn decode_exr_pixels(path: &Path) -> Option<(usize, usize, Vec<Vec3A>)> {
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
    Some(image.layer_data.channel_data.pixels)
}

/// An EXR's RGB samples, interleaved, row-major, linear — nothing else.
///
/// The same decode [`load_exr_environment`] does, without the importance-
/// sampling structure an [`EnvironmentMap`] builds on top. It exists for the
/// `.tx` converter and the preloaded UV texture path, which need the pixels
/// and none of the rest, and it lives here so there is still exactly one place
/// in the workspace that knows how to read an EXR.
///
/// **Any channel layout, not just RGBA.** Texture EXRs are routinely single
/// channel — a roughness or metallic map — and name their channels with a
/// layer prefix: ALab writes `rgb.R` alone for 3 584 of its 6 832 maps, and
/// `rgb.R`/`rgb.G`/`rgb.B` for most of the rest. The RGBA convenience reader
/// refuses both ("no layer in the image matched"), so this reads the first
/// layer's channels as they are and picks `R`, `G`, `B` by *base* name (the
/// part after the last `.`). A layer with none of them but exactly one channel
/// — or a `Y` luminance channel — is replicated into all three, which is what
/// the streaming EXR reader already does; a missing colour channel otherwise
/// reads 0.
pub fn read_exr_rgb(path: &Path) -> Option<(Vec<f32>, usize, usize)> {
    let image = read_first_flat_layer_from_file(path)
        .map_err(|e| error!("EXR decode failed for {}: {e}", path.display()))
        .ok()?;
    let layer = &image.layer_data;
    let (w, h) = (layer.size.width(), layer.size.height());
    let channels = &layer.channel_data.list;
    let base = |c: &AnyChannel<FlatSamples>| {
        let name = c.name.to_string();
        name.rsplit('.').next().unwrap_or_default().to_owned()
    };
    let find = |want: &str| channels.iter().position(|c| base(c) == want);
    let mono = find("Y").or_else(|| (channels.len() == 1).then_some(0));
    let pick = |want: &str| find(want).or(mono);
    let rgb = [pick("R"), pick("G"), pick("B")];
    if rgb.iter().all(Option::is_none) {
        error!(
            "EXR decode failed for {}: no R, G, B or Y channel among {:?}",
            path.display(),
            channels
                .iter()
                .map(|c| c.name.to_string())
                .collect::<Vec<_>>()
        );
        return None;
    }
    let mut pixels = vec![0.0f32; w * h * 3];
    for (k, channel) in rgb.iter().enumerate() {
        let Some(i) = channel else { continue };
        for (t, v) in channels[*i]
            .sample_data
            .values_as_f32()
            .enumerate()
            .take(w * h)
        {
            pixels[t * 3 + k] = v;
        }
    }
    Some((pixels, w, h))
}

pub fn load_image_environment(path: &Path) -> Option<EnvironmentMap> {
    let (w, h, pixels) = decode_image_pixels(path)?;
    EnvironmentMap::new(w, h, pixels)
}

fn decode_image_pixels(path: &Path) -> Option<(usize, usize, Vec<Vec3A>)> {
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
    Some((w, h, pixels))
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

    /// A light texture keeps an EXR's range and linearises an 8-bit PNG —
    /// the two ways a `RectLight` card is commonly authored.
    #[test]
    fn rgb_image_keeps_hdr_range_and_linearises_ldr() {
        let dir = std::env::temp_dir().join("crust_rgb_image");
        std::fs::create_dir_all(&dir).expect("temp dir");

        let exr_path = dir.join("card.exr");
        exr::prelude::write_rgb_file(&exr_path, 2, 1, |x, _| (8.0 * (x + 1) as f32, 0.5, 0.0))
            .expect("write exr");
        let (w, h, px) = read_rgb_image(&exr_path).expect("exr decodes");
        assert_eq!((w, h), (2, 1));
        assert_eq!(px[1], Vec3A::new(16.0, 0.5, 0.0), "no clamp above 1.0");

        let png_path = dir.join("card.png");
        image::RgbImage::from_pixel(1, 1, image::Rgb([255, 128, 0]))
            .save(&png_path)
            .expect("write png");
        let (_, _, px) = read_rgb_image(&png_path).expect("png decodes");
        assert!((px[0].x - 1.0).abs() < 1e-6);
        let mid = crate::srgb_to_linear(128.0 / 255.0);
        assert!(
            (px[0].y - mid).abs() < 1e-6 && mid < 0.25,
            "sRGB-decoded: {}",
            px[0].y
        );

        let _ = std::fs::remove_file(&exr_path);
        let _ = std::fs::remove_file(&png_path);
    }
}
