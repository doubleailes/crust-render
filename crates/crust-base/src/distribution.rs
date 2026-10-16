//! Sampling by CDF inversion, shared by the environment map and placement.

/// A piecewise-constant 1D distribution over `[0, 1)`, sampled by inverting
/// its CDF. The building block of the 2D environment distribution (one of
/// these over rows, and one per row over columns) and of area-weighted
/// triangle picks for placement (`crate::place`).
///
/// The weights accumulate in f64 and the CDF is stored in f32, as fine as
/// the f32 numbers it is inverted at: a bin under ~1e-7 of the total weight
/// can be over- or under-drawn.
pub struct Distribution1D {
    /// Unnormalized per-bin weights.
    func: Vec<f32>,
    /// `cdf[i]` is the summed weight below bin `i`; `len = func.len() + 1`.
    cdf: Vec<f32>,
    /// Integral of `func` over `[0, 1)` — the mean of `func`.
    pub integral: f32,
}

impl Distribution1D {
    pub fn new(func: Vec<f32>) -> Self {
        let n = func.len();
        let mut cdf = Vec::with_capacity(n + 1);
        cdf.push(0.0);
        let mut running = 0.0f64;
        for &f in &func {
            running += f as f64 / n as f64;
            cdf.push(running as f32);
        }
        let integral = running as f32;
        if integral > 0.0 {
            for c in cdf.iter_mut() {
                *c /= integral;
            }
        } else {
            // Degenerate (all-black) input: fall back to uniform, so a
            // black environment still samples without dividing by zero.
            for (i, c) in cdf.iter_mut().enumerate() {
                *c = i as f32 / n as f32;
            }
        }
        Self {
            func,
            cdf,
            integral,
        }
    }

    /// Inverts the CDF at `u`, returning `(x in [0,1), pdf, bin)`. The pdf
    /// is with respect to `x`, so it integrates to 1 over `[0, 1)`.
    pub fn sample(&self, u: f32) -> (f32, f32, usize) {
        // First index whose cdf exceeds `u`, minus one.
        let bin = match self
            .cdf
            .binary_search_by(|c| c.partial_cmp(&u).unwrap_or(std::cmp::Ordering::Less))
        {
            Ok(i) => i.min(self.func.len() - 1),
            Err(i) => i.saturating_sub(1).min(self.func.len() - 1),
        };
        let span = self.cdf[bin + 1] - self.cdf[bin];
        let within = if span > 0.0 {
            (u - self.cdf[bin]) / span
        } else {
            0.5
        };
        let x = (bin as f32 + within) / self.func.len() as f32;
        (x, self.pdf(bin), bin)
    }

    /// Density of bin `bin` with respect to `x` in `[0, 1)`.
    pub fn pdf(&self, bin: usize) -> f32 {
        if self.integral > 0.0 {
            self.func[bin] / self.integral
        } else {
            1.0
        }
    }
}
