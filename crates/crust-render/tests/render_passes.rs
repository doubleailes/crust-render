//! `--pass` end to end on `samples/render_passes.usda`: which settings a
//! render pass renders with, the refusals that stop a command before
//! anything is traced, and `crust ls pass`.

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
    let dir = std::env::temp_dir().join("crust_render_passes").join(name);
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

/// Everything the command logged: `render` logs to stdout, the other
/// commands to stderr.
fn log(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn stage() -> String {
    sample("render_passes.usda").to_str().unwrap().to_owned()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON object ({e}):\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// `command -i input --pass pass`, at one sample per pixel for a render.
fn args<'a>(command: &'a str, input: &'a str, pass: &'a str) -> Vec<&'a str> {
    let mut args = vec![command, "-i", input, "--pass", pass];
    if command == "render" {
        args.extend(["-s", "1"]);
    }
    args
}

/// A stage that sublayers the sample and adds `extra` at its root.
fn with_extra(dir: &Path, extra: &str) -> String {
    let path = dir.join("shot.usda");
    std::fs::write(
        &path,
        format!(
            "#usda 1.0\n(\n    subLayers = [@{}@]\n)\n\n{extra}\n",
            stage().replace('\\', "/")
        ),
    )
    .unwrap();
    path.to_str().unwrap().to_owned()
}

