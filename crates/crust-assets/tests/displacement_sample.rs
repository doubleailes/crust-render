//! `samples/displacement.usda` with a host that decodes its height map: both
//! meshes displace, and the largest offset is the map's maximum (1.0, read
//! raw) times the texture's `scale` of 0.3 — the stage is diced at one vertex
//! per texel, so the footprint reaches the finest level.

use crust_assets::FileAssets;
use crust_core::Scene;
use std::path::Path;

#[test]
fn the_displacement_sample_displaces_both_meshes() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/displacement.usda");
    let scene = Scene::from_usd_with_assets(&path, &FileAssets::new()).expect("loads");
    let d = &scene.stats.displacement;
    assert_eq!(d.meshes, 3, "two grounds and the cube");
    assert!(
        (d.max_offset - 0.3).abs() < 1e-6,
        "max |offset| {} — 0.3 × the map's peak of 1.0",
        d.max_offset
    );
    assert_eq!(d.at_cage, 0);
    assert!(scene.stats.report().contains("displacement"));
}

/// "A RenderMan displacement read through Ptex", in numbers: on
/// `samples/pxr_displace.usda` at its cage resolution, every vertex sits at
/// `dispAmount` (0.05) times the raw Ptex value at its owner face and corner,
/// filtered over the whole face (the cage's vertex spacing).
#[test]
fn renderman_displacement_offsets_are_disp_amount_times_the_texel() {
    use crust_core::{AssetLoader, ColorSpace, Ray, UsdImportOptions, Vec3A};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let assets = FileAssets::new();
    let options = UsdImportOptions {
        subdivision_level: Some(0),
        ..UsdImportOptions::default()
    };
    let scene = Scene::from_usd_with_options(&root.join("pxr_displace.usda"), &assets, &options)
        .expect("loads");
    assert_eq!(scene.stats.displacement.meshes, 1);
    assert_eq!(scene.stats.displacement.at_cage, 1);

    let tex = assets
        .load_ptex(&root.join("textures/quad_f32.ptx"), ColorSpace::RAW)
        .expect("the fixture opens");
    let colour = assets
        .load_ptex(&root.join("textures/quad_f32.ptx"), ColorSpace::GAMMA22)
        .expect("the fixture opens");
    // (x, z) of each cage vertex, its owner face and corner in that face.
    let vertices = [
        ((0.0f32, 0.0f32), 0u32, [0.0f32, 0.0f32]),
        ((1.0, 0.0), 0, [1.0, 0.0]),
        ((1.0, -1.0), 0, [1.0, 1.0]),
        ((0.0, -1.0), 0, [0.0, 1.0]),
        ((2.0, 0.0), 1, [1.0, 0.0]),
        ((2.0, -1.0), 1, [1.0, 1.0]),
    ];
    let mut raw_differs = false;
    for ((x, z), face, [u, v]) in vertices {
        let texel = tex.eval(face, u, v, 1.0).x;
        raw_differs |= (colour.eval(face, u, v, 1.0).x - texel).abs() > 1e-3;
        let want = 0.05 * texel;
        // A hair inside the mesh, toward its centre, so the ray meets the
        // vertex's own triangles.
        let (px, pz) = (x + (1.0 - x) * 1e-4, z + (-0.5 - z) * 1e-4);
        let ray = Ray::new(Vec3A::new(px, 50.0, pz), Vec3A::new(0.0, -1.0, 0.0));
        let hit = scene.world.intersect(&ray, 1e-4, 1e4).expect("hit");
        let y = 50.0 - hit.rec.t;
        assert!(
            (y - want).abs() < 2e-3,
            "vertex ({x}, {z}) of face {face}: at {y}, want 0.05 × {texel} = {want}"
        );
    }
    assert!(
        raw_differs,
        "the fixture's values tell raw from gamma-decoded"
    );
}

/// The MaterialX ground (`displacement_height.mtlx`) and the preview-surface
/// ground read the same map at the same scale: their displaced surfaces
/// match at every point, the one placed 2.4 units left of the other.
#[test]
fn materialx_and_preview_displacement_agree() {
    use crust_core::{Ray, Vec3A};
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/displacement.usda");
    let scene = Scene::from_usd_with_assets(&path, &FileAssets::new()).expect("loads");
    let height = |x: f32, z: f32| {
        let ray = Ray::new(Vec3A::new(x, 10.0, z), Vec3A::new(0.0, -1.0, 0.0));
        let hit = scene.world.intersect(&ray, 1e-4, 1e4).expect("hit");
        10.0 - hit.rec.t
    };
    let mut worst = 0.0f32;
    let mut peak = 0.0f32;
    for i in 0..=40 {
        for j in 0..=40 {
            let (dx, dz) = (
                -0.97 + 1.94 * i as f32 / 40.0,
                -0.97 + 1.94 * j as f32 / 40.0,
            );
            let preview = height(-2.4 + dx, dz);
            let mtlx = height(dx, dz);
            worst = worst.max((preview - mtlx).abs());
            peak = peak.max(mtlx);
        }
    }
    assert!(
        peak > 0.2,
        "the MaterialX ground is displaced (peak {peak})"
    );
    assert!(worst < 1e-5, "the grounds differ by up to {worst}");
}

