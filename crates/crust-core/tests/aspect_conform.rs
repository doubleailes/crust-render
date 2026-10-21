//! `aspectRatioConformPolicy` frames the render (#339): the policy authored
//! on the render settings re-frames a camera whose aperture has another
//! aspect than the image, the schema default frames such a narrow aperture
//! exactly as crust did before it read the policy, and the policy authored
//! on the camera prim — where #339's repro put it, and where OpenUSD does not
//! read it — changes nothing.

use std::path::{Path, PathBuf};

use crust_core::{Buffer, Renderer, Scene};

/// `samples/dome_backdrop.usda` under a root layer giving its camera the
/// 3:2 aperture of #339 (narrower than its 240 × 136 image), and `overs`.
fn stage(name: &str, overs: &str) -> PathBuf {
    let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/dome_backdrop.usda");
    let sample = sample.canonicalize().expect("samples/dome_backdrop.usda");
    let dir = std::env::temp_dir().join("crust_aspect_conform_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(
        &path,
        format!(
            "#usda 1.0\n(\n    subLayers = [@{}@]\n)\n\nover \"World\"\n{{\n    over \"Cam\"\n    \
             {{\n        float verticalAperture = 13.97\n    }}\n}}\n{overs}\n",
            sample.display()
        ),
    )
    .expect("write stage");
    path
}

/// The settings prim authoring `policy`.
fn on_settings(policy: &str) -> String {
    format!(
        "over \"Render\"\n{{\n    over \"settings\"\n    {{\n        uniform token \
         aspectRatioConformPolicy = \"{policy}\"\n    }}\n}}\n"
    )
}

/// The beauty at 16 spp, where every pixel takes exactly 16 samples.
fn render(name: &str, overs: &str) -> Buffer {
    let scene = Scene::from_usd(&stage(name, overs)).unwrap_or_else(|e| panic!("{name}: {e}"));
    let settings = scene.settings.with_samples_per_pixel(16).with_max_depth(4);
    Renderer::new(scene.camera, scene.world, scene.lights, settings).render()
}

fn bits(buffer: &Buffer) -> Vec<[u32; 3]> {
    let (w, h) = buffer.size();
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| buffer.get_pixel(x, y).to_array().map(f32::to_bits))
        .collect()
}

#[test]
fn the_settings_policy_frames_the_render_and_the_camera_policy_does_not() {
    let unauthored = bits(&render("unauthored", ""));
    assert!(
        unauthored
            .iter()
            .any(|p| p.iter().any(|&c| f32::from_bits(c) > 0.0)),
        "the render sees the scene"
    );
    // #339: a horizontal fit shows less, so the image changes.
    let height = bits(&render(
        "adjust_aperture_height",
        &on_settings("adjustApertureHeight"),
    ));
    assert_ne!(unauthored, height);
    // The schema default keeps a narrow aperture's height, which is how
    // crust framed every camera before it read the policy.
    let width = bits(&render(
        "adjust_aperture_width",
        &on_settings("adjustApertureWidth"),
    ));
    assert_eq!(unauthored, width);
    // On the camera prim, the policy is not read.
    let on_camera = bits(&render(
        "on_camera",
        "over \"World\"\n{\n    over \"Cam\"\n    {\n        uniform token \
         aspectRatioConformPolicy = \"adjustApertureHeight\"\n    }\n}\n",
    ));
    assert_eq!(unauthored, on_camera);
}
