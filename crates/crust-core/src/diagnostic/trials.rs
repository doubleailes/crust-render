//! The arithmetic of a trial (design D4, D5): per-pair efficiency, the
//! verdict rules, the overall result across crops, and the shared reference
//! every image of a crop is measured against. Pure functions, so every rule
//! the spec states is a unit test.

use super::report::Verdict;

/// Within ±5% a ratio is noise, never a gain.
pub const EPSILON: f64 = 0.05;

/// A trial is suggested only above this overall ΔEff — and `converged`
/// holds when none is.
pub const SUGGEST_ABOVE: f64 = 1.10;

/// `E_T / E_B` for one interleaved pair, `E = 1/(time · MRSE)`:
/// `(t_B · MRSE_B) / (t_T · MRSE_T)`. Not finite when either side measured
/// nothing (a zero time or error), which every verdict reads as
/// inconclusive.
pub fn delta_eff(time_b: f64, mrse_b: f64, time_t: f64, mrse_t: f64) -> f64 {
    (time_b * mrse_b) / (time_t * mrse_t)
}

/// One crop's verdict from its pairs: `better` when every pair is above
/// `1 + ε`, `worse` when every pair is below `1 − ε`, `inconclusive`
/// otherwise — and with no pair, or any that is not finite.
pub fn crop_verdict(pairs: &[f64]) -> Verdict {
    if pairs.is_empty() || pairs.iter().any(|p| !p.is_finite()) {
        Verdict::Inconclusive
    } else if pairs.iter().all(|&p| p > 1.0 + EPSILON) {
        Verdict::Better
    } else if pairs.iter().all(|&p| p < 1.0 - EPSILON) {
        Verdict::Worse
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

/// The overall result of a trial over its crops: the geometric mean of the
/// per-crop medians, and the verdict.
///
/// - `mixed` when one crop is `better` and another `worse`;
/// - `better` when at least one crop is, none is worse, and the overall
///   ΔEff itself clears `1 + ε`;
/// - `worse`, symmetrically;
/// - `inconclusive` otherwise.
pub fn overall(per_crop: &[(f64, Verdict)]) -> (f64, Verdict) {
    if per_crop.is_empty() || per_crop.iter().any(|(m, _)| !(m.is_finite() && *m > 0.0)) {
        return (f64::NAN, Verdict::Inconclusive);
    }
    let geo = (per_crop.iter().map(|(m, _)| m.ln()).sum::<f64>() / per_crop.len() as f64).exp();
    let any = |v: Verdict| per_crop.iter().any(|(_, x)| *x == v);
    let verdict = if any(Verdict::Better) && any(Verdict::Worse) {
        Verdict::Mixed
    } else if any(Verdict::Better) && geo > 1.0 + EPSILON {
        Verdict::Better
    } else if any(Verdict::Worse) && geo < 1.0 - EPSILON {
        Verdict::Worse
    } else {
        Verdict::Inconclusive
    };
    (geo, verdict)
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
/// image of it (each weighted by one over its mean variance, as
/// `render_guided` blends passes), and that blend's own variance,
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
/// `mean_relative_error`, the one estimator guiding's own ΔEff uses.
pub fn mrse(var: &[f64], reference_lum: &[f64]) -> f64 {
    crate::tracer::mean_relative_error(var, reference_lum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_the_noise_is_inconclusive() {
        // The spec's scenario: 1.03, 1.08, 0.98 on every crop.
        let pairs = [1.03, 1.08, 0.98];
        assert_eq!(crop_verdict(&pairs), Verdict::Inconclusive);
        let (m, _, _) = summary(&pairs);
        let (geo, v) = overall(&[(m, Verdict::Inconclusive); 3]);
        assert!((geo - 1.03).abs() < 1e-12);
        assert_eq!(v, Verdict::Inconclusive);
    }

    #[test]
    fn every_pair_must_clear_epsilon() {
        assert_eq!(crop_verdict(&[1.2, 1.3, 1.06]), Verdict::Better);
        assert_eq!(crop_verdict(&[1.2, 1.3, 1.05]), Verdict::Inconclusive);
        assert_eq!(crop_verdict(&[0.5, 0.9, 0.94]), Verdict::Worse);
        assert_eq!(crop_verdict(&[0.5, 0.9, 0.95]), Verdict::Inconclusive);
        assert_eq!(crop_verdict(&[]), Verdict::Inconclusive);
        assert_eq!(crop_verdict(&[1.5, f64::NAN]), Verdict::Inconclusive);
        assert_eq!(crop_verdict(&[1.5, f64::INFINITY]), Verdict::Inconclusive);
    }

    #[test]
    fn crops_that_disagree_are_mixed() {
        // Guiding better on the high-variance crop, worse on the median.
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
}
