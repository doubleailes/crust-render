//! The session against the CLI, in process: what needs the session's own
//! buffers rather than what the protocol carries.

use super::render::planes;
use super::session::State;
use crate::{Cli, Command, RenderRun, render_and_write};
use clap::Parser;
use crust_core::RenderControl;
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
