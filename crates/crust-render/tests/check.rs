//! `crust check` end to end: what it reports (text, `crust-check/2`), what
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

/// `--json -`: one `crust-check/2` object on stdout, the log on stderr, no
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
    assert_eq!(v["format"], "crust-check/2");
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

/// A sphere whose material reads `albedo.png` through a UsdUVTexture with no
/// `sourceColorSpace` (so `auto`, sRGB for a PNG).
const ALBEDO_LOOK: &str = r#"    def Scope "Looks"
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

/// The `crust-check/2` finding ids for `args` on a stage that binds
/// `albedo.png`.
fn finding_ids(dir: &Path, args: &[&str]) -> Vec<String> {
    let path = stage(dir, ALBEDO_LOOK, "");
    let mut all = vec!["check", "-i", path.to_str().unwrap(), "--json", "-"];
    all.extend_from_slice(args);
    let out = crust(dir, &all);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    parse(&out.stdout)["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect()
}

/// `--tx-color-space` names the `.tx` a check looks for, as it names the one
/// `--auto-tx` writes: a file named for `Linear Rec.709 (sRGB)` streams the
/// texture under that target (spelled by any alias) and is not looked for
/// under the default ACEScg, where the texture has no `.tx`.
#[test]
fn the_tx_target_space_is_a_shared_flag() {
    let dir = work_dir("tx_color_space");
    image::RgbImage::from_pixel(4, 4, image::Rgb([200, 100, 50]))
        .save(dir.join("albedo.png"))
        .expect("png");
    // Any readable `.tx` will do: an Arnold-written one, under the name a
    // Linear Rec.709 target gives this binding.
    let arnold = Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "../crust-assets/tests/data/arnold_tx/srgb_canon_sRGB Encoded Rec.709 (sRGB)_ACEScg.png.tx",
    );
    std::fs::copy(
        arnold,
        dir.join("albedo_sRGB Encoded Rec.709 (sRGB)_Linear Rec.709 (sRGB).png.tx"),
    )
    .expect("copy the .tx");
    let without = "textures_without_tx".to_string();
    assert!(finding_ids(&dir, &[]).contains(&without));
    for target in ["Linear Rec.709 (sRGB)", "lin_rec709"] {
        let ids = finding_ids(&dir, &["--tx-color-space", target]);
        assert!(!ids.contains(&without), "{target}: {ids:?}");
    }
}

/// A `.tx` target the OCIO config does not define is a usage error, for
/// `render` as for `check`, and nothing is loaded or written.
#[test]
fn an_unknown_tx_target_space_is_a_usage_error() {
    let dir = work_dir("tx_color_space_unknown");
    let input = cornellbox();
    for command in ["render", "check"] {
        let out = crust(
            &dir,
            &[
                command,
                "-i",
                input.to_str().unwrap(),
                "--tx-color-space",
                "nonsense",
            ],
        );
        assert_eq!(code(&out), 2, "{command}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("defines no such colour space"),
            "{command}: {err}"
        );
    }
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
}

