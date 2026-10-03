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
        uniform string sourceName = "C<RD>L"
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
