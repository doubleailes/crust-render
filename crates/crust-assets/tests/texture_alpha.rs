//! A `UsdPreviewSurface` cutout reading a texture's alpha, end to end: the
//! stage through the importer, the file through `FileAssets`, preloaded and
//! streamed (issue #267). Foliage cards wire `opacity` to a `UsdUVTexture`'s
//! `outputs:a` under an `opacityThreshold`; with the alpha never decoded,
//! every texel read 1.0 and the cards rendered as solid rectangles.

use crust_assets::FileAssets;
use crust_core::{MASK_CAMERA, Ray, Scene, UsdImportOptions, Vec3A};
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crust_texture_alpha_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// The issue's leaf: a red 64x64 RGBA PNG, transparent (alpha 0) on its left
/// half and opaque on its right.
fn leaf_png(path: &Path) {
    let img = image::RgbaImage::from_fn(64, 64, |x, _| {
        image::Rgba([200, 20, 20, if x < 32 { 0 } else { 255 }])
    });
    img.save(path).expect("write png");
}

/// The issue's stage: a unit quad over `[0, 1]²` at z = 0, its `st` the
/// identity, bound to a `UsdPreviewSurface` wired as DCC exports write a
/// cutout — `opacity` from the texture's `a` under `opacityThreshold` 0.5.
fn leaf_stage(dir: &Path, file: &str) -> PathBuf {
    let stage = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
)
def Xform "World"
{{
    def Scope "Asset"
    {{
        def Material "Leaf"
        {{
            token outputs:surface.connect = </World/Asset/Leaf/Surface.outputs:surface>

            def Shader "Surface"
            {{
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </World/Asset/Leaf/Tex.outputs:rgb>
                float inputs:opacity.connect = </World/Asset/Leaf/Tex.outputs:a>
                float inputs:opacityThreshold = 0.5
                token outputs:surface
            }}

            def Shader "Tex"
            {{
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @./{file}@
                float2 inputs:st.connect = </World/Asset/Leaf/St.outputs:result>
                float3 outputs:rgb
                float outputs:a
            }}

            def Shader "St"
            {{
                uniform token info:id = "UsdPrimvarReader_float2"
                token inputs:varname = "st"
                float2 outputs:result
            }}
        }}
    }}
    def Mesh "Card" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (interpolation = "faceVarying")
        rel material:binding = </World/Asset/Leaf>
    }}
}}
"#
    );
    let path = dir.join("leaf.usda");
    std::fs::write(&path, stage).expect("write stage");
    path
}

/// The card's opacity under a camera ray straight down at `x`: what the
/// integrator asks before it shades the hit.
fn opacity_at(scene: &Scene, x: f32) -> f32 {
    let r = Ray::new(Vec3A::new(x, 0.5, 5.0), Vec3A::new(0.0, 0.0, -1.0)).with_mask(MASK_CAMERA);
    let hit = scene
        .world
        .intersect(&r, 0.001, f32::INFINITY)
        .expect("hits the card");
    assert!(hit.mat.has_cutout(), "the preview surface is a cutout");
    hit.mat.opacity(&r, &hit.rec)
}

fn load(path: &Path, assets: &FileAssets) -> Scene {
    Scene::from_usd_with_options(path, assets, &UsdImportOptions::default()).expect("stage loads")
}

/// The issue's reproduction, preloaded: the left half is cut away, the right
/// half kept, and the import says nothing about the material — the alpha is
/// read, not approximated.
#[test]
fn a_preview_cutout_reads_its_texture_alpha() {
    let dir = scratch("preload");
    leaf_png(&dir.join("leaf.png"));
    let scene = load(&leaf_stage(&dir, "leaf.png"), &FileAssets::new());
    assert_eq!(opacity_at(&scene, 0.25), 0.0, "the transparent half is cut");
    assert_eq!(opacity_at(&scene, 0.75), 1.0, "the opaque half is kept");
    let preview: Vec<_> = scene
        .warnings
        .iter()
        .filter(|w| w.code.as_str().starts_with("preview."))
        .collect();
    assert!(preview.is_empty(), "nothing approximated: {preview:?}");
}

/// The same card streamed from the `.tx` `--auto-tx` writes beside it: the
/// conversion keeps the alpha, and the streamed mask is the preloaded one.
#[test]
fn a_streamed_cutout_reads_the_same_alpha() {
    let dir = scratch("streamed");
    leaf_png(&dir.join("leaf.png"));
    let assets = FileAssets::new().with_auto_tx(true);
    let scene = load(&leaf_stage(&dir, "leaf.png"), &assets);
    assert_eq!(assets.tx_report().0, 1, "--auto-tx converted the leaf");
    assert_eq!(assets.texture_cache_stats().files, 1, "and it streams");
    assert_eq!(opacity_at(&scene, 0.25), 0.0);
    assert_eq!(opacity_at(&scene, 0.75), 1.0);
}

/// A texture without alpha reads 1.0 at `outputs:a`, as `UsdUVTexture`
/// specifies, so the same wiring over an RGB file keeps the whole card.
#[test]
fn a_texture_without_alpha_reads_opaque() {
    let dir = scratch("no_alpha");
    image::RgbImage::from_pixel(8, 8, image::Rgb([200, 20, 20]))
        .save(dir.join("solid.png"))
        .expect("write png");
    let scene = load(&leaf_stage(&dir, "solid.png"), &FileAssets::new());
    assert_eq!(opacity_at(&scene, 0.25), 1.0);
    assert_eq!(opacity_at(&scene, 0.75), 1.0);
}

