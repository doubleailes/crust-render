//! MaterialX `chiang_hair_bsdf`: Chiang, Bitterli, Tappan and Burley 2016's
//! fibre scattering model, as pbrt-v3's `HairBSDF` implements it, read with
//! MaterialX's parameters.
//!
//! The leaf frame is the fibre's: `x` along the strand (the leaf's tangent,
//! `curve_direction`), `z` the tube's ray-facing normal, `y = z × x`. A
//! direction's longitudinal angle θ is measured from the plane normal to the
//! fibre (`sin θ = w.x`) and its azimuth φ in that plane (`atan2(w.z, w.y)`),
//! exactly pbrt-v3's convention. The offset across the fibre is not an input
//! here: γo, the angle between the view direction projected into the normal
//! plane and the normal, is what pbrt derives from `h`, and on a round tube
//! the normal already says where on the circle the ray landed — the way
//! MaterialX's genglsl derives it too.
//!
//! What differs from genglsl, deliberately (see the materials design record):
//! genglsl has no sampling routine, scales its response by `1/π`, and gives
//! TRRT+ an azimuthal term of π/2 instead of `1/(2π)`. Here the model is
//! pbrt's — energy-conserving and importance-sampled — and only the
//! parameterisation is MaterialX's:
//!
//! - `tint_R`, `tint_TT`, `tint_TRT` scale their lobe's attenuation; TRRT+
//!   takes `tint_TRT`.
//! - Each roughness is `(v, s)`: the longitudinal variance and the azimuthal
//!   logistic scale, clamped to [0.001, 1] as genglsl clamps them, with the
//!   logistic's √(π/8) applied here (pbrt folds it into `s`).
//! - `cuticle_angle` in [0, 1] is `α = cuticle_angle·π − π/2`. genglsl tilts
//!   θi by `(2 − 3p)·α`, which moves each lobe's peak the opposite way to
//!   pbrt's tilt of θo by the same α; pbrt's formulas therefore run on `−α`,
//!   so a highlight lands where MaterialX's viewer puts it.
//! - `absorption_coefficient` is σa per unit radius.

use glam::Vec3A;
use std::f32::consts::PI;

/// The four lobes: R, TT, TRT, and TRRT+ (every longer path, summed in
/// closed form).
const P_MAX: usize = 3;

/// √(π/8): the logistic scale factor pbrt-v3 applies to `s` and genglsl to
/// its trimmed logistic (`0.626657`).
const SQRT_PI_OVER_8: f32 = 0.626_657_07;

/// A `chiang_hair_bsdf` leaf's inputs, as the closure tree resolved them.
#[derive(Clone, Copy, Debug)]
pub struct HairParams {
    pub tint: [Vec3A; 3],
    pub ior: f32,
    /// `(longitudinal variance, azimuthal scale)` for R, TT and TRT.
    pub roughness: [(f32, f32); 3],
    /// MaterialX's `cuticle_angle`, in [0, 1].
    pub cuticle_angle: f32,
    pub absorption: Vec3A,
}

/// A hair lobe prepared at one vertex: everything that depends only on ωo is
/// computed once, so `eval` and `sample` pay only for ωi.
#[derive(Clone, Copy, Debug)]
pub struct Hair {
    /// `sin θo'` and `cos θo'` per lobe, θo tilted by the cuticle as that
    /// lobe sees it (TRRT+ untilted).
    sin_o: [f32; 4],
    cos_o: [f32; 4],
    /// The tinted attenuation `tint_p · A_p(ωo)` per lobe, RGB.
    ap: [[f32; 3]; 4],
    /// The probability of sampling each lobe: its share of the attenuation's
    /// luminance.
    ap_pdf: [f32; 4],
    /// Longitudinal variance per lobe; TRRT+ shares TRT's.
    v: [f32; 3],
    /// Azimuthal logistic scale per lobe, √(π/8) applied.
    s: [f32; 3],
    gamma_o: f32,
    gamma_t: f32,
    phi_o: f32,
}

