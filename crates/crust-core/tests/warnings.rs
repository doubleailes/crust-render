//! Coded import warnings, end to end: small stages written on the fly, each
//! authoring one cause per code, and the records the import hands back on
//! `Scene::warnings` — code, kind, count and prims. Also: every sample stage
//! imports to the same records twice.

use crust_core::{NoAssets, Scene, Warning, WarningCode, WarningKind};
use std::path::{Path, PathBuf};

fn write_stage(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_warnings_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

/// `body` under `/World`, `root` beside it (a `/Render` scope, say), with a
/// camera so no stage falls back to the procedural one unless it means to.
fn load(name: &str, body: &str, root: &str) -> Scene {
    let text = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{{
    def Camera "cam"
    {{
    }}
{body}
}}
{root}
"#
    );
    let path = write_stage(name, &text);
    Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn record(scene: &Scene, code: WarningCode) -> &Warning {
    scene
        .warnings
        .iter()
        .find(|w| w.code == code)
        .unwrap_or_else(|| panic!("no {code} record in {:#?}", scene.warnings))
}

fn codes(scene: &Scene) -> Vec<WarningCode> {
    scene.warnings.iter().map(|w| w.code).collect()
}

#[test]
fn a_clean_stage_has_no_warnings() {
    let scene = load("clean", "", "");
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
}

#[test]
fn render_settings_warnings() {
    let scene = load(
        "settings",
        "",
        r#"def Scope "Render"
{
    def RenderSettings "settings"
    {
        int crust:minSamplesPerPixel = -4
        token crust:samplingStrategy = "sometimes"
        int crust:lightSamples = 1000000
        rel camera = </World/Nope>
        string renderingColorSpace = "not_a_space"
    }
}
"#,
    );
    let invalid = record(&scene, WarningCode::SettingsInvalidValue);
    assert_eq!(invalid.kind, WarningKind::Refused);
    assert_eq!(invalid.count, 2);
    assert_eq!(invalid.prims, ["/Render/settings"]);
    let clamped = record(&scene, WarningCode::SettingsLightSamplesClamped);
    assert_eq!(clamped.kind, WarningKind::Approximated);
    let space = record(&scene, WarningCode::ColorWorkingSpaceRefused);
    assert_eq!(space.kind, WarningKind::Refused);
    let camera = record(&scene, WarningCode::CameraNotACamera);
    assert_eq!(camera.prims, ["/World/Nope"]);
    assert!(
        !camera.message.starts_with('['),
        "the record's message has no code prefix: {}",
        camera.message
    );
}

#[test]
fn a_frame_outside_the_time_range_is_stage_level() {
    let path = write_stage(
        "time",
        r#"#usda 1.0
(
    startTimeCode = 1
    endTimeCode = 10
)
def Camera "cam"
{
}
"#,
    );
    let scene = Scene::from_usd_at_frame(&path, &NoAssets, Some(42.0)).expect("loads");
    let time = record(&scene, WarningCode::TimeOutsideRange);
    assert_eq!(time.count, 1);
    assert!(time.prims.is_empty());
}

#[test]
fn a_stage_without_a_camera() {
    let path = write_stage("no_camera", "#usda 1.0\ndef Xform \"World\"\n{\n}\n");
    let scene = Scene::from_usd(&path).expect("loads");
    assert_eq!(codes(&scene), [WarningCode::CameraMissing]);
}

#[test]
fn product_and_aov_warnings() {
    let scene = load(
        "products",
        "",
        r#"def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel products = [</Render/beauty>, </Render/notAProduct>]
    }
    def RenderProduct "beauty"
    {
        token productName = "beauty.exr"
        rel orderedVars = [</Render/color>, </Render/bogus>, </Render/badLpe>, </Render/primvar>]
    }
    def Scope "notAProduct"
    {
    }
    def RenderVar "color"
    {
        uniform string sourceName = "color"
    }
    def RenderVar "bogus"
    {
        uniform string sourceName = "noSuchSource"
    }
    def RenderVar "badLpe"
    {
        uniform token sourceType = "lpe"
        uniform string sourceName = "C<<<"
    }
    def RenderVar "primvar"
    {
        uniform token sourceType = "primvar"
        uniform string sourceName = "st"
    }
}
"#,
    );
    assert_eq!(
        record(&scene, WarningCode::ProductNotARenderProduct).prims,
        ["/Render/notAProduct"]
    );
    assert_eq!(
        record(&scene, WarningCode::AovUnknownSource).prims,
        ["/Render/bogus"]
    );
    assert_eq!(
        record(&scene, WarningCode::LpeInvalid).prims,
        ["/Render/badLpe"]
    );
    assert_eq!(
        record(&scene, WarningCode::AovUnsupportedSource).prims,
        ["/Render/primvar"]
    );
    for w in &scene.warnings {
        assert_eq!(w.kind, WarningKind::Skipped, "{w:?}");
    }
}

