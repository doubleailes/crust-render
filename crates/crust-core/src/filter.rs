//! Pixel reconstruction filters, applied by **filter importance sampling**
//! (FIS): instead of splatting each sample into every pixel whose filter
//! footprint covers it, each pixel draws its sample *positions* from the
//! filter's own distribution and weights the radiance by `f(x)/p(x)`. The
//! expectation is the same filtered measurement, but every sample belongs to
//! exactly one pixel — which is what keeps the renderer's strictly per-pixel
//! machinery (adaptive early-stop, per-pixel QMC domains, inverse-variance
//! pass blending) intact. A weight-buffer splatting film would break all
//! three.
//!
//! For the non-negative filters the sampled density *is* the filter, so the
//! weight is 1 (up to the tabulation quadrature); Mitchell's negative lobes
//! sample `|f|` and carry the sign in the weight, which is how a filter with
//! negative lobes stays unbiased under FIS.
//!
//! The default is [`PixelFilter::Triangle`] at radius 1.0. The historical
//! behavior — the uniform in-pixel jitter — is still available as `box` at
//! radius 0.5, which reduces to it **bit-identically** (pinned by
//! `box_half_radius_is_the_identity_jitter`); select it explicitly to
//! compare against renders from before filtering existed.

/// Which reconstruction filter shapes each pixel's measurement, and over what
/// radius (in pixels, from the pixel center; a radius of 0.5 covers exactly
/// the pixel's own footprint). Selected per scene via `crust:pixelFilter` /
/// `crust:pixelFilterRadius` or the `--filter` / `--filter-radius` CLI flags.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PixelFilter {
    /// Uniform box. At radius 0.5, the classic one-pixel box — bit-identical
    /// to the unfiltered jitter the renderer used before filters existed.
    BoxFilter { radius: f32 },
    /// Tent / triangle filter — the default, at radius 1.0.
    Triangle { radius: f32 },
    /// Truncated Gaussian, σ = radius/3 (so the kernel reaches ~zero at the
    /// radius), with the tail value subtracted so it is exactly zero there.
    Gaussian { radius: f32 },
    /// 4-term Blackman-Harris window.
    Blackman { radius: f32 },
    /// Mitchell-Netravali, B = C = 1/3. The only filter here with negative
    /// lobes: sharper edges, at the price of possible ringing (and negative
    /// pixel values) next to hard contrast.
    Mitchell { radius: f32 },
}

impl Default for PixelFilter {
    fn default() -> Self {
        PixelFilter::Triangle { radius: 1.0 }
    }
}

/// Narrower than any sensible filter, but keeps a zero or negative authored
/// radius from producing a degenerate distribution.
const MIN_RADIUS: f32 = 0.01;

impl PixelFilter {
    /// The filter a name maps to, at that filter's conventional default
    /// radius (box 0.5, triangle 1.0, gaussian/blackman 1.5, mitchell 2.0).
    /// `None` for a name that is no filter — the caller owns the warning.
    pub fn from_name(name: &str) -> Option<PixelFilter> {
        Some(match name {
            "box" => PixelFilter::BoxFilter { radius: 0.5 },
            "triangle" => PixelFilter::Triangle { radius: 1.0 },
            "gaussian" => PixelFilter::Gaussian { radius: 1.5 },
            "blackman" => PixelFilter::Blackman { radius: 1.5 },
            "mitchell" => PixelFilter::Mitchell { radius: 2.0 },
            _ => return None,
        })
    }

    /// The same filter with an explicit radius (clamped to stay sane).
    pub fn with_radius(self, radius: f32) -> PixelFilter {
        let radius = radius.max(MIN_RADIUS);
        match self {
            PixelFilter::BoxFilter { .. } => PixelFilter::BoxFilter { radius },
            PixelFilter::Triangle { .. } => PixelFilter::Triangle { radius },
            PixelFilter::Gaussian { .. } => PixelFilter::Gaussian { radius },
            PixelFilter::Blackman { .. } => PixelFilter::Blackman { radius },
            PixelFilter::Mitchell { .. } => PixelFilter::Mitchell { radius },
        }
    }

