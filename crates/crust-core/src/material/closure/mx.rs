//! MaterialX's reference BSDF math, ported from its GLSL library
//! (`libraries/pbrlib/genglsl/lib/mx_microfacet*.glsl`, MaterialX 1.39,
//! Apache-2.0 — see `THIRD-PARTY.md`).
//!
//! The closure evaluator uses these where MaterialX's own semantics are the
//! reference: the Fresnel models a leaf names (dielectric, conductor,
//! generalized Schlick with F82, Airy thin film), the GGX directional-albedo
//! fit and Turquin's multiple-scattering compensation built on it, and the
//! diffuse and sheen lobes with their albedo fits. Function names follow the
//! GLSL (`mx_` dropped) so each can be checked against its source line by line.
//!
//! The fits' coefficients are copied digit for digit from the GLSL, beyond
//! `f32` precision in places, so they can be diffed against the source rather
//! than re-derived.
#![allow(clippy::excessive_precision)]

use glam::Vec3A;
use std::f32::consts::PI;
use utils::exp3;

const FLOAT_EPS: f32 = 1e-8;

fn sq(x: f32) -> f32 {
    x * x
}

fn pow5(x: f32) -> f32 {
    sq(sq(x)) * x
}

fn pow6(x: f32) -> f32 {
    let x2 = sq(x);
    sq(x2) * x2
}

// ---------------------------------------------------------------------------
// GGX albedo and energy compensation
// ---------------------------------------------------------------------------

/// `mx_ggx_dir_albedo_analytic`: rational quadratic fit to the GGX
/// directional albedo, split into its `F0` and `F90` terms.
pub fn ggx_dir_albedo(n_dot_v: f32, alpha: f32, f0: Vec3A, f90: Vec3A) -> Vec3A {
    let x = n_dot_v;
    let y = alpha;
    let x2 = sq(x);
    let y2 = sq(y);
    let c = |k: [f32; 9]| {
        k[0] + k[1] * x
            + k[2] * y
            + k[3] * x * y
            + k[4] * x2
            + k[5] * y2
            + k[6] * x2 * y
            + k[7] * x * y2
            + k[8] * x2 * y2
    };
    let r0 = c([
        0.1003, -0.6303, 9.748, -2.038, 29.34, -8.245, -26.44, 19.99, -5.448,
    ]);
    let r1 = c([
        0.9345, -2.323, 2.229, -3.748, 1.424, -0.7684, 1.436, 0.2913, 0.6286,
    ]);
    let r2 = c([
        1.0, -1.765, 8.263, 11.53, 28.96, -7.507, -36.11, 15.86, 33.37,
    ]);
    let r3 = c([
        1.0, 0.2281, 15.94, -55.83, 13.08, 41.26, 54.9, 300.2, -285.1,
    ]);
    let a = (r0 / r2).clamp(0.0, 1.0);
    let b = (r1 / r3).clamp(0.0, 1.0);
    f0 * a + f90 * b
}

/// `mx_ggx_energy_compensation`: Turquin's multiple-scattering scale for a
/// single-scatter Fresnel `fss`.
pub fn ggx_energy_compensation(n_dot_v: f32, alpha: f32, fss: Vec3A) -> Vec3A {
    let ess = ggx_dir_albedo(n_dot_v, alpha, Vec3A::ONE, Vec3A::ONE).x;
    Vec3A::ONE + fss * (1.0 - ess) / ess.max(FLOAT_EPS)
}

/// `mx_average_alpha`.
pub fn average_alpha(ax: f32, ay: f32) -> f32 {
    (ax * ay).sqrt()
}

/// `mx_ior_to_f0`.
pub fn ior_to_f0(ior: f32) -> f32 {
    sq((ior - 1.0) / (ior + 1.0))
}

/// `mx_f0_to_ior`, per channel.
pub fn f0_to_ior(f0: Vec3A) -> Vec3A {
    let s = f0.clamp(Vec3A::splat(0.01), Vec3A::splat(0.99)).powf(0.5);
    (Vec3A::ONE + s) / (Vec3A::ONE - s)
}

// ---------------------------------------------------------------------------
// Fresnel
// ---------------------------------------------------------------------------

