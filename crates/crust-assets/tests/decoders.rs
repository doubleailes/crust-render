//! The host side of the asset seam, tested end to end: file decoding into
//! the samplers `crust-core` consumes, using the images checked into
//! `samples/` and small files written on the fly.

use crust_assets::{
    DEFAULT_MAX_EDGE, DEFAULT_MAX_LOG2, FileAssets, PtexColor, UvTexture, load_exr_environment,
    load_image_environment, max_log2_from_env, read_channel, srgb_to_linear,
};
use crust_core::{AssetLoader, ColorSpace, Texture2D, Vec3A};
use std::path::PathBuf;

fn samples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_assets_tests").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// sRGB
// ---------------------------------------------------------------------------

#[test]
fn srgb_to_linear_is_anchored_and_monotonic() {
    assert_eq!(srgb_to_linear(0.0), 0.0);
    assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
    let mut prev = -1.0;
    for i in 0..=255 {
        let v = srgb_to_linear(i as f32 / 255.0);
        assert!(v > prev, "not monotonic at {i}");
        prev = v;
    }
}

#[test]
fn srgb_to_linear_matches_the_reference_points() {
    // 0.5 display ≈ 0.214 linear; 188/255 ≈ 0.5 linear.
    assert!((srgb_to_linear(0.5) - 0.2140).abs() < 1e-3);
    assert!((srgb_to_linear(188.0 / 255.0) - 0.5).abs() < 0.01);
    // The linear toe: below the knee it is a plain division.
    assert!((srgb_to_linear(0.02) - 0.02 / 12.92).abs() < 1e-7);
}

#[test]
fn srgb_to_linear_inverts_the_encoding_curve() {
    let encode = |l: f32| {
        if l <= 0.0031308 {
            12.92 * l
        } else {
            1.055 * l.powf(1.0 / 2.4) - 0.055
        }
    };
    for l in [0.001f32, 0.01, 0.1, 0.5, 0.9] {
        assert!((srgb_to_linear(encode(l)) - l).abs() < 1e-5, "{l}");
    }
}

// ---------------------------------------------------------------------------
// Environment maps
// ---------------------------------------------------------------------------

#[test]
fn the_sample_sky_exr_decodes() {
    let map = load_exr_environment(&samples().join("sky_env.exr")).expect("sky_env.exr decodes");
    assert!(map.width() > 0 && map.height() > 0);
    // A lat-long panorama is twice as wide as it is tall.
    assert_eq!(map.width(), 2 * map.height());
    // It must be sampleable, and the sampled direction must be unit length.
    let (dir, radiance, pdf) = map.sample(0.3, 0.6).expect("not black");
    assert!((dir.length() - 1.0).abs() < 1e-4);
    assert!(radiance.min_element() >= 0.0);
    assert!(pdf > 0.0);
}

#[test]
fn file_assets_dispatches_on_extension() {
    let map = FileAssets
        .load_environment(&samples().join("sky_env.exr"))
        .expect("exr through the trait");
    assert_eq!(map.width(), 2 * map.height());
}

#[test]
fn missing_files_decline_rather_than_panic() {
    let ghost = samples().join("does_not_exist.exr");
    assert!(load_exr_environment(&ghost).is_none());
    assert!(load_image_environment(&samples().join("does_not_exist.png")).is_none());
    assert!(FileAssets.load_environment(&ghost).is_none());
    assert!(FileAssets.load_ptex(&samples().join("does_not_exist.ptx")).is_none());
    assert!(FileAssets
        .load_texture(&samples().join("does_not_exist.png"), ColorSpace::Raw)
        .is_none());
    assert!(PtexColor::open(&samples().join("does_not_exist.ptx")).is_err());
}

