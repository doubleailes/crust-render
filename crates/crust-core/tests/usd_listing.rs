//! `Scene::list_usd_records` (`crust ls --json`): the listed prims with the
//! values a render reads for each, read through the import's own readers —
//! so an unauthored value is the render's fallback, the camera marked
//! `is_render_camera` is the one the import renders through, and a
//! material's `bound` is the import's binding resolution, not a raw target.

use crust_core::{
    CameraRecord, LightRecord, ListKind, ListRecord, MaterialRecord, NoAssets, Scene,
    UsdImportOptions,
};
use std::path::{Path, PathBuf};

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

fn stage(name: &str, usda: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_usd_listing");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(name);
    std::fs::write(&path, usda).expect("write stage");
    path
}

fn records(path: &Path, kind: ListKind, frame: Option<f64>) -> Vec<ListRecord> {
    Scene::list_usd_records(path, kind, frame).expect("lists")
}

fn cameras(path: &Path) -> Vec<CameraRecord> {
    records(path, ListKind::Camera, None)
        .into_iter()
        .map(|r| match r {
            ListRecord::Camera(c) => c,
            other => panic!("not a camera: {other:?}"),
        })
        .collect()
}

fn lights(path: &Path, frame: Option<f64>) -> Vec<LightRecord> {
    records(path, ListKind::Light, frame)
        .into_iter()
        .map(|r| match r {
            ListRecord::Light(l) => l,
            other => panic!("not a light: {other:?}"),
        })
        .collect()
}

fn materials(path: &Path) -> Vec<MaterialRecord> {
    records(path, ListKind::Material, None)
        .into_iter()
        .map(|r| match r {
            ListRecord::Material(m) => m,
            other => panic!("not a material: {other:?}"),
        })
        .collect()
}

/// The records are the text listing's prims, in its order, on a stage that
/// imports whole and on one that streams (the Cornell box).
#[test]
fn records_are_the_listing_in_its_order() {
    let solo = stage(
        "solo.usda",
        r#"#usda 1.0
def Camera "Rig"
{
    def Xform "A" {}
    def Xform "B" { def Material "Look" {} }
    def Xform "C" { def Camera "Inner" {} }
    def Xform "D" { def RectLight "Key" {} }
}
"#,
    );
    for path in [solo, sample("cornellbox.usda"), sample("usdlux.usda")] {
        for kind in [ListKind::Camera, ListKind::Light, ListKind::Material] {
            let paths = Scene::list_usd(&path, kind).expect("lists");
            let recs: Vec<String> = records(&path, kind, None)
                .iter()
                .map(|r| r.path().to_owned())
                .collect();
            assert_eq!(recs, paths, "{} {kind:?}", path.display());
        }
    }
}

const CAMERAS: &str = r#"#usda 1.0
def Xform "cams"
{
    def Camera "layout"
    {
        float focalLength = 35
        float horizontalAperture = 36
        float verticalAperture = 24
        float fStop = 2.8
        float focusDistance = 4
    }
    def Camera "shot" {}
    def Xform "Hidden"
    {
        token visibility = "invisible"
        def Camera "witness" {}
    }
}
"#;

/// A lens as the render reads it: authored values as authored, an
/// unauthored focal length and aperture at the render's fallbacks (50, and
/// 20.955 with the vertical one from the image's aspect), a pinhole without
/// an f-stop.
#[test]
fn camera_records_read_the_lens_as_the_render_does() {
    let path = stage(
        "lens.usda",
        &format!(
            "{CAMERAS}def Scope \"Render\"\n{{\n    def RenderSettings \"settings\"\n    {{\n        \
             int2 resolution = (200, 100)\n    }}\n}}\n"
        ),
    );
    let cams = cameras(&path);
    let names: Vec<&str> = cams.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        names,
        ["/cams/layout", "/cams/shot", "/cams/Hidden/witness"]
    );
    let layout = &cams[0];
    assert_eq!(layout.focal_length_mm, 35.0);
    assert_eq!(layout.aperture_mm, [36.0, 24.0]);
    assert_eq!((layout.f_stop, layout.focus_distance), (2.8, 4.0));
    let shot = &cams[1];
    assert_eq!(shot.focal_length_mm, 50.0);
    assert_eq!(shot.aperture_mm, [20.955, 20.955 * 100.0 / 200.0]);
    assert_eq!((shot.f_stop, shot.focus_distance), (0.0, 10.0));
    assert!(!shot.hidden && cams[2].hidden);
}

