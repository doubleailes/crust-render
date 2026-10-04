//! Textures bound into a working space on other primaries than their own:
//! the curve in the decode table, the change of primaries once per lookup,
//! and every backend — preloaded bytes, preloaded floats, a streamed `.tx`,
//! an environment map — landing on the same numbers.

use crust_assets::FileAssets;
use crust_assets::tiled::{TxFormat, make_tx};
use crust_core::color::{Space, convert, working_space};
use crust_core::{AssetLoader, ColorSpace, Vec3A};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crust_wide_gamut_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn acescg() -> Space {
    working_space("acescg").expect("the builtin config defines ACEScg")
}

/// A flat 8-bit RGB image.
fn png(path: &Path, rgb: [u8; 3]) {
    image::RgbImage::from_pixel(8, 8, image::Rgb(rgb))
        .save(path)
        .expect("png");
}

/// A flat float RGB image.
fn exr(path: &Path, rgb: [f32; 3]) {
    exr::prelude::write_rgb_file(path, 8, 8, |_, _| (rgb[0], rgb[1], rgb[2])).expect("exr");
}

fn rgb(px: [f32; 4]) -> Vec3A {
    Vec3A::new(px[0], px[1], px[2])
}

/// What a byte-encoded sRGB colour is in `working`, by the colour module
/// alone: the reference every backend must reproduce.
fn srgb_bytes_in(bytes: [u8; 3], working: Space) -> Vec3A {
    let encoded = Vec3A::new(bytes[0] as f32, bytes[1] as f32, bytes[2] as f32) / 255.0;
    convert(encoded, Space::SRGB_TEXTURE, working)
}

#[test]
fn an_srgb_texture_into_acescg_takes_the_rec709_to_ap1_matrix() {
    let dir = scratch("png");
    let p = dir.join("albedo.png");
    let bytes = [200, 60, 30];
    png(&p, bytes);
    let assets = FileAssets::new();
    let space = ColorSpace::new(Space::SRGB_TEXTURE, acescg());
    let got = rgb(assets
        .load_texture(&p, space)
        .expect("loads")
        .eval(0.5, 0.5, 0.0));
    let want = srgb_bytes_in(bytes, acescg());
    assert!((got - want).abs().max_element() < 1e-6, "{got} vs {want}");
    // And it is not the lin_rec709 value: the primaries really moved.
    let rec709 = rgb(assets
        .load_texture(&p, ColorSpace::SRGB)
        .unwrap()
        .eval(0.5, 0.5, 0.0));
    assert!(
        (got - rec709).abs().max_element() > 0.01,
        "{got} vs {rec709}"
    );
}

#[test]
fn auto_in_acescg_decodes_an_8_bit_image_as_srgb_and_converts_it() {
    let dir = scratch("auto");
    let p = dir.join("albedo.png");
    let bytes = [30, 140, 220];
    png(&p, bytes);
    let assets = FileAssets::new();
    let auto = ColorSpace::from_usd(None, acescg());
    let got = rgb(assets.load_texture(&p, auto).unwrap().eval(0.5, 0.5, 0.0));
    let want = srgb_bytes_in(bytes, acescg());
    assert!((got - want).abs().max_element() < 1e-6, "{got} vs {want}");
}

#[test]
fn a_float_texture_is_converted_whole_at_load() {
    let dir = scratch("exr");
    let p = dir.join("radiance.exr");
    let value = [2.0, 0.5, 0.25];
    exr(&p, value);
    let assets = FileAssets::new();
    let space = ColorSpace::from_mtlx(Some("lin_rec709"), acescg());
    let got = rgb(assets.load_texture(&p, space).unwrap().eval(0.5, 0.5, 0.0));
    let want = convert(Vec3A::from_array(value), Space::LIN_REC709, acescg());
    assert!((got - want).abs().max_element() < 1e-5, "{got} vs {want}");
    // Raw data stays raw, whatever the working space.
    let raw = rgb(assets
        .load_texture(&p, ColorSpace::RAW)
        .unwrap()
        .eval(0.5, 0.5, 0.0));
    assert_eq!(raw, Vec3A::from_array(value));
}

#[test]
fn a_colour_outside_the_working_gamut_clamps_at_zero() {
    let dir = scratch("outside");
    let p = dir.join("ap1_red.exr");
    exr(&p, [1.0, 0.0, 0.0]);
    let assets = FileAssets::new();
    let space = ColorSpace::from_mtlx(Some("acescg"), Space::LIN_REC709);
    let got = rgb(assets.load_texture(&p, space).unwrap().eval(0.5, 0.5, 0.0));
    assert!(got.x > 1.7, "{got}");
    assert_eq!((got.y, got.z), (0.0, 0.0), "negatives clamp");
}

/// The bit-identity the `.tx` path promises — a streamed render equals a
/// preloaded one — holds off the working primaries too: both store bytes
/// in the file's encoding and apply the same matrix after filtering.
#[test]
fn a_streamed_tx_matches_the_preloaded_texture_in_acescg() {
    let dir = scratch("tx");
    let src = dir.join("albedo.png");
    let img = image::RgbImage::from_fn(64, 64, |x, y| {
        image::Rgb([(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8])
    });
    img.save(&src).expect("png");
    let space = ColorSpace::new(Space::SRGB_TEXTURE, acescg());
    let tx = dir.join("albedo.tx");
    make_tx(&src, &tx, space, TxFormat::Tiff).expect("convert");

    let preload = FileAssets::with_config(crust_core::Config {
        tex_stream: false,
        ..Default::default()
    });
    let preloaded = preload.load_texture(&src, space).expect("preloads");
    let streamed = FileAssets::new()
        .load_texture(&src, space)
        .expect("streams");
    for (u, v, w) in [(0.3, 0.7, 0.0), (0.51, 0.12, 0.02), (0.9, 0.9, 0.2)] {
        let (a, b) = (preloaded.eval(u, v, w), streamed.eval(u, v, w));
        assert_eq!(
            a.map(f32::to_bits),
            b.map(f32::to_bits),
            "({u}, {v}, {w}): {a:?} {b:?}"
        );
    }
}

#[test]
fn an_ldr_environment_is_srgb_converted_into_the_working_space() {
    let dir = scratch("env");
    let p = dir.join("sky.png");
    let bytes = [90, 160, 240];
    png(&p, bytes);
    let assets = FileAssets::new();
    let auto = ColorSpace::AUTO.into_working(acescg());
    let map = assets.load_environment(&p, auto).expect("loads");
    let got = map.radiance(Vec3A::Y);
    let want = srgb_bytes_in(bytes, acescg());
    assert!((got - want).abs().max_element() < 1e-5, "{got} vs {want}");
    // A float sky is taken as already in the working space.
    let hdr = dir.join("sky.exr");
    exr(&hdr, [3.0, 2.0, 1.0]);
    let map = assets.load_environment(&hdr, auto).expect("loads");
    assert_eq!(map.radiance(Vec3A::Y), Vec3A::new(3.0, 2.0, 1.0));
}
