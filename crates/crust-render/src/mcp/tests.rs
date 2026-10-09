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
