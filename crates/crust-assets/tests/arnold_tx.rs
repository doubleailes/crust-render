//! Crust's `.tx` convention against Arnold 7.5's, case by case: what Arnold's
//! auto-tx wrote and how it read it, recorded by `scripts/arnold_tx_oracle.py`
//! in `tests/data/arnold_tx/cases.json` beside the sources and the `.tx` files
//! themselves. Nothing here runs Arnold.

use crust_assets::tx_name;
use crust_core::ColorSpace;
use crust_core::color::Space;
use crust_core::mtlx::TexelSpace;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/arnold_tx")
}

fn cases() -> serde_json::Value {
    let text = std::fs::read_to_string(fixtures().join("cases.json")).expect("cases.json");
    serde_json::from_str(&text).expect("json")
}

fn acescg() -> Space {
    Space::named("ACEScg").expect("ACEScg")
}

/// How crust reads what an Arnold `image` node's `color_space` says: as a
/// MaterialX colour image with that `colorspace`, or `auto`.
fn image_binding(color_space: &str) -> ColorSpace {
    let w = Space::LIN_REC709;
    match color_space {
        "auto" => ColorSpace::AUTO,
        name => ColorSpace::from_mtlx_image(TexelSpace::Named(name), w),
    }
}

/// How crust's import reads one of the oracle's USD bindings.
fn usd_binding(case: &serde_json::Value) -> ColorSpace {
    let w = Space::LIN_REC709;
    let space = case["color_space"].as_str();
    let meta = case["authored_as"] == "meta";
    match case["shader"].as_str().expect("shader") {
        "UsdUVTexture" if meta => {
            ColorSpace::new(Space::named(space.expect("named")).expect("known"), w)
        }
        "UsdUVTexture" => ColorSpace::from_usd(space, w),
        "ND_image_color3" => {
            ColorSpace::from_mtlx_image(space.map_or(TexelSpace::Unset, TexelSpace::Named), w)
        }
        _ => ColorSpace::from_mtlx_image(TexelSpace::Data, w),
    }
}

/// The name crust gives a binding Arnold names differently by design: the
/// `sRGB` token, which crust reads as the sRGB space and Arnold, whose OCIO
/// config defines no `sRGB`, passes through unconverted (design decision 1).
fn by_design(case: &str, arnold: &str) -> Option<String> {
    matches!(case, "srgb_word" | "usd_uv_srgb")
        .then(|| arnold.replace("_sRGB_", "_sRGB Encoded Rec.709 (sRGB)_"))
}

