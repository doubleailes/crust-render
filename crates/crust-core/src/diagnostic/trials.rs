//! The arithmetic of a trial (design D4, D5): per-pair efficiency, the
//! picture check, the trimmed error and the noise floor, the verdict rules,
//! the overall result across crops, and the shared reference every image of
//! a crop is measured against. Pure functions, so every rule the spec
//! states is a unit test.

use super::report::Verdict;

/// Within ±5% a ratio is noise, never a gain.
pub const EPSILON: f64 = 0.05;

/// A trial is suggested only above this overall ΔEff, and above it at the
/// target too — and `converged` holds when none is. A noise floor above it
/// means the probe could not have resolved a gain worth suggesting.
pub const SUGGEST_ABOVE: f64 = 1.10;

/// A crop is `biased` when its luminance moves by more than this share…
pub const BIAS_TOLERANCE: f64 = 0.02;

/// …and by more than this many standard errors, in every pair.
pub const BIAS_Z: f64 = 4.0;

/// The share of pixels the picture check leaves out: those whose two
/// values differ most, where fireflies land.
pub const SHIFT_TRIM: f64 = 0.01;

/// The share of pixels a trimmed MRSE leaves out: `crust diff`'s 0.1%.
pub const MRSE_TRIM: f64 = 0.001;

/// `E_T / E_B` for one interleaved pair, `E = 1/(time · MRSE)`:
/// `(t_B · MRSE_B) / (t_T · MRSE_T)`. Not finite when either side measured
/// nothing (a zero time or error), which every verdict reads as
/// inconclusive.
pub fn delta_eff(time_b: f64, mrse_b: f64, time_t: f64, mrse_t: f64) -> f64 {
    (time_b * mrse_b) / (time_t * mrse_t)
}

/// What one crop's verdict is decided from.
#[derive(Debug, Clone, Copy)]
pub struct CropEvidence<'a> {
    /// Each pair's trimmed ΔEff.
    pub pairs: &'a [f64],
    /// The median of the pairs' untrimmed ΔEff.
    pub untrimmed_median: f64,
    /// The crop's [`noise_floor`]; `None` (taken as 1) when unmeasured.
    pub noise_floor: Option<f64>,
    /// Whether the crop failed the picture check ([`biased`]).
    pub biased: bool,
}

/// One crop's verdict, the first that applies:
///
/// - `biased` when the crop failed the picture check;
/// - `better` when every pair is above both `1 + ε` and the noise floor,
///   and the untrimmed median is above 1;
/// - `worse` when every pair is below both `1 − ε` and the noise floor's
///   reciprocal, and the untrimmed median is below 1;
/// - `insufficient_samples` when the noise floor is above
///   [`SUGGEST_ABOVE`];
/// - `inconclusive` otherwise — and with no pair, or any that is not
///   finite.
///
/// Trimming withholds a verdict, never creates one: the untrimmed median
/// must already lean the same way.
pub fn crop_verdict(e: CropEvidence) -> Verdict {
    if e.biased {
        return Verdict::Biased;
    }
    if e.pairs.is_empty() || e.pairs.iter().any(|p| !p.is_finite()) {
        return Verdict::Inconclusive;
    }
    let floor = e.noise_floor.unwrap_or(1.0);
    let floor = if floor.is_nan() { f64::INFINITY } else { floor };
    let above = (1.0 + EPSILON).max(floor);
    let below = (1.0 - EPSILON).min(1.0 / floor);
    if e.pairs.iter().all(|&p| p > above) && e.untrimmed_median > 1.0 {
        Verdict::Better
    } else if e.pairs.iter().all(|&p| p < below) && e.untrimmed_median < 1.0 {
        Verdict::Worse
    } else if floor > SUGGEST_ABOVE {
        Verdict::InsufficientSamples
    } else {
        Verdict::Inconclusive
    }
}

/// `(median, min, max)` of `values`; NaN for an empty slice.
pub fn summary(values: &[f64]) -> (f64, f64, f64) {
    if values.is_empty() {
        return (f64::NAN, f64::NAN, f64::NAN);
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let median = if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    };
    (median, v[0], v[n - 1])
}

