//! `crust diagnostic` end to end: what it writes (Markdown on stdout, the
//! JSON report, nothing else), what it refuses, and its exit statuses.
//!
//! On `samples/cornellbox.usda` with small budgets, so a debug build runs
//! each case in a few seconds: under them the baseline may well use the
//! whole budget, which is itself a case (exit 3, the report still written).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cornellbox() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/cornellbox.usda")
        .canonicalize()
        .expect("samples/cornellbox.usda")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_diagnostic_cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `crust diagnostic <args>`, run in `dir`.
fn crust(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(dir)
        .arg("diagnostic")
        .args(args)
        .output()
        .expect("run crust")
}

/// Every file under `dir`, with its bytes.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(dir)
        .expect("list")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .map(|p| {
            let bytes = std::fs::read(&p).expect("read");
            (p, bytes)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn a_run_writes_the_two_reports_and_nothing_else() {
    let dir = work_dir("outputs");
    let stage = cornellbox();
    let samples = snapshot(stage.parent().unwrap());
    let out = crust(
        &dir,
        &[
            "-i",
            stage.to_str().unwrap(),
            "--budget",
            "4s",
            "--repeats",
            "1",
        ],
    );
    let code = out.status.code();
    assert!(matches!(code, Some(0) | Some(3)), "{out:?}");
    // stdout is the Markdown report and only that.
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    assert!(stdout.starts_with("# crust diagnostic: "), "{stdout}");
    assert!(stdout.contains("\n## Verdict\n") && stdout.contains("\n## Suggested command\n"));
    assert!(
        !stdout.contains("INFO") && !stdout.contains("WARN"),
        "a log line on stdout"
    );
    // The JSON, at the default path in the working directory.
    let json = std::fs::read_to_string(dir.join("crust-diagnostic.json")).expect("the JSON report");
    let report: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(report["format"], "crust-diagnostic/1");
    assert_eq!(report["run"]["exit"].as_i64(), code.map(i64::from));
    // Nothing else: no image here, and the stage's directory untouched.
    let written: Vec<_> = snapshot(&dir).into_iter().map(|(p, _)| p).collect();
    assert_eq!(written, [dir.join("crust-diagnostic.json")]);
    assert!(
        snapshot(stage.parent().unwrap()) == samples,
        "a file beside the stage changed"
    );
}

#[test]
fn a_one_second_budget_exits_3_with_the_trials_not_tried() {
    let dir = work_dir("budget");
    let json = dir.join("r.json");
    let out = crust(
        &dir,
        &[
            "-i",
            cornellbox().to_str().unwrap(),
            "--budget",
            "1s",
            "--json",
            json.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).expect("written anyway")).unwrap();
    let not_tried = report["not_tried"].as_array().expect("an array");
    assert!(
        not_tried
            .iter()
            .any(|n| n["reason"] == "budget" && n["tier"] == 1),
        "{not_tried:?}"
    );
    assert!(report["run"]["budget_exceeded_in"].is_string());
}

#[test]
fn render_only_flags_are_usage_errors() {
    let dir = work_dir("usage");
    let stage = cornellbox();
    for extra in [
        &["-s", "64"][..],
        &["-o", "x.exr"],
        &["--view", "Raw"],
        &["--stats"],
    ] {
        let mut args = vec!["-i", stage.to_str().unwrap()];
        args.extend_from_slice(extra);
        let out = crust(&dir, &args);
        assert_eq!(out.status.code(), Some(2), "{extra:?}: {out:?}");
    }
    // A stage is required.
    assert_eq!(crust(&dir, &["--budget", "1s"]).status.code(), Some(2));
    assert_eq!(
        crust(&dir, &["-i", "s.usda", "--budget", "soon"])
            .status
            .code(),
        Some(2)
    );
    assert!(snapshot(&dir).is_empty(), "a refused run wrote something");
}

#[test]
fn a_missing_stage_or_baseline_is_an_error_with_no_report() {
    let dir = work_dir("missing");
    let out = crust(&dir, &["-i", "no/such/stage.usda", "--budget", "1s"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let out = crust(
        &dir,
        &[
            "-i",
            cornellbox().to_str().unwrap(),
            "--baseline",
            "no/such/report.json",
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(snapshot(&dir).is_empty(), "an error wrote a report");
}

#[test]
fn a_baseline_of_another_camera_is_not_comparable() {
    let dir = work_dir("baseline");
    let prev = dir.join("prev.json");
    let out = crust(
        &dir,
        &[
            "-i",
            cornellbox().to_str().unwrap(),
            "--budget",
            "1s",
            "--json",
            prev.to_str().unwrap(),
        ],
    );
    assert!(matches!(out.status.code(), Some(0) | Some(3)), "{out:?}");
    // The same report, as if made through another camera.
    let text = std::fs::read_to_string(&prev).expect("the first report");
    let other = text.replacen("\"camera\": null", "\"camera\": \"/cams/other\"", 1);
    assert_ne!(other, text);
    std::fs::write(&prev, other).unwrap();
    let json = dir.join("now.json");
    let out = crust(
        &dir,
        &[
            "-i",
            cornellbox().to_str().unwrap(),
            "--budget",
            "1s",
            "--json",
            json.to_str().unwrap(),
            "--baseline",
            prev.to_str().unwrap(),
        ],
    );
    assert!(matches!(out.status.code(), Some(0) | Some(3)), "{out:?}");
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(report["deltas"]["comparable"], false);
    assert_eq!(
        report["deltas"]["note"],
        "not comparable: a different camera"
    );
}
