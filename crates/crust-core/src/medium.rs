//! Homogeneous participating medium — absorption + scattering + phase.
//!
//! A `Medium` describes the interior of a solid or volume that light passes
//! through. It is carried by a `Ray` (as an `Option`) so the tracer knows
//! when to apply Beer-Lambert attenuation between surface hits, and can
//! sample volume scattering events for participating volumes.
//!
//! For an OpenPBR transmissive surface, the medium is derived from
//! `transmission_color` and `transmission_depth`:
//!
//!   σₐ = -ln(transmission_color) / max(transmission_depth, ε)
//!
//! `transmission_depth = 0` collapses to a purely tinting delta transmission
//! (Beer-Lambert becomes identity), which is what artists usually want for
//! thin coloured glass.

use glam::Vec3A;

#[derive(Debug, Clone)]
pub struct Medium {
    /// Per-channel absorption coefficient.
    pub sigma_a: Vec3A,
    /// Per-channel scattering coefficient. Zero → non-scattering medium
    /// (Beer-Lambert only, no volume scattering events).
    pub sigma_s: Vec3A,
    /// Henyey–Greenstein phase-function anisotropy in (-1, 1).
    pub g: f32,
}

impl Medium {
    /// Build a Medium from OpenPBR-style tint + depth. `depth = 0` yields
    /// zero absorption (identity transmittance regardless of `tint`).
    pub fn from_transmission(tint: Vec3A, depth: f32) -> Self {
        let sigma_a = if depth <= 1e-6 {
            Vec3A::ZERO
        } else {
            // Per-channel: σₐ = -ln(tint) / depth, clamped so a fully-black
            // channel doesn't blow up.
            let t = tint.max(Vec3A::splat(1e-4)).min(Vec3A::ONE);
            Vec3A::new(-t.x.ln(), -t.y.ln(), -t.z.ln()) / depth
        };
        Self {
            sigma_a,
            sigma_s: Vec3A::ZERO,
            g: 0.0,
        }
    }

    /// Build a scattering Medium for subsurface scattering — the
    /// "artist-friendly" parameterisation from Chiang et al. 2016 as adapted
    /// by OpenPBR (`subsurface_color` = albedo, `radius * radius_scale` =
    /// mean free path per channel).
    pub fn from_subsurface(albedo: Vec3A, radius: f32, radius_scale: Vec3A, g: f32) -> Self {
        let mfp = (Vec3A::splat(radius) * radius_scale).max(Vec3A::splat(1e-4));
        let sigma_t = Vec3A::ONE / mfp;
        let a = albedo.clamp(Vec3A::splat(1e-4), Vec3A::splat(0.999));
        let sigma_s = sigma_t * a;
        let sigma_a = sigma_t - sigma_s;
        Self { sigma_a, sigma_s, g }
    }

    /// Beer–Lambert transmittance across a segment of length `t`.
    pub fn transmittance(&self, t: f32) -> Vec3A {
        let e = (self.sigma_a + self.sigma_s) * t;
        Vec3A::new((-e.x).exp(), (-e.y).exp(), (-e.z).exp())
    }

    /// True when the medium scatters (subsurface, participating volumes).
    /// Non-scattering media use pure Beer-Lambert and never fire volume
    /// scattering events.
    pub fn is_scattering(&self) -> bool {
        self.sigma_s.max_element() > 1e-6
    }

    /// Extinction majorant used for distance sampling — the max-channel
    /// value of `σₐ + σₛ`.
    pub fn sigma_t_max(&self) -> f32 {
        (self.sigma_a + self.sigma_s).max_element()
    }

    /// Single-scattering albedo `σₛ / σₜ`, per channel.
    pub fn albedo(&self) -> Vec3A {
        let sigma_t = self.sigma_a + self.sigma_s;
        let denom = sigma_t.max(Vec3A::splat(1e-6));
        self.sigma_s / denom
    }
}

/// Henyey–Greenstein phase-function value — equal to its solid-angle pdf,
/// since HG is normalized over the sphere. `cos_theta` is the dot product
/// between the *propagation* direction of the incoming ray and the sampled
/// outgoing direction, matching `sample_henyey_greenstein` (forward
/// scattering ⇒ `cos_theta → +1` for `g > 0`).
pub fn hg_phase(cos_theta: f32, g: f32) -> f32 {
    use std::f32::consts::PI;
    let denom = (1.0 + g * g - 2.0 * g * cos_theta).max(1e-6);
    (1.0 - g * g) / (4.0 * PI * denom * denom.sqrt())
}