impl Hair {
    /// The lobe toward local ωo `wo` (unit length), with `luma` the working
    /// space's luminance.
    pub fn new(p: &HairParams, wo: Vec3A, luma: impl Fn(Vec3A) -> f32) -> Hair {
        let ior = if p.ior.is_finite() {
            p.ior.max(1.0 + 1e-4)
        } else {
            1.55
        };
        let rough = |i: usize| {
            let (v, s) = p.roughness[i];
            let clamp = |x: f32| {
                if x.is_finite() {
                    x.clamp(0.001, 1.0)
                } else {
                    0.001
                }
            };
            (clamp(v), clamp(s) * SQRT_PI_OVER_8)
        };
        let (v, s) = (
            [rough(0).0, rough(1).0, rough(2).0],
            [rough(0).1, rough(1).1, rough(2).1],
        );

        let sin_theta_o = wo.x.clamp(-1.0, 1.0);
        let cos_theta_o = safe_sqrt(1.0 - sin_theta_o * sin_theta_o);
        let phi_o = wo.z.atan2(wo.y);
        // γo: the tube normal is ωo's projection into the normal plane,
        // turned toward +y by γo.
        let across = (wo.y * wo.y + wo.z * wo.z).sqrt();
        let (sin_gamma_o, cos_gamma_o) = if across > 1e-7 {
            ((-wo.y / across).clamp(-1.0, 1.0), wo.z / across)
        } else {
            (0.0, 1.0)
        };
        let gamma_o = sin_gamma_o.asin();

        // The refracted ray inside the fibre.
        let sin_theta_t = sin_theta_o / ior;
        let cos_theta_t = safe_sqrt(1.0 - sin_theta_t * sin_theta_t);
        let etap = safe_sqrt(ior * ior - sin_theta_o * sin_theta_o) / cos_theta_o.max(1e-6);
        let sin_gamma_t = (sin_gamma_o / etap).clamp(-1.0, 1.0);
        let cos_gamma_t = safe_sqrt(1.0 - sin_gamma_t * sin_gamma_t);
        let gamma_t = sin_gamma_t.asin();

        // Attenuation: Fresnel at the entry, absorption along each internal
        // chord, and the TRRT+ geometric series in closed form.
        let absorption = sanitize(p.absorption);
        let t = (-absorption * (2.0 * cos_gamma_t / cos_theta_t.max(1e-6))).exp();
        let f = fresnel_dielectric(cos_theta_o * cos_gamma_o.abs(), ior);
        let mut ap = [Vec3A::ZERO; 4];
        ap[0] = Vec3A::splat(f);
        ap[1] = (1.0 - f) * (1.0 - f) * t;
        ap[2] = ap[1] * t * f;
        let denom = Vec3A::ONE - t * f;
        ap[3] = ap[2] * t * f / denom.max(Vec3A::splat(1e-6));
        let tint = |i: usize| sanitize(p.tint[i.min(2)]);
        for (i, a) in ap.iter_mut().enumerate() {
            *a *= tint(i);
        }
        let lum = ap.map(|a| luma(a).max(0.0));
        let total: f32 = lum.iter().sum();
        let ap_pdf = if total > 0.0 {
            lum.map(|l| l / total)
        } else {
            [0.0; 4]
        };

        // The cuticle tilt, pbrt's rotation of θo by 2α (R), −α (TT) and
        // −4α (TRT), on genglsl's α negated.
        let alpha = -(p.cuticle_angle.clamp(0.0, 1.0) * PI - PI / 2.0);
        let shift = [2.0 * alpha, -alpha, -4.0 * alpha];
        let mut sin_o = [sin_theta_o; 4];
        let mut cos_o = [cos_theta_o; 4];
        for (k, &a) in shift.iter().enumerate() {
            let (sa, ca) = a.sin_cos();
            // sin(θo − a), cos(θo − a)
            sin_o[k] = sin_theta_o * ca - cos_theta_o * sa;
            cos_o[k] = (cos_theta_o * ca + sin_theta_o * sa).abs();
        }

        Hair {
            sin_o,
            cos_o,
            ap: ap.map(|a| a.to_array()),
            ap_pdf,
            v,
            s,
            gamma_o,
            gamma_t,
            phi_o,
        }
    }

    /// The directional albedo `Σ_p tint_p·A_p(ωo)`: exact, because each lobe's
    /// `M_p` and `N_p` integrate to one.
    pub fn albedo(&self) -> Vec3A {
        self.ap
            .iter()
            .fold(Vec3A::ZERO, |sum, a| sum + Vec3A::from_array(*a))
    }

    /// Whether every lobe is black (all tints zero): nothing to sample.
    pub fn is_black(&self) -> bool {
        self.ap_pdf.iter().all(|&p| p == 0.0)
    }

    /// `(f, pdf)` toward local ωi `wi` (unit length): the BSDF value with the
    /// `1/|cos θ|` pbrt divides by (`|wi.z|`, which the closure multiplies
    /// back), and the sampling density per solid angle. Both are zero where
    /// `wi.z` vanishes, a set of measure zero that would otherwise be
    /// `inf · 0`.
    pub fn eval(&self, wi: Vec3A) -> (Vec3A, f32) {
        let cos_n = wi.z.abs();
        if cos_n < 1e-7 {
            return (Vec3A::ZERO, 0.0);
        }
        let (sum, pdf) = self.terms(wi);
        (sum / cos_n, pdf)
    }

