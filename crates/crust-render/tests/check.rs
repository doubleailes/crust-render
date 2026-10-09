//! `crust check` end to end: what it reports (text, `crust-check/1`), what
//! it refuses, what it leaves untouched, and its exit statuses.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cornellbox() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/cornellbox.usda")
        .canonicalize()
        .expect("samples/cornellbox.usda")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_check_cli").join(name);
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

fn parse(text: &[u8]) -> serde_json::Value {
    serde_json::from_slice(text).unwrap_or_else(|e| {
        panic!(
            "not one JSON object ({e}):\n{}",
            String::from_utf8_lossy(text)
        )
    })
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("list")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `body` under `/World` (with a camera), `root` beside it, written as
/// `stage.usda` in `dir`.
fn stage(dir: &Path, body: &str, root: &str) -> PathBuf {
    let path = dir.join("stage.usda");
    std::fs::write(
        &path,
        format!(
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\ndef Xform \"World\"\n{{\n    def Camera \"cam\"\n    {{\n    }}\n{body}}}\n{root}"
        ),
    )
    .expect("write stage");
    path
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("exited")
}

#[test]
fn usage_errors_exit_2_and_load_nothing() {
    let dir = work_dir("usage");
    let input = cornellbox();
    let input = input.to_str().unwrap();
    for args in [
        vec!["check"],
        vec!["check", "-i", input, "--deny", "fatal"],
        vec!["check", "-i", input, "-o", "out.exr"],
        vec!["check", "-i", input, "-s", "4"],
    ] {
        let out = crust(&dir, &args);
        assert_eq!(
            code(&out),
            2,
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(files(&dir).is_empty());
}

#[test]
fn a_stage_that_cannot_be_opened_exits_1_with_no_report() {
    let dir = work_dir("missing");
    let out = crust(&dir, &["check", "-i", "missing.usda", "--json", "r.json"]);
    assert_eq!(code(&out), 1);
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
}

/// `--json -`: one `crust-check/1` object on stdout, the log on stderr, no
/// file written, and the light count `crust render --stats-json -` reports.
#[test]
fn json_on_stdout_writes_nothing_else() {
    let dir = work_dir("json_stdout");
    let input = cornellbox();
    let out = crust(
        &dir,
        &["check", "-i", input.to_str().unwrap(), "--json", "-"],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    assert_eq!(v["format"], "crust-check/1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[material.fallback_default]"),
        "the log is on stderr: {stderr}"
    );
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    assert_eq!(v["products"][0]["file"], "output.exr");
    assert_eq!(v["products"][0]["prim"], serde_json::Value::Null);
    assert_eq!(v["scene"]["camera"], "/scene/camera1");
    assert_eq!(v["denied"], serde_json::json!([]));

    let render = crust(
        &dir,
        &[
            "render",
            "-i",
            input.to_str().unwrap(),
            "-o",
            "out.exr",
            "-s",
            "1",
            "--stats-json",
            "-",
        ],
    );
    assert!(render.status.success());
    let stats = parse(&render.stdout);
    assert_eq!(v["counts"]["lights"], stats["scene"]["lights"]);
    assert_eq!(v["counts"], stats["scene"]);
}

const TWO_FLAT_LIGHTS: &str = r#"    def RectLight "a"
    {
        float inputs:width = 0
    }
    def RectLight "b"
    {
        float inputs:width = 0
    }
"#;

#[test]
fn denying_skipped_warnings_exits_3_with_the_codes() {
    let dir = work_dir("deny_skipped");
    let path = stage(&dir, TWO_FLAT_LIGHTS, "");
    let out = crust(
        &dir,
        &[
            "check",
            "-i",
            path.to_str().unwrap(),
            "--deny",
            "skipped",
            "--json",
            "-",
        ],
    );
    assert_eq!(code(&out), 3, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    assert_eq!(v["denied"], serde_json::json!(["light.degenerate_shape"]));
    let w = &v["warnings"][0];
    assert_eq!(w["code"], "light.degenerate_shape");
    assert_eq!(w["kind"], "skipped");
    assert_eq!(w["count"], 2);
    assert_eq!(w["prims"].as_array().unwrap().len(), 2);

    // A kind the stage does not raise.
    let out = crust(
        &dir,
        &[
            "check",
            "-i",
            path.to_str().unwrap(),
            "--deny",
            "refused,approximated",
        ],
    );
    assert_eq!(code(&out), 0);
    // `all`, in text: the denied codes are named.
    let out = crust(
        &dir,
        &["check", "-i", path.to_str().unwrap(), "--deny", "all"],
    );
    assert_eq!(code(&out), 3);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("denied: light.degenerate_shape"), "{text}");
    assert!(
        text.trim_end()
            .ends_with("2 warnings (0 refused, 0 approximated, 2 skipped)"),
        "{text}"
    );
}

/// A texture with no `.tx` is a finding, not a warning: `--deny all` passes.
#[test]
fn findings_alone_do_not_deny() {
    let dir = work_dir("findings_only");
    image::RgbImage::from_pixel(4, 4, image::Rgb([200, 100, 50]))
        .save(dir.join("albedo.png"))
        .expect("png");
    let body = r#"    def Scope "Looks"
    {
        def Material "M"
        {
            token outputs:surface.connect = </World/Looks/M/Surface.outputs:surface>
            def Shader "Surface"
            {
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </World/Looks/M/Map.outputs:rgb>
                token outputs:surface
            }
            def Shader "Map"
            {
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @albedo.png@
                float3 outputs:rgb
            }
        }
    }
    def Sphere "ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        rel material:binding = </World/Looks/M>
    }
"#;
    let path = stage(&dir, body, "");
    let out = crust(
        &dir,
        &[
            "check",
            "-i",
            path.to_str().unwrap(),
            "--deny",
            "all",
            "--json",
            "-",
        ],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    assert_eq!(v["warnings"], serde_json::json!([]), "{v:#}");
    let ids: Vec<&str> = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["textures_without_tx"]);
    assert_eq!(v["findings"][0]["action"]["flag"], "--auto-tx");
    assert!(!dir.join("albedo.tx").exists(), "check converts nothing");
}

#[test]
fn a_clean_stage_text_report() {
    let dir = work_dir("clean");
    let path = stage(&dir, "", "");
    let out = crust(
        &dir,
        &[
            "check",
            "-i",
            path.to_str().unwrap(),
            "--json",
            "check.json",
        ],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\nFindings\n  no findings\n"), "{text}");
    assert!(text.contains("\nWarnings\n  no warnings\n"), "{text}");
    assert_eq!(text.lines().last(), Some("no findings and no warnings"));
    // The sections in the spec's order.
    let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("no {s:?} in {text}"));
    assert!(at("Render\n") < at("Effective settings\n"));
    assert!(at("Effective settings\n") < at("Import\n"));
    assert!(at("Import\n") < at("Findings\n"));
    assert!(at("Findings\n") < at("Warnings\n"));
    let v = parse(&std::fs::read(dir.join("check.json")).expect("check.json"));
    assert_eq!(v["format"], "crust-check/1");
    assert_eq!(files(&dir), ["check.json", "stage.usda"]);
}

/// Products resolve as a render resolves them, and none is written.
#[test]
fn products_are_listed_with_their_channels_and_not_written() {
    let dir = work_dir("products");
    let root = r#"def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel products = [</Render/main>, </Render/extra>]
        uniform int2 resolution = (32, 16)
    }
    def RenderProduct "main"
    {
        token productName = "main.exr"
        rel orderedVars = [</Render/color>, </Render/albedo>, </Render/normal>]
    }
    def RenderProduct "extra"
    {
        token productName = "extra.exr"
        rel orderedVars = [</Render/albedo>]
    }
    def RenderVar "color"
    {
        uniform token dataType = "color4f"
        uniform string sourceName = "color"
    }
    def RenderVar "albedo"
    {
        uniform token dataType = "color3f"
        uniform string sourceName = "albedo"
    }
    def RenderVar "normal"
    {
        uniform token dataType = "normal3f"
        uniform string sourceName = "N"
    }
}
"#;
    let path = stage(&dir, "", root);
    let out = crust(
        &dir,
        &["check", "-i", path.to_str().unwrap(), "--json", "-"],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    let products = v["products"].as_array().unwrap();
    assert_eq!(products.len(), 2);
    assert_eq!(products[0]["prim"], "/Render/main");
    assert_eq!(products[0]["file"], "main.exr");
    let channels: Vec<&str> = products[0]["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    for want in ["R", "G", "B", "A", "albedo.R", "normal.X"] {
        assert!(
            channels.iter().any(|c| c.eq_ignore_ascii_case(want)),
            "{want} not in {channels:?}"
        );
    }
    assert_eq!(products[1]["file"], "extra.exr");
    assert_eq!(v["scene"]["resolution"], serde_json::json!([32, 16]));
    assert_eq!(files(&dir), ["stage.usda"], "neither product is written");
}

/// Products the render would refuse when it selects what to write — no
/// `productName`, or a path an earlier product already writes — are warning
/// records like the import's, so `--deny skipped` catches them.
#[test]
fn products_the_render_would_refuse_are_denied() {
    let dir = work_dir("refused_products");
    let root = r#"def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel products = [</Render/main>, </Render/unnamed>, </Render/again>]
        uniform int2 resolution = (32, 16)
    }
    def RenderProduct "main"
    {
        token productName = "main.exr"
        rel orderedVars = [</Render/color>]
    }
    def RenderProduct "unnamed"
    {
        rel orderedVars = [</Render/color>]
    }
    def RenderProduct "again"
    {
        token productName = "main.exr"
        rel orderedVars = [</Render/color>]
    }
    def RenderVar "color"
    {
        uniform token dataType = "color4f"
        uniform string sourceName = "color"
    }
}
"#;
    let path = stage(&dir, "", root);
    let out = crust(
        &dir,
        &[
            "check",
            "-i",
            path.to_str().unwrap(),
            "--json",
            "-",
            "--deny",
            "skipped",
        ],
    );
    assert_eq!(code(&out), 3, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    let products = v["products"].as_array().unwrap();
    assert_eq!(products.len(), 1, "{products:#?}");
    assert_eq!(products[0]["prim"], "/Render/main");
    let record = |code: &str| {
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["code"] == code)
            .unwrap_or_else(|| panic!("no {code} in {:#?}", v["warnings"]))
            .clone()
    };
    assert_eq!(
        record("product.no_name")["prims"],
        serde_json::json!(["/Render/unnamed"])
    );
    assert_eq!(
        record("product.shared_path")["prims"],
        serde_json::json!(["/Render/again"])
    );
    let denied: Vec<&str> = v["denied"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert!(denied.contains(&"product.no_name"), "{denied:?}");
    assert!(denied.contains(&"product.shared_path"), "{denied:?}");
}