    pub fn radius(&self) -> f32 {
        match *self {
            PixelFilter::BoxFilter { radius }
            | PixelFilter::Triangle { radius }
            | PixelFilter::Gaussian { radius }
            | PixelFilter::Blackman { radius }
            | PixelFilter::Mitchell { radius } => radius,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            PixelFilter::BoxFilter { .. } => "box",
            PixelFilter::Triangle { .. } => "triangle",
            PixelFilter::Gaussian { .. } => "gaussian",
            PixelFilter::Blackman { .. } => "blackman",
            PixelFilter::Mitchell { .. } => "mitchell",
        }
    }

    /// Evaluate the (unnormalized) 1D filter kernel at `x` pixels from the
    /// pixel center. Zero outside `[-radius, radius]`. The 2D filter is the
    /// separable product.
    pub fn eval(&self, x: f32) -> f32 {
        let r = self.radius();
        if x.abs() > r {
            return 0.0;
        }
        match *self {
            PixelFilter::BoxFilter { .. } => 1.0,
            PixelFilter::Triangle { radius } => radius - x.abs(),
            PixelFilter::Gaussian { radius } => {
                let sigma = radius / 3.0;
                let g = |v: f32| (-0.5 * v * v / (sigma * sigma)).exp();
                (g(x) - g(radius)).max(0.0)
            }
            PixelFilter::Blackman { radius } => {
                // 4-term Blackman-Harris window over [0, 1].
                let t = (x + radius) / (2.0 * radius);
                let w = std::f32::consts::TAU * t;
                0.35875 - 0.48829 * w.cos() + 0.14128 * (2.0 * w).cos()
                    - 0.01168 * (3.0 * w).cos()
            }
            PixelFilter::Mitchell { radius } => {
                // Mitchell-Netravali with B = C = 1/3 on |x'| ∈ [0, 2].
                const B: f32 = 1.0 / 3.0;
                const C: f32 = 1.0 / 3.0;
                let x = (2.0 * x / radius).abs();
                let x2 = x * x;
                let x3 = x2 * x;
                (if x < 1.0 {
                    (12.0 - 9.0 * B - 6.0 * C) * x3
                        + (-18.0 + 12.0 * B + 6.0 * C) * x2
                        + (6.0 - 2.0 * B)
                } else {
                    (-B - 6.0 * C) * x3 + (6.0 * B + 30.0 * C) * x2
                        + (-12.0 * B - 48.0 * C) * x
                        + (8.0 * B + 24.0 * C)
                }) / 6.0
            }
        }
    }
}

/// Tabulated-CDF bin count for the kinds without an analytic inverse CDF.
const BINS: usize = 64;

/// Per-pass sampler for a [`PixelFilter`]: turns one uniform `[0,1)` variate
/// per axis into a sample offset plus its FIS weight. Built once per render
/// pass (the tabulation is 64 kernel evaluations), shared read-only across
/// the worker threads.
pub struct FilterSampler {
    filter: PixelFilter,
    /// `|f|` importance table — only for gaussian / blackman / mitchell.
    table: Option<FilterTable>,
}

struct FilterTable {
    /// CDF of the per-bin `|f|` masses; `cdf[0] = 0`, `cdf[BINS] = 1`.
    cdf: [f32; BINS + 1],
    /// `|f(midpoint_b)|` — the piecewise-constant density is proportional
    /// to this within bin `b`.
    abs_mid: [f32; BINS],
    /// Σ|f(mid)| and Σf(mid): together they normalize the weight so that
    /// `E[w] = 1` (the effective filter integrates to one).
    abs_sum: f32,
    signed_sum: f32,
}

impl FilterSampler {
    pub fn new(filter: PixelFilter) -> Self {
        let table = match filter {
            PixelFilter::BoxFilter { .. } | PixelFilter::Triangle { .. } => None,
            _ => Some(FilterTable::new(filter)),
        };
        FilterSampler { filter, table }
    }

