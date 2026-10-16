//! Tier 2's adaptive sampling sweep (`tune-adaptive-sampling-in-diagnostic`,
//! design D2–D3): a grid of settings, each one's predicted error and time
//! from a crop's recorded fixed-count render, the settings no other beats
//! on both, and the one that reaches the target soonest.
//!
//! Pure bookkeeping over a [`LuminanceTrace`] and numbers: the renders are
//! `mod.rs`'s, and the stop rule is the tracer's own ([`replay`]).

use rayon::prelude::*;

use crate::tracer::{AdaptiveSetting, LuminanceTrace, Replayed, replay};

use super::trials;

/// The fixed thresholds a grid tries when adaptive sampling is off as
/// authored, so there is no threshold to scale.
const THRESHOLDS_WHEN_OFF: [f32; 4] = [0.01, 0.02, 0.05, 0.1];
/// The multiples of the authored threshold a grid tries.
const THRESHOLD_SCALES: [f32; 4] = [0.25, 0.5, 1.0, 2.0];
const MINIMUMS: [u32; 4] = [8, 16, 32, 64];
const TOLERANCES: [f32; 4] = [-1.0, 0.0, 1.0, 2.0];

/// The most the recordings' traces may hold together, across every crop:
/// each crop's is kept until its verification, which waits for the pick
/// over all of them.
pub(super) const TRACE_BYTES: usize = 512 << 20;

/// The samples per pixel tier 2 records its crops at (design D3–D4): the
/// authored count within 64..=1024, halved while the traces of
/// `crop_pixels` would pass `trace_bytes` (down to 4), and while a
/// recording and a verification of every crop, at `s_per_pixel_spp`, would
/// take more than half of `left_s` (down to 64).
pub(super) fn recording_spp(
    authored_spp: u32,
    crop_pixels: usize,
    s_per_pixel_spp: f64,
    left_s: f64,
    trace_bytes: usize,
) -> u32 {
    let mut spp = authored_spp.clamp(64, 1024);
    let bytes = |spp: u32| crop_pixels * spp as usize * 8;
    let cost = |spp: u32| 2.0 * s_per_pixel_spp * crop_pixels as f64 * spp as f64;
    while spp > 64 && cost(spp) > 0.5 * left_s {
        spp /= 2;
    }
    while spp > 4 && bytes(spp) > trace_bytes {
        spp /= 2;
    }
    spp
}

/// The `sampleCount` and `variance` AOVs alone: what a verifying render
/// asks for.
pub(super) fn count_and_variance() -> crate::AovRequest {
    use crate::{AovProduct, AovRequest, AovSource, AovVar, Precision};
    let var = |name: &str, source: AovSource| AovVar {
        prim_path: format!("/Render/Products/Vars/{name}"),
        name: name.to_owned(),
        channel_prefix: None,
        source,
        components: source.components(),
        precision: Precision::Float,
        accumulation: source.default_accumulation(),
        clear: source.default_clear(),
        expression: None,
        raw: false,
        variance: false,
    };
    AovRequest {
        products: vec![AovProduct {
            prim_path: "/Render/Products/adaptive".into(),
            name: "adaptive.exr".into(),
            vars: vec![
                var("sampleCount", AovSource::SampleCount),
                var("variance", AovSource::Variance),
            ],
            attributes: Vec::new(),
        }],
    }
}

/// A raster-order plane of a `w`-wide region (`PixelRect::index`, rows
/// bottom-up) as top-down rows, as an AOV plane and an image hold it.
pub(super) fn top_down<T: Copy>(raster: &[T], w: usize) -> Vec<T> {
    let h = raster.len() / w.max(1);
    (0..raster.len())
        .map(|i| raster[(h - 1 - i / w) * w + i % w])
        .collect()
}

