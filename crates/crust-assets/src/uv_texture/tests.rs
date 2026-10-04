use super::mip::*;
use super::udim::*;
use super::*;

/// Writes a 1x1 PNG of a flat colour, creating parent directories.
///
/// One texel is enough: these tests are about *which file* a coordinate
/// reaches, so the colour is the tile's identity and bilinear filtering
/// across a uniform tile returns it exactly.
fn write_tile(path: &Path, rgb: [u8; 3]) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
    image::RgbImage::from_pixel(1, 1, image::Rgb(rgb))
        .save(path)
        .expect("write png");
}

/// A scratch directory of its own per test, so the parallel test runner
/// cannot have one test's tiles satisfy another's glob.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("crust_uv_texture_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// The channel values a tile is written with, recovered from a lookup.
/// Raw decode, so the round trip is exact.
fn sampled(tex: &UvTexture, u: f32, v: f32) -> [u8; 3] {
    let px = tex.eval(u, v, 0.0);
    [
        (px[0] * 255.0).round() as u8,
        (px[1] * 255.0).round() as u8,
        (px[2] * 255.0).round() as u8,
    ]
}

#[test]
fn a_udim_set_loads_and_addresses_by_tile() {
    let dir = scratch("udim");
    // 1001 is (0,0), 1002 is (1,0), 1011 is (0,1) — and 1012 is left off
    // disk, so the set has a hole.
    write_tile(&dir.join("a.1001.png"), [10, 0, 0]);
    write_tile(&dir.join("a.1002.png"), [20, 0, 0]);
    write_tile(&dir.join("a.1011.png"), [30, 0, 0]);

    let tex = UvTexture::open(&dir.join("a.<UDIM>.png"), ColorSpace::RAW)
        .expect("a <UDIM> set must load the tiles that exist");
    assert_eq!(tex.tile_count(), 3, "only the tiles on disk are decoded");
    assert_eq!(sampled(&tex, 0.5, 0.5), [10, 0, 0]);
    assert_eq!(sampled(&tex, 1.5, 0.5), [20, 0, 0]);
    assert_eq!(sampled(&tex, 0.5, 1.5), [30, 0, 0]);
}

#[test]
fn a_uvtile_set_loads_and_addresses_by_the_same_tile() {
    // The regression this pins: `<UVTILE>` reached `open` intact (the
    // document parser escapes and restores it) and was then never
    // expanded, so the literal name was opened as a single image, failed,
    // and the whole set loaded *nothing*.
    let dir = scratch("uvtile");
    // Both indices are 1-based: u1_v1 is the tile <UDIM> numbers 1001.
    write_tile(&dir.join("a.u1_v1.png"), [10, 0, 0]);
    write_tile(&dir.join("a.u2_v1.png"), [20, 0, 0]);
    write_tile(&dir.join("a.u1_v2.png"), [30, 0, 0]);

    let tex = UvTexture::open(&dir.join("a.<UVTILE>.png"), ColorSpace::RAW)
        .expect("a <UVTILE> set must load like a <UDIM> one");
    assert_eq!(tex.tile_count(), 3);
    // Identical coordinates to the <UDIM> test above: the two tokens
    // spell the same grid, so addressing must not depend on which named
    // the files.
    assert_eq!(sampled(&tex, 0.5, 0.5), [10, 0, 0]);
    assert_eq!(sampled(&tex, 1.5, 0.5), [20, 0, 0]);
    assert_eq!(sampled(&tex, 0.5, 1.5), [30, 0, 0]);
}

#[test]
fn a_missing_tile_reads_black_rather_than_a_neighbour() {
    let dir = scratch("holes");
    write_tile(&dir.join("a.u1_v1.png"), [10, 20, 30]);
    let tex = UvTexture::open(&dir.join("a.<UVTILE>.png"), ColorSpace::RAW).expect("loads");

    // A hole inside the grid, and coordinates off the grid entirely.
    // Both are black, so a mis-scaled chart looks wrong instead of
    // plausibly tiled — the set must not wrap onto the tile it does have.
    assert_eq!(sampled(&tex, 3.5, 2.5), [0, 0, 0], "hole");
    assert_eq!(sampled(&tex, 10.5, 0.5), [0, 0, 0], "past the grid");
    assert_eq!(sampled(&tex, -0.5, 0.5), [0, 0, 0], "before the grid");
    assert_eq!(
        sampled(&tex, 0.5, 0.5),
        [10, 20, 30],
        "the tile that exists"
    );
}

