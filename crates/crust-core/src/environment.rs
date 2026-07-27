//! Lat-long environment maps for [`crate::light::DomeLight`].
//!
//! crust-core decodes nothing: the host hands it pixels through
//! [`crate::scene::AssetLoader`], which is what keeps this crate free of
//! image dependencies. What lives here is the part that is renderer
//! business — the direction↔pixel mapping and the importance sampling that
//! stops a small bright sun in a 4K HDRI from becoming a firefly farm.
//!
//! # Conventions
//!
//! Y-up, matching the rest of crust. For a unit direction `d`:
//!
//! - `v = acos(d.y) / π` — 0 at the +Y pole, 1 at −Y, so image row 0 is the
//!   top of the sky (the usual storage order).
//! - `u = 0.5 + atan2(d.x, −d.z) / 2π` — puts −Z, the direction a USD
//!   camera looks down, at the centre of the image.

use glam::Vec3A;

/// Perceptual weight used to decide where the light is. Importance
/// sampling only needs a scalar that tracks brightness; the sampled
/// radiance is always the full colour.
fn luminance(c: Vec3A) -> f32 {
    0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
}

/// A piecewise-constant 1D distribution over `[0, 1)`, sampled by inverting
/// its CDF. The building block of the 2D environment distribution: one of
/// these over rows, and one per row over columns.
struct Distribution1D {
    /// Unnormalized per-bin weights.
    func: Vec<f32>,
    /// `cdf[i]` is the summed weight below bin `i`; `len = func.len() + 1`.
    cdf: Vec<f32>,
    /// Integral of `func` over `[0, 1)` — the mean of `func`.
    integral: f32,
}