/// One setting's predicted outcome on one crop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Point {
    pub(super) mean_spp: f64,
    pub(super) early_stopped_share: f64,
    pub(super) held_share: f64,
    /// The crop's MRSE (`var / ref²`, untrimmed, as tier 2's target).
    pub(super) mrse: f64,
    /// Seconds of sampling: each pixel's samples at its tile's measured
    /// cost per sample.
    pub(super) time_s: f64,
}

/// The settings of a sweep: every combination of the thresholds, minimums,
/// tolerances and caps below, with `authored` (its cap clamped to the
/// recording) always among them, in a fixed order. A minimum at or above a
/// cap cannot stop a pixel, so only the authored one is kept there.
///
/// - thresholds: ¼, ½, 1 and 2 × the authored one, or 0.01, 0.02, 0.05 and
///   0.1 when it is off, plus `√target` when there is a target;
/// - minimums: the authored one, 8, 16, 32 and 64;
/// - tolerances: −1, 0, 1, 2 and the authored one;
/// - caps: a quarter, half and all of `spp_recorded`, and the authored one.
pub(super) fn grid(
    authored: AdaptiveSetting,
    target: Option<f64>,
    spp_recorded: u32,
) -> Vec<AdaptiveSetting> {
    let authored = AdaptiveSetting {
        spp: authored.spp.min(spp_recorded),
        ..authored
    };
    let mut thresholds: Vec<f32> = if authored.threshold > 0.0 {
        THRESHOLD_SCALES
            .iter()
            .map(|s| s * authored.threshold)
            .collect()
    } else {
        THRESHOLDS_WHEN_OFF.to_vec()
    };
    if let Some(t) = target.filter(|t| t.is_finite() && *t > 0.0) {
        thresholds.push(t.sqrt() as f32);
    }
    let mut minimums = vec![authored.min_samples];
    minimums.extend(MINIMUMS);
    let mut tolerances = TOLERANCES.to_vec();
    tolerances.push(authored.tolerance);
    let mut caps: Vec<u32> = [spp_recorded / 4, spp_recorded / 2, spp_recorded]
        .into_iter()
        .filter(|&c| c >= 1)
        .collect();
    caps.push(authored.spp);
    for v in [&mut thresholds, &mut tolerances] {
        v.sort_by(f32::total_cmp);
        v.dedup_by_key(|x| x.to_bits());
    }
    for v in [&mut minimums, &mut caps] {
        v.sort_unstable();
        v.dedup();
    }

    let mut out = vec![authored];
    for &spp in &caps {
        for &min_samples in &minimums {
            if min_samples >= spp {
                continue;
            }
            for &threshold in &thresholds {
                for &tolerance in &tolerances {
                    let s = AdaptiveSetting {
                        spp,
                        min_samples,
                        threshold,
                        tolerance,
                    };
                    if s != authored {
                        out.push(s);
                    }
                }
            }
        }
    }
    out
}

/// Each region pixel's wall-clock seconds per sample (`rect.index`): its
/// tile's share of the recording's `render_s`, by the tile's measured
/// seconds, over the tile's samples; `render_s` spread evenly when the
/// recording was not timed. The tiles' own seconds add up the workers'
/// time, not the wall's — on N threads about N times `render_s` — so they
/// say only where the time went, and `render_s` says how much.
pub(super) fn pixel_costs(trace: &LuminanceTrace, render_s: f64) -> Vec<f64> {
    let rect = trace.rect();
    let spp = trace.spp() as f64;
    let mut costs = vec![render_s / (rect.area().max(1) as f64 * spp); rect.area()];
    let worker_s: f64 = trace.tiles().iter().map(|(_, s)| s).sum();
    if worker_s <= 0.0 {
        return costs;
    }
    let wall = render_s / worker_s;
    for &(t, secs) in trace.tiles() {
        let c = wall * secs / (t.area().max(1) as f64 * spp);
        for y in t.y0..t.y1 {
            for x in t.x0..t.x1 {
                if rect.contains(x, y) {
                    costs[rect.index(x, y)] = c;
                }
            }
        }
    }
    costs
}