/// The camera marked is the one the render goes through: a product's over
/// `RenderSettings.camera`; the fallback when the settings name a missing
/// one — checked against what the import itself records; none on a stage
/// without cameras.
#[test]
fn the_render_camera_is_the_one_the_import_uses() {
    let marked = |path: &Path| -> Vec<String> {
        cameras(path)
            .into_iter()
            .filter(|c| c.is_render_camera)
            .map(|c| c.path)
            .collect()
    };
    let with_settings = |name: &str, settings: &str| {
        stage(
            name,
            &format!(
                "{CAMERAS}def Scope \"Render\"\n{{\n    def RenderSettings \"settings\"\n    {{\n{settings}\n    }}\n}}\n"
            ),
        )
    };
    let product = with_settings(
        "product.usda",
        "        rel camera = </cams/layout>\n        rel products = [</Render/settings/p>]\n        \
         def RenderProduct \"p\" { rel camera = </cams/shot> }",
    );
    assert_eq!(marked(&product), ["/cams/shot"]);
    let settings = with_settings("settings.usda", "        rel camera = </cams/layout>");
    assert_eq!(marked(&settings), ["/cams/layout"]);

    for (name, settings) in [
        ("missing.usda", "        rel camera = </cams/nope>"),
        ("unnamed.usda", ""),
    ] {
        let path = with_settings(name, settings);
        let imported = Scene::from_usd_with_options(&path, &NoAssets, &UsdImportOptions::default())
            .expect("imports");
        let render = imported.camera_path.expect("a stage camera");
        assert_eq!(marked(&path), [render], "{name}");
    }

    let none = stage("no_camera.usda", "#usda 1.0\ndef Sphere \"S\" {}\n");
    assert!(cameras(&none).is_empty());
    assert!(marked(&none).is_empty());
}

/// Lights as authored, at the time code asked for: an animated intensity
/// reads its default without one and its sample with one, and an unauthored
/// input reads the schema fallback of the light's own type.
#[test]
fn light_records_are_the_authored_inputs_at_the_time_code() {
    let lux = lights(&sample("usdlux.usda"), None);
    assert!(!lux.is_empty());
    for l in &lux {
        assert!(
            ["sphere", "rect", "disk", "cylinder", "distant", "dome"].contains(&l.kind),
            "{}: {}",
            l.path,
            l.kind
        );
    }
    let path = stage(
        "animated.usda",
        r#"#usda 1.0
def SphereLight "Key"
{
    float inputs:intensity = 2
    float inputs:intensity.timeSamples = { 1: 1, 10: 4 }
    float inputs:exposure = 1
    color3f inputs:color = (1, 0.5, 0.25)
    bool inputs:normalize = 1
}
def DomeLight "Sky" {}
def DistantLight "Sun" {}
"#,
    );
    let at = |frame| lights(&path, frame);
    let key = &at(None)[0];
    assert_eq!(key.kind, "sphere");
    assert_eq!(key.intensity, 2.0, "the default without -f");
    assert_eq!((key.exposure, key.color), (1.0, [1.0, 0.5, 0.25]));
    assert!(key.normalize);
    assert_eq!(at(Some(10.0))[0].intensity, 4.0);
    assert_eq!(at(Some(1.0))[0].intensity, 1.0);
    let sky = &at(None)[1];
    assert_eq!(sky.kind, "dome");
    assert_eq!(
        (sky.intensity, sky.exposure),
        (1.0, 0.0),
        "schema fallbacks"
    );
    assert_eq!(sky.color, [1.0, 1.0, 1.0]);
    assert!(!sky.normalize);
    // `DistantLight` overrides `LightAPI`'s intensity fallback with 50000.
    let sun = &at(None)[2];
    assert_eq!(sun.kind, "distant");
    assert_eq!(
        (sun.intensity, sun.exposure),
        (50000.0, 0.0),
        "schema fallbacks"
    );
    assert!(matches!(
        Scene::list_usd_records(&path, ListKind::Light, Some(f64::NAN)),
        Err(crust_core::Error::InvalidFrame(_))
    ));
}