#[test]
fn a_non_image_file_declines() {
    let dir = scratch("garbage");
    let path = dir.join("not_an_image.png");
    std::fs::write(&path, b"this is not a PNG").unwrap();
    assert!(load_image_environment(&path).is_none());
    assert!(UvTexture::open(&path, ColorSpace::Raw).is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn ldr_environment_rows_map_to_the_poles() {
    let dir = scratch("ldr_env");
    let path = dir.join("poles.png");
    let mut img = image::RgbImage::new(4, 2);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = if y == 0 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) };
        let _ = x;
    }
    img.save(&path).unwrap();
    let map = load_image_environment(&path).expect("png decodes");
    assert_eq!((map.width(), map.height()), (4, 2));
    let up = map.radiance(Vec3A::Y);
    assert!(up.x > 0.99 && up.z < 0.01, "top row is red: {up}");
    let down = map.radiance(-Vec3A::Y);
    assert!(down.z > 0.99 && down.x < 0.01, "bottom row is blue: {down}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn hdr_values_survive_the_exr_path_unclamped() {
    let dir = scratch("hdr_exr");
    let path = dir.join("bright.exr");
    exr::prelude::write_rgb_file(&path, 4, 2, |_, _| (1000.0, 0.5, 0.0)).unwrap();
    let map = load_exr_environment(&path).unwrap();
    let c = map.radiance(Vec3A::X);
    assert!((c.x - 1000.0).abs() < 1e-3);
    assert!((c.y - 0.5).abs() < 1e-5);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// UV textures
// ---------------------------------------------------------------------------

#[test]
fn a_single_sample_png_loads_as_one_tile() {
    let tex = UvTexture::open(&samples().join("textures/mtlx_mask.png"), ColorSpace::Raw)
        .expect("mtlx_mask.png loads");
    assert_eq!(tex.tile_count(), 1);
    let (w, h) = tex.tile_size();
    assert!(w > 0 && h > 0);
    assert!(w <= DEFAULT_MAX_EDGE && h <= DEFAULT_MAX_EDGE);
    assert_eq!(tex.bytes(), w * h * 3, "8-bit RGB storage");
    let px = tex.eval(0.5, 0.5);
    for c in px {
        assert!((0.0..=1.0).contains(&c), "{px:?}");
    }
}

#[test]
fn a_sample_udim_set_loads_both_tiles() {
    let tex = UvTexture::open(
        &samples().join("textures/mtlx_base.<UDIM>.png"),
        ColorSpace::Srgb,
    )
    .expect("the <UDIM> set loads");
    assert_eq!(tex.tile_count(), 2, "1001 and 1002 are on disk");
    // Both tiles answer; a tile that is not on disk reads black.
    let a = tex.eval(0.5, 0.5);
    let b = tex.eval(1.5, 0.5);
    let hole = tex.eval(0.5, 1.5);
    assert!(a.iter().all(|c| c.is_finite()));
    assert!(b.iter().all(|c| c.is_finite()));
    assert_eq!(hole[..3], [0.0, 0.0, 0.0]);
}

#[test]
fn the_normal_map_set_loads_raw() {
    let tex = UvTexture::open(
        &samples().join("textures/mtlx_normal.<UDIM>.png"),
        ColorSpace::Raw,
    )
    .expect("normal set loads");
    assert_eq!(tex.tile_count(), 2);
    // A tangent-space normal map is mostly blue-ish: z ≈ 1 encodes as ~1.
    let px = tex.eval(0.5, 0.5);
    assert!(px[2] > 0.5, "normal map blue channel {px:?}");
}

#[test]
fn colour_space_changes_the_decoded_value() {
    let path = samples().join("textures/mtlx_base.<UDIM>.png");
    let raw = UvTexture::open(&path, ColorSpace::Raw).unwrap();
    let srgb = UvTexture::open(&path, ColorSpace::Srgb).unwrap();
    let g22 = UvTexture::open(&path, ColorSpace::Gamma22).unwrap();
    // Sample at a texel centre so bilinear filtering does not blend
    // neighbours (a blend of encoded values is not the encoding of a
    // blend).
    let (w, h) = raw.tile_size();
    let (u, v) = ((w as f32 * 0.3).floor() + 0.5, (h as f32 * 0.3).floor() + 0.5);
    let (u, v) = (u / w as f32, v / h as f32);
    let (r, s, g) = (raw.eval(u, v), srgb.eval(u, v), g22.eval(u, v));
    for ch in 0..3 {
        let encoded = r[ch];
        if encoded > 0.02 && encoded < 0.98 {
            assert!(s[ch] < encoded, "sRGB decode darkens midtones: {encoded} -> {}", s[ch]);
            assert!((s[ch] - srgb_to_linear(encoded)).abs() < 2e-3);
            assert!((g[ch] - encoded.powf(2.2)).abs() < 2e-3);
        }
    }
}

#[test]
fn file_assets_hands_back_a_texture2d() {
    let tex: std::sync::Arc<dyn Texture2D> = FileAssets
        .load_texture(&samples().join("textures/mtlx_mask.png"), ColorSpace::Raw)
        .expect("through the trait");
    let px = tex.eval(0.25, 0.25);
    assert!(px.iter().all(|c| c.is_finite()));
}

#[test]
fn a_written_gradient_is_sampled_at_the_right_place() {
    let dir = scratch("gradient");
    let path = dir.join("grad.png");
    let w = 64u32;
    let mut img = image::RgbImage::new(w, 8);
    for (x, _, p) in img.enumerate_pixels_mut() {
        let v = (x * 255 / (w - 1)) as u8;
        *p = image::Rgb([v, 0, 255 - v]);
    }
    img.save(&path).unwrap();
    let tex = UvTexture::open(&path, ColorSpace::Raw).expect("loads");
    let left = tex.eval(0.02, 0.5);
    let right = tex.eval(0.98, 0.5);
    assert!(left[0] < 0.1 && left[2] > 0.9, "left is blue: {left:?}");
    assert!(right[0] > 0.9 && right[2] < 0.1, "right is red: {right:?}");
    // Wrapping: u = 1.02 reads like u = 0.02.
    let wrapped = tex.eval(1.02, 0.5);
    assert!((wrapped[0] - left[0]).abs() < 0.05, "{wrapped:?} vs {left:?}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn alpha_channel_reads_one_for_rgb_files() {
    let tex = UvTexture::open(&samples().join("textures/mtlx_mask.png"), ColorSpace::Raw).unwrap();
    assert_eq!(tex.eval(0.5, 0.5)[3], 1.0);
}

#[test]
fn non_finite_coordinates_do_not_panic() {
    let tex = UvTexture::open(&samples().join("textures/mtlx_mask.png"), ColorSpace::Raw).unwrap();
    for (u, v) in [(f32::NAN, 0.5), (0.5, f32::INFINITY), (-1e30, 1e30)] {
        let px = tex.eval(u, v);
        assert_eq!(px.len(), 4);
    }
    let set = UvTexture::open(&samples().join("textures/mtlx_base.<UDIM>.png"), ColorSpace::Raw).unwrap();
    let _ = set.eval(f32::NAN, f32::NAN);
}

// ---------------------------------------------------------------------------
// Ptex helpers
// ---------------------------------------------------------------------------

#[test]
fn default_ptex_cap_is_used_without_the_env_override() {
    // The test process does not set CRUST_PTEX_MAX_LOG2, so the default
    // (32x32) applies.
    if std::env::var("CRUST_PTEX_MAX_LOG2").is_err() {
        assert_eq!(max_log2_from_env(), DEFAULT_MAX_LOG2);
        assert_eq!(DEFAULT_MAX_LOG2, 5);
    }
}

#[test]
fn read_channel_decodes_every_ptex_data_type() {
    assert_eq!(read_channel(&[200], ptex::DataType::UInt8), 200.0);
    assert_eq!(read_channel(&300u16.to_le_bytes(), ptex::DataType::UInt16), 300.0);
    assert_eq!(read_channel(&1.5f32.to_le_bytes(), ptex::DataType::Float), 1.5);
    // 0x3C00 is half-precision 1.0.
    assert_eq!(read_channel(&0x3C00u16.to_le_bytes(), ptex::DataType::Half), 1.0);
    // 0xC000 is half-precision -2.0.
    assert_eq!(read_channel(&0xC000u16.to_le_bytes(), ptex::DataType::Half), -2.0);
}