    /// Map a uniform `u ∈ [0,1)` to `(offset, weight)`: the sample's offset
    /// from the pixel's lower corner along one axis (`0.5` is the pixel
    /// center; outside `[0,1]` reaches into neighboring pixels' footprints),
    /// and the FIS weight `f(x)/p(x)` its radiance is multiplied by. The 2D
    /// weight is the product of the two per-axis weights.
    ///
    /// The mapping is monotone in `u`, so a stratified/QMC input stays
    /// stratified in the warped position.
    ///
    /// For the box at radius 0.5 this is the identity `(u, 1.0)` —
    /// bit-exactly, which is what makes it the escape hatch back to the
    /// pre-filter jitter: the arithmetic below evaluates as
    /// `(0.5 - 0.5) + (2.0·0.5)·u = 0.0 + 1.0·u`, and both of those
    /// operations are exact in IEEE 754.
    pub fn sample(&self, u: f32) -> (f32, f32) {
        let r = self.filter.radius();
        match self.filter {
            PixelFilter::BoxFilter { .. } => ((0.5 - r) + (2.0 * r) * u, 1.0),
            PixelFilter::Triangle { .. } => {
                // Analytic inverse CDF of the tent on [-r, r].
                let x = if u < 0.5 {
                    r * ((2.0 * u).sqrt() - 1.0)
                } else {
                    r * (1.0 - (2.0 * (1.0 - u)).sqrt())
                };
                (0.5 + x, 1.0)
            }
            _ => {
                let table = self.table.as_ref().expect("tabulated kinds carry a table");
                let (x, abs_mid) = table.sample(u, r);
                // w = f(x)/(p(x)·c): p is the tabulated |f| density, c the
                // signed normalization — Σ terms arranged so the bin width
                // cancels. Negative exactly on Mitchell's negative lobes.
                let w = self.filter.eval(x) * table.abs_sum / (abs_mid * table.signed_sum);
                (0.5 + x, w)
            }
        }
    }
}

impl FilterTable {
    fn new(filter: PixelFilter) -> Self {
        let r = filter.radius();
        let mut abs_mid = [0.0f32; BINS];
        let mut signed_sum = 0.0f32;
        for (b, slot) in abs_mid.iter_mut().enumerate() {
            let x = -r + (b as f32 + 0.5) * (2.0 * r / BINS as f32);
            let f = filter.eval(x);
            *slot = f.abs();
            signed_sum += f;
        }
        let abs_sum: f32 = abs_mid.iter().sum();
        debug_assert!(
            abs_sum > 0.0 && signed_sum > 0.0,
            "a pixel filter must have positive mass"
        );
        let mut cdf = [0.0f32; BINS + 1];
        for b in 0..BINS {
            cdf[b + 1] = cdf[b] + abs_mid[b] / abs_sum;
        }
        cdf[BINS] = 1.0;
        FilterTable {
            cdf,
            abs_mid,
            abs_sum,
            signed_sum,
        }
    }

