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
    let map = FileAssets::new()
        .load_environment(&samples().join("sky_env.exr"))
        .expect("exr through the trait");
    assert_eq!(map.width(), 2 * map.height());
}

#[test]
fn missing_files_decline_rather_than_panic() {
    let ghost = samples().join("does_not_exist.exr");
    assert!(load_exr_environment(&ghost).is_none());
    assert!(load_image_environment(&samples().join("does_not_exist.png")).is_none());
    assert!(FileAssets::new().load_environment(&ghost).is_none());
    assert!(
        FileAssets::new()
            .load_ptex(&samples().join("does_not_exist.ptx"))
            .is_none()
    );
    assert!(
        FileAssets::new()
            .load_texture(&samples().join("does_not_exist.png"), ColorSpace::Raw)
            .is_none()
    );
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
        *p = if y == 0 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        };
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
    // 8-bit RGB storage, plus the mip chain: each level halves both axes, so
    // the series sums to just under 4/3 of the base.
    let (mut lw, mut lh, mut expect, mut levels) = (w, h, 0usize, 0usize);
    loop {
        expect += lw * lh * 3;
        levels += 1;
        if lw <= 1 && lh <= 1 {
            break;
        }
        (lw, lh) = (lw.div_ceil(2), lh.div_ceil(2));
    }
    assert_eq!(
        tex.bytes(),
        expect,
        "8-bit RGB storage over the whole pyramid"
    );
    assert_eq!(tex.level_count(), levels);
    assert!(
        tex.bytes() < w * h * 3 * 4 / 3 + 3,
        "pyramid costs under a third extra"
    );
    let px = tex.eval(0.5, 0.5, 0.0);
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
    let a = tex.eval(0.5, 0.5, 0.0);
    let b = tex.eval(1.5, 0.5, 0.0);
    let hole = tex.eval(0.5, 1.5, 0.0);
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
    let px = tex.eval(0.5, 0.5, 0.0);
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
    let (u, v) = (
        (w as f32 * 0.3).floor() + 0.5,
        (h as f32 * 0.3).floor() + 0.5,
    );
    let (u, v) = (u / w as f32, v / h as f32);
    let (r, s, g) = (
        raw.eval(u, v, 0.0),
        srgb.eval(u, v, 0.0),
        g22.eval(u, v, 0.0),
    );
    for ch in 0..3 {
        let encoded = r[ch];
        if encoded > 0.02 && encoded < 0.98 {
            assert!(
                s[ch] < encoded,
                "sRGB decode darkens midtones: {encoded} -> {}",
                s[ch]
            );
            assert!((s[ch] - srgb_to_linear(encoded)).abs() < 2e-3);
            assert!((g[ch] - encoded.powf(2.2)).abs() < 2e-3);
        }
    }
}