    /// `Σ_p M_p·A_p·N_p` and `Σ_p pdf_p·M_p·N_p` toward ωi.
    fn terms(&self, wi: Vec3A) -> (Vec3A, f32) {
        let sin_i = wi.x.clamp(-1.0, 1.0);
        let cos_i = safe_sqrt(1.0 - sin_i * sin_i);
        let phi = wi.z.atan2(wi.y) - self.phi_o;
        let mut sum = Vec3A::ZERO;
        let mut pdf = 0.0;
        for p in 0..P_MAX {
            let mp = mp(cos_i, self.cos_o[p], sin_i, self.sin_o[p], self.v[p]);
            let np = np(phi, p, self.s[p], self.gamma_o, self.gamma_t);
            sum += Vec3A::from_array(self.ap[p]) * (mp * np);
            pdf += self.ap_pdf[p] * mp * np;
        }
        let mp = mp(
            cos_i,
            self.cos_o[P_MAX],
            sin_i,
            self.sin_o[P_MAX],
            self.v[2],
        );
        let np = 1.0 / (2.0 * PI);
        sum += Vec3A::from_array(self.ap[P_MAX]) * (mp * np);
        pdf += self.ap_pdf[P_MAX] * mp * np;
        (sum, pdf)
    }

    /// A direction drawn from the lobe mixture, pbrt-v3's `Sample_f`: `u`
    /// picks the lobe and `uv` samples `M_p`; the azimuth's own number is
    /// demultiplexed from `u`, as pbrt does, since a closure leaf is handed
    /// three. `None` when every lobe is black.
    pub fn sample(&self, uv: [f32; 2], u: f32) -> Option<Vec3A> {
        if self.is_black() {
            return None;
        }
        let [mut pick, u_phi] = demux(u);
        let mut p = P_MAX;
        for (i, &w) in self.ap_pdf.iter().enumerate() {
            if pick < w {
                p = i;
                break;
            }
            pick -= w;
        }
        // Sample M_p about the lobe's tilted θo (in f64: at small v the
        // logarithm's argument underflows f32).
        let v = f64::from(self.v[p.min(2)]);
        let u0 = f64::from(uv[0].max(1e-5));
        let cos_theta = 1.0 + v * (u0 + (1.0 - u0) * (-2.0 / v).exp()).ln();
        let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
        let cos_phi = (2.0 * std::f64::consts::PI * f64::from(uv[1])).cos();
        let (sin_o, cos_o) = (f64::from(self.sin_o[p]), f64::from(self.cos_o[p]));
        let sin_i = (-cos_theta * sin_o + sin_theta * cos_phi * cos_o).clamp(-1.0, 1.0) as f32;
        let cos_i = safe_sqrt(1.0 - sin_i * sin_i);
        // Sample N_p about the lobe's exit azimuth.
        let dphi = if p < P_MAX {
            phi_p(p, self.gamma_o, self.gamma_t)
                + sample_trimmed_logistic(u_phi, self.s[p], -PI, PI)
        } else {
            2.0 * PI * u_phi
        };
        let phi_i = self.phi_o + dphi;
        let (sin_phi, cos_phi) = phi_i.sin_cos();
        Some(Vec3A::new(sin_i, cos_i * cos_phi, cos_i * sin_phi))
    }

    /// How far a sampled ray's footprint widens: the R lobe's longitudinal
    /// spread (`√v`), which a ray cone reads as its angle — one figure for
    /// the leaf, whichever lobe was sampled.
    pub fn spread(&self) -> f32 {
        self.v[0].sqrt()
    }
}

/// The longitudinal scattering function `M_p` (d'Eon et al. 2011), pbrt-v3's
/// form: the log-space version below `v = 0.1`, where the plain one
/// overflows. Evaluated in f64 — at `v = 0.001` its exponent is a difference
/// of thousands.
fn mp(cos_i: f32, cos_o: f32, sin_i: f32, sin_o: f32, v: f32) -> f32 {
    let (cos_i, cos_o, sin_i, sin_o, v) = (
        f64::from(cos_i),
        f64::from(cos_o),
        f64::from(sin_i),
        f64::from(sin_o),
        f64::from(v),
    );
    let a = cos_i * cos_o / v;
    let b = sin_i * sin_o / v;
    let m = if v <= 0.1 {
        (log_i0(a) - b - 1.0 / v + std::f64::consts::LN_2 + (1.0 / (2.0 * v)).ln()).exp()
    } else {
        ((-b).exp() * i0(a)) / ((1.0 / v).sinh() * 2.0 * v)
    };
    m as f32
}

