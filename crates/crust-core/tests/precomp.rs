//! The precomp (`openspec/specs/precomp-filters/`): sample and display
//! filters authored on the render settings, read at import and run by the
//! renderer. Every stage here is USD, as a precomp only exists in one.
//!
//! What is pinned: the import's refusals, one warning code each; a sample
//! filter's output antialiased with the beauty's own weights, reading every
//! sample's own value; exposure and holdouts; guided passes; the beauty
//! untouched; bit-identity across tiles, scanlines and thread counts; and
//! the displayed output a snapshot shows.

use crust_core::{
    AovFilm, AovRequest, AovVar, Buffer, NoAssets, RenderControl, Renderer, Scene,
    UsdImportOptions, WarningCode,
};
use std::path::{Path, PathBuf};

/// A camera at the origin looking down −Z; a wall at z = −6 and, in front
/// of it at z = −3, a card covering the left of the frame up to a little
/// right of the centre, which authors `primvars:matte = 1`. A dome lights
/// both. `settings` and `render` go into the settings prim and `/Render`.
fn stage(name: &str, settings: &str, render: &str) -> PathBuf {
    let text = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
    def Camera "cam"
    {{
        float focalLength = 24
        float horizontalAperture = 24
        float verticalAperture = 18
    }}
    def Mesh "Wall"
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-20, -20, -6), (20, -20, -6), (20, 20, -6), (-20, 20, -6)]
    }}
    def Mesh "Card"
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-9, -9, -3), (0.137, -9, -3), (0.137, 9, -3), (-9, 9, -3)]
        float primvars:matte = 1 (
            interpolation = "constant"
        )
    }}
    def DomeLight "sky"
    {{
        color3f inputs:color = (1, 1, 1)
    }}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/cam>
        int2 resolution = (32, 24)
        rel products = [</Render/p>]
        int crust:samplesPerPixel = 16
        int crust:maxDepth = 2
{settings}
    }}
    def RenderProduct "p"
    {{
        token productName = "p.exr"
        rel orderedVars = [{vars}]
    }}
    def RenderVar "C"
    {{
        uniform token dataType = "color3f"
        uniform string sourceName = "color"
    }}
    def RenderVar "matte"
    {{
        uniform token dataType = "float"
        uniform string sourceName = "matte"
        uniform token sourceType = "primvar"
    }}
    def RenderVar "depth"
    {{
        uniform token dataType = "float"
        uniform string sourceName = "depth"
    }}
    def RenderVar "depth_filtered"
    {{
        uniform token dataType = "float"
        uniform string sourceName = "depth"
        uniform bool driver:parameters:aov:multiSampled = 1
    }}
    def RenderVar "variance"
    {{
        uniform token dataType = "float"
        uniform string sourceName = "variance"
    }}
{render}
}}
"#,
        vars = VARS
            .iter()
            .map(|v| format!("</Render/{v}>"))
            .chain(extra_vars(render))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let dir = std::env::temp_dir().join("crust_precomp_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.usda"));
    std::fs::write(&path, text).unwrap();
    path
}

/// The vars every stage's product holds.
const VARS: &[&str] = &["C", "matte", "depth", "depth_filtered", "variance"];

/// Every `RenderVar` `render` defines: the product writes them too.
fn extra_vars(render: &str) -> Vec<String> {
    render
        .lines()
        .filter_map(|l| l.trim().strip_prefix("def RenderVar \""))
        .filter_map(|l| l.split('"').next())
        .map(|n| format!("</Render/{n}>"))
        .collect()
}

