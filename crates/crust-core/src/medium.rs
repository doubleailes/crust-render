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
}
