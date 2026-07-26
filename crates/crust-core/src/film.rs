//! The accumulation film: per-pixel running state of a render-pass
//! sequence, including the split odd-sample buffer that powers adaptive
//! sampling (a MoonRay/Dammertz-style stopping estimator — see
//! `docs/moonray_comparison.md`).

use crate::buffer::Buffer;
use crate::checkpoint::CheckpointState;
use crate::guiding::luminance;
use glam::Vec3A;

/// One pixel's contribution from one pass: everything [`Film::merge`] needs,
/// accumulated worker-locally so the film itself is only touched serially.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PixelDelta {
    /// Sum of all sample radiances in the pass's sample range.
    pub sum: Vec3A,
    /// Sum of the samples with odd *global* per-pixel sample index. The
    /// global index keeps the odd/even split identical across passes and
    /// across a checkpoint/resume boundary.
    pub odd_sum: Vec3A,
    pub count: u32,
    /// Luminance moments feeding [`Film::pass_stats`] (the guiding
    /// blend/efficiency statistics — not the adaptive stop).
    pub lum_sum: f64,
    pub lum_sq: f64,
}

/// Accumulated state of a whole render-pass sequence, row-major.
///
/// The image estimate is `sum / count`. The half-rate `odd_sum` buffer
/// yields a second, correlated estimate of the same integral; the distance
/// between the two shrinks as 1/√n and is the adaptive stopping error
/// (Dammertz et al. 2010, as used by MoonRay).
pub struct Film {
    width: usize,
    height: usize,
    sum: Vec<Vec3A>,
    odd_sum: Vec<Vec3A>,
    count: Vec<u32>,
    lum_sum: Vec<f64>,
    lum_sq: Vec<f64>,
}

/// Calibration between the raw split-buffer error and the relative standard
/// error of the pixel mean, preserving the historical meaning of
/// `crust:varianceThreshold`: `mean − mean_odd` is half of
/// `mean_even − mean_odd`, whose std is `2σ/√n`, so the folded-normal
/// expectation of the raw error is `√(2/π)·σ/√n ≈ 0.798 ×` the standard
/// error the threshold is expressed in.
pub(crate) const ERROR_CALIBRATION: f32 = 0.798;

