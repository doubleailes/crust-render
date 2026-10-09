//! `crust render --stats-json`: the `crust-stats/1` report, where it goes,
//! and where the log goes with it (the `cli` spec's "A JSON report on stdout
//! moves the log to stderr").
//!
//! On `samples/cornellbox.usda` at one sample per pixel, so a debug build
//! renders each case in seconds.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cornellbox() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/cornellbox.usda")
        .canonicalize()
        .expect("samples/cornellbox.usda")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_stats_json_cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `crust render` of the Cornell box into `dir`, with `args` appended.
fn render(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(dir)
        .arg("render")
        .arg("-i")
        .arg(cornellbox())
        .args(["-o", "out.exr", "-s", "1"])
        .args(args)
        .output()
        .expect("run crust");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn parse(text: &[u8]) -> serde_json::Value {
    serde_json::from_slice(text).unwrap_or_else(|e| {
        panic!(
            "not one JSON object ({e}):\n{}",
            String::from_utf8_lossy(text)
        )
    })
}

#[test]
fn stats_json_on_stdout_moves_the_log_to_stderr() {
    let dir = work_dir("stdout");
    let out = render(&dir, &["--stats-json", "-"]);
    let v = parse(&out.stdout);
    assert_eq!(v["format"], "crust-stats/1");
    assert!(v["crust_version"].is_string());
    let phases = v["phases"].as_array().expect("phases");
    assert!(phases.iter().any(|p| p["name"] == "Render"));
    for p in phases {
        assert!(p["name"].is_string() && p["depth"].is_u64() && p["time_s"].is_f64());
    }
    assert!(v["rays"]["camera_rays"].as_u64().unwrap() > 0);
    assert!(v["rays"]["total_rays"].as_u64().unwrap() > 0);
    assert!(v["rays"]["mean_path_length"].as_f64().unwrap() > 0.0);
    assert!(v.get("profile").is_none(), "no --profile, no profile");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Render finished"), "{stderr}");
    assert!(
        !stderr.contains("Render Statistics"),
        "no text table without --stats"
    );
    assert!(
        dir.join("out.exr").is_file(),
        "the images are still written"
    );
}

#[test]
fn stats_json_to_a_file_keeps_the_log_and_the_text_report_on_stdout() {
    let dir = work_dir("file");
    let out = render(&dir, &["--stats", "--stats-json", "stats.json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Render finished"), "{stdout}");
    assert!(stdout.contains("Render Statistics"), "{stdout}");
    let v = parse(&std::fs::read(dir.join("stats.json")).expect("stats.json"));
    assert_eq!(v["format"], "crust-stats/1");
    // One snapshot for both forms: the text prints the JSON's figure.
    if let Some(peak) = v["peak_memory_bytes"].as_u64() {
        let mib = format!("{:.2} MiB", peak as f64 / (1u64 << 20) as f64);
        let line = stdout
            .lines()
            .find(|l| l.contains("peak memory (RSS)"))
            .expect("the peak line");
        assert!(line.ends_with(&mib), "{line} vs {mib}");
    }
}

#[test]
fn profile_adds_a_profile_object() {
    let dir = work_dir("profile");
    let out = render(&dir, &["--profile", "--stats-json", "-"]);
    let v = parse(&out.stdout);
    let profile = &v["profile"];
    assert!(profile["threads"].as_u64().unwrap() >= 1);
    let sections = profile["sections"].as_array().expect("sections");
    assert!(sections.iter().any(|s| s["name"] == "MainLoop"));
    assert!(profile["tree"].as_array().is_some_and(|t| !t.is_empty()));
    // `--profile` implies the text report, which then goes to stderr.
    assert!(String::from_utf8_lossy(&out.stderr).contains("Render profile"));
}
