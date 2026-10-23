//! The session against the CLI, in process: what needs the session's own
//! buffers rather than what the protocol carries.

use super::render::{self, planes};
use super::session::State;
use crate::{Cli, Command, RenderRun, render_and_write};
use clap::Parser;
use crust_core::RenderControl;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .canonicalize()
        .expect("samples")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_mcp_unit").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `crust render -i <layer> -s 16 --region <region>`'s own body, its relative
/// outputs in `dir` (the CLI's are in its working directory).
fn cli_render(layer: &Path, region: [usize; 4], dir: &Path) {
    let [x0, y0, x1, y1] = region;
    let region = format!("{x0},{y0},{x1},{y1}");
    let args = [
        "crust",
        "render",
        "-i",
        layer.to_str().unwrap(),
        "-s",
        "16",
        "--region",
        &region,
    ];
    let Cli {
        command: Command::Render(args),
        ..
    } = Cli::try_parse_from(args).expect("render arguments")
    else {
        unreachable!("parsed as render")
    };
    let beauty = dir.join("output.exr");
    let control = RenderControl::without_snapshots();
    let run = RenderRun {
        control: &control,
        progress_bar: false,
        rendering: &|| {},
        rendered: &|| false,
        anchor: Some(dir),
        beauty: Some(beauty.to_str().unwrap()),
    };
    render_and_write(&args, &run).expect("the CLI renders");
}

/// A session render of `sample` at 16 spp to completion is the CLI's render
/// of the session's layer, bit for bit, file by file.
fn session_matches_cli(name: &str, sample: &str, region: [usize; 4]) {
    let dir = work_dir(name);
    let layer = dir.join("lookdev.usda");
    let mut state = State::default();
    state
        .open_session(
            samples().join(sample).to_str().unwrap(),
            layer.to_str().unwrap(),
            None,
        )
        .expect("opened");
    let open = state.session().expect("open");
    let (job, _) = open.start_render(Some(region), Some(16)).expect("started");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(job.wait(Duration::from_secs(3600)));
    let finished = job.finished().expect("finished");
    assert_eq!(job.status(), "done");
    let session = planes(&finished, &job.aovs, "session");

    cli_render(&layer, region, &dir);
    assert!(!session.is_empty());
    for (product, ours) in &session {
        let file = match product {
            None => dir.join("output.exr"),
            Some(prim) => {
                let p = job
                    .aovs
                    .products
                    .iter()
                    .find(|p| &p.prim_path == prim)
                    .unwrap();
                dir.join(&p.name)
            }
        };
        let cli = crust_assets::read_exr_planes(&file).expect("the CLI's file");
        let report = crust_core::compare::compare(&cli, ours);
        assert!(report.identical, "{name} {product:?}: {}", report.to_text());
        assert_eq!(
            cli.channels.keys().collect::<Vec<_>>(),
            ours.channels.keys().collect::<Vec<_>>(),
            "{name} {product:?}: the same channels"
        );
    }
}

/// An `undo` whose restored layer does not import is not applied: the layer
/// keeps its bytes, the scene stays the one it renders, and the step stays
/// in the history to be tried again.
#[test]
fn an_undo_that_does_not_import_is_not_applied() {
    let dir = work_dir("undo_fails");
    let layer = dir.join("lookdev.usda");
    let mut state = State::default();
    state
        .open_session(
            samples().join("cornellbox.usda").to_str().unwrap(),
            layer.to_str().unwrap(),
            None,
        )
        .expect("opened");
    let open = state.session().expect("open");
    let edit = super::edit::Snippet::parse(
        "over \"scene\" { over \"Sky\" { float inputs:exposure = -1 } }",
    )
    .expect("a snippet");
    open.edit(&edit).expect("edited");
    let edited = std::fs::read(&layer).unwrap();
    // A step whose bytes are no layer: its restore cannot import.
    open.undo.push(b"#usda 1.0\nover \"scene\" {".to_vec());
    let depth = open.undo.len();
    let e = open.undo().expect_err("the restored layer does not import");
    assert!(e.contains("not applied"), "{e}");
    assert_eq!(
        std::fs::read(&layer).unwrap(),
        edited,
        "the layer is as it was"
    );
    assert_eq!(open.undo.len(), depth, "the step is still there");
    // The stage was put back too: the edit is still what it composes.
    let exposure = super::query::query(&open.stage, "/scene/Sky.inputs:exposure")
        .expect("the edited attribute");
    assert_eq!(exposure["value"], -1.0);
    // Drop the bad step: the real one undoes.
    open.undo.pop();
    open.undo().expect("undone");
}

#[test]
fn a_session_render_is_the_clis_render_of_its_layer() {
    session_matches_cli("repro_cornellbox", "cornellbox.usda", [256, 96, 352, 192]);
}

#[test]
fn a_session_render_with_products_and_aovs_is_the_clis() {
    session_matches_cli("repro_aovs", "aovs.usda", [40, 20, 120, 70]);
}

/// A stage with one of everything an edit can apply in place: the render
/// settings and camera, a rect light and a textured dome, a textured
/// preview surface and an inline Standard Surface.
fn lookdev_stage(dir: &Path) -> PathBuf {
    let samples = samples();
    let s = samples.display();
    let stage = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Y"
)

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 4
    }}
}}

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 24
        double3 xformOp:translate = (0, 1, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}

    def DomeLight "Sky"
    {{
        asset inputs:texture:file = @{s}/sky_gradient.exr@
        float inputs:intensity = 0.5
    }}

    def RectLight "Key"
    {{
        float inputs:width = 2
        float inputs:height = 2
        float inputs:intensity = 8
        color3f inputs:color = (1, 1, 1)
        double3 xformOp:translate = (0, 4, 1)
        float3 xformOp:rotateXYZ = (-90, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]
    }}

    def Mesh "Floor" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-4, 0, 4), (4, 0, 4), (4, 0, -4), (-4, 0, -4)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (1, 1), (0, 1)] (
            interpolation = "vertex"
        )
        rel material:binding = </World/Looks/Tex>
    }}

    def Sphere "Ball" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {{
        double radius = 1
        double3 xformOp:translate = (0, 1, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        rel material:binding = </World/Looks/Std>
    }}

    def Sphere "Left" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {{
        double radius = 0.5
        double3 xformOp:translate = (-2, 0.5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        rel material:binding = </World/Looks/Base>
    }}

    def Sphere "Right" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {{
        double radius = 0.5
        double3 xformOp:translate = (2, 0.5, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        rel material:binding = </World/Looks/Derived>
    }}

    def Scope "Looks"
    {{
        def Material "Tex"
        {{
            token outputs:surface.connect = </World/Looks/Tex/Surface.outputs:surface>

            def Shader "Surface"
            {{
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </World/Looks/Tex/Albedo.outputs:rgb>
                float inputs:roughness = 0.6
                token outputs:surface
            }}

            def Shader "Albedo"
            {{
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @{s}/textures/mtlx_base.1001.png@
                token inputs:sourceColorSpace = "sRGB"
                float2 inputs:st.connect = </World/Looks/Tex/Reader.outputs:result>
                vector3f outputs:rgb
            }}

            def Shader "Reader"
            {{
                uniform token info:id = "UsdPrimvarReader_float2"
                string inputs:varname = "st"
                float2 outputs:result
            }}
        }}

        def Material "Base"
        {{
            token outputs:mtlx:surface.connect = </World/Looks/Base/Surface.outputs:out>

            def Shader "Surface"
            {{
                uniform token info:id = "ND_standard_surface_surfaceshader_100"
                color3f inputs:base_color = (0.2, 0.7, 0.3)
                token outputs:out
            }}
        }}

        def Material "Derived" (
            inherits = </World/Looks/Base>
        )
        {{
        }}

        def Material "Std"
        {{
            token outputs:mtlx:surface.connect = </World/Looks/Std/Surface.outputs:out>

            def Shader "Surface"
            {{
                uniform token info:id = "ND_standard_surface_surfaceshader_100"
                color3f inputs:base_color = (0.8, 0.2, 0.1)
                float inputs:specular_roughness = 0.3
                float inputs:emission = 0
                token outputs:out
            }}
        }}
    }}
}}
"#
    );
    let path = dir.join("stage.usda");
    std::fs::write(&path, stage).expect("write the stage");
    path
}

/// The session's render of its scene, full frame at 16 spp, every plane.
fn render_planes(state: &mut State) -> Vec<(Option<String>, crust_core::compare::Planes)> {
    let open = state.session().expect("open");
    let (job, _) = open.start_render(None, Some(16)).expect("started");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(job.wait(Duration::from_secs(3600)));
    let finished = job.finished().expect("finished");
    assert_eq!(job.status(), "done");
    planes(&finished, &job.aovs, "session")
}

/// `state`'s scene against a fresh session that imports `state`'s saved
/// layer whole: the same warnings, and a render identical plane by plane.
fn same_as_full_import(state: &mut State, dir: &Path, what: &str) {
    let open = state.session().expect("open");
    let (input, layer) = (open.input.clone(), open.output.clone());
    let warnings = open.scene.report.warnings.clone();
    let ours = render_planes(state);

    let copy = dir.join(format!("full_{}.usda", what.replace(' ', "_")));
    std::fs::copy(&layer, &copy).expect("copy the layer");
    let mut full = State::default();
    let opened = full
        .open_session(input.to_str().unwrap(), copy.to_str().unwrap(), None)
        .expect("the copy resumes");
    assert_eq!(opened["resumed"], true);
    assert_eq!(
        full.session().unwrap().scene.report.warnings,
        warnings,
        "{what}: the warnings a full import reports"
    );
    let theirs = render_planes(&mut full);
    assert_eq!(ours.len(), theirs.len());
    for ((product, a), (_, b)) in ours.iter().zip(&theirs) {
        let report = crust_core::compare::compare(b, a);
        assert!(report.identical, "{what} {product:?}: {}", report.to_text());
    }
}

/// Opens a session on the lookdev stage in `dir`.
fn lookdev_session(name: &str) -> (State, PathBuf) {
    let dir = work_dir(name);
    let input = lookdev_stage(&dir);
    let layer = dir.join("lookdev.usda");
    let mut state = State::default();
    state
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap(), None)
        .expect("opened");
    (state, dir)
}

/// Authors `usda` as one batch: how it was applied.
fn author(state: &mut State, usda: &str) -> String {
    let result = state
        .session()
        .expect("open")
        .author_usda(usda)
        .expect("authored");
    result["applied"].as_str().expect("applied").to_owned()
}

fn undo(state: &mut State) -> String {
    let result = state.session().expect("open").undo().expect("undone");
    result["applied"].as_str().expect("applied").to_owned()
}

/// Each class applies in place, and a render after it — and after its undo
/// — is a full import's of the same layer, bit for bit (the in place ↔ full
/// re-import pair).
#[test]
fn edits_in_place_render_as_a_full_import() {
    let (mut state, dir) = lookdev_session("in_place");
    let s = samples();
    let s = s.display();
    let cases: &[(&str, &str, String)] = &[
        (
            "exposure",
            "settings",
            r#"over "World" { over "Cam" { float exposure = 1 } }"#.to_owned(),
        ),
        (
            "max depth",
            "settings",
            r#"over "Render" { over "settings" { int crust:maxDepth = 2 } }"#.to_owned(),
        ),
        (
            "sampler",
            "settings",
            r#"over "Render" { over "settings" { token crust:sampler = "pmjBn" } }"#.to_owned(),
        ),
        (
            "rect light",
            "lights",
            r#"over "World" { over "Key" { float inputs:intensity = 20
                color3f inputs:color = (1, 0.5, 0.2) } }"#
                .to_owned(),
        ),
        (
            "dome texture",
            "lights",
            format!(
                r#"over "World" {{ over "Sky" {{ asset inputs:texture:file = @{s}/sky_env.exr@ }} }}"#
            ),
        ),
        (
            "base colour",
            "materials",
            r#"over "World" { over "Looks" { over "Std" { over "Surface" {
                color3f inputs:base_color = (0.1, 0.3, 0.9) } } } }"#
                .to_owned(),
        ),
        (
            "roughness",
            "materials",
            r#"over "World" { over "Looks" { over "Tex" { over "Surface" {
                float inputs:roughness = 0.2 } } } }"#
                .to_owned(),
        ),
        (
            "texture file",
            "materials",
            format!(
                r#"over "World" {{ over "Looks" {{ over "Tex" {{ over "Albedo" {{
                asset inputs:file = @{s}/textures/mtlx_base.1002.png@ }} }} }} }}"#
            ),
        ),
    ];
    for (what, class, usda) in cases {
        assert_eq!(author(&mut state, usda), *class, "{what}");
        same_as_full_import(&mut state, &dir, what);
    }
    for (what, class, _) in cases.iter().rev() {
        assert_eq!(undo(&mut state), *class, "undo {what}");
        same_as_full_import(&mut state, &dir, &format!("undo {what}"));
    }
}

