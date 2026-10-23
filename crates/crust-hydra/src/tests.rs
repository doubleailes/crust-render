//! The session through its Rust API: the layers a delegate writes, imported,
//! rendered while polled, changed and cancelled.

use super::*;
use crust_render::session::delta::Applied;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `samples/<name>`, absolute, with forward slashes as a USD asset path.
pub(super) fn sample(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name);
    path.canonicalize()
        .expect("the sample exists")
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
}

/// A fresh session directory for one test.
pub(super) fn session_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_hydra_tests").join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the test directory");
    dir
}

/// Writes `edits.usda` in `dir` as the delegate does: an override layer over
/// `scene`, with `body` after the header.
pub(super) fn write_edits(dir: &Path, scene: &str, body: &str) -> PathBuf {
    let path = dir.join("edits.usda");
    let text = format!("#usda 1.0\n(\n    subLayers = [@{scene}@]\n)\n\n{body}");
    std::fs::write(&path, text).expect("the layer is written");
    path
}

/// Small and quick: the sample's own settings at 32×32 and `spp`.
pub(super) fn settings(spp: u32) -> String {
    format!(
        "over \"Render\"\n{{\n    over \"settings\"\n    {{\n        int2 resolution = (32, 18)\n        \
         int crust:samplesPerPixel = {spp}\n    }}\n}}\n"
    )
}

/// Settings the layer defines itself, at the conventional `/Render/settings`,
/// for a sample that authors none.
pub(super) fn own_settings(spp: u32) -> String {
    format!(
        "def Scope \"Render\"
{{
    def RenderSettings \"settings\"
    {{
                 int2 resolution = (32, 32)
        int crust:samplesPerPixel = {spp}
    }}
}}
"
    )
}