/// A `--json` file that cannot be written is an error, and no report is
/// printed: the text would read as if the check had completed.
#[test]
fn a_report_that_cannot_be_written_prints_nothing() {
    let dir = work_dir("unwritable");
    let path = stage(&dir, "", "");
    // The directory itself: a file cannot be written over it.
    let out = crust(
        &dir,
        &["check", "-i", path.to_str().unwrap(), "--json", "."],
    );
    assert_eq!(code(&out), 1, "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `RenderSettings.camera` naming a prim that is not a camera: the report's
/// camera is the one a render falls back to.
#[test]
fn the_reported_camera_is_the_render_s_fallback() {
    let dir = work_dir("camera");
    let root = r#"def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/notACamera>
    }
}
"#;
    let path = stage(&dir, "    def Xform \"notACamera\"\n    {\n    }\n", root);
    let out = crust(
        &dir,
        &["check", "-i", path.to_str().unwrap(), "--json", "-"],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    assert_eq!(v["scene"]["camera"], "/World/cam");
    assert_eq!(v["warnings"][0]["code"], "camera.not_a_camera");
}

/// A flag overrides the stage, and the setting names both.
#[test]
fn a_flag_overrides_the_stage_s_setting() {
    let dir = work_dir("override");
    let root = r#"def Scope "Render"
{
    def RenderSettings "settings"
    {
        token crust:lightSelection = "uniform"
    }
}
"#;
    let path = stage(&dir, "", root);
    let out = crust(
        &dir,
        &[
            "check",
            "-i",
            path.to_str().unwrap(),
            "--light-selection",
            "power",
            "--json",
            "-",
        ],
    );
    assert_eq!(code(&out), 0);
    let v = parse(&out.stdout);
    let row = v["effective_settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "light_selection")
        .expect("light_selection")
        .clone();
    assert_eq!(row["value"], "power");
    assert_eq!(row["flag"], "--light-selection");
    assert_eq!(row["usd_attribute"], "crust:lightSelection");
}