/// What is not confined is applied as `full`.
#[test]
fn unconfined_edits_are_full() {
    let (mut state, _dir) = lookdev_session("full");
    let cases = [
        (
            "a transform",
            r#"over "World" { over "Ball" { double3 xformOp:translate = (0, 2, 0) } }"#,
        ),
        (
            "an emission",
            r#"over "World" { over "Looks" { over "Std" { over "Surface" {
                float inputs:emission = 1 } } } }"#,
        ),
        (
            "a light's shape",
            r#"over "World" { over "Key" { float inputs:width = 3 } }"#,
        ),
        ("a new prim", r#"over "World" { def Sphere "Other" { } }"#),
        (
            "a binding",
            r#"over "World" { over "Ball" { rel material:binding = </World/Looks/Tex> } }"#,
        ),
    ];
    for (what, usda) in cases {
        assert_eq!(author(&mut state, usda), "full", "{what}");
    }
    // A batch takes its most expensive class.
    let mixed = r#"over "World" { over "Cam" { float exposure = 0.5 }
        over "Key" { float inputs:intensity = 4 } }"#;
    assert_eq!(author(&mut state, mixed), "lights");
}

/// `CRUST_SCENE_DELTAS=0`: a session without kept assets imports every
/// edit.
#[test]
fn without_scene_deltas_every_edit_is_full() {
    let (mut state, _dir) = lookdev_session("no_deltas");
    state.session().unwrap().assets = None;
    let dim = r#"over "World" { over "Key" { float inputs:intensity = 4 } }"#;
    assert_eq!(author(&mut state, dim), "full");
}

/// A `full` edit on a textured stage whose files did not change decodes no
/// texture a second time.
#[test]
fn a_full_edit_decodes_no_texture_again() {
    let (mut state, _dir) = lookdev_session("decodes");
    let decodes = |state: &mut State| {
        let open = state.session().unwrap();
        open.assets.as_ref().expect("kept assets").decodes()
    };
    let before = decodes(&mut state);
    assert!(before > 0, "the stage decodes its texture");
    let moved = r#"over "World" { over "Ball" { double3 xformOp:translate = (0, 1.5, 0) } }"#;
    assert_eq!(author(&mut state, moved), "full");
    assert_eq!(decodes(&mut state), before);
}

/// A texture overwritten on disk mid-session is read again by the next
/// import: the session renders what a fresh one does.
#[test]
fn a_re_exported_texture_is_read_again() {
    let (mut state, dir) = lookdev_session("re_exported");
    let s = samples();
    let texture = dir.join("albedo.png");
    std::fs::copy(s.join("textures/mtlx_base.1001.png"), &texture).unwrap();
    let use_it = format!(
        r#"over "World" {{ over "Looks" {{ over "Tex" {{ over "Albedo" {{
            asset inputs:file = @{}@ }} }} }} }}"#,
        texture.display()
    );
    assert_eq!(author(&mut state, &use_it), "materials");
    render_planes(&mut state);

    let other = s.join("textures/mtlx_base.1002.png");
    assert_ne!(
        std::fs::metadata(&other).unwrap().len(),
        std::fs::metadata(&texture).unwrap().len()
    );
    std::fs::copy(&other, &texture).unwrap();
    let moved = r#"over "World" { over "Ball" { double3 xformOp:translate = (0, 1.5, 0) } }"#;
    assert_eq!(author(&mut state, moved), "full");
    same_as_full_import(&mut state, &dir, "re-exported");
}

/// A shader another used material composes (here through `inherits`) is
/// not confined to its own material: the edit is `full`, and the scene is a
/// full import's.
#[test]
fn an_edit_another_material_reads_is_full() {
    let (mut state, dir) = lookdev_session("shared_shader");
    let base = r#"over "World" { over "Looks" { over "Base" { over "Surface" {
        color3f inputs:base_color = (0.9, 0.1, 0.1) } } } }"#;
    assert_eq!(author(&mut state, base), "full");
    same_as_full_import(&mut state, &dir, "shared shader");
}

/// A file the scene read changed on disk before an edit that would apply in
/// place: the scene's materials hold its old decode, so the edit is `full`
/// and the scene a fresh import's.
#[test]
fn a_file_changed_on_disk_makes_the_next_edit_full() {
    let (mut state, dir) = lookdev_session("changed_on_disk");
    let s = samples();
    let texture = dir.join("albedo.png");
    std::fs::copy(s.join("textures/mtlx_base.1001.png"), &texture).unwrap();
    let use_it = format!(
        r#"over "World" {{ over "Looks" {{ over "Tex" {{ over "Albedo" {{
            asset inputs:file = @{}@ }} }} }} }}"#,
        texture.display()
    );
    assert_eq!(author(&mut state, &use_it), "materials");
    std::fs::copy(s.join("textures/mtlx_base.1002.png"), &texture).unwrap();
    let dim = r#"over "World" { over "Key" { float inputs:intensity = 4 } }"#;
    assert_eq!(author(&mut state, dim), "full");
    same_as_full_import(&mut state, &dir, "changed on disk");
}

/// After an in-place edit, the check report lists the import's phases and
/// then the update, and counts the import's preloaded textures.
#[test]
fn the_report_after_an_in_place_edit_names_the_update() {
    let (mut state, _dir) = lookdev_session("report");
    let before = state.session().unwrap().scene.report.clone();
    let dim = r#"over "World" { over "Key" { float inputs:intensity = 4 } }"#;
    assert_eq!(author(&mut state, dim), "lights");
    let after = &state.session().unwrap().scene.report;
    let names = |r: &crust_core::check::CheckReport| -> Vec<String> {
        r.import.iter().map(|p| p.name.clone()).collect()
    };
    let mut expected = names(&before);
    expected.push("Update in place (lights)".to_owned());
    assert_eq!(names(after), expected);
    assert_eq!(after.findings, before.findings);
    // A second update replaces the first's phase rather than adding to it.
    let again = r#"over "World" { over "Key" { float inputs:intensity = 5 } }"#;
    assert_eq!(author(&mut state, again), "lights");
    assert_eq!(names(&state.session().unwrap().scene.report), expected);
}

/// Starts a full-frame render of the session's scene at 1 spp and waits for it.
fn finished_render(state: &mut State) -> std::sync::Arc<super::render::Job> {
    let (job, _) = state
        .session()
        .expect("open")
        .start_render(None, Some(1))
        .expect("started");
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(job.wait(Duration::from_secs(3600)));
    job
}

/// `probe` at pixel (`x`, `y`) of render `job`: its `prim` and `material`.
fn probe_names(state: &mut State, job: &super::render::Job, x: usize, y: usize) -> [Value; 2] {
    let open = state.session().expect("open");
    let result = render::probe(job, &open.scene, x, y).expect("probed");
    [result["prim"].clone(), result["material"].clone()]
}

/// `probe` names the prim a pixel's camera ray hits and its bound material
/// (spec: "Probing a pixel"), through an in-place edit, which moves no
/// geometry, and not once the scene was imported again.
#[test]
fn probe_names_the_prim_and_material_a_pixel_hits() {
    let (mut state, _dir) = lookdev_session("probe");
    let job = finished_render(&mut state);
    // The lookdev stage at 48 × 32: the ball fills the centre, the floor the
    // bottom rows, and the top rows look past everything into the dome.
    let ball = [json!("/World/Ball"), json!("/World/Looks/Std")];
    let floor = [json!("/World/Floor"), json!("/World/Looks/Tex")];
    assert_eq!(probe_names(&mut state, &job, 24, 16), ball);
    assert_eq!(probe_names(&mut state, &job, 24, 31), floor);
    assert_eq!(
        probe_names(&mut state, &job, 24, 0),
        [Value::Null, Value::Null]
    );

    let colour = r#"over "World" { over "Looks" { over "Std" { over "Surface" {
        color3f inputs:base_color = (0.1, 0.3, 0.9) } } } }"#;
    assert_eq!(author(&mut state, colour), "materials");
    assert_eq!(probe_names(&mut state, &job, 24, 16), ball);

    let moved = r#"over "World" { over "Ball" { double3 xformOp:translate = (0, 1.5, 0) } }"#;
    assert_eq!(author(&mut state, moved), "full");
    let open = state.session().expect("open");
    let stale = render::probe(&job, &open.scene, 24, 16).expect("probed");
    assert_eq!(stale["prim"], Value::Null);
    assert!(
        stale["prim_note"]
            .as_str()
            .unwrap()
            .contains("imported again"),
        "{stale}"
    );
    let job = finished_render(&mut state);
    assert_eq!(probe_names(&mut state, &job, 24, 16), ball);
}

/// A hit inside a native instance names the instance prim, and one inside a
/// PointInstancer's prototype the instancer: what the import's geometry runs
/// record. The material is the prototype mesh's.
#[test]
fn probe_names_the_instance_or_the_instancer() {
    let dir = work_dir("probe_instancing");
    let layer = dir.join("lookdev.usda");
    let input = samples().join("instancing.usda");
    let mut state = State::default();
    state
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap(), None)
        .expect("opened");
    let job = finished_render(&mut state);
    assert_eq!(
        probe_names(&mut state, &job, 35, 110),
        [json!("/World/TowerA"), json!("/World/Looks/Copper")]
    );
    assert_eq!(
        probe_names(&mut state, &job, 420, 135),
        [json!("/World/Scatter"), json!("/World/Looks/Emerald")]
    );
}

/// Opens a session on the Cornell box and renders `region` at 16 spp to
/// completion.
fn cornell_render(name: &str, region: [usize; 4]) -> (State, std::sync::Arc<super::render::Job>) {
    let dir = work_dir(name);
    let layer = dir.join("lookdev.usda");
    let mut state = State::default();
    state
        .open_session(
            samples().join("cornellbox.usda").to_str().unwrap(),
            layer.to_str().unwrap(),
            None,
        )
        .expect("opened");
    let (job, _) = state
        .session()
        .expect("open")
        .start_render(Some(region), Some(16))
        .expect("started");
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(job.wait(Duration::from_secs(3600)));
    assert_eq!(job.status(), "done");
    (state, job)
}

/// The spec's flow: a probe on a completed render, then `light_paths` on
/// the region around the pixel, whose beauty is the render's; refused once
/// an edit changed the layer, and the same report once the edit is undone.
#[test]
fn light_paths_inspects_the_render_its_layer_made() {
    use crust_core::inspect::GroupBy;
    let (mut state, job) = cornell_render("light_paths", [300, 150, 340, 190]);
    let region = [315, 165, 325, 175];
    let open = state.session().expect("open");
    let probed = render::probe(&job, &open.scene, 320, 170).expect("probed");
    assert!(probed["beauty"].is_array(), "{probed}");
    let report = open
        .light_paths(job.id, region, GroupBy::ExprLight, 8)
        .expect("inspected");
    assert_eq!(report["format"], "crust-light-paths/1");
    assert_eq!(report["reproduces_frame"], true, "{report:#}");
    assert_eq!(report["records"].as_array().unwrap().len(), 8);
    let finished = job.finished().expect("finished");
    let (_, h) = job.settings.get_dimensions();
    for p in report["pixels"].as_array().unwrap() {
        let (x, y) = (
            p["pixel"][0].as_u64().unwrap() as usize,
            p["pixel"][1].as_u64().unwrap() as usize,
        );
        let ours = finished.buffer.get_pixel(x, h - 1 - y);
        for (c, want) in ours.to_array().iter().enumerate() {
            let got = p["beauty"][c].as_f64().unwrap() as f32;
            assert_eq!(got.to_bits(), want.to_bits(), "pixel {x},{y}");
        }
    }
    assert!(
        report["command"]
            .as_str()
            .unwrap()
            .contains("-s 16 --region 315,165,325,175"),
        "{}",
        report["command"]
    );

    // Outside the render's region, or over the cap — its area past
    // `usize` included — or more records than the cap.
    for bad in [
        [290, 165, 300, 175],
        [300, 150, 340, 190],
        [0, 0, usize::MAX, usize::MAX],
    ] {
        assert!(
            open.light_paths(job.id, bad, GroupBy::Expr, 8).is_err(),
            "{bad:?}"
        );
    }
    let e = open
        .light_paths(job.id, region, GroupBy::Expr, 1001)
        .unwrap_err();
    assert!(e.contains("1000"), "{e}");
    let e = open
        .light_paths(job.id, region, GroupBy::ExprPrimsLight, 8)
        .unwrap_err();
    assert!(e.contains("prim"), "{e}");

    let edit = super::edit::Snippet::parse(
        "over \"scene\" { over \"Sky\" { float inputs:exposure = -1 } }",
    )
    .expect("a snippet");
    open.edit(&edit).expect("edited");
    let e = open
        .light_paths(job.id, region, GroupBy::ExprLight, 8)
        .unwrap_err();
    assert!(e.contains("changed since render"), "{e}");
    assert!(!e.contains("  "), "one space between words: {e}");

    open.undo().expect("undone");
    let again = open
        .light_paths(job.id, region, GroupBy::ExprLight, 8)
        .expect("the layer is the render's again");
    assert_eq!(again, report);
}

