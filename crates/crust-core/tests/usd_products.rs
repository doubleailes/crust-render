//! `RenderSettings.products` → `RenderProduct` → `RenderVar`, resolved at
//! import into the scene's [`AovRequest`]: inheritance from the settings,
//! overrides, time-sampled names, the Houdini-authored keys, every refusal,
//! and agreement with `openusd_schemas::render::compute_render_spec`.

use crust_core::{
    Accumulation, AovRequest, AovSource, NoAssets, Precision, Scene, UsdImportOptions,
};
use std::path::PathBuf;

/// A stage with one camera under `/World` and `render` (prims under
/// `/Render`, which must hold `settings`).
fn stage_text(render: &str) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Camera "cam"
    {{
        double3 xformOp:translate = (0, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    def Camera "other"
    {{
    }}
    def Sphere "ball"
    {{
        double radius = 1
    }}
}}

def Scope "Render"
{{
{render}
}}
"#
    )
}

fn write_stage(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_usd_products_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).expect("write stage");
    path
}

fn load_at(name: &str, render: &str, frame: Option<f64>) -> Scene {
    let path = write_stage(name, &stage_text(render));
    let options = UsdImportOptions {
        frame,
        ..UsdImportOptions::default()
    };
    Scene::from_usd_with_options(&path, &NoAssets, &options)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn load(name: &str, render: &str) -> Scene {
    load_at(name, render, None)
}

fn sources(request: &AovRequest, product: usize) -> Vec<(String, AovSource)> {
    request.products[product]
        .vars
        .iter()
        .map(|v| (v.name.clone(), v.source))
        .collect()
}

const BEAUTY_VAR: &str = r#"
    def RenderVar "beauty"
    {
        uniform token dataType = "color4f"
        uniform string sourceName = "color"
    }
"#;

#[test]
fn no_products_is_an_empty_request() {
    let scene = load(
        "no_products",
        r#"
    def RenderSettings "settings"
    {
        uniform int2 resolution = (64, 36)
    }
"#,
    );
    assert!(scene.aovs.products.is_empty());
    assert!(!scene.aovs.needs_film());
    assert_eq!(scene.settings.get_dimensions(), (64, 36));
}

#[test]
fn a_product_inherits_the_settings_camera_and_resolution() {
    let scene = load(
        "inherits",
        &format!(
            r#"
    def RenderSettings "settings"
    {{
        rel camera = </World/cam>
        uniform int2 resolution = (64, 36)
        rel products = [</Render/beauty>]
    }}
    def RenderProduct "beauty"
    {{
        token productName = "renders/beauty.exr"
        rel orderedVars = [</Render/beauty_var>]
    }}
    {}
"#,
            BEAUTY_VAR.replace("\"beauty\"", "\"beauty_var\"")
        ),
    );
    assert_eq!(scene.settings.get_dimensions(), (64, 36));
    let p = &scene.aovs.products[0];
    assert_eq!(p.name, "renders/beauty.exr");
    assert_eq!(p.prim_path, "/Render/beauty");
    assert_eq!(
        sources(&scene.aovs, 0),
        [("beauty_var".into(), AovSource::Color)]
    );
    assert_eq!(p.vars[0].components, 4, "color4f adds alpha");
    assert!(scene.aovs.needs_film());
}

#[test]
fn the_first_product_overrides_the_resolution_and_a_mismatched_one_is_refused() {
    let scene = load(
        "overrides",
        &format!(
            r#"
    def RenderSettings "settings"
    {{
        rel camera = </World/cam>
        uniform int2 resolution = (64, 36)
        rel products = [</Render/a>, </Render/b>, </Render/c>]
    }}
    def RenderProduct "a"
    {{
        uniform int2 resolution = (32, 18)
        token productName = "a.exr"
        rel orderedVars = [</Render/beauty>]
    }}
    def RenderProduct "b"
    {{
        token productName = "b.exr"
        rel orderedVars = [</Render/beauty>]
    }}
    def RenderProduct "c"
    {{
        uniform int2 resolution = (32, 18)
        rel camera = </World/other>
        token productName = "c.exr"
        rel orderedVars = [</Render/beauty>]
    }}
    {BEAUTY_VAR}
"#
        ),
    );
    assert_eq!(scene.settings.get_dimensions(), (32, 18));
    // `b` inherits 64x36 and `c` another camera: both differ from the render.
    let names: Vec<_> = scene
        .aovs
        .products
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["a.exr"]);
}