/// A sample filter `look`: red on the matte, blue off it. Its literal
/// colours make the output an exact function of the matte: red is
/// `Σ wᵢ·[mᵢ > ½] / Σ wᵢ`, the matte var's own estimator.
const SELECT: &str = r#"
    def NodeGraph "look"
    {
        float inputs:matte = 0
        color3f outputs:out.connect = </Render/look/pick.outputs:out>
        def Shader "pick"
        {
            uniform token info:id = "ND_ifgreater_color3"
            float inputs:value1.connect = </Render/look.inputs:matte>
            float inputs:value2 = 0.5
            color3f inputs:in1 = (1, 0, 0)
            color3f inputs:in2 = (0, 0, 1)
            color3f outputs:out
        }
    }
    def RenderVar "look_out"
    {
        uniform token dataType = "color3f"
        rel crust:filter = </Render/look>
    }
"#;

fn load(path: &Path) -> Scene {
    Scene::from_usd_with_options(path, &NoAssets, &UsdImportOptions::default()).expect("loads")
}

fn codes(scene: &Scene) -> Vec<WarningCode> {
    scene.warnings.iter().map(|w| w.code).collect()
}

fn renderer(scene: Scene) -> (Renderer, AovRequest) {
    let request = scene.aovs.clone();
    let (camera, world, lights, settings) =
        (scene.camera, scene.world, scene.lights, scene.settings);
    (Renderer::new(camera, world, lights, settings), request)
}

fn var<'a>(request: &'a AovRequest, name: &str) -> &'a AovVar {
    request
        .products
        .iter()
        .flat_map(|p| &p.vars)
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("no var {name}"))
}

fn channels(film: &AovFilm, beauty: &Buffer, request: &AovRequest, name: &str) -> Vec<Vec<f32>> {
    film.var_channels(beauty, var(request, name))
}

fn bits(planes: &[Vec<f32>]) -> Vec<Vec<u32>> {
    planes
        .iter()
        .map(|p| p.iter().map(|x| x.to_bits()).collect())
        .collect()
}

fn beauty_bits(b: &Buffer) -> Vec<[u32; 3]> {
    let (w, h) = b.size();
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (r, g, b) = b.get_rgb(x, y);
            [r.to_bits(), g.to_bits(), b.to_bits()]
        })
        .collect()
}

fn render(r: &Renderer, request: &AovRequest, tiled: bool) -> (Buffer, AovFilm) {
    let (beauty, film, _) = r.render_with_aovs(tiled, &|_, _| {}, request);
    (beauty, film)
}

#[test]
fn the_sample_stage_imports_its_precomp_without_a_warning() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/precomp.usda");
    let scene = load(&path);
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    let pc = scene.aovs.precomp.as_deref().expect("a precomp");
    assert_eq!(pc.sample_filter_paths(), ["/Render/Precomp/look"]);
    assert_eq!(
        pc.display_filter_paths(),
        [
            "/Render/Precomp/glow",
            "/Render/Precomp/bloom",
            "/Render/Precomp/speed"
        ]
    );
    assert_eq!(pc.display_name(), "look_final");
    // What `crust check` reports of it.
    let info = crust_core::check::PrecompInfo::from(pc);
    assert_eq!(info.display, "look_final");
    assert_eq!(info.filters.len(), 4);
    assert!(
        info.filters[0].contains("key = var /Render/Products/Vars/key"),
        "{:?}",
        info.filters
    );
    assert!(
        info.filters[3].contains("crust:vectorBlur"),
        "{:?}",
        info.filters
    );
}

#[test]
fn a_stage_without_a_precomp_has_none() {
    let scene = load(&stage("none", "", ""));
    assert!(scene.aovs.precomp.is_none());
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
}