/// A render that has not completed has nothing to inspect.
#[test]
fn light_paths_refuses_a_render_that_did_not_complete() {
    use crust_core::inspect::GroupBy;
    let (mut state, _) = cornell_render("light_paths_running", [300, 150, 310, 160]);
    let open = state.session().expect("open");
    let (job, _) = open.start_render(None, Some(4096)).expect("started");
    let e = open
        .light_paths(job.id, [300, 150, 310, 160], GroupBy::ExprLight, 8)
        .unwrap_err();
    assert!(e.contains("rendering"), "{e}");
    assert!(!e.contains("  "), "one space between words: {e}");
    open.renders.cancel(job.id).expect("cancelled");
    let e = open
        .light_paths(job.id, [300, 150, 310, 160], GroupBy::ExprLight, 8)
        .unwrap_err();
    assert!(e.contains("cancelled"), "{e}");
    assert!(!e.contains("  "), "one space between words: {e}");
}

// --- Sources on disk (`live-session-source-reload`) ---

/// Writes a stage whose parts live in other layers, each of which another
/// program may rewrite: `lights.usda` (a sublayer defining the key light),
/// `prop.usda` (referenced) and `pay.usda` (behind a payload). Returns the
/// root layer, the session's input.
fn sourced_stage(dir: &Path) -> PathBuf {
    std::fs::write(dir.join("lights.usda"), lights_layer(8.0)).unwrap();
    std::fs::write(dir.join("prop.usda"), quad_layer("Prop", 0.0)).unwrap();
    std::fs::write(dir.join("pay.usda"), quad_layer("Pay", 0.0)).unwrap();
    let root = dir.join("shot.usda");
    std::fs::write(
        &root,
        r#"#usda 1.0
(
    defaultPrim = "World"
    subLayers = [@./lights.usda@]
    upAxis = "Y"
)

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 4
    }
}

def Xform "World"
{
    def Camera "Cam"
    {
        float focalLength = 24
        double3 xformOp:translate = (0, 1, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def DomeLight "Sky"
    {
        float inputs:intensity = 0.5
    }

    def "Prop" (
        prepend references = @./prop.usda@
    )
    {
    }

    def "Heavy" (
        prepend payload = @./pay.usda@
    )
    {
    }
}
"#,
    )
    .unwrap();
    root
}

/// The sublayer: the key light at `intensity`.
fn lights_layer(intensity: f32) -> String {
    format!(
        r#"#usda 1.0

over "World"
{{
    def RectLight "Key"
    {{
        float inputs:width = 2
        float inputs:height = 2
        float inputs:intensity = {intensity}
        color3f inputs:color = (1, 1, 1)
        double3 xformOp:translate = (0, 4, 1)
        float3 xformOp:rotateXYZ = (-90, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]
    }}
}}
"#
    )
}

/// A 2 x 2 quad on the ground as `name`'s layer, lifted by `height`.
fn quad_layer(name: &str, height: f64) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "{name}"
)

def Mesh "{name}"
{{
    int[] faceVertexCounts = [4]
    int[] faceVertexIndices = [0, 1, 2, 3]
    point3f[] points = [(-1, 0, -1), (1, 0, -1), (1, 0, 1), (-1, 0, 1)]
    double3 xformOp:translate = (0, {height}, 0)
    uniform token[] xformOpOrder = ["xformOp:translate"]
}}
"#
    )
}

/// What another program does to a source: `path` rewritten as `text`, its
/// modification time a second on, so a coarse filesystem clock sees it.
fn rewrite(path: &Path, text: &str) {
    let before = std::fs::metadata(path).and_then(|m| m.modified()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(before + Duration::from_secs(1))
        .unwrap();
}

/// Opens a session on the sourced stage in a fresh `dir`.
fn sourced_session(name: &str) -> (State, PathBuf) {
    let dir = work_dir(name).canonicalize().unwrap();
    let input = sourced_stage(&dir);
    let layer = dir.join("lookdev.usda");
    let mut state = State::default();
    state
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap(), None)
        .expect("opened");
    (state, dir)
}

fn changed(result: &Value) -> Vec<String> {
    result["sources_changed"]
        .as_array()
        .unwrap_or_else(|| panic!("{result:#}"))
        .iter()
        .map(|p| p.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_session_watches_every_layer_its_import_composed() {
    let (mut state, dir) = sourced_session("sources_listed");
    let open = state.session().unwrap();
    let mut layers = open.scene.layers.clone();
    layers.sort();
    let mut expected: Vec<PathBuf> = [
        "lookdev.usda",
        "lights.usda",
        "pay.usda",
        "prop.usda",
        "shot.usda",
    ]
    .iter()
    .map(|f| dir.join(f))
    .collect();
    expected.sort();
    assert_eq!(layers, expected);
}

/// A light dimmed after its sublayer was rewritten is a full import, and
/// renders as a fresh session of the same layer does; the same edit with
/// nothing changed on disk applies in place.
#[test]
fn a_rewritten_sublayer_makes_the_next_edit_full() {
    let (mut state, dir) = sourced_session("sources_sublayer_edit");
    // The key's intensity shows, so a scene still at the old intensity
    // would not render as the fresh session does.
    let lit = render_planes(&mut state);
    author(
        &mut state,
        r#"over "World" { over "Key" { float inputs:intensity = 3 } }"#,
    );
    let dimmer = render_planes(&mut state);
    assert!(!crust_core::compare::compare(&lit[0].1, &dimmer[0].1).identical);
    undo(&mut state);
    let tint =
        |c: &str| format!(r#"over "World" {{ over "Key" {{ color3f inputs:color = {c} }} }}"#);
    assert_eq!(author(&mut state, &tint("(1, 0.9, 0.8)")), "lights");
    rewrite(&dir.join("lights.usda"), &lights_layer(3.0));
    let result = state
        .session()
        .unwrap()
        .author_usda(&tint("(1, 0.8, 0.7)"))
        .expect("authored");
    assert_eq!(result["applied"], "full", "{result:#}");
    assert_eq!(
        changed(&result),
        [dir.join("lights.usda").display().to_string()]
    );
    same_as_full_import(&mut state, &dir, "sublayer rewritten");
    // Seen once: the next edit applies in place again.
    assert_eq!(author(&mut state, &tint("(1, 1, 1)")), "lights");
}

#[test]
fn a_query_after_a_referenced_layer_changed_answers_the_new_value() {
    let (mut state, dir) = sourced_session("sources_query");
    let open = state.session().unwrap();
    let ask = |open: &mut super::session::Open| {
        open.refresh().expect("refreshed");
        super::query::query(&open.stage, "/World/Prop.xformOp:translate").expect("queried")["value"]
            .clone()
    };
    assert_eq!(ask(open), json!([0.0, 0.0, 0.0]));
    rewrite(&dir.join("prop.usda"), &quad_layer("Prop", 0.5));
    assert_eq!(ask(open), json!([0.0, 0.5, 0.0]));
}

/// A render with no edit in between shows a rewritten source — the layer
/// behind a payload included — and is a fresh session's render.
#[test]
fn a_render_after_a_source_changed_shows_it() {
    let (mut state, dir) = sourced_session("sources_render");
    let before = render_planes(&mut state);
    rewrite(&dir.join("pay.usda"), &quad_layer("Pay", 0.75));
    let open = state.session().unwrap();
    assert_eq!(
        open.bring_current().expect("imported"),
        vec![dir.join("pay.usda")]
    );
    assert!(open.bring_current().expect("current").is_empty());
    let after = render_planes(&mut state);
    let report = crust_core::compare::compare(&before[0].1, &after[0].1);
    assert!(!report.identical, "the payload's quad moved");
    same_as_full_import(&mut state, &dir, "payload rewritten");
}

/// A source rewritten into text that does not parse fails the render, the
/// old scene unrendered; once it parses again the render goes ahead.
#[test]
fn a_source_that_no_longer_imports_fails_the_render_until_fixed() {
    let (mut state, dir) = sourced_session("sources_broken");
    let shot = dir.join("shot.usda");
    let good = std::fs::read_to_string(&shot).unwrap();
    rewrite(&shot, "#usda 1.0\ndef Xform \"World\" {\n");
    let open = state.session().unwrap();
    let e = open.start_render(None, Some(1)).map(|_| ()).unwrap_err();
    assert!(e.contains("shot.usda"), "{e}");
    assert!(open.stale.is_some());
    let e = open.start_render(None, Some(1)).map(|_| ()).unwrap_err();
    assert!(e.contains("shot.usda"), "tried again: {e}");
    rewrite(&shot, &good);
    open.start_render(None, Some(1)).expect("imports again");
    assert!(open.stale.is_none());
    same_as_full_import(&mut state, &dir, "fixed");
}

/// `light_paths` refuses a render made before a source changed on disk.
#[test]
fn light_paths_refuses_a_render_from_before_a_source_changed() {
    use crust_core::inspect::GroupBy;
    let (mut state, dir) = sourced_session("sources_light_paths");
    let region = [20, 12, 26, 18];
    let open = state.session().unwrap();
    let (job, _) = open.start_render(Some(region), Some(4)).expect("started");
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(job.wait(Duration::from_secs(3600)));
    open.light_paths(job.id, region, GroupBy::ExprLight, 4)
        .expect("inspected");
    rewrite(&dir.join("lights.usda"), &lights_layer(2.0));
    let e = open
        .light_paths(job.id, region, GroupBy::ExprLight, 4)
        .unwrap_err();
    assert!(e.contains("source layer"), "{e}");
}

#[test]
fn reload_names_what_changed_and_imports_even_when_nothing_did() {
    let (mut state, dir) = sourced_session("sources_reload");
    let before = render_planes(&mut state);
    let result = state.session().unwrap().reload().expect("reloaded");
    assert_eq!(result["applied"], "full", "{result:#}");
    assert!(changed(&result).is_empty());
    let again = render_planes(&mut state);
    let report = crust_core::compare::compare(&before[0].1, &again[0].1);
    assert!(report.identical, "{}", report.to_text());

    rewrite(&dir.join("lights.usda"), &lights_layer(2.0));
    let result = state.session().unwrap().reload().expect("reloaded");
    assert_eq!(
        changed(&result),
        [dir.join("lights.usda").display().to_string()]
    );
    same_as_full_import(&mut state, &dir, "reloaded");
}

/// The undo history survives a reload: the layer goes back to its bytes
/// before the edit, composed over the rewritten source.
#[test]
fn undo_after_a_reload_composes_over_the_new_source() {
    let (mut state, dir) = sourced_session("sources_undo");
    let layer = dir.join("lookdev.usda");
    let original = std::fs::read(&layer).unwrap();
    author(
        &mut state,
        r#"over "World" { over "Key" { float inputs:exposure = 1 } }"#,
    );
    rewrite(&dir.join("lights.usda"), &lights_layer(2.0));
    state.session().unwrap().reload().expect("reloaded");
    undo(&mut state);
    assert_eq!(std::fs::read(&layer).unwrap(), original);
    same_as_full_import(&mut state, &dir, "undone after reload");
}

#[test]
fn reload_cancels_a_running_render() {
    let (mut state, _) = sourced_session("sources_reload_cancels");
    let open = state.session().unwrap();
    let (job, _) = open.start_render(None, Some(65536)).expect("started");
    let result = open.reload().expect("reloaded");
    assert_eq!(result["cancelled_render"], job.id, "{result:#}");
    assert_eq!(job.status(), "cancelled");
}

/// Opens a session on `stage`, written into a fresh work dir as `input.usda`.
fn session_on(name: &str, stage: &str) -> (State, PathBuf) {
    let dir = work_dir(name);
    let input = dir.join("input.usda");
    std::fs::write(&input, stage).expect("write the stage");
    let layer = dir.join("edits.usda");
    let mut state = State::default();
    state
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap(), None)
        .expect("opened");
    (state, dir)
}

/// Tessellation that reads the camera (adaptive subdivision, an implicit
/// shape sized on screen) keeps a settings edit in place unless the
/// projection it read moves: an exposure stays, a focal length does not.
#[test]
fn camera_tessellated_settings_edits_stay_in_place() {
    let s = samples();
    let s = s.display();
    // The adaptive sample, at a test's resolution: its subdivision reads
    // this projection.
    let adaptive = format!(
        r#"#usda 1.0
(
    subLayers = [@{s}/subdivision_adaptive.usda@]
    defaultPrim = "World"
    upAxis = "Y"
)

over "Render" {{ over "settings" {{ int2 resolution = (48, 27) }} }}
"#
    );
    let on_screen = r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 2
    }
}

def Xform "World"
{
    def Camera "Cam"
    {
        float focalLength = 24
        double3 xformOp:translate = (0, 0, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def DistantLight "Sun"
    {
        float inputs:intensity = 3
    }

    def Cylinder "Post"
    {
        double radius = 1
        double height = 2
    }
}
"#;
    for (name, stage) in [("adaptive", adaptive.as_str()), ("on_screen", on_screen)] {
        let (mut state, dir) = session_on(&format!("tessellated_{name}"), stage);
        let exposure = r#"over "World" { over "Cam" { float exposure = 1 } }"#;
        assert_eq!(author(&mut state, exposure), "settings", "{name}");
        same_as_full_import(&mut state, &dir, &format!("{name} exposure"));
        assert_eq!(undo(&mut state), "settings", "{name} undo");
        same_as_full_import(&mut state, &dir, &format!("{name} undo exposure"));
        // Without tessellation reading the camera, a lens is a settings edit:
        // `full` here is the projection check at work.
        let lens = r#"over "World" { over "Cam" { float focalLength = 35 } }"#;
        assert_eq!(author(&mut state, lens), "full", "{name} focal length");
    }
}

/// A translate, then a rotate, of the camera at `camera`: each
/// is `class`, and when in place, the scene is a full import's.
fn move_camera(state: &mut State, dir: &Path, name: &str, camera: &str, class: &str) {
    let (parent, cam) = camera.rsplit_once('/').unwrap();
    let parent = parent.trim_start_matches('/');
    let moves = [
        ("translate", "double3 xformOp:translate = (0.3, 0.6, 5.5)"),
        (
            "rotate",
            "float3 xformOp:rotateXYZ = (-4, 6, 0)\n uniform token[] xformOpOrder = \
             [\"xformOp:translate\", \"xformOp:rotateXYZ\"]",
        ),
    ];
    for (what, op) in moves {
        let usda = format!(r#"over "{parent}" {{ over "{cam}" {{ {op} }} }}"#);
        assert_eq!(author(state, &usda), class, "{name} {what}");
        if class != "full" {
            same_as_full_import(state, dir, &format!("{name} {what}"));
        }
    }
}

/// A stage of meshes only (nothing tessellated from the camera) seen by
/// `/World/Cam`; `settings` and `under_camera` are spliced into the settings
/// prim and the camera prim.
fn mesh_stage(settings: &str, under_camera: &str) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 2
        {settings}
    }}
}}

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 24
        double3 xformOp:translate = (0, 0.5, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
{under_camera}
    }}

    def DomeLight "Sky"
    {{
        color3f inputs:color = (0.4, 0.5, 0.8)
    }}

    def DistantLight "Sun"
    {{
        float inputs:intensity = 3
    }}

    def Mesh "Floor"
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-4, 0, 4), (4, 0, 4), (4, 0, -4), (-4, 0, -4)]
    }}

    def Mesh "Card"
    {{
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, 0, 0), (1, 0, 0), (1, 2, 0), (-1, 2, 0)]
    }}
}}
"#
    )
}

