//! `crust render --region`: a crop is written as an EXR whose display window
//! is the frame and whose data window is the region, with the full render's
//! pixels there, and a PNG of the region alone; a region that is malformed,
//! or outside the frame, is refused without writing anything.
//!
//! The crop is compared at `-s 16`, as every image comparison here is
//! (CLAUDE.md, "Measuring a change"), on `samples/dome_backdrop.usda`
//! (240×136): small enough to render twice at 16 spp in a debug build, and
//! still a frame the 64×64 crop sits inside away from every edge.

use exr::prelude::{IntegerBounds, ReadChannels, ReadLayers, Vec2, read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("samples/{name}: {e}"))
}

/// `samples/dome_backdrop.usda`'s resolution.
const W: usize = 240;
const H: usize = 136;

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_region_cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn crust(args: &[&str], stage: &Path, out: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crust"))
        .args(["render", "-i"])
        .arg(stage)
        .arg("-o")
        .arg(out)
        .args(["-s", "16", "-l", "error"])
        .args(args)
        .output()
        .expect("run crust")
}

/// An EXR's display window, data window and its `R` plane (top-down).
fn load(path: &Path) -> (IntegerBounds, IntegerBounds, Vec<f32>) {
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
    (
        image.attributes.display_window,
        layer.absolute_bounds(),
        plane,
    )
}

#[test]
fn a_crop_is_placed_in_the_frame_and_matches_it() {
    let dir = work_dir("crop");
    let (full, crop) = (dir.join("full.exr"), dir.join("crop.exr"));
    let stage = sample("dome_backdrop.usda");
    let out = crust(&[], &stage, &full);
    assert!(out.status.success(), "{out:?}");
    let out = crust(&["--region", "100,50,164,114"], &stage, &crop);
    assert!(out.status.success(), "{out:?}");

    let (display, data, pixels) = load(&crop);
    assert_eq!(display, IntegerBounds::new((0, 0), (W, H)));
    assert_eq!(data.position, Vec2(100, 50));
    assert_eq!(data.max(), Vec2(163, 113));
    let (_, full_data, full_pixels) = load(&full);
    assert_eq!(full_data, IntegerBounds::new((0, 0), (W, H)));
    let expected: Vec<f32> = (50..114)
        .flat_map(|y| (100..164).map(move |x| (x, y)))
        .map(|(x, y)| full_pixels[y * W + x])
        .collect();
    // Not vacuous: the crop sees something.
    assert!(pixels.iter().any(|&v| v > 0.0));
    assert!(
        pixels
            .iter()
            .map(|v| v.to_bits())
            .eq(expected.iter().map(|v| v.to_bits())),
        "the crop's pixels are not the full frame's"
    );

    let png = image::open(crop.with_extension("png")).expect("a PNG beside the EXR");
    assert_eq!((png.width(), png.height()), (64, 64));
}

#[test]
fn a_malformed_region_is_a_usage_error_before_loading() {
    let dir = work_dir("malformed");
    let out = dir.join("out.exr");
    // The stage does not exist: a usage error exits 2 before it is read.
    for region in ["10,10,5,20", "1,2,3", "-1,0,4,4", "a,0,4,4", "0,0,4,0"] {
        let result = crust(
            &["--region", region],
            Path::new("/no/such/stage.usda"),
            &out,
        );
        assert_eq!(result.status.code(), Some(2), "{region}: {result:?}");
    }
    assert!(!out.exists());
}

#[test]
fn a_region_outside_the_frame_names_the_resolution() {
    let dir = work_dir("outside");
    let out = dir.join("out.exr");
    // Cornell box: 640×360, authored by no RenderSettings (the default).
    let result = crust(
        &["--region", "700,0,800,100"],
        &sample("cornellbox.usda"),
        &out,
    );
    assert!(!result.status.success());
    let log = String::from_utf8_lossy(&result.stdout).into_owned()
        + &String::from_utf8_lossy(&result.stderr);
    assert!(log.contains("640x360"), "{log}");
    assert!(!out.exists() && !out.with_extension("png").exists());
}