/// `mx_fresnel_dielectric` (Lagarde), `1` under total internal reflection.
pub fn fresnel_dielectric(cos_theta: f32, ior: f32) -> f32 {
    let c = cos_theta;
    let g2 = ior * ior + c * c - 1.0;
    if g2 < 0.0 {
        return 1.0;
    }
    let g = g2.sqrt();
    0.5 * sq((g - c) / (g + c)) * (1.0 + sq(((g + c) * c - 1.0) / ((g - c) * c + 1.0)))
}

/// `mx_fresnel_dielectric_polarized`: `(Rp, Rs)`.
fn fresnel_dielectric_polarized(cos_theta: f32, ior: f32) -> (f32, f32) {
    let cos2 = sq(cos_theta.clamp(0.0, 1.0));
    let sin2 = 1.0 - cos2;
    let t0 = (ior * ior - sin2).max(0.0);
    let t1 = t0 + cos2;
    let t2 = 2.0 * t0.sqrt() * cos_theta;
    let rs = (t1 - t2) / (t1 + t2);
    let t3 = cos2 * t0 + sin2 * sin2;
    let t4 = t2 * sin2;
    let rp = rs * (t3 - t4) / (t3 + t4);
    (rp, rs)
}

/// `mx_fresnel_conductor_polarized`: `(Rp, Rs)` per channel.
fn fresnel_conductor_polarized(cos_theta: f32, n: Vec3A, k: Vec3A) -> (Vec3A, Vec3A) {
    let cos2 = sq(cos_theta.clamp(0.0, 1.0));
    let sin2 = 1.0 - cos2;
    let n2 = n * n;
    let k2 = k * k;
    let t0 = n2 - k2 - Vec3A::splat(sin2);
    let a2b2 = (t0 * t0 + 4.0 * n2 * k2).powf(0.5);
    let t1 = a2b2 + Vec3A::splat(cos2);
    let a = (0.5 * (a2b2 + t0)).max(Vec3A::ZERO).powf(0.5);
    let t2 = 2.0 * a * cos_theta;
    let rs = (t1 - t2) / (t1 + t2);
    let t3 = cos2 * a2b2 + Vec3A::splat(sin2 * sin2);
    let t4 = t2 * sin2;
    let rp = rs * (t3 - t4) / (t3 + t4);
    (rp, rs)
}

/// `mx_fresnel_conductor`.
pub fn fresnel_conductor(cos_theta: f32, n: Vec3A, k: Vec3A) -> Vec3A {
    let (rp, rs) = fresnel_conductor_polarized(cos_theta, n, k);
    0.5 * (rp + rs)
}

/// `mx_fresnel_hoffman_schlick`: generalized Schlick with an F82 tint.
///
/// At `f90 = 1`, `exponent = 5` this is `brdf::fresnel_f82_tint`, native
/// OpenPBR's metal Fresnel. The two are kept apart on purpose: this one tracks
/// the GLSL (free `f90` and exponent, no clamp), and folding either into the
/// other would move the other path's bits.
pub fn fresnel_hoffman_schlick(
    cos_theta: f32,
    f0: Vec3A,
    f82: Vec3A,
    f90: Vec3A,
    exponent: f32,
) -> Vec3A {
    const COS_THETA_MAX: f32 = 1.0 / 7.0;
    let factor = 1.0 / (COS_THETA_MAX * (1.0 - COS_THETA_MAX).powi(6));
    let x = cos_theta.clamp(0.0, 1.0);
    let a = f0.lerp(f90, (1.0 - COS_THETA_MAX).powf(exponent)) * (Vec3A::ONE - f82) * factor;
    f0.lerp(f90, (1.0 - x).powf(exponent)) - a * x * pow6(1.0 - x)
}

/// A leaf's Fresnel model, MaterialX's `FresnelData`.
#[derive(Clone, Copy, Debug)]
pub enum FresnelModel {
    Dielectric {
        ior: f32,
    },
    Conductor {
        n: Vec3A,
        k: Vec3A,
    },
    Schlick {
        f0: Vec3A,
        f82: Vec3A,
        f90: Vec3A,
        exponent: f32,
    },
}

/// MaterialX's `FresnelData`: a model plus an optional thin film
/// (thickness in nanometres, film IOR).
#[derive(Clone, Copy, Debug)]
pub struct Fresnel {
    pub model: FresnelModel,
    pub thin_film: Option<(f32, f32)>,
}