/// The antialiased-edge scenario: pixels straddling the card's edge hold a
/// weighted mean of red and blue, never one of them alone, with exactly the
/// matte var's weights.
#[test]
fn a_sample_filter_is_antialiased_with_the_beauty_weights() {
    for guiding in [false, true] {
        let settings = format!(
            "        rel crust:sampleFilters = </Render/look>\n        bool crust:pathGuiding = \
             {}\n        int crust:guidingTrainIterations = 1",
            u8::from(guiding)
        );
        let scene = load(&stage(&format!("edge_{guiding}"), &settings, SELECT));
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        let (r, request) = renderer(scene);
        let (beauty, film) = render(&r, &request, true);
        let look = channels(&film, &beauty, &request, "look_out");
        let matte = channels(&film, &beauty, &request, "matte");
        assert_eq!(bits(&look[..1]), bits(&matte), "guiding {guiding}");
        let edge = look[0].iter().filter(|x| **x > 0.0 && **x < 1.0).count();
        assert!(edge > 0, "no pixel straddles the edge");
        for (red, blue) in look[0].iter().zip(&look[2]) {
            assert!((red + blue - 1.0).abs() < 1e-5, "{red} + {blue}");
        }
    }
}

/// A `closest` var feeds the filter each sample's own value: the filter's
/// output is the filtered depth, not the closest one.
#[test]
fn a_closest_var_is_read_per_sample() {
    let render_prims = r#"
    def NodeGraph "z"
    {
        float inputs:depth
        float outputs:out.connect = </Render/z/same.outputs:out>
        def Shader "same"
        {
            uniform token info:id = "ND_multiply_float"
            float inputs:in1.connect = </Render/z.inputs:depth>
            float inputs:in2 = 1
            float outputs:out
        }
    }
    def RenderVar "z_out"
    {
        uniform token dataType = "float"
        bool crust:filter:radiance = 0
        rel crust:filter = </Render/z>
    }
"#;
    let scene = load(&stage(
        "closest",
        "        rel crust:sampleFilters = </Render/z>",
        render_prims,
    ));
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    let (r, request) = renderer(scene);
    let (beauty, film) = render(&r, &request, true);
    let z = channels(&film, &beauty, &request, "z_out");
    let filtered = channels(&film, &beauty, &request, "depth_filtered");
    let closest = channels(&film, &beauty, &request, "depth");
    assert_eq!(bits(&z), bits(&filtered));
    assert_ne!(bits(&z), bits(&closest), "the edge blends two depths");
}

/// Exposure scales a radiance output and leaves `crust:filter:radiance = 0`
/// alone.
#[test]
fn exposure_scales_radiance_filter_outputs_only() {
    let both = format!(
        "{SELECT}{}",
        r#"
    def RenderVar "look_data"
    {
        uniform token dataType = "color3f"
        bool crust:filter:radiance = 0
        rel crust:filter = </Render/look>
    }
"#
    );
    let run = |exposure: f32| {
        let path = stage(
            &format!("exposure_{exposure}"),
            "        rel crust:sampleFilters = </Render/look>",
            &both,
        );
        let text = std::fs::read_to_string(&path).unwrap().replace(
            "float focalLength = 24",
            &format!("float focalLength = 24\n        float exposure = {exposure}"),
        );
        std::fs::write(&path, text).unwrap();
        let scene = load(&path);
        let (r, request) = renderer(scene);
        let (beauty, film) = render(&r, &request, true);
        (
            channels(&film, &beauty, &request, "look_out"),
            channels(&film, &beauty, &request, "look_data"),
        )
    };
    let (radiance0, data0) = run(0.0);
    let (radiance2, data2) = run(2.0);
    assert_eq!(bits(&data0), bits(&data2));
    assert_eq!(bits(&data0), bits(&radiance0));
    for (a, b) in radiance0[0].iter().zip(&radiance2[0]) {
        assert_eq!(a * 4.0, *b);
    }
}

/// A holdout sample feeds the filter its AOVs' clear values: the card held
/// out reads as off the matte, blue.
#[test]
fn a_holdout_feeds_the_filter_clear_values() {
    let pass = format!(
        "{SELECT}{}",
        r#"
    def RenderPass "hold"
    {
        rel renderSource = </Render/settings>
        prepend rel collection:matte:includes = </World/Card>
    }
"#
    );
    let path = stage(
        "holdout",
        "        rel crust:sampleFilters = </Render/look>",
        &pass,
    );
    let options = UsdImportOptions {
        pass: Some("/Render/hold".into()),
        ..UsdImportOptions::default()
    };
    let scene = Scene::from_usd_with_options(&path, &NoAssets, &options).expect("loads");
    let (r, request) = renderer(scene);
    let (beauty, film) = render(&r, &request, true);
    let look = channels(&film, &beauty, &request, "look_out");
    assert!(look[0].iter().all(|x| *x == 0.0), "no sample on the matte");
    assert!(look[2].iter().all(|x| (x - 1.0).abs() < 1e-5));
}

