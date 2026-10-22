//! `crust render --all-passes` (the `render-passes` spec's "Every pass of a
//! stage in one render", "Each pass renders as it does alone" and "Passes
//! share an import", and the `cli` spec's "Passes that would write the same
//! file").
//!
//! Rendered on `samples/render_passes.usda` at `-s 4` over a region, so a
//! debug build renders each pass in seconds; `crust diff` compares the files.

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
    let dir = std::env::temp_dir().join("crust_all_passes").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `samples/render_passes.usda` with each `(from, to)` replaced, written into
/// `dir`.
fn stage_with(dir: &Path, edits: &[(&str, &str)]) -> PathBuf {
    let mut text = std::fs::read_to_string(sample("render_passes.usda")).expect("read");
    for (from, to) in edits {
        assert!(text.contains(from), "the sample holds no {from:?}");
        text = text.replace(from, to);
    }
    let path = dir.join("stage.usda");
    std::fs::write(&path, text).expect("write stage");
    path
}

/// `crust <args>`, run in `dir`, its log at info on stdout.
fn crust(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run crust")
}

const FAST: [&str; 4] = ["-s", "4", "--region", "40,20,104,60"];

fn render(dir: &Path, stage: &Path, extra: &[&str]) -> Output {
    let stage = stage.to_str().unwrap();
    let args: Vec<&str> = ["render", "-i", stage]
        .into_iter()
        .chain(FAST)
        .chain(extra.iter().copied())
        .collect();
    crust(dir, &args)
}

fn log(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn imports(out: &Output) -> usize {
    log(out).matches("once for ").count()
}

/// Both passes' files, each identical to the pass rendered alone, from one
/// import; `char.exr` first.
#[test]
fn every_pass_renders_as_it_does_alone_from_one_import() {
    let all = work_dir("all");
    let stage = sample("render_passes.usda");
    let out = render(&all, &stage, &["--all-passes"]);
    assert!(out.status.success(), "{}", log(&out));
    assert_eq!(
        imports(&out),
        1,
        "one import for both passes:\n{}",
        log(&out)
    );
    let text = log(&out);
    let (char_at, props_at) = (
        text.find("Pass /Render/Passes/char")
            .expect("char rendered"),
        text.find("Pass /Render/Passes/props")
            .expect("props rendered"),
    );
    assert!(char_at < props_at, "in `crust ls pass` order");
    for pass in ["char", "props"] {
        let alone = work_dir(&format!("alone_{pass}"));
        let out = render(
            &alone,
            &stage,
            &["--pass", &format!("/Render/Passes/{pass}")],
        );
        assert!(out.status.success(), "{}", log(&out));
        let (a, b) = (
            alone.join(format!("{pass}.exr")),
            all.join(format!("{pass}.exr")),
        );
        let diff = crust(&all, &["diff", a.to_str().unwrap(), b.to_str().unwrap()]);
        assert!(diff.status.success(), "{pass}: {}", log(&diff));
    }
}

/// A stage that defines no pass stops with an error and writes nothing.
#[test]
fn a_stage_without_passes_fails() {
    let dir = work_dir("no_pass");
    let out = render(&dir, &sample("cornellbox.usda"), &["--all-passes"]);
    assert!(!out.status.success());
    assert!(log(&out).contains("defines no RenderPass"), "{}", log(&out));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

/// Two passes with no products would both write the default beauty: the
/// render stops before importing, naming both and the file.
#[test]
fn two_passes_on_the_default_path_fail_before_rendering() {
    let dir = work_dir("shared_output");
    let stage = stage_with(
        &dir,
        &[
            ("        rel products = </Render/Products/props>\n", ""),
            ("        rel products = </Render/Products/char>\n", ""),
        ],
    );
    let out = render(&dir, &stage, &["--all-passes"]);
    assert!(!out.status.success());
    let text = log(&out);
    assert!(
        text.contains("/Render/Passes/char and /Render/Passes/props would all write output.exr"),
        "{text}"
    );
    assert_eq!(imports(&out), 0, "{text}");
    assert!(!text.contains("Rendering "), "{text}");
    assert!(!dir.join("output.exr").exists());
}

/// A pass whose settings dice differently imports apart.
#[test]
fn a_pass_that_dices_differently_imports_twice() {
    let dir = work_dir("dicing");
    let stage = stage_with(
        &dir,
        &[(
            "        rel products = </Render/Products/char>\n",
            "        rel products = </Render/Products/char>\n        int crust:subdivisionLevel = 1\n",
        )],
    );
    let out = render(&dir, &stage, &["--all-passes"]);
    assert!(out.status.success(), "{}", log(&out));
    assert_eq!(imports(&out), 2, "{}", log(&out));
    assert!(dir.join("char.exr").exists() && dir.join("props.exr").exists());
}

/// Two spellings of one file are one file: a pass writing `./char.exr`
/// beside one writing `char.exr` stops the render before it imports.
#[test]
fn two_spellings_of_one_file_fail_before_rendering() {
    let dir = work_dir("spelling");
    let stage = stage_with(
        &dir,
        &[(
            "token productName = \"props.exr\"",
            "token productName = \"./char.exr\"",
        )],
    );
    let out = render(&dir, &stage, &["--all-passes"]);
    assert!(!out.status.success());
    let text = log(&out);
    assert!(text.contains("would all write"), "{text}");
    assert_eq!(imports(&out), 0, "{text}");
    assert!(!dir.join("char.exr").exists());
}