/// A config without `ACEScg` (it needs only crust's five well-known spaces)
/// still renders when no `--tx-color-space` is given: the default target falls
/// back to the data space. Only a target the user names is checked.
#[test]
fn a_config_without_the_default_tx_target_still_renders() {
    let dir = work_dir("tx_color_space_no_acescg");
    let config = dir.join("no_acescg.ocio");
    std::fs::write(
        &config,
        "ocio_profile_version: 2\n\
         roles:\n  default: lin_rec709\n  scene_linear: lin_rec709\n  data: raw\n\
         displays:\n  sRGB:\n    - !<View> {name: Raw, colorspace: raw}\n\
         colorspaces:\n\
         \x20 - !<ColorSpace>\n    name: lin_rec709\n\
         \x20 - !<ColorSpace>\n    name: raw\n    isdata: true\n\
         \x20 - !<ColorSpace>\n    name: srgb_texture\n    from_scene_reference: \
         !<ExponentWithLinearTransform> {gamma: 2.4, offset: 0.055, direction: inverse}\n\
         \x20 - !<ColorSpace>\n    name: g22_rec709\n    from_scene_reference: \
         !<ExponentTransform> {value: 2.2, direction: inverse}\n\
         \x20 - !<ColorSpace>\n    name: g18_rec709\n    from_scene_reference: \
         !<ExponentTransform> {value: 1.8, direction: inverse}\n",
    )
    .expect("write the config");
    let input = cornellbox();
    let render = |extra: &[&str]| {
        let mut args = vec![
            "render",
            "-i",
            input.to_str().unwrap(),
            "-s",
            "1",
            "--ocio-config",
            config.to_str().unwrap(),
            "--display",
            "sRGB",
            "--view",
            "Raw",
            "-o",
            "out.exr",
        ];
        args.extend_from_slice(extra);
        crust(&dir, &args)
    };
    let out = render(&[]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let out = render(&["--tx-color-space", "ACEScg"]);
    assert_eq!(code(&out), 2, "{}", String::from_utf8_lossy(&out.stderr));
}

/// A texture with no `.tx` is a finding, not a warning: `--deny all` passes.
#[test]
fn findings_alone_do_not_deny() {
    let dir = work_dir("findings_only");
    image::RgbImage::from_pixel(4, 4, image::Rgb([200, 100, 50]))
        .save(dir.join("albedo.png"))
        .expect("png");
    let body = ALBEDO_LOOK;
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
    assert!(
        !files(&dir).iter().any(|f| f.ends_with(".tx")),
        "check converts nothing: {:?}",
        files(&dir)
    );
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
    assert_eq!(v["format"], "crust-check/2");
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

/// `--sampler` is a shared flag: the report's effective settings show it
/// with its flag and attribute, as a render with the same flag would use.
#[test]
fn the_sampler_is_a_shared_flag() {
    let dir = work_dir("sampler_flag");
    let input = cornellbox();
    let out = crust(
        &dir,
        &[
            "check",
            "-i",
            input.to_str().unwrap(),
            "--sampler",
            "sobolBn",
            "--json",
            "-",
        ],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    let row = v["effective_settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "sampler")
        .expect("sampler")
        .clone();
    assert_eq!(row["value"], "sobolBn");
    assert_eq!(row["flag"], "--sampler");
    assert_eq!(row["usd_attribute"], "crust:sampler");
}

/// A product's frame variables are expanded at the checked frame, and two
/// products are judged to collide on their expanded paths.
#[test]
fn products_are_reported_with_their_frame_expanded() {
    let dir = work_dir("frame_vars");
    let root = r#"def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel products = [</Render/main>, </Render/udim>, </Render/again>]
        uniform int2 resolution = (32, 16)
    }
    def RenderProduct "main"
    {
        token productName = "renders/beauty.$F4.exr"
        rel orderedVars = [</Render/color>]
    }
    def RenderProduct "udim"
    {
        token productName = "renders/raw.<F>.exr"
        rel orderedVars = [</Render/color>]
    }
    def RenderProduct "again"
    {
        token productName = "renders/beauty.0005.exr"
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
    let check = |frame: &str| {
        let out = crust(
            &dir,
            &[
                "check",
                "-i",
                path.to_str().unwrap(),
                "-f",
                frame,
                "--json",
                "-",
            ],
        );
        assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
        parse(&out.stdout)
    };
    let codes = |v: &serde_json::Value| -> Vec<String> {
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_owned())
            .collect()
    };

    let v = check("5");
    let products = v["products"].as_array().unwrap();
    let names: Vec<&str> = products
        .iter()
        .map(|p| p["file"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["renders/beauty.0005.exr", "renders/raw.5.exr"]);
    let shared = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "product.shared_path")
        .unwrap_or_else(|| panic!("no product.shared_path in {:#?}", v["warnings"]));
    assert_eq!(shared["prims"], serde_json::json!(["/Render/again"]));
    assert!(!codes(&v).contains(&"product.subframe_frame".to_owned()));

    let v = check("5.5");
    let names: Vec<&str> = v["products"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["file"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "renders/beauty.0006.exr",
            "renders/raw.6.exr",
            "renders/beauty.0005.exr"
        ]
    );
    let subframe = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "product.subframe_frame")
        .unwrap_or_else(|| panic!("no product.subframe_frame in {:#?}", v["warnings"]));
    assert_eq!(
        subframe["prims"],
        serde_json::json!(["/Render/main", "/Render/udim"])
    );
    assert_eq!(files(&dir), ["stage.usda"], "nothing is written");
}

/// An unknown variable is left in the path as written and reported.
#[test]
fn an_unknown_path_variable_is_kept_and_reported() {
    let dir = work_dir("unknown_var");
    let root = r#"def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel products = [</Render/main>]
        uniform int2 resolution = (32, 16)
    }
    def RenderProduct "main"
    {
        token productName = "$SHOT/beauty.$F4.exr"
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
            "-f",
            "2",
            "--json",
            "-",
        ],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    assert_eq!(v["products"][0]["file"], "$SHOT/beauty.0002.exr");
    let unknown = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "product.unknown_variable")
        .unwrap_or_else(|| panic!("no product.unknown_variable in {:#?}", v["warnings"]));
    assert_eq!(unknown["prims"], serde_json::json!(["/Render/main"]));
    assert_eq!(unknown["kind"], "refused");
}

fn render_passes() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/render_passes.usda")
        .canonicalize()
        .expect("samples/render_passes.usda")
}

/// The `scene-check` spec's "A pass with its own settings" and "No pass":
/// the report names the pass, the settings its `renderSource` chooses, the
/// products those write and what it held out.
#[test]
fn the_active_pass_and_its_settings_are_reported() {
    let dir = work_dir("pass");
    let input = render_passes();
    let input = input.to_str().unwrap();
    let out = crust(
        &dir,
        &[
            "check",
            "-i",
            input,
            "--pass",
            "/Render/Passes/char",
            "--json",
            "-",
        ],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    assert_eq!(v["pass"]["path"], "/Render/Passes/char");
    // The crate, the mirror ball and the floor; the sky is a light.
    assert_eq!(v["pass"]["holdouts"], 3);
    assert_eq!(v["pass"]["camera_hidden"], 0);
    assert_eq!(v["settings"], "/Render/charSettings");
    assert_eq!(v["products"][0]["file"], "char.exr");

    let out = crust(&dir, &["check", "-i", input, "--json", "-"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let v = parse(&out.stdout);
    assert!(v["pass"].is_null());
    assert_eq!(v["settings"], "/Render/settings");
    assert_eq!(v["products"][0]["file"], "props.exr");

    let out = crust(&dir, &["check", "-i", input, "--pass", "/World"]);
    assert_eq!(code(&out), 1, "a path that is not a pass stops the check");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("/World") && stderr.contains("RenderPass"),
        "{stderr}"
    );
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));

    // A stage with no settings prim renders on the defaults, and says so:
    // no settings prim is named, not the conventional path it fell back to.
    let cornell = cornellbox();
    let out = crust(
        &dir,
        &["check", "-i", cornell.to_str().unwrap(), "--json", "-"],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert!(parse(&out.stdout)["settings"].is_null());
}