/// The leaf's `.tx` as a crust from before this change wrote it: RGB, with
/// What an older crust left beside the leaf: `leaf.tx`, under its own old
/// name, converted from an RGB image — so without the leaf's alpha.
fn pre_alpha_tx(dir: &Path) -> PathBuf {
    let rgb = dir.join("leaf_rgb.png");
    image::RgbImage::from_pixel(64, 64, image::Rgb([200, 20, 20]))
        .save(&rgb)
        .expect("write png");
    let tx = dir.join("leaf.tx");
    crust_assets::make_tx(
        &rgb,
        &tx,
        crust_core::ColorSpace::AUTO,
        crust_assets::default_tx_color_space(),
    )
    .expect("convert");
    std::fs::remove_file(&rgb).expect("remove");
    tx
}

/// A `.tx` an older crust converted from the leaf, alpha dropped, under its
/// old name `leaf.tx`, is not looked for: without `--auto-tx` the leaf
/// preloads and cuts; with it, the `.tx` Arnold names is written beside the
/// leaf and streams the alpha.
#[test]
fn a_tx_from_before_alpha_is_not_looked_for() {
    let dir = scratch("pre_alpha_tx");
    leaf_png(&dir.join("leaf.png"));
    pre_alpha_tx(&dir);
    let stage = leaf_stage(&dir, "leaf.png");

    let scene = load(&stage, &FileAssets::new());
    assert_eq!(opacity_at(&scene, 0.25), 0.0, "the old .tx does not answer");
    assert_eq!(opacity_at(&scene, 0.75), 1.0);

    let assets = FileAssets::new().with_auto_tx(true);
    let scene = load(&stage, &assets);
    assert_eq!(opacity_at(&scene, 0.25), 0.0);
    assert_eq!(opacity_at(&scene, 0.75), 1.0);
    assert_eq!(assets.tx_report().0, 1, "--auto-tx converted the leaf");
    let arnold_named = std::fs::read_dir(&dir).expect("list").any(|e| {
        let name = e.expect("entry").file_name().to_string_lossy().into_owned();
        name.starts_with("leaf_") && name.ends_with(".png.tx")
    });
    assert!(arnold_named, "the .tx is written under Arnold's name");
}

/// **The `Texture alpha` scenarios on both backends** (design decision 9 of
/// `tx-arnold-convention`): a lookup's colour is premultiplied by its alpha,
/// in linear light, preloaded from the PNG and streamed from the `.tx`
/// `--auto-tx` writes alike — a leaf card's cut half is black, a texture at
/// alpha 128 reads its decoded colour times 128/255 at every footprint, and a
/// texture without alpha reads its colour and 1.0.
#[test]
fn texture_colour_is_premultiplied_preloaded_and_streamed() {
    use crust_core::AssetLoader;
    let dir = scratch("premultiplied");
    leaf_png(&dir.join("leaf.png"));
    image::RgbaImage::from_pixel(32, 32, image::Rgba([230, 100, 20, 128]))
        .save(dir.join("half.png"))
        .expect("png");
    image::RgbImage::from_pixel(32, 32, image::Rgb([230, 100, 20]))
        .save(dir.join("rgb.png"))
        .expect("png");
    let srgb = |b: u8| crust_core::ResolvedColorSpace::SRGB.decode_curve(b as f32 / 255.0);
    let preload = FileAssets::with_config(crust_core::Config {
        tex_stream: false,
        ..Default::default()
    });
    let streamed = FileAssets::new().with_auto_tx(true);
    for (assets, what) in [(&preload, "preloaded"), (&streamed, "streamed")] {
        let load = |f: &str| {
            assets
                .load_texture(&dir.join(f), crust_core::ColorSpace::SRGB)
                .expect("loads")
        };
        let close = |got: f32, want: f32, at: &str| {
            assert!(
                (got - want).abs() < 0.004,
                "{what} {at}: {got} against {want}"
            );
        };

        let leaf = load("leaf.png");
        let cut = leaf.eval(0.25, 0.5, 0.0);
        assert_eq!(cut[3], 0.0, "{what}");
        for c in &cut[..3] {
            close(*c, 0.0, "cut colour");
        }
        let kept = leaf.eval(0.75, 0.5, 0.0);
        assert_eq!(kept[3], 1.0, "{what}");
        close(kept[0], srgb(200), "kept colour");

        let half = load("half.png");
        for width in [0.0f32, 0.05, 0.3, 2.0] {
            let px = half.eval(0.5, 0.5, width);
            close(px[3], 128.0 / 255.0, "alpha");
            close(px[0], srgb(230) * 128.0 / 255.0, "premultiplied colour");
        }

        let rgb = load("rgb.png").eval(0.5, 0.5, 0.0);
        assert_eq!(rgb[3], 1.0, "{what}");
        close(rgb[0], srgb(230), "colour without alpha");
    }
    assert_eq!(
        streamed.texture_cache_stats().files,
        3,
        "all three streamed"
    );
}