/// The render camera's transform is a settings edit, in place and a full
/// import's bit for bit, where nothing tessellated reads it: meshes alone,
/// the Cornell box (whose camera is the first one met, unnamed), and a dome
/// the camera does not see.
#[test]
fn moving_the_render_camera_is_a_settings_edit() {
    let (mut state, dir) = session_on("camera_moves", &mesh_stage("", ""));
    move_camera(&mut state, &dir, "meshes", "/World/Cam", "settings");
    assert_eq!(undo(&mut state), "settings", "undo the rotate");
    same_as_full_import(&mut state, &dir, "undo the rotate");

    let hidden = mesh_stage("bool domeLightCameraVisibility = 0", "");
    let (mut state, dir) = session_on("camera_moves_dome_hidden", &hidden);
    move_camera(&mut state, &dir, "dome hidden", "/World/Cam", "settings");

    let s = samples();
    let cornell = format!(
        r#"#usda 1.0
(
    subLayers = [@{}/cornellbox.usda@]
)

def Scope "Render" {{ def RenderSettings "settings" {{ uniform int2 resolution = (48, 32) }} }}
"#,
        s.display()
    );
    let (mut state, dir) = session_on("camera_moves_cornell", &cornell);
    move_camera(&mut state, &dir, "cornellbox", "/scene/camera1", "settings");
}

/// A `!resetXformStack!` on the render camera drops its parent's transform:
/// in place, as the rebuild re-reads the camera through the import's own
/// composition, and the render is a full import's.
#[test]
fn resetting_the_render_camera_s_transform_stack_is_a_settings_edit() {
    let stage = mesh_stage("", "").replace(
        "def Xform \"World\"\n{\n",
        "def Xform \"World\"\n{\n    double3 xformOp:translate = (0.5, 0.4, 0)\n    \
         uniform token[] xformOpOrder = [\"xformOp:translate\"]\n",
    );
    assert!(stage.contains("(0.5, 0.4, 0)"), "the parent is transformed");
    let (mut state, dir) = session_on("camera_reset", &stage);
    let reset = r#"over "World" { over "Cam" {
        uniform token[] xformOpOrder = ["!resetXformStack!", "xformOp:translate"] } }"#;
    assert_eq!(author(&mut state, reset), "settings");
    same_as_full_import(&mut state, &dir, "reset the camera's transform stack");
    assert_eq!(undo(&mut state), "settings");
    same_as_full_import(&mut state, &dir, "undo the reset");
}

/// A camera that carries an imported prim moves it: `full`.
#[test]
fn moving_a_camera_with_prims_below_it_is_full() {
    let carried = r#"
        def Mesh "Badge"
        {
            uniform token subdivisionScheme = "none"
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(0, 0, -2), (0.2, 0, -2), (0, 0.2, -2)]
        }"#;
    let (mut state, dir) = session_on("camera_carries", &mesh_stage("", carried));
    move_camera(&mut state, &dir, "carried", "/World/Cam", "full");
    // An exposure still is not a transform.
    let exposure = r#"over "World" { over "Cam" { float exposure = 1 } }"#;
    assert_eq!(author(&mut state, exposure), "settings");

    // A volume region carried by the camera moves with it too.
    let fog = r#"
        def Cube "Fog"
        {
            double size = 1
            token crust:volume:type = "homogeneous"
            color3f crust:volume:sigmaS = (0.4, 0.4, 0.4)
            color3f crust:volume:sigmaA = (0.05, 0.05, 0.05)
            double3 xformOp:translate = (0, 0, -3)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }"#;
    let (mut state, dir) = session_on("camera_carries_fog", &mesh_stage("", fog));
    move_camera(&mut state, &dir, "carried fog", "/World/Cam", "full");
    same_as_full_import(&mut state, &dir, "carried fog");
}

/// Tessellation diced for another camera stands when the render camera
/// moves; moving the dicing camera, or retargeting it, re-imports, and so
/// does moving a render camera that dices.
#[test]
fn a_dicing_camera_keeps_render_camera_moves_in_place() {
    let s = samples();
    let s = s.display();
    let dicer = r#"
    def Camera "Dicer"
    {
        float focalLength = 24
        double3 xformOp:translate = (0, 0.5, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }"#;
    let adaptive = |dicing: &str| {
        format!(
            r#"#usda 1.0
(
    subLayers = [@{s}/subdivision_adaptive.usda@]
    defaultPrim = "World"
    upAxis = "Y"
)

over "Render" {{ over "settings" {{
    int2 resolution = (48, 27)
    {dicing}
}} }}

over "World" {{ {dicer} }}
"#
        )
    };
    // No dicing camera: the render camera dices, and moving it moves geometry.
    let (mut state, dir) = session_on("dicing_none", &adaptive(""));
    move_camera(&mut state, &dir, "adaptive", "/World/Cam", "full");

    let dicing = "rel crust:dicingCamera = </World/Dicer>";
    let (mut state, dir) = session_on("dicing_adaptive", &adaptive(dicing));
    assert!(!has_warning(&mut state, "subdiv.adaptive_needs_camera"));
    move_camera(&mut state, &dir, "adaptive diced", "/World/Cam", "settings");
    let moved = r#"over "World" { over "Dicer" { double3 xformOp:translate = (0, 0.5, 4) } }"#;
    assert_eq!(
        author(&mut state, moved),
        "full",
        "moving the dicing camera"
    );
    let retarget = r#"over "Render" { over "settings" { rel crust:dicingCamera = </World/Cam> } }"#;
    assert_eq!(
        author(&mut state, retarget),
        "full",
        "retargeting the dicing camera"
    );

    // Implicit shapes sized on screen, diced for another camera.
    let on_screen = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        {dicing}
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 2
    }}
}}

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 24
        double3 xformOp:translate = (0, 0, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
    {dicer}

    def DistantLight "Sun"
    {{
        float inputs:intensity = 3
    }}

    def Cylinder "Post"
    {{
        double radius = 1
        double height = 2
    }}
}}
"#
    );
    let (mut state, dir) = session_on("dicing_on_screen", &on_screen);
    move_camera(
        &mut state,
        &dir,
        "on-screen diced",
        "/World/Cam",
        "settings",
    );
}

