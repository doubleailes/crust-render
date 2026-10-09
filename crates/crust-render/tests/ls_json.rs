//! `crust ls --json` and `ls -f` end to end: the `crust-ls/1` report, its
//! records in the text listing's order, each kind's key set, and the log on
//! stderr.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("samples/{name}: {e}"))
}

fn ls(args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_crust"))
        .arg("ls")
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

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON object ({e}):\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// A record's keys, sorted (`serde_json::Value` keeps no order): each
/// kind's key set is the `crust-ls/1` contract.
fn keys(record: &serde_json::Value) -> Vec<&str> {
    let mut k: Vec<&str> = record
        .as_object()
        .expect("a record is an object")
        .keys()
        .map(String::as_str)
        .collect();
    k.sort();
    k
}

#[test]
fn ls_json_records_are_the_text_listing_with_their_values() {
    for (kind, stage, want) in [
        (
            "camera",
            "cornellbox.usda",
            &[
                "path",
                "focal_length_mm",
                "aperture_mm",
                "f_stop",
                "focus_distance",
                "is_render_camera",
                "hidden",
            ][..],
        ),
        (
            "light",
            "usdlux.usda",
            &[
                "path",
                "type",
                "intensity",
                "exposure",
                "color",
                "normalize",
            ],
        ),
        (
            "material",
            "usdpreview_textured.usda",
            &["path", "surface", "bound"],
        ),
    ] {
        let stage = sample(stage);
        let input = stage.to_str().unwrap();
        let text = ls(&[kind, "-i", input]);
        let paths: Vec<String> = String::from_utf8_lossy(&text.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        assert!(!paths.is_empty(), "{kind}");
        let out = ls(&[kind, "-i", input, "--json", "-"]);
        let v = json(&out);
        assert_eq!(v["format"], "crust-ls/1");
        assert_eq!(v["kind"], kind);
        assert!(v["frame"].is_null());
        let prims = v["prims"].as_array().expect("prims");
        let listed: Vec<&str> = prims.iter().map(|p| p["path"].as_str().unwrap()).collect();
        assert_eq!(listed, paths, "{kind}: the text listing's order");
        let mut want = want.to_vec();
        want.sort();
        for p in prims {
            assert_eq!(keys(p), want, "{kind}");
        }
    }
}

#[test]
fn ls_json_to_a_file_keeps_the_paths_on_stdout() {
    let stage = sample("cornellbox.usda");
    let dir = std::env::temp_dir().join("crust_ls_json_cli");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cameras.json");
    let out = ls(&[
        "camera",
        "-i",
        stage.to_str().unwrap(),
        "--json",
        file.to_str().unwrap(),
    ]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "/scene/camera1\n");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(v["prims"][0]["is_render_camera"], true);
}

#[test]
fn ls_frame_evaluates_the_values_at_a_time_code() {
    let dir = std::env::temp_dir().join("crust_ls_frame_cli");
    std::fs::create_dir_all(&dir).unwrap();
    let stage = dir.join("animated.usda");
    std::fs::write(
        &stage,
        "#usda 1.0\ndef SphereLight \"Key\"\n{\n    float inputs:intensity.timeSamples = { 1: 1, 10: 4 }\n}\n",
    )
    .unwrap();
    let input = stage.to_str().unwrap();
    let at = |frame: &str| {
        let v = json(&ls(&["light", "-i", input, "-f", frame, "--json", "-"]));
        assert_eq!(v["frame"].as_f64(), frame.parse().ok());
        v["prims"][0]["intensity"].as_f64().unwrap()
    };
    assert_eq!(at("10"), 4.0);
    assert_eq!(at("1"), 1.0);
    // Parsed as `render -f` parses it: a negative frame, not a flag.
    assert_eq!(at("-5"), 1.0);
    // The paths do not depend on it.
    let text = ls(&["light", "-i", input, "-f", "10"]);
    assert_eq!(String::from_utf8_lossy(&text.stdout), "/Key\n");
    let bad = Command::new(env!("CARGO_BIN_EXE_crust"))
        .args(["ls", "light", "-i", input, "-f", "nan"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2), "a usage error");
}