impl Fresnel {
    /// `mx_compute_fresnel`.
    pub fn eval(&self, cos_theta: f32) -> Vec3A {
        if self.thin_film.is_some() {
            return self.airy(cos_theta);
        }
        match self.model {
            FresnelModel::Dielectric { ior } => Vec3A::splat(fresnel_dielectric(cos_theta, ior)),
            FresnelModel::Conductor { n, k } => fresnel_conductor(cos_theta, n, k),
            FresnelModel::Schlick {
                f0,
                f82,
                f90,
                exponent,
            } => fresnel_hoffman_schlick(cos_theta, f0, f82, f90, exponent),
        }
    }

    /// `mx_ggx_dir_albedo(NdotV, alpha, FresnelData)`.
    pub fn dir_albedo(&self, n_dot_v: f32, alpha: f32) -> Vec3A {
        if self.thin_film.is_some() {
            let mirror = self.eval(n_dot_v);
            let f0 = self.airy(1.0);
            let rough = ggx_dir_albedo(n_dot_v, alpha, f0, Vec3A::ONE);
            return mirror.lerp(rough, alpha.sqrt());
        }
        match self.model {
            FresnelModel::Dielectric { ior } => {
                ggx_dir_albedo(n_dot_v, alpha, Vec3A::splat(ior_to_f0(ior)), Vec3A::ONE)
            }
            FresnelModel::Conductor { n, k } => {
                ggx_dir_albedo(n_dot_v, alpha, fresnel_conductor(1.0, n, k), Vec3A::ONE)
            }
            FresnelModel::Schlick { f0, f90, .. } => ggx_dir_albedo(n_dot_v, alpha, f0, f90),
        }
    }

    /// `mx_fresnel_airy`: Belcour & Barla's thin-film iridescence.
    fn airy(&self, cos_theta: f32) -> Vec3A {
        let (thickness, tf_ior) = self.thin_film.unwrap_or((0.0, 1.5));
        let eta1 = 1.0f32;
        let eta2 = tf_ior.max(eta1);
        let (eta3, kappa3, schlick) = match self.model {
            FresnelModel::Dielectric { ior } => (Vec3A::splat(ior), Vec3A::ZERO, false),
            FresnelModel::Conductor { n, k } => (n, k, false),
            FresnelModel::Schlick { f0, .. } => (f0_to_ior(f0), Vec3A::ZERO, true),
        };
        let cos_t2 = 1.0 - (1.0 - sq(cos_theta)) * sq(eta1 / eta2);
        let cos_t = cos_t2.max(0.0).sqrt();
        let (mut r12p, mut r12s) = fresnel_dielectric_polarized(cos_theta, eta2 / eta1);
        if cos_t <= 0.0 {
            r12p = 1.0;
            r12s = 1.0;
        }
        let (t121p, t121s) = (1.0 - r12p, 1.0 - r12s);
        let (r23p, r23s) = if schlick {
            let FresnelModel::Schlick {
                f0,
                f82,
                f90,
                exponent,
            } = self.model
            else {
                unreachable!()
            };
            let f = fresnel_hoffman_schlick(cos_t, f0, f82, f90, exponent);
            (0.5 * f, 0.5 * f)
        } else {
            fresnel_conductor_polarized(cos_t, eta3 / eta2, kappa3 / eta2)
        };
        let cos_b = (eta2 / eta1).atan().cos();
        let phi21 = (if cos_theta < cos_b { 0.0 } else { PI }, PI);
        let (phi23p, phi23s) = if schlick {
            let p = Vec3A::new(
                if eta3.x < eta2 { PI } else { 0.0 },
                if eta3.y < eta2 { PI } else { 0.0 },
                if eta3.z < eta2 { PI } else { 0.0 },
            );
            (p, p)
        } else {
            conductor_phase_polarized(cos_t, eta2, eta3, kappa3)
        };
        let r123p = (r12p * r23p).max(Vec3A::ZERO).powf(0.5);
        let r123s = (r12s * r23s).max(Vec3A::ZERO).powf(0.5);
        let opd = 2.0 * eta2 * cos_t * thickness * 1.0e-9;

        let mut i = Vec3A::ZERO;
        let rs = (sq(t121p) * r23p) / (Vec3A::ONE - r12p * r23p);
        i += Vec3A::splat(r12p) + rs;
        let mut cm = rs - Vec3A::splat(t121p);
        for m in 1..=AIRY_FRESNEL_ITERATIONS {
            cm *= r123p;
            let sm =
                2.0 * eval_sensitivity(m as f32 * opd, m as f32 * (phi23p + Vec3A::splat(phi21.0)));
            i += cm * sm;
        }
        let rp = (sq(t121s) * r23s) / (Vec3A::ONE - r12s * r23s);
        i += Vec3A::splat(r12s) + rp;
        let mut cm = rp - Vec3A::splat(t121s);
        for m in 1..=AIRY_FRESNEL_ITERATIONS {
            cm *= r123s;
            let sm =
                2.0 * eval_sensitivity(m as f32 * opd, m as f32 * (phi23s + Vec3A::splat(phi21.1)));
            i += cm * sm;
        }
        i *= 0.5;
        // XYZ → CIE 1931 RGB (E illuminant), GLSL's column-major `mat3`.
        let rgb = Vec3A::new(2.3706743, -0.5138850, 0.0052982) * i.x
            + Vec3A::new(-0.9000405, 1.4253036, -0.0146949) * i.y
            + Vec3A::new(-0.4706338, 0.0885814, 1.0093968) * i.z;
        rgb.clamp(Vec3A::ZERO, Vec3A::ONE)
    }
}