/// The geometric mean of `values`; NaN when any is not a finite positive.
pub fn geometric_mean(values: &[f64]) -> f64 {
    if values.is_empty() || values.iter().any(|m| !(m.is_finite() && *m > 0.0)) {
        return f64::NAN;
    }
    (values.iter().map(|m| m.ln()).sum::<f64>() / values.len() as f64).exp()
}

/// The overall result of a trial over its crops: the geometric mean of the
/// per-crop medians, and the verdict.
///
/// - `biased` when any crop is, whatever the efficiency;
/// - `mixed` when one crop is `better` and another `worse`;
/// - `better` when at least one crop is, none is worse, and the overall
///   ΔEff itself clears `1 + ε`;
/// - `worse`, symmetrically;
/// - `insufficient_samples` when no crop is `better` or `worse` and one is
///   `insufficient_samples`;
/// - `inconclusive` otherwise.
pub fn overall(per_crop: &[(f64, Verdict)]) -> (f64, Verdict) {
    let geo = geometric_mean(&per_crop.iter().map(|(m, _)| *m).collect::<Vec<_>>());
    let any = |v: Verdict| per_crop.iter().any(|(_, x)| *x == v);
    if any(Verdict::Biased) {
        return (geo, Verdict::Biased);
    }
    if !geo.is_finite() {
        return (f64::NAN, Verdict::Inconclusive);
    }
    let verdict = if any(Verdict::Better) && any(Verdict::Worse) {
        Verdict::Mixed
    } else if any(Verdict::Better) && geo > 1.0 + EPSILON {
        Verdict::Better
    } else if any(Verdict::Worse) && geo < 1.0 - EPSILON {
        Verdict::Worse
    } else if !any(Verdict::Better) && !any(Verdict::Worse) && any(Verdict::InsufficientSamples) {
        Verdict::InsufficientSamples
    } else {
        Verdict::Inconclusive
    };
    (geo, verdict)
}

/// Whether a trial clears the suggestion bar: overall `better`, with both
/// its overall ΔEff and its ΔEff at the target at least [`SUGGEST_ABOVE`].
/// The at-target value only vetoes: a NaN one fails.
pub fn meets_bar(verdict: Verdict, overall: f64, at_target: f64) -> bool {
    verdict == Verdict::Better && overall >= SUGGEST_ABOVE && at_target >= SUGGEST_ABOVE
}

/// A trial's ΔEff at the target (design D9): the baseline's projected time
/// to reach the target, setup included, over the trial's.
/// `t*_B = setup_B + R_B`, `t*_T = setup_T + R_B / ΔEff`, where `R_B` is
/// the baseline's projected render time. With no setup on either side it
/// is `ΔEff` exactly; NaN when either is unknown.
pub fn delta_eff_at_target(delta_eff: f64, setup_b: f64, setup_t: f64, render_b: f64) -> f64 {
    if !delta_eff.is_finite() {
        return f64::NAN;
    }
    if setup_b == 0.0 && setup_t == 0.0 {
        return delta_eff;
    }
    if !(render_b.is_finite() && render_b > 0.0) {
        return f64::NAN;
    }
    (setup_b + render_b) / (setup_t + render_b / delta_eff)
}

/// The picture check of one pair (design D2): how far the trial's mean
/// luminance moved from the baseline's, both rendered with the same seed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shift {
    /// Mean luminance over the kept pixels.
    pub mean_baseline: f64,
    pub mean_trial: f64,
    /// `Σ lum_T / Σ lum_B − 1` over the kept pixels.
    pub shift: f64,
    /// `shift` over its standard error `√Σ(var_T + var_B) / Σ lum_B`.
    pub z: f64,
}

impl Shift {
    /// Whether this pair moved the picture: beyond both the tolerance and
    /// the noise.
    pub fn moved(&self) -> bool {
        self.shift.abs() > BIAS_TOLERANCE && self.z.abs() > BIAS_Z
    }
}

/// How many of `n` pixels a trim of `share` leaves out: at least one, never
/// all (none of a single pixel).
fn trimmed_count(n: usize, share: f64) -> usize {
    if n < 2 {
        return 0;
    }
    ((n as f64 * share).ceil() as usize).clamp(1, n - 1)
}

