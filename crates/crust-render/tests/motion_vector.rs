//! The `motionvector` AOV on the EXR the CLI writes for
//! `samples/motionvector.usda`, checked in numbers against an independent
//! pinhole projection of the `P` channel the same product carries: the
//! right-moving sphere moves right by the predicted pixels, the rising one
//! has `v > 0` (up is up in the file, not in some internal buffer), the
//! receding floor's vectors shorten with distance, the background is zero,
//! an edge pixel never blends, and motion blur changes nothing.

use exr::prelude::{ReadChannels, ReadLayers, read};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const W: usize = 128;
const H: usize = 72;
/// `samples/motionvector.usda`'s camera: at `CAM`, looking down −Z, 24 mm
/// over a 20.955 mm horizontal aperture.
const CAM: [f64; 3] = [0.0, 1.0, 8.0];
const FOCAL: f64 = 24.0;
const H_APERTURE: f64 = 20.955;

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/motionvector.usda")
        .canonicalize()
        .expect("samples/motionvector.usda")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("crust_motion_vector_cli")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Renders `stage` at 16 spp, unbiased, into `out`.
fn render(stage: &Path, out: &Path) {
    let status = Command::new(env!("CARGO_BIN_EXE_crust"))
        .args(["render", "-i"])
        .arg(stage)
        .arg("-o")
        .arg(out)
        .args(["-s", "16", "--indirect-clamp", "0", "-l", "error"])
        .status()
        .expect("run crust");
    assert!(status.success(), "crust render failed: {status}");
}

/// The product's channels, each `W × H` top-down as the file stores them.
struct Exr {
    channels: HashMap<String, Vec<f32>>,
}

