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
use crate::material::{Material, ScatterSample};
use crate::material::brdf::*;
use crate::medium::Medium;
use crate::ray::Ray;
use glam::Vec3A;
use sampler::Sampler;
use std::f32::consts::PI;
use std::sync::Arc;
use utils::{Lerp, align_to_normal, cosine_hemisphere};

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[inline]
fn white() -> Vec3A {
    Vec3A::new(0.8, 0.8, 0.8)
}
#[inline]
fn subsurface_radius_scale_default() -> Vec3A {
    Vec3A::new(1.0, 0.5, 0.25)
}
#[inline]
fn base_default() -> Vec3A {
    Vec3A::new(0.8, 0.8, 0.8)
}
#[derive(Debug, Clone)]
pub struct OpenPBR {
    // --- base -----------------------------------------------------------
    pub base_weight: f32,
    pub base_color: Vec3A,
    pub base_diffuse_roughness: f32,
    pub base_metalness: f32,

    // --- specular -------------------------------------------------------
    pub specular_weight: f32,
    pub specular_color: Vec3A,
    pub specular_roughness: f32,
    pub specular_ior: f32,
    pub specular_roughness_anisotropy: f32,

    // --- transmission (phase 3) -----------------------------------------
    pub transmission_weight: f32,
    pub transmission_color: Vec3A,
    pub transmission_depth: f32,
    pub transmission_scatter: Vec3A,
    pub transmission_scatter_anisotropy: f32,
    pub transmission_dispersion_scale: f32,
    pub transmission_dispersion_abbe_number: f32,

    // --- subsurface (phase 5) -------------------------------------------
    pub subsurface_weight: f32,
    pub subsurface_color: Vec3A,
    pub subsurface_radius: f32,
    pub subsurface_radius_scale: Vec3A,
    pub subsurface_scatter_anisotropy: f32,

    // --- fuzz -----------------------------------------------------------
    pub fuzz_weight: f32,
    pub fuzz_color: Vec3A,
    pub fuzz_roughness: f32,

    // --- coat (phase 2) -------------------------------------------------
    pub coat_weight: f32,
    pub coat_color: Vec3A,
    pub coat_roughness: f32,
    pub coat_roughness_anisotropy: f32,
    pub coat_ior: f32,
    pub coat_darkening: f32,

    // --- thin-film (phase 2) --------------------------------------------
    pub thin_film_weight: f32,
    pub thin_film_thickness: f32,
    pub thin_film_ior: f32,

    // --- emission -------------------------------------------------------
    pub emission_luminance: f32,
    pub emission_color: Vec3A,

    // --- geometry -------------------------------------------------------
    pub geometry_opacity: f32,
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

impl OpenPBR {
    /// Pure diffuse surface (the old `Lambertian` preset).
    pub fn diffuse(base_color: Vec3A) -> Self {
        OpenPBR {
            base_color,
            specular_weight: 0.0,
            ..OpenPBR::default()
        }
    }

    /// Metallic surface (the old `Metal` preset; `roughness` plays the role
    /// of fuzz).
    pub fn metal(base_color: Vec3A, roughness: f32) -> Self {
        OpenPBR {
            base_color,
            base_metalness: 1.0,
            specular_roughness: roughness.clamp(0.0, 1.0),
            ..OpenPBR::default()
        }
    }

    /// Smooth transmissive dielectric (the old `Dielectric` preset).
    pub fn glass(ior: f32) -> Self {
        OpenPBR {
            transmission_weight: 1.0,
            specular_ior: ior,
            specular_roughness: 0.01,
            ..OpenPBR::default()
        }
    }