/// `AIRY_FRESNEL_ITERATIONS`, as MaterialX's GLSL generator defines it.
const AIRY_FRESNEL_ITERATIONS: u32 = 2;

/// `mx_fresnel_conductor_phase_polarized`.
fn conductor_phase_polarized(
    cos_theta: f32,
    eta1: f32,
    eta2: Vec3A,
    kappa2: Vec3A,
) -> (Vec3A, Vec3A) {
    let k2 = kappa2 / eta2;
    let sin2 = Vec3A::splat(1.0 - cos_theta * cos_theta);
    let a = eta2 * eta2 * (Vec3A::ONE - k2 * k2) - eta1 * eta1 * sin2;
    let b = (a * a + (2.0 * eta2 * eta2 * k2) * (2.0 * eta2 * eta2 * k2)).powf(0.5);
    let u = ((a + b) / 2.0).max(Vec3A::ZERO).powf(0.5);
    let v = ((b - a) / 2.0).max(Vec3A::ZERO).powf(0.5);
    let atan2 = |y: Vec3A, x: Vec3A| Vec3A::new(y.x.atan2(x.x), y.y.atan2(x.y), y.z.atan2(x.z));
    let phi_s = atan2(
        2.0 * eta1 * v * cos_theta,
        u * u + v * v - Vec3A::splat(sq(eta1 * cos_theta)),
    );
    let phi_p = atan2(
        2.0 * eta1 * eta2 * eta2 * cos_theta * (2.0 * k2 * u - (Vec3A::ONE - k2 * k2) * v),
        {
            let t = eta2 * eta2 * (Vec3A::ONE + k2 * k2) * cos_theta;
            t * t - eta1 * eta1 * (u * u + v * v)
        },
    );
    (phi_p, phi_s)
}

/// `mx_eval_sensitivity`: the Gaussian fit to the CIE XYZ sensitivity.
fn eval_sensitivity(opd: f32, shift: Vec3A) -> Vec3A {
    let phase = 2.0 * PI * opd;
    let val = Vec3A::new(5.4856e-13, 4.4201e-13, 5.2481e-13);
    let pos = Vec3A::new(1.6810e+06, 1.7953e+06, 2.2084e+06);
    let var = Vec3A::new(4.3278e+09, 9.3046e+09, 6.6121e+09);
    let cos = |v: Vec3A| Vec3A::new(v.x.cos(), v.y.cos(), v.z.cos());
    let mut xyz =
        val * (2.0 * PI * var).powf(0.5) * cos(pos * phase + shift) * exp3(-var * phase * phase);
    xyz.x += 9.7470e-14
        * (2.0 * PI * 4.5282e+09f32).sqrt()
        * (2.2399e+06 * phase + shift.x).cos()
        * (-4.5282e+09 * phase * phase).exp();
    xyz / 1.0685e-7
}

// ---------------------------------------------------------------------------
// Diffuse
// ---------------------------------------------------------------------------

const FUJII_1: f32 = 0.5 - 2.0 / (3.0 * PI);
const FUJII_2: f32 = 2.0 / 3.0 - 28.0 / (15.0 * PI);