/// The indices of the `k` largest of `values` (NaN counts as largest).
fn top(values: &[f64], k: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..values.len()).collect();
    if k == 0 {
        return Vec::new();
    }
    let key = |q: &usize| {
        let v = values[*q];
        if v.is_nan() { f64::INFINITY } else { v }
    };
    idx.select_nth_unstable_by(k - 1, |a, b| key(b).total_cmp(&key(a)));
    idx.truncate(k);
    idx
}

/// The ratio of two sums over the kept pixels, and its z (D2, D4):
/// `Σ lum_T / Σ lum_B − 1`, over `√Σ(var_T + var_B) / Σ lum_B`. Without a
/// covariance term the error is overestimated: the two images share their
/// seed, so they move together.
fn shift_over(b: &CropImage, t: &CropImage, keep: impl Iterator<Item = usize>) -> Shift {
    let (mut sb, mut st, mut sv, mut n) = (0.0, 0.0, 0.0, 0usize);
    for q in keep {
        sb += b.lum[q];
        st += t.lum[q];
        sv += b.var[q] + t.var[q];
        n += 1;
    }
    let shift = if sb == st { 0.0 } else { st / sb - 1.0 };
    let se = sv.sqrt() / sb;
    let z = if shift == 0.0 { 0.0 } else { shift / se };
    let n = n.max(1) as f64;
    Shift {
        mean_baseline: sb / n,
        mean_trial: st / n,
        shift,
        z,
    }
}

/// The picture check of one pair (D2): the luminance shift and its z over
/// the crop, leaving out the [`SHIFT_TRIM`] of pixels whose two values
/// differ most — a few firefly pixels cannot produce it.
pub fn luminance_shift(baseline: &CropImage, trial: &CropImage) -> Shift {
    let n = baseline.lum.len().min(trial.lum.len());
    let d: Vec<f64> = (0..n)
        .map(|q| (trial.lum[q] - baseline.lum[q]).abs())
        .collect();
    let mut out = vec![false; n];
    for q in top(&d, trimmed_count(n, SHIFT_TRIM)) {
        out[q] = true;
    }
    shift_over(baseline, trial, (0..n).filter(|&q| !out[q]))
}

/// The same ratio over every pixel (D4): the light-sampling reach, whose
/// missing energy is itself carried by the brightest pixels.
pub fn luminance_ratio(baseline: &CropImage, trial: &CropImage) -> Shift {
    shift_over(baseline, trial, 0..baseline.lum.len().min(trial.lum.len()))
}

/// Whether a crop is `biased`: every pair moved the picture. No pair is not
/// evidence.
pub fn biased(shifts: &[Shift]) -> bool {
    !shifts.is_empty() && shifts.iter().all(Shift::moved)
}

/// `var / max(ref², 1e-4)`, the term `mean_relative_error` sums.
fn relative(var: f64, reference: f64) -> f64 {
    var / (reference * reference).max(1e-4)
}

/// Both sides' MRSE in one pair (D8), trimmed and untrimmed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PairMrse {
    pub baseline: f64,
    pub trial: f64,
    pub baseline_untrimmed: f64,
    pub trial_untrimmed: f64,
}

/// The MRSE of both sides of a pair against `reference` (D8). The trimmed
/// values leave out the same pixels on both sides: those in the top
/// [`MRSE_TRIM`] (at least one) of either side's `var / ref²`, so the pair
/// compares the same pixels.
pub fn mrse_pair(baseline: &[f64], trial: &[f64], reference: &[f64]) -> PairMrse {
    let n = baseline.len().min(trial.len()).min(reference.len());
    let rb: Vec<f64> = (0..n)
        .map(|q| relative(baseline[q], reference[q]))
        .collect();
    let rt: Vec<f64> = (0..n).map(|q| relative(trial[q], reference[q])).collect();
    let k = trimmed_count(n, MRSE_TRIM);
    let mut out = vec![false; n];
    for q in top(&rb, k).into_iter().chain(top(&rt, k)) {
        out[q] = true;
    }
    // A union covering every pixel leaves nothing to compare: untrimmed.
    if out.iter().all(|&o| o) {
        out.iter_mut().for_each(|o| *o = false);
    }
    let mean = |r: &[f64], keep: &dyn Fn(usize) -> bool| {
        let (sum, count) = (0..n)
            .filter(|&q| keep(q))
            .fold((0.0, 0usize), |(s, c), q| (s + r[q], c + 1));
        sum / count.max(1) as f64
    };
    PairMrse {
        baseline: mean(&rb, &|q| !out[q]),
        trial: mean(&rt, &|q| !out[q]),
        baseline_untrimmed: mean(&rb, &|_| true),
        trial_untrimmed: mean(&rt, &|_| true),
    }
}