/// The beauty and the samples taken do not move for a sample filter and two
/// display filters, adaptive or not; and every channel, filter outputs
/// included, is the same in tiles and scanlines.
#[test]
fn filters_never_change_the_beauty_and_agree_across_render_modes() {
    let display = format!(
        "{SELECT}{}",
        r#"
    def Shader "soft"
    {
        uniform token info:id = "crust:blur"
        token inputs:in = "look_out"
        float inputs:radius = 2
    }
    def Shader "graded"
    {
        uniform token info:id = "crust:grade"
        token inputs:in = "soft"
        color3f inputs:gain = (2, 1, 0.5)
    }
    def RenderVar "final"
    {
        uniform token dataType = "color3f"
        rel crust:filter = </Render/graded>
    }
"#
    );
    for variance in [0.0, 0.05] {
        let settings = format!(
            "        rel crust:sampleFilters = </Render/look>\n        rel crust:displayFilters \
             = [</Render/soft>, </Render/graded>]\n        token crust:display = \
             \"final\"\n        float crust:varianceThreshold = {variance}\n        int \
             crust:minSamplesPerPixel = 4"
        );
        let scene = load(&stage(
            &format!("invariant_{variance}"),
            &settings,
            &display,
        ));
        assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
        let (r, request) = renderer(scene);
        let (plain, plain_rays) = r.render_with_stats(true, &|_, _| {});
        let (beauty, film, rays) = r.render_with_aovs(true, &|_, _| {}, &request);
        assert_eq!(
            beauty_bits(&plain),
            beauty_bits(&beauty),
            "variance {variance}"
        );
        assert_eq!(
            (plain_rays.spp_min, plain_rays.spp_max),
            (rays.spp_min, rays.spp_max)
        );
        let (scan_beauty, scan_film) = render(&r, &request, false);
        assert_eq!(beauty_bits(&beauty), beauty_bits(&scan_beauty));
        for name in ["look_out", "final", "matte", "variance"] {
            assert_eq!(
                bits(&channels(&film, &beauty, &request, name)),
                bits(&channels(&scan_film, &scan_beauty, &request, name)),
                "{name}"
            );
        }
        // One thread or many: the same bits.
        let one = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| render(&r, &request, true));
        assert_eq!(
            bits(&channels(&film, &beauty, &request, "final")),
            bits(&channels(&one.1, &one.0, &request, "final"))
        );
        // The displayed output is the `final` var's.
        let shown =
            crust_core::precomp::displayed(request.precomp.as_deref(), &beauty, &film).unwrap();
        let fin = channels(&film, &beauty, &request, "final");
        let (w, _) = beauty.size();
        let (x, y) = (5, 7);
        let (sr, sg, sb) = shown.get_rgb(x, y);
        assert_eq!(
            [sr, sg, sb],
            [fin[0][y * w + x], fin[1][y * w + x], fin[2][y * w + x]]
        );
    }
}

