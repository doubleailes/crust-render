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
    let scene = load(
        &leaf_stage(&dir, "leaf.png"),
        &FileAssets::new().with_auto_tx(true),
    );
    assert!(dir.join("leaf.tx").exists(), "--auto-tx converted the leaf");
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