/// **Every name Arnold wrote, byte for byte**, from the binding crust reads:
/// the `image` node cases and the USD procedural's.
#[test]
fn crust_names_each_tx_as_arnold_does() {
    let cases = cases();
    let mut checked = 0;
    for (section, binding) in [
        (
            "image",
            &(|c: &serde_json::Value| image_binding(c["color_space"].as_str().unwrap()))
                as &dyn Fn(&serde_json::Value) -> ColorSpace,
        ),
        ("usd", &usd_binding),
    ] {
        for case in cases[section].as_array().expect("cases") {
            let name = case["case"].as_str().expect("case");
            let arnold = case["tx"][0]["tx"].as_str().expect("tx");
            let want = by_design(name, arnold).unwrap_or_else(|| arnold.to_string());
            let source = fixtures().join(case["source"].as_str().expect("source"));
            let got = tx_name(&source, binding(case), acescg())
                .unwrap_or_else(|| panic!("{name}: no name"));
            assert_eq!(
                got.path.file_name().unwrap().to_string_lossy(),
                want,
                "{section} case {name}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 50, "only {checked} cases");
}

/// One texel of `file` (a fixture source), preloaded — so the value is the
/// source's, whatever `.tx` Arnold wrote beside it — at texel `(x, y)`.
fn texel(file: &str, space: ColorSpace, x: usize, y: usize) -> [f32; 4] {
    use crust_core::AssetLoader;
    let preload = crust_assets::FileAssets::with_config(crust_core::Config {
        tex_stream: false,
        ..Default::default()
    });
    let path = fixtures().join(file);
    let (w, h) = image::image_dimensions(&path)
        .map(|(w, h)| (w as f32, h as f32))
        .unwrap_or((37.0, 23.0));
    let tex = preload.load_texture(&path, space).expect("loads");
    tex.eval((x as f32 + 0.5) / w, 1.0 - (y as f32 + 0.5) / h, 0.0)
}

/// The first sample of `file`'s texel `(x, y)`, as stored, over 255.
fn byte(file: &str, x: u32, y: u32) -> f32 {
    let img = image::open(fixtures().join(file))
        .expect("decode")
        .to_rgb8();
    img.get_pixel(x, y)[0] as f32 / 255.0
}

/// **`auto` and an unset MaterialX `colorspace`, as Arnold resolves them**
/// (design decision 8, the `textures` scenarios under "Colour space travels
/// with the texture"), on the oracle's own sources: the bindings whose `.tx`
/// Arnold converted from sRGB decode as sRGB, and the ones it left `_raw`
/// read as stored.
#[test]
fn auto_and_an_unset_materialx_colorspace_read_as_arnold_reads_them() {
    let w = Space::LIN_REC709;
    let srgb = |v: f32| crust_core::ResolvedColorSpace::SRGB.decode_curve(v);
    let close = |a: f32, b: f32| (a - b).abs() < 1e-5;

    // A greyscale roughness PNG under UsdUVTexture with no
    // `sourceColorSpace`: decoded as sRGB (Arnold: `usd_uv_auto_grey1`).
    let got = texel("usd_uv_auto_grey1.png", ColorSpace::from_usd(None, w), 3, 2);
    let want = srgb(byte("usd_uv_auto_grey1.png", 3, 2));
    assert!(close(got[0], want), "{got:?} against {want}");

    // The same image through `ND_image_float`: data, as stored
    // (`usd_mx_float_unset`).
    let data = ColorSpace::from_mtlx_image(TexelSpace::Data, w);
    let got = texel("usd_mx_float_unset.png", data, 3, 2);
    let want = byte("usd_mx_float_unset.png", 3, 2);
    assert!(close(got[0], want), "{got:?} against {want}");

    // A MaterialX albedo with no `colorspace` anywhere: `auto`, so sRGB
    // for an 8-bit RGB PNG (`usd_mx_unset`).
    let unset = ColorSpace::from_mtlx_image(TexelSpace::Unset, w);
    let got = texel("usd_mx_unset.png", unset, 5, 1);
    let want = srgb(byte("usd_mx_unset.png", 5, 1));
    assert!(close(got[0], want), "{got:?} against {want}");

    // A float EXR under `auto`: as stored (`hdr_auto`). Its red is
    // `(x + 1) / 4`, well past what a curve would leave alone.
    let got = texel("hdr_auto.exr", ColorSpace::AUTO, 6, 0);
    assert!(close(got[0], 1.75), "{got:?}");
}

/// A case of `cases.json` by name.
fn case(name: &str) -> serde_json::Value {
    let cases = cases();
    ["image", "usd"]
        .iter()
        .flat_map(|s| cases[s].as_array().expect("cases").clone())
        .find(|c| c["case"] == name)
        .unwrap_or_else(|| panic!("no case {name}"))
}

/// A fixture source and the `.tx` Arnold wrote beside it, copied into a fresh
/// directory of `test`'s own, the `.tx` stamped with the source's time as
/// Arnold stamps it.
fn arnold_pair(name: &str, test: &str) -> PathBuf {
    let c = case(name);
    let dir = std::env::temp_dir().join(format!("crust_arnold_pair_{test}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let src = dir.join(c["source"].as_str().unwrap());
    std::fs::copy(fixtures().join(c["source"].as_str().unwrap()), &src).expect("source");
    let tx = dir.join(c["tx"][0]["tx"].as_str().unwrap());
    std::fs::copy(fixtures().join(c["tx"][0]["tx"].as_str().unwrap()), &tx).expect("tx");
    let when = std::fs::metadata(&src)
        .and_then(|m| m.modified())
        .expect("time");
    std::fs::File::options()
        .write(true)
        .open(&tx)
        .and_then(|f| f.set_modified(when))
        .expect("stamp");
    src
}

/// What Arnold rendered for one direct binding (`direct_binding`).
fn arnold_direct(binding: &str) -> [f32; 3] {
    let cases = cases();
    let row = cases["direct_binding"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|r| r["binding"] == binding)
        .unwrap_or_else(|| panic!("no row {binding}"))
        .clone();
    let v = |k: usize| row["value"][k].as_f64().unwrap() as f32;
    [v(0), v(1), v(2)]
}

/// Level-0 agreement at every texel centre, decision 7's bounds: per channel,
/// a mean absolute difference of at most 0.005 and a 99th percentile of at
/// most 0.02, relative above 1.
fn assert_level0_agrees(
    a: &dyn crust_core::Texture2D,
    b: &dyn crust_core::Texture2D,
    w: usize,
    h: usize,
    what: &str,
) {
    for k in 0..4 {
        let mut diffs: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let (u, v) = (
                    (x as f32 + 0.5) / w as f32,
                    1.0 - (y as f32 + 0.5) / h as f32,
                );
                let (p, q) = (a.eval(u, v, 0.0)[k], b.eval(u, v, 0.0)[k]);
                (p - q).abs() / q.abs().max(1.0)
            })
            .collect();
        diffs.sort_by(f32::total_cmp);
        let mean = diffs.iter().sum::<f32>() / diffs.len() as f32;
        let p99 = diffs[(diffs.len() * 99 / 100).min(diffs.len() - 1)];
        assert!(
            mean <= 0.005 && p99 <= 0.02,
            "{what} channel {k}: mean {mean}, p99 {p99}"
        );
    }
}

/// **An Arnold `.tx` of an sRGB albedo in a Rec.709 render**: found beside
/// its source under Arnold's name, read as linear ACEScg — no sRGB curve — and
/// brought to Rec.709, so it reads the source's colour within tolerance.
#[test]
fn an_arnold_tx_of_an_srgb_albedo_reads_as_linear_acescg_in_a_rec709_render() {
    use crust_core::AssetLoader;
    let src = arnold_pair("srgb_canon", "rec709");
    let streamed = crust_assets::FileAssets::new();
    let tex = streamed
        .load_texture(&src, ColorSpace::SRGB)
        .expect("streams");
    let stats = streamed.texture_cache_stats();
    assert_eq!(
        (stats.files, stats.preloaded),
        (1, 0),
        "Arnold's .tx streamed"
    );
    let preload = crust_assets::FileAssets::with_config(crust_core::Config {
        tex_stream: false,
        ..Default::default()
    });
    let pre = preload
        .load_texture(&src, ColorSpace::SRGB)
        .expect("preloads");
    assert_level0_agrees(&*tex, &*pre, 37, 23, "srgb_canon");
}

/// **An already-converted `.tx` bound as sRGB** is read in the binding's
/// space, whatever the file holds: its half values go through the sRGB curve
/// and the Rec.709 → working change, as Arnold reads it (about four times too
/// dark). **Bound with no colour space** in an ACEScg render, its values are
/// returned as stored. And a plain 8-bit `.tx` bound as sRGB decodes through
/// the table as its source would.
#[test]
fn a_directly_bound_tx_is_read_in_the_binding_s_space_as_arnold_reads_it() {
    use crust_core::AssetLoader;
    let aces = acescg();
    let at_centre = |file: &str, space: ColorSpace| {
        let assets = crust_assets::FileAssets::new();
        let tex = assets
            .load_texture(&fixtures().join(file), space)
            .expect("loads");
        assert_eq!(assets.texture_cache_stats().files, 1, "{file} streamed");
        let v = tex.eval(0.5, 0.5, 0.0);
        [v[0], v[1], v[2]]
    };
    let close = |got: [f32; 3], want: [f32; 3], what: &str| {
        for k in 0..3 {
            assert!(
                (got[k] - want[k]).abs() <= 2e-3 + 0.01 * want[k],
                "{what}: {got:?} against Arnold's {want:?}"
            );
        }
    };
    let converted = "const_srgb_sRGB Encoded Rec.709 (sRGB)_ACEScg.png.tx";
    let srgb = ColorSpace::new(Space::SRGB_TEXTURE, aces);
    close(
        at_centre(converted, srgb),
        arnold_direct("converted .tx, bound sRGB"),
        "converted, bound sRGB",
    );
    close(
        at_centre(converted, ColorSpace::RAW),
        arnold_direct("converted .tx, bound \"\""),
        "converted, unbound",
    );
    close(
        at_centre("const_big_empty_raw.png.tx", srgb),
        arnold_direct("u8 .tx unconverted, bound sRGB"),
        "plain u8, bound sRGB",
    );
}

/// A stage beside `src` whose one material reads it through a UsdUVTexture
/// bound `sRGB`, imported through `assets`.
fn import_binding(src: &Path, assets: &crust_assets::FileAssets) -> crust_core::Scene {
    let stage = src.with_file_name("stage.usda");
    let file = src.file_name().unwrap().to_string_lossy();
    std::fs::write(
        &stage,
        format!(
            r#"#usda 1.0
(
    defaultPrim = "World"
)
def Xform "World"
{{
    def Sphere "ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        rel material:binding = </World/M>
    }}
    def Material "M"
    {{
        token outputs:surface.connect = </World/M/Surface.outputs:surface>
        def Shader "Surface"
        {{
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor.connect = </World/M/Map.outputs:rgb>
            token outputs:surface
        }}
        def Shader "Map"
        {{
            uniform token info:id = "UsdUVTexture"
            asset inputs:file = @./{file}@
            token inputs:sourceColorSpace = "sRGB"
            float3 outputs:rgb
        }}
    }}
}}
"#
        ),
    )
    .expect("write stage");
    crust_core::Scene::from_usd_with_options(&stage, assets, &Default::default()).expect("imports")
}

fn set_time(path: &Path, t: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(t))
        .expect("set time");
}