    /// Invert the tabulated CDF: bin by binary search, linear within the
    /// bin. Returns the offset from the pixel center and the bin's `|f|`
    /// (the unnormalized density the weight divides by).
    fn sample(&self, u: f32, r: f32) -> (f32, f32) {
        // partition_point returns the first index with cdf > u; that index
        // is the bin's upper edge, so the bin itself is one less.
        let b = self.cdf[..BINS].partition_point(|&c| c <= u).max(1) - 1;
        let lo = self.cdf[b];
        let hi = self.cdf[b + 1];
        let t = if hi > lo { (u - lo) / (hi - lo) } else { 0.5 };
        let x = -r + (b as f32 + t) * (2.0 * r / BINS as f32);
        (x, self.abs_mid[b].max(f32::MIN_POSITIVE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openqmc::pcg::Rng;

    /// The renderer's historical camera jitter is `pixel + u` with `u`
    /// uniform in [0,1). Box at radius 0.5 must reproduce it bit-exactly —
    /// it is the escape hatch for comparing against pre-filter renders.
    #[test]
    fn box_half_radius_is_the_identity_jitter() {
        let s = FilterSampler::new(PixelFilter::BoxFilter { radius: 0.5 });
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            let u = rng.next_f32();
            let (x, w) = s.sample(u);
            assert_eq!(x.to_bits(), u.to_bits());
            assert_eq!(w.to_bits(), 1.0f32.to_bits());
        }
    }

    /// Every filter's samples stay inside its support, and the mapping is
    /// monotone (a warped stratified sequence stays stratified).
    #[test]
    fn samples_stay_in_support_and_monotone() {
        for filter in all_filters() {
            let s = FilterSampler::new(filter);
            let r = filter.radius();
            let mut prev = f32::NEG_INFINITY;
            for i in 0..=1000 {
                let u = (i as f32 / 1000.0).min(0.999_999);
                let (x, _) = s.sample(u);
                assert!(
                    (0.5 - r - 1e-3..=0.5 + r + 1e-3).contains(&x),
                    "{}: x = {x} outside radius {r}",
                    filter.name()
                );
                assert!(x >= prev, "{}: not monotone at u = {u}", filter.name());
                prev = x;
            }
        }
    }

    /// FIS weights must average to 1 — that is the normalization of the
    /// effective filter, and any drift here is an image-wide exposure
    /// error. Also pins that only Mitchell produces negative weights.
    #[test]
    fn weights_average_to_one() {
        for filter in all_filters() {
            let s = FilterSampler::new(filter);
            let n = 200_000;
            let mut sum = 0.0f64;
            let mut negatives = 0usize;
            let mut rng = Rng::new(11);
            for _ in 0..n {
                let (_, w) = s.sample(rng.next_f32());
                sum += w as f64;
                negatives += (w < 0.0) as usize;
            }
            let mean = sum / n as f64;
            assert!(
                (mean - 1.0).abs() < 5e-3,
                "{}: E[w] = {mean}",
                filter.name()
            );
            assert_eq!(
                negatives > 0,
                matches!(filter, PixelFilter::Mitchell { .. }),
                "{}: negative lobes only belong to Mitchell",
                filter.name()
            );
        }
    }

    /// The sampled positions, weighted by the FIS weight, reproduce the
    /// filter kernel itself: a histogram of Σw per bin must match ∫f over
    /// the bin. This is the whole contract — sampling + weight = filter.
    #[test]
    fn weighted_histogram_reproduces_the_kernel() {
        for filter in all_filters() {
            let s = FilterSampler::new(filter);
            let r = filter.radius();
            const H: usize = 16;
            let mut hist = [0.0f64; H];
            let n = 400_000;
            let mut rng = Rng::new(23);
            for _ in 0..n {
                let (x, w) = s.sample(rng.next_f32());
                let b = (((x - 0.5 + r) / (2.0 * r)) * H as f32) as usize;
                hist[b.min(H - 1)] += w as f64 / n as f64;
            }
            // Expected mass per bin: midpoint-rule ∫f over the bin,
            // normalized by ∫f over the support.
            let eval_mid = |b: usize| {
                let x = -r + (b as f32 + 0.5) * (2.0 * r / H as f32);
                filter.eval(x) as f64
            };
            let total: f64 = (0..H).map(eval_mid).sum();
            for (b, got) in hist.iter().enumerate() {
                let want = eval_mid(b) / total;
                assert!(
                    (got - want).abs() < 0.01,
                    "{} bin {b}: got {got:.4}, want {want:.4}",
                    filter.name()
                );
            }
        }
    }

    /// `from_name` covers exactly the documented names, at the documented
    /// default radii; `with_radius` clamps away degenerate values.
    #[test]
    fn names_and_radii() {
        assert_eq!(
            PixelFilter::from_name("box"),
            Some(PixelFilter::BoxFilter { radius: 0.5 })
        );
        assert_eq!(
            PixelFilter::from_name("triangle"),
            Some(PixelFilter::Triangle { radius: 1.0 })
        );
        assert_eq!(
            PixelFilter::from_name("gaussian"),
            Some(PixelFilter::Gaussian { radius: 1.5 })
        );
        assert_eq!(
            PixelFilter::from_name("blackman"),
            Some(PixelFilter::Blackman { radius: 1.5 })
        );
        assert_eq!(
            PixelFilter::from_name("mitchell"),
            Some(PixelFilter::Mitchell { radius: 2.0 })
        );
        assert_eq!(PixelFilter::from_name("lanczos"), None);
        assert_eq!(
            PixelFilter::from_name("gaussian").unwrap().with_radius(2.0),
            PixelFilter::Gaussian { radius: 2.0 }
        );
        assert!(PixelFilter::default().with_radius(-1.0).radius() > 0.0);
    }

    fn all_filters() -> [PixelFilter; 5] {
        [
            PixelFilter::BoxFilter { radius: 0.5 },
            PixelFilter::Triangle { radius: 1.0 },
            PixelFilter::Gaussian { radius: 1.5 },
            PixelFilter::Blackman { radius: 1.5 },
            PixelFilter::Mitchell { radius: 2.0 },
        ]
    }
}
