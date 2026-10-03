//! Numeric diff of two rendered EXRs — the check that a change to the
//! intersection kernel did not change the image.
//!
//! ```text
//! cargo run --release -p crust-render --example exr_diff -- a.exr b.exr
//! ```
//!
//! Every named channel of every layer is compared, so a RenderProduct's AOVs
//! are checked as well as its beauty; a channel present in only one file is a
//! difference. The first line counts the pixels where *any* channel differs
//! (`scripts/check_images.sh` reads it), then one line per differing channel.
//!
//! On the beauty (`R`, `G`, `B`, when both files have them) it also prints the
//! largest absolute and relative channel difference, the mean absolute
//! difference, the RMSE and the relative MSE (against `a`, so pass the
//! reference first). A pure performance change should report either zero
//! differing pixels or a handful at the float epsilon (exact-tie ordering
//! inside a BVH leaf), never a structural difference.

use exr::prelude::*;
use std::collections::BTreeMap;

/// One channel: its layer's size, and its samples as f32 in row order.
struct Channel {
    size: (usize, usize),
    values: Vec<f32>,
}

/// Every channel of every layer, by full name (`layer.channel`, or the bare
/// channel name in an unnamed layer). `width`/`height` are the first
/// layer's; a multi-part file's other layers may differ, so each channel
/// keeps its own size.
struct Planes {
    width: usize,
    height: usize,
    channels: BTreeMap<String, Channel>,
}