impl Distribution1D {
    fn new(func: Vec<f32>) -> Self {
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
    fn sample(&self, u: f32) -> (f32, f32, usize) {
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
    fn pdf(&self, bin: usize) -> f32 {
        if self.integral > 0.0 {
            self.func[bin] / self.integral
        } else {
            1.0
        }
    }
}

/// A lat-long environment map with a 2D sampling distribution built over
/// it.
///
/// The distribution weights each texel by luminance **times `sin θ`**: a
/// lat-long image devotes as many pixels to a degree near the pole as near
/// the equator, but those polar texels cover far less solid angle, and
/// omitting the Jacobian would over-sample the poles and bias the estimate.
pub struct EnvironmentMap {
    width: usize,
    height: usize,
    /// Row-major, row 0 at the +Y pole.
    pixels: Vec<Vec3A>,
    /// Over rows (`v`).
    marginal: Distribution1D,
    /// One per row, over columns (`u`).
    conditional: Vec<Distribution1D>,
}

impl EnvironmentMap {
    /// Builds a map from row-major RGB pixels, row 0 at the +Y pole.
    /// Returns `None` for an empty or mis-sized buffer.
    pub fn new(width: usize, height: usize, pixels: Vec<Vec3A>) -> Option<Self> {
        if width == 0 || height == 0 || pixels.len() != width * height {
            return None;
        }
        let mut conditional = Vec::with_capacity(height);
        let mut row_weights = Vec::with_capacity(height);
        for y in 0..height {
            // Solid-angle weight of this row of texels.
            let theta = (y as f32 + 0.5) / height as f32 * std::f32::consts::PI;
            let sin_theta = theta.sin();
            let row: Vec<f32> = (0..width)
                .map(|x| luminance(pixels[y * width + x]).max(0.0) * sin_theta)
                .collect();
            let d = Distribution1D::new(row);
            row_weights.push(d.integral);
            conditional.push(d);
        }
        Some(Self {
            width,
            height,
            pixels,
            marginal: Distribution1D::new(row_weights),
            conditional,
        })
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// `(u, v)` in `[0, 1)²` for a unit direction — see the module header
    /// for the convention.
    fn direction_to_uv(d: Vec3A) -> (f32, f32) {
        let v = d.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
        let u = 0.5 + d.x.atan2(-d.z) / std::f32::consts::TAU;
        (u.rem_euclid(1.0), v.clamp(0.0, 1.0))
    }

    /// The inverse of [`Self::direction_to_uv`].
    fn uv_to_direction(u: f32, v: f32) -> Vec3A {
        let theta = v * std::f32::consts::PI;
        let phi = (u - 0.5) * std::f32::consts::TAU;
        let sin_theta = theta.sin();
        Vec3A::new(sin_theta * phi.sin(), theta.cos(), -sin_theta * phi.cos())
    }

    /// Nearest-texel radiance along `direction`.
    pub fn radiance(&self, direction: Vec3A) -> Vec3A {
        let (u, v) = Self::direction_to_uv(direction);
        let x = ((u * self.width as f32) as usize).min(self.width - 1);
        let y = ((v * self.height as f32) as usize).min(self.height - 1);
        self.pixels[y * self.width + x]
    }

    /// Importance-samples a direction. Returns `(direction, radiance,
    /// solid-angle pdf)`; `None` only for a wholly black map, which has
    /// nothing to sample.
    pub fn sample(&self, u1: f32, u2: f32) -> Option<(Vec3A, Vec3A, f32)> {
        if self.marginal.integral <= 0.0 {
            return None;
        }
        let (v, pdf_v, row) = self.marginal.sample(u2);
        let (u, pdf_u, _) = self.conditional[row].sample(u1);
        let direction = Self::uv_to_direction(u, v);
        let pdf = self.solid_angle_pdf(pdf_u * pdf_v, v);
        (pdf > 0.0).then(|| (direction, self.radiance(direction), pdf))
    }

    /// Solid-angle pdf of `direction` under [`Self::sample`] — the bounce
    /// side of MIS, and it must agree with what `sample` reported.
    pub fn pdf(&self, direction: Vec3A) -> f32 {
        if self.marginal.integral <= 0.0 {
            return 0.0;
        }
        let (u, v) = Self::direction_to_uv(direction);
        let x = ((u * self.width as f32) as usize).min(self.width - 1);
        let y = ((v * self.height as f32) as usize).min(self.height - 1);
        self.solid_angle_pdf(self.conditional[y].pdf(x) * self.marginal.pdf(y), v)
    }

    /// Converts a density over `(u, v)` into one over solid angle. The
    /// lat-long map covers the sphere as `dω = 2π² sin θ du dv`, so the
    /// pdf divides by that; at the poles `sin θ → 0` and the direction is
    /// unreachable.
    fn solid_angle_pdf(&self, pdf_uv: f32, v: f32) -> f32 {
        let sin_theta = (v * std::f32::consts::PI).sin();
        if sin_theta <= 0.0 {
            return 0.0;
        }
        pdf_uv / (2.0 * std::f32::consts::PI * std::f32::consts::PI * sin_theta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map with a single bright texel on an otherwise dim background —
    /// the case importance sampling exists for.
    fn spotty_map(w: usize, h: usize, bright_x: usize, bright_y: usize) -> EnvironmentMap {
        let mut pixels = vec![Vec3A::splat(0.05); w * h];
        pixels[bright_y * w + bright_x] = Vec3A::splat(500.0);
        EnvironmentMap::new(w, h, pixels).expect("valid map")
    }

    #[test]
    fn direction_and_uv_round_trip() {
        for d in [
            Vec3A::Y,
            -Vec3A::Y,
            Vec3A::X,
            -Vec3A::X,
            Vec3A::Z,
            -Vec3A::Z,
            Vec3A::new(0.3, 0.5, -0.8).normalize(),
            Vec3A::new(-0.6, -0.2, 0.7).normalize(),
        ] {
            let (u, v) = EnvironmentMap::direction_to_uv(d);
            let back = EnvironmentMap::uv_to_direction(u, v);
            assert!(
                back.distance(d) < 1e-4,
                "round trip failed for {d:?}: uv=({u}, {v}) -> {back:?}"
            );
        }
    }

    /// A USD camera looks down -Z, so -Z must land at the centre of the
    /// image and +Y at the top row.
    #[test]
    fn conventions_are_as_documented() {
        let (u, v) = EnvironmentMap::direction_to_uv(-Vec3A::Z);
        assert!((u - 0.5).abs() < 1e-5, "-Z should be at u = 0.5, got {u}");
        assert!((v - 0.5).abs() < 1e-5, "the horizon should be at v = 0.5, got {v}");

        let (_, v_top) = EnvironmentMap::direction_to_uv(Vec3A::Y);
        assert!(v_top < 1e-5, "+Y should be the top row, got v = {v_top}");
        let (_, v_bot) = EnvironmentMap::direction_to_uv(-Vec3A::Y);
        assert!(v_bot > 1.0 - 1e-5, "-Y should be the bottom row, got v = {v_bot}");
    }

    /// `sample` and `pdf` are the two MIS sides of one strategy: for any
    /// sampled direction they must report the same density, or emission is
    /// double-counted.
    #[test]
    fn sample_and_pdf_agree() {
        let map = spotty_map(64, 32, 40, 8);
        let mut rng = openqmc::pcg::Rng::new(11);
        for _ in 0..3000 {
            let Some((dir, _, pdf)) = map.sample(rng.next_f32(), rng.next_f32()) else {
                continue;
            };
            let queried = map.pdf(dir);
            assert!(
                (pdf - queried).abs() <= 1e-3 * pdf.max(queried),
                "MIS sides disagree: sample {pdf} vs pdf() {queried} for {dir:?}"
            );
        }
    }

    /// The sampling density must integrate to 1 over the sphere. Estimated
    /// as E[1/pdf] over the map's own samples, which converges to the
    /// sphere's total solid angle, 4π, when the density is normalized.
    #[test]
    fn pdf_is_normalized_over_the_sphere() {
        let map = spotty_map(32, 16, 20, 6);
        let mut rng = openqmc::pcg::Rng::new(5);
        let n = 200_000;
        let mut total = 0.0f64;
        for _ in 0..n {
            if let Some((_, _, pdf)) = map.sample(rng.next_f32(), rng.next_f32())
                && pdf > 0.0
            {
                total += 1.0 / pdf as f64;
            }
        }
        let estimate = total / n as f64;
        let expected = 4.0 * std::f64::consts::PI;
        assert!(
            (estimate - expected).abs() < 0.05 * expected,
            "E[1/pdf] = {estimate}, want ~{expected} (4π)"
        );
    }

    /// Importance sampling must actually chase the bright texel: with one
    /// texel 10 000x the background, most samples should land on it.
    #[test]
    fn sampling_concentrates_on_bright_texels() {
        let (w, h, bx, by) = (32usize, 16usize, 20usize, 6usize);
        let map = spotty_map(w, h, bx, by);
        let mut rng = openqmc::pcg::Rng::new(3);
        let (mut hits, mut n) = (0usize, 0usize);
        for _ in 0..20_000 {
            let Some((dir, _, _)) = map.sample(rng.next_f32(), rng.next_f32()) else {
                continue;
            };
            n += 1;
            let (u, v) = EnvironmentMap::direction_to_uv(dir);
            let x = ((u * w as f32) as usize).min(w - 1);
            let y = ((v * h as f32) as usize).min(h - 1);
            if x == bx && y == by {
                hits += 1;
            }
        }
        let fraction = hits as f32 / n as f32;
        // Uniform sampling would land there ~1/512 of the time.
        assert!(
            fraction > 0.5,
            "only {:.1}% of samples found the bright texel — importance \
             sampling is not working",
            100.0 * fraction
        );
    }

    /// The sin θ Jacobian. Without it, polar texels — which cover far less
    /// solid angle than equatorial ones but occupy just as many pixels —
    /// would be sampled far too often, and their solid-angle pdf would
    /// blow up as `1/sin θ`.
    ///
    /// Evaluated at texel *centres*, where the density's piecewise-constant
    /// discretization vanishes: the pdf of a uniform map is then exactly
    /// uniform over the sphere, `1/4π`. (Away from centres it differs by
    /// the ratio of the texel's `sin θ` to the direction's — a real
    /// property of a piecewise-constant density, not an error. What must
    /// hold everywhere is normalization and agreement between the two MIS
    /// sides, which `pdf_is_normalized_over_the_sphere` and
    /// `sample_and_pdf_agree` cover.)
    #[test]
    fn uniform_map_has_uniform_solid_angle_pdf() {
        let (w, h) = (32usize, 16usize);
        let map = EnvironmentMap::new(w, h, vec![Vec3A::ONE; w * h]).expect("valid");
        let expected = 1.0 / (4.0 * std::f32::consts::PI);

        // Every row from the pole to the equator, at the texel centre.
        for y in 0..h {
            let v = (y as f32 + 0.5) / h as f32;
            let u = 0.5 / w as f32;
            let d = EnvironmentMap::uv_to_direction(u, v);
            let pdf = map.pdf(d);
            assert!(
                (pdf - expected).abs() < 0.01 * expected,
                "row {y} (v = {v}): pdf {pdf} is not uniform (want {expected}) — \
                 the sin θ Jacobian is missing or wrong"
            );
        }
    }

    /// The same property stated the way it fails: a near-pole direction and
    /// an equatorial one must have comparable solid-angle densities. Drop
    /// the Jacobian and the pole's would be larger by `1/sin θ` — here
    /// about tenfold.
    #[test]
    fn poles_are_not_oversampled() {
        let (w, h) = (64usize, 32usize);
        let map = EnvironmentMap::new(w, h, vec![Vec3A::ONE; w * h]).expect("valid");
        let at_row = |y: usize| {
            let v = (y as f32 + 0.5) / h as f32;
            map.pdf(EnvironmentMap::uv_to_direction(0.5 / w as f32, v))
        };
        let pole = at_row(0);
        let equator = at_row(h / 2);
        assert!(
            (pole - equator).abs() < 0.02 * equator,
            "pole pdf {pole} vs equator {equator} — polar texels are being \
             over-sampled"
        );
    }

    #[test]
    fn rejects_malformed_buffers() {
        assert!(EnvironmentMap::new(0, 4, vec![]).is_none());
        assert!(EnvironmentMap::new(4, 4, vec![Vec3A::ONE; 3]).is_none());
    }

    /// An all-black map has nothing to sample, and must say so rather than
    /// dividing by a zero integral.
    #[test]
    fn black_map_declines_to_sample() {
        let map = EnvironmentMap::new(8, 4, vec![Vec3A::ZERO; 32]).expect("valid");
        assert!(map.sample(0.5, 0.5).is_none());
        assert_eq!(map.pdf(Vec3A::Y), 0.0);
    }
}