/// What `replayed` predicts on its crop: the image's variance in the
/// reference's units (the trace is in scene radiance, before the camera's
/// `exposure`), against `reference_lum`, and its time at `costs`.
pub(super) fn point(
    replayed: &Replayed,
    reference_lum: &[f64],
    exposure: f32,
    costs: &[f64],
) -> Point {
    let n = replayed.taken.len().max(1) as f64;
    let square = f64::from(exposure) * f64::from(exposure);
    let var: Vec<f64> = replayed.variance.iter().map(|v| v * square).collect();
    Point {
        mean_spp: replayed.taken.iter().map(|&t| t as f64).sum::<f64>() / n,
        early_stopped_share: replayed.early_stopped as f64 / n,
        held_share: replayed.held as f64 / n,
        mrse: trials::mrse(&var, reference_lum),
        time_s: replayed
            .taken
            .iter()
            .zip(costs)
            .map(|(&t, c)| t as f64 * c)
            .sum(),
    }
}

/// Every setting of `grid` replayed over `trace` and predicted, in grid
/// order; the replays run in parallel, each on its own.
pub(super) fn sweep(
    trace: &LuminanceTrace,
    grid: &[AdaptiveSetting],
    reference_lum: &[f64],
    exposure: f32,
    costs: &[f64],
) -> Vec<Point> {
    grid.par_iter()
        .map(|&s| point(&replay(trace, s), reference_lum, exposure, costs))
        .collect()
}

/// The indices of the points no other point beats on both time and MRSE
/// (at least as good on both and better on one), by increasing time, each
/// outcome once: of settings that predict the same time and MRSE (a
/// tolerance no pixel's neighbour reaches, a threshold no pixel passes),
/// the first.
pub(super) fn front(points: &[Point]) -> Vec<usize> {
    let dominated = |a: &Point| {
        points.iter().any(|b| {
            b.time_s <= a.time_s && b.mrse <= a.mrse && (b.time_s < a.time_s || b.mrse < a.mrse)
        })
    };
    let mut out: Vec<usize> = (0..points.len())
        .filter(|&g| points[g].mrse.is_finite() && !dominated(&points[g]))
        .collect();
    out.sort_by(|&a, &b| {
        points[a]
            .time_s
            .total_cmp(&points[b].time_s)
            .then(a.cmp(&b))
    });
    out.dedup_by(|b, a| {
        points[*a].time_s.to_bits() == points[*b].time_s.to_bits()
            && points[*a].mrse.to_bits() == points[*b].mrse.to_bits()
    });
    out
}

/// The sweep's answer across crops (`per_crop[k][g]`: crop `k`, setting
/// `g`): the setting with the least summed time whose MRSE is at or below
/// `targets[k]` on every crop `k`, and `true`; else the one with the least
/// summed MRSE, and `false`. Ties go to the earlier setting, so the
/// authored one (setting 0) wins a tie.
pub(super) fn pick(per_crop: &[Vec<Point>], targets: &[f64]) -> (usize, bool) {
    let settings = per_crop.first().map_or(0, Vec::len);
    let sum = |g: usize, f: fn(&Point) -> f64| per_crop.iter().map(|c| f(&c[g])).sum::<f64>();
    let reaches = |g: usize| per_crop.iter().zip(targets).all(|(c, &t)| c[g].mrse <= t);
    let cheapest = (0..settings)
        .filter(|&g| reaches(g))
        .min_by(|&a, &b| sum(a, |p| p.time_s).total_cmp(&sum(b, |p| p.time_s)));
    match cheapest {
        Some(g) => (g, true),
        None => {
            let g = (0..settings)
                .filter(|&g| sum(g, |p| p.mrse).is_finite())
                .min_by(|&a, &b| sum(a, |p| p.mrse).total_cmp(&sum(b, |p| p.mrse)))
                .unwrap_or(0);
            (g, false)
        }
    }
}