/// Henyey–Greenstein phase-function sampling. Returns a scattering direction
/// in the local frame with the incoming direction `wi` on the +z axis of
/// its own frame, then rotated so `wi` maps back to itself. `g > 0` scatters
/// forward (along the propagation direction `wi`), matching `hg_phase`.
pub fn sample_henyey_greenstein(wi: Vec3A, g: f32, u1: f32, u2: f32) -> Vec3A {
    use std::f32::consts::PI;
    let cos_theta = if g.abs() < 1e-3 {
        1.0 - 2.0 * u1
    } else {
        // PBRT's inversion, but evaluated for the propagation-direction
        // convention: the sign is chosen so g > 0 peaks at cos θ = +1
        // (forward). PBRT's own frame is built around the reversed
        // direction wo, which is why its formula carries a leading minus.
        let sq = (1.0 - g * g) / (1.0 - g + 2.0 * g * u1);
        (1.0 + g * g - sq * sq) / (2.0 * g)
    };
    let cos_theta = cos_theta.clamp(-1.0, 1.0);
    let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
    let phi = 2.0 * PI * u2;

    // Build a frame with wi along +z.
    let up = if wi.z.abs() < 0.999 {
        Vec3A::Z
    } else {
        Vec3A::X
    };
    let t = wi.cross(up).normalize();
    let b = wi.cross(t);
    (t * (sin_theta * phi.cos()) + b * (sin_theta * phi.sin()) + wi * cos_theta).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hg_phase_normalizes_over_sphere() {
        // ∫ p(µ) dµ over the sphere: 2π ∫_{-1}^{1} p(µ) dµ = 1.
        for &g in &[-0.7f32, 0.0, 0.4, 0.9] {
            let n = 20_000;
            let mut sum = 0.0f64;
            for i in 0..n {
                let mu = -1.0 + 2.0 * (i as f32 + 0.5) / n as f32;
                sum += hg_phase(mu, g) as f64;
            }
            let integral = 2.0 * std::f64::consts::PI * sum * (2.0 / n as f64);
            assert!(
                (integral - 1.0).abs() < 1e-3,
                "g={g}: integral={integral}"
            );
        }
    }

    #[test]
    fn hg_sampling_consistent_with_pdf() {
        // Histogram sampled cos θ against the pdf. This pins the sign
        // convention shared by `sample_henyey_greenstein` and `hg_phase`:
        // cos θ is measured against the incoming *propagation* direction.
        for &g in &[-0.5f32, 0.4, 0.8] {
            let wi = Vec3A::new(0.3, -0.5, 0.8).normalize();
            let bins = 20usize;
            let n = 200_000u32;
            let mut hist = vec![0u32; bins];
            let mut rng = 0x9e3779b9u32;
            let mut next = || {
                // xorshift — deterministic, good enough for a histogram.
                rng ^= rng << 13;
                rng ^= rng >> 17;
                rng ^= rng << 5;
                (rng >> 8) as f32 / (1u32 << 24) as f32
            };
            for _ in 0..n {
                let dir = sample_henyey_greenstein(wi, g, next(), next());
                let mu = wi.dot(dir).clamp(-1.0, 1.0);
                let b = (((mu + 1.0) * 0.5) * bins as f32).min(bins as f32 - 1.0) as usize;
                hist[b] += 1;
            }
            // Exact bin mass from the HG CDF:
            // ∫ 2π p dµ = (1-g²)/(2g) · [(1+g²-2gµ)^{-1/2}]_a^b.
            let cdf_term = |mu: f32| (1.0 + g * g - 2.0 * g * mu).max(1e-9).powf(-0.5);
            for b in 0..bins {
                let lo = -1.0 + b as f32 * 2.0 / bins as f32;
                let hi = lo + 2.0 / bins as f32;
                let expected = (1.0 - g * g) / (2.0 * g) * (cdf_term(hi) - cdf_term(lo));
                let observed = hist[b] as f32 / n as f32;
                // 5% relative, floored at 4σ of binomial counting noise.
                let tol = (0.05 * expected).max(4.0 * (expected / n as f32).sqrt());
                assert!(
                    (observed - expected).abs() < tol,
                    "g={g} bin={b}: observed={observed} expected={expected}"
                );
            }
        }
    }
}