#[test]
fn a_time_sampled_product_name_follows_the_frame() {
    let render = format!(
        r#"
    def RenderSettings "settings"
    {{
        rel products = [</Render/shot>]
    }}
    def RenderProduct "shot"
    {{
        token productName.timeSamples = {{
            1: "shot.0001.exr",
            2: "shot.0002.exr",
        }}
        rel orderedVars = [</Render/beauty>]
    }}
    {BEAUTY_VAR}
"#
    );
    let at = |f| {
        load_at(&format!("time_sampled_{f}"), &render, Some(f))
            .aovs
            .products[0]
            .name
            .clone()
    };
    assert_eq!(at(1.0), "shot.0001.exr");
    assert_eq!(at(2.0), "shot.0002.exr");
}

/// The shape of a Solaris export (arnold-usd `testsuite/test_0228`): a var
/// whose channel name comes from `driver:parameters:aov:name`, whose format
/// is authored twice, and whose accumulation is Hydra's `multiSampled`.
#[test]
fn a_houdini_authored_var_resolves_its_driver_parameters() {
    let scene = load(
        "houdini_var",
        r#"
    def RenderSettings "settings"
    {
        rel products = [</Render/Products/aovs>]
    }
    def Scope "Products"
    {
        def RenderProduct "aovs"
        {
            token productName = "aovs.exr"
            string driver:parameters:artist = "someone"
            string driver:parameters:OpenEXR:comment = "a comment"
            rel orderedVars = [</Render/Products/Vars/Z>, </Render/Products/Vars/N>, </Render/Products/Vars/Z>]
        }
        def Scope "Vars"
        {
            def RenderVar "Z"
            {
                uniform token dataType = "float"
                uniform string sourceName = "Z"
                uniform token sourceType = "raw"
                string driver:parameters:aov:name = "depth"
                token driver:parameters:aov:format = "float"
                bool driver:parameters:aov:multiSampled = 0
                float driver:parameters:aov:clearValue = 0
            }
            def RenderVar "N"
            {
                uniform token dataType = "normal3f"
                uniform string sourceName = "N"
                uniform string sourceType = "raw"
                token driver:parameters:aov:name = "N"
                token driver:parameters:aov:format = "half3"
            }
        }
    }
"#,
    );
    let product = &scene.aovs.products[0];
    // `Z` is targeted twice and used once.
    assert_eq!(
        sources(&scene.aovs, 0),
        [
            ("depth".into(), AovSource::Depth),
            ("N".into(), AovSource::Normal)
        ]
    );
    let z = &product.vars[0];
    assert_eq!(z.accumulation, Accumulation::Closest);
    assert_eq!(z.clear, 0.0, "the authored clear value wins over +inf");
    assert_eq!(z.precision, Precision::Float);
    let n = &product.vars[1];
    assert_eq!(
        n.precision,
        Precision::Half,
        "aov:format overrides dataType"
    );
    assert_eq!(n.accumulation, Accumulation::Filtered);
    assert_eq!(
        product.attributes,
        [
            ("artist".to_string(), "someone".to_string()),
            ("comment".to_string(), "a comment".to_string())
        ]
    );
}

