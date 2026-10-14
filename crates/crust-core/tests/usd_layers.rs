//! `Scene::layers`: the file of every USD layer the import composed, which a
//! host watches to see the stage change on disk (`crust mcp`'s session).

use crust_core::Scene;
use std::path::{Path, PathBuf};

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_usd_layers").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.canonicalize().expect("canonical")
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write layer");
}

/// A layer holding one quad under `name`, for a reference or payload target.
fn quad(name: &str) -> String {
    format!(
        r#"#usda 1.0
(
    defaultPrim = "{name}"
)

def Mesh "{name}"
{{
    int[] faceVertexCounts = [4]
    int[] faceVertexIndices = [0, 1, 2, 3]
    point3f[] points = [(-1, 0, -1), (1, 0, -1), (1, 0, 1), (-1, 0, 1)]
}}
"#
    )
}

/// Writes a root layer sublayering `sub.usda`, with `subtrees` children of
/// `/World`: the first references `ref.usda`, the last payloads `pay.usda`
/// (so a streamed import reaches each from a different chunk). Returns the
/// root layer and every file the import should list.
fn stage(dir: &Path, subtrees: usize) -> (PathBuf, Vec<PathBuf>) {
    write(&dir.join("ref.usda"), &quad("Ref"));
    write(&dir.join("pay.usda"), &quad("Pay"));
    write(&dir.join("sub.usda"), "#usda 1.0\n\nover \"World\"\n{\n}\n");
    let mut children = String::new();
    for n in 0..subtrees {
        let arc = if n == 0 {
            "prepend references = @./ref.usda@".to_owned()
        } else if n + 1 == subtrees {
            "prepend payload = @./pay.usda@".to_owned()
        } else {
            String::new()
        };
        children.push_str(&format!(
            "    def Xform \"C{n}\" (\n        {arc}\n    )\n    {{\n    }}\n"
        ));
    }
    let root = dir.join("root.usda");
    write(
        &root,
        &format!(
            "#usda 1.0\n(\n    subLayers = [@./sub.usda@]\n)\n\ndef Xform \"World\"\n{{\n{children}}}\n"
        ),
    );
    let mut expected: Vec<PathBuf> = ["pay.usda", "ref.usda", "root.usda", "sub.usda"]
        .iter()
        .map(|f| dir.join(f))
        .collect();
    expected.sort();
    (root, expected)
}

#[test]
fn a_single_stage_import_lists_sublayers_references_and_payloads() {
    let dir = work_dir("single");
    // Two subtrees: too few to stream.
    let (root, expected) = stage(&dir, 2);
    let scene = Scene::from_usd(&root).expect("imports");
    assert_eq!(scene.layers, expected);
}

#[test]
fn a_streamed_import_lists_the_layers_of_every_chunk() {
    let dir = work_dir("streamed");
    // Six subtrees: streamed one masked stage per child, the reference
    // reached only from the first and the payload only from the last.
    let (root, expected) = stage(&dir, 6);
    let scene = Scene::from_usd(&root).expect("imports");
    assert_eq!(scene.layers, expected);
}

#[test]
fn a_layer_inside_a_package_is_listed_as_the_package() {
    let dir = work_dir("package");
    let package = dir.join("asset.usdz");
    let mut archive = openusd::usdz::ArchiveWriter::create(&package).expect("create usdz");
    archive
        .add_layer("asset.usda", quad("Asset").as_bytes())
        .expect("add layer");
    archive.finish().expect("finish usdz");
    let root = dir.join("root.usda");
    write(
        &root,
        "#usda 1.0\n\ndef Xform \"World\"\n{\n    def Xform \"A\" (\n        \
         prepend references = @./asset.usdz@\n    )\n    {\n    }\n}\n",
    );
    let scene = Scene::from_usd(&root).expect("imports");
    assert_eq!(scene.layers, vec![package, root]);
}