/// Polls until the render converges, keeping every snapshot generation seen.
fn converge(session: &Session) -> Vec<u64> {
    let started = Instant::now();
    let mut generations = Vec::new();
    while !session.converged() {
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the render did not converge"
        );
        if let Ok(s) = session.snapshot() {
            generations.push(s.generation);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    generations.push(session.snapshot().expect("a converged render").generation);
    generations
}

#[test]
fn loads_renders_progressively_and_converges() {
    let dir = session_dir("converges");
    let layer = write_edits(&dir, &sample("aovs.usda"), &settings(16));
    let session = Session::new(&dir).expect("a session");
    session.load(&layer).expect("the layer imports");
    assert_eq!(session.applied(), Some(Applied::Full));
    let generations = converge(&session);
    assert!(
        generations.windows(2).all(|w| w[0] <= w[1]),
        "generations go backwards: {generations:?}"
    );
    let last = session.snapshot().expect("the final image");
    assert_eq!((last.width, last.height), (32, 18));
    assert_eq!(last.rgba.len(), 32 * 18 * 4);
    assert!(last.rgba.iter().any(|&v| v > 0.0), "the image is black");
}

#[test]
fn reads_depth_once_complete() {
    let dir = session_dir("depth");
    let layer = write_edits(&dir, &sample("aovs.usda"), &settings(4));
    let session = Session::new(&dir).expect("a session");
    session.load(&layer).expect("the layer imports");
    converge(&session);
    // `Z` is aovs.usda's RenderVar for crust's `depth` source.
    let depth = session.aov("Z").expect("the depth AOV");
    assert_eq!((depth.width, depth.height, depth.channels), (32, 18, 1));
    assert!(depth.data.iter().any(|d| d.is_finite() && *d > 0.0));
    assert!(matches!(
        session.aov("no_such_var"),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn cancels_and_keeps_the_last_image() {
    let dir = session_dir("cancel");
    let layer = write_edits(&dir, &sample("aovs.usda"), &settings(1 << 16));
    let session = Session::new(&dir).expect("a session");
    session.load(&layer).expect("the layer imports");
    let started = Instant::now();
    while session.snapshot().is_err() {
        assert!(started.elapsed() < Duration::from_secs(120), "no image");
        std::thread::sleep(Duration::from_millis(5));
    }
    session.cancel();
    assert!(!session.converged(), "a cancelled render never converges");
    assert!(session.snapshot().is_ok(), "the last image stays");
    assert!(matches!(session.aov("Z"), Err(Error::Busy(_))));
}

#[test]
fn a_settings_change_applies_in_place_and_restarts() {
    let dir = session_dir("apply");
    // cornellbox, whose import raises no warning a settings change could
    // move: aovs.usda's `aov.unknown_source` makes the session go full.
    let layer = write_edits(&dir, &sample("cornellbox.usda"), &own_settings(4));
    let session = Session::new(&dir).expect("a session");
    session.load(&layer).expect("the layer imports");
    let first = *converge(&session).last().unwrap();
    write_edits(&dir, &sample("cornellbox.usda"), &own_settings(8));
    session.apply().expect("the change applies");
    assert_eq!(session.applied(), Some(Applied::Settings));
    let second = converge(&session);
    assert!(
        second[0] > first,
        "a restarted render's generations continue: {first} then {second:?}"
    );
}

#[test]
fn a_layer_that_does_not_import_is_an_error_and_the_next_one_renders() {
    let dir = session_dir("refused");
    let missing = dir.join("nothing.usda");
    let session = Session::new(&dir).expect("a session");
    assert!(matches!(
        session.load(&missing),
        Err(Error::ImportFailed(_))
    ));
    // `apply` imports the layer `load` named, once it exists.
    let layer = write_edits(&dir, &sample("aovs.usda"), &settings(4));
    std::fs::rename(&layer, &missing).expect("renamed");
    session.apply().expect("the layer imports now");
    converge(&session);
}

/// The `crust` binary of this build's profile, built first when missing:
/// the equality below is with the CLI itself, not with a copy of its path.
fn crust_binary() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary");
    // target/<profile dir>/deps/<test>: the profile dir holds the binaries.
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>");
    let crust = profile_dir.join(format!("crust{}", std::env::consts::EXE_SUFFIX));
    if !crust.is_file() {
        let profile = match profile_dir.file_name().and_then(|n| n.to_str()) {
            Some("debug") => "dev",
            Some(other) => other,
            None => "dev",
        };
        let status = std::process::Command::new(env!("CARGO"))
            .args([
                "build",
                "-p",
                "crust-render",
                "--bin",
                "crust",
                "--profile",
                profile,
            ])
            .status()
            .expect("cargo runs");
        assert!(status.success(), "the crust binary builds");
    }
    crust
}

/// `crust render -i <layer> -o <dir>/cli.exr -s 16`, run in `dir` (where
/// relative product names land); the planes of the EXR at `read`.
fn crust_render(dir: &Path, layer: &Path, read: &str) -> crust_core::compare::Planes {
    let out = std::process::Command::new(crust_binary())
        .current_dir(dir)
        .args(["render", "-l", "error", "-s", "16", "-o", "cli.exr", "-i"])
        .arg(layer)
        .output()
        .expect("crust runs");
    assert!(
        out.status.success(),
        "crust render failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    crust_assets::read_exr_planes(&dir.join(read)).expect("the EXR reads")
}

/// The bits of channel `name` of `planes`, top-down.
fn bits(planes: &crust_core::compare::Planes, name: &str) -> Vec<u32> {
    planes.channels[name]
        .values
        .iter()
        .map(|v| v.to_bits())
        .collect()
}

/// The session's final image, channel `c` (0 = R), as bits.
fn snapshot_bits(snapshot: &Snapshot, c: usize) -> Vec<u32> {
    snapshot.rgba.chunks(4).map(|p| p[c].to_bits()).collect()
}

#[test]
fn the_session_renders_cornellbox_as_crust_render_does() {
    let dir = session_dir("equal_cornellbox");
    let layer = write_edits(&dir, &sample("cornellbox.usda"), &own_settings(16));
    let session = Session::new(&dir).expect("a session");
    session.load(&layer).expect("the layer imports");
    converge(&session);
    let ours = session.snapshot().expect("the final image");
    let cli = crust_render(&dir, &layer, "cli.exr");
    assert_eq!((cli.width, cli.height), (ours.width, ours.height));
    for (c, name) in ["R", "G", "B"].into_iter().enumerate() {
        assert!(
            bits(&cli, name) == snapshot_bits(&ours, c),
            "channel {name} differs from crust render's"
        );
    }
}

#[test]
fn the_session_renders_aovs_as_crust_render_does() {
    let dir = session_dir("equal_aovs");
    let layer = write_edits(&dir, &sample("aovs.usda"), &settings(16));
    let session = Session::new(&dir).expect("a session");
    session.load(&layer).expect("the layer imports");
    converge(&session);
    let ours = session.snapshot().expect("the final image");
    // `-o` replaces the first product's name; the data product keeps its own.
    let beauty = crust_render(&dir, &layer, "cli.exr");
    for (c, name) in ["R", "G", "B"].into_iter().enumerate() {
        assert!(
            bits(&beauty, name) == snapshot_bits(&ours, c),
            "channel {name} differs from crust render's"
        );
    }
    let data = crust_assets::read_exr_planes(&dir.join("renders/aovs_data.exr"))
        .expect("the data product");
    let depth = session.aov("Z").expect("the depth AOV");
    let ours: Vec<u32> = depth.data.iter().map(|v| v.to_bits()).collect();
    assert!(
        bits(&data, "Z") == ours,
        "depth differs from crust render's"
    );
}