#[test]
fn an_empty_tile_set_declines_instead_of_loading_the_literal_name() {
    let dir = scratch("empty");
    assert!(UvTexture::open(&dir.join("gone.<UDIM>.png"), ColorSpace::RAW).is_none());
    assert!(UvTexture::open(&dir.join("gone.<UVTILE>.png"), ColorSpace::RAW).is_none());
}

#[test]
fn a_single_image_wraps_instead_of_tiling() {
    let dir = scratch("single");
    write_tile(&dir.join("flat.png"), [7, 8, 9]);
    let tex = UvTexture::open(&dir.join("flat.png"), ColorSpace::RAW).expect("loads");
    // MaterialX's default address mode is `periodic`, so a coordinate
    // outside the unit square wraps back rather than reading black.
    assert_eq!(sampled(&tex, 0.5, 0.5), [7, 8, 9]);
    assert_eq!(sampled(&tex, 3.5, 2.5), [7, 8, 9]);
}

#[test]
fn the_two_tokens_spell_the_same_grid() {
    for (u, v) in [(0, 0), (1, 0), (0, 1), (9, 9)] {
        let udim = TileToken::Udim.expand("a.<UDIM>.png", u, v);
        let uvtile = TileToken::UvTile.expand("a.<UVTILE>.png", u, v);
        assert_eq!(udim, format!("a.{}.png", udim_number(u, v)));
        assert_eq!(uvtile, format!("a.u{}_v{}.png", u + 1, v + 1));
    }
    assert_eq!(TileToken::detect("a.<UDIM>.png"), Some(TileToken::Udim));
    assert_eq!(TileToken::detect("a.<UVTILE>.png"), Some(TileToken::UvTile));
    assert_eq!(TileToken::detect("a.png"), None);
}

fn at(space: ResolvedColorSpace, encoded: u8) -> f32 {
    space.to_linear_table()[encoded as usize]
}

#[test]
fn gamma_tables_are_pure_power_laws() {
    for encoded in [0u8, 3, 13, 26, 128, 255] {
        let c = encoded as f32 / 255.0;
        assert!((at(ResolvedColorSpace::GAMMA22, encoded) - c.powf(2.2)).abs() < 1e-7);
        assert!((at(ResolvedColorSpace::GAMMA18, encoded) - c.powf(1.8)).abs() < 1e-7);
    }
    // Both curves are anchored: black stays black, white stays white, so
    // a fully-lit albedo keeps its exposure whichever tag it carries.
    for space in [
        ResolvedColorSpace::GAMMA22,
        ResolvedColorSpace::GAMMA18,
        ResolvedColorSpace::SRGB,
    ] {
        assert_eq!(at(space, 0), 0.0);
        assert!((at(space, 255) - 1.0).abs() < 1e-6);
    }
}

#[test]
fn gamma_22_is_not_the_srgb_curve_in_the_shadows() {
    // The regression this pins. `g22_rec709` used to decode through the
    // piecewise sRGB curve, whose linear toe lifts near-black by an order
    // of magnitude — an albedo of 0.01 encoded reads 19x too bright.
    let (srgb, g22) = (
        at(ResolvedColorSpace::SRGB, 3),
        at(ResolvedColorSpace::GAMMA22, 3),
    );
    assert!(srgb > g22 * 10.0, "srgb {srgb} vs gamma22 {g22}");
    // And converges in the midtones, which is why the bug is invisible
    // on a look-dev turntable and only shows up in dark albedo.
    let (srgb, g22) = (
        at(ResolvedColorSpace::SRGB, 128),
        at(ResolvedColorSpace::GAMMA22, 128),
    );
    assert!((srgb - g22).abs() < 0.005, "srgb {srgb} vs gamma22 {g22}");
}

#[test]
fn gamma_18_is_brighter_than_both_across_the_range() {
    // 1.8 is the shallower exponent, so it decodes above 2.2 everywhere
    // strictly inside [0,1] — the error is not confined to the toe.
    for encoded in [13u8, 64, 128, 200] {
        let (g18, g22) = (
            at(ResolvedColorSpace::GAMMA18, encoded),
            at(ResolvedColorSpace::GAMMA22, encoded),
        );
        assert!(g18 > g22, "at {encoded}: g18 {g18} !> g22 {g22}");
        assert!(
            g18 > at(ResolvedColorSpace::SRGB, encoded),
            "at {encoded}: g18 {g18}"
        );
    }
}