/// A stage authoring `metadata` whose camera has a lens (`fStop = 2`), for
/// the depth-of-field edits.
fn lens_stage(metadata: &str) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
    {metadata}
)

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 2
    }}
}}

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 50
        float fStop = 2
        float focusDistance = 6
        double3 xformOp:translate = (0, 0, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}

    def DistantLight "Sun"
    {{
        float inputs:intensity = 3
    }}

    def Sphere "Near"
    {{
        double radius = 0.5
        double3 xformOp:translate = (-1, 0, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}

    def Sphere "Far"
    {{
        double radius = 2
        double3 xformOp:translate = (1, 0, -6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
}}
"#
    )
}

fn has_warning(state: &mut State, code: &str) -> bool {
    state
        .session()
        .expect("open")
        .scene
        .report
        .warnings
        .iter()
        .any(|w| w.code.as_str() == code)
}

/// `disableDepthOfField` is a settings edit: in place, it turns the lens
/// into a pinhole as a full import of the same layer does, and its undo
/// brings the lens back the same way. On a stage whose `metersPerUnit` the
/// lens refuses, the refusal goes with the lens, as a full import drops it.
#[test]
fn disabling_depth_of_field_edits_as_a_full_import() {
    let disable = r#"over "Render" { over "settings" { uniform bool disableDepthOfField = 1 } }"#;

    let (mut state, dir) = session_on("dof_in_place", &lens_stage("metersPerUnit = 0.01"));
    assert!(state.session().unwrap().scene.settings.depth_of_field());
    assert_eq!(author(&mut state, disable), "settings");
    assert!(!state.session().unwrap().scene.settings.depth_of_field());
    same_as_full_import(&mut state, &dir, "disable depth of field");
    assert_eq!(undo(&mut state), "settings");
    same_as_full_import(&mut state, &dir, "undo disable depth of field");

    let refused = "camera.invalid_meters_per_unit";
    let (mut state, dir) = session_on("dof_bad_unit", &lens_stage("metersPerUnit = 0"));
    assert!(has_warning(&mut state, refused));
    author(&mut state, disable);
    assert!(!has_warning(&mut state, refused), "no stale refusal");
    same_as_full_import(&mut state, &dir, "disable depth of field, bad unit");
    undo(&mut state);
    assert!(has_warning(&mut state, refused), "the lens is back");
    same_as_full_import(&mut state, &dir, "undo disable depth of field, bad unit");
}

/// A session over a metre-scale input with a lens renders as `crust render`
/// of the input does, in a session render and in `render_final`: the
/// override layer carries the input's `metersPerUnit`, so the lens is the
/// input's 25 mm, not one read in centimetres.
#[test]
fn a_session_renders_the_inputs_lens() {
    let stage = lens_stage("metersPerUnit = 1").replace(
        "int crust:maxDepth = 2",
        "int crust:maxDepth = 2\n        int crust:samplesPerPixel = 16",
    );
    let (mut state, dir) = session_on("lens_units", &stage);
    let open = state.session().expect("open");
    let (input, output) = (open.input.clone(), open.output.clone());
    let region = [0, 0, 48, 32];

    let (job, _) = open.start_render(Some(region), Some(16)).expect("started");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(job.wait(Duration::from_secs(3600)));
    let finished = job.finished().expect("finished");
    let session = planes(&finished, &job.aovs, "session");
    let [(None, session)] = &session[..] else {
        panic!("one beauty: {:?}", session.len())
    };

    let cli_dir = dir.join("cli");
    std::fs::create_dir_all(&cli_dir).unwrap();
    cli_render(&input, region, &cli_dir);
    let cli = crust_assets::read_exr_planes(&cli_dir.join("output.exr")).expect("the CLI's file");
    let report = crust_core::compare::compare(&cli, session);
    assert!(report.identical, "session render: {}", report.to_text());

    state
        .session()
        .unwrap()
        .render_final()
        .expect("render_final");
    let last = crust_assets::read_exr_planes(&output.with_extension("exr")).expect("render_final");
    let report = crust_core::compare::compare(&cli, &last);
    assert!(report.identical, "render_final: {}", report.to_text());
}

/// A session over a Z-up input is Z up on its own stage, which reads its
/// metadata from the override layer.
#[test]
fn a_session_over_a_z_up_input_is_z_up() {
    let stage = lens_stage("").replace("upAxis = \"Y\"", "upAxis = \"Z\"");
    let (mut state, _dir) = session_on("z_up_units", &stage);
    let open = state.session().expect("open");
    let axis = openusd_schemas::geom::StageMetadata::up_axis(&open.stage).expect("readable");
    assert_eq!(axis.as_ref().map(|t| t.as_str()), Some("Z"));
}

/// Resuming a layer written before sessions copied the input's metrics adds
/// the ones it lacks, keeping its opinions and any value it authors.
#[test]
fn resuming_adds_the_inputs_metrics_the_layer_lacks() {
    let dir = work_dir("resume_metrics");
    let input = dir.join("input.usda");
    std::fs::write(
        &input,
        lens_stage("metersPerUnit = 1").replace("upAxis = \"Y\"", "upAxis = \"Z\""),
    )
    .unwrap();
    // As the old `new_layer_text` wrote it, with an edit and an up axis of
    // its own.
    let layer = dir.join("edits.usda");
    std::fs::write(
        &layer,
        "#usda 1.0\n(\n    subLayers = [\n        @./input.usda@\n    ]\n    upAxis = \"Y\"\n)\n\n\
         over \"World\"\n{\n    over \"Cam\"\n    {\n        float exposure = 1\n    }\n}\n",
    )
    .unwrap();
    let mut state = State::default();
    let opened = state
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap(), None)
        .expect("resumed");
    assert_eq!(opened["resumed"], true);
    assert_eq!(
        layer_field(&layer, "/", "metersPerUnit"),
        Some(openusd::sdf::Value::Double(1.0))
    );
    assert!(
        matches!(layer_field(&layer, "/", "upAxis"), Some(openusd::sdf::Value::Token(t)) if t.as_str() == "Y"),
        "the layer's own up axis stays"
    );
    assert_eq!(
        layer_field(&layer, "/World/Cam.exposure", "default"),
        Some(openusd::sdf::Value::Float(1.0)),
        "the edit stays"
    );
    let text = std::fs::read_to_string(&layer).unwrap();
    assert!(text.contains("input.usda"), "{text}");
}

/// The shutter motion is measured over was read before the traversal and
/// placed geometry at its ends, so editing it re-imports, and renders as a
/// full import of the same layer would; the camera's exposure still applies
/// in place.
#[test]
fn a_shutter_edit_reimports_and_an_exposure_stays_in_place() {
    let stage = r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
    startTimeCode = 1
    endTimeCode = 10
)

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 2
    }
}

def Xform "World"
{
    def Camera "Cam"
    {
        float focalLength = 24
        double3 xformOp:translate = (0, 0, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def DistantLight "Sun"
    {
        float inputs:intensity = 3
    }

    def Sphere "Mover"
    {
        double radius = 0.5
        double3 xformOp:translate.timeSamples = {
            1: (-1, 0, 0),
            10: (1, 0, 0),
        }
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
"#;
    let (mut state, dir) = session_on("shutter_edit", stage);
    let exposure = r#"over "World" { over "Cam" { float exposure = 1 } }"#;
    assert_eq!(author(&mut state, exposure), "settings");
    same_as_full_import(&mut state, &dir, "shutter exposure");
    let shutter = r#"over "World" { over "Cam" { double shutter:close = 0.5 } }"#;
    assert_eq!(author(&mut state, shutter), "full");
    same_as_full_import(&mut state, &dir, "shutter close");
    assert_eq!(undo(&mut state), "full", "undo the shutter");
    same_as_full_import(&mut state, &dir, "undo shutter close");

    // Over a shutter that opens before the frame, `disableMotionBlur` poses
    // the moving sphere at the frame rather than at shutter open: geometry
    // moves, so the settings edit re-imports.
    let centred = r#"over "World" { over "Cam" {
        double shutter:open = -0.25
        double shutter:close = 0.25
    } }"#;
    assert_eq!(author(&mut state, centred), "full");
    let sharp = r#"over "Render" { over "settings" { uniform bool disableMotionBlur = 1 } }"#;
    assert_eq!(author(&mut state, sharp), "full");
    same_as_full_import(&mut state, &dir, "sharp at the frame");
}

/// A light-linked scene keeps its resolved links through an in-place light
/// edit: each linked light (shadow-linked, light-link excludes, include-only)
/// dims and recolours in place as a full import of the same layer renders it.
/// A light the links took out of the list is not a light to rebuild, and a
/// light after it in the list is still the one an edit names.
#[test]
fn linked_light_edits_stay_in_place() {
    let s = samples();
    let s = s.display();
    let linking = format!(
        r#"#usda 1.0
(
    subLayers = [@{s}/light_linking.usda@]
    defaultPrim = "World"
    upAxis = "Y"
)

over "Render" {{ over "settings" {{ int2 resolution = (48, 27) }} }}
"#
    );
    let (mut state, dir) = session_on("linked", &linking);
    let lights = ["Key", "Fill", "Rim"];
    for name in lights {
        let edit = format!(
            r#"over "World" {{ over "{name}" {{ float inputs:intensity = 5
                color3f inputs:color = (0.2, 0.9, 0.4) }} }}"#
        );
        assert_eq!(author(&mut state, &edit), "lights", "{name}");
        same_as_full_import(&mut state, &dir, &format!("linked {name}"));
    }
    for name in lights.iter().rev() {
        assert_eq!(undo(&mut state), "lights", "undo {name}");
        same_as_full_import(&mut state, &dir, &format!("linked undo {name}"));
    }

    // `Dead` illuminates nothing and leaves the list between two kept lights
    // on either side, so whichever order the traversal meets them in, two
    // kept lights come after it.
    let light = |name: &str, x: f32, link: &str| {
        format!(
            r#"    def SphereLight "{name}"
    {{
        float inputs:radius = 0.2
        float inputs:intensity = 10
        double3 xformOp:translate = ({x}, 3, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        {link}
    }}
"#
        )
    };
    let nothing = "uniform bool collection:lightLink:includeRoot = 0";
    let lights: String = [
        light("K1", -2.0, ""),
        light("K2", -1.0, ""),
        light("Dead", 0.0, nothing),
        light("K3", 1.0, ""),
        light("K4", 2.0, ""),
    ]
    .concat();
    let demoting = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 2
    }}
}}

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 24
        double3 xformOp:translate = (0, 1.5, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
{lights}
    def Mesh "Floor"
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-4, 0, 4), (4, 0, 4), (4, 0, -4), (-4, 0, -4)]
    }}
}}
"#
    );
    let (mut state, dir) = session_on("linked_demoted", &demoting);
    let dead = r#"over "World" { over "Dead" { float inputs:intensity = 3 } }"#;
    assert_eq!(
        author(&mut state, dead),
        "full",
        "a light the links took out"
    );
    for name in ["K1", "K4"] {
        let edit = format!(r#"over "World" {{ over "{name}" {{ float inputs:intensity = 2 }} }}"#);
        assert_eq!(author(&mut state, &edit), "lights", "{name}");
        same_as_full_import(&mut state, &dir, &format!("demoted {name}"));
    }
}

/// A look a payload brings in is read through the session's authoring stage,
/// which leaves payloads unloaded, so its shaders show there only as the
/// edit's typeless `over`s: a shader input still applies in place, checked on
/// the material's own stage, and an input on a prim that is not a shader
/// does not.
#[test]
fn a_shader_behind_a_payload_edits_in_place() {
    let dir = work_dir("payload_look");
    std::fs::write(
        dir.join("looks.usda"),
        r#"#usda 1.0
def Scope "Looks"
{
    def Material "Clay"
    {
        token outputs:surface.connect = </Looks/Clay/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.7, 0.4, 0.3)
            float inputs:roughness = 0.6
            token outputs:surface
        }

        def NodeGraph "Graph"
        {
            float inputs:gain = 1
        }
    }
}
"#,
    )
    .expect("write the look");
    let input = dir.join("input.usda");
    std::fs::write(
        &input,
        r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 2
    }
}

def Xform "World"
{
    def Camera "Cam"
    {
        float focalLength = 24
        double3 xformOp:translate = (0, 1, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def DistantLight "Sun"
    {
        float inputs:intensity = 3
        float3 xformOp:rotateXYZ = (-50, 20, 0)
        uniform token[] xformOpOrder = ["xformOp:rotateXYZ"]
    }

    def Scope "Looks" (
        prepend payload = @looks.usda@</Looks>
    )
    {
    }

    def Sphere "Ball" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        double radius = 1
        double3 xformOp:translate = (0, 1, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
        rel material:binding = </World/Looks/Clay>
    }
}
"#,
    )
    .expect("write the stage");
    let layer = dir.join("edits.usda");
    let mut state = State::default();
    state
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap(), None)
        .expect("opened");
    let roughness = r#"over "World" { over "Looks" { over "Clay" { over "Surface" {
        float inputs:roughness = 0.15 } } } }"#;
    assert_eq!(author(&mut state, roughness), "materials");
    same_as_full_import(&mut state, &dir, "payload roughness");
    assert_eq!(undo(&mut state), "materials", "undo");
    same_as_full_import(&mut state, &dir, "payload undo roughness");
    let graph = r#"over "World" { over "Looks" { over "Clay" { over "Graph" {
        float inputs:gain = 2 } } } }"#;
    assert_eq!(
        author(&mut state, graph),
        "full",
        "a node graph is not a shader"
    );
}

// --- Placement (`session-placement-tools`) ---

use super::edit::PLACE_OP;
use super::place::ScatterArgs;
use crust_core::rt::{Piece, RayMask};
use glam::{DMat4, DVec3};

/// A mesh prim from points and faces, under an Xform `name` translated by
/// `at`, at `indent` levels.
fn mesh_prim(name: &str, at: [f64; 3], points: &[[f64; 3]], faces: &[&[u32]]) -> String {
    let pts: Vec<String> = points
        .iter()
        .map(|p| format!("({}, {}, {})", p[0], p[1], p[2]))
        .collect();
    let counts: Vec<String> = faces.iter().map(|f| f.len().to_string()).collect();
    let idx: Vec<String> = faces
        .iter()
        .flat_map(|f| f.iter().map(|i| i.to_string()))
        .collect();
    format!(
        r#"
    def Xform "{name}"
    {{
        double3 xformOp:translate = ({}, {}, {})
        uniform token[] xformOpOrder = ["xformOp:translate"]
        def Mesh "geo"
        {{
            int[] faceVertexCounts = [{}]
            int[] faceVertexIndices = [{}]
            point3f[] points = [{}]
        }}
    }}
"#,
        at[0],
        at[1],
        at[2],
        counts.join(", "),
        idx.join(", "),
        pts.join(", ")
    )
}