/// A snapshot shows the displayed output: after the render, exactly the
/// displayed output of the image it returned.
#[test]
fn a_snapshot_shows_the_displayed_output() {
    let display = format!(
        "{SELECT}{}",
        r#"
    def Shader "soft"
    {
        uniform token info:id = "crust:blur"
        token inputs:in = "look_out"
        float inputs:radius = 3
    }
    def RenderVar "final"
    {
        uniform token dataType = "color3f"
        rel crust:filter = </Render/soft>
    }
"#
    );
    let settings = "        rel crust:sampleFilters = </Render/look>\n        rel \
                    crust:displayFilters = </Render/soft>\n        token crust:display = \
                    \"final\"";
    let scene = load(&stage("snapshot", settings, &display));
    let (r, request) = renderer(scene);
    let control = RenderControl::new();
    let rendered = r.render_with_control(true, None, Some(&request), &control);
    let film = rendered.film.expect("a film");
    let shown = crust_core::precomp::displayed(request.precomp.as_deref(), &rendered.buffer, &film)
        .unwrap();
    let (_, snapshot) = control.snapshot().expect("published");
    assert_eq!(beauty_bits(&snapshot), beauty_bits(&shown));
    assert_ne!(beauty_bits(&snapshot), beauty_bits(&rendered.buffer));
}

#[test]
fn an_unbound_input_reads_its_default() {
    let graph = r#"
    def NodeGraph "g"
    {
        float inputs:nothing = 0.25
        float outputs:out.connect = </Render/g/m.outputs:out>
        def Shader "m"
        {
            uniform token info:id = "ND_multiply_float"
            float inputs:in1.connect = </Render/g.inputs:nothing>
            float inputs:in2 = 2
            float outputs:out
        }
    }
    def RenderVar "g_out"
    {
        uniform token dataType = "float"
        bool crust:filter:radiance = 0
        rel crust:filter = </Render/g>
    }
"#;
    let scene = load(&stage(
        "unbound",
        "        rel crust:sampleFilters = </Render/g>",
        graph,
    ));
    assert_eq!(codes(&scene), [WarningCode::PrecompUnboundInput]);
    let (r, request) = renderer(scene);
    let (beauty, film) = render(&r, &request, true);
    let out = channels(&film, &beauty, &request, "g_out");
    assert!(
        out[0].iter().all(|x| (x - 0.5).abs() < 1e-6),
        "{:?}",
        &out[0][..4]
    );
}

#[test]
fn a_variance_input_is_unbindable() {
    let graph = r#"
    def NodeGraph "g"
    {
        float inputs:variance
        float outputs:out.connect = </Render/g/m.outputs:out>
        def Shader "m"
        {
            uniform token info:id = "ND_multiply_float"
            float inputs:in1.connect = </Render/g.inputs:variance>
            float outputs:out
        }
    }
"#;
    let scene = load(&stage(
        "unbindable",
        "        rel crust:sampleFilters = </Render/g>",
        graph,
    ));
    assert_eq!(codes(&scene), [WarningCode::PrecompUnbindableInput]);
}

#[test]
fn a_texture_in_a_sample_filter_is_refused_and_reads_clear() {
    let graph = r#"
    def NodeGraph "g"
    {
        color3f outputs:out.connect = </Render/g/tex.outputs:out>
        def Shader "tex"
        {
            uniform token info:id = "ND_image_color3"
            asset inputs:file = @nothing.png@
            color3f outputs:out
        }
    }
    def RenderVar "g_out"
    {
        uniform token dataType = "color3f"
        rel crust:filter = </Render/g>
    }
"#;
    let scene = load(&stage(
        "unsupported",
        "        rel crust:sampleFilters = </Render/g>",
        graph,
    ));
    assert_eq!(codes(&scene), [WarningCode::PrecompUnsupportedNode]);
    let (r, request) = renderer(scene);
    let (beauty, film) = render(&r, &request, true);
    let out = channels(&film, &beauty, &request, "g_out");
    assert!(out.iter().flatten().all(|x| *x == 0.0));
}