#[test]
fn raw_is_the_identity() {
    for encoded in [0u8, 1, 77, 255] {
        assert_eq!(at(ResolvedColorSpace::RAW, encoded), encoded as f32 / 255.0);
    }
}

/// Greyscale texels as a level: value `v` in all three channels.
fn grey(values: &[u8]) -> Vec<u8> {
    values.iter().flat_map(|&v| [v, v, v]).collect()
}

/// The red channel of every texel of a level.
fn reds(level: &[u8]) -> Vec<u8> {
    level.iter().step_by(3).copied().collect()
}

/// Reduces one row under `Raw`, where the decode table and the code steps are
/// the identity and a level is its own u8 values back.
fn reduce_row(values: &[u8]) -> Vec<u8> {
    let table = ResolvedColorSpace::RAW.to_linear_table();
    let (out, w, h) = reduce_half(
        &grey(values),
        values.len(),
        1,
        &table,
        &ResolvedColorSpace::RAW.code_steps(),
    );
    assert_eq!((w, h), (values.len().div_ceil(2), 1));
    reds(&out)
}

/// **An odd axis is resampled by area, not by duplicating its last
/// texel.**
///
/// The sampler maps `x = u * width - 0.5`, so it reads each destination
/// texel as an equal-width slice of the whole tile. Five source texels
/// into three therefore means each new texel averages the source over
/// exactly its own 5/3 of the row: `[1, 2/3]`, `[1/3, 1, 1/3]`,
/// `[2/3, 1]`, normalised. Clamping the source index instead — which is
/// what this did — makes the last texel the average of source column 4
/// alone, a fifth of the domain stretched over a third of it.
///
/// The numbers are chosen to land exactly, and the two schemes are 5, 25
/// and 20 units apart, so the tolerance for the round trip through the
/// 256-entry table cannot blur them together.
#[test]
fn an_odd_axis_is_area_weighted_not_edge_duplicated() {
    let got = reduce_row(&[250, 200, 150, 100, 50]);
    let want = [230u8, 150, 70]; // clamped gave [225, 125, 50]
    for (i, (&g, &w)) in got.iter().zip(want.iter()).enumerate() {
        assert!(
            (g as i32 - w as i32).abs() <= 1,
            "texel {i}: got {g}, want {w} (the whole level is {got:?})"
        );
    }
}

/// And because the taps tile the axis, the level's mean is the source's.
///
/// This is the half that shows up in a render: the clamp gave the
/// trailing column a third of the level's weight where it is owed a
/// fifth, so the mean drifted at *every* level and the drift compounded
/// down the chain.
#[test]
fn an_odd_level_preserves_the_mean() {
    let src = [250u8, 200, 150, 100, 50];
    let got = reduce_row(&src);
    let mean = |v: &[u8]| v.iter().map(|&x| x as f32).sum::<f32>() / v.len() as f32;
    assert!(
        (mean(&got) - mean(&src)).abs() < 1.0,
        "level mean {} against source mean {} (the clamp gave 133.3)",
        mean(&got),
        mean(&src),
    );
}

