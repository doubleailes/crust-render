//! OpenPBR Surface — Academy Software Foundation shading model.
//!
//! Full parameter set matches the OpenPBR MaterialX reference
//! (https://academysoftwarefoundation.github.io/OpenPBR/). Every field
//! defaults to the spec value, so RON scenes can specify only what they
//! need.
//!
//! ## Phase status
//!
//! - Phase 1 (done): base_diffuse + base_specular_dielectric + base_metal
//!   + fuzz + emission, sampled with multi-lobe MIS.
//! - Phase 2: coat + thin-film interference.
//! - Phase 3: transmission (thin-walled + rough refraction) + dispersion.
//! - Phase 4: `Ray` medium stack + Beer-Lambert in the tracer.
//! - Phase 5: subsurface (random walk).
//!
//! Parameters for not-yet-wired features (transmission, subsurface, coat,
//! thin_film, opacity) are accepted, deserialised, and preserved so scenes
//! authored today continue to render correctly once later phases land.

use crate::hittable::HitRecord;
use crate::material::Material;
use crate::material::brdf::*;
use crate::ray::Ray;
use glam::Vec3A;
use serde::{Deserialize, Serialize};
use std::f32::consts::PI;
use utils::{Lerp, align_to_normal, random, random_cosine_direction};

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[inline]
fn one_vec() -> Vec3A {
    Vec3A::ONE
}
#[inline]
fn white() -> Vec3A {
    Vec3A::new(0.8, 0.8, 0.8)
}
#[inline]
fn subsurface_radius_scale_default() -> Vec3A {
    Vec3A::new(1.0, 0.5, 0.25)
}
#[inline]
fn f_zero() -> f32 {
    0.0
}
#[inline]
fn f_one() -> f32 {
    1.0
}
#[inline]
fn f_half() -> f32 {
    0.5
}
#[inline]
fn base_default() -> Vec3A {
    Vec3A::new(0.8, 0.8, 0.8)
}
#[inline]
fn specular_roughness_default() -> f32 {
    0.3
}
#[inline]
fn specular_ior_default() -> f32 {
    1.5
}
#[inline]
fn coat_ior_default() -> f32 {
    1.6
}
#[inline]
fn thin_film_thickness_default() -> f32 {
    0.5
}
#[inline]
fn thin_film_ior_default() -> f32 {
    1.4
}
#[inline]
fn abbe_default() -> f32 {
    20.0
}
#[inline]
fn subsurface_radius_default() -> f32 {
    1.0
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OpenPBR {
    // --- base -----------------------------------------------------------
    #[serde(default = "f_one")]
    pub base_weight: f32,
    #[serde(default = "base_default")]
    pub base_color: Vec3A,
    #[serde(default = "f_zero")]
    pub base_diffuse_roughness: f32,
    #[serde(default = "f_zero")]
    pub base_metalness: f32,

    // --- specular -------------------------------------------------------
    #[serde(default = "f_one")]
    pub specular_weight: f32,
    #[serde(default = "one_vec")]
    pub specular_color: Vec3A,
    #[serde(default = "specular_roughness_default")]
    pub specular_roughness: f32,
    #[serde(default = "specular_ior_default")]
    pub specular_ior: f32,
    #[serde(default = "f_zero")]
    pub specular_roughness_anisotropy: f32,

    // --- transmission (phase 3) -----------------------------------------
    #[serde(default = "f_zero")]
    pub transmission_weight: f32,
    #[serde(default = "one_vec")]
    pub transmission_color: Vec3A,
    #[serde(default = "f_zero")]
    pub transmission_depth: f32,
    #[serde(default)]
    pub transmission_scatter: Vec3A,
    #[serde(default = "f_zero")]
    pub transmission_scatter_anisotropy: f32,
    #[serde(default = "f_zero")]
    pub transmission_dispersion_scale: f32,
    #[serde(default = "abbe_default")]
    pub transmission_dispersion_abbe_number: f32,

    // --- subsurface (phase 5) -------------------------------------------
    #[serde(default = "f_zero")]
    pub subsurface_weight: f32,
    #[serde(default = "white")]
    pub subsurface_color: Vec3A,
    #[serde(default = "subsurface_radius_default")]
    pub subsurface_radius: f32,
    #[serde(default = "subsurface_radius_scale_default")]
    pub subsurface_radius_scale: Vec3A,
    #[serde(default = "f_zero")]
    pub subsurface_scatter_anisotropy: f32,

    // --- fuzz -----------------------------------------------------------
    #[serde(default = "f_zero")]
    pub fuzz_weight: f32,
    #[serde(default = "one_vec")]
    pub fuzz_color: Vec3A,
    #[serde(default = "f_half")]
    pub fuzz_roughness: f32,

    // --- coat (phase 2) -------------------------------------------------
    #[serde(default = "f_zero")]
    pub coat_weight: f32,
    #[serde(default = "one_vec")]
    pub coat_color: Vec3A,
    #[serde(default = "f_zero")]
    pub coat_roughness: f32,
    #[serde(default = "f_zero")]
    pub coat_roughness_anisotropy: f32,
    #[serde(default = "coat_ior_default")]
    pub coat_ior: f32,
    #[serde(default = "f_one")]
    pub coat_darkening: f32,

    // --- thin-film (phase 2) --------------------------------------------
    #[serde(default = "f_zero")]
    pub thin_film_weight: f32,
    #[serde(default = "thin_film_thickness_default")]
    pub thin_film_thickness: f32,
    #[serde(default = "thin_film_ior_default")]
    pub thin_film_ior: f32,

    // --- emission -------------------------------------------------------
    #[serde(default = "f_zero")]
    pub emission_luminance: f32,
    #[serde(default = "one_vec")]
    pub emission_color: Vec3A,

    // --- geometry -------------------------------------------------------
    #[serde(default = "f_one")]
    pub geometry_opacity: f32,
    #[serde(default)]
    pub geometry_thin_walled: bool,
}

impl Default for OpenPBR {
    fn default() -> Self {
        Self {
            base_weight: 1.0,
            base_color: base_default(),
            base_diffuse_roughness: 0.0,
            base_metalness: 0.0,
            specular_weight: 1.0,
            specular_color: Vec3A::ONE,
            specular_roughness: 0.3,
            specular_ior: 1.5,
            specular_roughness_anisotropy: 0.0,
            transmission_weight: 0.0,
            transmission_color: Vec3A::ONE,
            transmission_depth: 0.0,
            transmission_scatter: Vec3A::ZERO,
            transmission_scatter_anisotropy: 0.0,
            transmission_dispersion_scale: 0.0,
            transmission_dispersion_abbe_number: 20.0,
            subsurface_weight: 0.0,
            subsurface_color: white(),
            subsurface_radius: 1.0,
            subsurface_radius_scale: subsurface_radius_scale_default(),
            subsurface_scatter_anisotropy: 0.0,
            fuzz_weight: 0.0,
            fuzz_color: Vec3A::ONE,
            fuzz_roughness: 0.5,
            coat_weight: 0.0,
            coat_color: Vec3A::ONE,
            coat_roughness: 0.0,
            coat_roughness_anisotropy: 0.0,
            coat_ior: 1.6,
            coat_darkening: 1.0,
            thin_film_weight: 0.0,
            thin_film_thickness: 0.5,
            thin_film_ior: 1.4,
            emission_luminance: 0.0,
            emission_color: Vec3A::ONE,
            geometry_opacity: 1.0,
            geometry_thin_walled: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal shading state
// ---------------------------------------------------------------------------

/// A local shading frame plus cached view / half / light vectors in world
/// space. Kept small so it can move by value.
struct Frame {
    n: Vec3A,
    t: Vec3A,
    b: Vec3A,
}

impl Frame {
    fn new(n: Vec3A) -> Self {
        let (t, b) = tangent_frame(n);
        Self { n, t, b }
    }
    fn to_local(&self, v: Vec3A) -> Vec3A {
        to_tangent(v, self.t, self.b, self.n)
    }
    fn to_world(&self, v_local: Vec3A) -> Vec3A {
        from_tangent(v_local, self.t, self.b, self.n)
    }
}

// ---------------------------------------------------------------------------
// Discrete lobe PMF for Phase 1
// ---------------------------------------------------------------------------

/// Phase-1 sampling lobes. The specular lobe covers both the metal and
/// dielectric-specular contributions (they share the same GGX distribution
/// and sampling, only Fresnel differs).
#[derive(Clone, Copy)]
enum Lobe {
    Diffuse,
    Specular,
    Fuzz,
}

struct LobePmf {
    p_diffuse: f32,
    p_specular: f32,
    p_fuzz: f32,
}

impl LobePmf {
    /// Fresnel-averaged energy heuristic. Not perfect, but stable and cheap
    /// — the mixture PDF gets the direction right regardless of the exact
    /// per-lobe weights we sample by.
    fn from_params(m: &OpenPBR) -> Self {
        let f0_diel = f0_from_ior(m.specular_ior);
        let base_luma = luma(m.base_color).max(0.02);
        let spec_luma = luma(m.specular_color).max(0.02);
        let fuzz_luma = luma(m.fuzz_color).max(0.02);

        let w_metal = m.base_metalness * base_luma;
        let w_diel_spec = (1.0 - m.base_metalness) * m.specular_weight * spec_luma * f0_diel;
        let w_specular = (w_metal + w_diel_spec).max(1e-4);

        let w_diffuse = ((1.0 - m.base_metalness) * m.base_weight * base_luma * (1.0 - f0_diel))
            .max(1e-4);

        let w_fuzz = (m.fuzz_weight * fuzz_luma).max(1e-6);

        let total = w_diffuse + w_specular + w_fuzz;
        Self {
            p_diffuse: w_diffuse / total,
            p_specular: w_specular / total,
            p_fuzz: w_fuzz / total,
        }
    }

    fn pick(&self, u: f32) -> Lobe {
        if u < self.p_diffuse {
            Lobe::Diffuse
        } else if u < self.p_diffuse + self.p_specular {
            Lobe::Specular
        } else {
            Lobe::Fuzz
        }
    }
}

#[inline]
fn luma(c: Vec3A) -> f32 {
    0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z
}

// ---------------------------------------------------------------------------
// Lobe evaluations. Each returns a *linear-space* BRDF value (no cosine).
// ---------------------------------------------------------------------------

fn eval_diffuse(
    m: &OpenPBR,
    _v_local: Vec3A,
    l_local: Vec3A,
    h_local: Vec3A,
    f_avg_diel: f32,
) -> Vec3A {
    // Disney diffuse re-parameterised for OpenPBR: `base_diffuse_roughness`
    // is the diffuse-only roughness (independent of specular_roughness).
    let n = Vec3A::Z;
    let n_dot_l = l_local.z.max(0.0);
    let n_dot_v = _v_local.z.max(0.0);
    if n_dot_l <= 0.0 || n_dot_v <= 0.0 {
        return Vec3A::ZERO;
    }
    let l_dot_h = l_local.dot(h_local).max(0.0);

    let fd90 = 0.5 + 2.0 * l_dot_h * l_dot_h * m.base_diffuse_roughness;
    let fl = schlick_weight(n_dot_l);
    let fv = schlick_weight(n_dot_v);
    let disney = m.base_color
        * (1.0 / PI)
        * (1.0 + (fd90 - 1.0) * fl)
        * (1.0 + (fd90 - 1.0) * fv);

    // Energy left after specular reflection: `(1 - F_dielectric_avg) *
    // (1 - metalness) * base_weight`. Using the directional Fresnel here
    // would double-count with the specular lobe's own Fresnel — the
    // average avoids that.
    let _ = n;
    disney * (1.0 - f_avg_diel) * (1.0 - m.base_metalness) * m.base_weight
}

fn eval_specular(
    m: &OpenPBR,
    v_local: Vec3A,
    l_local: Vec3A,
    h_local: Vec3A,
    ax: f32,
    ay: f32,
) -> Vec3A {
    let n_dot_v = v_local.z.max(1e-4);
    let n_dot_l = l_local.z.max(1e-4);
    let n_dot_h = h_local.z.max(1e-4);
    let v_dot_h = v_local.dot(h_local).max(1e-4);

    let d = ggx_d_aniso(n_dot_h, h_local.x, h_local.y, ax, ay);
    let g = ggx_g2_smith_aniso(
        n_dot_v, v_local.x, v_local.y, n_dot_l, l_local.x, l_local.y, ax, ay,
    );

    let f0_diel_scalar = f0_from_ior(m.specular_ior);
    let f0_diel = m.specular_color * f0_diel_scalar * m.specular_weight;
    let f_diel = fresnel_schlick(v_dot_h, f0_diel);
    let f_metal = fresnel_schlick(v_dot_h, m.base_color);

    let brdf = d * g / (4.0 * n_dot_v * n_dot_l);
    // Metal path: base_color-tinted Fresnel * brdf * metalness
    // Dielectric-specular path: F_dielectric * brdf * (1 - metalness)
    (f_metal * m.base_metalness + f_diel * (1.0 - m.base_metalness)) * brdf
}

fn eval_fuzz(m: &OpenPBR, v_local: Vec3A, l_local: Vec3A, h_local: Vec3A) -> Vec3A {
    let n_dot_v = v_local.z.max(1e-4);
    let n_dot_l = l_local.z.max(1e-4);
    let n_dot_h = h_local.z.max(0.0);
    m.fuzz_color * m.fuzz_weight * sheen_charlie(n_dot_v, n_dot_l, n_dot_h, m.fuzz_roughness)
}

fn eval_all(m: &OpenPBR, v_local: Vec3A, l_local: Vec3A) -> Vec3A {
    if l_local.z <= 0.0 || v_local.z <= 0.0 {
        return Vec3A::ZERO;
    }
    let h_local = (v_local + l_local).normalize();

    let (ax, ay) = roughness_to_alpha_aniso(m.specular_roughness, m.specular_roughness_anisotropy);
    let f_avg_diel = f0_from_ior(m.specular_ior);

    let diffuse = eval_diffuse(m, v_local, l_local, h_local, f_avg_diel);
    let specular = eval_specular(m, v_local, l_local, h_local, ax, ay);
    let fuzz = eval_fuzz(m, v_local, l_local, h_local);

    // Fuzz sits above base+spec as an outer layer; approximate energy
    // conservation with a directional-albedo-independent scalar attenuation
    // by `(1 - fuzz_weight)`. A Zeltner-style pre-integrated LUT is a
    // Phase 2/6 upgrade.
    let base_atten = (1.0 - m.fuzz_weight).clamp(0.0, 1.0);
    fuzz + base_atten * (diffuse + specular)
}

// ---------------------------------------------------------------------------
// Mixture PDF: p(l) = Σ p_lobe · pdf_lobe(l)
// ---------------------------------------------------------------------------

fn pdf_all(m: &OpenPBR, pmf: &LobePmf, v_local: Vec3A, l_local: Vec3A) -> f32 {
    if l_local.z <= 0.0 || v_local.z <= 0.0 {
        return 0.0;
    }
    let h_local = (v_local + l_local).normalize();

    let (ax, ay) = roughness_to_alpha_aniso(m.specular_roughness, m.specular_roughness_anisotropy);

    let pdf_diffuse = l_local.z.max(0.0) / PI;
    let pdf_specular = pdf_vndf_ggx_aniso_local(v_local, h_local, ax, ay);
    let pdf_fuzz = l_local.z.max(0.0) / PI;

    pmf.p_diffuse * pdf_diffuse + pmf.p_specular * pdf_specular + pmf.p_fuzz * pdf_fuzz
}

// ---------------------------------------------------------------------------
// Material impl
// ---------------------------------------------------------------------------

impl Material for OpenPBR {
    fn scatter(&self, _r_in: &Ray, _rec: &HitRecord, _: &mut Vec3A, _: &mut Ray) -> bool {
        false
    }

    fn scatter_importance(&self, r_in: &Ray, rec: &HitRecord) -> Option<(Ray, Vec3A, f32)> {
        let frame = Frame::new(rec.normal);
        let v_world = -r_in.direction().normalize();
        let v_local = frame.to_local(v_world);
        if v_local.z <= 0.0 {
            return None;
        }

        let pmf = LobePmf::from_params(self);
        let lobe = pmf.pick(random());

        // Sample a direction from the picked lobe.
        let l_local = match lobe {
            Lobe::Diffuse | Lobe::Fuzz => random_cosine_direction(),
            Lobe::Specular => {
                let (ax, ay) = roughness_to_alpha_aniso(
                    self.specular_roughness,
                    self.specular_roughness_anisotropy,
                );
                let h_local = sample_vndf_ggx_aniso_local(v_local, ax, ay);
                // Reflect view around sampled half-vector.
                let l = 2.0 * v_local.dot(h_local) * h_local - v_local;
                if l.z <= 0.0 {
                    return None;
                }
                l
            }
        };

        // Mixture PDF and full-mixture BRDF value.
        let pdf = pdf_all(self, &pmf, v_local, l_local).max(1e-4);
        let brdf = eval_all(self, v_local, l_local);
        let n_dot_l = l_local.z.max(0.0);

        let l_world = frame.to_world(l_local);
        // Convention across this codebase's materials: return brdf * cos as
        // the "throughput" and the tracer multiplies by cos again. Match.
        Some((Ray::new(rec.p, l_world), brdf * n_dot_l, pdf))
    }

    fn emitted(&self) -> Vec3A {
        self.emission_color * self.emission_luminance
    }
}

// A tiny helper kept out of `Frame` for readability at the call sites. Same
// spirit as `utils::align_to_normal` but with an explicit frame.
#[inline]
#[allow(dead_code)]
fn align(local: Vec3A, n: Vec3A) -> Vec3A {
    align_to_normal(local, n)
}

// Re-export the Lerp helper into scope for future coat / thin-film work.
#[allow(dead_code)]
fn _lerp_ping(a: f32, b: f32, t: f32) -> f32 {
    a.lerp(b, t)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f0_from_ior_glass() {
        let f0 = f0_from_ior(1.5);
        assert!((f0 - 0.04).abs() < 1e-3, "f0(1.5) = {f0}");
    }

    #[test]
    fn f0_from_ior_air() {
        assert!(f0_from_ior(1.0).abs() < 1e-6);
    }

    #[test]
    fn iso_matches_aniso_at_zero() {
        let r = 0.4;
        let (ax, ay) = roughness_to_alpha_aniso(r, 0.0);
        assert!((ax - r * r).abs() < 1e-5);
        assert!((ay - r * r).abs() < 1e-5);
    }

    #[test]
    fn sheen_nonneg() {
        for &nv in &[0.1, 0.3, 0.5, 0.9] {
            for &nl in &[0.1, 0.3, 0.5, 0.9] {
                for &nh in &[0.1, 0.5, 0.9] {
                    let v = sheen_charlie(nv, nl, nh, 0.4);
                    assert!(v.is_finite() && v >= 0.0, "sheen({nv},{nl},{nh}) = {v}");
                }
            }
        }
    }

    #[test]
    fn defaults_match_spec() {
        let m = OpenPBR::default();
        assert_eq!(m.base_weight, 1.0);
        assert_eq!(m.specular_ior, 1.5);
        assert_eq!(m.coat_ior, 1.6);
        assert_eq!(m.thin_film_ior, 1.4);
        assert_eq!(m.transmission_dispersion_abbe_number, 20.0);
        assert_eq!(m.subsurface_radius_scale, Vec3A::new(1.0, 0.5, 0.25));
    }

    #[test]
    fn scatter_importance_finite() {
        use crate::hittable::HitRecord;
        let m = OpenPBR {
            base_color: Vec3A::new(0.7, 0.3, 0.2),
            base_metalness: 0.3,
            fuzz_weight: 0.2,
            fuzz_color: Vec3A::new(0.9, 0.9, 0.9),
            ..OpenPBR::default()
        };
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Y;
        let ray = Ray::new(Vec3A::new(0.0, 1.0, 1.0), Vec3A::new(0.0, -1.0, -1.0).normalize());
        // Run many samples: none should be NaN or negative.
        for _ in 0..64 {
            if let Some((_, throughput, pdf)) = m.scatter_importance(&ray, &rec) {
                assert!(pdf.is_finite() && pdf > 0.0, "pdf = {pdf}");
                assert!(throughput.is_finite(), "throughput = {throughput:?}");
                assert!(
                    throughput.x >= 0.0 && throughput.y >= 0.0 && throughput.z >= 0.0,
                    "throughput = {throughput:?}"
                );
            }
        }
    }
}