/// `mx_oren_nayar_diffuse`: the qualitative Oren–Nayar factor.
pub fn oren_nayar(n_dot_v: f32, n_dot_l: f32, l_dot_v: f32, roughness: f32) -> f32 {
    let s = l_dot_v - n_dot_l * n_dot_v;
    let stinv = if s > 0.0 {
        s / n_dot_l.max(n_dot_v)
    } else {
        0.0
    };
    let sigma2 = sq(roughness);
    let a = 1.0 - 0.5 * (sigma2 / (sigma2 + 0.33));
    let b = 0.45 * sigma2 / (sigma2 + 0.09);
    a + b * stinv
}

/// `mx_oren_nayar_diffuse_dir_albedo_analytic`.
pub fn oren_nayar_dir_albedo(n_dot_v: f32, roughness: f32) -> f32 {
    let x = 1.0 - 0.4297 * roughness - 0.7632 * n_dot_v * roughness + 1.4385 * sq(roughness);
    let y = 1.0 - 0.6076 * roughness - 0.4993 * n_dot_v * roughness + 2.0315 * sq(roughness);
    (x / y).clamp(0.0, 1.0)
}

/// `mx_oren_nayar_fujii_diffuse_dir_albedo`.
fn fujii_dir_albedo(cos_theta: f32, roughness: f32) -> f32 {
    let a = 1.0 / (1.0 + FUJII_1 * roughness);
    let b = roughness * a;
    let si = (1.0 - sq(cos_theta)).max(0.0).sqrt();
    let c = cos_theta.max(FLOAT_EPS);
    let g = si * (cos_theta.clamp(-1.0, 1.0).acos() - si * cos_theta)
        + 2.0 * ((si / c) * (1.0 - si * si * si) - si) / 3.0;
    a + b * g / PI
}

/// `mx_oren_nayar_fujii_diffuse_avg_albedo`.
fn fujii_avg_albedo(roughness: f32) -> f32 {
    let a = 1.0 / (1.0 + FUJII_1 * roughness);
    a * (1.0 + FUJII_2 * roughness)
}

/// `mx_oren_nayar_compensated_diffuse` (EON), *without* the `1/π` the GLSL
/// applies at the call site.
pub fn eon(n_dot_v: f32, n_dot_l: f32, l_dot_v: f32, roughness: f32, color: Vec3A) -> Vec3A {
    let s = l_dot_v - n_dot_l * n_dot_v;
    let stinv = if s > 0.0 { s / n_dot_l.max(n_dot_v) } else { s };
    let a = 1.0 / (1.0 + FUJII_1 * roughness);
    let single = color * a * (1.0 + roughness * stinv);
    let ev = fujii_dir_albedo(n_dot_v, roughness);
    let el = fujii_dir_albedo(n_dot_l, roughness);
    let avg = fujii_avg_albedo(roughness);
    let ms_color = color * color * avg / (Vec3A::ONE - color * (1.0 - avg).max(0.0));
    let multi = ms_color * (1.0 - ev).max(FLOAT_EPS) * (1.0 - el).max(FLOAT_EPS)
        / (1.0 - avg).max(FLOAT_EPS);
    single + multi
}

/// `mx_oren_nayar_compensated_diffuse_dir_albedo`.
pub fn eon_dir_albedo(cos_theta: f32, roughness: f32, color: Vec3A) -> Vec3A {
    let e = fujii_dir_albedo(cos_theta, roughness);
    let avg = fujii_avg_albedo(roughness);
    let ms_color = color * color * avg / (Vec3A::ONE - color * (1.0 - avg).max(0.0));
    ms_color.lerp(color, e)
}

/// `mx_burley_diffuse`.
pub fn burley(n_dot_v: f32, n_dot_l: f32, l_dot_h: f32, roughness: f32) -> f32 {
    let f90 = 0.5 + 2.0 * roughness * sq(l_dot_h);
    let schlick = |c: f32| 1.0 + (f90 - 1.0) * pow5((1.0 - c).clamp(0.0, 1.0));
    schlick(n_dot_l) * schlick(n_dot_v)
}

/// `mx_burley_diffuse_dir_albedo` (Stephen Hill's fit).
pub fn burley_dir_albedo(n_dot_v: f32, roughness: f32) -> f32 {
    let x = n_dot_v;
    let fit0 = 0.97619 - 0.488095 * pow5(1.0 - x);
    let fit1 = 1.55754 + (-2.02221 + (2.56283 - 1.06244 * x) * x) * x;
    fit0 + (fit1 - fit0) * roughness
}

// ---------------------------------------------------------------------------
// Sheen
// ---------------------------------------------------------------------------

