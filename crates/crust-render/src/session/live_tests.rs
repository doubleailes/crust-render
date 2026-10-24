//! The live session over an owned layer stack (`live-session-host`): a root
//! the host writes, sublayers it owns beside it, a sample beneath them.

use super::live::{Error, LiveSession};
use crate::session::delta::Applied;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `samples/<name>`, absolute, with forward slashes as a USD asset path.
fn sample(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name);
    path.canonicalize()
        .expect("the sample exists")
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

fn work_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_live_session").join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the test directory");
    dir
}

/// Writes `path` and moves its modification time a second past the old one,
/// so a change is seen on a filesystem whose clock is coarse.
fn write(path: &Path, text: &str) {
    let before = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    std::fs::write(path, text).expect("the layer is written");
    if let Some(before) = before {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(before + Duration::from_secs(1)).unwrap();
    }
}

/// A root over `sublayers` (relative names) and the Cornell box, with small
/// settings of its own.
fn write_root(dir: &Path, sublayers: &[&str]) -> PathBuf {
    let mut subs: Vec<String> = sublayers.iter().map(|s| format!("@./{s}@")).collect();
    subs.push(format!("@{}@", sample("cornellbox.usda")));
    let root = dir.join("root.usda");
    write(
        &root,
        &format!(
            "#usda 1.0\n(\n    subLayers = [{}]\n)\n\ndef Scope \"Render\"\n{{\n    \
             def RenderSettings \"settings\"\n    {{\n        int2 resolution = (24, 18)\n        \
             int crust:samplesPerPixel = 4\n    }}\n}}\n",
            subs.join(", ")
        ),
    );
    root
}

fn graph_layer(body: &str) -> String {
    format!("#usda 1.0\n\nover \"scene\"\n{{\n{body}\n}}\n")
}

fn sky(intensity: f32) -> String {
    format!("    over \"Sky\"\n    {{\n        float inputs:intensity = {intensity}\n    }}")
}

/// Waits for the render to complete and returns its final image.
fn converged(session: &LiveSession) -> Vec<f32> {
    let started = Instant::now();
    while !session.converged() {
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the render did not converge"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let snapshot = session.snapshot().expect("a converged render");
    assert!(snapshot.completed);
    snapshot.rgba
}

/// The image a fresh session gives of the same files.
fn fresh(root: &Path, owned: &[PathBuf]) -> Vec<f32> {
    let session = LiveSession::new(Default::default()).expect("a session");
    session.load_owned(root, owned).expect("the layers import");
    converged(&session)
}

#[test]
fn a_light_edit_in_an_owned_sublayer_applies_in_place_as_a_fresh_import() {
    let dir = work_dir("light_in_sublayer");
    let graph = dir.join("graph.usda");
    write(&graph, &graph_layer(&sky(1.0)));
    let root = write_root(&dir, &["graph.usda"]);
    let owned = [graph.clone()];
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, &owned)
        .expect("the layers import");
    assert_eq!(session.applied(), Some(Applied::Full));
    let first = converged(&session);
    assert_eq!(session.render(), 1);

    write(&graph, &graph_layer(&sky(2.5)));
    session.apply().expect("the edit applies");
    assert_eq!(session.applied(), Some(Applied::Lights));
    assert_eq!(session.render(), 2, "an apply restarts the render");
    let edited = converged(&session);
    assert_ne!(first, edited, "the edit shows");
    assert!(
        edited == fresh(&root, &owned),
        "in place renders as a fresh import"
    );
}

#[test]
fn an_edit_in_the_root_and_in_a_sublayer_classify_together() {
    let dir = work_dir("root_and_sublayer");
    let graph = dir.join("graph.usda");
    write(&graph, &graph_layer(&sky(1.0)));
    let root = write_root(&dir, &["graph.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, std::slice::from_ref(&graph))
        .expect("imports");
    converged(&session);
    // A setting in the root, a light in the sublayer: the costlier class.
    let text = std::fs::read_to_string(&root).unwrap();
    write(
        &root,
        &text.replace("crust:samplesPerPixel = 4", "crust:samplesPerPixel = 2"),
    );
    write(&graph, &graph_layer(&sky(3.0)));
    session.apply().expect("applies");
    assert_eq!(session.applied(), Some(Applied::Lights));
    assert!(converged(&session) == fresh(&root, &[graph]));
}

#[test]
fn a_transform_in_an_owned_sublayer_is_full_and_renders_as_a_fresh_import() {
    let dir = work_dir("transform_in_sublayer");
    let graph = dir.join("graph.usda");
    write(&graph, &graph_layer(&sky(1.0)));
    let root = write_root(&dir, &["graph.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, std::slice::from_ref(&graph))
        .expect("imports");
    converged(&session);
    let moved =
        "    over \"pCube2\"\n    {\n        double3 xformOp:translate = (0.5, 0.5, 0)\n    }";
    write(&graph, &graph_layer(moved));
    session.apply().expect("applies");
    assert_eq!(session.applied(), Some(Applied::Full));
    assert!(converged(&session) == fresh(&root, &[graph]));
}

#[test]
fn a_change_to_a_sublayer_the_session_does_not_own_is_full() {
    let dir = work_dir("foreign_sublayer");
    let (graph, other) = (dir.join("graph.usda"), dir.join("other.usda"));
    write(&graph, &graph_layer(&sky(1.0)));
    write(&other, &graph_layer(""));
    let root = write_root(&dir, &["graph.usda", "other.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    session
        .load_owned(&root, std::slice::from_ref(&graph))
        .expect("imports");
    converged(&session);
    // The same light edit, in a layer the session only watches.
    write(&other, &graph_layer(&sky(2.0)));
    session.apply().expect("applies");
    assert_eq!(session.applied(), Some(Applied::Full));
    // The owned layer is weaker here only by order: `graph` is stronger.
    assert!(converged(&session) == fresh(&root, &[graph]));
}

#[test]
fn an_owned_layer_outside_the_root_layer_stack_is_refused() {
    let dir = work_dir("outside_stack");
    let (graph, stray) = (dir.join("graph.usda"), dir.join("stray.usda"));
    write(&graph, &graph_layer(""));
    write(&stray, &graph_layer(""));
    let root = write_root(&dir, &["graph.usda"]);
    let session = LiveSession::new(Default::default()).expect("a session");
    let refused = session.load_owned(&root, &[stray]);
    assert!(
        matches!(refused, Err(Error::InvalidArgument(_))),
        "{refused:?}"
    );
}

#[test]
fn cancel_render_stops_the_render_without_waiting_for_the_session_thread() {
    let dir = work_dir("cancel_render");
    let graph = dir.join("graph.usda");
    write(&graph, &graph_layer(""));
    let root = write_root(&dir, &["graph.usda"]);
    let text = std::fs::read_to_string(&root).unwrap();
    write(
        &root,
        &text
            .replace("(24, 18)", "(160, 120)")
            .replace("crust:samplesPerPixel = 4", "crust:samplesPerPixel = 4096"),
    );
    let session = LiveSession::new(Default::default()).expect("a session");
    session.load_owned(&root, &[graph]).expect("imports");
    let started = Instant::now();
    while session.generation().is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "no first image"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let asked = Instant::now();
    session.cancel_render();
    while session.outcome().is_none() {
        assert!(
            asked.elapsed() < Duration::from_secs(10),
            "the render did not stop"
        );
        std::thread::sleep(Duration::from_micros(200));
    }
    assert_eq!(
        session.outcome(),
        Some(crust_core::RenderOutcome::Cancelled)
    );
    assert!(!session.converged());
}