#[test]
fn unsupported_products_and_vars_are_refused() {
    let scene = load(
        "refusals",
        r#"
    def RenderSettings "settings"
    {
        rel products = [</Render/deep>, </Render/flat>, </Render/notaproduct>]
    }
    def RenderProduct "deep"
    {
        uniform token productType = "deepRaster"
        token productName = "deep.exr"
        rel orderedVars = [</Render/depth>]
    }
    def RenderProduct "flat"
    {
        token productName = "flat.exr"
        rel orderedVars = [</Render/intrinsic>, </Render/unknown>, </Render/lpe>,
                           </Render/primvar>, </Render/int_color>, </Render/float_normal>,
                           </Render/later>, </Render/depth>, </Render/notavar>]
    }
    def Scope "notaproduct"
    {
    }
    def RenderVar "intrinsic"
    {
        uniform token dataType = "float"
        uniform string sourceName = "cameraDepth"
        uniform token sourceType = "intrinsic"
    }
    def RenderVar "unknown"
    {
        uniform string sourceName = "diffuse_direct"
    }
    def RenderVar "lpe"
    {
        uniform string sourceName = "C<RD>?L"
        uniform token sourceType = "lpe"
    }
    def RenderVar "primvar"
    {
        uniform string sourceName = "displayColor"
        uniform token sourceType = "primvar"
    }
    def RenderVar "int_color"
    {
        uniform token dataType = "int"
        uniform string sourceName = "color"
    }
    def RenderVar "float_normal"
    {
        uniform token dataType = "float"
        uniform string sourceName = "normal"
    }
    def RenderVar "later"
    {
        uniform token dataType = "int"
        uniform string sourceName = "primId"
    }
    def RenderVar "depth"
    {
        uniform token dataType = "float"
        uniform string sourceName = "depth"
    }
    def Scope "notavar"
    {
    }
"#,
    );
    let names: Vec<_> = scene
        .aovs
        .products
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["flat.exr"]);
    assert_eq!(
        sources(&scene.aovs, 0),
        [("depth".into(), AovSource::Depth)]
    );
    let depth = &scene.aovs.products[0].vars[0];
    assert_eq!(depth.clear, f32::INFINITY);
    assert_eq!(depth.accumulation, Accumulation::Closest);
}

#[test]
fn an_empty_source_name_falls_back_to_the_channel_name() {
    let scene = load(
        "empty_source_name",
        r#"
    def RenderSettings "settings"
    {
        rel products = [</Render/p>]
    }
    def RenderProduct "p"
    {
        token productName = "p.exr"
        rel orderedVars = [</Render/Pworld>]
    }
    def RenderVar "Pworld"
    {
        uniform token dataType = "point3f"
    }
"#,
    );
    assert_eq!(sources(&scene.aovs, 0), [("Pworld".into(), AovSource::P)]);
}

#[test]
fn renderer_filter_attributes_choose_the_accumulation() {
    let scene = load(
        "filters",
        r#"
    def RenderSettings "settings"
    {
        rel products = [</Render/p>]
    }
    def RenderProduct "p"
    {
        token productName = "p.exr"
        rel orderedVars = [</Render/arnold>, </Render/karma>, </Render/rman>, </Render/multi>]
    }
    def RenderVar "arnold"
    {
        uniform token dataType = "normal3f"
        uniform string sourceName = "N"
        token arnold:filter = "closest_filter"
    }
    def RenderVar "karma"
    {
        uniform token dataType = "float"
        uniform string sourceName = "alpha"
        string driver:parameters:aov:filter = "[\"closest\",{}]"
    }
    def RenderVar "rman"
    {
        uniform token dataType = "texCoord2f"
        uniform string sourceName = "st"
        token ri:accumulationRule = "zmin"
    }
    def RenderVar "multi"
    {
        uniform token dataType = "float"
        uniform string sourceName = "depth"
        bool driver:parameters:aov:multiSampled = 1
    }
"#,
    );
    let modes: Vec<_> = scene.aovs.products[0]
        .vars
        .iter()
        .map(|v| v.accumulation)
        .collect();
    assert_eq!(
        modes,
        [
            Accumulation::Closest,
            Accumulation::Closest,
            Accumulation::Closest,
            Accumulation::Filtered
        ]
    );
}