#[test]
fn an_unknown_operation_passes_its_input_through() {
    let prims = format!(
        "{SELECT}{}",
        r#"
    def Shader "glare"
    {
        uniform token info:id = "crust:glare"
        token inputs:in = "look_out"
    }
    def RenderVar "glare_out"
    {
        uniform token dataType = "color3f"
        rel crust:filter = </Render/glare>
    }
"#
    );
    let settings = "        rel crust:sampleFilters = </Render/look>\n        rel \
                    crust:displayFilters = </Render/glare>";
    let scene = load(&stage("unknown_op", settings, &prims));
    assert_eq!(codes(&scene), [WarningCode::PrecompUnknownOperation]);
    let (r, request) = renderer(scene);
    let (beauty, film) = render(&r, &request, true);
    assert_eq!(
        bits(&channels(&film, &beauty, &request, "glare_out")),
        bits(&channels(&film, &beauty, &request, "look_out"))
    );
}

#[test]
fn a_filter_reading_a_later_one_is_a_cycle() {
    let prims = r#"
    def Shader "first"
    {
        uniform token info:id = "crust:blur"
        token inputs:in = "second"
    }
    def Shader "second"
    {
        uniform token info:id = "crust:blur"
        token inputs:in = "C"
    }
"#;
    let settings = "        rel crust:displayFilters = [</Render/first>, </Render/second>]";
    let scene = load(&stage("cycle", settings, prims));
    assert_eq!(codes(&scene), [WarningCode::PrecompCycle]);
}

#[test]
fn an_unknown_display_shows_the_beauty() {
    let scene = load(&stage(
        "unknown_display",
        "        token crust:display = \"nope\"",
        "",
    ));
    assert_eq!(codes(&scene), [WarningCode::PrecompUnknownDisplay]);
    let pc = scene.aovs.precomp.as_deref().expect("a precomp");
    assert_eq!(pc.display_name(), "beauty");
    assert!(pc.displayed_var().is_none());
}

#[test]
fn a_var_targeting_no_listed_filter_writes_nothing() {
    let scene = load(&stage("unknown_filter", "", SELECT));
    assert_eq!(codes(&scene), [WarningCode::PrecompUnknownFilter]);
    assert!(
        scene
            .aovs
            .products
            .iter()
            .flat_map(|p| &p.vars)
            .all(|v| v.name != "look_out")
    );
}

#[test]
fn a_filter_var_wider_than_its_output_is_refused() {
    let prims = format!(
        "{SELECT}{}",
        r#"
    def NodeGraph "f"
    {
        float inputs:matte
        float outputs:out.connect = </Render/f/m.outputs:out>
        def Shader "m"
        {
            uniform token info:id = "ND_multiply_float"
            float inputs:in1.connect = </Render/f.inputs:matte>
            float outputs:out
        }
    }
    def RenderVar "f_out"
    {
        uniform token dataType = "float2"
        rel crust:filter = </Render/f>
    }
"#
    );
    // A float output fills any width: no refusal.
    let scene = load(&stage(
        "wide_ok",
        "        rel crust:sampleFilters = [</Render/look>, </Render/f>]",
        &prims,
    ));
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    // Four channels never fit a filter output.
    let wide = prims.replace("\"float2\"", "\"color4f\"");
    let scene = load(&stage(
        "wide",
        "        rel crust:sampleFilters = [</Render/look>, </Render/f>]",
        &wide,
    ));
    assert_eq!(codes(&scene), [WarningCode::AovTypeMismatch]);
}

#[test]
fn filters_follow_the_active_pass() {
    let prims = format!(
        "{SELECT}{}",
        r#"
    def RenderSettings "other"
    {
        rel camera = </World/cam>
        int2 resolution = (32, 24)
        rel products = [</Render/p>]
        rel crust:displayFilters = </Render/soft>
    }
    def Shader "soft"
    {
        uniform token info:id = "crust:blur"
        token inputs:in = "C"
    }
    def RenderPass "a"
    {
        rel renderSource = </Render/settings>
    }
    def RenderPass "b"
    {
        rel renderSource = </Render/other>
    }
"#
    );
    let path = stage(
        "passes",
        "        rel crust:sampleFilters = </Render/look>",
        &prims,
    );
    let read = |pass: &str| {
        let options = UsdImportOptions {
            pass: Some(pass.into()),
            ..UsdImportOptions::default()
        };
        let scene = Scene::from_usd_with_options(&path, &NoAssets, &options).expect("loads");
        let pc = scene.aovs.precomp.expect("a precomp");
        (pc.sample_filter_paths(), pc.display_filter_paths())
    };
    assert_eq!(read("/Render/a"), (vec!["/Render/look".into()], vec![]));
    assert_eq!(read("/Render/b"), (vec![], vec!["/Render/soft".into()]));
}

