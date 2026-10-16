//! Choosing the crops the trials render (design D6): up to three windows of
//! the frame, each representative for a different reason, from the
//! baseline's per-pixel relative variance and time.

use crate::PixelRect;

/// The tile grid crops are aligned to: the renderer's own.
pub const TILE: usize = 16;

/// The smallest crop side, in pixels.
pub const MIN_SIDE: usize = 128;

/// The baseline, per pixel, in image space (top-left origin, row-major):
/// what a crop is chosen from.
#[derive(Debug, Clone)]
pub struct CropMaps {
    pub width: usize,
    pub height: usize,
    /// `var / max(lum², 1e-4)`.
    pub rel_var: Vec<f64>,
    /// Seconds, each tile's time spread evenly over its pixels.
    pub time: Vec<f64>,
    pub lum: Vec<f64>,
}

/// A chosen crop and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Picked {
    /// Image space.
    pub rect: PixelRect,
    /// `highest_relative_variance`, `highest_time`, `median` or `region`.
    pub reason: &'static str,
    /// Mean relative variance over the crop.
    pub rel_var: f64,
    /// Thread-seconds the baseline spent in it.
    pub time_s: f64,
}

/// The crop side for `threads` workers: at least four 16×16 tiles per
/// worker, and never below [`MIN_SIDE`] — a smaller crop measures the
/// thread pool's ramp-up rather than the scene.
/// `S = max(128, 16 · ⌈√(4 · threads)⌉)`.
pub fn crop_side(threads: usize) -> usize {
    let tiles = (4.0 * threads.max(1) as f64).sqrt().ceil() as usize;
    MIN_SIDE.max(TILE * tiles)
}

/// A summed-area table over a `w × h` plane: any rectangle's sum in four
/// lookups.
struct Sat {
    w: usize,
    sums: Vec<f64>,
}

impl Sat {
    fn new(plane: &[f64], w: usize, h: usize) -> Self {
        let mut sums = vec![0.0; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0.0;
            for x in 0..w {
                row += plane[y * w + x];
                sums[(y + 1) * (w + 1) + x + 1] = sums[y * (w + 1) + x + 1] + row;
            }
        }
        Sat { w, sums }
    }

    fn sum(&self, r: PixelRect) -> f64 {
        let at = |x: usize, y: usize| self.sums[y * (self.w + 1) + x];
        at(r.x1, r.y1) - at(r.x0, r.y1) - at(r.x1, r.y0) + at(r.x0, r.y0)
    }
}