/// The resolver and `compute_render_spec` (a port of `UsdRenderComputeSpec`)
/// agree on everything both compute, on a stage with nothing time-sampled.
#[test]
fn the_resolver_agrees_with_compute_render_spec() {
    let text = stage_text(&format!(
        r#"
    def RenderSettings "settings"
    {{
        rel camera = </World/cam>
        uniform int2 resolution = (64, 36)
        rel products = [</Render/a>, </Render/b>]
    }}
    def RenderProduct "a"
    {{
        uniform int2 resolution = (32, 18)
        token productName = "a.exr"
        rel orderedVars = [</Render/beauty>, </Render/depth>, </Render/beauty>]
    }}
    def RenderProduct "b"
    {{
        uniform int2 resolution = (32, 18)
        token productName = "b.exr"
        rel orderedVars = [</Render/depth>, </Render/normal>]
    }}
    {BEAUTY_VAR}
    def RenderVar "depth"
    {{
        uniform token dataType = "float"
        uniform string sourceName = "depth"
    }}
    def RenderVar "normal"
    {{
        uniform token dataType = "normal3f"
        uniform string sourceName = "normal"
    }}
"#
    ));
    let path = write_stage("agrees_with_spec", &text);
    let scene = Scene::from_usd(&path).expect("loads");

    let stage = openusd::usd::Stage::open(path.to_str().unwrap()).expect("opens");
    let spec = openusd_schemas::render::compute_render_spec(
        &stage,
        &openusd::sdf::path("/Render/settings").unwrap(),
        &[],
    )
    .expect("computes")
    .expect("a RenderSettings prim");

    assert_eq!(spec.products.len(), scene.aovs.products.len());
    for (theirs, ours) in spec.products.iter().zip(&scene.aovs.products) {
        assert_eq!(theirs.render_product_path, ours.prim_path);
        assert_eq!(theirs.name, ours.name);
        let [w, h] = theirs.resolution;
        assert_eq!((w as usize, h as usize), scene.settings.get_dimensions());
        assert_eq!(theirs.camera_path.as_deref(), Some("/World/cam"));
        let their_vars: Vec<_> = theirs
            .render_var_indices
            .iter()
            .map(|&i| spec.render_vars[i].render_var_path.as_str())
            .collect();
        let our_vars: Vec<_> = ours.vars.iter().map(|v| v.prim_path.as_str()).collect();
        assert_eq!(their_vars, our_vars, "{}", ours.prim_path);
    }
}

/// The products are resolved on the index stage, where payloads are
/// unloaded; a shot camera inside a payload is not composed there, and must
/// not make every product disappear.
#[test]
fn a_shot_camera_under_a_payload_keeps_the_products() {
    write_stage(
        "payload_camera_payload",
        r#"#usda 1.0
(
    defaultPrim = "Shot"
)

def Xform "Shot"
{
    def Camera "cam"
    {
        double3 xformOp:translate = (0, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Sphere "ball"
    {
    }
}
"#,
    );
    let path = write_stage(
        "payload_camera",
        &format!(
            r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World" (
    prepend payload = @./payload_camera_payload.usda@
)
{{
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/cam>
        uniform int2 resolution = (48, 32)
        rel products = [</Render/p>]
    }}
    def RenderProduct "p"
    {{
        token productName = "p.exr"
        rel orderedVars = [</Render/beauty>]
    }}
    {BEAUTY_VAR}
}}
"#
        ),
    );
    let scene = Scene::from_usd(&path).expect("loads");
    assert_eq!(scene.aovs.products.len(), 1);
    assert_eq!(scene.aovs.products[0].name, "p.exr");
    assert_eq!(scene.settings.get_dimensions(), (48, 32));
}

