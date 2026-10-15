//! Output paths expand husk's frame variables (`$F4`, `<F4>`, `$FF`) before
//! anything is written: `-o` for a stage without products, and every
//! product's `productName`. The PNG preview follows the expanded EXR.
//!
//! Rendered at `-s 1` on `samples/dome_backdrop.usda` (240×136): only the
//! file names are checked, never the pixels. `-o` names a file in the
//! working directory: a render without products does not create `-o`'s
//! parent directories (products do), with or without variables.

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
    let dir = std::env::temp_dir().join("crust_output_paths").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `crust render -s 1 <args>`, run in `dir`.
fn render(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(dir)
        .args(["render", "-s", "1"])
        .args(args)
        .output()
        .expect("run crust")
}

/// The files under `dir`, relative and sorted.
fn files(dir: &Path) -> Vec<String> {
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

#[test]
fn a_padded_frame_in_o_names_the_exr_and_its_png() {
    let dir = work_dir("padded_o");
    let stage = sample("dome_backdrop.usda");
    let out = render(
        &dir,
        &["-i", stage.to_str().unwrap(), "-f", "12", "-o", "b.$F4.exr"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(files(&dir), ["b.0012.exr", "b.0012.png"]);
}

#[test]
fn a_subframe_rounds_and_warns() {
    let dir = work_dir("subframe_o");
    let stage = sample("dome_backdrop.usda");
    let out = render(
        &dir,
        &[
            "-i",
            stage.to_str().unwrap(),
            "-f",
            "12.5",
            "-o",
            "b.<F4>.exr",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(log.contains("[product.subframe_frame]"), "{log}");
    assert_eq!(files(&dir), ["b.0013.exr", "b.0013.png"]);
}

#[test]
fn a_path_without_variables_is_written_as_given() {
    let dir = work_dir("plain_o");
    let stage = sample("dome_backdrop.usda");
    let out = render(&dir, &["-i", stage.to_str().unwrap(), "-o", "plain.exr"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stdout);
    assert!(!log.contains("[product."), "{log}");
    assert_eq!(files(&dir), ["plain.exr", "plain.png"]);
}

/// A stage whose every product is refused writes its beauty to `-o`, which
/// was already expanded as the first product's name: its issues are warned
/// about once, not again for the fallback.
#[test]
fn a_refused_products_fallback_warns_about_o_once() {
    let dir = work_dir("refused_fallback");
    let stage = dir.join("stage.usda");
    std::fs::write(
        &stage,
        r#"#usda 1.0
(
    defaultPrim = "World"
)

def Xform "World"
{
    def Camera "cam"
    {
    }
}

def Scope "Render"
{
    def RenderSettings "settings"
    {
        rel camera = </World/cam>
        rel products = [</Render/main>]
        uniform int2 resolution = (16, 8)
    }
    def RenderProduct "main"
    {
        token productName = "main.exr"
        rel orderedVars = [</Render/intrinsic>]
    }
    def RenderVar "intrinsic"
    {
        uniform token dataType = "float"
        uniform token sourceType = "intrinsic"
        uniform string sourceName = "anything"
    }
}
"#,
    )
    .expect("write stage");
    let out = render(
        &dir,
        &[
            "-i",
            stage.to_str().unwrap(),
            "-f",
            "2.5",
            "-o",
            "b.$SHOT.$F2.exr",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stdout);
    for code in ["[product.unknown_variable]", "[product.subframe_frame]"] {
        assert_eq!(log.matches(code).count(), 1, "{code} in {log}");
    }
    assert_eq!(
        files(&dir),
        ["b.$SHOT.03.exr", "b.$SHOT.03.png", "stage.usda"]
    );
}