/// Intersection over union of two rectangles.
pub fn iou(a: PixelRect, b: PixelRect) -> f64 {
    let ix = a.x1.min(b.x1).saturating_sub(a.x0.max(b.x0));
    let iy = a.y1.min(b.y1).saturating_sub(a.y0.max(b.y0));
    let inter = (ix * iy) as f64;
    let union = (a.area() + b.area()) as f64 - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

/// Window origins along one axis: every tile-aligned start that fits, and
/// the last start that fits when it is not on the grid, so the far edge of
/// the frame is a candidate too.
fn starts(extent: usize, side: usize) -> Vec<usize> {
    let last = extent - side;
    let mut v: Vec<usize> = (0..=last).step_by(TILE).collect();
    if v.last() != Some(&last) {
        v.push(last);
    }
    v
}

/// Up to three crops of `side` pixels (clipped to the frame), each one the
/// best window for its criterion that does not overlap an already chosen
/// crop by an IoU above 0.5:
///
/// - A: the highest summed relative variance;
/// - B: the highest summed time;
/// - C: the window whose relative-variance and time ranks are closest to
///   the median of both, skipping pure background (mean luminance 0).
///
/// A criterion with nothing left adds no crop. With `region`, that region
/// is the only crop.
pub fn pick(maps: &CropMaps, side: usize, region: Option<PixelRect>) -> Vec<Picked> {
    let (w, h) = (maps.width, maps.height);
    let rel = Sat::new(&maps.rel_var, w, h);
    let time = Sat::new(&maps.time, w, h);
    let lum = Sat::new(&maps.lum, w, h);
    let describe = |rect: PixelRect, reason| Picked {
        rect,
        reason,
        rel_var: rel.sum(rect) / rect.area().max(1) as f64,
        time_s: time.sum(rect),
    };
    if let Some(r) = region {
        return vec![describe(r, "region")];
    }
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let (sw, sh) = (side.min(w), side.min(h));
    let mut windows = Vec::new();
    for y in starts(h, sh) {
        for x in starts(w, sw) {
            windows.push(PixelRect::new(x, y, x + sw, y + sh));
        }
    }
    let scores: Vec<(f64, f64, f64)> = windows
        .iter()
        .map(|&r| (rel.sum(r), time.sum(r), lum.sum(r)))
        .collect();
    // Ties go to the earlier window (top-left first), so the choice is
    // deterministic for equal maps.
    let by = |key: &dyn Fn(usize) -> f64| -> Vec<usize> {
        let mut order: Vec<usize> = (0..windows.len()).collect();
        order.sort_by(|&a, &b| key(b).total_cmp(&key(a)).then(a.cmp(&b)));
        order
    };
    let a = by(&|i| scores[i].0);
    let b = by(&|i| scores[i].1);
    // Normalised ranks, 0 for the lowest score and 1 for the highest.
    let n = windows.len();
    let rank_of = |order: &[usize]| {
        let mut rank = vec![0.0; n];
        for (k, &i) in order.iter().enumerate() {
            rank[i] = if n > 1 {
                1.0 - k as f64 / (n - 1) as f64
            } else {
                0.5
            };
        }
        rank
    };
    let (ra, rb) = (rank_of(&a), rank_of(&b));
    let c: Vec<usize> = by(&|i| -((ra[i] - 0.5).abs() + (rb[i] - 0.5).abs()))
        .into_iter()
        .filter(|&i| scores[i].2 > 0.0)
        .collect();

    let mut chosen: Vec<Picked> = Vec::new();
    for (order, reason) in [
        (&a, "highest_relative_variance"),
        (&b, "highest_time"),
        (&c, "median"),
    ] {
        let next = order
            .iter()
            .map(|&i| windows[i])
            .find(|&r| chosen.iter().all(|p| iou(p.rect, r) <= 0.5));
        if let Some(r) = next {
            chosen.push(describe(r, reason));
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maps(
        w: usize,
        h: usize,
        rel: impl Fn(usize, usize) -> f64,
        t: impl Fn(usize, usize) -> f64,
    ) -> CropMaps {
        let at = |f: &dyn Fn(usize, usize) -> f64| {
            (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| f(x, y))
                .collect::<Vec<f64>>()
        };
        CropMaps {
            width: w,
            height: h,
            rel_var: at(&rel),
            time: at(&t),
            lum: vec![1.0; w * h],
        }
    }

    #[test]
    fn the_side_gives_each_thread_four_tiles() {
        assert_eq!(crop_side(1), 128);
        assert_eq!(crop_side(16), 128);
        // 64 threads: √256 = 16 tiles a side.
        assert_eq!(crop_side(64), 256);
        // 100 threads: ⌈√400⌉ = 20.
        assert_eq!(crop_side(100), 320);
    }

    #[test]
    fn a_hot_corner_is_found_by_variance_and_time_apart() {
        // Noise in the bottom-right corner, time in the top-left one.
        let m = maps(
            512,
            384,
            |x, y| if x >= 384 && y >= 256 { 10.0 } else { 0.1 },
            |x, y| if x < 128 && y < 128 { 1.0 } else { 0.01 },
        );
        let p = pick(&m, 128, None);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].reason, "highest_relative_variance");
        assert!(
            p[0].rect.x0 >= 384 && p[0].rect.y0 >= 256,
            "{:?}",
            p[0].rect
        );
        assert_eq!(p[1].reason, "highest_time");
        assert_eq!(p[1].rect, PixelRect::new(0, 0, 128, 128));
        assert_eq!(p[2].reason, "median");
        for i in 0..3 {
            for j in 0..i {
                assert!(iou(p[i].rect, p[j].rect) <= 0.5);
            }
        }
    }

    #[test]
    fn a_uniform_map_still_gives_three_separate_crops() {
        let m = maps(512, 384, |_, _| 1.0, |_, _| 1.0);
        let p = pick(&m, 128, None);
        assert_eq!(p.len(), 3);
        // All windows tie; the earliest that does not overlap wins.
        assert_eq!(p[0].rect, PixelRect::new(0, 0, 128, 128));
        assert!(iou(p[1].rect, p[0].rect) <= 0.5);
        assert!(iou(p[2].rect, p[0].rect) <= 0.5 && iou(p[2].rect, p[1].rect) <= 0.5);
    }

    #[test]
    fn when_a_and_b_coincide_b_takes_its_next_best() {
        let hot = |x: usize, y: usize| {
            if (200..328).contains(&x) && (100..228).contains(&y) {
                5.0
            } else {
                0.0
            }
        };
        let m = maps(512, 384, hot, hot);
        let p = pick(&m, 128, None);
        assert_eq!(p[0].reason, "highest_relative_variance");
        assert_eq!(p[1].reason, "highest_time");
        assert!(iou(p[0].rect, p[1].rect) <= 0.5);
        // B is the next best window for time: still overlapping the hot spot.
        assert!(time_overlap(&p[1].rect) > 0);
    }

    fn time_overlap(r: &PixelRect) -> usize {
        let ix = r.x1.min(328).saturating_sub(r.x0.max(200));
        let iy = r.y1.min(228).saturating_sub(r.y0.max(100));
        ix * iy
    }

    #[test]
    fn pure_background_is_never_the_median_crop() {
        let mut m = maps(512, 128, |x, _| x as f64, |x, _| x as f64);
        // Only the left half has anything in it.
        for y in 0..128 {
            for x in 256..512 {
                m.lum[y * 512 + x] = 0.0;
            }
        }
        let p = pick(&m, 128, None);
        let median = p
            .iter()
            .find(|c| c.reason == "median")
            .expect("a median crop");
        assert!(median.rect.x0 < 256, "{:?}", median.rect);
    }

    #[test]
    fn a_region_is_the_only_crop() {
        let m = maps(512, 384, |_, _| 1.0, |_, _| 1.0);
        let r = PixelRect::new(0, 0, 256, 256);
        let p = pick(&m, 128, Some(r));
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].rect, r);
        assert_eq!(p[0].reason, "region");
        assert_eq!(p[0].rel_var, 1.0);
    }

    #[test]
    fn a_frame_smaller_than_the_side_is_one_clipped_crop() {
        let m = maps(100, 60, |_, _| 1.0, |_, _| 1.0);
        let p = pick(&m, 128, None);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].rect, PixelRect::new(0, 0, 100, 60));
    }
}
