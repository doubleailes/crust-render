//! A precomp end to end through `crust render` (the `image-output` spec's
//! "Tone-mapped sRGB PNG conversion next to the EXR" and "EXRs record the
//! precomp", and `precomp-filters`' "The displayed output"): the PNG shows
//! the displayed output while the EXR's beauty stays the beauty, `--checkpoint`
//! ends with the files a render without it writes, and every EXR names the
//! filters that made it.
//!
//! Each render is cropped to a small region, so a debug build renders it in
//! seconds.

use exr::meta::attribute::AttributeValue;
use exr::prelude::{ReadChannels, ReadLayers, read};
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
    let dir = std::env::temp_dir().join("crust_precomp_cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `crust render -i <stage> -o <name>.exr` over a small crop, in `dir`; its
/// log.
fn render(dir: &Path, stage: &Path, name: &str, extra: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(dir)
        .arg("render")
        .arg("-i")
        .arg(stage)
        .args([
            "-o",
            &format!("{name}.exr"),
            "-s",
            "4",
            "--region",
            "90,40,250,130",
            "-l",
            "debug",
        ])
        .args(extra)
        .output()
        .expect("run crust");
    let log = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{log}");
    log
}

/// The sample stage with `crust:display` set to `display` (`None` leaves it
/// unauthored, removed from the sample), as a layer in `dir`.
fn displaying(dir: &Path, display: Option<&str>) -> PathBuf {
    let text = std::fs::read_to_string(sample("precomp.usda")).unwrap();
    let text = match display {
        Some(d) => text.replace(
            "token crust:display = \"look_final\"",
            &format!("token crust:display = \"{d}\""),
        ),
        None => text.replace("token crust:display = \"look_final\"", ""),
    };
    let path = dir.join(format!("precomp_{}.usda", display.unwrap_or("none")));
    std::fs::write(&path, text).unwrap();
    path
}

/// An EXR's attributes and its channels' bits, by name.
#[allow(clippy::type_complexity)]
fn exr(path: &Path) -> (BTreeMap<String, AttributeValue>, BTreeMap<String, Vec<u32>>) {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let layer = &image.layer_data;
    let attributes = layer
        .attributes
        .other
        .iter()
        .filter(|(k, _)| k.to_string().starts_with("crust:"))
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let channels = layer
        .channel_data
        .list
        .iter()
        .map(|c| {
            let bits = (0..layer.size.area())
                .map(|i| c.sample_data.value_by_flat_index(i).to_f32().to_bits())
                .collect();
            (c.name.to_string(), bits)
        })
        .collect();
    (attributes, channels)
}

fn text(attributes: &BTreeMap<String, AttributeValue>, name: &str) -> String {
    match attributes.get(name) {
        Some(AttributeValue::Text(t)) => t.to_string(),
        other => panic!("{name}: {other:?}"),
    }
}

/// The PNG shows the displayed output; the EXR's beauty does not move.
#[test]
fn the_png_shows_the_displayed_output_and_the_beauty_stays() {
    let dir = work_dir("png");
    let png = |name: &str| std::fs::read(dir.join(format!("{name}.png"))).unwrap();
    render(&dir, &displaying(&dir, Some("look_final")), "look", &[]);
    render(&dir, &displaying(&dir, Some("C")), "beauty_var", &[]);
    render(&dir, &displaying(&dir, None), "unauthored", &[]);
    // Displaying the beauty var is displaying nothing.
    assert!(png("beauty_var") == png("unauthored"));
    assert!(png("look") != png("unauthored"), "the look is shown");
    let (look, beauty_var, unauthored) = (
        exr(&dir.join("look.exr")),
        exr(&dir.join("beauty_var.exr")),
        exr(&dir.join("unauthored.exr")),
    );
    for c in ["R", "G", "B", "look.R", "look_final.G"] {
        assert_eq!(look.1[c], unauthored.1[c], "{c}");
        assert_eq!(beauty_var.1[c], unauthored.1[c], "{c}");
    }
    assert_eq!(text(&look.0, "crust:display"), "look_final");
    assert_eq!(text(&beauty_var.0, "crust:display"), "C");
    assert_eq!(text(&unauthored.0, "crust:display"), "beauty");
}

/// Every EXR names its precomp's filters, in order; a stage without one
/// writes them empty.
#[test]
fn every_exr_records_its_precomp() {
    let dir = work_dir("stamp");
    render(&dir, &sample("precomp.usda"), "precomp", &[]);
    let (attributes, _) = exr(&dir.join("precomp.exr"));
    assert_eq!(
        text(&attributes, "crust:sampleFilters"),
        "/Render/Precomp/look"
    );
    assert_eq!(
        text(&attributes, "crust:displayFilters"),
        "/Render/Precomp/glow,/Render/Precomp/bloom,/Render/Precomp/speed"
    );
    assert_eq!(text(&attributes, "crust:display"), "look_final");

    render(&dir, &sample("cornellbox.usda"), "plain", &[]);
    let (attributes, _) = exr(&dir.join("plain.exr"));
    assert_eq!(text(&attributes, "crust:sampleFilters"), "");
    assert_eq!(text(&attributes, "crust:displayFilters"), "");
    assert_eq!(text(&attributes, "crust:display"), "beauty");
}

/// `--checkpoint` on a stage with a displayed output rewrites the preview
/// while the render runs and ends with the files a render without it
/// writes: the displayed output's PNG, byte for byte.
#[test]
fn a_checkpointed_precomp_ends_with_the_files_of_one_without() {
    let dir = work_dir("checkpoint");
    let stage = sample("precomp.usda");
    let plain = render(&dir, &stage, "plain", &[]);
    assert!(!plain.contains("--checkpoint"), "{plain}");
    let watched = render(&dir, &stage, "watched", &["--checkpoint", "0.02"]);
    assert!(
        watched.contains("watched.png rewritten"),
        "no preview was rewritten while the render ran:\n{watched}"
    );
    let png = |name: &str| std::fs::read(dir.join(format!("{name}.png"))).unwrap();
    assert!(png("plain") == png("watched"), "the PNG differs");
    assert_eq!(
        exr(&dir.join("plain.exr")).1,
        exr(&dir.join("watched.exr")).1,
        "the EXR pixels differ"
    );
}