/// A cube of half-side `s` around its prim's origin, as points and faces.
fn cube_mesh(s: f64) -> (Vec<[f64; 3]>, Vec<Vec<u32>>) {
    let points = [
        [-1., -1., -1.],
        [1., -1., -1.],
        [1., 1., -1.],
        [-1., 1., -1.],
        [-1., -1., 1.],
        [1., -1., 1.],
        [1., 1., 1.],
        [-1., 1., 1.],
    ]
    .map(|p: [f64; 3]| p.map(|c| c * s))
    .to_vec();
    let faces = vec![
        vec![0, 3, 2, 1],
        vec![4, 5, 6, 7],
        vec![0, 1, 5, 4],
        vec![2, 3, 7, 6],
        vec![1, 2, 6, 5],
        vec![0, 4, 7, 3],
    ];
    (points, faces)
}

fn cube_prim(name: &str, at: [f64; 3], s: f64) -> String {
    let (points, faces) = cube_mesh(s);
    let faces: Vec<&[u32]> = faces.iter().map(Vec::as_slice).collect();
    mesh_prim(name, at, &points, &faces)
}

/// A flat quad facing up (+Y), from `lo` to `hi` in x and z.
fn plate_prim(name: &str, at: [f64; 3], lo: [f64; 2], hi: [f64; 2]) -> String {
    let points = [
        [lo[0], 0., lo[1]],
        [lo[0], 0., hi[1]],
        [hi[0], 0., hi[1]],
        [hi[0], 0., lo[1]],
    ];
    mesh_prim(name, at, &points, &[&[0, 1, 2, 3]])
}

/// A cube asset layer, its root `name` (for a payload or a prototype file).
fn cube_asset(name: &str, s: f64, ops: &str) -> String {
    let (points, faces) = cube_mesh(s);
    let pts: Vec<String> = points
        .iter()
        .map(|p| format!("({}, {}, {})", p[0], p[1], p[2]))
        .collect();
    let idx: Vec<String> = faces.iter().flatten().map(|i| i.to_string()).collect();
    format!(
        r#"#usda 1.0
(
    defaultPrim = "{name}"
)

def Xform "{name}"
{{
{ops}
    def Mesh "geo"
    {{
        int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
        int[] faceVertexIndices = [{}]
        point3f[] points = [{}]
    }}
}}
"#,
        idx.join(", "),
        pts.join(", ")
    )
}

/// The table, in a sublayer another program may rewrite: a plate at
/// `height`.
fn table_layer(height: f64) -> String {
    format!(
        "#usda 1.0\n\nover \"World\"\n{{{}}}\n",
        plate_prim("table", [0., height, 0.], [-1., -1.], [1., 1.])
    )
}

/// The placement stage (Y up): a floor; a table (in `table.usda`) with a
/// shelf above it; a bumpy terrain; a slope of 30° about X; a flat field of
/// area 50 and steps with a vertical face to scatter over; a payload-placed
/// crate whose ops the payload authors; a native instance; and prims to
/// drop.
fn placement_stage(dir: &Path) -> PathBuf {
    std::fs::write(dir.join("table.usda"), table_layer(1.0)).unwrap();
    let crate_ops = "    double3 xformOp:translate = (-3, 2, 5)\n    float xformOp:rotateY = 30\n    \
                     uniform token[] xformOpOrder = [\"xformOp:translate\", \"xformOp:rotateY\"]";
    std::fs::write(dir.join("crate.usda"), cube_asset("Crate", 0.25, crate_ops)).unwrap();
    std::fs::write(dir.join("stone_a.usda"), cube_asset("StoneA", 0.05, "")).unwrap();
    std::fs::write(dir.join("stone_b.usda"), cube_asset("StoneB", 0.08, "")).unwrap();

    let n = 8;
    let mut terrain = Vec::new();
    for j in 0..=n {
        for i in 0..=n {
            let (x, z) = (4.0 * i as f64 / n as f64, 4.0 * j as f64 / n as f64 - 2.0);
            terrain.push([x, 0.5 + 0.3 * (2.0 * x).sin() * (1.5 * z).cos(), z]);
        }
    }
    let mut quads = Vec::new();
    for j in 0..n {
        for i in 0..n {
            let (k, w) = ((j * (n + 1) + i) as u32, (n + 1) as u32);
            quads.push(vec![k, k + w, k + w + 1, k + 1]);
        }
    }
    let quads: Vec<&[u32]> = quads.iter().map(Vec::as_slice).collect();
    let (s, c) = (30f64.to_radians().sin(), 30f64.to_radians().cos());
    let slope = [
        [-2.0, 2.0 * s, -2.0 * c],
        [-2.0, -2.0 * s, 2.0 * c],
        [2.0, -2.0 * s, 2.0 * c],
        [2.0, 2.0 * s, -2.0 * c],
    ];
    let steps = [
        [0., 0., 0.],
        [0., 0., 1.],
        [1., 0., 1.],
        [1., 0., 0.],
        [1., 1., 0.],
        [1., 1., 1.],
    ];
    let mut world = String::new();
    world += &plate_prim("floor", [0., 0., 0.], [-20., -20.], [20., 20.]);
    world += &plate_prim("shelf", [0., 2., 0.], [-0.5, -0.5], [0.5, 0.5]);
    world += &mesh_prim("terrain", [6., 0., 0.], &terrain, &quads);
    world += &mesh_prim("slope", [-8., 1., 0.], &slope, &[&[0, 1, 2, 3]]);
    world += &plate_prim("field", [0., 0.5, -15.], [0., 0.], [10., 5.]);
    world += &mesh_prim(
        "steps",
        [12., 0.5, -15.],
        &steps,
        &[&[0, 1, 2, 3], &[3, 2, 5, 4]],
    );
    world += &cube_prim("vase", [0., 3.1, 0.], 0.1);
    world += &cube_prim("rock", [8., 3., 0.3], 0.3);
    world += &cube_prim("sunk", [-3., 0., -5.], 0.25);
    world += &cube_prim("box", [-8., 3., 0.], 0.25);
    world += &cube_prim("away", [30., 3., 0.], 0.25);
    world += &cube_prim("pebble", [10., 0.25, -8.], 0.25);
    let stage = format!(
        r#"#usda 1.0
(
    defaultPrim = "World"
    subLayers = [@./table.usda@]
    upAxis = "Y"
)

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        uniform int2 resolution = (48, 32)
        int crust:maxDepth = 3
    }}
}}

class Xform "_Cube"
{{
{}
}}

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = 18
        double3 xformOp:translate = (0, 4, 14)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}

    def DomeLight "Sky"
    {{
        float inputs:intensity = 0.5
    }}

    def Xform "crate" (
        prepend payload = @./crate.usda@
    )
    {{
    }}

    def Xform "inst" (
        instanceable = true
        references = </_Cube>
    )
    {{
        double3 xformOp:translate = (12, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}
{world}}}
"#,
        cube_prim("body", [0., 0., 0.], 0.25)
    );
    let path = dir.join("stage.usda");
    std::fs::write(&path, stage).unwrap();
    path
}

fn placement_session(name: &str) -> (State, PathBuf) {
    let dir = work_dir(name).canonicalize().unwrap();
    let input = placement_stage(&dir);
    let layer = dir.join("placed.usda");
    let mut state = State::default();
    state
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap(), None)
        .expect("opened");
    (state, dir)
}

/// The world-space vertices of `prim`'s geometry in the session's scene.
fn session_vertices(state: &mut State, prim: &str) -> Vec<DVec3> {
    let open = state.session().unwrap();
    let prov = open.scene.provenance().expect("provenance");
    let mut out = Vec::new();
    for ids in prov.geometry_of(prim) {
        open.scene
            .renderer
            .world()
            .visit(ids, RayMask::CAMERA, |_, piece| {
                if let Piece::Triangle(vs) = piece {
                    out.extend(vs.map(|v| v.as_dvec3()));
                }
            });
    }
    assert!(!out.is_empty(), "{prim} has no geometry");
    out
}

fn lowest(vs: &[DVec3], up: DVec3) -> f64 {
    vs.iter().map(|v| v.dot(up)).fold(f64::INFINITY, f64::min)
}

fn drop(state: &mut State, prims: &[&str], target: Option<&[&str]>, align: bool) -> Value {
    let prims: Vec<String> = prims.iter().map(|s| s.to_string()).collect();
    let target: Option<Vec<String>> = target.map(|t| t.iter().map(|s| s.to_string()).collect());
    state
        .session()
        .unwrap()
        .drop_to_surface(&prims, target.as_deref(), align)
        .unwrap_or_else(|e| panic!("drop: {e}"))
}

/// A field of the saved override layer.
fn layer_field(layer: &Path, path: &str, field: &str) -> Option<openusd::sdf::Value> {
    use openusd::sdf::AbstractData;
    let data = openusd::usda::parse(&std::fs::read_to_string(layer).unwrap()).unwrap();
    data.try_field(&openusd::sdf::Path::new(path).unwrap(), field)
        .unwrap()
        .map(|v| v.as_ref().clone())
}

#[test]
fn drops_rest_on_the_rendered_surface_and_undo_restores_them() {
    let (mut state, _dir) = placement_session("drop_basic");
    let layer = state.session().unwrap().output.clone();
    let before_bytes = std::fs::read(&layer).unwrap();
    let before_render = render_planes(&mut state);
    let vase = session_vertices(&mut state, "/World/vase");

    // Vase onto the shelf, the highest surface beneath it; the sunk cube up
    // onto the floor; the cup-like vase passes nothing else.
    let result = drop(&mut state, &["/World/vase", "/World/sunk"], None, false);
    assert_eq!(result["applied"], "full", "{result:#}");
    assert_eq!(result["undo_depth"], 1);
    assert_eq!(result["prims"][0]["status"], "placed", "{result:#}");
    let after = session_vertices(&mut state, "/World/vase");
    assert!((lowest(&after, DVec3::Y) - 2.0).abs() < 1e-4, "{after:?}");
    for (a, b) in vase.iter().zip(&after) {
        assert!(
            (a.x - b.x).abs() < 1e-5 && (a.z - b.z).abs() < 1e-5,
            "moved across"
        );
    }
    let sunk = session_vertices(&mut state, "/World/sunk");
    assert!(
        lowest(&sunk, DVec3::Y).abs() < 1e-4,
        "raised onto the floor"
    );
    // One op, first, and the prim's own ops after it.
    let order = layer_field(&layer, "/World/vase.xformOpOrder", "default");
    let Some(openusd::sdf::Value::TokenVec(order)) = order else {
        panic!("{order:?}")
    };
    let order: Vec<&str> = order.iter().map(|t| t.as_str()).collect();
    assert_eq!(order, [PLACE_OP, "xformOp:translate"]);

    // Undo: the layer's bytes, and the render, as before.
    state.session().unwrap().undo().expect("undone");
    assert_eq!(std::fs::read(&layer).unwrap(), before_bytes);
    let undone = render_planes(&mut state);
    for ((p, a), (_, b)) in before_render.iter().zip(&undone) {
        let report = crust_core::compare::compare(a, b);
        assert!(report.identical, "{p:?}: {}", report.to_text());
    }
}

#[test]
fn a_drop_lands_only_on_its_target_and_on_uneven_ground() {
    let (mut state, _dir) = placement_session("drop_target");
    drop(&mut state, &["/World/vase"], Some(&["/World/table"]), false);
    let vase = session_vertices(&mut state, "/World/vase");
    assert!(
        (lowest(&vase, DVec3::Y) - 1.0).abs() < 1e-4,
        "through the shelf"
    );

    drop(&mut state, &["/World/rock"], None, false);
    let rock = session_vertices(&mut state, "/World/rock");
    let open = state.session().unwrap();
    let prov = open.scene.provenance().unwrap();
    let terrain = prov.geometry_of("/World/terrain");
    let world = open.scene.renderer.world();
    let gaps: Vec<f64> = rock
        .iter()
        .filter_map(|v| {
            let origin = glam::Vec3A::new(v.x as f32, 50.0, v.z as f32);
            let accept = |id: u32| terrain.iter().any(|r| r.contains(&id));
            let below = crust_core::place::first_hit(world, origin, -glam::Vec3A::Y, &accept)?;
            Some(v.y - below.point.y as f64)
        })
        .collect();
    assert!(gaps.iter().all(|g| *g > -1e-4), "{gaps:?}");
    assert!(
        gaps.iter().cloned().fold(f64::INFINITY, f64::min) < 1e-4,
        "{gaps:?}"
    );
}

