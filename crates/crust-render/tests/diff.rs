//! `crust diff` end to end: its exit status (0 identical, 1 differs, 2
//! error), the JSON report, and the comparability notes read from the
//! renders' sampling stamps — which never change the exit status.
//!
//! The renders are 16×16 crops of `samples/cornellbox.usda`, so a debug
//! build makes them in seconds.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cornellbox() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/cornellbox.usda")
        .canonicalize()
        .expect("samples/cornellbox.usda")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_diff_cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn crust(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run crust")
}

/// A 16×16 crop of the Cornell box at `out`, with `args`.
fn render(dir: &Path, out: &str, args: &[&str]) {
    let stage = cornellbox();
    let mut all = vec![
        "render",
        "-i",
        stage.to_str().unwrap(),
        "-o",
        out,
        "--region",
        "0,0,16,16",
        "-l",
        "error",
    ];
    all.extend_from_slice(args);
    let o = crust(dir, &all);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON object ({e}):\n{}",
            String::from_utf8_lossy(&o.stdout)
        )
    })
}

#[test]
fn identical_renders_exit_0_and_report_ok() {
    let dir = work_dir("identical");
    let fixed = ["-s", "16", "--indirect-clamp", "0"];
    render(&dir, "a.exr", &fixed);
    render(&dir, "b.exr", &fixed);
    let o = crust(&dir, &["diff", "a.exr", "b.exr"]);
    assert_eq!(o.status.code(), Some(0));
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(
        text.starts_with("16x16  differing pixels: 0/256 (0.0000%)\n"),
        "{text}"
    );
    assert!(o.stderr.is_empty(), "ok prints no note");

    let o = crust(&dir, &["diff", "a.exr", "b.exr", "--json", "-"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json(&o);
    assert_eq!(v["format"], "crust-diff/1");
    assert_eq!(v["identical"], true);
    assert_eq!(v["differing_pixels"], 0);
    assert_eq!(v["total_pixels"], 256);
    assert!(v["channels"].is_array());
    assert_eq!(v["beauty"]["rmse"], 0.0);
    assert_eq!(v["comparability"]["status"], "ok");
}

#[test]
fn differing_renders_exit_1_and_adaptive_sampling_warns() {
    let dir = work_dir("differs");
    render(&dir, "a.exr", &["-s", "16", "--indirect-clamp", "0"]);
    render(&dir, "b.exr", &["-s", "64", "--indirect-clamp", "0"]);
    let o = crust(&dir, &["diff", "a.exr", "b.exr", "--json", "report.json"]);
    assert_eq!(o.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("comparability: warn"), "{stderr}");
    assert!(stderr.contains("adaptive sampling"), "{stderr}");
    // `--json PATH` writes the file beside the text report.
    assert!(String::from_utf8_lossy(&o.stdout).contains("relmse: "));
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("report.json")).unwrap()).unwrap();
    assert_eq!(v["identical"], false);
    assert!(v["beauty"]["relmse"].as_f64().unwrap() > 0.0);
    // 0.1% of 256 pixels trims none: the two are one mean, summed in two
    // orders.
    let (all, trimmed) = (
        v["beauty"]["relmse"].as_f64().unwrap(),
        v["beauty"]["relmse_trimmed"].as_f64().unwrap(),
    );
    assert!(trimmed <= all * (1.0 + 1e-9), "{trimmed} > {all}");
    assert_eq!(v["comparability"]["status"], "warn");

    // A clamp on one side only warns, and the exit status still answers
    // only "did the pixels change".
    render(&dir, "c.exr", &["-s", "16", "--indirect-clamp", "10"]);
    let o = crust(&dir, &["diff", "a.exr", "c.exr", "--json", "-"]);
    let notes = json(&o)["comparability"]["notes"].to_string();
    assert!(notes.contains("indirectClamp"), "{notes}");
    assert!(matches!(o.status.code(), Some(0 | 1)));
}

#[test]
fn an_unreadable_file_exits_2_naming_it() {
    let dir = work_dir("missing");
    render(&dir, "a.exr", &["-s", "1"]);
    let o = crust(&dir, &["diff", "a.exr", "nope.exr"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(o.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("nope.exr"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    let o = crust(&dir, &["diff", "a.exr"]);
    assert_eq!(o.status.code(), Some(2), "a usage error");
}

#[test]
fn an_unstamped_file_is_unknown_and_still_compared() {
    let dir = work_dir("unstamped");
    render(&dir, "a.exr", &["-s", "1"]);
    // Rewritten by another writer: the same pixels, no crust:* attributes.
    let image = exr::prelude::read_first_rgba_layer_from_file(
        dir.join("a.exr"),
        |res, _| vec![vec![(0.0f32, 0.0f32, 0.0f32, 0.0f32); res.width()]; res.height()],
        |px: &mut Vec<Vec<(f32, f32, f32, f32)>>, p, (r, g, b, a): (f32, f32, f32, f32)| {
            px[p.y()][p.x()] = (r, g, b, a)
        },
    )
    .expect("reads");
    let px = &image.layer_data.channel_data.pixels;
    exr::prelude::write_rgb_file(dir.join("other.exr"), 16, 16, |x, y| {
        let (r, g, b, _) = px[y][x];
        (r, g, b)
    })
    .expect("writes");
    let o = crust(&dir, &["diff", "other.exr", "a.exr", "--json", "-"]);
    let v = json(&o);
    assert_eq!(v["comparability"]["status"], "unknown");
    assert!(v["a"]["stamp"].is_null());
    assert_eq!(o.status.code(), Some(0), "{v}");
}
