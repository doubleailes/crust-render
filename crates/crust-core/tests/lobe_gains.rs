//! UsdLux `inputs:diffuse` / `inputs:specular`, a light's multipliers on the
//! diffuse and the specular response: read by the import, and applied on
//! both sides of MIS — NEE's connection and the emission a bounce reaches —
//! so every strategy agrees and the image is linear in the two multipliers.

use crust_core::{Buffer, LobeGains, Renderer, Scene, WarningCode};
use std::path::PathBuf;

fn write_stage(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_lobe_gains_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

fn render(scene: Scene) -> Buffer {
    Renderer::new(scene.camera, scene.world, scene.lights, scene.settings)
        .with_volumes(scene.volumes)
        .render()
}

const SIZE: usize = 16;

/// A floor of `material` (UsdPreviewSurface inputs) seen from above, lit by
/// a sphere light near its mirror direction (found by a bounce hitting it)
/// and a distant sun (found by a bounce escaping), both with `gains`
/// authored. Depth 2, so the floor's bounce reaches either.
fn scene(name: &str, material: &str, gains: &str, strategy: &str, seed: isize) -> Scene {
    let path = stage(name, material, gains, strategy);
    let mut scene = Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
    scene.settings = scene.settings.with_frame(seed);
    scene
}

/// [`scene`]'s stage, written to disk.
fn stage(name: &str, material: &str, gains: &str, strategy: &str) -> PathBuf {
    let text = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 50
        float horizontalAperture = 20
        float verticalAperture = 20
        double3 xformOp:translate = (0, 5, 6)
        float xformOp:rotateX = -39.8
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateX"]
    }}
    def Material "M"
    {{
        token outputs:surface.connect = </World/M/S.outputs:surface>
        def Shader "S"
        {{
            uniform token info:id = "UsdPreviewSurface"
            {material}
            token outputs:surface
        }}
    }}
    def Mesh "Floor" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-10, 0, -10), (-10, 0, 10), (10, 0, 10), (10, 0, -10)]
        rel material:binding = </World/M>
    }}
    def SphereLight "Key"
    {{
        float inputs:radius = 0.5
        float inputs:intensity = 50
        {gains}
        double3 xformOp:translate = (0, 5, -6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def DistantLight "Sun"
    {{
        float inputs:angle = 5
        float inputs:intensity = 5
        {gains}
        float xformOp:rotateX = -120
        uniform token[] xformOpOrder = ["xformOp:rotateX"]
    }}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        int2 resolution = ({SIZE}, {SIZE})
        int crust:samplesPerPixel = 16
        int crust:minSamplesPerPixel = 16
        int crust:maxDepth = 2
        float crust:varianceThreshold = 0
        float crust:indirectClamp = 0
        token crust:samplingStrategy = "{strategy}"
    }}
}}
"#
    );
    write_stage(name, &text)
}

const PLASTIC: &str = "color3f inputs:diffuseColor = (0.5, 0.4, 0.3)
            float inputs:roughness = 0.3";
const METAL: &str = "color3f inputs:diffuseColor = (0.9, 0.8, 0.7)
            float inputs:metallic = 1
            float inputs:roughness = 0.3";

fn gains(diffuse: f32, specular: f32) -> String {
    format!("float inputs:diffuse = {diffuse}\n        float inputs:specular = {specular}")
}

fn image(material: &str, d: f32, s: f32, strategy: &str, seed: isize) -> Vec<[f64; 3]> {
    let name = format!(
        "{}_{d}_{s}_{strategy}_{seed}",
        if material == METAL {
            "metal"
        } else {
            "plastic"
        }
    );
    let b = render(scene(&name, material, &gains(d, s), strategy, seed));
    let mut out = Vec::with_capacity(SIZE * SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let c = b.get_pixel(x, y);
            out.push([c.x as f64, c.y as f64, c.z as f64]);
        }
    }
    out
}

fn mean(img: &[[f64; 3]]) -> f64 {
    img.iter().map(|c| c[0] + c[1] + c[2]).sum::<f64>() / (3 * img.len()) as f64
}

/// The importer reads both multipliers onto the light list, with no warning
/// left for them; a light that authors neither keeps `LobeGains::ONE`.
#[test]
fn the_import_reads_both_multipliers() {
    let s = scene("import", PLASTIC, &gains(0.25, 3.0), "power", 0);
    assert!(s.lights.has_lobe_gains());
    for i in 0..s.lights.count() {
        assert_eq!(
            s.lights.lobe_gains(i),
            LobeGains {
                diffuse: 0.25,
                specular: 3.0
            }
        );
    }
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);

    let plain = scene("import_plain", PLASTIC, "", "power", 0);
    assert!(!plain.lights.has_lobe_gains());
    assert_eq!(plain.lights.lobe_gains(0), LobeGains::ONE);
}

/// A non-finite multiplier is refused for its fallback, 1, with the light
/// inputs' own warning.
#[test]
fn a_non_finite_multiplier_falls_back_to_one() {
    let s = scene(
        "non_finite",
        PLASTIC,
        "float inputs:diffuse = nan\n        float inputs:specular = 0.5",
        "power",
        0,
    );
    assert_eq!(
        s.lights.lobe_gains(0),
        LobeGains {
            diffuse: 1.0,
            specular: 0.5
        }
    );
    assert!(
        s.warnings
            .iter()
            .any(|w| w.code == WarningCode::LightNonFiniteInput),
        "{:?}",
        s.warnings
    );
}