#[test]
fn a_pass_picks_its_settings() {
    let dir = work_dir("picks_settings");
    let input = stage();
    let resolution = |extra: &[&str]| {
        let mut args = vec!["check", "-i", &input, "--json", "-"];
        args.extend_from_slice(extra);
        let out = crust(&dir, &args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        json(&out)["scene"]["resolution"].clone()
    };
    // The stage's own settings, with or without the pass that names them.
    assert_eq!(resolution(&[]), serde_json::json!([160, 90]));
    assert_eq!(
        resolution(&["--pass", "/Render/Passes/render"]),
        serde_json::json!([160, 90])
    );
    // The fx pass's renderSource.
    assert_eq!(
        resolution(&["--pass", "/Render/Passes/fx"]),
        serde_json::json!([64, 36])
    );
}

#[test]
fn a_pass_without_a_render_source_uses_the_stages_settings() {
    let dir = work_dir("no_source");
    let input = with_extra(
        &dir,
        "over \"Render\"\n{\n    over \"Passes\"\n    {\n        def RenderPass \"bare\"\n        {\n        }\n    }\n}",
    );
    let out = crust(
        &dir,
        &[
            "check",
            "-i",
            &input,
            "--pass",
            "/Render/Passes/bare",
            "--json",
            "-",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        json(&out)["scene"]["resolution"],
        serde_json::json!([160, 90])
    );
}

#[test]
fn not_a_render_pass_stops_the_command() {
    let dir = work_dir("not_a_pass");
    let input = stage();
    for (command, pass) in [
        ("render", "/World/Cam"),
        ("render", "/Render/Passes/missing"),
        ("check", "/World/Cam"),
    ] {
        let out = crust(&dir, &args(command, &input, pass));
        assert!(!out.status.success(), "{command} --pass {pass}");
        let log = log(&out);
        assert!(log.contains(pass), "{command} --pass {pass}: {log}");
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "{command} --pass {pass} wrote nothing"
        );
    }
    // A malformed path is refused the same way.
    let out = crust(&dir, &["render", "-i", &input, "--pass", "Passes/render"]);
    assert!(!out.status.success());
    assert!(log(&out).contains("Passes/render"));
}

#[test]
fn a_source_that_is_not_render_settings_stops_the_command() {
    let dir = work_dir("bad_source");
    let input = with_extra(
        &dir,
        "over \"Render\"\n{\n    over \"Passes\"\n    {\n        def RenderPass \"wrong\"\n        {\n            rel renderSource = </World/Cam>\n        }\n    }\n}",
    );
    for command in ["render", "check"] {
        let out = crust(&dir, &args(command, &input, "/Render/Passes/wrong"));
        assert!(!out.status.success(), "{command}");
        let log = log(&out);
        assert!(
            log.contains("/Render/Passes/wrong") && log.contains("/World/Cam"),
            "{command}: {log}"
        );
    }
}

#[test]
fn ls_pass_lists_the_render_passes_of_a_stage() {
    let dir = work_dir("ls_pass");
    let out = crust(&dir, &["ls", "pass", "-i", &stage()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "/Render/Passes/render\n/Render/Passes/fx\n"
    );
}

#[test]
fn pass_records() {
    let dir = work_dir("pass_records");
    let out = crust(&dir, &["ls", "passes", "-i", &stage(), "--json", "-"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    assert_eq!(v["kind"], "pass");
    assert_eq!(
        v["prims"],
        serde_json::json!([
            {
                "path": "/Render/Passes/render",
                "render_source": "/Render/settings",
                "layers": ["set", "hero", "default"],
                "file_pattern": "renders/$l_$n_$o.$04f.$x",
                "layer_pattern": "$n_$o",
            },
            {
                "path": "/Render/Passes/fx",
                "render_source": "/Render/fx_settings",
                "layers": ["default"],
                "file_pattern": "$l_$n_$o.$04f.$x",
                "layer_pattern": "$n_$o",
            },
        ])
    );
}

#[test]
fn an_unknown_kind_names_the_valid_ones() {
    let dir = work_dir("ls_mesh");
    let out = crust(&dir, &["ls", "mesh", "-i", &stage()]);
    assert_eq!(out.status.code(), Some(2), "a usage error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    for kind in ["camera", "light", "material", "pass"] {
        assert!(stderr.contains(kind), "{kind}: {stderr}");
    }
}

// --- The files a pass writes (`render-passes` "The pass's file pattern",
// "Outputs sharing a path share a file", "Preview PNGs of a pass"), at one
// sample per pixel: only the file names are checked here; the channels are
// `products.rs`'s unit tests and `check.rs`'s.

/// The files under `dir`, relative and sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("list").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap();
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn render_pass(dir: &Path, extra: &[&str]) -> Output {
    let input = stage();
    let mut args = vec![
        "render",
        "-i",
        &input,
        "--pass",
        "/Render/Passes/render",
        "-s",
        "1",
    ];
    args.extend_from_slice(extra);
    let out = crust(dir, &args);
    assert!(out.status.success(), "{}", log(&out));
    out
}

#[test]
fn the_authored_pattern() {
    let dir = work_dir("authored_pattern");
    render_pass(&dir, &["-f", "3"]);
    let mut want: Vec<String> = Vec::new();
    for layer in ["set", "hero", "default"] {
        for var in ["Beauty", "Diffuse", "Depth"] {
            want.push(format!("renders/render_{layer}_{var}.0003.exr"));
        }
        // One output per file: the beauty's file gets its layer's preview.
        want.push(format!("renders/render_{layer}_Beauty.0003.png"));
    }
    want.sort();
    assert_eq!(tree(&dir), want);
}

#[test]
fn o_replaces_the_pattern() {
    let dir = work_dir("o_replaces");
    let out = render_pass(&dir, &["-f", "3", "-o", "out/$n.$04f.$x"]);
    assert_eq!(
        tree(&dir),
        [
            "out/default.0003.exr",
            "out/default.0003.png",
            "out/hero.0003.exr",
            "out/hero.0003.png",
            "out/set.0003.exr",
            "out/set.0003.png",
        ]
    );
    // Expanded as a pattern only: husk's dialect never sees it.
    assert!(
        !log(&out).contains("product.unknown_variable"),
        "{}",
        log(&out)
    );
}

#[test]
fn o_with_a_pass_is_not_expanded_as_husks() {
    let dir = work_dir("not_husk");
    let out = render_pass(&dir, &["-f", "12", "-o", "out/$n.$04f.$x"]);
    let log = log(&out);
    for name in ["$n", "$04f", "$x"] {
        assert!(!log.contains(&format!("{name} in")), "{name}: {log}");
    }
    assert!(!log.contains("product.unknown_variable"), "{log}");
    assert!(tree(&dir).contains(&"out/hero.0012.exr".to_owned()));
}

#[test]
fn no_o_and_per_layer_previews() {
    let dir = work_dir("no_o");
    render_pass(&dir, &["-o", "$l_$n.$x"]);
    assert_eq!(
        tree(&dir),
        [
            "render_default.exr",
            "render_default.png",
            "render_hero.exr",
            "render_hero.png",
            "render_set.exr",
            "render_set.png",
        ]
    );
}

#[test]
fn neither_n_nor_o() {
    let dir = work_dir("neither");
    render_pass(&dir, &["-o", "$l.$x"]);
    // Every layer in one file, and so no preview.
    assert_eq!(tree(&dir), ["render.exr"]);
}

#[test]
fn a_pattern_that_is_not_an_exr_is_refused() {
    let dir = work_dir("not_exr");
    let out = crust(
        &dir,
        &[
            "render",
            "-i",
            &stage(),
            "--pass",
            "/Render/Passes/render",
            "-s",
            "1",
            "-o",
            "$l_$n_$o.png",
        ],
    );
    assert!(!out.status.success());
    assert!(log(&out).contains(".png"), "{}", log(&out));
    assert!(tree(&dir).is_empty());
}