impl Exr {
    fn load(path: &Path) -> Exr {
        let image = read()
            .no_deep_data()
            .largest_resolution_level()
            .all_channels()
            .first_valid_layer()
            .all_attributes()
            .from_file(path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let layer = &image.layer_data;
        assert_eq!(layer.size, (W, H).into());
        let channels = layer
            .channel_data
            .list
            .iter()
            .map(|c| {
                let values = (0..W * H)
                    .map(|i| c.sample_data.value_by_flat_index(i).to_f32())
                    .collect();
                (c.name.to_string(), values)
            })
            .collect();
        Exr { channels }
    }

    /// `channel` at column `x`, row `y` from the top.
    fn at(&self, channel: &str, x: usize, y: usize) -> f32 {
        let c = self
            .channels
            .get(channel)
            .unwrap_or_else(|| panic!("no channel {channel}; have {:?}", self.names()));
        c[y * W + x]
    }

    fn names(&self) -> Vec<&String> {
        let mut n: Vec<&String> = self.channels.keys().collect();
        n.sort();
        n
    }

    /// `(forward.u, forward.v)` at a pixel.
    fn forward(&self, x: usize, y: usize) -> (f32, f32) {
        (self.at("forward.u", x, y), self.at("forward.v", x, y))
    }

    /// The first hit's world position at a pixel.
    fn p(&self, x: usize, y: usize) -> [f64; 3] {
        [
            self.at("P.X", x, y) as f64,
            self.at("P.Y", x, y) as f64,
            self.at("P.Z", x, y) as f64,
        ]
    }
}

/// Where world point `p` lands on the image, in pixels from the bottom-left
/// corner (`x` right, `y` up), through the sample's camera as a pinhole:
/// `W · focal / aperture` pixels per unit of `x / depth`, and the same per
/// unit of `y / depth` (the vertical aperture is the horizontal one over the
/// aspect ratio, so pixels are square).
fn screen(p: [f64; 3]) -> (f64, f64) {
    let px_per_unit = W as f64 * FOCAL / H_APERTURE;
    let depth = CAM[2] - p[2];
    (
        W as f64 / 2.0 + px_per_unit * (p[0] - CAM[0]) / depth,
        H as f64 / 2.0 + px_per_unit * (p[1] - CAM[1]) / depth,
    )
}

/// The pixel (column, row from the top) holding world point `p`.
fn pixel(p: [f64; 3]) -> (usize, usize) {
    let (sx, sy) = screen(p);
    (sx.floor() as usize, (H as f64 - sy).floor() as usize)
}

/// The vector a hit at `p` moving by `v` over the shutter should carry.
fn predicted(p: [f64; 3], v: [f64; 3]) -> (f64, f64) {
    let (x0, y0) = screen(p);
    let (x1, y1) = screen([p[0] + v[0], p[1] + v[1], p[2] + v[2]]);
    (x1 - x0, y1 - y0)
}

fn close(got: (f32, f32), want: (f64, f64), tol: f64) -> bool {
    (got.0 as f64 - want.0).abs() < tol && (got.1 as f64 - want.1).abs() < tol
}

/// Renders the sample as authored into a directory of its own: tests run
/// in parallel, and `work_dir` clears the directory it is given.
fn rendered_sample(test: &str) -> Exr {
    let out = work_dir(&format!("{test}_sharp")).join("mv.exr");
    render(&sample(), &out);
    Exr::load(&out)
}

const RIGHT_CENTRE: [f64; 3] = [-1.5, 1.0, 0.0];
const RIGHT_V: [f64; 3] = [1.2, 0.0, 0.0];
const UP_CENTRE: [f64; 3] = [1.5, 0.6, 0.0];
const UP_V: [f64; 3] = [0.0, 1.0, 0.0];
const FLOOR_V: [f64; 3] = [0.8, 0.0, 0.0];
const CARD_V: [f64; 3] = [0.5, 0.3, 0.0];

#[test]
fn the_written_vectors_match_an_independent_projection() {
    let exr = rendered_sample("projection");
    assert!(
        exr.channels.contains_key("forward.u") && exr.channels.contains_key("forward.v"),
        "{:?}",
        exr.names()
    );

    // The right-moving sphere, at its centre pixel: `u` is the projection
    // difference of the hit and the hit plus the translation, `v` is 0.
    let (x, y) = pixel(RIGHT_CENTRE);
    let p = exr.p(x, y);
    assert!(
        p[2] > 0.3,
        "the centre pixel hits the sphere's front: {p:?}"
    );
    let got = exr.forward(x, y);
    let want = predicted(p, RIGHT_V);
    assert!(got.0 > 0.0 && close(got, want, 0.01), "{got:?} vs {want:?}");
    assert!(got.1.abs() < 0.01, "{got:?}");

    // The rising sphere: `v` positive, up is up in the file.
    let (x, y) = pixel(UP_CENTRE);
    let p = exr.p(x, y);
    assert!(p[2] > 0.3, "{p:?}");
    let got = exr.forward(x, y);
    let want = predicted(p, UP_V);
    assert!(got.1 > 0.0 && close(got, want, 0.01), "{got:?} vs {want:?}");
    assert!(got.0.abs() < 0.01, "{got:?}");

    // The receding floor, down the middle column (its near edge, 5 units
    // away, sits about 7 rows above the frame's bottom): on the floor at
    // both rows, longer near the camera than far from it, each as predicted.
    let (near, far) = (exr.forward(W / 2, 60), exr.forward(W / 2, 47));
    let (p_near, p_far) = (exr.p(W / 2, 60), exr.p(W / 2, 47));
    assert!(
        p_near[1].abs() < 1e-3 && p_far[1].abs() < 1e-3,
        "{p_near:?} {p_far:?}"
    );
    assert!(
        p_near[2] > 0.0 && p_near[2] > p_far[2] + 5.0 && p_far[2] < -3.0,
        "{p_near:?} {p_far:?}"
    );
    assert!(near.0 > far.0 && far.0 > 0.0, "{near:?} {far:?}");
    assert!(close(near, predicted(p_near, FLOOR_V), 0.01));
    assert!(close(far, predicted(p_far, FLOOR_V), 0.01));

    // The background (the dome above everything): zero, and no position.
    for x in [3, 40, W / 2, 90, W - 4] {
        assert_eq!(exr.forward(x, 2), (0.0, 0.0), "column {x}");
        assert_eq!(exr.p(x, 2), [0.0; 3], "column {x}");
    }

    // Along the row through the right-moving sphere's centre, left half of
    // the frame: every pixel is the background's (0, 0) or a sphere hit
    // with its own predicted vector, never a blend of the two.
    let y = pixel(RIGHT_CENTRE).1;
    let (mut hits, mut misses) = (0, 0);
    for x in 0..60 {
        let p = exr.p(x, y);
        let got = exr.forward(x, y);
        if p == [0.0; 3] {
            assert_eq!(got, (0.0, 0.0), "column {x}");
            misses += 1;
        } else {
            let want = predicted(p, RIGHT_V);
            assert!(close(got, want, 0.01), "column {x}: {got:?} vs {want:?}");
            hits += 1;
        }
    }
    assert!(hits > 5 && misses > 5, "{hits} hits, {misses} misses");
}

/// The vectors do not depend on the beauty's blur. With blur on, a pixel's
/// closest sample hits the moving object at some shutter time and is
/// rebased to shutter open, so it still carries the vector its own hit
/// predicts. On the card, which faces the camera at one depth, every
/// interior pixel holds one vector, blur or not.
#[test]
fn motion_blur_does_not_change_the_vectors() {
    let sharp = rendered_sample("blur");
    let dir = work_dir("blur_blurred");
    let stage = dir.join("motionvector_blurred.usda");
    let text = std::fs::read_to_string(sample()).expect("read sample");
    assert!(text.contains("uniform bool disableMotionBlur = 1"));
    std::fs::write(
        &stage,
        text.replace(
            "uniform bool disableMotionBlur = 1",
            "uniform bool disableMotionBlur = 0",
        ),
    )
    .expect("write stage");
    let out = dir.join("mv.exr");
    render(&stage, &out);
    let blurred = Exr::load(&out);

    // The card: z = −2 in both renders means both chosen samples hit it.
    let (cx, cy) = pixel([0.0, 2.1, -2.0]);
    let mut compared = 0;
    for y in cy - 8..cy + 8 {
        for x in cx - 8..cx + 8 {
            let (a, b) = (sharp.p(x, y), blurred.p(x, y));
            if (a[2] + 2.0).abs() < 1e-3 && (b[2] + 2.0).abs() < 1e-3 {
                let (s, m) = (sharp.forward(x, y), blurred.forward(x, y));
                assert!(
                    (s.0 - m.0).abs() < 1e-3 && (s.1 - m.1).abs() < 1e-3,
                    "({x}, {y}): sharp {s:?}, blurred {m:?}"
                );
                assert!(close(s, predicted(a, CARD_V), 0.01), "({x}, {y}): {s:?}");
                compared += 1;
            }
        }
    }
    assert!(compared > 20, "{compared} card pixels compared");

    // The right-moving sphere: with blur on, the chosen sample's hit is
    // where the sphere was at its shutter time, and its vector is the one
    // that hit predicts (the rebase); the sharp render's differs only by
    // the hit point — a sphere's depth varies across a pixel, so the two
    // agree to a fraction of a pixel, not to rounding as the card does.
    let (cx, cy) = pixel(RIGHT_CENTRE);
    let mut compared = 0;
    for y in cy - 6..cy + 6 {
        for x in cx - 6..cx + 6 {
            let (a, b) = (sharp.p(x, y), blurred.p(x, y));
            if a[2] > 0.3 && b[2] > 0.3 {
                let (s, m) = (sharp.forward(x, y), blurred.forward(x, y));
                assert!(close(m, predicted(b, RIGHT_V), 0.01), "({x}, {y}): {m:?}");
                assert!(close(s, predicted(a, RIGHT_V), 0.01), "({x}, {y}): {s:?}");
                assert!(
                    (s.0 - m.0).abs() < 0.5,
                    "({x}, {y}): sharp {s:?}, blurred {m:?}"
                );
                compared += 1;
            }
        }
    }
    assert!(compared > 20, "{compared} sphere pixels compared");
}
