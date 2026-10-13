//! `crust light-paths` end to end: what it refuses before loading, the
//! report on stdout and in `crust-light-paths/1`, and the `cli` spec's
//! promise that the inspected pixels are the pixels `crust render` writes
//! with the same flags.
//!
//! On `samples/dome_backdrop.usda` (240×136), small enough to render at
//! 16 spp in a debug build.

use exr::prelude::{ReadChannels, ReadLayers, read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("samples/{name}: {e}"))
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("crust_light_paths_cli")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `crust <args>`, run in `dir`.
fn crust(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run crust")
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("exited")
}

fn parse(text: &[u8]) -> serde_json::Value {
    serde_json::from_slice(text).unwrap_or_else(|e| {
        panic!(
            "not one JSON object ({e}):\n{}",
            String::from_utf8_lossy(text)
        )
    })
}

/// Usage errors exit 2 before the stage is loaded: the stage does not
/// exist, so a load would have failed with 1.
#[test]
fn usage_errors_exit_2_and_load_nothing() {
    let dir = work_dir("usage");
    for args in [
        &["light-paths", "-i", "missing.usda"][..],
        &["light-paths", "--region", "0,0,5,5"],
        &["light-paths", "-i", "missing.usda", "--region", "0,0,20,20"],
        &[
            "light-paths",
            "-i",
            "missing.usda",
            "--region",
            "0,0,5,5",
            "--group-by",
            "expr+prims+light",
        ],
        &[
            "light-paths",
            "-i",
            "missing.usda",
            "--region",
            "0,0,5,5",
            "-o",
            "out.exr",
        ],
        &["light-paths", "-i", "missing.usda", "--region", "5,5,5,9"],
        &[
            "light-paths",
            "-i",
            "missing.usda",
            "--region",
            "0,0,5,5",
            "--top",
            "5000",
        ],
        &[
            "light-paths",
            "-i",
            "missing.usda",
            "--region",
            "0,0,18446744073709551615,18446744073709551615",
        ],
    ] {
        let out = crust(&dir, args);
        assert_eq!(
            code(&out),
            2,
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    let stderr = String::from_utf8_lossy(
        &crust(
            &dir,
            &["light-paths", "-i", "missing.usda", "--region", "0,0,20,20"],
        )
        .stderr,
    )
    .into_owned();
    assert!(stderr.contains("100"), "{stderr}");
    let stderr = String::from_utf8_lossy(
        &crust(
            &dir,
            &[
                "light-paths",
                "-i",
                "missing.usda",
                "--region",
                "0,0,5,5",
                "--group-by",
                "expr+prims+light",
            ],
        )
        .stderr,
    )
    .into_owned();
    assert!(stderr.contains("prim"), "{stderr}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    // A well-formed request on a stage that cannot be opened is an error.
    let out = crust(
        &dir,
        &["light-paths", "-i", "missing.usda", "--region", "0,0,5,5"],
    );
    assert_eq!(code(&out), 1);
}

/// `--json -`: one `crust-light-paths/1` object on stdout, the Markdown
/// and the log on stderr, nothing written.
#[test]
fn json_on_stdout_is_the_only_stdout() {
    let dir = work_dir("json_stdout");
    let stage = sample("dome_backdrop.usda");
    let out = crust(
        &dir,
        &[
            "light-paths",
            "-i",
            stage.to_str().unwrap(),
            "-s",
            "4",
            "--region",
            "115,63,125,73",
            "--json",
            "-",
        ],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    assert_eq!(v["format"], "crust-light-paths/1");
    assert_eq!(v["region"], serde_json::json!([115, 63, 125, 73]));
    assert_eq!(v["pixels"].as_array().unwrap().len(), 100);
    assert!(v["summary"].is_string());
    assert!(
        v["command"]
            .as_str()
            .unwrap()
            .starts_with("crust light-paths -i ")
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("# crust light-paths"), "{stderr}");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

/// The `R` plane of an EXR's data window, top-down, and its width.
fn red(path: &Path) -> (Vec<f32>, usize) {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .expect("reads back");
    let layer = &image.layer_data;
    let r = layer
        .channel_data
        .list
        .iter()
        .find(|c| c.name.to_string() == "R")
        .expect("an R channel");
    let plane = (0..layer.size.area())
        .map(|i| r.sample_data.value_by_flat_index(i).to_f32())
        .collect();
    (plane, layer.size.width())
}

/// The spec's scenario: the pixels `crust light-paths` inspects are the
/// pixels `crust render` writes with the same flags, bit for bit; and the
/// Markdown on stdout comes with the JSON file.
#[test]
fn the_inspected_pixels_are_the_rendered_ones() {
    let dir = work_dir("same_pixels");
    let stage = sample("dome_backdrop.usda");
    let stage = stage.to_str().unwrap();
    let out = crust(
        &dir,
        &[
            "render", "-i", stage, "-s", "16", "-o", "f.exr", "-l", "error",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let out = crust(
        &dir,
        &[
            "light-paths",
            "-i",
            stage,
            "-s",
            "16",
            "--region",
            "40,40,50,50",
            "--json",
            "r.json",
        ],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("# crust light-paths: region 40,40,50,50"),
        "{stdout}"
    );
    let report = parse(&std::fs::read(dir.join("r.json")).expect("r.json"));
    assert_eq!(report["reproduces_frame"], true);
    let (plane, width) = red(&dir.join("f.exr"));
    for p in report["pixels"].as_array().unwrap() {
        let (x, y) = (
            p["pixel"][0].as_u64().unwrap() as usize,
            p["pixel"][1].as_u64().unwrap() as usize,
        );
        let inspected = p["beauty"][0].as_f64().unwrap() as f32;
        assert_eq!(
            inspected.to_bits(),
            plane[y * width + x].to_bits(),
            "pixel {x},{y}"
        );
    }
}