/// The efficiency gain of setting `g` over setting `h`: summed time ×
/// summed MRSE of `h`, over `g`'s.
pub(super) fn delta_eff(per_crop: &[Vec<Point>], g: usize, h: usize) -> f64 {
    let t = |g: usize| per_crop.iter().map(|c| c[g].time_s).sum::<f64>();
    let m = |g: usize| per_crop.iter().map(|c| c[g].mrse).sum::<f64>();
    (t(h) * m(h)) / (t(g) * m(g))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(spp: u32, min_samples: u32, threshold: f32, tolerance: f32) -> AdaptiveSetting {
        AdaptiveSetting {
            spp,
            min_samples,
            threshold,
            tolerance,
        }
    }

    fn p(time_s: f64, mrse: f64) -> Point {
        Point {
            mean_spp: 0.0,
            early_stopped_share: 0.0,
            held_share: 0.0,
            mrse,
            time_s,
        }
    }

    /// The spec's scenario "The authored setting is always on the grid":
    /// first, once, with its cap clamped to the recording; every setting a
    /// minimum below its cap; no repeats.
    #[test]
    fn the_authored_setting_is_always_on_the_grid() {
        let authored = s(128, 8, 0.03, 1.0);
        let g = grid(authored, Some(0.03 * 0.03), 512);
        assert_eq!(g[0], authored);
        assert_eq!(g.iter().filter(|x| **x == authored).count(), 1);
        assert!(g[1..].iter().all(|x| x.min_samples < x.spp));
        for (a, x) in g.iter().enumerate() {
            assert!(!g[a + 1..].contains(x), "{x:?} twice");
        }
        // The authored cap and the recording's three.
        let caps: std::collections::BTreeSet<u32> = g.iter().map(|x| x.spp).collect();
        assert_eq!(caps.into_iter().collect::<Vec<_>>(), vec![128, 256, 512]);
        // The cap above the recording is clamped to it.
        let g = grid(s(4096, 8, 0.03, 1.0), None, 512);
        assert_eq!(g[0], s(512, 8, 0.03, 1.0));
        // A minimum the authored cap cannot pass is still the authored point.
        let g = grid(s(16, 32, 0.05, 1.0), None, 256);
        assert_eq!(g[0], s(16, 32, 0.05, 1.0));
    }

    /// With adaptive sampling off as authored, the grid tries the fixed
    /// thresholds (and the target's), and the authored point is the
    /// fixed-count render.
    #[test]
    fn with_adaptive_sampling_off_the_grid_uses_fixed_thresholds() {
        let g = grid(s(256, 32, 0.0, 1.0), None, 256);
        let thresholds: std::collections::BTreeSet<u32> =
            g[1..].iter().map(|x| x.threshold.to_bits()).collect();
        let want: std::collections::BTreeSet<u32> =
            THRESHOLDS_WHEN_OFF.iter().map(|t| t.to_bits()).collect();
        assert_eq!(thresholds, want);
        assert_eq!(g[0].threshold, 0.0);
        let g = grid(s(256, 32, 0.0, 1.0), Some(0.0009), 256);
        assert!(g.iter().any(|x| (x.threshold - 0.03).abs() < 1e-6));
    }

    /// A fixed-count setting's predicted time is the recording's time: each
    /// pixel's samples at its tile's cost per sample.
    #[test]
    fn a_fixed_setting_costs_the_recorded_time() {
        let replayed = Replayed {
            taken: vec![64; 6],
            variance: vec![0.01; 6],
            mean: vec![1.0; 6],
            early_stopped: 0,
            held: 0,
        };
        // Two tiles of three pixels: 0.3 s and 0.9 s at 64 spp.
        let costs: Vec<f64> = [0.3, 0.3, 0.3, 0.9, 0.9, 0.9]
            .iter()
            .map(|t| t / (3.0 * 64.0))
            .collect();
        let pt = point(&replayed, &[1.0; 6], 1.0, &costs);
        assert!((pt.time_s - 1.2).abs() < 1e-12);
        assert_eq!(pt.mean_spp, 64.0);
        assert!((pt.mrse - 0.01).abs() < 1e-12);
        // The exposure scales the variance into the reference's units.
        let pt = point(&replayed, &[2.0; 6], 2.0, &costs);
        assert!((pt.mrse - 0.01).abs() < 1e-12);
    }

    /// The front keeps the points nothing beats on both, by time.
    #[test]
    fn the_front_drops_dominated_points() {
        let pts = [
            p(3.0, 1.0),
            p(1.0, 3.0),
            p(2.0, 2.0),
            p(2.5, 2.5),
            p(4.0, 1.0),
            p(2.0, 2.0),
        ];
        assert_eq!(front(&pts), vec![1, 2, 0]);
    }

    /// The spec's scenario "A lower threshold with a higher cap": the
    /// authored point misses the target on one crop; the cheapest setting
    /// that reaches it on every crop is picked.
    #[test]
    fn the_cheapest_setting_at_the_target_is_picked() {
        // Settings: 0 authored, 1 lower threshold + higher cap, 2 slower
        // still, 3 fast but noisy.
        let per_crop = vec![
            vec![p(1.0, 0.002), p(1.5, 0.001), p(3.0, 0.0005), p(0.5, 0.01)],
            vec![p(1.0, 0.004), p(1.6, 0.002), p(3.0, 0.001), p(0.5, 0.02)],
        ];
        assert_eq!(pick(&per_crop, &[0.0025; 2]), (1, true));
        assert!(delta_eff(&per_crop, 1, 0) > 1.0);
        // A tie goes to the authored point.
        let tie = vec![vec![p(1.0, 0.001), p(1.0, 0.001)]];
        assert_eq!(pick(&tie, &[0.0025]), (0, true));
        // Without a target, the authored point's own MRSE is each crop's:
        // the cheapest setting at least as clean everywhere.
        let targets: Vec<f64> = per_crop.iter().map(|c| c[0].mrse).collect();
        assert_eq!(pick(&per_crop, &targets), (0, true));
    }

    /// The recording's count follows the authored one within 64..=1024,
    /// and halves for the budget (to 64) and for the trace memory (to 4).
    #[test]
    fn the_recording_halves_to_fit_its_budget_and_memory() {
        let px = 3 * 128 * 128;
        assert_eq!(recording_spp(128, px, 0.0, 100.0, TRACE_BYTES), 128);
        assert_eq!(recording_spp(16, px, 0.0, 100.0, TRACE_BYTES), 64);
        assert_eq!(recording_spp(4096, px, 0.0, 100.0, TRACE_BYTES), 1024);
        // 2 × 1e-7 s × 49152 px × spp ≤ 5 s: spp ≤ 508, so 256.
        assert_eq!(recording_spp(1024, px, 1e-7, 10.0, TRACE_BYTES), 256);
        assert_eq!(recording_spp(1024, px, 1.0, 10.0, TRACE_BYTES), 64);
        // A forced-small bound: 49152 px × 8 B × spp ≤ 4 MiB → 10, so 8.
        assert_eq!(recording_spp(1024, px, 0.0, 100.0, 4 << 20), 8);
        assert_eq!(recording_spp(1024, px, 0.0, 100.0, 1), 4);
    }

    #[test]
    fn top_down_flips_the_rows() {
        // A 2-wide, 3-high raster: rows bottom-up [0 1] [2 3] [4 5].
        assert_eq!(top_down(&[0, 1, 2, 3, 4, 5], 2), vec![4, 5, 2, 3, 0, 1]);
    }

    /// The spec's scenario "Nothing reaches the target": the lowest-MRSE
    /// setting, and the target not reached.
    #[test]
    fn below_every_setting_the_lowest_mrse_is_picked() {
        let per_crop = vec![vec![p(1.0, 0.002), p(2.0, 0.001), p(0.5, 0.004)]];
        assert_eq!(pick(&per_crop, &[1e-6]), (1, false));
    }
}