/// **A source edited after conversion**: Arnold's `.tx` is still what is
/// read without `--auto-tx`, and the import warns, naming it and the fix.
#[test]
fn a_source_edited_after_conversion_is_used_with_a_warning() {
    let src = arnold_pair("srgb_canon", "edited");
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    set_time(&src, later);
    let assets = crust_assets::FileAssets::new();
    let scene = import_binding(&src, &assets);
    assert_eq!(assets.texture_cache_stats().files, 1, "the .tx is used");
    let stale: Vec<_> = scene
        .warnings
        .iter()
        .filter(|w| w.code.as_str() == "texture.tx_stale")
        .collect();
    assert_eq!(stale.len(), 1, "{:?}", scene.warnings);
    assert!(
        stale[0].message.contains("srgb_canon.png"),
        "{}",
        stale[0].message
    );
    assert!(
        stale[0].message.contains("--auto-tx"),
        "{}",
        stale[0].message
    );
}

/// **A `.tx` Arnold wrote** — stamped with its source's time truncated to
/// the second, as OIIO stamps it — is current: `--auto-tx` streams it and
/// leaves it as it is.
#[test]
fn a_tx_arnold_wrote_is_not_rewritten_under_auto_tx() {
    let src = arnold_pair("srgb_canon", "arnold_wrote");
    let tx = src.with_file_name(case("srgb_canon")["tx"][0]["tx"].as_str().unwrap());
    let second = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_000);
    set_time(&src, second + std::time::Duration::from_millis(600));
    set_time(&tx, second);
    let before = std::fs::read(&tx).expect("read");
    let assets = crust_assets::FileAssets::new().with_auto_tx(true);
    let scene = import_binding(&src, &assets);
    assert_eq!(assets.tx_report().0, 0, "nothing converted");
    assert_eq!(
        assets.texture_cache_stats().files,
        1,
        "Arnold's .tx streams"
    );
    assert_eq!(std::fs::read(&tx).expect("read"), before, "left as it is");
    assert!(
        !scene
            .warnings
            .iter()
            .any(|w| w.code.as_str() == "texture.tx_stale"),
        "{:?}",
        scene.warnings
    );
}