/// One image's MRSE against `reference`, trimmed on its own top
/// [`MRSE_TRIM`]: what the noise floor compares across seeds.
pub fn mrse_trimmed(var: &[f64], reference: &[f64]) -> f64 {
    let n = var.len().min(reference.len());
    let r: Vec<f64> = (0..n).map(|q| relative(var[q], reference[q])).collect();
    let mut out = vec![false; n];
    for q in top(&r, trimmed_count(n, MRSE_TRIM)) {
        out[q] = true;
    }
    let (sum, count) = (0..n)
        .filter(|&q| !out[q])
        .fold((0.0, 0usize), |(s, c), q| (s + r[q], c + 1));
    sum / count.max(1) as f64
}

/// A crop's noise floor (D7): how far its error estimate moves when only
/// the seed changes — the largest over the smallest of the baseline's
/// MRSEs, one per seed. `None` with fewer than two seeds.
pub fn noise_floor(baseline_mrse: &[f64]) -> Option<f64> {
    if baseline_mrse.len() < 2 {
        return None;
    }
    let (_, min, max) = summary(baseline_mrse);
    Some(max / min)
}

/// One unbiased image of a crop, reduced to what efficiency needs: each
/// pixel's luminance and the variance of its mean.
#[derive(Debug, Clone)]
pub struct CropImage {
    pub lum: Vec<f64>,
    pub var: Vec<f64>,
}

impl CropImage {
    fn mean_var(&self) -> f64 {
        self.var.iter().sum::<f64>() / self.var.len().max(1) as f64
    }
}

/// The reference of a crop: the inverse-variance blend of every unbiased
/// image of it (each weighted by one over its mean variance), and that blend's own variance,
/// `Σ (wₖ/W)² varₖ` per pixel. An image whose variance cannot be weighed
/// (non-finite or zero) weighs nothing; with none weighable, the first
/// image is the reference.
pub fn reference(images: &[&CropImage]) -> CropImage {
    let weights: Vec<f64> = images
        .iter()
        .map(|im| {
            let v = im.mean_var();
            if v.is_finite() && v > 0.0 {
                1.0 / v
            } else {
                0.0
            }
        })
        .collect();
    let total: f64 = weights.iter().sum();
    if images.is_empty() {
        return CropImage {
            lum: Vec::new(),
            var: Vec::new(),
        };
    }
    if total <= 0.0 {
        return images[0].clone();
    }
    let n = images[0].lum.len();
    let mut lum = vec![0.0; n];
    let mut var = vec![0.0; n];
    for (im, w) in images.iter().zip(&weights) {
        let share = w / total;
        if share == 0.0 {
            continue;
        }
        for q in 0..n {
            lum[q] += share * im.lum[q];
            var[q] += share * share * im.var[q];
        }
    }
    CropImage { lum, var }
}