/// The modified Bessel function of the first kind, order 0, by its series
/// (pbrt-v3's ten terms).
fn i0(x: f64) -> f64 {
    let mut val = 0.0;
    let mut x2i = 1.0;
    let mut ifact = 1.0;
    let mut i4 = 1.0;
    for i in 0..10 {
        if i > 1 {
            ifact *= f64::from(i);
        }
        val += x2i / (i4 * ifact * ifact);
        x2i *= x * x;
        i4 *= 4.0;
    }
    val
}

fn log_i0(x: f64) -> f64 {
    if x > 12.0 {
        x + 0.5 * (-(2.0 * std::f64::consts::PI).ln() + (1.0 / x).ln() + 1.0 / (8.0 * x))
    } else {
        i0(x).ln()
    }
}

/// The azimuth lobe `p` leaves at, relative to ωo: `Φ(p, γo, γt)`.
fn phi_p(p: usize, gamma_o: f32, gamma_t: f32) -> f32 {
    let p = p as f32;
    2.0 * p * gamma_t - 2.0 * gamma_o + p * PI
}

/// The azimuthal scattering function `N_p`: a logistic about `Φ(p)`, trimmed
/// to [−π, π].
fn np(phi: f32, p: usize, s: f32, gamma_o: f32, gamma_t: f32) -> f32 {
    let dphi = phi - phi_p(p, gamma_o, gamma_t);
    if !dphi.is_finite() {
        return 0.0;
    }
    // Into [−π, π].
    let dphi = (dphi + PI).rem_euclid(2.0 * PI) - PI;
    trimmed_logistic(dphi, s, -PI, PI)
}

fn logistic(x: f32, s: f32) -> f32 {
    let e = (-x.abs() / s).exp();
    e / (s * (1.0 + e) * (1.0 + e))
}

fn logistic_cdf(x: f32, s: f32) -> f32 {
    1.0 / (1.0 + (-x / s).exp())
}

fn trimmed_logistic(x: f32, s: f32, a: f32, b: f32) -> f32 {
    logistic(x, s) / (logistic_cdf(b, s) - logistic_cdf(a, s))
}

fn sample_trimmed_logistic(u: f32, s: f32, a: f32, b: f32) -> f32 {
    let k = logistic_cdf(b, s) - logistic_cdf(a, s);
    let x = -s * (1.0 / (u * k + logistic_cdf(a, s)) - 1.0).ln();
    if x.is_finite() { x.clamp(a, b) } else { 0.0 }
}

/// Two numbers in [0, 1) from one, by de-interleaving the bits of its 32-bit
/// fixed-point form — pbrt-v3's `DemuxFloat`.
fn demux(f: f32) -> [f32; 2] {
    let v = (f64::from(f.clamp(0.0, 1.0)) * 4_294_967_296.0).min(4_294_967_295.0) as u64;
    let compact = |x: u64| {
        let mut x = (x as u32) & 0x5555_5555;
        x = (x ^ (x >> 1)) & 0x3333_3333;
        x = (x ^ (x >> 2)) & 0x0f0f_0f0f;
        x = (x ^ (x >> 4)) & 0x00ff_00ff;
        x = (x ^ (x >> 8)) & 0x0000_ffff;
        x as f32 / 65_536.0
    };
    [compact(v), compact(v >> 1)]
}

/// The unpolarised Fresnel reflectance of a dielectric of relative IOR `eta`
/// at incidence cosine `cos_i`.
fn fresnel_dielectric(cos_i: f32, eta: f32) -> f32 {
    let cos_i = cos_i.clamp(0.0, 1.0);
    let sin_t2 = (1.0 - cos_i * cos_i) / (eta * eta);
    if sin_t2 >= 1.0 {
        return 1.0;
    }
    let cos_t = safe_sqrt(1.0 - sin_t2);
    let rs = (cos_i - eta * cos_t) / (cos_i + eta * cos_t);
    let rp = (eta * cos_i - cos_t) / (eta * cos_i + cos_t);
    0.5 * (rs * rs + rp * rp)
}

fn safe_sqrt(x: f32) -> f32 {
    x.max(0.0).sqrt()
}

/// Non-negative and finite, channel by channel.
fn sanitize(c: Vec3A) -> Vec3A {
    Vec3A::select(c.cmpge(Vec3A::ZERO) & c.is_finite_mask(), c, Vec3A::ZERO)
}

#[cfg(test)]
mod tests;