#[test]
fn light_path_expressions_and_albedo_are_accepted() {
    let scene = load(
        "lpe_vars",
        r#"
    def RenderSettings "settings"
    {
        rel products = [</Render/p>]
    }
    def RenderProduct "p"
    {
        token productName = "p.exr"
        rel orderedVars = [</Render/diffuse>, </Render/prefixed>, </Render/rgba>,
                           </Render/scalar>, </Render/albedo>]
    }
    def RenderVar "diffuse"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "C<RD>[LO]"
        uniform token sourceType = "lpe"
    }
    def RenderVar "prefixed"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "lpe:C.*<L.'key'>"
        uniform string sourceType = "lpe"
    }
    def RenderVar "rgba"
    {
        uniform token dataType = "color4f"
        uniform string sourceName = "C<RG>.*L"
        uniform token sourceType = "lpe"
    }
    def RenderVar "scalar"
    {
        uniform token dataType = "float"
        uniform string sourceName = "C<RD>L"
        uniform token sourceType = "lpe"
    }
    def RenderVar "albedo"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "albedo"
    }
"#,
    );
    let vars = &scene.aovs.products[0].vars;
    let got: Vec<_> = vars
        .iter()
        .map(|v| {
            (
                v.name.as_str(),
                v.source,
                v.expression.as_deref(),
                v.components,
            )
        })
        .collect();
    // A light path expression is colour: a float var for one is refused.
    assert_eq!(
        got,
        [
            ("diffuse", AovSource::Lpe, Some("C<RD>[LO]"), 3),
            ("prefixed", AovSource::Lpe, Some("C.*<L.'key'>"), 3),
            ("rgba", AovSource::Lpe, Some("C<RG>.*L"), 4),
            ("albedo", AovSource::Albedo, None, 3),
        ]
    );
    assert!(vars[2].with_alpha());
}

/// An expression the renderer could not compile is refused at import, with
/// its var — never a panic once the scene has loaded.
#[test]
fn an_expression_too_complex_to_compile_is_refused_at_import() {
    let scene = load(
        "lpe_too_complex",
        r#"
    def RenderSettings "settings"
    {
        rel products = [</Render/p>]
    }
    def RenderProduct "p"
    {
        token productName = "p.exr"
        rel orderedVars = [</Render/ok>, </Render/blowup>, </Render/nested>, </Render/also_ok>]
    }
    def RenderVar "ok"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "C<RD>[LO]"
        uniform token sourceType = "lpe"
    }
    def RenderVar "blowup"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "C.*<RD>.{16}[LO]"
        uniform token sourceType = "lpe"
    }
    def RenderVar "nested"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "C(((.{32}){32}){32}){32}L"
        uniform token sourceType = "lpe"
    }
    def RenderVar "also_ok"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "C.*[LO]"
        uniform token sourceType = "lpe"
    }
"#,
    );
    let names: Vec<_> = scene.aovs.products[0]
        .vars
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    assert_eq!(names, ["ok", "also_ok"]);
}

#[test]
fn raw_light_sources_and_the_diffuse_filter_resolve() {
    let scene = load(
        "raw_light",
        r#"
    def RenderSettings "settings"
    {
        rel products = [</Render/p>]
    }
    def RenderProduct "p"
    {
        token productName = "p.exr"
        rel orderedVars = [</Render/rawLight>, </Render/RawGI>, </Render/RawTotalLighting>,
                           </Render/filter>, </Render/vray_filter>, </Render/albedo>,
                           </Render/key_raw>, </Render/not_diffuse>, </Render/bare_label>,
                           </Render/raw_on_depth>]
    }
    def RenderVar "rawLight"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "rawLight"
    }
    def RenderVar "RawGI"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "RawGI"
    }
    def RenderVar "RawTotalLighting"
    {
        uniform token dataType = "color4f"
        uniform string sourceName = "RawTotalLighting"
    }
    def RenderVar "filter"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "diffuse_albedo"
    }
    def RenderVar "vray_filter"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "DiffuseFilter"
    }
    def RenderVar "albedo"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "albedo"
    }
    def RenderVar "key_raw"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "C<RD>.*<L.'key'>"
        uniform token sourceType = "lpe"
        bool crust:aov:raw = 1
    }
    def RenderVar "not_diffuse"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "C.*[LO]"
        uniform token sourceType = "lpe"
        bool crust:aov:raw = 1
    }
    def RenderVar "bare_label"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "C'diffuse'.*L"
        uniform token sourceType = "lpe"
        bool crust:aov:raw = 1
    }
    def RenderVar "raw_on_depth"
    {
        uniform token dataType = "float"
        uniform string sourceName = "depth"
        bool crust:aov:raw = 1
    }