#[test]
fn a_drop_reports_what_it_could_not_place_and_refuses_instance_proxies() {
    let (mut state, _dir) = placement_session("drop_report");
    let layer = state.session().unwrap().output.clone();
    let result = drop(&mut state, &["/World/vase", "/World/away"], None, false);
    assert_eq!(result["prims"][0]["status"], "placed");
    assert_eq!(result["prims"][1]["status"], "nothing below", "{result:#}");
    assert!(
        layer_field(&layer, "/World/away", "specifier").is_none(),
        "no opinion on the prim that stayed"
    );

    let bytes = std::fs::read(&layer).unwrap();
    let e = state
        .session()
        .unwrap()
        .drop_to_surface(&["/World/inst/body".into()], None, false)
        .expect_err("inside an instance");
    assert!(e.contains("/World/inst/body"), "{e}");
    assert_eq!(std::fs::read(&layer).unwrap(), bytes, "nothing authored");
}

/// The crate's payload authors its ops: they are kept after the placement
/// op, which a second drop folds into, and its rotation stays.
#[test]
fn a_drop_keeps_the_ops_a_payload_authors() {
    let (mut state, dir) = placement_session("drop_payload");
    let layer = state.session().unwrap().output.clone();
    let frame = |state: &mut State| {
        let open = state.session().unwrap();
        open.scene
            .provenance()
            .unwrap()
            .prim_frames(&open.output, &["/World/crate".into()], PLACE_OP)
            .unwrap()
            .remove(0)
    };
    let before = frame(&mut state).local_to_world;
    drop(&mut state, &["/World/crate"], None, false);
    drop(&mut state, &["/World/crate"], None, false);
    let after = frame(&mut state);
    assert_eq!(
        after.xform_op_order,
        [PLACE_OP, "xformOp:translate", "xformOp:rotateY"]
    );
    let lin = |m: DMat4| glam::DMat3::from_mat4(m);
    assert!(
        lin(after.local_to_world).abs_diff_eq(lin(before), 1e-5),
        "rotation unchanged"
    );
    let crate_ = session_vertices(&mut state, "/World/crate");
    assert!(lowest(&crate_, DVec3::Y).abs() < 1e-4);
    let text = std::fs::read_to_string(&layer).unwrap();
    assert_eq!(
        text.matches("xformOp:transform:crustPlace =").count(),
        1,
        "{text}"
    );
    let _ = dir;
}

#[test]
fn an_aligned_drop_follows_the_slope() {
    let (mut state, _dir) = placement_session("drop_align");
    let result = drop(&mut state, &["/World/box"], None, true);
    let (s, c) = (30f64.to_radians().sin(), 30f64.to_radians().cos());
    let n = DVec3::new(0.0, c, s);
    let normal: Vec<f64> = serde_json::from_value(result["prims"][0]["normal"].clone()).unwrap();
    assert!(
        DVec3::from_slice(&normal).abs_diff_eq(n, 1e-4),
        "{normal:?}"
    );
    let open = state.session().unwrap();
    let frame = open
        .scene
        .provenance()
        .unwrap()
        .prim_frames(&open.output, &["/World/box".into()], PLACE_OP)
        .unwrap()
        .remove(0);
    let up = frame.local_to_world.transform_vector3(DVec3::Y).normalize();
    assert!(up.abs_diff_eq(n, 1e-4), "{up:?}");
    let centre = DVec3::new(-8.0, 1.0, 0.0);
    let heights: Vec<f64> = session_vertices(&mut state, "/World/box")
        .iter()
        .map(|v| (*v - centre).dot(n))
        .collect();
    let min = heights.iter().cloned().fold(f64::INFINITY, f64::min);
    assert!(min.abs() < 1e-4, "{heights:?}");
}

#[test]
fn placement_reads_the_sources_as_they_are_on_disk() {
    let (mut state, dir) = placement_session("drop_rewritten");
    rewrite(&dir.join("table.usda"), &table_layer(1.5));
    let result = drop(&mut state, &["/World/vase"], Some(&["/World/table"]), false);
    assert_eq!(changed(&result), [dir.join("table.usda").to_str().unwrap()]);
    let vase = session_vertices(&mut state, "/World/vase");
    assert!(
        (lowest(&vase, DVec3::Y) - 1.5).abs() < 1e-4,
        "the rewritten table's height"
    );
}

#[test]
fn a_z_up_stage_drops_along_z() {
    let stage = format!(
        r#"#usda 1.0
(
    upAxis = "Z"
)

def Xform "World"
{{{}{}}}
"#,
        mesh_prim(
            "floor",
            [0., 0., 0.],
            &[[-5., -5., 0.], [5., -5., 0.], [5., 5., 0.], [-5., 5., 0.]],
            &[&[0, 1, 2, 3]]
        ),
        cube_prim("cube", [1., 2., 3.], 0.25)
    );
    let (mut state, _dir) = session_on("drop_z_up", &stage);
    let result = drop(&mut state, &["/World/cube"], None, false);
    let moved: Vec<f64> =
        serde_json::from_value(result["prims"][0]["origin_moved_by"].clone()).unwrap();
    assert!(moved[0].abs() < 1e-9 && moved[1].abs() < 1e-9, "{moved:?}");
    assert!((moved[2] + 2.75).abs() < 1e-4, "{moved:?}");
    let cube = session_vertices(&mut state, "/World/cube");
    assert!(lowest(&cube, DVec3::Z).abs() < 1e-4);
}

fn scatter_args(target: &str, prototypes: &[&str], path: &str, seed: u32) -> ScatterArgs {
    ScatterArgs {
        target: vec![target.into()],
        prototypes: prototypes.iter().map(|s| s.to_string()).collect(),
        path: path.into(),
        seed,
        count: Some(500),
        density: None,
        max_slope: None,
        random_rotation: true,
        scale: Some([0.8, 1.2]),
    }
}

fn scatter(state: &mut State, args: &ScatterArgs) -> Value {
    state
        .session()
        .unwrap()
        .scatter_on_surface(args)
        .unwrap_or_else(|e| panic!("scatter: {e}"))
}

#[test]
fn a_scatter_authors_an_instancer_on_the_surface_that_renders_as_imported() {
    let (mut state, dir) = placement_session("scatter_stones");
    let layer = state.session().unwrap().output.clone();
    let args = scatter_args(
        "/World/field",
        &["stone_a.usda", "stone_b.usda"],
        "/World/stones",
        7,
    );
    let result = scatter(&mut state, &args);
    assert_eq!(result["count"], 500, "{result:#}");
    assert_eq!(result["applied"], "full");
    let Some(openusd::sdf::Value::Vec3fVec(positions)) =
        layer_field(&layer, "/World/stones.positions", "default")
    else {
        panic!("positions")
    };
    assert_eq!(positions.len(), 500);
    for p in &positions {
        assert!((p.y - 0.5).abs() < 1e-4, "on the field: {p:?}");
        assert!((-1e-4..=10.0001).contains(&p.x) && (-15.0001..=-9.9999).contains(&p.z));
    }
    let Some(openusd::sdf::Value::IntVec(indices)) =
        layer_field(&layer, "/World/stones.protoIndices", "default")
    else {
        panic!("protoIndices")
    };
    assert!(indices.contains(&0) && indices.contains(&1));
    same_as_full_import(&mut state, &dir, "scatter");

    // The same seed, after an undo, authors the same bytes; another differs.
    let first = std::fs::read(&layer).unwrap();
    state.session().unwrap().undo().expect("undone");
    scatter(&mut state, &args);
    assert_eq!(std::fs::read(&layer).unwrap(), first, "byte-identical");
    state.session().unwrap().undo().expect("undone");
    scatter(
        &mut state,
        &ScatterArgs {
            seed: 8,
            ..scatter_args(
                "/World/field",
                &["stone_a.usda", "stone_b.usda"],
                "/World/stones",
                8,
            )
        },
    );
    let Some(openusd::sdf::Value::Vec3fVec(other)) =
        layer_field(&layer, "/World/stones.positions", "default")
    else {
        panic!("positions")
    };
    assert_ne!(other, positions);

    // The path now names a prim: refused, nothing authored.
    let bytes = std::fs::read(&layer).unwrap();
    let e = state
        .session()
        .unwrap()
        .scatter_on_surface(&args)
        .expect_err("an existing path");
    assert!(e.contains("/World/stones"), "{e}");
    assert_eq!(std::fs::read(&layer).unwrap(), bytes);
}

#[test]
fn a_scatter_takes_a_density_a_slope_and_a_prototype_from_the_stage() {
    let (mut state, _dir) = placement_session("scatter_options");
    let layer = state.session().unwrap().output.clone();
    let mut by_density = scatter_args("/World/field", &["/World/pebble"], "/World/dense", 1);
    by_density.count = None;
    by_density.density = Some(2.0);
    let result = scatter(&mut state, &by_density);
    assert_eq!(result["count"], 100, "{result:#}");
    assert!((result["area"].as_f64().unwrap() - 50.0).abs() < 1e-3);

    // The pebble is defined at (10, 0.25, -8): each instance places its
    // geometry at the instance, not 10 units away, and the pebble itself
    // still renders where it was.
    let Some(openusd::sdf::Value::Vec3fVec(positions)) =
        layer_field(&layer, "/World/dense.positions", "default")
    else {
        panic!("positions")
    };
    let instanced = session_vertices(&mut state, "/World/dense");
    for v in &instanced {
        let near = positions.iter().any(|p| {
            DVec3::new(p.x as f64, p.y as f64, p.z as f64).distance(*v)
                < 0.25 * 1.2 * 3f64.sqrt() + 1e-3
        });
        assert!(near, "{v:?} is no instance's");
    }
    let pebble = session_vertices(&mut state, "/World/pebble");
    assert!(
        pebble.iter().all(|v| (v.x - 10.0).abs() < 0.3),
        "the pebble stays"
    );

    let mut steep = scatter_args("/World/steps", &["stone_a.usda"], "/World/flat", 3);
    steep.max_slope = Some(30.0);
    scatter(&mut state, &steep);
    let Some(openusd::sdf::Value::Vec3fVec(flat)) =
        layer_field(&layer, "/World/flat.positions", "default")
    else {
        panic!("positions")
    };
    assert!(
        flat.iter().all(|p| (p.y - 0.5).abs() < 1e-4),
        "none on the vertical face"
    );
}

#[test]
fn a_drop_refuses_a_prim_with_its_ancestor() {
    let (mut state, _dir) = placement_session("drop_overlap");
    let layer = state.session().unwrap().output.clone();
    let bytes = std::fs::read(&layer).unwrap();
    let e = state
        .session()
        .unwrap()
        .drop_to_surface(
            &["/World/vase".into(), "/World/vase/geo".into()],
            None,
            false,
        )
        .expect_err("a prim and its ancestor");
    assert!(
        e.contains("/World/vase/geo") && e.contains("/World/vase"),
        "{e}"
    );
    assert_eq!(std::fs::read(&layer).unwrap(), bytes, "nothing authored");
}

#[test]
fn a_scatter_takes_usd_prototypes_only_and_absolute_ones_too() {
    let (mut state, dir) = placement_session("scatter_prototypes");
    let layer = state.session().unwrap().output.clone();
    std::fs::write(dir.join("rock.obj"), "v 0 0 0\n").unwrap();
    let bytes = std::fs::read(&layer).unwrap();
    let e = state
        .session()
        .unwrap()
        .scatter_on_surface(&scatter_args(
            "/World/field",
            &["rock.obj"],
            "/World/objs",
            1,
        ))
        .expect_err("not a USD file");
    assert!(
        e.contains("rock.obj") && e.contains("not a USD file"),
        "{e}"
    );
    assert_eq!(std::fs::read(&layer).unwrap(), bytes, "nothing authored");

    // An absolute path to a file is an asset, not a prim path.
    let absolute = dir.join("stone_a.usda");
    let absolute = absolute.to_str().unwrap();
    let result = scatter(
        &mut state,
        &scatter_args("/World/field", &[absolute], "/World/abs", 2),
    );
    assert_eq!(result["count"], 500, "{result:#}");
    assert_eq!(result["prototypes"][0], "/World/abs/Prototypes/stone_a");
}

#[test]
fn a_scatter_under_an_instance_is_refused() {
    let (mut state, _dir) = placement_session("scatter_instance");
    let layer = state.session().unwrap().output.clone();
    let bytes = std::fs::read(&layer).unwrap();
    for path in ["/World/inst/scatter", "/World/inst/body/scatter"] {
        let e = state
            .session()
            .unwrap()
            .scatter_on_surface(&scatter_args("/World/field", &["stone_a.usda"], path, 1))
            .expect_err("under an instance");
        assert!(e.contains("/World/inst"), "{path}: {e}");
    }
    assert_eq!(std::fs::read(&layer).unwrap(), bytes, "nothing authored");
}