#[test]
fn transform_curves_and_volume_warnings() {
    let scene = load(
        "geometry",
        r#"    def Xform "odd" (
        prepend apiSchemas = []
    )
    {
        double3 xformOp:translate = (0, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:frobnicate"]
    }
    def BasisCurves "hair"
    {
        uniform token type = "cubic"
        uniform token basis = "hermite"
        int[] curveVertexCounts = [4]
        point3f[] points = [(0, 0, 0), (0, 1, 0), (0, 2, 0), (0, 3, 0)]
    }
    def Cube "fog"
    {
        token crust:volume:type = "plasma"
    }
"#,
        "",
    );
    let curves = record(&scene, WarningCode::CurvesUnsupportedBasis);
    assert_eq!(curves.prims, ["/World/hair"]);
    let volume = record(&scene, WarningCode::VolumeUnknownType);
    assert_eq!(volume.prims, ["/World/fog"]);
    assert_eq!(volume.kind, WarningKind::Skipped);
    let op = record(&scene, WarningCode::XformUnknownOp);
    assert_eq!(op.prims, ["/World/odd"]);
}

/// A prim of a type the registry does not know drops its ops with a warning;
/// a `Scope` drops them silently, as C++ USD does by definition.
#[test]
fn an_unknown_prim_type_drops_its_ops() {
    let scene = load(
        "unknown_type",
        r#"    def StudioRig "rig"
    {
        double3 xformOp:translate = (5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        def Sphere "ball"
        {
        }
    }
    def Scope "group"
    {
        double3 xformOp:translate = (5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
"#,
        "",
    );
    let unknown = record(&scene, WarningCode::XformUnknownType);
    assert_eq!(unknown.prims, ["/World/rig"]);
    assert_eq!(unknown.kind, WarningKind::Skipped);
}

#[test]
fn a_point_instancer_without_prototypes() {
    let scene = load(
        "instancer",
        r#"    def PointInstancer "scatter"
    {
        int[] protoIndices = [0, 0]
        point3f[] positions = [(0, 0, 0), (1, 0, 0)]
    }
"#,
        "",
    );
    let r = record(&scene, WarningCode::InstancingNoPrototypes);
    assert_eq!(r.kind, WarningKind::Skipped);
    assert_eq!(r.prims, ["/World/scatter"]);
}

/// Two zero-width `RectLight`s: one `light.degenerate_shape` record, of kind
/// `skipped`, counting both.
#[test]
fn degenerate_lights_are_skipped() {
    let scene = load(
        "degenerate_lights",
        r#"    def RectLight "a"
    {
        float inputs:width = 0
    }
    def RectLight "b"
    {
        float inputs:width = 0
    }
"#,
        "",
    );
    assert_eq!(codes(&scene), [WarningCode::LightDegenerateShape]);
    let r = &scene.warnings[0];
    assert_eq!(r.kind, WarningKind::Skipped);
    assert_eq!(r.count, 2);
    // In the traversal's order, which visits siblings last to first.
    assert_eq!(r.prims, ["/World/b", "/World/a"]);
}

#[test]
fn a_non_finite_intensity_is_refused() {
    let scene = load(
        "inf_light",
        r#"    def SphereLight "hot"
    {
        float inputs:intensity = inf
    }
"#,
        "",
    );
    let r = record(&scene, WarningCode::LightNonFiniteInput);
    assert_eq!(r.kind, WarningKind::Refused);
    assert_eq!(r.prims, ["/World/hot"]);
}

/// Two differently broken materials — no surface shader, an unrecognised
/// `info:id` — share `material.fallback_default`.
#[test]
fn two_broken_materials_share_one_code() {
    let scene = load(
        "broken_materials",
        r#"    def Material "Empty"
    {
    }
    def Material "Alien"
    {
        token outputs:surface.connect = </World/Alien/S.outputs:surface>
        def Shader "S"
        {
            uniform token info:id = "AlienSurface"
            token outputs:surface
        }
    }
    def Sphere "a" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding = </World/Empty>
    }
    def Sphere "b" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding = </World/Alien>
    }
"#,
        "",
    );
    let r = record(&scene, WarningCode::MaterialFallbackDefault);
    assert_eq!(r.kind, WarningKind::Skipped);
    assert_eq!(r.count, 2);
    assert_eq!(r.prims, ["/World/Empty", "/World/Alien"]);
}

/// Without a host that decodes assets, a dome's map is declined by the
/// default loader (`asset.unsupported_by_host`), which is the whole story:
/// the file was never read, so it is not also `light_map.unreadable`.
#[test]
fn a_dome_map_without_a_decoder() {
    let scene = load(
        "dome_no_assets",
        r#"    def DomeLight "sky"
    {
        asset inputs:texture:file = @sky.exr@
    }
"#,
        "",
    );
    assert_eq!(codes(&scene), [WarningCode::AssetUnsupportedByHost]);
}

/// Every sample imports to the same records twice, and every code it raises
/// is one the vocabulary lists.
#[test]
fn sample_imports_are_deterministic() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let mut stages: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("samples")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "usda"))
        .collect();
    stages.sort();
    assert!(!stages.is_empty());
    for stage in stages {
        let first = Scene::from_usd(&stage).map(|s| s.warnings);
        let second = Scene::from_usd(&stage).map(|s| s.warnings);
        match (first, second) {
            (Ok(a), Ok(b)) => {
                assert_eq!(a, b, "{}", stage.display());
                for w in &a {
                    assert!(WarningCode::ALL.contains(&w.code));
                }
            }
            (Err(_), Err(_)) => {}
            (a, b) => panic!("{}: {a:?} vs {b:?}", stage.display()),
        }
    }
}