/// `mx_imageworks_sheen_brdf`: the Conty–Kulla Charlie sheen with Neubelt's
/// smoother denominator (Fresnel and geometry terms are 1).
pub fn imageworks_sheen(n_dot_l: f32, n_dot_v: f32, n_dot_h: f32, roughness: f32) -> f32 {
    let inv_r = 1.0 / roughness.max(0.005);
    let sin2 = 1.0 - n_dot_h * n_dot_h;
    let d = (2.0 + inv_r) * sin2.max(0.0).powf(inv_r * 0.5) / (2.0 * PI);
    d / (4.0 * (n_dot_l + n_dot_v - n_dot_l * n_dot_v))
}

/// `mx_imageworks_sheen_dir_albedo_analytic`, clamped to [0, 1].
pub fn imageworks_sheen_dir_albedo(n_dot_v: f32, roughness: f32) -> f32 {
    let x = 13.67300 - 68.78018 * n_dot_v + 799.08825 * roughness - 905.00061 * n_dot_v * roughness
        + 60.28956 * sq(n_dot_v)
        + 1086.96473 * sq(roughness);
    let y = 1.0
        + 61.57746 * n_dot_v
        + 442.78211 * roughness
        + 2597.49308 * n_dot_v * roughness
        + 121.81241 * sq(n_dot_v)
        + 3045.55075 * sq(roughness);
    (x / y).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ggx_albedo_fit_matches_its_known_endpoints() {
        // A smooth white mirror reflects everything; F0 = 0, F90 = 0 nothing.
        let e = ggx_dir_albedo(1.0, 1e-4, Vec3A::ONE, Vec3A::ONE).x;
        assert!((e - 1.0).abs() < 0.02, "{e}");
        assert_eq!(
            ggx_dir_albedo(0.5, 0.5, Vec3A::ZERO, Vec3A::ZERO),
            Vec3A::ZERO
        );
        // Roughness loses single-scatter energy, which compensation restores.
        let rough = ggx_dir_albedo(0.5, 1.0, Vec3A::ONE, Vec3A::ONE).x;
        assert!(rough < 0.8, "{rough}");
        let comp = ggx_energy_compensation(0.5, 1.0, Vec3A::ONE).x;
        assert!((rough * comp - 1.0).abs() < 1e-5);
    }

    #[test]
    fn dielectric_fresnel_has_the_textbook_normal_incidence_value() {
        assert!((fresnel_dielectric(1.0, 1.5) - 0.04).abs() < 1e-6);
        assert_eq!(fresnel_dielectric(0.1, 1.0 / 1.5), 1.0, "TIR");
    }

    #[test]
    fn a_conductor_with_no_extinction_is_a_dielectric() {
        let c = fresnel_conductor(0.7, Vec3A::splat(1.5), Vec3A::ZERO);
        assert!((c.x - fresnel_dielectric(0.7, 1.5)).abs() < 1e-5);
    }

    #[test]
    fn hoffman_schlick_hits_f0_at_normal_incidence_and_f90_at_grazing() {
        let f0 = Vec3A::new(0.9, 0.6, 0.2);
        let f82 = Vec3A::new(0.8, 0.8, 0.8);
        assert!(fresnel_hoffman_schlick(1.0, f0, f82, Vec3A::ONE, 5.0).abs_diff_eq(f0, 1e-6));
        assert!(
            fresnel_hoffman_schlick(0.0, f0, f82, Vec3A::ONE, 5.0).abs_diff_eq(Vec3A::ONE, 1e-6)
        );
    }

    #[test]
    fn a_zero_thickness_film_is_close_to_the_bare_interface() {
        let bare = Fresnel {
            model: FresnelModel::Dielectric { ior: 1.5 },
            thin_film: None,
        };
        let film = Fresnel {
            thin_film: Some((1e-3, 1.3)),
            ..bare
        };
        let (a, b) = (bare.eval(0.8), film.eval(0.8));
        assert!((a - b).abs().max_element() < 0.02, "{a} vs {b}");
    }

    #[test]
    fn eon_conserves_energy_on_white() {
        // A white EON surface's directional albedo is 1 at every angle.
        for mu in [0.2f32, 0.5, 0.9] {
            let e = eon_dir_albedo(mu, 0.7, Vec3A::ONE).x;
            assert!((e - 1.0).abs() < 1e-3, "{mu}: {e}");
        }
    }
}
