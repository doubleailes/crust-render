//! The adaptive stop rule replayed over a recorded fixed-count pass
//! (`crust diagnostic`'s tier 2, design D1 of
//! `tune-adaptive-sampling-in-diagnostic`).
//!
//! A sample depends only on `(pixel, seed, sample index)`, never on when it
//! is traced, so a pixel's first `n` samples in a fixed-count pass are the
//! ones it would have taken adaptively by `taken = n`. Given each sample's
//! luminance, the stop rule can therefore be run again for any threshold,
//! minimum, tolerance or cap up to the recorded count, and decides exactly
//! as an adaptive render of the same region would. The replay goes through
//! the pass's own pieces — [`PixelState::add_luminance`],
//! [`PixelState::finish_round`], [`keeps_sampling`], [`adaptive_check_points`]
//! and [`batch_schedule`] — so the two cannot drift apart; a bitwise test
//! pins them against a real adaptive render (`tracer/tests.rs`).

use super::{PixelRect, PixelState, Tile, adaptive_check_points, batch_schedule, keeps_sampling};

/// Every sample's luminance of a fixed-count pass over `rect`: what the
/// pass added to each pixel's stop-rule sums, in sample order, in scene
/// radiance. Held as the pass's work units left it, so recording copies
/// nothing.
pub(crate) struct LuminanceTrace {
    rect: PixelRect,
    spp: u32,
    /// Each unit's tile and its pixel-major samples, `spp` a pixel.
    parts: Vec<(Tile, Vec<f64>)>,
    /// Per region pixel (`rect.index`): its unit and its first sample there.
    at: Vec<(u32, u32)>,
    /// The pass's units and their seconds, when it was timed.
    tiles: Vec<(PixelRect, f64)>,
}

impl LuminanceTrace {
    pub(super) fn new(
        rect: PixelRect,
        spp: u32,
        parts: Vec<(Tile, Vec<f64>)>,
        tiles: Vec<(PixelRect, f64)>,
    ) -> Self {
        let mut at = vec![(0, 0); rect.area()];
        for (k, (tile, samples)) in parts.iter().enumerate() {
            debug_assert_eq!(samples.len(), tile.width * tile.height * spp as usize);
            for p in 0..tile.width * tile.height {
                let (i, j) = (tile.x + p % tile.width, tile.y + p / tile.width);
                at[rect.index(i, j)] = (k as u32, (p * spp as usize) as u32);
            }
        }
        LuminanceTrace {
            rect,
            spp,
            parts,
            at,
            tiles,
        }
    }

    /// The pixels the pass traced, in raster space.
    pub(crate) fn rect(&self) -> PixelRect {
        self.rect
    }

    /// The samples every pixel took.
    pub(crate) fn spp(&self) -> u32 {
        self.spp
    }

    /// The pass's work units (raster space) and their seconds; empty unless
    /// the pass was timed.
    pub(crate) fn tiles(&self) -> &[(PixelRect, f64)] {
        &self.tiles
    }

    /// Region pixel `q`'s samples, in sample order.
    pub(crate) fn samples(&self, q: usize) -> &[f64] {
        let (k, start) = self.at[q];
        let start = start as usize;
        &self.parts[k as usize].1[start..start + self.spp as usize]
    }

    /// The bytes it holds.
    pub(crate) fn bytes(&self) -> usize {
        self.parts.iter().map(|(_, s)| s.len() * 8).sum::<usize>() + self.at.len() * 8
    }
}

/// An adaptive setting a replay decides with: what `crust:samplesPerPixel`,
/// `crust:minSamplesPerPixel`, `crust:varianceThreshold` and
/// `crust:adaptiveNeighbourTolerance` would say.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AdaptiveSetting {
    pub(crate) spp: u32,
    pub(crate) min_samples: u32,
    pub(crate) threshold: f32,
    pub(crate) tolerance: f32,
}

/// Per region pixel (`rect.index`), what the stop rule left it with.
pub(crate) struct Replayed {
    /// The samples it took: what the `sampleCount` AOV would hold.
    pub(crate) taken: Vec<u32>,
    /// The variance of its luminance mean: what the `variance` AOV would hold.
    pub(crate) variance: Vec<f64>,
    /// Its luminance mean over those samples (Σ lum / taken).
    pub(crate) mean: Vec<f64>,
    /// Pixels stopped short of the cap, and held back by a neighbour at
    /// least once, as the render's counters count them.
    pub(crate) early_stopped: u64,
    pub(crate) held: u64,
}

/// The stop rule run over `trace` at `setting`, exactly as an adaptive
/// pass over the trace's region would run it. `setting.spp` must not exceed
/// the recorded count.
pub(crate) fn replay(trace: &LuminanceTrace, setting: AdaptiveSetting) -> Replayed {
    let spp = setting.spp;
    assert!(
        spp >= 1 && spp <= trace.spp,
        "a replay reaches {spp} spp of {} recorded",
        trace.spp
    );
    let rect = trace.rect;
    let n = rect.area();
    let threshold = setting.threshold as f64;
    let adaptive = threshold > 0.0;
    let (_, first_check) = adaptive_check_points(spp, setting.min_samples);
    let sweep_to = if adaptive { spp.min(first_check) } else { spp };
    let schedule = if adaptive {
        batch_schedule(spp, first_check)
    } else {
        Vec::new()
    };
    let advance = |st: &mut PixelState, samples: &[f64], target: u32| {
        for &lum in &samples[st.taken as usize..target as usize] {
            st.add_luminance(lum);
        }
        st.taken = target;
    };

    // The first sweep, then its test, as the pass runs them.
    let mut states = vec![PixelState::new(); n];
    for (q, st) in states.iter_mut().enumerate() {
        advance(st, trace.samples(q), sweep_to);
        st.finish_round(spp, threshold);
    }
    // The rounds: freeze, decide, advance the still active.
    let mut index = vec![f32::INFINITY; n];
    let mut active = vec![false; n];
    for &target in &schedule {
        let mut any_active = false;
        for (q, st) in states.iter().enumerate() {
            index[q] = st.index;
            active[q] = !st.stopped;
            any_active |= !st.stopped;
        }
        if !any_active {
            break;
        }
        for j in rect.y0..rect.y1 {
            for i in rect.x0..rect.x1 {
                let q = rect.index(i, j);
                let st = &mut states[q];
                if st.stopped || !keeps_sampling(st, &index, &active, rect, i, j, setting.tolerance)
                {
                    continue;
                }
                advance(st, trace.samples(q), target);
                st.finish_round(spp, threshold);
            }
        }
    }

    let mut out = Replayed {
        taken: Vec::with_capacity(n),
        variance: Vec::with_capacity(n),
        mean: Vec::with_capacity(n),
        early_stopped: 0,
        held: 0,
    };
    for st in &states {
        out.taken.push(st.taken);
        out.variance.push(st.var_of_mean());
        out.mean.push(if st.taken > 0 {
            st.lum_sum / st.taken as f64
        } else {
            0.0
        });
        if st.stopped && st.taken < spp {
            out.early_stopped += 1;
        }
        if st.held {
            out.held += 1;
        }
    }
    out
}