/// isDunesA's `soil` network on the two-quad mesh of `pxr_displace.usda`:
/// `PxrDisplace ← PxrDispTransform (mode 2) ← PxrBlend (operation 18, multiply)`
/// of two Ptex maps. `operation` is substituted so the same stage can show a
/// different blend being refused.
fn blend_stage(name: &str, operation: i32) -> std::path::PathBuf {
    let textures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/textures")
        .canonicalize()
        .unwrap();
    let f32_map = textures.join("quad_f32.ptx");
    let u8_map = textures.join("quad_u8.ptx");
    let text = format!(
        r#"#usda 1.0
def Xform "World"
{{
    def Material "Soil"
    {{
        token outputs:ri:surface.connect = </World/Soil/Bsdf.outputs:bxdf_out>
        token outputs:ri:displacement.connect = </World/Soil/PxrDisplace.outputs:displace>
        def Shader "Bsdf"
        {{
            uniform token info:id = "PxrDisneyBsdf"
            token outputs:bxdf_out
        }}
        def Shader "PxrDisplace"
        {{
            uniform token info:id = "PxrDisplace"
            float inputs:dispAmount = 5
            float inputs:dispScalar.connect = </World/Soil/Transform.outputs:resultF>
            token outputs:displace
        }}
        def Shader "Transform"
        {{
            uniform token info:id = "PxrDispTransform"
            float inputs:dispCenter = 0.5
            float inputs:dispDepth = 0.35
            float inputs:dispHeight = 0.35
            int inputs:dispRemapMode = 2
            float inputs:dispScalar.connect = </World/Soil/Blend.outputs:resultR>
            float outputs:resultF
        }}
        def Shader "Blend"
        {{
            uniform token info:id = "PxrBlend"
            int inputs:operation = {operation}
            color3f inputs:topRGB.connect = </World/Soil/Top.outputs:resultRGB>
            color3f inputs:bottomRGB.connect = </World/Soil/Mask.outputs:resultRGB>
            float outputs:resultR
        }}
        def Shader "Top"
        {{
            uniform token info:id = "PxrPtexture"
            asset inputs:filename = @{top}@
            color3f outputs:resultRGB
        }}
        def Shader "Mask"
        {{
            uniform token info:id = "PxrPtexture"
            asset inputs:filename = @{mask}@
            color3f outputs:resultRGB
        }}
    }}
    def Mesh "Quads" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4, 4]
        int[] faceVertexIndices = [0, 1, 4, 3, 1, 2, 5, 4]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (2, 0, 0), (0, 0, -1), (1, 0, -1), (2, 0, -1)]
        rel material:binding = </World/Soil>
    }}
}}
"#,
        top = f32_map.display(),
        mask = u8_map.display()
    );
    let dir = std::env::temp_dir().join("crust_displacement_blend_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).unwrap();
    path
}

/// A `PxrBlend` multiply of two Ptex maps, as isDunesA's `soil` authors it:
/// every cage vertex sits at `dispAmount · remap(top · mask)`, both maps read
/// raw at the vertex's owner face and corner.
#[test]
fn a_pxr_blend_multiply_of_two_ptex_maps_displaces() {
    use crust_core::{AssetLoader, ColorSpace, DispRemap, Ray, UsdImportOptions, Vec3A};
    let assets = FileAssets::new();
    let options = UsdImportOptions {
        subdivision_level: Some(0),
        ..UsdImportOptions::default()
    };
    let scene = Scene::from_usd_with_options(&blend_stage("multiply", 18), &assets, &options)
        .expect("loads");
    assert_eq!(scene.stats.displacement.meshes, 1, "the blend is read");

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/textures");
    let top = assets
        .load_ptex(&root.join("quad_f32.ptx"), ColorSpace::RAW)
        .unwrap();
    let mask = assets
        .load_ptex(&root.join("quad_u8.ptx"), ColorSpace::RAW)
        .unwrap();
    let remap = DispRemap::DepthHeight {
        center: 0.5,
        depth: 0.35,
        height: 0.35,
    };
    let vertices = [
        ((0.0f32, 0.0f32), 0u32, [0.0f32, 0.0f32]),
        ((1.0, 0.0), 0, [1.0, 0.0]),
        ((1.0, -1.0), 0, [1.0, 1.0]),
        ((0.0, -1.0), 0, [0.0, 1.0]),
        ((2.0, 0.0), 1, [1.0, 0.0]),
        ((2.0, -1.0), 1, [1.0, 1.0]),
    ];
    let mut moved = 0;
    for ((x, z), face, [u, v]) in vertices {
        let s = top.eval(face, u, v, 1.0).x * mask.eval(face, u, v, 1.0).x;
        let want = 5.0 * remap.apply(s);
        let (px, pz) = (x + (1.0 - x) * 1e-4, z + (-0.5 - z) * 1e-4);
        let ray = Ray::new(Vec3A::new(px, 50.0, pz), Vec3A::new(0.0, -1.0, 0.0));
        let y = 50.0 - scene.world.intersect(&ray, 1e-4, 1e4).expect("hit").rec.t;
        assert!(
            (y - want).abs() < 2e-3,
            "vertex ({x}, {z}) of face {face}: at {y}, want 5 · remap({s}) = {want}"
        );
        moved += usize::from(want.abs() > 1e-3);
    }
    assert!(moved > 0, "the test moves something");
}

/// Any other blend operation is refused: the mesh is not displaced.
#[test]
fn another_pxr_blend_operation_is_refused() {
    let scene = Scene::from_usd_with_assets(&blend_stage("other_op", 17), &FileAssets::new())
        .expect("loads");
    assert_eq!(scene.stats.displacement.meshes, 0);
}
