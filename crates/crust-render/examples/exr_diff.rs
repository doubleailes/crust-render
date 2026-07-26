//! Numeric diff of two rendered EXRs — the check that a change to the
//! intersection kernel did not change the image.
//!
//! ```text
//! cargo run --release -p crust-render --example exr_diff -- a.exr b.exr
//! ```
//!
//! Prints the number of differing pixels, the largest absolute and
//! relative channel difference, and the mean absolute difference. A pure
//! performance change should report either zero differing pixels or a
//! handful at the float epsilon (exact-tie ordering inside a BVH leaf),
//! never a structural difference.

use exr::prelude::*;

fn load(path: &str) -> (usize, usize, Vec<f32>) {
    let image = read_first_rgba_layer_from_file(
        path,
        |resolution, _| {
            let (w, h) = (resolution.width(), resolution.height());
            (w, h, vec![0.0f32; w * h * 4])
        },
        |(w, _h, pixels): &mut (usize, usize, Vec<f32>), pos, (r, g, b, a): (f32, f32, f32, f32)| {
            let i = (pos.y() * *w + pos.x()) * 4;
            pixels[i] = r;
            pixels[i + 1] = g;
            pixels[i + 2] = b;
            pixels[i + 3] = a;
        },
    )
    .unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    image.layer_data.channel_data.pixels
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: exr_diff <a.exr> <b.exr>");
        std::process::exit(2);
    }
    let (aw, ah, a) = load(&args[0]);
    let (bw, bh, b) = load(&args[1]);
    assert_eq!((aw, ah), (bw, bh), "resolutions differ");

    let mut differing_pixels = 0usize;
    let mut max_abs = 0.0f32;
    let mut max_rel = 0.0f32;
    let mut sum_abs = 0.0f64;
    for p in 0..aw * ah {
        let mut pixel_differs = false;
        for c in 0..3 {
            let (x, y) = (a[p * 4 + c], b[p * 4 + c]);
            let d = (x - y).abs();
            sum_abs += d as f64;
            if d != 0.0 {
                pixel_differs = true;
                max_abs = max_abs.max(d);
                let scale = x.abs().max(y.abs());
                if scale > 0.0 {
                    max_rel = max_rel.max(d / scale);
                }
            }
        }
        if pixel_differs {
            differing_pixels += 1;
            if differing_pixels <= 8 {
                println!(
                    "  differs at ({}, {}): {:?} vs {:?}",
                    p % aw,
                    p / aw,
                    &a[p * 4..p * 4 + 3],
                    &b[p * 4..p * 4 + 3]
                );
            }
        }
    }
    let total = aw * ah;
    println!(
        "{}x{}  differing pixels: {differing_pixels}/{total} ({:.4}%)",
        aw,
        ah,
        100.0 * differing_pixels as f64 / total as f64
    );
    println!("max abs diff: {max_abs:e}   max rel diff: {max_rel:e}");
    println!("mean abs diff: {:e}", sum_abs / (total * 3) as f64);
}