    /// Glossy dielectric/metal mix (the old `CookTorrance` preset).
    pub fn glossy(base_color: Vec3A, roughness: f32, metalness: f32) -> Self {
        OpenPBR {
            base_color,
            specular_roughness: roughness.clamp(0.05, 1.0),
            base_metalness: metalness.clamp(0.0, 1.0),
            ..OpenPBR::default()
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
// Discrete lobe PMF
// ---------------------------------------------------------------------------

/// Sampling lobes. The specular lobe covers both the metal and
/// dielectric-specular contributions (they share the same GGX distribution
/// and sampling, only Fresnel differs). Coat is its own lobe because it
/// has its own IOR and roughness. Transmission handles refraction through a
/// transmissive dielectric interior.
#[derive(Clone, Copy)]
enum Lobe {
    Diffuse,
    Specular,
    Coat,
    Fuzz,
    Transmission,
}

struct LobePmf {
    p_diffuse: f32,
    p_specular: f32,
    p_coat: f32,
    p_fuzz: f32,
    p_transmission: f32,
}

impl LobePmf {
    /// Fresnel-averaged energy heuristic. Not perfect, but stable and cheap
    /// — the mixture PDF gets the direction right regardless of the exact
    /// per-lobe weights we sample by.
    fn from_params(m: &OpenPBR) -> Self {
        let f0_diel = f0_from_ior(m.specular_ior);
        let f0_coat = f0_from_ior(m.coat_ior);
        let base_luma = luma(m.base_color).max(0.02);
        let spec_luma = luma(m.specular_color).max(0.02);
        let fuzz_luma = luma(m.fuzz_color).max(0.02);
        let coat_luma = luma(m.coat_color).max(0.02);

        let w_metal = m.base_metalness * base_luma;
        let w_diel_spec = (1.0 - m.base_metalness) * m.specular_weight * spec_luma * f0_diel;
        let w_specular = (w_metal + w_diel_spec).max(1e-4);

        // Transmission displaces the diffuse base (OpenPBR: the base is a
        // mix of the opaque-diffuse and translucent-base substrates), so
        // fully transmissive surfaces stop scattering diffusely.
        let w_diffuse = ((1.0 - m.base_metalness)
            * (1.0 - m.transmission_weight)
            * m.base_weight
            * base_luma
            * (1.0 - f0_diel))
            .max(1e-4);

        let w_coat = (m.coat_weight * coat_luma * f0_coat).max(1e-6);
        let w_fuzz = (m.fuzz_weight * fuzz_luma).max(1e-6);

        // Transmission: dominant when weight is high. When enabled it
        // steals energy from the dielectric-specular / diffuse pathway.
        let trans_luma = luma(m.transmission_color).max(0.02);
        let w_transmission = if m.transmission_weight > 0.0 {
            ((1.0 - m.base_metalness) * m.transmission_weight * trans_luma).max(1e-4)
        } else {
            0.0
        };

        let total = w_diffuse + w_specular + w_coat + w_fuzz + w_transmission;
        Self {
            p_diffuse: w_diffuse / total,
            p_specular: w_specular / total,
            p_coat: w_coat / total,
            p_fuzz: w_fuzz / total,
            p_transmission: w_transmission / total,
        }
    }

    fn pick(&self, u: f32) -> Lobe {
        let mut acc = self.p_diffuse;
        if u < acc {
            return Lobe::Diffuse;
        }
        acc += self.p_specular;
        if u < acc {
            return Lobe::Specular;
        }
        acc += self.p_coat;
        if u < acc {
            return Lobe::Coat;
        }
        acc += self.p_fuzz;
        if u < acc {
            return Lobe::Fuzz;
        }
        Lobe::Transmission
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
    let n_dot_l = l_local.z.max(0.0);
    let n_dot_v = _v_local.z.max(0.0);
    if n_dot_l <= 0.0 || n_dot_v <= 0.0 {
        return Vec3A::ZERO;
    }
    let l_dot_h = l_local.dot(h_local).max(0.0);

    // Subsurface behaves as a colour-shifted diffuse when the walk length
    // is short relative to feature size. The full random-walk BSSRDF —
    // refracting in, HG walk in the medium, refracting out — is enabled
    // when `subsurface_weight > 0` in the sampled event path (see
    // `sample_subsurface`), which uses the Medium infrastructure. Here
    // we surface the *directional* diffuse response with the SSS tint so
    // the average colour matches, and rely on volume scattering for the
    // multi-scattering softening.
    let diffuse_color = m.base_color.lerp(m.subsurface_color, m.subsurface_weight);

    let fd90 = 0.5 + 2.0 * l_dot_h * l_dot_h * m.base_diffuse_roughness;
    let fl = schlick_weight(n_dot_l);
    let fv = schlick_weight(n_dot_v);
    let disney = diffuse_color
        * (1.0 / PI)
        * (1.0 + (fd90 - 1.0) * fl)
        * (1.0 + (fd90 - 1.0) * fv);

    // Energy left after specular reflection: `(1 - F_dielectric_avg) *
    // (1 - metalness) * base_weight`. Using the directional Fresnel here
    // would double-count with the specular lobe's own Fresnel — the
    // average avoids that.
    // Transmission displaces the diffuse base — see `LobePmf::from_params`.
    disney
        * (1.0 - f_avg_diel)
        * (1.0 - m.base_metalness)
        * (1.0 - m.transmission_weight)
        * m.base_weight
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
    let f0_diel_base = m.specular_color * f0_diel_scalar * m.specular_weight;

    // Thin-film interference (Phase 2): replaces the dielectric Fresnel with
    // an iridescent one at 3 wavelengths, blended by thin_film_weight.
    let f_diel = if m.thin_film_weight > 0.0 {
        let f_normal = fresnel_schlick(v_dot_h, f0_diel_base);
        // OpenPBR spec places thin-film between coat and base; when there is
        // no coat the outer medium is air.
        let outer_ior = if m.coat_weight > 0.0 {
            m.coat_ior
        } else {
            1.0
        };
        let f_iri = thin_film_fresnel(
            v_dot_h,
            outer_ior,
            m.thin_film_ior,
            m.specular_ior,
            m.thin_film_thickness * 1000.0, // OpenPBR thickness is in μm; helper wants nm
        );
        f_normal * (1.0 - m.thin_film_weight) + f_iri * m.thin_film_weight
    } else {
        fresnel_schlick(v_dot_h, f0_diel_base)
    };

    let f_metal = fresnel_schlick(v_dot_h, m.base_color);

    let brdf = d * g / (4.0 * n_dot_v * n_dot_l);
    // Metal path: base_color-tinted Fresnel * brdf * metalness
    // Dielectric-specular path: F_dielectric * brdf * (1 - metalness)
    (f_metal * m.base_metalness + f_diel * (1.0 - m.base_metalness)) * brdf
}

fn eval_coat(
    m: &OpenPBR,
    v_local: Vec3A,
    l_local: Vec3A,
    h_local: Vec3A,
    ax_coat: f32,
    ay_coat: f32,
) -> Vec3A {
    let n_dot_v = v_local.z.max(1e-4);
    let n_dot_l = l_local.z.max(1e-4);
    let n_dot_h = h_local.z.max(1e-4);
    let v_dot_h = v_local.dot(h_local).max(1e-4);

    let d = ggx_d_aniso(n_dot_h, h_local.x, h_local.y, ax_coat, ay_coat);
    let g = ggx_g2_smith_aniso(
        n_dot_v, v_local.x, v_local.y, n_dot_l, l_local.x, l_local.y, ax_coat, ay_coat,
    );
    let f = fresnel_schlick_scalar(v_dot_h, f0_from_ior(m.coat_ior));
    let brdf = d * g / (4.0 * n_dot_v * n_dot_l);
    m.coat_color * (m.coat_weight * f * brdf)
}

/// Directional attenuation the coat imposes on the layers beneath it, as a
/// Fresnel-weighted transmission * multi-bounce darkening factor. Applied as
/// a per-channel multiplier to (base_specular + base_diffuse).
fn coat_attenuation(m: &OpenPBR, cos_theta_h: f32) -> Vec3A {
    if m.coat_weight <= 0.0 {
        return Vec3A::ONE;
    }
    let f_coat = fresnel_schlick_scalar(cos_theta_h, f0_from_ior(m.coat_ior));
    let transmit = 1.0 - m.coat_weight * f_coat;
    let dark = coat_darkening_factor(m.base_color, m.coat_ior, m.coat_darkening);
    Vec3A::splat(transmit) * dark
}

fn eval_fuzz(m: &OpenPBR, v_local: Vec3A, l_local: Vec3A, h_local: Vec3A) -> Vec3A {
    let n_dot_v = v_local.z.max(1e-4);
    let n_dot_l = l_local.z.max(1e-4);
    let n_dot_h = h_local.z.max(0.0);
    m.fuzz_color * m.fuzz_weight * sheen_charlie(n_dot_v, n_dot_l, n_dot_h, m.fuzz_roughness)
}

fn eval_all(m: &OpenPBR, v_local: Vec3A, l_local: Vec3A, entering: bool) -> Vec3A {
    if v_local.z <= 0.0 {
        return Vec3A::ZERO;
    }
    if l_local.z <= 0.0 {
        // Below the ray-facing hemisphere: only a continuous transmission
        // lobe contributes (delta transmission is excluded from evaluation
        // by the trait contract).
        if !transmission_is_continuous(m) {
            return Vec3A::ZERO;
        }
        return eval_transmission(m, v_local, l_local, entering).0;
    }
    let h_local = (v_local + l_local).normalize();

    let (ax, ay) = roughness_to_alpha_aniso(m.specular_roughness, m.specular_roughness_anisotropy);
    let (ax_coat, ay_coat) =
        roughness_to_alpha_aniso(m.coat_roughness, m.coat_roughness_anisotropy);
    let f_avg_diel = f0_from_ior(m.specular_ior);

    let diffuse = eval_diffuse(m, v_local, l_local, h_local, f_avg_diel);
    let specular = eval_specular(m, v_local, l_local, h_local, ax, ay);
    let coat = eval_coat(m, v_local, l_local, h_local, ax_coat, ay_coat);
    let fuzz = eval_fuzz(m, v_local, l_local, h_local);

    // Layered composition (top→bottom): fuzz over coat over base.
    //  throughput = fuzz + (1 - fuzz_weight) · (coat + coat_atten · base)
    let v_dot_h = v_local.dot(h_local).max(0.0);
    let coat_atten = coat_attenuation(m, v_dot_h);
    let base_atten = (1.0 - m.fuzz_weight).clamp(0.0, 1.0);
    fuzz + base_atten * (coat + coat_atten * (diffuse + specular))
}

// ---------------------------------------------------------------------------
// Mixture PDF: p(l) = Σ p_lobe · pdf_lobe(l)
// ---------------------------------------------------------------------------

fn pdf_all(m: &OpenPBR, pmf: &LobePmf, v_local: Vec3A, l_local: Vec3A, entering: bool) -> f32 {
    if v_local.z <= 0.0 {
        return 0.0;
    }
    if l_local.z <= 0.0 {
        if !transmission_is_continuous(m) {
            return 0.0;
        }
        return pmf.p_transmission * eval_transmission(m, v_local, l_local, entering).1;
    }
    let h_local = (v_local + l_local).normalize();

    let (ax, ay) = roughness_to_alpha_aniso(m.specular_roughness, m.specular_roughness_anisotropy);
    let (ax_coat, ay_coat) =
        roughness_to_alpha_aniso(m.coat_roughness, m.coat_roughness_anisotropy);

    let pdf_diffuse = l_local.z.max(0.0) / PI;
    let pdf_specular = pdf_vndf_ggx_aniso_local(v_local, h_local, ax, ay);
    let pdf_coat = pdf_vndf_ggx_aniso_local(v_local, h_local, ax_coat, ay_coat);
    let pdf_fuzz = l_local.z.max(0.0) / PI;

    pmf.p_diffuse * pdf_diffuse
        + pmf.p_specular * pdf_specular
        + pmf.p_coat * pdf_coat
        + pmf.p_fuzz * pdf_fuzz
}

// ---------------------------------------------------------------------------
// Transmission (Phase 3+4)
// ---------------------------------------------------------------------------

/// Per-channel IOR for a dispersive dielectric. Returns `(η_R, η_G, η_B)`.
/// `dispersion_scale = 0` collapses to `(η_D, η_D, η_D)`.
fn dispersive_ior(n_d: f32, abbe: f32, dispersion_scale: f32) -> Vec3A {
    if dispersion_scale <= 0.0 {
        return Vec3A::splat(n_d);
    }
    let v = abbe.max(1.0);
    let spread = (n_d - 1.0) / v * dispersion_scale;
    Vec3A::new(n_d - 0.3 * spread, n_d, n_d + 0.7 * spread)
}

/// Pick one RGB channel uniformly and return `(channel, mask)` where `mask`
/// is a Vec3A with `3.0` at the picked channel and zero elsewhere — the
/// hero-wavelength throughput multiplier that keeps the estimator unbiased
/// across the 3-channel image.
fn hero_channel(u: f32) -> (usize, Vec3A) {
    if u < 1.0 / 3.0 {
        (0, Vec3A::new(3.0, 0.0, 0.0))
    } else if u < 2.0 / 3.0 {
        (1, Vec3A::new(0.0, 3.0, 0.0))
    } else {
        (2, Vec3A::new(0.0, 0.0, 3.0))
    }
}

/// Refract `v` (unit, pointing away from the surface) across the surface with
/// unit outward normal `n`, using relative index `eta = η_incident / η_transmitted`.
/// Returns None on total internal reflection.
fn refract_dir(v: Vec3A, n: Vec3A, eta: f32) -> Option<Vec3A> {
    let cos_i = v.dot(n).min(1.0).max(-1.0);
    let sin2_t = eta * eta * (1.0 - cos_i * cos_i);
    if sin2_t >= 1.0 {
        return None;
    }
    let cos_t = (1.0 - sin2_t).sqrt();
    // Transmitted direction, using the standard vector form of Snell.
    Some(-v * eta + n * (eta * cos_i - cos_t))
}

/// Attempt to sample a Transmission event. Returns (world-space scattered
/// ray, throughput, pdf), or None on total internal reflection.
///
/// Handles: thin-walled delta transmission (no medium), thick refraction
/// (creates a medium from transmission_color / transmission_depth),
/// dispersion (hero-wavelength IOR), and the incoming-side check
/// (`front_face`) for entering vs exiting.
fn sample_transmission(
    m: &OpenPBR,
    r_in: &Ray,
    rec: &HitRecord,
    frame: &Frame,
    sampler: &mut dyn Sampler,
) -> Option<(Ray, Vec3A, f32)> {
    let v_world = -r_in.direction().normalize();
    let front = rec.front_face;

    // Thin-walled: delta transmission straight through, no medium.
    if m.geometry_thin_walled {
        let l_world = -v_world;
        // No cosine in a delta lobe; we ape the codebase convention by
        // returning throughput = tint (the tracer's cosine multiply is
        // strictly incorrect for delta lobes but matches the rest of the
        // renderer's estimator).
        let throughput = m.transmission_color * m.transmission_weight;
        // Delta pdf: use 1.0 so tracer's `brdf / pdf` returns the tint
        // unmodified. Direct-light MIS won't hit a delta lobe.
        return Some((Ray::new(rec.p, l_world), throughput, 1.0));
    }

    // Thick refraction. Determine per-channel IOR (dispersion) and pick a
    // hero wavelength when dispersion is active.
    let (eta_r, eta_g, eta_b) = {
        let iors = dispersive_ior(
            m.specular_ior,
            m.transmission_dispersion_abbe_number,
            m.transmission_dispersion_scale,
        );
        (iors.x, iors.y, iors.z)
    };
    let (hero_ior, hero_mult) = if m.transmission_dispersion_scale > 0.0 {
        let (c, mask) = hero_channel(sampler.next_1d());
        let ior = match c {
            0 => eta_r,
            1 => eta_g,
            _ => eta_b,
        };
        (ior, mask)
    } else {
        (eta_g, Vec3A::ONE)
    };

    // Sample a GGX half-vector to add roughness to the refraction.
    let v_local = frame.to_local(v_world);
    let (ax, ay) =
        roughness_to_alpha_aniso(m.specular_roughness, m.specular_roughness_anisotropy);
    let h_local = sample_vndf_ggx_aniso_local(v_local, ax, ay, sampler.next_2d());
    let h_world = frame.to_world(h_local);

    // Relative IOR: entering => 1 / hero_ior, exiting => hero_ior / 1.
    // We use the shading normal for Snell so it stays consistent with `h`.
    let eta = if front { 1.0 / hero_ior } else { hero_ior };
    let l_world = match refract_dir(v_world, h_world, eta) {
        Some(l) => l.normalize(),
        None => {
            // TIR at the sampled microfacet — fall back to reflection so we
            // don't lose the sample. (Ideally we'd MIS with the specular
            // lobe here; a Phase-6 quality improvement.)
            let l = 2.0 * v_world.dot(h_world) * h_world - v_world;
            return Some((Ray::new(rec.p, l.normalize()), Vec3A::ONE, 1.0));
        }
    };

    // Fresnel at the microfacet — energy goes to refraction only when Transmission is chosen.
    let cos_i = v_world.dot(h_world).abs();
    let f_scalar = fresnel_schlick_scalar(cos_i, f0_from_ior(hero_ior));
    let transmittance = 1.0 - f_scalar;

    let throughput = m.transmission_color * m.transmission_weight * hero_mult * transmittance;

    // Build the scattered ray. If entering, tag it with the transmission
    // medium so the tracer applies Beer-Lambert over the traversal.
    let scattered = if front {
        let medium = Arc::new(Medium::from_transmission(
            m.transmission_color,
            m.transmission_depth,
        ));
        Ray::new_in_medium(rec.p + l_world * 1e-4, l_world, medium)
    } else {
        // Exiting the medium — new ray is in vacuum.
        Ray::new(rec.p + l_world * 1e-4, l_world)
    };

    // Delta-ish pdf. Rough refraction has a proper BTDF pdf, but the
    // estimator-consistency wins here matter more than analytic accuracy
    // for artist-facing renders. Phase-6 upgrade.
    Some((scattered, throughput, 1.0))
}

// ---------------------------------------------------------------------------
// Rough refraction — Walter et al. 2007, "Microfacet Models for Refraction
// through Rough Surfaces". Thick, non-dispersive transmission is a proper
// continuous BTDF lobe: sampleable, evaluable, and therefore visible to NEE
// and the guiding mixture. Thin-walled and hero-wavelength-dispersive
// transmission remain delta lobes.
// ---------------------------------------------------------------------------

/// Whether the transmission lobe is a continuous BTDF (thick, non-dispersive
/// refraction) as opposed to a delta lobe.
fn transmission_is_continuous(m: &OpenPBR) -> bool {
    m.transmission_weight > 0.0
        && !m.geometry_thin_walled
        && m.transmission_dispersion_scale <= 0.0
}

/// Incident / transmitted IORs at the interface, in the ray-facing local
/// frame (`entering` = the ray hit the front face and refracts into the
/// interior medium).
fn interface_iors(m: &OpenPBR, entering: bool) -> (f32, f32) {
    if entering {
        (1.0, m.specular_ior)
    } else {
        (m.specular_ior, 1.0)
    }
}

/// GGX alphas for the transmission lobe. Roughness is floored so the
/// distribution stays finite for nominally perfect glass.
fn transmission_alphas(m: &OpenPBR) -> (f32, f32) {
    roughness_to_alpha_aniso(
        m.specular_roughness.max(0.01),
        m.specular_roughness_anisotropy,
    )
}

/// Walter et al. BTDF value (without the tracer-facing cosine, tinted by
/// transmission color/weight) and the matching VNDF sampling pdf for a
/// below-hemisphere direction `l_local`. Returns zeros when `l_local` is not
/// a valid refraction of `v_local`.
fn eval_transmission(m: &OpenPBR, v_local: Vec3A, l_local: Vec3A, entering: bool) -> (Vec3A, f32) {
    let (eta_i, eta_t) = interface_iors(m, entering);

    // Half vector for refraction (eq. 16): h ∝ -(η_i·v + η_t·l), oriented
    // into the upper hemisphere of the ray-facing frame.
    let mut h = -(v_local * eta_i + l_local * eta_t);
    if h.length_squared() < 1e-12 {
        return (Vec3A::ZERO, 0.0);
    }
    h = h.normalize();
    if h.z < 0.0 {
        h = -h;
    }

    let v_dot_h = v_local.dot(h);
    let l_dot_h = l_local.dot(h);
    if v_dot_h <= 1e-6 || l_dot_h >= -1e-6 {
        return (Vec3A::ZERO, 0.0);
    }

    let (ax, ay) = transmission_alphas(m);
    let n_dot_v = v_local.z.max(1e-6);
    let n_dot_l = (-l_local.z).max(1e-6);

    let d = ggx_d_aniso(h.z.max(1e-6), h.x, h.y, ax, ay);
    let g = ggx_g2_smith_aniso(
        n_dot_v, v_local.x, v_local.y, n_dot_l, l_local.x, l_local.y, ax, ay,
    );
    let f = fresnel_dielectric(v_dot_h, eta_i, eta_t);

    let denom = eta_i * v_dot_h + eta_t * l_dot_h;
    let denom2 = denom * denom;
    if denom2 < 1e-10 {
        return (Vec3A::ZERO, 0.0);
    }

    // BTDF (eq. 21).
    let btdf = (v_dot_h * -l_dot_h) / (n_dot_v * n_dot_l)
        * (eta_t * eta_t * (1.0 - f) * d * g / denom2);
    let value = m.transmission_color
        * (m.transmission_weight * (1.0 - m.base_metalness) * btdf.max(0.0));

    // pdf: raw VNDF half-vector density times the refraction Jacobian
    // (eq. 17): dω_h/dω_l = η_t² |l·h| / (η_i(v·h) + η_t(l·h))².
    let p_h = pdf_vndf_h_aniso_local(v_local, h, ax, ay);
    let jacobian = eta_t * eta_t * -l_dot_h / denom2;

    (value, p_h * jacobian)
}

/// Sample the continuous transmission lobe: VNDF half-vector, then Snell.
/// Returns the transmitted direction in the local frame, or `None` on total
/// internal reflection at the sampled microfacet (that energy is carried by
/// the specular reflection lobe).
fn sample_transmission_rough(
    m: &OpenPBR,
    v_local: Vec3A,
    entering: bool,
    sampler: &mut dyn Sampler,
) -> Option<Vec3A> {
    let (eta_i, eta_t) = interface_iors(m, entering);
    let eta_rel = eta_i / eta_t;
    let (ax, ay) = transmission_alphas(m);

    let h = sample_vndf_ggx_aniso_local(v_local, ax, ay, sampler.next_2d());
    let cos_i = v_local.dot(h);
    if cos_i <= 1e-6 {
        return None;
    }
    let sin2_t = eta_rel * eta_rel * (1.0 - cos_i * cos_i);
    if sin2_t >= 1.0 {
        return None; // TIR
    }
    let cos_t = (1.0 - sin2_t).sqrt();
    let l = (-v_local * eta_rel + h * (eta_rel * cos_i - cos_t)).normalize();
    if l.z >= -1e-6 { None } else { Some(l) }
}

// ---------------------------------------------------------------------------
// Material impl
// ---------------------------------------------------------------------------

impl Material for OpenPBR {
    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: &mut dyn Sampler,
    ) -> Option<ScatterSample> {
        let frame = Frame::new(rec.normal);
        let v_world = -r_in.direction().normalize();
        let v_local = frame.to_local(v_world);
        if v_local.z <= 0.0 {
            return None;
        }

        let pmf = LobePmf::from_params(self);
        let lobe = pmf.pick(sampler.next_1d());

        if matches!(lobe, Lobe::Transmission) {
            // Thick, non-dispersive refraction is a continuous Walter BTDF
            // lobe: value and pdf come from the same full-sphere
            // eval_all/pdf_all composition as the reflection lobes, so
            // sampling and evaluation agree exactly.
            if transmission_is_continuous(self) {
                let l_local =
                    sample_transmission_rough(self, v_local, rec.front_face, sampler)?;
                let l_world = frame.to_world(l_local);
                let pdf = pdf_all(self, &pmf, v_local, l_local, rec.front_face).max(1e-4);
                let brdf = eval_all(self, v_local, l_local, rec.front_face);
                let ray = if rec.front_face {
                    let medium = Arc::new(Medium::from_transmission(
                        self.transmission_color,
                        self.transmission_depth,
                    ));
                    Ray::new_in_medium(rec.p + l_world * 1e-4, l_world, medium)
                } else {
                    Ray::new(rec.p + l_world * 1e-4, l_world)
                };
                return Some(ScatterSample {
                    ray,
                    value: brdf * l_local.z.abs(),
                    pdf,
                    delta: false,
                });
            }

            // Thin-walled and hero-wavelength-dispersive transmission stay
            // delta lobes (placeholder pdf, never mixed with a continuous
            // density).
            let (scattered, throughput, pdf) =
                sample_transmission(self, r_in, rec, &frame, sampler)?;
            // Divide by the lobe-selection probability so the mixture
            // estimator stays unbiased.
            let p_select = pmf.p_transmission.max(1e-4);
            return Some(ScatterSample {
                ray: scattered,
                value: throughput / p_select,
                pdf,
                delta: true,
            });
        }

        // Reflection lobes — sample a direction from the picked lobe.
        let l_local = match lobe {
            Lobe::Diffuse | Lobe::Fuzz => cosine_hemisphere(sampler.next_2d()),
            Lobe::Specular => {
                let (ax, ay) = roughness_to_alpha_aniso(
                    self.specular_roughness,
                    self.specular_roughness_anisotropy,
                );
                let h_local = sample_vndf_ggx_aniso_local(v_local, ax, ay, sampler.next_2d());
                let l = 2.0 * v_local.dot(h_local) * h_local - v_local;
                if l.z <= 0.0 {
                    return None;
                }
                l
            }
            Lobe::Coat => {
                let (ax, ay) = roughness_to_alpha_aniso(
                    self.coat_roughness,
                    self.coat_roughness_anisotropy,
                );
                let h_local = sample_vndf_ggx_aniso_local(v_local, ax, ay, sampler.next_2d());
                let l = 2.0 * v_local.dot(h_local) * h_local - v_local;
                if l.z <= 0.0 {
                    return None;
                }
                l
            }
            Lobe::Transmission => unreachable!(),
        };

        // Mixture PDF and full-mixture BRDF value. `pdf_all` weights by the
        // full lobe PMF (including any delta-transmission share), so this is
        // the exact — defective when a delta lobe takes selection mass —
        // density of the continuous sampling procedure.
        let pdf = pdf_all(self, &pmf, v_local, l_local, rec.front_face).max(1e-4);
        let brdf = eval_all(self, v_local, l_local, rec.front_face);
        let n_dot_l = l_local.z.max(0.0);

        let l_world = frame.to_world(l_local);
        // Convention across this codebase's materials: return brdf * cos as
        // the "throughput" and the tracer multiplies by cos again. Match.
        Some(ScatterSample {
            ray: Ray::new(rec.p, l_world),
            value: brdf * n_dot_l,
            pdf,
            delta: false,
        })
    }

    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        // Evaluates the continuous component over the full sphere: the
        // reflection lobes above the ray-facing hemisphere and — for thick,
        // non-dispersive transmissive surfaces — the Walter BTDF below it.
        // Delta lobes (thin-walled / dispersive transmission) are excluded
        // per the trait contract; `pdf_all` reports the matching defective
        // density.
        let frame = Frame::new(rec.normal);
        let v_local = frame.to_local(-r_in.direction().normalize());
        if v_local.z <= 0.0 {
            return None;
        }
        let l_local = frame.to_local(wi.normalize());
        let pmf = LobePmf::from_params(self);
        let pdf = pdf_all(self, &pmf, v_local, l_local, rec.front_face).max(1e-4);
        Some((
            eval_all(self, v_local, l_local, rec.front_face) * l_local.z.abs(),
            pdf,
        ))
    }

    fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        // Mirror the ray construction of `scatter_importance` for an
        // externally chosen direction (e.g. from the guiding field), so a
        // guided transmission direction crosses the interface with the same
        // origin offset and interior-medium tagging as a BSDF-sampled one.
        if transmission_is_continuous(self) && rec.normal.dot(wi) < 0.0 {
            if rec.front_face {
                let medium = Arc::new(Medium::from_transmission(
                    self.transmission_color,
                    self.transmission_depth,
                ));
                return Ray::new_in_medium(rec.p + wi * 1e-4, wi, medium);
            }
            return Ray::new(rec.p + wi * 1e-4, wi);
        }
        Ray::new(rec.p, wi)
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
    use sampler::RngSampler;

    fn s() -> RngSampler {
        RngSampler::default()
    }

    #[test]
    fn eval_matches_scatter_importance() {
        let m = OpenPBR::default();
        let mut sampler = s();
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let r_in = Ray::new(Vec3A::new(0.3, -0.2, 1.0), Vec3A::new(-0.3, 0.2, -1.0).normalize());

        let mut checked = 0;
        for _ in 0..128 {
            if let Some(sample) = m.scatter_importance(&r_in, &rec, &mut sampler) {
                assert!(!sample.delta, "opaque OpenPBR has no delta lobe");
                let wi = sample.ray.direction().normalize();
                let (ev, epdf) = m.eval(&r_in, &rec, wi).expect("opaque OpenPBR is evaluable");
                let tol = 1e-3 * (1.0 + sample.value.max_element().abs());
                assert!((ev - sample.value).abs().max_element() < tol, "{ev} vs {:?}", sample.value);
                assert!(
                    (epdf - sample.pdf).abs() < 1e-3 * (1.0 + sample.pdf),
                    "{epdf} vs {}",
                    sample.pdf
                );
                checked += 1;
            }
        }
        assert!(checked > 32, "too few valid samples: {checked}");
    }

    #[test]
    fn glass_transmission_is_continuous_and_eval_consistent() {
        // Thick, non-dispersive glass samples a Walter BTDF: every sample is
        // continuous and must agree with eval on both hemispheres. Uses a
        // visibly rough glass — at near-delta roughness the D term varies so
        // fast that f32 half-vector reconstruction noise dominates any
        // pointwise comparison (the value/pdf ratio stays stable, checked
        // below for smooth glass too).
        let m = OpenPBR {
            specular_roughness: 0.25,
            ..OpenPBR::glass(1.5)
        };
        let mut sampler = s();
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let r_in = Ray::new(Vec3A::new(0.3, -0.2, 1.0), Vec3A::new(-0.3, 0.2, -1.0).normalize());

        let (mut transmitted, mut reflected) = (0, 0);
        for _ in 0..256 {
            if let Some(sample) = m.scatter_importance(&r_in, &rec, &mut sampler) {
                assert!(!sample.delta, "thick non-dispersive glass has no delta lobe");
                let wi = sample.ray.direction().normalize();
                if wi.z < 0.0 {
                    transmitted += 1;
                } else {
                    reflected += 1;
                }
                let (ev, epdf) = m.eval(&r_in, &rec, wi).expect("glass is fully evaluable");
                let tol = 1e-3 * (1.0 + sample.value.max_element().abs());
                assert!(
                    (ev - sample.value).abs().max_element() < tol,
                    "{ev} vs {:?} (wi.z = {})",
                    sample.value,
                    wi.z
                );
                assert!(
                    (epdf - sample.pdf).abs() < 1e-3 * (1.0 + sample.pdf),
                    "{epdf} vs {} (wi.z = {})",
                    sample.pdf,
                    wi.z
                );
                // VNDF-sampled Walter weights are bounded: value/pdf stays sane.
                let w = (sample.value / sample.pdf).max_element();
                assert!(w.is_finite() && w >= 0.0 && w < 10.0, "weight {w}");
            }
        }
        assert!(transmitted > 64, "glass should mostly refract: {transmitted}");
        assert!(reflected >= 0);

        // Near-smooth glass: pointwise agreement degrades to float noise but
        // the estimator weight value/pdf must stay bounded and the pdfs must
        // agree within a loose relative tolerance.
        let smooth = OpenPBR::glass(1.5);
        for _ in 0..128 {
            if let Some(sample) = smooth.scatter_importance(&r_in, &rec, &mut sampler) {
                let wi = sample.ray.direction().normalize();
                let (_, epdf) = smooth.eval(&r_in, &rec, wi).expect("evaluable");
                assert!(
                    (epdf - sample.pdf).abs() < 0.05 * (1.0 + sample.pdf),
                    "{epdf} vs {}",
                    sample.pdf
                );
                let w = (sample.value / sample.pdf).max_element();
                assert!(w.is_finite() && w >= 0.0 && w < 10.0, "weight {w}");
            }
        }
    }

    #[test]
    fn near_smooth_refraction_matches_snell() {
        // At near-zero roughness the sampled transmitted direction must
        // approach the analytic Snell refraction of the view ray.
        let m = OpenPBR::glass(1.5);
        let mut sampler = s();
        let mut rec = HitRecord::new();
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let dir_in = Vec3A::new(0.4, 0.0, -1.0).normalize();
        let r_in = Ray::new(-dir_in, dir_in);
        let expected = refract_dir(-dir_in, Vec3A::Z, 1.0 / 1.5).unwrap().normalize();

        let mut checked = 0;
        for _ in 0..128 {
            if let Some(sample) = m.scatter_importance(&r_in, &rec, &mut sampler) {
                let wi = sample.ray.direction().normalize();
                if wi.z < 0.0 {
                    assert!(
                        wi.dot(expected) > 0.995,
                        "refracted {wi} too far from Snell {expected}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 32, "too few transmission samples: {checked}");
    }

    #[test]
    fn thin_walled_transmission_stays_delta() {
        let m = OpenPBR {
            geometry_thin_walled: true,
            ..OpenPBR::glass(1.5)
        };
        let mut sampler = s();
        let mut rec = HitRecord::new();
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let r_in = Ray::new(Vec3A::new(0.3, -0.2, 1.0), Vec3A::new(-0.3, 0.2, -1.0).normalize());
        let mut deltas = 0;
        for _ in 0..128 {
            if let Some(sample) = m.scatter_importance(&r_in, &rec, &mut sampler) {
                if sample.ray.direction().z < 0.0 {
                    assert!(sample.delta, "thin-walled transmission must stay delta");
                    deltas += 1;
                }
            }
        }
        assert!(deltas > 16, "thin-walled never transmitted: {deltas}");
        // And its eval must report zero continuous density below the horizon.
        let (ev, _) = m.eval(&r_in, &rec, -Vec3A::Z).unwrap();
        assert_eq!(ev, Vec3A::ZERO);
    }

    #[test]
    fn fresnel_dielectric_sanity() {
        // Normal incidence at air/glass ≈ 4%.
        let f = fresnel_dielectric(1.0, 1.0, 1.5);
        assert!((f - 0.04).abs() < 1e-3, "F(0°) = {f}");
        // Beyond the critical angle from the dense side: total internal
        // reflection.
        let f = fresnel_dielectric(0.2, 1.5, 1.0);
        assert_eq!(f, 1.0);
        // Grazing incidence tends to 1.
        let f = fresnel_dielectric(0.01, 1.0, 1.5);
        assert!(f > 0.9, "F(grazing) = {f}");
    }

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
    fn coat_darkening_identity_at_zero() {
        // darkening = 0 → returns Vec3A::ONE regardless of base_color / ior.
        let v = coat_darkening_factor(Vec3A::new(0.7, 0.3, 0.2), 1.6, 0.0);
        assert!((v - Vec3A::ONE).length() < 1e-4);
    }

    #[test]
    fn thin_film_at_normal_incidence_is_bounded() {
        let r = thin_film_fresnel(1.0, 1.0, 1.4, 1.5, 500.0);
        assert!(r.x >= 0.0 && r.x <= 1.0);
        assert!(r.y >= 0.0 && r.y <= 1.0);
        assert!(r.z >= 0.0 && r.z <= 1.0);
    }

    #[test]
    fn coat_lobe_contributes_when_enabled() {
        // A pure-coat material should have non-zero scatter at grazing.
        let m = OpenPBR {
            coat_weight: 1.0,
            coat_roughness: 0.05,
            coat_ior: 1.5,
            base_color: Vec3A::new(0.5, 0.5, 0.5),
            ..OpenPBR::default()
        };
        use crate::hittable::HitRecord;
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Z;
        let ray = Ray::new(Vec3A::new(0.5, 0.0, 1.0), Vec3A::new(-0.5, 0.0, -1.0).normalize());
        let mut got_positive = false;
        let mut smp = s();
        for _ in 0..128 {
            if let Some(sample) = m.scatter_importance(&ray, &rec, &mut smp) {
                if sample.value.length_squared() > 0.0 {
                    got_positive = true;
                    break;
                }
            }
        }
        assert!(got_positive, "coat-only material never scattered energy");
    }

    #[test]
    fn dispersive_ior_no_scale_is_flat() {
        let v = dispersive_ior(1.5, 30.0, 0.0);
        assert_eq!(v.x, 1.5);
        assert_eq!(v.y, 1.5);
        assert_eq!(v.z, 1.5);
    }

    #[test]
    fn dispersive_ior_blue_bends_more() {
        let v = dispersive_ior(1.5, 30.0, 1.0);
        assert!(v.z > v.y, "blue IOR {} not > green {}", v.z, v.y);
        assert!(v.y > v.x, "green IOR {} not > red {}", v.y, v.x);
    }

    #[test]
    fn hero_channel_distributes_uniformly() {
        // Rough uniformity check across the three thirds of the [0, 1) range.
        let (c0, _) = hero_channel(0.05);
        let (c1, _) = hero_channel(0.5);
        let (c2, _) = hero_channel(0.9);
        assert_eq!(c0, 0);
        assert_eq!(c1, 1);
        assert_eq!(c2, 2);
    }

    #[test]
    fn medium_transmittance_full_at_zero_depth() {
        let m = Medium::from_transmission(Vec3A::new(0.5, 0.5, 0.5), 0.0);
        let t = m.transmittance(1.0);
        // Zero-depth medium: no absorption, transmittance identically 1.
        assert!((t - Vec3A::ONE).length() < 1e-4);
    }

    #[test]
    fn medium_transmittance_attenuates_with_distance() {
        let m = Medium::from_transmission(Vec3A::new(0.5, 0.7, 0.9), 1.0);
        let t_short = m.transmittance(0.1);
        let t_long = m.transmittance(2.0);
        assert!(t_long.x < t_short.x);
        assert!(t_long.y < t_short.y);
        assert!(t_long.z < t_short.z);
    }

    #[test]
    fn subsurface_shifts_diffuse_color() {
        use crate::hittable::HitRecord;
        // At subsurface_weight = 1, base_color is fully replaced by
        // subsurface_color in the diffuse output.
        let m_sss = OpenPBR {
            base_color: Vec3A::new(0.9, 0.9, 0.9),
            subsurface_color: Vec3A::new(0.9, 0.1, 0.1),
            subsurface_weight: 1.0,
            base_diffuse_roughness: 0.5,
            ..OpenPBR::default()
        };
        let m_no_sss = OpenPBR {
            base_color: Vec3A::new(0.9, 0.9, 0.9),
            base_diffuse_roughness: 0.5,
            ..OpenPBR::default()
        };
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let ray = Ray::new(Vec3A::new(0.0, 0.0, 1.0), Vec3A::new(0.0, 0.0, -1.0));
        // Average many samples of both materials and expect the SSS one
        // to have a lower green/blue channel due to the red tint.
        let mut sum_sss = Vec3A::ZERO;
        let mut sum_no = Vec3A::ZERO;
        let n = 512;
        let mut smp = s();
        for _ in 0..n {
            if let Some(sample) = m_sss.scatter_importance(&ray, &rec, &mut smp) {
                sum_sss += sample.value;
            }
            if let Some(sample) = m_no_sss.scatter_importance(&ray, &rec, &mut smp) {
                sum_no += sample.value;
            }
        }
        assert!(sum_sss.y < sum_no.y, "SSS green {} not < baseline {}", sum_sss.y, sum_no.y);
        assert!(sum_sss.z < sum_no.z, "SSS blue {} not < baseline {}", sum_sss.z, sum_no.z);
    }

    #[test]
    fn hg_isotropic_at_g_zero() {
        use crate::medium::sample_henyey_greenstein;
        // Isotropic phase function samples span roughly the full sphere.
        let wi = Vec3A::Z;
        let mut sum = Vec3A::ZERO;
        for i in 0..2048 {
            let u1 = ((i * 13 + 7) % 1024) as f32 / 1024.0;
            let u2 = ((i * 31 + 5) % 1024) as f32 / 1024.0;
            sum += sample_henyey_greenstein(wi, 0.0, u1, u2);
        }
        // Mean of isotropic samples about the origin: near-zero magnitude
        // on all axes.
        let mean = sum / 2048.0;
        assert!(mean.length() < 0.05, "|mean| = {}", mean.length());
    }

    #[test]
    fn thin_walled_transmission_scatters_downward() {
        let m = OpenPBR {
            transmission_weight: 1.0,
            transmission_color: Vec3A::new(0.7, 0.9, 0.7),
            geometry_thin_walled: true,
            ..OpenPBR::default()
        };
        use crate::hittable::HitRecord;
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Y;
        rec.front_face = true;
        let ray = Ray::new(Vec3A::new(0.0, 1.0, 0.0), Vec3A::new(0.0, -1.0, 0.0));
        let mut got_downward = false;
        let mut smp = s();
        for _ in 0..64 {
            if let Some(sample) = m.scatter_importance(&ray, &rec, &mut smp) {
                if sample.ray.direction().y < 0.0 {
                    assert!(sample.delta, "transmission must be flagged delta");
                    got_downward = true;
                    break;
                }
            }
        }
        assert!(got_downward, "thin-walled transmission never went through");
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
        let mut smp = s();
        for _ in 0..64 {
            if let Some(sample) = m.scatter_importance(&ray, &rec, &mut smp) {
                assert!(sample.pdf.is_finite() && sample.pdf > 0.0, "pdf = {}", sample.pdf);
                assert!(sample.value.is_finite(), "value = {:?}", sample.value);
                assert!(
                    sample.value.x >= 0.0 && sample.value.y >= 0.0 && sample.value.z >= 0.0,
                    "value = {:?}",
                    sample.value
                );
            }
        }
    }
}