/// The scenarios of "--auto-tx writes what Arnold's auto-tx writes", on
/// crust's own conversion, read back through the files' headers: an 8-bit
/// sRGB source is a 64x64-tiled, rounded-down, DWAA `half` EXR with OIIO's
/// attributes; a linear or data source an 8-bit zip TIFF; a float or 16-bit
/// source keeps its sample type; a greyscale source is one channel that a
/// lookup reads as grey.
#[test]
fn auto_tx_writes_what_arnolds_auto_tx_writes() {
    use crust_core::AssetLoader;
    use exr::meta::attribute::{AttributeValue, LevelMode};
    let dir = std::env::temp_dir().join("crust_auto_tx_scenarios");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let w = Space::LIN_REC709;
    let png = dir.join("albedo.png");
    image::RgbImage::from_fn(70, 40, |x, y| {
        image::Rgb([(x * 3) as u8, (y * 5) as u8, 90])
    })
    .save(&png)
    .expect("png");
    let convert = |src: &Path, space: ColorSpace| {
        crust_assets::make_tx_atomic(src, space, acescg()).expect("converts")
    };

    // An 8-bit sRGB source.
    let made = convert(&png, ColorSpace::SRGB);
    assert_eq!(made.kind, "half, exr, dwaa");
    let meta = exr::meta::MetaData::read_from_file(&made.dst, false).expect("exr header");
    let header = &meta.headers[0];
    assert!(matches!(
        header.compression,
        exr::compression::Compression::DWAA(_)
    ));
    let exr::meta::BlockDescription::Tiles(tiles) = header.blocks else {
        panic!("tiled")
    };
    assert_eq!(
        (tiles.tile_size.width(), tiles.tile_size.height()),
        (64, 64)
    );
    assert_eq!(tiles.level_mode, LevelMode::MipMap);
    assert_eq!(tiles.rounding_mode, exr::math::RoundingMode::Down);
    let attr = |key: &str| {
        header
            .own_attributes
            .other
            .iter()
            .find(|(k, _)| k.as_slice() == key.as_bytes())
            .map(|(_, v)| v.clone())
    };
    let text = |key: &str| match attr(key) {
        Some(AttributeValue::Text(t)) => t.to_string(),
        other => panic!("{key}: {other:?}"),
    };
    assert_eq!(text("textureformat"), "Plain Texture");
    assert_eq!(
        header
            .own_attributes
            .wrap_mode_name
            .as_ref()
            .map(|t| t.to_string()),
        Some("black,black".to_string())
    );
    assert!(matches!(attr("autotx"), Some(AttributeValue::I32(1))));
    assert!(text("Software").starts_with("OpenImageIO-Arnold 2.6.3.0dev : maketx "));

    // A linear or data source.
    for space in [
        ColorSpace::from_mtlx(Some("lin_rec709"), w),
        ColorSpace::from_mtlx(Some("Raw"), w),
    ] {
        let made = convert(&png, space);
        assert_eq!(made.kind, "8-bit, tiff");
        let mut tiff = tiff::decoder::Decoder::new(std::fs::File::open(&made.dst).expect("open"))
            .expect("a TIFF");
        assert_eq!(
            tiff.get_tag_u32(tiff::tags::Tag::Compression)
                .expect("compression"),
            8,
            "zip"
        );
    }

    // A float or 16-bit source.
    let exr_src = dir.join("hdr.exr");
    exr::prelude::write_rgb_file(&exr_src, 8, 8, |x, _| (x as f32, 0.5f32, 2.0f32)).expect("exr");
    assert_eq!(convert(&exr_src, ColorSpace::SRGB).kind, "float, exr");
    let deep = dir.join("deep.png");
    image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_fn(8, 8, |x, _| {
        image::Rgb([x as u16 * 4000, 30000, 60000])
    })
    .save(&deep)
    .expect("png");
    assert_eq!(convert(&deep, ColorSpace::SRGB).kind, "16-bit, tiff");

    // A greyscale source: one channel, read as grey.
    let grey = dir.join("grey.png");
    image::RgbImage::from_fn(16, 16, |x, _| {
        let v = (x * 15) as u8;
        image::Rgb([v, v, v])
    })
    .save(&grey)
    .expect("png");
    for space in [ColorSpace::RAW, ColorSpace::SRGB] {
        let made = convert(&grey, space);
        let assets = crust_assets::FileAssets::new();
        let tex = assets.load_texture(&grey, space).expect("loads");
        assert_eq!(assets.texture_cache_stats().files, 1, "streamed");
        let px = tex.eval(9.5 / 16.0, 0.5, 0.0);
        assert!(
            px[0] > 0.0 && (px[0] - px[1]).abs() < 1e-5 && (px[1] - px[2]).abs() < 1e-5,
            "{:?}: {px:?}",
            made.kind
        );
        let channels = if made.kind.contains("exr") {
            exr::meta::MetaData::read_from_file(&made.dst, false)
                .expect("exr")
                .headers[0]
                .channels
                .list
                .len()
        } else {
            let mut tiff =
                tiff::decoder::Decoder::new(std::fs::File::open(&made.dst).expect("open")).unwrap();
            tiff.get_tag_u32(tiff::tags::Tag::SamplesPerPixel).unwrap() as usize
        };
        assert_eq!(channels, 1, "{}", made.kind);
    }
}
