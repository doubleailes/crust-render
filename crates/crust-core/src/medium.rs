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

/// Henyey–Greenstein phase-function sampling. Returns a scattering direction
/// in the local frame with the incoming direction `wi` on the +z axis of
/// its own frame, then rotated so `wi` maps back to itself.
pub fn sample_henyey_greenstein(wi: Vec3A, g: f32, u1: f32, u2: f32) -> Vec3A {
    use std::f32::consts::PI;
    let cos_theta = if g.abs() < 1e-3 {
        1.0 - 2.0 * u1
    } else {
        let sq = (1.0 - g * g) / (1.0 - g + 2.0 * g * u1);
        -(1.0 + g * g - sq * sq) / (2.0 * g)
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