/// A groom whose MaterialX material is authored inline, and whose curves
/// author a per-strand colour the material does not read yet.
fn groom_stage() -> String {
    r#"#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Y"
)

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/Cam>
        uniform int2 resolution = (40, 24)
        int crust:maxDepth = 3
    }
}

def Xform "World"
{
    def Camera "Cam"
    {
        float focalLength = 30
        double3 xformOp:translate = (0, 0, 6)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def DomeLight "Sky"
    {
        float inputs:intensity = 1
    }

    def BasisCurves "Groom" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        uniform token type = "linear"
        int[] curveVertexCounts = [2, 2, 2]
        point3f[] points = [(-2, -1, 0), (2, -1, 0), (-2, 0, 0), (2, 0, 0), (-2, 1, 0), (2, 1, 0)]
        float[] widths = [0.4] (interpolation = "constant")
        color3f[] primvars:hairColor = [(1, 0, 0), (0, 1, 0), (0, 0, 1)] (
            interpolation = "uniform"
        )
        rel material:binding = </World/Looks/Hair>
    }

    def Scope "Looks"
    {
        def Material "Hair"
        {
            token outputs:mtlx:surface.connect = </World/Looks/Hair/Surface.outputs:out>

            def Shader "Surface"
            {
                uniform token info:id = "ND_standard_surface_surfaceshader_100"
                color3f inputs:base_color = (0.8, 0.6, 0.3)
                token outputs:out
            }

            def Shader "Strand"
            {
                uniform token info:id = "ND_geompropvalue_color3"
                string inputs:geomprop = "hairColor"
                color3f outputs:out
            }
        }
    }
}
"#
    .to_owned()
}

/// A curve material that keeps reading what it read is swapped in place;
/// one that starts reading a primvar needs the strand table its curves were
/// imported without, so the edit is `full`. Either way, a render after it is
/// a full import's of the same layer, bit for bit.
#[test]
fn a_curve_material_reading_a_new_primvar_reimports() {
    let (mut state, dir) = session_on("groom_primvar", &groom_stage());
    assert_eq!(
        author(
            &mut state,
            r#"over "World" { over "Looks" { over "Hair" { over "Surface" {
                color3f inputs:base_color = (0.2, 0.4, 0.9) } } } }"#,
        ),
        "materials"
    );
    same_as_full_import(&mut state, &dir, "groom base colour");
    assert_eq!(
        author(
            &mut state,
            r#"over "World" { over "Looks" { over "Hair" { over "Surface" {
                color3f inputs:base_color.connect = </World/Looks/Hair/Strand.outputs:out> } } } }"#,
        ),
        "full"
    );
    same_as_full_import(&mut state, &dir, "groom reads its primvar");
    assert_eq!(undo(&mut state), "full");
    same_as_full_import(&mut state, &dir, "groom primvar undone");
}

/// A stage with a mesh authoring `primvars:rest`, whose settings render the
/// product without a primvar var.
fn primvar_stage() -> String {
    r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{
    def Camera "Cam"
    {
        double3 xformOp:translate = (0, 0, 8)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def DistantLight "Sun"
    {
        float inputs:intensity = 3
    }
    def Mesh "M"
    {
        uniform token subdivisionScheme = "none"
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)]
        point3f[] primvars:rest = [(-1, -1, 0), (1, -1, 0), (1, 1, 0), (-1, 1, 0)] (
            interpolation = "vertex"
        )
    }
}

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/Cam>
        uniform int2 resolution = (32, 24)
        int crust:maxDepth = 2
        rel products = [</Render/plain>]
    }
    def RenderProduct "plain"
    {
        token productName = "plain.exr"
        rel orderedVars = [</Render/C>]
    }
    def RenderProduct "withRest"
    {
        token productName = "rest.exr"
        rel orderedVars = [</Render/C>, </Render/Pref>]
    }
    def RenderVar "C"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "color"
    }
    def RenderVar "Pref"
    {
        uniform token dataType = "point3f"
        uniform string sourceName = "rest"
        uniform token sourceType = "primvar"
    }
}
"#
    .to_owned()
}

/// Retargeting `products` to one with a primvar var asks the geometry for a
/// primvar it was read without, so the edit is `full`, and its render — the
/// `Pref` channel included — is a full import's of the same layer, bit for
/// bit; so is its undo.
#[test]
fn a_product_asking_for_a_new_primvar_reimports() {
    let (mut state, dir) = session_on("primvar_products", &primvar_stage());
    assert_eq!(
        author(
            &mut state,
            r#"over "Render" { over "settings" { rel products = [</Render/withRest>] } }"#,
        ),
        "full"
    );
    same_as_full_import(&mut state, &dir, "products with Pref");
    assert_eq!(undo(&mut state), "full");
    same_as_full_import(&mut state, &dir, "products with Pref undone");
}

/// The `mcp-session` spec's "A session renders one pass": a session on
/// `/Render/Passes/char` renders what `crust render --pass` of its layer
/// renders, an edit to the pass's `matte` re-imports and holds the new target
/// out, and a pass that is not one fails the call.
#[test]
fn a_session_renders_its_pass() {
    let dir = work_dir("pass");
    // The sample, small and quick.
    let input = dir.join("shot.usda");
    let text = std::fs::read_to_string(samples().join("render_passes.usda"))
        .unwrap()
        .replace("int2 resolution = (160, 90)", "int2 resolution = (32, 18)")
        .replace(
            "int crust:samplesPerPixel = 64",
            "int crust:samplesPerPixel = 4",
        );
    std::fs::write(&input, text).unwrap();
    let layer = dir.join("lookdev.usda");
    let mut state = State::default();

    let e = state
        .open_session(
            input.to_str().unwrap(),
            layer.to_str().unwrap(),
            Some("/World"),
        )
        .expect_err("not a pass");
    assert!(e.contains("/World") && e.contains("RenderPass"), "{e}");
    assert!(!layer.exists(), "a refused session leaves no layer");

    let opened = state
        .open_session(
            input.to_str().unwrap(),
            layer.to_str().unwrap(),
            Some("/Render/Passes/char"),
        )
        .expect("opened");
    assert_eq!(opened["pass"], "/Render/Passes/char");
    let pass = |state: &mut State| state.session().unwrap().scene.report.pass.clone().unwrap();
    assert_eq!(pass(&mut state).holdouts, 3);

    // An edit applied in place keeps the pass in the check report.
    let applied = author(
        &mut state,
        r#"over "World" { over "Key" { float inputs:intensity = 50 } }"#,
    );
    assert_eq!(applied, "lights", "a light input applies in place");
    assert_eq!(pass(&mut state).holdouts, 3, "still the char pass");

    state
        .session()
        .unwrap()
        .render_final()
        .expect("render_final");
    let session = crust_assets::read_exr_planes(&dir.join("char.exr")).expect("char.exr");
    let cli_dir = dir.join("cli");
    std::fs::create_dir_all(&cli_dir).unwrap();
    let Cli {
        command: Command::Render(args),
        ..
    } = Cli::try_parse_from([
        "crust",
        "render",
        "-i",
        layer.to_str().unwrap(),
        "--pass",
        "/Render/Passes/char",
    ])
    .expect("render arguments")
    else {
        unreachable!("parsed as render")
    };
    let control = RenderControl::without_snapshots();
    let beauty = cli_dir.join("output.exr");
    let run = RenderRun {
        control: &control,
        progress_bar: false,
        rendering: &|| {},
        rendered: &|| false,
        anchor: Some(&cli_dir),
        beauty: Some(beauty.to_str().unwrap()),
    };
    render_and_write(&args, &run).expect("the CLI renders");
    let cli = crust_assets::read_exr_planes(&cli_dir.join("char.exr")).expect("the CLI's char.exr");
    let report = crust_core::compare::compare(&cli, &session);
    assert!(report.identical, "render_final: {}", report.to_text());

    let applied = author(
        &mut state,
        r#"over "Render" { over "Passes" { over "char" {
            prepend rel collection:matte:includes = </World/Chars/Hero> } } }"#,
    );
    assert_eq!(applied, "full", "a collection edit re-imports");
    assert_eq!(pass(&mut state).holdouts, 4, "the hero is held out too");
}

/// The latency of a render camera move, in place against `full`, measured as
/// the `mcp-session` design record reports it: two sessions on the Cornell box
/// at 640×480, the second without kept assets (`CRUST_SCENE_DELTAS=0`'s
/// session), their translates interleaved so load lands on both. Prints each
/// side's `import_s` min / mean. A measurement, not a check:
/// `cargo test --release -p crust-render --bin crust camera_move_latency -- --ignored --nocapture`.
#[test]
#[ignore]
fn camera_move_latency() {
    let s = samples();
    let stage = format!(
        r#"#usda 1.0
(
    subLayers = [@{}/cornellbox.usda@]
)

def Scope "Render" {{ def RenderSettings "settings" {{ uniform int2 resolution = (640, 480) }} }}
"#,
        s.display()
    );
    let (mut in_place, _) = session_on("latency_in_place", &stage);
    let (mut full, _) = session_on("latency_full", &stage);
    full.session().unwrap().assets = None;
    let moves = [
        r#"over "scene" { over "camera1" { double3 xformOp:translate = (0.0074, 1.99, 4.01) } }"#,
        r#"over "scene" { over "camera1" { double3 xformOp:translate = (0.2, 2.1, 4.3) } }"#,
    ];
    let mut times = [Vec::new(), Vec::new()];
    for round in 0..20 {
        for (side, (state, class)) in [(&mut in_place, "settings"), (&mut full, "full")]
            .into_iter()
            .enumerate()
        {
            let result = state
                .session()
                .unwrap()
                .author_usda(moves[round % 2])
                .expect("authored");
            assert_eq!(result["applied"], class);
            times[side].push(result["import_s"].as_f64().unwrap() * 1e3);
        }
    }
    for (side, t) in ["in place", "full"].iter().zip(&times) {
        let min = t.iter().cloned().fold(f64::INFINITY, f64::min);
        let mean = t.iter().sum::<f64>() / t.len() as f64;
        eprintln!(
            "{side}: {min:.2} / {mean:.2} ms (min / mean of {})",
            t.len()
        );
    }
}

/// A session render of a stage with a displayed output (`crust:display`)
/// answers with that output, as `crust render`'s PNG shows it — its
/// snapshots while it runs, and its image once returned — while `probe`
/// still reads the beauty.
#[test]
fn a_session_shows_the_precomps_displayed_output() {
    let dir = work_dir("precomp_display");
    let layer = dir.join("lookdev.usda");
    let mut state = State::default();
    state
        .open_session(
            samples().join("precomp.usda").to_str().unwrap(),
            layer.to_str().unwrap(),
            None,
        )
        .expect("opened");
    let open = state.session().expect("open");
    let (job, _) = open
        .start_render(Some([90, 40, 250, 130]), Some(4))
        .expect("started");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(job.wait(Duration::from_secs(3600)));
    let finished = job.finished().expect("finished");
    let film = finished.film.as_ref().expect("a film");
    let shown = job.shown().expect("an image");
    let displayed =
        crust_core::precomp::displayed(job.aovs.precomp.as_deref(), &finished.buffer, film)
            .expect("a displayed output");
    let pixels = |b: &crust_core::Buffer| {
        let (w, h) = b.size();
        (0..w * h)
            .map(|i| {
                let (r, g, b) = b.get_rgb(i % w, i / w);
                [r.to_bits(), g.to_bits(), b.to_bits()]
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(pixels(&shown), pixels(&displayed));
    assert_ne!(
        pixels(&shown),
        pixels(&finished.buffer),
        "the look, not the beauty"
    );
    // The beauty is still what `probe` reads.
    assert_eq!(pixels(&job.image().unwrap()), pixels(&finished.buffer));
    // The last snapshot is the displayed output of the units it holds: all
    // of them, so the image returned.
    let (_, snapshot) = job.control.snapshot().expect("published");
    assert_eq!(pixels(&snapshot), pixels(&displayed));
    // The answer's image is the displayed output's PNG.
    let png = render::png(&displayed, job.working_space).unwrap();
    let answer = serde_json::to_value(render::answer(&job, json!({}))).unwrap();
    let data = answer["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "image")
        .and_then(|c| c["data"].as_str())
        .expect("an image")
        .to_owned();
    use base64::Engine as _;
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap(),
        png
    );
}