fn load(path: &str) -> Planes {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .all_layers()
        .all_attributes()
        .from_file(path)
        .unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let mut channels = BTreeMap::new();
    let (width, height) = image
        .layer_data
        .first()
        .map_or((0, 0), |l| (l.size.width(), l.size.height()));
    for layer in &image.layer_data {
        let size = (layer.size.width(), layer.size.height());
        let prefix = layer
            .attributes
            .layer_name
            .as_ref()
            .map(|n| format!("{n}."))
            .unwrap_or_default();
        for channel in &layer.channel_data.list {
            let values = channel.sample_data.values_as_f32().collect();
            channels.insert(
                format!("{prefix}{}", channel.name),
                Channel { size, values },
            );
        }
    }
    Planes {
        width,
        height,
        channels,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: exr_diff <a.exr> <b.exr>");
        std::process::exit(2);
    }
    let a = load(&args[0]);
    let b = load(&args[1]);
    if (a.width, a.height) != (b.width, b.height) {
        println!(
            "resolutions differ: {}x{} vs {}x{}",
            a.width, a.height, b.width, b.height
        );
        std::process::exit(1);
    }
    let (aw, ah) = (a.width, a.height);
    let total = aw * ah;

    // Any channel: which pixels differ, and which channels.
    let mut pixel_differs = vec![false; total];
    let mut channel_lines = Vec::new();
    let names: Vec<&String> = {
        let mut n: Vec<&String> = a.channels.keys().chain(b.channels.keys()).collect();
        n.sort();
        n.dedup();
        n
    };
    for name in names {
        match (a.channels.get(name), b.channels.get(name)) {
            (Some(x), Some(y)) if x.size != y.size || x.size != (aw, ah) => {
                // A layer of its own size: no pixel lines up with the image's,
                // so the whole channel counts as different if the sizes do.
                if x.size != y.size {
                    channel_lines.push(format!(
                        "  channel {name}: sizes differ, {}x{} vs {}x{}",
                        x.size.0, x.size.1, y.size.0, y.size.1
                    ));
                    pixel_differs.iter_mut().for_each(|d| *d = true);
                } else if x
                    .values
                    .iter()
                    .zip(&y.values)
                    .any(|(p, q)| p.to_bits() != q.to_bits())
                {
                    channel_lines.push(format!(
                        "  channel {name}: differs (a {}x{} layer)",
                        x.size.0, x.size.1
                    ));
                    pixel_differs.iter_mut().for_each(|d| *d = true);
                }
            }
            (Some(x), Some(y)) => {
                let (x, y) = (&x.values, &y.values);
                let mut n = 0usize;
                let mut max_abs = 0.0f32;
                for p in 0..total {
                    // Bitwise, so a NaN or a signed zero that moved counts,
                    // and two equal infinities (a depth's clear value) do not.
                    if x[p].to_bits() != y[p].to_bits() {
                        n += 1;
                        pixel_differs[p] = true;
                        let d = (x[p] - y[p]).abs();
                        if d.is_finite() {
                            max_abs = max_abs.max(d);
                        }
                    }
                }
                if n > 0 {
                    channel_lines.push(format!(
                        "  channel {name}: {n} pixels differ, max abs diff {max_abs:e}"
                    ));
                }
            }
            (Some(_), None) => {
                channel_lines.push(format!("  channel {name}: only in {}", args[0]));
                pixel_differs.iter_mut().for_each(|d| *d = true);
            }
            (None, Some(_)) => {
                channel_lines.push(format!("  channel {name}: only in {}", args[1]));
                pixel_differs.iter_mut().for_each(|d| *d = true);
            }
            (None, None) => unreachable!("a name comes from one of the two"),
        }
    }
    let differing_pixels = pixel_differs.iter().filter(|d| **d).count();
    println!(
        "{}x{}  differing pixels: {differing_pixels}/{total} ({:.4}%)",
        aw,
        ah,
        100.0 * differing_pixels as f64 / total as f64
    );
    for line in &channel_lines {
        println!("{line}");
    }

    // The beauty's error metrics.
    let rgb = |p: &Planes| -> Option<[Vec<f32>; 3]> {
        let plane = |n: &str| {
            p.channels
                .get(n)
                .filter(|c| c.size == (aw, ah))
                .map(|c| c.values.clone())
        };
        Some([plane("R")?, plane("G")?, plane("B")?])
    };
    let (Some(a), Some(b)) = (rgb(&a), rgb(&b)) else {
        println!("(no R, G, B in both files: no beauty metrics)");
        return;
    };
    let mut max_abs = 0.0f32;
    let mut max_rel = 0.0f32;
    let mut sum_abs = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut sum_rel_sq = 0.0f64;
    let mut shown = 0;
    // Per-pixel relative squared error (mean over channels), for the trimmed
    // relMSE below.
    let mut pixel_rel = Vec::with_capacity(total);
    for p in 0..total {
        let mut differs = false;
        let mut rel = 0.0f64;
        for c in 0..3 {
            let (x, y) = (a[c][p], b[c][p]);
            let d = (x - y).abs();
            sum_abs += d as f64;
            sum_sq += (d as f64) * (d as f64);
            let r = (d as f64) * (d as f64) / ((x as f64) * (x as f64) + 1e-2);
            sum_rel_sq += r;
            rel += r / 3.0;
            if d != 0.0 {
                differs = true;
                max_abs = max_abs.max(d);
                let scale = x.abs().max(y.abs());
                if scale > 0.0 {
                    max_rel = max_rel.max(d / scale);
                }
            }
        }
        pixel_rel.push(rel);
        if differs && shown < 8 {
            shown += 1;
            println!(
                "  differs at ({}, {}): {:?} vs {:?}",
                p % aw,
                p / aw,
                [a[0][p], a[1][p], a[2][p]],
                [b[0][p], b[1][p], b[2][p]]
            );
        }
    }
    println!("max abs diff: {max_abs:e}   max rel diff: {max_rel:e}");
    println!("mean abs diff: {:e}", sum_abs / (total * 3) as f64);
    // RMSE alongside the mean, because they answer different questions: the
    // mean is dominated by how *much* of the image moved, while the square
    // weights the outliers — which is what aliasing is. A filtered render
    // improves the RMSE against its own reference far more than the mean.
    println!("rmse: {:e}", (sum_sq / (total * 3) as f64).sqrt());
    // Relative MSE against the *first* image, `(a − b)² / (a² + 0.01)`: the
    // noise metric of the sampling literature, and the one to use when `a` is
    // a high-spp reference. Unlike the RMSE it is not dominated by the few
    // pixels that see a light directly, so it measures the lit surfaces a
    // light-sampling change is actually meant to clean up.
    println!("relmse: {:e}", sum_rel_sq / (total * 3) as f64);
    // The same with the worst 0.1% of pixels discarded, the convention of the
    // path-guiding literature (Müller et al. 2017 and after). A handful of
    // fireflies -- a single one can carry an error of 1e5 -- otherwise decide
    // the mean on their own, and a change that cleans up the whole lit image
    // can read as no change at all.
    pixel_rel.sort_by(f64::total_cmp);
    let keep = total - total / 1000;
    println!(
        "relmse (trimmed 0.1%): {:e}",
        pixel_rel[..keep].iter().sum::<f64>() / keep.max(1) as f64
    );
}
