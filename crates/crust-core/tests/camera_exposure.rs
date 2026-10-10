//! The render camera's exposure, as the import reads it: USD's linear exposure
//! scale (C++ `UsdGeomCamera::ComputeLinearExposureScale`) on the scene's
//! settings, and the `enableExposureCompensation` render setting that turns it off.

use crust_core::{NoAssets, Scene, WarningCode};
use std::path::PathBuf;

/// A stage holding one camera whose body is `camera`, and `render` beside it
/// (a `RenderSettings` prim, say).
fn stage(name: &str, camera: &str, render: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_camera_exposure_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    let text = format!(
        "#usda 1.0\n(\n    renderSettingsPrimPath = \"/Render/settings\"\n)\n\n\
         def Camera \"cam\"\n{{\n{camera}\n}}\n\n{render}\n"
    );
    std::fs::write(&path, text).expect("write stage");
    path
}

fn scale(name: &str, camera: &str) -> f32 {
    let scene = Scene::from_usd(&stage(name, camera, "")).expect("import");
    scene.settings.exposure_scale()
}

/// A stage without a camera renders through the procedural one, at 1.
#[test]
fn the_procedural_camera_is_one() {
    let dir = std::env::temp_dir().join("crust_camera_exposure_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("no_camera.usda");
    std::fs::write(&path, "#usda 1.0\ndef Xform \"World\"\n{\n}\n").expect("write stage");
    let scene = Scene::from_usd(&path).expect("import");
    assert_eq!(scene.settings.exposure_scale(), 1.0);
}

#[test]
fn nothing_authored_is_one() {
    assert_eq!(scale("none", ""), 1.0);
}

#[test]
fn stops_double() {
    assert_eq!(scale("stops", "    float exposure = 2"), 4.0);
}

/// `1.5 × 0.5 × 400 × 2 / (100 × 2 × 2)`, exactly.
#[test]
fn the_photometric_set() {
    let camera = "    float exposure = 1
    float exposure:time = 0.5
    float exposure:iso = 400
    float exposure:fStop = 2
    float exposure:responsivity = 1.5";
    assert_eq!(scale("photometric", camera), 1.5);
}

/// The lens's depth-of-field f-stop is not the photometric one.
#[test]
fn the_lens_f_stop_does_not_expose() {
    assert_eq!(scale("lens_fstop", "    float fStop = 4"), 1.0);
}

#[test]
fn an_animated_exposure_follows_the_frame() {
    let camera = "    float exposure.timeSamples = {\n        1: 0,\n        2: 1,\n    }";
    let path = stage("animated", camera, "");
    let at = |frame: f64| {
        Scene::from_usd_at_frame(&path, &NoAssets, Some(frame))
            .expect("import")
            .settings
            .exposure_scale()
    };
    assert_eq!(at(1.0), 1.0);
    assert_eq!(at(2.0), 2.0);
}

/// `exposure:fStop = 0` divides by zero: refused, with a warning naming the
/// camera, and the image is not scaled.
#[test]
fn a_scale_that_cannot_apply_is_refused() {
    let scene =
        Scene::from_usd(&stage("refused", "    float exposure:fStop = 0", "")).expect("import");
    assert_eq!(scene.settings.exposure_scale(), 1.0);
    let w = scene
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::CameraInvalidExposure)
        .unwrap_or_else(|| panic!("no camera.invalid_exposure in {:#?}", scene.warnings));
    assert_eq!(w.prims, ["/cam"]);
}

fn render_settings(attrs: &str) -> String {
    format!(
        "def Scope \"Render\"\n{{\n    def RenderSettings \"settings\"\n    {{\n        \
         rel camera = </cam>\n{attrs}\n    }}\n}}"
    )
}

#[test]
fn exposure_compensation_turned_off() {
    let render = render_settings("        bool enableExposureCompensation = false");
    let scene = Scene::from_usd(&stage(
        "compensation_off",
        "    float exposure = 2",
        &render,
    ))
    .expect("import");
    assert_eq!(scene.settings.exposure_scale(), 1.0);
}

#[test]
fn the_crust_alias_wins() {
    let render = render_settings(
        "        bool enableExposureCompensation = false\n        \
         bool crust:enableExposureCompensation = true",
    );
    let scene = Scene::from_usd(&stage(
        "compensation_alias",
        "    float exposure = 2",
        &render,
    ))
    .expect("import");
    assert_eq!(scene.settings.exposure_scale(), 4.0);
}

/// The scale is checked for the camera rendered through, once: a camera whose
/// exposure cannot apply, built as the fallback while the named one is still to
/// come, raises no warning when the named camera is the one used. Both orders,
/// so that whichever order the traversal meets cameras in, one of them builds
/// the broken camera first.
#[test]
fn only_the_render_camera_is_checked() {
    let dir = std::env::temp_dir().join("crust_camera_exposure_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let broken = "def Camera \"broken\"\n{\n    float exposure:fStop = 0\n}\n\n";
    let shot = "def Camera \"shot\"\n{\n    float exposure = 2\n}\n\n";
    for (name, cameras) in [
        ("broken_first", [broken, shot]),
        ("shot_first", [shot, broken]),
    ] {
        let path = dir.join(format!("two_cameras_{name}.usda"));
        let text = format!(
            "#usda 1.0\n(\n    renderSettingsPrimPath = \"/Render/settings\"\n)\n\n{}{}\
             def Scope \"Render\"\n{{\n    def RenderSettings \"settings\"\n    {{\n        \
             rel camera = </shot>\n    }}\n}}\n",
            cameras[0], cameras[1]
        );
        std::fs::write(&path, text).expect("write stage");
        let scene = Scene::from_usd(&path).expect("import");
        assert_eq!(scene.settings.exposure_scale(), 4.0, "{name}");
        assert!(
            !scene
                .warnings
                .iter()
                .any(|w| w.code == WarningCode::CameraInvalidExposure),
            "{name}: {:#?}",
            scene.warnings
        );
    }
}

/// An exposure turned off is not checked: nothing to warn about.
#[test]
fn a_disabled_exposure_is_not_checked() {
    let render = render_settings("        bool enableExposureCompensation = false");
    let scene = Scene::from_usd(&stage(
        "disabled_broken",
        "    float exposure:fStop = 0",
        &render,
    ))
    .expect("import");
    assert_eq!(scene.settings.exposure_scale(), 1.0);
    assert!(
        !scene
            .warnings
            .iter()
            .any(|w| w.code == WarningCode::CameraInvalidExposure),
        "{:#?}",
        scene.warnings
    );
}