impl Film {
    pub fn new(width: usize, height: usize) -> Self {
        let n = width * height;
        Film {
            width,
            height,
            sum: vec![Vec3A::ZERO; n],
            odd_sum: vec![Vec3A::ZERO; n],
            count: vec![0; n],
            lum_sum: vec![0.0; n],
            lum_sq: vec![0.0; n],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Fold one pass's pixel contribution in. Tiles are disjoint within a
    /// pass, and the caller merges tiles in a fixed order — so accumulation
    /// is deterministic. Two runs produce bit-identical films exactly when
    /// they merge the same deltas in the same order, i.e. when they render
    /// the same pass schedule — which is why the schedule is a pure
    /// function of `(spp, min_spp)` and why resume replays its suffix.
    pub(crate) fn merge(&mut self, x: usize, y: usize, d: &PixelDelta) {
        let i = y * self.width + x;
        self.sum[i] += d.sum;
        self.odd_sum[i] += d.odd_sum;
        self.count[i] += d.count;
        self.lum_sum[i] += d.lum_sum;
        self.lum_sq[i] += d.lum_sq;
    }

    /// The split-buffer stopping error of one pixel, in units of the
    /// relative standard error of the pixel mean (after
    /// [`ERROR_CALIBRATION`]): `luma(|mean − mean_odd|) / max(luma(mean),
    /// 1e-3)`. Infinite until both halves hold at least one sample.
    pub(crate) fn pixel_error(&self, x: usize, y: usize) -> f32 {
        let i = y * self.width + x;
        let n = self.count[i];
        let n_odd = n / 2;
        if n_odd == 0 {
            return f32::INFINITY;
        }
        let mean = self.sum[i] / n as f32;
        let mean_odd = self.odd_sum[i] / n_odd as f32;
        let diff = luminance((mean - mean_odd).abs());
        diff / luminance(mean).max(1e-3)
    }

    /// The image estimate: `sum / count`, black where nothing accumulated.
    pub fn to_buffer(&self) -> Buffer {
        let mut buffer = Buffer::new(self.width, self.height);
        for y in 0..self.height {
            for x in 0..self.width {
                let i = y * self.width + x;
                if self.count[i] > 0 {
                    buffer.set_pixel(x, y, self.sum[i] / self.count[i] as f32);
                }
            }
        }
        buffer
    }

    /// Samples accumulated at one pixel.
    pub(crate) fn sample_count(&self, x: usize, y: usize) -> u32 {
        self.count[y * self.width + x]
    }

    /// Materialize the resumable state at a uniform pass boundary.
    pub(crate) fn snapshot(&self, next_sample: u32, fingerprint: u64) -> CheckpointState {
        CheckpointState {
            width: self.width,
            height: self.height,
            sum: self.sum.clone(),
            odd_sum: self.odd_sum.clone(),
            count: self.count.clone(),
            next_sample,
            fingerprint,
        }
    }

    /// Rebuild a film from checkpoint state. The luminance moments start at
    /// zero: they only feed [`Self::pass_stats`], which the (non-guided,
    /// hence resumable) render path never consumes — a resumed film's stats
    /// would describe the resumed samples only.
    pub(crate) fn restore(state: &CheckpointState) -> Film {
        let n = state.width * state.height;
        Film {
            width: state.width,
            height: state.height,
            sum: state.sum.clone(),
            odd_sum: state.odd_sum.clone(),
            count: state.count.clone(),
            lum_sum: vec![0.0; n],
            lum_sq: vec![0.0; n],
        }
    }

    /// Per-pixel unbiased variance of the pixel-mean luminance and its
    /// image-wide mean — the same statistics the pre-film renderer
    /// produced, feeding inverse-variance pass blending and the guiding
    /// efficiency estimate.
    pub(crate) fn pass_stats(&self) -> (f64, Vec<f64>) {
        let mut var_map = vec![0.0f64; self.width * self.height];
        let mut variance_sum = 0.0f64;
        for (i, v) in var_map.iter_mut().enumerate() {
            let n = self.count[i] as f64;
            *v = if self.count[i] >= 2 {
                ((self.lum_sq[i] - self.lum_sum[i] * self.lum_sum[i] / n) / (n - 1.0) / n).max(0.0)
            } else {
                f64::INFINITY
            };
            variance_sum += *v;
        }
        let pixel_count = (self.width * self.height).max(1) as f64;
        (variance_sum / pixel_count, var_map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openqmc::pcg::Rng;

    fn delta_for(samples: &[(u32, Vec3A)]) -> PixelDelta {
        let mut d = PixelDelta::default();
        for &(index, c) in samples {
            d.sum += c;
            if index % 2 == 1 {
                d.odd_sum += c;
            }
            d.count += 1;
            let l = luminance(c) as f64;
            d.lum_sum += l;
            d.lum_sq += l * l;
        }
        d
    }

    /// The resume guarantee at the film level: a run that merges the same
    /// per-pass deltas in the same order — with an interruption and a state
    /// handoff in the middle — reproduces the uninterrupted film bit for
    /// bit. (Merging a range as *one* delta instead would associate the
    /// float additions differently; that is exactly why the pass schedule
    /// is a pure function and resume replays its suffix.)
    #[test]
    fn replaying_the_same_pass_partition_is_bit_exact() {
        let mut rng = Rng::new(11);
        let samples: Vec<(u32, Vec3A)> = (0..64)
            .map(|i| {
                (
                    i,
                    Vec3A::new(rng.next_f32(), rng.next_f32(), rng.next_f32()) * 3.0,
                )
            })
            .collect();
        // Schedule-shaped partition boundaries.
        let bounds = [0usize, 1, 2, 4, 8, 16, 32, 48, 64];
        let deltas: Vec<PixelDelta> = bounds
            .windows(2)
            .map(|w| delta_for(&samples[w[0]..w[1]]))
            .collect();

        let mut straight = Film::new(1, 1);
        for d in &deltas {
            straight.merge(0, 0, d);
        }
        // Interrupted run: first passes into one film, hand the state over
        // (as checkpoint/resume does), then the remaining passes.
        let mut before = Film::new(1, 1);
        for d in &deltas[..4] {
            before.merge(0, 0, d);
        }
        let mut resumed = Film::new(1, 1);
        resumed.sum = before.sum.clone();
        resumed.odd_sum = before.odd_sum.clone();
        resumed.count = before.count.clone();
        for d in &deltas[4..] {
            resumed.merge(0, 0, d);
        }
        assert_eq!(straight.sum[0], resumed.sum[0]);
        assert_eq!(straight.odd_sum[0], resumed.odd_sum[0]);
        assert_eq!(straight.count[0], resumed.count[0]);
        assert_eq!(
            straight.to_buffer().get_pixel(0, 0),
            resumed.to_buffer().get_pixel(0, 0)
        );
    }

    #[test]
    fn constant_samples_have_zero_error() {
        let mut film = Film::new(1, 1);
        let samples: Vec<(u32, Vec3A)> = (0..16).map(|i| (i, Vec3A::splat(0.5))).collect();
        film.merge(0, 0, &delta_for(&samples));
        assert!(film.pixel_error(0, 0) < 1e-6);
    }

    #[test]
    fn error_is_infinite_below_two_samples() {
        let mut film = Film::new(1, 1);
        assert!(film.pixel_error(0, 0).is_infinite());
        film.merge(0, 0, &delta_for(&[(0, Vec3A::ONE)]));
        assert!(film.pixel_error(0, 0).is_infinite());
    }

    /// The estimator's statistics match the calibration: for i.i.d. samples
    /// with std σ and mean μ, the mean observed error approaches
    /// `ERROR_CALIBRATION · σ/(√n·μ)`, and quadrupling n halves it.
    #[test]
    fn error_statistics_match_calibration() {
        let (mu, sigma) = (1.0f32, 0.5f32);
        let mean_error = |n: u32, pixels: u32, seed: u32| -> f64 {
            let mut rng = Rng::new(seed);
            let mut gauss = move || {
                // Box-Muller.
                let u1 = rng.next_f32().max(1e-7);
                let u2 = rng.next_f32();
                mu + sigma
                    * (-2.0 * u1.ln()).sqrt()
                    * (2.0 * std::f32::consts::PI * u2).cos()
            };
            let mut total = 0.0f64;
            for _ in 0..pixels {
                let mut film = Film::new(1, 1);
                let samples: Vec<(u32, Vec3A)> =
                    (0..n).map(|i| (i, Vec3A::splat(gauss()))).collect();
                film.merge(0, 0, &delta_for(&samples));
                total += film.pixel_error(0, 0) as f64;
            }
            total / pixels as f64
        };

        let n = 64u32;
        let expected = ERROR_CALIBRATION as f64 * sigma as f64 / ((n as f64).sqrt() * mu as f64);
        let observed = mean_error(n, 4000, 1);
        let ratio = observed / expected;
        assert!(
            (0.85..1.15).contains(&ratio),
            "observed {observed:.4} vs expected {expected:.4} (ratio {ratio:.3})"
        );

        // 1/√n decay: 4× the samples ≈ half the error.
        let observed_4n = mean_error(4 * n, 4000, 2);
        let decay = observed / observed_4n;
        assert!(
            (1.7..2.3).contains(&decay),
            "error decayed by {decay:.2}, expected ≈ 2"
        );
    }
}