/// `bound` is the import's binding resolution: direct, inherited and
/// collection bindings reach their material; one bound only for the preview
/// purpose, and one bound by nothing, do not.
///
/// `strongerThanDescendants` is where `bound` and the authored intent part:
/// openusd 0.7 parses a `.usda`'s `bindMaterialAs` as a string, and
/// openusd-schemas' `compute_bound_material` only recognises the token, so
/// the import — and with it the render — resolves the mesh to its own
/// binding (`Overridden`), not the ancestor's (`Stronger`). `bound` reports
/// what the render does; this pins it, and flips with the openusd fix (see
/// the `usd-scene-import` design record's known gaps).
#[test]
fn material_records_follow_the_binding_resolution() {
    let quad = |name: &str, binding: &str| {
        format!(
            r#"
    def Mesh "{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        {binding}
    }}"#
        )
    };
    let look = |name: &str| {
        format!(
            r#"
    def Material "{name}"
    {{
        token outputs:surface.connect = </mtl/{name}/S.outputs:surface>
        def Shader "S"
        {{
            uniform token info:id = "UsdPreviewSurface"
            token outputs:surface
        }}
    }}"#
        )
    };
    let usda = format!(
        r#"#usda 1.0
def Scope "mtl"
{{{direct}{inherited}{collected}{overridden}{stronger}{preview}{nothing}
    def Material "Empty" {{}}
}}
def Xform "geo"
{{{m1}
    def Xform "Group" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        rel material:binding = </mtl/Inherited>{m2}
    }}
    def Xform "Coll" (prepend apiSchemas = ["MaterialBindingAPI", "CollectionAPI:set"])
    {{
        rel collection:set:includes = [</geo/Coll/InSet>]
        rel material:binding:collection:set = [</geo/Coll.collection:set>, </mtl/Collected>]{m3}
    }}
    def Xform "Strong" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        rel material:binding = </mtl/Stronger> (
            bindMaterialAs = "strongerThanDescendants"
        ){m4}
    }}{m5}
}}
"#,
        direct = look("Direct"),
        inherited = look("Inherited"),
        collected = look("Collected"),
        overridden = look("Overridden"),
        stronger = look("Stronger"),
        preview = look("Preview"),
        nothing = look("Nothing"),
        m1 = quad("Mesh", "rel material:binding = </mtl/Direct>"),
        m2 = quad("Child", ""),
        m3 = quad("InSet", ""),
        m4 = quad("Own", "rel material:binding = </mtl/Overridden>"),
        m5 = quad("Previewed", "rel material:binding:preview = </mtl/Preview>"),
    );
    let path = stage("bindings.usda", &usda);
    let bound: Vec<(String, bool)> = materials(&path)
        .into_iter()
        .map(|m| (m.path, m.bound))
        .collect();
    let want = [
        ("/mtl/Direct", true),
        ("/mtl/Inherited", true),
        ("/mtl/Collected", true),
        // Under the openusd gap above; `false` / `true` once it is fixed.
        ("/mtl/Overridden", true),
        ("/mtl/Stronger", false),
        ("/mtl/Preview", false),
        ("/mtl/Nothing", false),
        ("/mtl/Empty", false),
    ];
    let want: Vec<(String, bool)> = want.iter().map(|(p, b)| ((*p).to_owned(), *b)).collect();
    assert_eq!(bound, want);
    let surface: Vec<Option<String>> = materials(&path).into_iter().map(|m| m.surface).collect();
    assert_eq!(surface[0].as_deref(), Some("UsdPreviewSurface"));
    assert_eq!(surface[7], None, "a material without a shader");
}

/// A `PointInstancer` renders its `prototypes` targets wherever they are,
/// a `class` outside it included: what is bound inside one is bound.
#[test]
fn a_class_prototype_of_an_instancer_binds_its_material() {
    let path = stage(
        "instancer_class.usda",
        r#"#usda 1.0
def Scope "mtl"
{
    def Material "Leaf" {}
    def Material "Unused" {}
}
class Xform "Protos"
{
    def Mesh "LeafMesh" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        int[] faceVertexCounts = [3]
        int[] faceVertexIndices = [0, 1, 2]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        rel material:binding = </mtl/Leaf>
    }
}
def PointInstancer "Scatter"
{
    rel prototypes = [</Protos/LeafMesh>]
    int[] protoIndices = [0, 0]
    point3f[] positions = [(0, 0, 0), (2, 0, 0)]
}
"#,
    );
    let bound: Vec<(String, bool)> = materials(&path)
        .into_iter()
        .map(|m| (m.path, m.bound))
        .collect();
    assert_eq!(
        bound,
        [
            ("/mtl/Leaf".to_owned(), true),
            ("/mtl/Unused".to_owned(), false)
        ]
    );
}