/// Nothing a multiplier changes steers an unguided render's sampling (a
/// guided one trains on the gained radiance), so under every strategy the
/// image is linear in the pair pixel by pixel, to rounding: `(d, s)` renders
/// as `d` times the diffuse-only light plus `s` times the specular-only one,
/// and the two halves sum to the light at `(1, 1)` — which takes the plain,
/// unsplit path.
#[test]
fn the_image_is_linear_in_the_multipliers() {
    for strategy in ["light", "bsdf", "power"] {
        let diffuse = image(PLASTIC, 1.0, 0.0, strategy, 1);
        let specular = image(PLASTIC, 0.0, 1.0, strategy, 1);
        let whole = image(PLASTIC, 1.0, 1.0, strategy, 1);
        let mixed = image(PLASTIC, 0.25, 3.0, strategy, 1);
        assert!(
            mean(&diffuse) > 1e-3 && mean(&specular) > 1e-3,
            "{strategy}"
        );
        for i in 0..whole.len() {
            for c in 0..3 {
                let (d, s) = (diffuse[i][c], specular[i][c]);
                let tol = 1e-4 * (d.abs() + s.abs()) + 1e-6;
                assert!(
                    (d + s - whole[i][c]).abs() <= tol,
                    "{strategy} pixel {i}: {d} + {s} vs {}",
                    whole[i][c]
                );
                let want = 0.25 * d + 3.0 * s;
                assert!(
                    (want - mixed[i][c]).abs() <= 3.0 * tol,
                    "{strategy} pixel {i}: {want} vs {}",
                    mixed[i][c]
                );
            }
        }
    }
}

/// A metal has no diffuse response: a diffuse-only light leaves it black and
/// a specular-only one lights it as the plain light does.
#[test]
fn a_metal_answers_to_the_specular_multiplier_only() {
    let diffuse = image(METAL, 1.0, 0.0, "power", 1);
    let specular = image(METAL, 0.0, 1.0, "power", 1);
    let whole = image(METAL, 1.0, 1.0, "power", 1);
    assert!(mean(&diffuse) <= 1e-6 * mean(&whole), "{}", mean(&diffuse));
    assert!(
        (mean(&specular) - mean(&whole)).abs() <= 1e-4 * mean(&whole),
        "{} vs {}",
        mean(&specular),
        mean(&whole)
    );
}

/// Mean and standard error of the mean.
fn mean_and_error(xs: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
    (mean, (var / n).sqrt())
}

/// Both MIS sides scale a light alike: NEE alone, BSDF sampling alone and
/// their combination estimate the same image of a glossy plastic under a
/// light that dims the diffuse and boosts the highlight. A gain on one side
/// only would leave the strategies apart by the highlight.
#[test]
fn every_strategy_agrees_under_a_split_light() {
    let seeds = [1, 2, 3, 4];
    let stats: Vec<(&str, (f64, f64))> = ["light", "bsdf", "power"]
        .into_iter()
        .map(|s| {
            let means: Vec<f64> = seeds
                .iter()
                .map(|&seed| mean(&image(PLASTIC, 0.25, 3.0, s, seed)))
                .collect();
            (s, mean_and_error(&means))
        })
        .collect();
    for (i, (a, (ma, ea))) in stats.iter().enumerate() {
        for (b, (mb, eb)) in &stats[i + 1..] {
            let sigma = (ea * ea + eb * eb).sqrt();
            assert!(
                (ma - mb).abs() <= 5.0 * sigma + 1e-3 * ma.abs(),
                "{a} {ma} ± {ea} vs {b} {mb} ± {eb}"
            );
        }
    }
}

fn beauty_bits(scene: Scene) -> Vec<[u32; 3]> {
    let b = render(scene);
    (0..SIZE)
        .flat_map(|y| (0..SIZE).map(move |x| (x, y)))
        .map(|(x, y)| b.get_pixel(x, y).to_array().map(f32::to_bits))
        .collect()
}

/// An edit of the multipliers is a light edit: applied in place it renders
/// bit for bit what a fresh import of the edited layer renders — the light
/// list keeps the multipliers beside the light, so the rebuild carries them.
#[test]
fn a_multiplier_edit_applies_in_place_as_a_reimport_would() {
    let path = stage("live", PLASTIC, "", "power");
    let mut scene = Scene::from_usd(&path).expect("load");
    let before = beauty_bits(Scene::from_usd(&path).expect("load"));
    stage("live", PLASTIC, &gains(0.25, 3.0), "power");
    let change = crust_core::live::Change {
        lights: vec!["/World/Key".into(), "/World/Sun".into()],
        ..Default::default()
    };
    let update = scene
        .provenance
        .as_ref()
        .expect("provenance")
        .rebuild(
            &path,
            &crust_core::NoAssets,
            &scene.warnings,
            &change,
            &scene.world,
        )
        .expect("a light edit applies in place");
    update.apply(&mut scene).expect("applies");
    assert!(scene.lights.has_lobe_gains());
    let fresh = Scene::from_usd(&path).expect("load the edited layer");
    let after = beauty_bits(scene);
    assert_eq!(after, beauty_bits(fresh));
    assert_ne!(after, before, "the edit changed the render");
}

#[test]
fn opposite_extreme_gains_blend_to_finite_values() {
    // Both finite, so the import keeps them; their difference is not.
    let gains = LobeGains {
        diffuse: -f32::MAX,
        specular: f32::MAX,
    };
    for s in [0.0, 0.25, 0.5, 1.0] {
        let g = gains.blend(glam::Vec3A::splat(s));
        assert!(g.is_finite(), "share {s}: {g}");
    }
    assert_eq!(
        gains.blend(glam::Vec3A::ZERO),
        glam::Vec3A::splat(-f32::MAX)
    );
    assert_eq!(gains.blend(glam::Vec3A::ONE), glam::Vec3A::splat(f32::MAX));
}
