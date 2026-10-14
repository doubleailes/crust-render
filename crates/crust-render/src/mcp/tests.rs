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
        .open_session(input.to_str().unwrap(), copy.to_str().unwrap())
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
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap())
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
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap())
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
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap())
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
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap())
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
        .open_session(input.to_str().unwrap(), layer.to_str().unwrap())
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