/// **An even axis must reduce exactly as it always has**, bit for bit.
///
/// Every overlap on an even axis is exactly `1.0` and the axis total
/// exactly `2.0`, so `weighted` divides by exactly `4.0` and the result is
/// the old `0.25 * (a + b + c + d)` unchanged. Every checked-in texture is
/// 64x64, so this is what says the sample goldens cannot move — and with
/// them the streamed-versus-preloaded invariant, which compares a `.tx`
/// chain against an in-memory one.
#[test]
fn an_even_axis_reduces_exactly_as_it_did() {
    let (sw, sh) = (8usize, 6usize);
    let src: Vec<u8> = (0..sw * sh * 3).map(|i| (i * 7 % 251) as u8).collect();
    let table = ResolvedColorSpace::SRGB.to_linear_table();
    let encode = |l: f32| encode(ResolvedColorSpace::SRGB, l);
    let (got, w, h) = reduce_half(&src, sw, sh, &table, &ResolvedColorSpace::SRGB.code_steps());
    assert_eq!((w, h), (4, 3));
    for y in 0..h {
        for x in 0..w {
            for k in 0..3 {
                let at = |xi: usize, yi: usize| table[src[(yi * sw + xi) * 3 + k] as usize];
                let mean = 0.25
                    * (at(2 * x, 2 * y)
                        + at(2 * x + 1, 2 * y)
                        + at(2 * x, 2 * y + 1)
                        + at(2 * x + 1, 2 * y + 1));
                let want = (encode(mean) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                assert_eq!(got[(y * w + x) * 3 + k], want, "at ({x}, {y}) channel {k}");
            }
        }
    }
}

/// The two reducers back the TIFF and the EXR `.tx` writer, and their doc
/// comments promise they agree on everything but the transfer curve.
///
/// Left to prose, that promise is exactly the kind a change keeps half of:
/// an EXR-backed `.tx` and a TIFF-backed one that disagreed on odd levels
/// would each be internally consistent, and nothing would look wrong until
/// two renders of the same texture were diffed against each other.
#[test]
fn the_two_reducers_agree_on_an_odd_level() {
    let (sw, sh) = (7usize, 5usize);
    let bytes: Vec<u8> = (0..sw * sh * 3).map(|i| (i * 11 % 251) as u8).collect();
    let floats: Vec<f32> = bytes.iter().map(|&b| b as f32 / 255.0).collect();

    let (from_u8, w, h) = reduce_half(
        &bytes,
        sw,
        sh,
        &ResolvedColorSpace::RAW.to_linear_table(),
        &ResolvedColorSpace::RAW.code_steps(),
    );
    let (from_f32, lw, lh) = reduce_half_linear(&floats, sw, sh);
    assert_eq!((w, h), (lw, lh), "the two disagree on level size");
    for (i, (&b, &f)) in from_u8.iter().zip(from_f32.iter()).enumerate() {
        let quantised = (f * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        assert_eq!(b, quantised, "component {i}");
    }
}

/// Three taps is the ceiling `Tap` is sized for, and it is arithmetic
/// rather than an observation — but the arithmetic is easy to get wrong,
/// and a fourth would be silently dropped in release.
#[test]
fn no_destination_texel_reaches_more_than_three_source_texels() {
    // Dense up to 2000 — the drift this caught first was at 1795 — plus
    // sizes past where `f32` stops counting `j * src` exactly.
    let sizes = (1..2000usize).chain([4095, 4096, 8191, 8192, 16383, 16384]);
    for src in sizes {
        let dst = src.div_ceil(2);
        let (taps, sum) = axis_taps(src, dst);
        assert_eq!(taps.len(), dst);
        for (j, t) in taps.iter().enumerate() {
            assert!((1..=3).contains(&t.count), "src {src} texel {j}: {t:?}");
            assert!(t.start + t.count <= src, "src {src} texel {j} runs past");
            let total: f32 = t.weight[..t.count].iter().sum();
            assert!(
                (total - sum).abs() < 1e-5,
                "src {src} texel {j} totals {total}, not {sum} — the taps \
                 must tile the axis or the level's mean drifts"
            );
        }
        // An even axis is the plain 2x2, exactly: this is the property
        // `an_even_axis_reduces_exactly_as_it_did` rests on.
        if src % 2 == 0 {
            assert_eq!(sum, 2.0);
            assert!(
                taps.iter()
                    .all(|t| t.count == 2 && t.weight[..2] == [1.0, 1.0])
            );
        }
    }
}

/// Writes an EXR of `w x h` texels from a per-texel colour.
fn write_exr(path: &Path, w: usize, h: usize, f: impl Fn(usize, usize) -> (f32, f32, f32) + Sync) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
    exr::prelude::write_rgb_file(path, w, h, f).expect("write exr");
}

#[test]
fn an_exr_preloads_at_full_float_precision_and_range() {
    let dir = scratch("exr_f32");
    let p = dir.join("hdr.exr");
    // A dark value 8-bit linear would round away and a bright one it
    // would clip: both have to come back exactly.
    write_exr(&p, 2, 1, |x, _| {
        if x == 0 {
            (0.02, 0.003, 0.5)
        } else {
            (4.0, 1.5, 0.25)
        }
    });
    let tex = UvTexture::open_with(&p, ColorSpace::AUTO, false).expect("loads");
    assert!(tex.is_float());
    assert_eq!(
        tex.color_space(),
        ResolvedColorSpace::RAW,
        "auto on a float file is raw"
    );
    assert_eq!(tex.bytes(), 2 * 3 * 4);
    // Texel centres: x = u * 2 - 0.5 lands exactly on 0 and 1.
    assert_eq!(tex.eval(0.25, 0.5, 0.0), [0.02, 0.003, 0.5, 1.0]);
    assert_eq!(tex.eval(0.75, 0.5, 0.0), [4.0, 1.5, 0.25, 1.0]);
}

#[test]
fn an_explicit_curve_on_an_exr_is_applied_once_at_load() {
    let dir = scratch("exr_srgb");
    let p = dir.join("enc.exr");
    write_exr(&p, 1, 1, |_, _| (0.5, 0.5, 0.5));
    let tex = UvTexture::open_with(&p, ColorSpace::SRGB, false).expect("loads");
    let want = ResolvedColorSpace::SRGB.decode_curve(0.5);
    assert!((tex.eval(0.5, 0.5, 0.0)[0] - want).abs() < 1e-6);
}

#[test]
fn an_exr_udim_set_addresses_by_tile() {
    let dir = scratch("exr_udim");
    write_exr(&dir.join("a.1001.exr"), 1, 1, |_, _| (1.0, 0.0, 0.0));
    write_exr(&dir.join("a.1002.exr"), 1, 1, |_, _| (0.0, 2.0, 0.0));
    let tex = UvTexture::open(&dir.join("a.<UDIM>.exr"), ColorSpace::RAW).expect("loads");
    assert_eq!(tex.tile_count(), 2);
    assert_eq!(tex.eval(0.5, 0.5, 0.0)[..3], [1.0, 0.0, 0.0]);
    assert_eq!(tex.eval(1.5, 0.5, 0.0)[..3], [0.0, 2.0, 0.0]);
    assert_eq!(
        tex.eval(2.5, 0.5, 0.0)[..3],
        [0.0, 0.0, 0.0],
        "no tile 1003"
    );
}

#[test]
fn an_exr_pyramid_preserves_the_mean_on_an_odd_axis() {
    let dir = scratch("exr_mip");
    let p = dir.join("row.exr");
    let src = [2.5f32, 2.0, 1.5, 1.0, 0.5];
    write_exr(&p, 5, 1, |x, _| (src[x], src[x], src[x]));
    let tex = UvTexture::open_with(&p, ColorSpace::RAW, true).expect("loads");
    let Storage::F32(tiles) = &tex.storage else {
        panic!("an EXR is f32");
    };
    let l1 = &tiles[0].levels[1];
    assert_eq!(l1.width, 3);
    let mean = l1.pixels.iter().step_by(3).sum::<f32>() / 3.0;
    assert!(
        (mean - 1.5).abs() < 1e-5,
        "level-1 mean {mean}, source mean 1.5"
    );
    // The chain reaches one texel, and that texel is the source mean.
    let top = tiles[0].levels.last().unwrap();
    assert_eq!((top.width, top.height), (1, 1));
    assert!((top.pixels[0] - 1.5).abs() < 1e-5);
}

#[test]
fn auto_decodes_an_rgb_png_and_leaves_a_grey_one_raw() {
    let dir = scratch("auto_png");
    let rgb = dir.join("rgb.png");
    write_tile(&rgb, [128, 128, 128]);
    let grey = dir.join("grey.png");
    image::GrayImage::from_pixel(1, 1, image::Luma([128]))
        .save(&grey)
        .expect("write png");

    let t = UvTexture::open(&rgb, ColorSpace::AUTO).expect("loads");
    assert_eq!(t.color_space(), ResolvedColorSpace::SRGB);
    assert!(
        (t.eval(0.5, 0.5, 0.0)[0] - ResolvedColorSpace::SRGB.decode_curve(128.0 / 255.0)).abs()
            < 1e-6
    );

    let t = UvTexture::open(&grey, ColorSpace::AUTO).expect("loads");
    assert_eq!(t.color_space(), ResolvedColorSpace::RAW);
    assert_eq!(t.eval(0.5, 0.5, 0.0)[0], 128.0 / 255.0);
}

/// Writes a scanline EXR whose channels are `names`, each flat `values`.
fn write_exr_channels(path: &Path, w: usize, h: usize, names: &[(&str, Vec<f32>)]) {
    use exr::prelude::{
        AnyChannel, AnyChannels, Encoding, FlatSamples, Image, Layer, LayerAttributes,
        WritableImage,
    };
    std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
    let channels = AnyChannels::sort(
        names
            .iter()
            .map(|(n, v)| AnyChannel::new(*n, FlatSamples::F32(v.clone())))
            .collect(),
    );
    let layer = Layer::new(
        (w, h),
        LayerAttributes::default(),
        Encoding::UNCOMPRESSED,
        channels,
    );
    Image::from_layer(layer)
        .write()
        .to_file(path)
        .expect("write exr");
}

#[test]
fn a_single_prefixed_channel_exr_replicates_into_rgb() {
    // ALab's roughness / metallic / ior maps: one channel named `rgb.R`,
    // which the RGBA convenience reader refused outright.
    let dir = scratch("exr_mono");
    let p = dir.join("rough.1001.exr");
    write_exr_channels(&p, 2, 1, &[("rgb.R", vec![0.25, 0.75])]);
    let tex = UvTexture::open(&dir.join("rough.<UDIM>.exr"), ColorSpace::AUTO).expect("loads");
    assert_eq!(tex.eval(0.25, 0.5, 0.0), [0.25, 0.25, 0.25, 1.0]);
    assert_eq!(tex.eval(0.75, 0.5, 0.0), [0.75, 0.75, 0.75, 1.0]);
}

#[test]
fn prefixed_rgb_channels_are_matched_by_base_name() {
    let dir = scratch("exr_prefixed");
    let p = dir.join("c.exr");
    write_exr_channels(
        &p,
        1,
        1,
        &[
            ("rgb.B", vec![0.3]),
            ("rgb.G", vec![0.2]),
            ("rgb.R", vec![0.1]),
        ],
    );
    let tex = UvTexture::open(&p, ColorSpace::RAW).expect("loads");
    assert_eq!(tex.eval(0.5, 0.5, 0.0), [0.1, 0.2, 0.3, 1.0]);
}

/// The streaming reader matches base names case-insensitively, so the
/// preload reader must too, or the same file decodes differently
/// depending on whether a `.tx` stands beside it.
#[test]
fn channel_base_names_match_case_insensitively() {
    let dir = scratch("exr_lowercase");
    let p = dir.join("c.exr");
    write_exr_channels(
        &p,
        1,
        1,
        &[
            ("rgb.b", vec![0.3]),
            ("rgb.g", vec![0.2]),
            ("rgb.r", vec![0.1]),
        ],
    );
    let tex = UvTexture::open(&p, ColorSpace::RAW).expect("loads");
    assert_eq!(tex.eval(0.5, 0.5, 0.0), [0.1, 0.2, 0.3, 1.0]);
}

/// The inverse curves a mip level used to re-encode through, written out:
/// the reference `quantize` must reproduce.
fn encode(space: ResolvedColorSpace, l: f32) -> f32 {
    let l = l.max(0.0);
    match space {
        ResolvedColorSpace::SRGB if l <= 0.003_130_8 => l * 12.92,
        ResolvedColorSpace::SRGB => 1.055 * l.powf(1.0 / 2.4) - 0.055,
        ResolvedColorSpace::GAMMA22 => l.powf(1.0 / 2.2),
        ResolvedColorSpace::GAMMA18 => l.powf(1.0 / 1.8),
        _ => l,
    }
}

/// `quantize` over `code_steps` is the re-encode a mip level used to run per
/// texel — round `encode(mean)` to the nearest byte — as a table. The two
/// may disagree only where `encode(mean)` sits on a half-code boundary: by
/// float rounding, and for sRGB by OCIO's toe, which is derived for
/// continuity rather than rounded to IEC 61966-2-1's constants.
#[test]
fn quantize_is_the_rounded_encode() {
    for space in [
        ResolvedColorSpace::SRGB,
        ResolvedColorSpace::GAMMA22,
        ResolvedColorSpace::GAMMA18,
        ResolvedColorSpace::RAW,
    ] {
        let steps = space.code_steps();
        assert!(
            steps.windows(2).all(|w| w[0] < w[1]),
            "{space:?} steps rise"
        );
        for i in 0..=12_000 {
            let mean = i as f32 / 10_000.0;
            let scaled = encode(space, mean) * 255.0;
            let rounded = (scaled + 0.5).clamp(0.0, 255.0) as u8;
            let got = crate::quantize(&steps, mean);
            let on_boundary = (scaled - scaled.floor() - 0.5).abs() < 5e-3;
            assert!(
                got == rounded || on_boundary,
                "{space:?} mean {mean}: {got} vs {rounded}"
            );
        }
        assert_eq!(crate::quantize(&steps, -1.0), 0);
        assert_eq!(crate::quantize(&steps, f32::NAN), 0);
        assert_eq!(crate::quantize(&steps, 2.0), 255);
    }
}