#[test]
fn file_assets_hands_back_a_texture2d() {
    let tex: std::sync::Arc<dyn Texture2D> = FileAssets::new()
        .load_texture(&samples().join("textures/mtlx_mask.png"), ColorSpace::Raw)
        .expect("through the trait");
    let px = tex.eval(0.25, 0.25, 0.0);
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
    let left = tex.eval(0.02, 0.5, 0.0);
    let right = tex.eval(0.98, 0.5, 0.0);
    assert!(left[0] < 0.1 && left[2] > 0.9, "left is blue: {left:?}");
    assert!(right[0] > 0.9 && right[2] < 0.1, "right is red: {right:?}");
    // Wrapping: u = 1.02 reads like u = 0.02.
    let wrapped = tex.eval(1.02, 0.5, 0.0);
    assert!(
        (wrapped[0] - left[0]).abs() < 0.05,
        "{wrapped:?} vs {left:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn alpha_channel_reads_one_for_rgb_files() {
    let tex = UvTexture::open(&samples().join("textures/mtlx_mask.png"), ColorSpace::Raw).unwrap();
    assert_eq!(tex.eval(0.5, 0.5, 0.0)[3], 1.0);
}

#[test]
fn non_finite_coordinates_do_not_panic() {
    let tex = UvTexture::open(&samples().join("textures/mtlx_mask.png"), ColorSpace::Raw).unwrap();
    for (u, v) in [(f32::NAN, 0.5), (0.5, f32::INFINITY), (-1e30, 1e30)] {
        let px = tex.eval(u, v, 0.0);
        assert_eq!(px.len(), 4);
    }
    let set = UvTexture::open(
        &samples().join("textures/mtlx_base.<UDIM>.png"),
        ColorSpace::Raw,
    )
    .unwrap();
    let _ = set.eval(f32::NAN, f32::NAN, 0.0);
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
    assert_eq!(
        read_channel(&300u16.to_le_bytes(), ptex::DataType::UInt16),
        300.0
    );
    assert_eq!(
        read_channel(&1.5f32.to_le_bytes(), ptex::DataType::Float),
        1.5
    );
    // 0x3C00 is half-precision 1.0.
    assert_eq!(
        read_channel(&0x3C00u16.to_le_bytes(), ptex::DataType::Half),
        1.0
    );
    // 0xC000 is half-precision -2.0.
    assert_eq!(
        read_channel(&0xC000u16.to_le_bytes(), ptex::DataType::Half),
        -2.0
    );
}

// ---------------------------------------------------------------------------
// Mip pyramids
// ---------------------------------------------------------------------------

/// A checkerboard of pure black and white texels, written as a PNG.
///
/// The one pattern where the right answer is known in advance: every 2x2 box
/// holds two black and two white texels, so every mip level above the base is
/// uniform mid-grey *in linear light* — which is what makes it a test of the
/// averaging space and not just of the arithmetic.
fn write_checker(path: &std::path::Path, n: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
    let img = image::RgbImage::from_fn(n, n, |x, y| {
        if (x + y) % 2 == 0 {
            image::Rgb([255, 255, 255])
        } else {
            image::Rgb([0, 0, 0])
        }
    });
    img.save(path).expect("write png");
}

#[test]
fn a_wide_footprint_converges_to_the_mean_and_a_zero_one_does_not() {
    let dir = std::env::temp_dir().join("crust_mip_checker");
    let _ = std::fs::remove_dir_all(&dir);
    let p = dir.join("checker.png");
    write_checker(&p, 64);
    let tex = UvTexture::open(&p, ColorSpace::Raw).expect("checker loads");
    assert!(tex.level_count() > 1, "a pyramid was built");

    // A footprint covering the whole tile reads the 1x1 level, which is the
    // average of the board: mid-grey.
    let wide = tex.eval(0.5, 0.5, 4.0);
    for c in &wide[..3] {
        assert!((c - 0.5).abs() < 0.02, "wide sample {wide:?}");
    }
    // A zero footprint still sees the board's full contrast: two texel
    // centres a texel apart must differ, which is exactly the aliasing the
    // pyramid exists to filter and the point sample must not hide.
    let a = tex.eval(0.5 / 64.0, 0.5 / 64.0, 0.0);
    let b = tex.eval(1.5 / 64.0, 0.5 / 64.0, 0.0);
    assert!((a[0] - b[0]).abs() > 0.9, "point samples {a:?} vs {b:?}");
}

#[test]
fn a_zero_footprint_is_exactly_what_the_unmipped_texture_returns() {
    // The guarantee `CRUST_RAY_CONES=0` rests on, and the reason the pyramid
    // could be landed without moving a single checked-in render: with no
    // footprint the lookup must reach level 0 and nothing else, bit for bit.
    let dir = std::env::temp_dir().join("crust_mip_zero");
    let _ = std::fs::remove_dir_all(&dir);
    let p = dir.join("checker.png");
    write_checker(&p, 32);

    let mipped = UvTexture::open_with(&p, ColorSpace::Srgb, true).expect("loads");
    let flat = UvTexture::open_with(&p, ColorSpace::Srgb, false).expect("loads");

    assert_eq!(flat.level_count(), 1);
    assert!(mipped.level_count() > 1);
    for i in 0..17 {
        for j in 0..17 {
            let (u, v) = (i as f32 / 16.0, j as f32 / 16.0);
            assert_eq!(
                mipped.eval(u, v, 0.0),
                flat.eval(u, v, 0.0),
                "at ({u}, {v})"
            );
        }
    }
}

#[test]
fn levels_average_in_linear_light_not_in_the_file_encoding() {
    // The distinction this test exists for: averaging a black/white checker
    // in sRGB-encoded bytes gives 127/255 ≈ 0.5 *encoded*, which decodes to
    // 0.21 linear — less than half the light actually there. Averaging in
    // linear and re-encoding gives 0.5 linear, which is 188/255 encoded.
    let dir = std::env::temp_dir().join("crust_mip_linear");
    let _ = std::fs::remove_dir_all(&dir);
    let p = dir.join("checker.png");
    write_checker(&p, 16);
    let tex = UvTexture::open(&p, ColorSpace::Srgb).expect("loads");
    let coarse = tex.eval(0.5, 0.5, 4.0);
    assert!(
        (coarse[0] - 0.5).abs() < 0.02,
        "coarsest level should be 0.5 linear, got {coarse:?} \
         (0.21 would mean the average was taken in the file's encoding)"
    );
}

#[test]
fn an_odd_level_halves_by_div_ceil_so_every_level_spans_the_whole_tile() {
    // `decode_tile` reduces by an arbitrary integer factor, so odd level-0
    // dimensions are routine. Flooring would drop the last half-texel and
    // each level's domain would slip against level 0's.
    let dir = std::env::temp_dir().join("crust_mip_odd");
    let _ = std::fs::remove_dir_all(&dir);
    let p = dir.join("odd.png");
    std::fs::create_dir_all(&dir).expect("temp dir");
    // 25x9: both axes odd, and they run out at different levels.
    image::RgbImage::from_fn(25, 9, |x, _| {
        image::Rgb([if x < 13 { 255 } else { 0 }, 128, 64])
    })
    .save(&p)
    .expect("write png");
    let tex = UvTexture::open(&p, ColorSpace::Raw).expect("loads");
    // 25 -> 13 -> 7 -> 4 -> 2 -> 1 is six levels; 9 reaches 1 sooner and pins.
    assert_eq!(tex.level_count(), 6);
    // Every level still spans the whole tile: u = 0.01 lands in the light
    // half and u = 0.99 in the dark one at every level that still has texels
    // to tell them apart. A floored halving would slip each level's domain
    // and let the halves drift across the midpoint.
    for w in [0.0, 0.05, 0.1, 0.2] {
        let left = tex.eval(0.01, 0.5, w);
        let right = tex.eval(0.99, 0.5, w);
        assert!(
            left[0] > 0.5 && right[0] < 0.5,
            "width {w}: {left:?} / {right:?}"
        );
    }
    // The coarsest level is one texel, so it is the whole tile's mean and
    // both edges read the same thing — the chain really does bottom out.
    let a = tex.eval(0.01, 0.5, 8.0);
    let b = tex.eval(0.99, 0.5, 8.0);
    assert_eq!(a, b);
    // And it is the tile's *actual* mean: 13 of 25 columns lit, so 0.52.
    // Reducing by a clamped 2x2 instead gave the trailing column a third of
    // each level's weight where it is owed a fifth, and this bottomed out at
    // 0.41 — the energy walked toward the right edge, level by level.
    assert!(
        (a[0] - 13.0 / 25.0).abs() < 0.01,
        "coarsest level {a:?} against the tile's mean of 0.52"
    );
}

/// **An odd level must not move the tile's energy toward one edge.**
///
/// A single lit column at one end of an odd axis is the sharpest form of the
/// question, because the answer is arithmetic: whatever else a mip chain does,
/// a 25-wide tile with one column at full brightness has a mean of 1/25, and
/// its 1x1 level *is* its mean.
///
/// Reducing by a 2x2 with the source index clamped to the last column read the
/// trailing column twice and averaged it as though two were there, so the lit
/// column was worth a third of the next level wherever it sat at the right
/// edge, and only a fifth of it at the left. Lit right, this bottomed out at
/// 0.25 — **six times** the truth; lit left, at 0.03. Area weighting gives
/// both 1/25, which is the point: the answer cannot depend on which end of the
/// row the energy sits at, and the old one did, by a factor of eight.
#[test]
fn an_odd_level_keeps_the_tile_mean_wherever_the_energy_sits() {
    let dir = std::env::temp_dir().join("crust_mip_odd_edge");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");

    // 25x9 again, both axes odd, but asymmetric: one lit column, at the left
    // in one texture and at the right in the other. `Raw`, so a texel's value
    // is its byte and the mean is arithmetic all the way down.
    let mut coarsest = Vec::new();
    for (name, lit) in [("left", 0u32), ("right", 24u32)] {
        let p = dir.join(format!("{name}.png"));
        image::RgbImage::from_fn(25, 9, |x, _| {
            image::Rgb(if x == lit { [255, 255, 255] } else { [0, 0, 0] })
        })
        .save(&p)
        .expect("write png");
        let tex = UvTexture::open(&p, ColorSpace::Raw).expect("loads");
        let got = tex.eval(0.5, 0.5, 64.0)[0];
        assert!(
            (got - 1.0 / 25.0).abs() < 0.01,
            "{name}-lit tile bottoms out at {got}, not the tile mean 0.04"
        );
        coarsest.push(got);
    }
    // Stated as a symmetry too, which needs no reference value at all: the
    // same energy at opposite edges has to reduce to the same number.
    assert!(
        (coarsest[0] - coarsest[1]).abs() < 0.005,
        "lit left gave {} and lit right {} — a reduction that can tell the \
         two apart is weighting one edge more than the other",
        coarsest[0],
        coarsest[1],
    );
}