#[test]
fn a_precomp_without_light_warns_of_a_black_beauty() {
    let path = stage(
        "black",
        "        rel crust:sampleFilters = </Render/look>",
        SELECT,
    );
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("def DomeLight \"sky\"", "def Xform \"sky\"");
    std::fs::write(&path, text).unwrap();
    let scene = load(&path);
    assert_eq!(codes(&scene), [WarningCode::PrecompBlackBeauty]);
}

/// A host that decodes one kernel: a 3×1 box, whatever the path.
struct BoxKernel;

impl crust_core::AssetLoader for BoxKernel {
    fn load_environment(
        &self,
        _: &std::path::Path,
        _: crust_core::ColorSpace,
    ) -> Option<crust_core::EnvironmentMap> {
        None
    }

    fn load_light_texture(
        &self,
        _: &std::path::Path,
        _: crust_core::ColorSpace,
    ) -> Option<std::sync::Arc<crust_core::LightTexture>> {
        crust_core::LightTexture::new(3, 1, vec![crust_core::Vec3A::ONE; 3])
            .map(std::sync::Arc::new)
    }
}

const CONVOLVE: &str = r#"
    def Shader "soft"
    {
        uniform token info:id = "crust:convolve"
        token inputs:in = "matte"
        asset inputs:kernel = @box.exr@
    }
    def RenderVar "soft_out"
    {
        uniform token dataType = "float"
        rel crust:filter = </Render/soft>
    }
"#;

/// The kernel crosses `AssetLoader` (crust-core decodes no image): a box
/// averages each pixel with its left and right neighbours, edges clamped.
#[test]
fn a_convolve_kernel_is_read_through_the_asset_loader() {
    let path = stage(
        "convolve",
        "        rel crust:displayFilters = </Render/soft>",
        CONVOLVE,
    );
    let scene =
        Scene::from_usd_with_options(&path, &BoxKernel, &UsdImportOptions::default()).unwrap();
    assert!(scene.warnings.is_empty(), "{:?}", scene.warnings);
    let (r, request) = renderer(scene);
    let (beauty, film) = render(&r, &request, true);
    let soft = &channels(&film, &beauty, &request, "soft_out")[0];
    let matte = &channels(&film, &beauty, &request, "matte")[0];
    let (w, h) = beauty.size();
    for y in 0..h {
        for x in 0..w {
            let at = |dx: isize| matte[y * w + (x as isize + dx).clamp(0, w as isize - 1) as usize];
            let want = (1.0 / 3.0) * at(-1) + (1.0 / 3.0) * at(0) + (1.0 / 3.0) * at(1);
            assert!((soft[y * w + x] - want).abs() < 1e-6, "({x}, {y})");
        }
    }
}

/// A kernel the host declines: the filter passes its input through, said
/// once.
#[test]
fn a_missing_kernel_passes_its_input_through() {
    let path = stage(
        "no_kernel",
        "        rel crust:displayFilters = </Render/soft>",
        CONVOLVE,
    );
    let scene = load(&path);
    assert!(
        codes(&scene).contains(&WarningCode::PrecompMissingKernel),
        "{:?}",
        codes(&scene)
    );
    let (r, request) = renderer(scene);
    let (beauty, film) = render(&r, &request, true);
    assert_eq!(
        bits(&channels(&film, &beauty, &request, "soft_out")),
        bits(&channels(&film, &beauty, &request, "matte"))
    );
}