/// MRSE of `var` against a reference luminance — the tracer's
/// `mean_relative_error`, the one estimator the diagnostic's efficiency comparisons use.
pub fn mrse(var: &[f64], reference_lum: &[f64]) -> f64 {
    crate::tracer::mean_relative_error(var, reference_lum)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A crop's evidence with pairs and an untrimmed median that agree, no
    /// floor, no bias.
    fn plain(pairs: &[f64]) -> CropEvidence<'_> {
        CropEvidence {
            pairs,
            untrimmed_median: summary(pairs).0,
            noise_floor: None,
            biased: false,
        }
    }

    #[test]
    fn within_the_noise_is_inconclusive() {
        // The spec's scenario: 1.03, 1.08, 0.98 on every crop, a noise
        // floor of 1.04.
        let pairs = [1.03, 1.08, 0.98];
        let e = CropEvidence {
            noise_floor: Some(1.04),
            ..plain(&pairs)
        };
        assert_eq!(crop_verdict(e), Verdict::Inconclusive);
        let (m, _, _) = summary(&pairs);
        let (geo, v) = overall(&[(m, Verdict::Inconclusive); 3]);
        assert!((geo - 1.03).abs() < 1e-12);
        assert_eq!(v, Verdict::Inconclusive);
        assert!(!meets_bar(v, geo, geo));
    }

    #[test]
    fn every_pair_must_clear_epsilon() {
        assert_eq!(crop_verdict(plain(&[1.2, 1.3, 1.06])), Verdict::Better);
        assert_eq!(
            crop_verdict(plain(&[1.2, 1.3, 1.05])),
            Verdict::Inconclusive
        );
        assert_eq!(crop_verdict(plain(&[0.5, 0.9, 0.94])), Verdict::Worse);
        assert_eq!(
            crop_verdict(plain(&[0.5, 0.9, 0.95])),
            Verdict::Inconclusive
        );
        assert_eq!(crop_verdict(plain(&[])), Verdict::Inconclusive);
        assert_eq!(crop_verdict(plain(&[1.5, f64::NAN])), Verdict::Inconclusive);
        assert_eq!(
            crop_verdict(plain(&[1.5, f64::INFINITY])),
            Verdict::Inconclusive
        );
    }

    #[test]
    fn every_pair_must_clear_the_noise_floor() {
        let pairs = [1.2, 1.3, 1.25];
        let floor = |f| CropEvidence {
            noise_floor: Some(f),
            ..plain(&pairs)
        };
        assert_eq!(crop_verdict(floor(1.1)), Verdict::Better);
        // 1.2 does not beat a floor of 1.22, which itself is above 1.10.
        assert_eq!(crop_verdict(floor(1.22)), Verdict::InsufficientSamples);
        let worse = [0.8, 0.85, 0.82];
        let e = CropEvidence {
            noise_floor: Some(1.2),
            ..plain(&worse)
        };
        // 0.85 is not below 1/1.2 = 0.833.
        assert_eq!(crop_verdict(e), Verdict::InsufficientSamples);
        let e = CropEvidence {
            noise_floor: Some(1.15),
            ..plain(&worse)
        };
        assert_eq!(crop_verdict(e), Verdict::Worse);
        // A floor that is not a number resolves nothing.
        assert_eq!(crop_verdict(floor(f64::NAN)), Verdict::InsufficientSamples);
    }

    /// The spec's scenario "The error moves between seeds".
    #[test]
    fn the_error_moves_between_seeds() {
        let floor = noise_floor(&[2.7, 6.1, 3.4]).expect("three seeds");
        assert!((floor - 2.259).abs() < 1e-3, "{floor}");
        let pairs = [1.3, 0.8, 1.9];
        let e = CropEvidence {
            noise_floor: Some(floor),
            ..plain(&pairs)
        };
        assert_eq!(crop_verdict(e), Verdict::InsufficientSamples);
        // One seed: no floor, taken as 1.
        assert_eq!(noise_floor(&[2.7]), None);
        assert_eq!(noise_floor(&[]), None);
    }

    /// The spec's scenario "Trimming withholds, never creates".
    #[test]
    fn trimming_withholds_never_creates() {
        let pairs = [1.2, 1.3, 1.25];
        let e = CropEvidence {
            untrimmed_median: 0.7,
            ..plain(&pairs)
        };
        assert_ne!(crop_verdict(e), Verdict::Better);
        let worse = [0.8, 0.7, 0.75];
        let e = CropEvidence {
            untrimmed_median: 1.3,
            ..plain(&worse)
        };
        assert_ne!(crop_verdict(e), Verdict::Worse);
    }

    #[test]
    fn a_biased_crop_is_biased_whatever_its_efficiency() {
        let pairs = [14.0, 13.0, 15.0];
        let e = CropEvidence {
            biased: true,
            ..plain(&pairs)
        };
        assert_eq!(crop_verdict(e), Verdict::Biased);
        // One biased crop makes the trial biased, better crops or not.
        let (geo, v) = overall(&[
            (14.0, Verdict::Biased),
            (13.0, Verdict::Better),
            (15.0, Verdict::Better),
        ]);
        assert_eq!(v, Verdict::Biased);
        assert!(geo > 13.0);
        assert!(!meets_bar(v, geo, geo));
        // Even when an efficiency could not be measured.
        assert_eq!(overall(&[(f64::NAN, Verdict::Biased)]).1, Verdict::Biased);
    }

    #[test]
    fn insufficient_samples_needs_no_resolved_crop() {
        let (_, v) = overall(&[
            (1.3, Verdict::InsufficientSamples),
            (1.0, Verdict::Inconclusive),
        ]);
        assert_eq!(v, Verdict::InsufficientSamples);
        // A resolved crop decides instead.
        let (_, v) = overall(&[(1.3, Verdict::InsufficientSamples), (1.5, Verdict::Better)]);
        assert_eq!(v, Verdict::Better);
        let (_, v) = overall(&[(1.0, Verdict::InsufficientSamples), (1.06, Verdict::Better)]);
        assert_eq!(v, Verdict::Inconclusive);
    }

    #[test]
    fn crops_that_disagree_are_mixed() {
        // The trial better on the high-variance crop, worse on the median.
        let (_, v) = overall(&[
            (1.8, Verdict::Better),
            (0.7, Verdict::Worse),
            (1.0, Verdict::Inconclusive),
        ]);
        assert_eq!(v, Verdict::Mixed);
    }

    #[test]
    fn the_overall_is_the_geometric_mean_of_the_medians() {
        let (geo, v) = overall(&[(2.0, Verdict::Better), (0.5, Verdict::Inconclusive)]);
        assert!((geo - 1.0).abs() < 1e-12);
        // One better crop does not carry an overall ratio that is noise.
        assert_eq!(v, Verdict::Inconclusive);
        let (geo, v) = overall(&[(1.5, Verdict::Better), (1.2, Verdict::Inconclusive)]);
        assert!((geo - (1.8f64).sqrt()).abs() < 1e-12);
        assert_eq!(v, Verdict::Better);
        let (_, v) = overall(&[(0.5, Verdict::Worse), (0.9, Verdict::Inconclusive)]);
        assert_eq!(v, Verdict::Worse);
        assert_eq!(overall(&[]).1, Verdict::Inconclusive);
        assert_eq!(
            overall(&[(f64::NAN, Verdict::Better)]).1,
            Verdict::Inconclusive
        );
    }

    #[test]
    fn summary_takes_the_middle() {
        assert_eq!(summary(&[3.0, 1.0, 2.0]), (2.0, 1.0, 3.0));
        assert_eq!(summary(&[4.0, 1.0, 2.0, 3.0]), (2.5, 1.0, 4.0));
        assert!(summary(&[]).0.is_nan());
    }

    #[test]
    fn delta_eff_compares_cost_times_error() {
        // Twice as fast at the same error: twice as efficient.
        assert_eq!(delta_eff(2.0, 0.1, 1.0, 0.1), 2.0);
        // Same time, a quarter of the error: four times.
        assert_eq!(delta_eff(1.0, 0.4, 1.0, 0.1), 4.0);
        assert!(!delta_eff(1.0, 0.1, 0.0, 0.0).is_finite());
    }

    #[test]
    fn the_reference_weighs_images_by_inverse_variance() {
        let a = CropImage {
            lum: vec![1.0, 2.0],
            var: vec![0.1, 0.1],
        };
        let b = CropImage {
            lum: vec![3.0, 4.0],
            var: vec![0.3, 0.3],
        };
        let r = reference(&[&a, &b]);
        // Weights 10 and 10/3: shares 3/4 and 1/4.
        assert!((r.lum[0] - (0.75 * 1.0 + 0.25 * 3.0)).abs() < 1e-12);
        assert!((r.var[0] - (0.75f64.powi(2) * 0.1 + 0.25f64.powi(2) * 0.3)).abs() < 1e-12);
        // An image with no variance estimate weighs nothing.
        let c = CropImage {
            lum: vec![9.0, 9.0],
            var: vec![f64::INFINITY, f64::INFINITY],
        };
        let r2 = reference(&[&a, &c]);
        assert_eq!(r2.lum, a.lum);
        assert_eq!(reference(&[&c]).lum, c.lum);
    }

    // -- The picture check -------------------------------------------------------

    /// A 100×100 crop of gently varying luminance, every pixel's variance
    /// `var`.
    fn image(var: f64) -> CropImage {
        let lum: Vec<f64> = (0..10_000)
            .map(|q| 0.5 + 0.25 * ((q as f64) * 0.37).sin())
            .collect();
        CropImage {
            var: vec![var; lum.len()],
            lum,
        }
    }

    #[test]
    fn identical_images_do_not_move() {
        let a = image(0.01);
        let s = luminance_shift(&a, &a.clone());
        assert_eq!((s.shift, s.z), (0.0, 0.0));
        assert!(!s.moved());
        // Without any variance either.
        let b = image(0.0);
        assert_eq!(luminance_shift(&b, &b).z, 0.0);
    }

    /// The spec's scenario "A setting that darkens the image": 39% of the
    /// baseline, far beyond the noise.
    #[test]
    fn a_uniform_darkening_is_biased() {
        let b = image(0.01);
        let mut t = b.clone();
        t.lum.iter_mut().for_each(|l| *l *= 0.39);
        t.var.iter_mut().for_each(|v| *v *= 0.39 * 0.39);
        let s = luminance_shift(&b, &t);
        assert!((s.shift + 0.61).abs() < 1e-9, "{s:?}");
        assert!(s.z < -BIAS_Z * 10.0, "{s:?}");
        assert!((s.mean_trial / s.mean_baseline - 0.39).abs() < 1e-9);
        assert!(biased(&[s, s, s]));
        // Every pair must move: one that did not keeps the crop unbiased.
        let still = luminance_shift(&b, &b);
        assert!(!biased(&[s, still, s]));
        assert!(!biased(&[]));
    }

    /// The spec's scenario "An unbiased setting on a scene with fireflies":
    /// a handful of firefly pixels, in either image, are left out.
    #[test]
    fn a_few_fireflies_are_not_a_bias() {
        let b = image(0.0001);
        let mut t = b.clone();
        // 50 of 10 000 pixels (0.5%), each 1000 times brighter: together
        // they would move the crop's mean several times over.
        for q in (0..10_000).step_by(200) {
            t.lum[q] *= 1000.0;
        }
        let s = luminance_shift(&b, &t);
        assert_eq!(s.shift, 0.0, "{s:?}");
        assert!(!s.moved());
        // Untrimmed, the same pixels are a severalfold shift.
        assert!(luminance_ratio(&b, &t).shift > 2.0);
    }

    /// The spec's scenario "A shift within the noise": 3% at z 1.5.
    #[test]
    fn a_shift_within_the_noise_is_not_biased() {
        let b = image(0.0);
        let mut t = b.clone();
        t.lum.iter_mut().for_each(|l| *l *= 1.03);
        // The standard error that makes this shift z = 1.5:
        // se = √(n_kept · 2v) / Σ lum_B.
        let kept: f64 = {
            let s = luminance_shift(&b, &t);
            s.mean_baseline * 9_900.0
        };
        let v = (0.03 * kept / 1.5).powi(2) / (2.0 * 9_900.0);
        let mut b = b;
        b.var.iter_mut().for_each(|x| *x = v);
        t.var.iter_mut().for_each(|x| *x = v);
        let s = luminance_shift(&b, &t);
        assert!((s.shift - 0.03).abs() < 1e-9, "{s:?}");
        assert!((s.z - 1.5).abs() < 1e-6, "{s:?}");
        assert!(!s.moved());
        assert!(!biased(&[s; 3]));
    }

    #[test]
    fn a_trim_leaves_at_least_one_and_never_all() {
        assert_eq!(trimmed_count(1, SHIFT_TRIM), 0);
        assert_eq!(trimmed_count(2, SHIFT_TRIM), 1);
        assert_eq!(trimmed_count(100, SHIFT_TRIM), 1);
        assert_eq!(trimmed_count(10_000, SHIFT_TRIM), 100);
        assert_eq!(trimmed_count(73_984, MRSE_TRIM), 74);
        assert_eq!(trimmed_count(3, 0.9), 2);
    }

    // -- Trimmed MRSE ------------------------------------------------------------

    /// One firefly pixel on one side is left out of both: the trimmed pair
    /// ratio stays at 1.
    #[test]
    fn one_firefly_leaves_the_trimmed_pair_ratio_at_one() {
        let reference = image(0.0).lum;
        let base = vec![0.01; reference.len()];
        let mut trial = base.clone();
        trial[1234] = 50.0;
        let m = mrse_pair(&base, &trial, &reference);
        assert_eq!(m.baseline, m.trial);
        assert!(m.trial_untrimmed > 1.5 * m.baseline_untrimmed, "{m:?}");
        assert_eq!(delta_eff(1.0, m.baseline, 1.0, m.trial), 1.0);
    }

    /// With no outlier, trimming changes the error by less than 0.1%.
    #[test]
    fn without_an_outlier_trimming_changes_little() {
        let reference: Vec<f64> = (0..10_000)
            .map(|q| 0.5 + 0.05 * ((q as f64) * 0.37).sin())
            .collect();
        let base: Vec<f64> = (0..reference.len())
            .map(|q| 0.01 * (1.0 + 0.1 * ((q as f64) * 1.3).cos()))
            .collect();
        let trial: Vec<f64> = base.iter().map(|v| 0.5 * v).collect();
        let m = mrse_pair(&base, &trial, &reference);
        for (a, b) in [
            (m.baseline, m.baseline_untrimmed),
            (m.trial, m.trial_untrimmed),
        ] {
            assert!((a / b - 1.0).abs() < 1e-3, "{m:?}");
        }
        assert!((m.trial / m.baseline - 0.5).abs() < 1e-3);
        // Untrimmed is the tracer's own estimator.
        assert!((m.baseline_untrimmed - mrse(&base, &reference)).abs() < 1e-15);
        // The same pixels, trimmed on one side alone, for the floor.
        assert!((mrse_trimmed(&base, &reference) / m.baseline - 1.0).abs() < 1e-3);
    }

    // -- At the target -----------------------------------------------------------

    #[test]
    fn without_setup_the_target_changes_nothing() {
        for d in [0.5, 1.0, 1.3, 14.0] {
            assert_eq!(delta_eff_at_target(d, 0.0, 0.0, 70.0), d);
            assert_eq!(delta_eff_at_target(d, 0.0, 0.0, f64::NAN), d);
        }
        assert!(delta_eff_at_target(f64::NAN, 0.0, 0.0, 70.0).is_nan());
    }

    /// The spec's scenario "Setup that the final render amortises": 30%
    /// more efficient, a pre-pass of twice a crop's 0.36 s render, against a
    /// 70 s projected render — counted once.
    #[test]
    fn a_setup_the_render_amortises() {
        let at = delta_eff_at_target(1.3, 0.0, 0.72, 70.0);
        assert!((at - 70.0 / (0.72 + 70.0 / 1.3)).abs() < 1e-12);
        assert!(at > SUGGEST_ABOVE, "{at}");
        assert!(meets_bar(Verdict::Better, 1.3, at));
        // The same setup on both sides weighs on neither.
        assert!(delta_eff_at_target(1.3, 0.72, 0.72, 70.0) < 1.3);
    }

    /// The spec's scenario "Setup that the final render does not
    /// amortise": better at 1.2 on render time, 0.9 at the target.
    #[test]
    fn a_setup_the_render_does_not_amortise_is_vetoed() {
        // 70 / (s + 70 / 1.2) = 0.9 → s = 70/0.9 − 70/1.2.
        let setup = 70.0 / 0.9 - 70.0 / 1.2;
        let at = delta_eff_at_target(1.2, 0.0, setup, 70.0);
        assert!((at - 0.9).abs() < 1e-12, "{at}");
        assert!(!meets_bar(Verdict::Better, 1.2, at));
        assert!(!meets_bar(Verdict::Better, 1.2, f64::NAN));
        assert!(delta_eff_at_target(1.2, 0.0, setup, f64::NAN).is_nan());
    }
}
