//! The sampling stamp, end to end: every EXR `crust render` writes — the
//! single beauty of a stage without products, and each RenderProduct —
//! records how it was sampled and what it was rendered from, as typed
//! `crust:*` header attributes (the `image-output` spec).
//!
//! Each render is cropped to a small region: the stamp does not depend on
//! the pixels, and a debug build then renders in seconds.

use exr::meta::attribute::AttributeValue;
use exr::prelude::{ReadChannels, ReadLayers, Text, Vec2, read};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("samples/{name}: {e}"))
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_stamp_cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `crust render -i <stage> <args>`, run in `dir`.
fn render(dir: &Path, stage: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(dir)
        .arg("render")
        .arg("-i")
        .arg(stage)
        .args(["-l", "error"])
        .args(args)
        .output()
        .expect("run crust");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The `crust:*` attributes of an EXR's first layer.
fn stamp(path: &Path) -> BTreeMap<String, AttributeValue> {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    image
        .layer_data
        .attributes
        .other
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .filter(|(k, _)| k.starts_with("crust:"))
        .collect()
}

fn text(s: &str) -> AttributeValue {
    AttributeValue::Text(Text::from(s))
}

#[test]
fn the_single_beauty_exr_is_stamped() {
    let dir = work_dir("beauty");
    render(
        &dir,
        &sample("cornellbox.usda"),
        &[
            "-o",
            "out.exr",
            "-s",
            "16",
            "--indirect-clamp",
            "0",
            "--region",
            "0,0,16,16",
        ],
    );
    let s = stamp(&dir.join("out.exr"));
    assert_eq!(s["crust:spp"], AttributeValue::I32(16));
    assert_eq!(s["crust:minSpp"], AttributeValue::I32(32));
    assert_eq!(s["crust:sppTaken"], AttributeValue::IntVec2(Vec2(16, 16)));
    assert_eq!(s["crust:indirectClamp"], AttributeValue::F32(0.0));
    assert!(matches!(
        s["crust:varianceThreshold"],
        AttributeValue::F32(_)
    ));
    assert!(matches!(s["crust:maxDepth"], AttributeValue::I32(_)));
    assert_eq!(s["crust:lightSamples"], AttributeValue::I32(1));
    assert_eq!(s["crust:lightSamplesIndirect"], AttributeValue::I32(1));
    assert!(matches!(
        s["crust:samplingStrategy"],
        AttributeValue::Text(_)
    ));
    assert!(matches!(s["crust:lightSelection"], AttributeValue::Text(_)));
    assert!(matches!(s["crust:pixelFilter"], AttributeValue::Text(_)));
    assert!(matches!(
        s["crust:pixelFilterRadius"],
        AttributeValue::F32(_)
    ));
    assert_eq!(s["crust:camera"], text("/scene/camera1"));
    assert_eq!(s["crust:version"], text(env!("CARGO_PKG_VERSION")));
    assert!(!s.contains_key("crust:frame"), "no -f, no frame");
}

#[test]
fn the_filter_and_the_subframe_are_recorded() {
    let dir = work_dir("filter");
    render(
        &dir,
        &sample("cornellbox.usda"),
        &[
            "-o",
            "out.exr",
            "-s",
            "1",
            "--region",
            "0,0,8,8",
            "--filter",
            "gaussian",
            "--filter-radius",
            "2",
            "-f",
            "10.5",
        ],
    );
    let s = stamp(&dir.join("out.exr"));
    assert_eq!(s["crust:pixelFilter"], text("gaussian"));
    assert_eq!(s["crust:pixelFilterRadius"], AttributeValue::F32(2.0));
    assert_eq!(s["crust:frame"], AttributeValue::F64(10.5));
}

#[test]
fn the_procedural_scene_has_no_frame_and_no_camera() {
    let dir = work_dir("procedural");
    let out = Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(&dir)
        .args([
            "render", "-o", "out.exr", "-s", "1", "--region", "0,0,8,8", "-l", "error",
        ])
        .output()
        .expect("run crust");
    assert!(out.status.success());
    let s = stamp(&dir.join("out.exr"));
    assert!(s.contains_key("crust:spp"));
    assert!(!s.contains_key("crust:frame"));
    assert!(!s.contains_key("crust:camera"));
}

#[test]
fn every_product_carries_the_same_stamp() {
    let dir = work_dir("products");
    render(
        &dir,
        &sample("aovs.usda"),
        &["-s", "16", "--indirect-clamp", "0", "--region", "0,0,16,16"],
    );
    let beauty = stamp(&dir.join("renders/aovs_beauty.exr"));
    let data = stamp(&dir.join("renders/aovs_data.exr"));
    assert_eq!(beauty, data);
    assert_eq!(beauty["crust:spp"], AttributeValue::I32(16));
    assert_eq!(beauty["crust:minSpp"], AttributeValue::I32(8));
    assert_eq!(beauty["crust:indirectClamp"], AttributeValue::F32(0.0));
    assert_eq!(beauty["crust:camera"], text("/World/Cam"));
    let AttributeValue::IntVec2(Vec2(lo, hi)) = beauty["crust:sppTaken"] else {
        panic!("sppTaken is a v2i");
    };
    assert!(8 <= lo && lo <= hi && hi <= 16, "{lo}..{hi}");
}

/// The sampler is stamped: `--sampler latticeBn` as `latticeBn`, and a
/// render with no sampler setting as `sobol`.
#[test]
fn the_sampler_is_recorded() {
    let dir = work_dir("sampler");
    let common = ["-s", "1", "--region", "0,0,8,8"];
    render(
        &dir,
        &sample("cornellbox.usda"),
        &[&["-o", "bn.exr", "--sampler", "latticeBn"][..], &common].concat(),
    );
    assert_eq!(
        stamp(&dir.join("bn.exr"))["crust:sampler"],
        text("latticeBn")
    );
    render(
        &dir,
        &sample("cornellbox.usda"),
        &[&["-o", "default.exr"][..], &common].concat(),
    );
    assert_eq!(
        stamp(&dir.join("default.exr"))["crust:sampler"],
        text("sobol")
    );
}

/// An override layer that sublayers the shot, opens the lens and hides a
/// prim: its EXR records the lens, depth of field on, both layers strongest
/// first with their content hashes, and the one prim it hid; `crust diff`
/// against the shot's own render names the depth of field on both sides.
#[test]
fn the_lens_the_layer_stack_and_the_overrides_are_recorded() {
    let dir = work_dir("lens");
    let shot = sample("cornellbox.usda");
    let layer = dir.join("override.usda");
    std::fs::write(
        &layer,
        format!(
            "#usda 1.0\n(\n    subLayers = [@{}@]\n)\nover \"scene\"\n{{\n    \
             over \"camera1\"\n    {{\n        float fStop = 0.02\n        \
             float focusDistance = 360\n    }}\n    over \"pTorus1\"\n    {{\n        \
             token visibility = \"invisible\"\n    }}\n}}\n",
            shot.display()
        ),
    )
    .expect("write the override layer");
    let args = ["-s", "1", "--region", "0,0,8,8"];
    render(&dir, &shot, &[&["-o", "a.exr"][..], &args].concat());
    render(&dir, &layer, &[&["-o", "b.exr"][..], &args].concat());

    let a = stamp(&dir.join("a.exr"));
    assert_eq!(a["crust:depthOfField"], AttributeValue::I32(0));
    assert_eq!(a["crust:motionBlur"], AttributeValue::I32(0));
    assert_eq!(a["crust:invisibleOverrides"], AttributeValue::I32(0));
    assert!(matches!(a["crust:focalLength"], AttributeValue::F32(_)));
    assert_eq!(a["crust:aspectRatioConformPolicy"], text("expandAperture"));

    let b = stamp(&dir.join("b.exr"));
    assert_eq!(b["crust:fStop"], AttributeValue::F32(0.02));
    assert_eq!(b["crust:focusDistance"], AttributeValue::F32(360.0));
    assert_eq!(b["crust:depthOfField"], AttributeValue::I32(1));
    assert_eq!(b["crust:invisibleOverrides"], AttributeValue::I32(1));
    let hash =
        |p: &Path| crust_core::stamp::content_hash(&std::fs::read(p).expect("read the layer"));
    let stack = format!(
        "{:016x} {}\n{:016x} {}",
        hash(&layer),
        layer.display(),
        hash(&shot),
        shot.display()
    );
    assert_eq!(b["crust:layerStack"], text(&stack));

    let out = Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(&dir)
        .args(["diff", "a.exr", "b.exr"])
        .output()
        .expect("run crust diff");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("depth of field differs (a: no depth of field; b: fStop 0.02, focus 360)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("crust:invisibleOverrides differs (0 vs 1)"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("crust:layerStack: {} only in b", layer.display())),
        "{stderr}"
    );
}