"#,
    );
    let got: Vec<_> = scene.aovs.products[0]
        .vars
        .iter()
        .map(|v| (v.name.as_str(), v.source, v.expression.as_deref(), v.raw))
        .collect();
    assert_eq!(
        got,
        [
            ("rawLight", AovSource::Lpe, Some("C<RD>[LO]"), true),
            ("RawGI", AovSource::Lpe, Some("C<RD>.+[LO]"), true),
            (
                "RawTotalLighting",
                AovSource::Lpe,
                Some("C<RD>.*[LO]"),
                true
            ),
            ("filter", AovSource::DiffuseFilter, None, false),
            ("vray_filter", AovSource::DiffuseFilter, None, false),
            ("albedo", AovSource::Albedo, None, false),
            ("key_raw", AovSource::Lpe, Some("C<RD>.*<L.'key'>"), true),
            // Refused: `not_diffuse` and `bare_label` do not start with a
            // diffuse reflection. `crust:aov:raw` on `depth` is ignored.
            ("raw_on_depth", AovSource::Depth, None, false),
        ]
    );
    assert!(scene.aovs.products[0].vars[2].with_alpha());
}

// ---------------------------------------------------------------------------
// disableMotionBlur / instantaneousShutter
// ---------------------------------------------------------------------------

/// A `tracing` subscriber that keeps every WARN message, so a test can say
/// which warnings a stage produced and which it did not.
struct Warnings(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for Warnings {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            let mut message = Message(String::new());
            event.record(&mut message);
            self.0.lock().unwrap().push(message.0);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Loads `render` while recording the import's warnings.
fn load_warnings(name: &str, render: &str) -> (Scene, Vec<String>) {
    let warnings = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let scene =
        tracing::subscriber::with_default(Warnings(warnings.clone()), || load(name, render));
    let warnings = warnings.lock().unwrap().clone();
    (scene, warnings)
}

const FORWARD_PRODUCT: &str = r#"
    def RenderProduct "p"
    {
        token productName = "p.exr"
        rel orderedVars = [</Render/beauty>]
    }
    def RenderVar "beauty"
    {
        uniform token dataType = "color4f"
        uniform string sourceName = "color"
    }
"#;

#[test]
fn motion_blur_is_on_unless_the_settings_disable_it() {
    let scene = load(
        "blur_default",
        r#"
    def RenderSettings "settings"
    {
        uniform int2 resolution = (64, 36)
    }
"#,
    );
    assert!(scene.settings.motion_blur());

    // Settings only, no products: the settings prim decides.
    let scene = load(
        "blur_settings_only",
        r#"
    def RenderSettings "settings"
    {
        uniform bool disableMotionBlur = 1
    }
"#,
    );
    assert!(!scene.settings.motion_blur());

    // The deprecated synonym means the same.
    let scene = load(
        "blur_synonym",
        r#"
    def RenderSettings "settings"
    {
        uniform bool instantaneousShutter = 1
    }
"#,
    );
    assert!(!scene.settings.motion_blur());

    // An authored `false` is the default, not a disable.
    let scene = load(
        "blur_false",
        r#"
    def RenderSettings "settings"
    {
        uniform bool disableMotionBlur = 0
        uniform bool instantaneousShutter = 0
    }
"#,
    );
    assert!(scene.settings.motion_blur());
}

#[test]
fn the_first_product_overrides_the_settings_motion_blur() {
    // The settings disable it, the product re-enables it.
    let scene = load(
        "blur_product_on",
        &format!(
            r#"
    def RenderSettings "settings"
    {{
        uniform bool disableMotionBlur = 1
        rel products = [</Render/p>]
    }}
{FORWARD_PRODUCT}"#
        )
        .replace(
            "token productName",
            "uniform bool disableMotionBlur = 0\n        token productName",
        ),
    );
    assert!(scene.settings.motion_blur());

    // The product disables it, the settings say nothing.
    let scene = load(
        "blur_product_off",
        &format!(
            r#"
    def RenderSettings "settings"
    {{
        rel products = [</Render/p>]
    }}
{FORWARD_PRODUCT}"#
        )
        .replace(
            "token productName",
            "uniform bool disableMotionBlur = 1\n        token productName",
        ),
    );
    assert!(!scene.settings.motion_blur());

    // Each flag inherits on its own: a product authoring only
    // `disableMotionBlur = false` still inherits the settings' synonym.
    let scene = load(
        "blur_product_inherits_synonym",
        &format!(
            r#"
    def RenderSettings "settings"
    {{
        uniform bool instantaneousShutter = 1
        rel products = [</Render/p>]
    }}
{FORWARD_PRODUCT}"#
        )
        .replace(
            "token productName",
            "uniform bool disableMotionBlur = 0\n        token productName",
        ),
    );
    assert!(!scene.settings.motion_blur());
}

#[test]
fn the_motion_blur_flags_are_no_longer_warned_about() {
    let (scene, warnings) = load_warnings(
        "blur_warnings",
        &format!(
            r#"
    def RenderSettings "settings"
    {{
        uniform bool disableMotionBlur = 1
        uniform bool instantaneousShutter = 1
        uniform bool disableDepthOfField = 1
        rel products = [</Render/p>]
    }}
{FORWARD_PRODUCT}"#
        ),
    );
    assert!(!scene.settings.motion_blur());
    let unhonoured: Vec<&String> = warnings
        .iter()
        .filter(|w| w.contains("not honoured"))
        .collect();
    assert_eq!(unhonoured.len(), 1, "{warnings:?}");
    assert!(
        unhonoured[0].contains("disableDepthOfField = true"),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .all(|w| !w.contains("disableMotionBlur") && !w.contains("instantaneousShutter")),
        "{warnings:?}"
    );
}

// ---------------------------------------------------------------------------
// motionvector
// ---------------------------------------------------------------------------

/// `motionvector` takes two floating-point components, through the type
/// spellings the resolver already knows; everything else is refused and gets
/// no channel. `int2` / `uint2` because unsigned samples cannot hold leftward
/// or downward motion, `vector2f` because `vector` is 3-component only in
/// USD, `float` and `color3f` because the count is wrong.
#[test]
fn motion_vector_vars_take_two_float_components_only() {
    let mut vars = String::new();
    for (name, ty) in [
        ("f2", "float2"),
        ("h2", "half2"),
        ("st2", "texCoord2f"),
        ("f1", "float"),
        ("c3", "color3f"),
        ("i2", "int2"),
        ("u2", "uint2"),
        ("v2", "vector2f"),
    ] {
        vars.push_str(&format!(
            r#"
    def RenderVar "{name}"
    {{
        uniform token dataType = "{ty}"
        uniform string sourceName = "motionvector"
    }}
"#
        ));
    }
    let scene = load(
        "motionvector_types",
        &format!(
            r#"
    def RenderSettings "settings"
    {{
        rel products = [</Render/p>]
    }}
    def RenderProduct "p"
    {{
        token productName = "p.exr"
        rel orderedVars = [</Render/f2>, </Render/h2>, </Render/st2>, </Render/f1>,
                           </Render/c3>, </Render/i2>, </Render/u2>, </Render/v2>]
    }}
{vars}"#
        ),
    );
    let got: Vec<_> = scene.aovs.products[0]
        .vars
        .iter()
        .map(|v| {
            (
                v.name.as_str(),
                v.source,
                v.components,
                v.precision,
                v.accumulation,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "f2",
                AovSource::MotionVector,
                2,
                Precision::Float,
                Accumulation::Closest
            ),
            (
                "h2",
                AovSource::MotionVector,
                2,
                Precision::Half,
                Accumulation::Closest
            ),
            (
                "st2",
                AovSource::MotionVector,
                2,
                Precision::Float,
                Accumulation::Closest
            ),
        ]
    );
    assert_eq!(scene.aovs.products[0].vars[0].clear, 0.0);
}

/// The other renderers' names for a motion pass are not aliases: each is
/// refused as unknown, with no channel.
#[test]
fn other_renderers_motion_names_are_refused() {
    let scene = load(
        "motionvector_aliases",
        r#"
    def RenderSettings "settings"
    {
        rel products = [</Render/p>]
    }
    def RenderProduct "p"
    {
        token productName = "p.exr"
        rel orderedVars = [</Render/velocity>, </Render/Vector>, </Render/motionFore>, </Render/ok>]
    }
    def RenderVar "velocity"
    {
        uniform token dataType = "float2"
        uniform string sourceName = "velocity"
    }
    def RenderVar "Vector"
    {
        uniform token dataType = "float2"
    }
    def RenderVar "motionFore"
    {
        uniform token dataType = "float2"
        uniform string sourceName = "motionFore"
    }
    def RenderVar "ok"
    {
        uniform token dataType = "half2"
        uniform string sourceName = "motionvector"
    }
"#,
    );
    assert_eq!(
        sources(&scene.aovs, 0),
        [("ok".to_owned(), AovSource::MotionVector)]
    );
}

/// Products are refused only for a different camera or resolution. One
/// that differs in its shutter flags alone is still written, with the first
/// product's blur, and warned about; two spellings of the same setting are
/// not a difference at all.
#[test]
fn a_product_differing_only_in_motion_blur_is_kept_and_warned_about() {
    let products = |first: &str, second: &str| {
        format!(
            r#"
    def RenderSettings "settings"
    {{
        rel products = [</Render/a>, </Render/b>]
    }}
    def RenderProduct "a"
    {{
        {first}
        token productName = "a.exr"
        rel orderedVars = [</Render/beauty>]
    }}
    def RenderProduct "b"
    {{
        {second}
        token productName = "b.exr"
        rel orderedVars = [</Render/beauty>]
    }}
    def RenderVar "beauty"
    {{
        uniform token dataType = "color4f"
        uniform string sourceName = "color"
    }}
"#
        )
    };
    // Different effective settings: both written, the render sharp (the
    // first's), one warning naming the second and motion blur.
    let (scene, warnings) = load_warnings(
        "blur_products_differ",
        &products(
            "uniform bool disableMotionBlur = 1",
            "uniform bool disableMotionBlur = 0",
        ),
    );
    let names: Vec<&str> = scene
        .aovs
        .products
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(names, ["a.exr", "b.exr"]);
    assert!(!scene.settings.motion_blur());
    let blur: Vec<&String> = warnings
        .iter()
        .filter(|w| w.contains("motion blur"))
        .collect();
    assert_eq!(blur.len(), 1, "{warnings:?}");
    assert!(blur[0].contains("/Render/b") && blur[0].contains("asks for motion blur on"));
    assert!(
        warnings.iter().all(|w| !w.contains("no file is written")),
        "{warnings:?}"
    );

    // The same setting under its two names: both written, nothing to say.
    let (scene, warnings) = load_warnings(
        "blur_products_synonyms",
        &products(
            "uniform bool disableMotionBlur = 1",
            "uniform bool instantaneousShutter = 1",
        ),
    );
    assert_eq!(scene.aovs.products.len(), 2);
    assert!(!scene.settings.motion_blur());
    assert!(
        warnings
            .iter()
            .all(|w| !w.contains("motion blur") && !w.contains("no file is written")),
        "{warnings:?}"
    );
}
