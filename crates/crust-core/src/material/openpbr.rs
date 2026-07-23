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

        // Metal reflectivity is base_color · base_weight scaled by
        // specular_weight (the reference's `metal_bsdf` weight input).
        let w_metal =
            m.base_metalness * m.specular_weight * luma(m.base_color * m.base_weight).max(0.02);
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

        // The coat reflection is untinted — coat_color only attenuates the
        // substrate — so its lobe weight ignores the color.
        let w_coat = (m.coat_weight * f0_coat).max(1e-6);
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

fn eval_diffuse(m: &OpenPBR, v_local: Vec3A, l_local: Vec3A, f_avg_diel: f32) -> Vec3A {
    // EON diffuse (energy-preserving Fujii Oren-Nayar) — the model the
    // OpenPBR spec names for the base diffuse slab. `base_diffuse_roughness`
    // is the diffuse-only roughness (independent of specular_roughness).
    if l_local.z <= 0.0 || v_local.z <= 0.0 {
        return Vec3A::ZERO;
    }

    // Subsurface behaves as a colour-shifted diffuse when the walk length
    // is short relative to feature size: surface the directional diffuse
    // response with the SSS tint so the average colour matches (the full
    // random-walk BSSRDF is future work — see the module header).
    let diffuse_color = m.base_color.lerp(m.subsurface_color, m.subsurface_weight);

    // Fold the presence weights into the EON albedo, as Adobe's reference
    // does (`diffuse_albedo = base_color · base_weight · opaque-dielectric
    // fraction`): the multiple-scattering term is nonlinear in ρ and should
    // saturate with the *effective* albedo, not the raw color.
    // Transmission displaces the diffuse base — see `LobePmf::from_params`.
    let rho = diffuse_color
        * (m.base_weight * (1.0 - m.base_metalness) * (1.0 - m.transmission_weight));

    // Energy left after specular reflection: `1 - F_dielectric_avg`. Using
    // the directional Fresnel here would double-count with the specular
    // lobe's own Fresnel — the average avoids that.
    eon_diffuse(rho, m.base_diffuse_roughness, v_local, l_local) * (1.0 - f_avg_diel)
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

    // OpenPBR spec places thin-film between coat and base; when there is
    // no coat the outer medium is air.
    let outer_ior = if m.coat_weight > 0.0 {
        m.coat_ior
    } else {
        1.0
    };
    // OpenPBR thickness is in μm; the thin-film helpers want nm.
    let tf_thickness_nm = m.thin_film_thickness * 1000.0;

    // Thin-film interference (Phase 2): replaces the Fresnel with an
    // iridescent one at 3 wavelengths, blended by thin_film_weight.
    let f_diel = if m.thin_film_weight > 0.0 {
        let f_normal = fresnel_schlick(v_dot_h, f0_diel_base);
        let f_iri = thin_film_fresnel(
            v_dot_h,
            outer_ior,
            m.thin_film_ior,
            m.specular_ior,
            tf_thickness_nm,
        );
        f_normal * (1.0 - m.thin_film_weight) + f_iri * m.thin_film_weight
    } else {
        fresnel_schlick(v_dot_h, f0_diel_base)
    };

    // Metal slab per the MaterialX reference `generalized_schlick_bsdf`:
    // F0 = base_color · base_weight, F82 edge tint = specular_color, the
    // whole lobe scaled by specular_weight — with its own thin-film variant
    // (`metal_bsdf_tf`) blended in by thin_film_weight.
    let metal_f0 = m.base_color * m.base_weight;
    let f_metal_base = fresnel_f82_tint(v_dot_h, metal_f0, m.specular_color);
    let f_metal = (if m.thin_film_weight > 0.0 {
        let f_iri = thin_film_fresnel_metal(
            v_dot_h,
            outer_ior,
            m.thin_film_ior,
            metal_f0,
            tf_thickness_nm,
        );
        f_metal_base * (1.0 - m.thin_film_weight) + f_iri * m.thin_film_weight
    } else {
        f_metal_base
    }) * m.specular_weight;

    let brdf = d * g / (4.0 * n_dot_v * n_dot_l);
    // Metal path: F82-tinted Fresnel * brdf * metalness
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
    // The coat reflection itself is untinted (the reference's `coat_bsdf`
    // has no color input) — `coat_color` is absorption on the way *through*
    // the coat and lives in `coat_attenuation`.
    Vec3A::splat(m.coat_weight * f * brdf)
}

/// Directional attenuation the coat imposes on the layers beneath it:
/// Fresnel-weighted transmission * coat absorption tint * multi-bounce
/// darkening factor. Applied as a per-channel multiplier to
/// (base_specular + base_diffuse). The tint term is the reference's
/// `coat_attenuation` node — `lerp(white, coat_color, coat_weight)` — which
/// puts `coat_color` on light transmitted through the coat, not on the
/// coat's own reflection.
fn coat_attenuation(m: &OpenPBR, cos_theta_h: f32) -> Vec3A {
    if m.coat_weight <= 0.0 {
        return Vec3A::ONE;
    }
    let f_coat = fresnel_schlick_scalar(cos_theta_h, f0_from_ior(m.coat_ior));
    let transmit = 1.0 - m.coat_weight * f_coat;
    let absorb = Vec3A::ONE.lerp(m.coat_color, m.coat_weight);
    let dark = coat_darkening_factor(m.base_color, m.coat_ior, m.coat_darkening);
    Vec3A::splat(transmit) * absorb * dark
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

    let diffuse = eval_diffuse(m, v_local, l_local, f_avg_diel);
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

/// Refract `v` (unit, pointing away from the surface) across the surface with
/// unit outward normal `n`, using relative index `eta = η_incident / η_transmitted`.
/// Returns None on total internal reflection. (Analytic Snell reference,
/// used by tests to cross-check the sampled BTDF directions.)
#[cfg_attr(not(test), allow(dead_code))]
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

/// Thin-walled delta transmission: straight through, no bending, no medium.
/// Returns (world-space scattered ray, throughput, placeholder pdf).
fn sample_transmission_thin(m: &OpenPBR, r_in: &Ray, rec: &HitRecord) -> (Ray, Vec3A, f32) {
    let l_world = r_in.direction().normalize();
    // No cosine in a delta lobe; we ape the codebase convention by
    // returning throughput = tint (the tracer's cosine multiply is
    // strictly incorrect for delta lobes but matches the rest of the
    // renderer's estimator).
    let throughput = m.transmission_color * m.transmission_weight;
    // Delta pdf: use 1.0 so tracer's `brdf / pdf` returns the tint
    // unmodified. Direct-light MIS won't hit a delta lobe.
    (Ray::new(rec.p, l_world), throughput, 1.0)
}

// ---------------------------------------------------------------------------
// Rough refraction — Walter et al. 2007, "Microfacet Models for Refraction
// through Rough Surfaces". Thick transmission — dispersive or not — is a
// proper continuous BTDF lobe: sampleable, evaluable, and therefore visible
// to NEE and the guiding mixture. Dispersion is continuous per-channel:
// each RGB channel refracts with its own IOR, sampling picks one channel's
// IOR uniformly, and evaluation runs three per-channel BTDF evaluations
// whose sampling pdfs average into the channel-mixture density. Only
// thin-walled transmission remains a delta lobe.
// ---------------------------------------------------------------------------

/// Whether the transmission lobe is a continuous BTDF (thick refraction,
/// dispersive or not) as opposed to a delta lobe (thin-walled only).
fn transmission_is_continuous(m: &OpenPBR) -> bool {
    m.transmission_weight > 0.0 && !m.geometry_thin_walled
}

/// Per-channel interior IORs of the transmission lobe (all three equal when
/// dispersion is off).
fn transmission_iors(m: &OpenPBR) -> Vec3A {
    dispersive_ior(
        m.specular_ior,
        m.transmission_dispersion_abbe_number,
        m.transmission_dispersion_scale,
    )
}

/// Incident / transmitted IORs at the interface for an interior IOR `ior`,
/// in the ray-facing local frame (`entering` = the ray hit the front face
/// and refracts into the interior medium).
fn interface_iors(ior: f32, entering: bool) -> (f32, f32) {
    if entering { (1.0, ior) } else { (ior, 1.0) }
}

/// GGX alphas for the transmission lobe. Roughness is floored so the
/// distribution stays finite for nominally perfect glass.
fn transmission_alphas(m: &OpenPBR) -> (f32, f32) {
    roughness_to_alpha_aniso(
        m.specular_roughness.max(0.01),
        m.specular_roughness_anisotropy,
    )
}

/// Walter et al. BTDF for one color channel with interior IOR `ior`:
/// untinted scalar BTDF value (without the tracer-facing cosine) and the
/// matching VNDF sampling pdf for a below-hemisphere direction `l_local`.
/// Returns zeros when `l_local` is not a valid refraction of `v_local` at
/// this IOR.
fn eval_transmission_channel(
    m: &OpenPBR,
    v_local: Vec3A,
    l_local: Vec3A,
    entering: bool,
    ior: f32,
) -> (f32, f32) {
    let (eta_i, eta_t) = interface_iors(ior, entering);

    // Half vector for refraction (eq. 16): h ∝ -(η_i·v + η_t·l), oriented
    // into the upper hemisphere of the ray-facing frame.
    let mut h = -(v_local * eta_i + l_local * eta_t);
    if h.length_squared() < 1e-12 {
        return (0.0, 0.0);
    }
    h = h.normalize();
    if h.z < 0.0 {
        h = -h;
    }

    let v_dot_h = v_local.dot(h);
    let l_dot_h = l_local.dot(h);
    if v_dot_h <= 1e-6 || l_dot_h >= -1e-6 {
        return (0.0, 0.0);
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
        return (0.0, 0.0);
    }

    // BTDF (eq. 21).
    let btdf = (v_dot_h * -l_dot_h) / (n_dot_v * n_dot_l)
        * (eta_t * eta_t * (1.0 - f) * d * g / denom2);

    // pdf: raw VNDF half-vector density times the refraction Jacobian
    // (eq. 17): dω_h/dω_l = η_t² |l·h| / (η_i(v·h) + η_t(l·h))².
    let p_h = pdf_vndf_h_aniso_local(v_local, h, ax, ay);
    let jacobian = eta_t * eta_t * -l_dot_h / denom2;

    (btdf.max(0.0), p_h * jacobian)
}

/// Transmission BTDF value (tinted, without the tracer-facing cosine) and
/// sampling pdf for a below-hemisphere direction `l_local`.
///
/// Without dispersion this is a single Walter BTDF. With dispersion each RGB
/// channel refracts with its own IOR, so the lobe is a uniform per-channel
/// mixture: three BTDF evaluations — channel `c`'s value comes from η_c —
/// and a pdf that averages the three per-channel sampling densities
/// (matching `sample_transmission_rough`, which picks a channel uniformly).
fn eval_transmission(m: &OpenPBR, v_local: Vec3A, l_local: Vec3A, entering: bool) -> (Vec3A, f32) {
    // The reference's `if_transmission_tint`: with `transmission_depth > 0`
    // the interior Beer-Lambert medium owns the color (see
    // `Medium::from_transmission`), so the interface BTDF is untinted —
    // tinting both would apply `transmission_color` twice. Only at zero
    // depth does the color act as a non-physical surface tint.
    let color = if m.transmission_depth > 0.0 {
        Vec3A::ONE
    } else {
        m.transmission_color
    };
    let tint = color * (m.transmission_weight * (1.0 - m.base_metalness));
    let iors = transmission_iors(m);
    if m.transmission_dispersion_scale <= 0.0 {
        let (btdf, pdf) = eval_transmission_channel(m, v_local, l_local, entering, iors.y);
        return (tint * btdf, pdf);
    }
    let mut value = [0.0f32; 3];
    let mut pdf = 0.0;
    for (c, ior) in [iors.x, iors.y, iors.z].into_iter().enumerate() {
        let (btdf, p) = eval_transmission_channel(m, v_local, l_local, entering, ior);
        value[c] = btdf;
        pdf += p / 3.0;
    }
    (tint * Vec3A::from_array(value), pdf)
}

/// Sample the continuous transmission lobe: VNDF half-vector, then Snell.
/// With dispersion active, one RGB channel's IOR is picked uniformly — the
/// estimator divides by the channel-averaged pdf from `eval_transmission`
/// (one-sample channel mixture), so no hero-channel throughput mask is
/// needed. Returns the transmitted direction in the local frame, or `None`
/// on total internal reflection at the sampled microfacet (that energy is
/// carried by the specular reflection lobe).
fn sample_transmission_rough(
    m: &OpenPBR,
    v_local: Vec3A,
    entering: bool,
    sampler: &mut dyn Sampler,
) -> Option<Vec3A> {
    let iors = transmission_iors(m);
    let ior = if m.transmission_dispersion_scale > 0.0 {
        let u = sampler.next_1d();
        if u < 1.0 / 3.0 {
            iors.x
        } else if u < 2.0 / 3.0 {
            iors.y
        } else {
            iors.z
        }
    } else {
        iors.y
    };
    let (eta_i, eta_t) = interface_iors(ior, entering);
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
            // Thick refraction — dispersive or not — is a continuous Walter
            // BTDF lobe: value and pdf come from the same full-sphere
            // eval_all/pdf_all composition as the reflection lobes, so
            // sampling and evaluation agree exactly. Dispersion samples one
            // channel's IOR; eval_all/pdf_all answer with the three-channel
            // BTDF value and the channel-averaged mixture pdf.
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

            // Thin-walled transmission stays a delta lobe (placeholder pdf,
            // never mixed with a continuous density).
            let (scattered, throughput, pdf) = sample_transmission_thin(self, r_in, rec);
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
        // reflection lobes above the ray-facing hemisphere and — for thick
        // transmissive surfaces, dispersive or not — the Walter BTDF below
        // it (per-channel with the channel-mixture pdf when dispersion is
        // active). The only delta lobe left (thin-walled transmission) is
        // excluded per the trait contract; `pdf_all` reports the matching
        // defective density.
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

    /// Emission seen through the coat, per the reference's `emission_edf`
    /// mix: the coated branch tints the EDF by `coat_color` and modulates it
    /// by a `generalized_schlick_edf` with `color0 = 1 − coat_F0`,
    /// `color90 = 0`, exponent 5 — i.e. `(1 − F0)(1 − (1 − μ)⁵)` — the coat's
    /// view-dependent Fresnel transmission. Blended with the uncoated EDF by
    /// `coat_weight`.
    fn emitted_directional(&self, cos_theta_o: f32) -> Vec3A {
        let uncoated = self.emission_color * self.emission_luminance;
        if self.coat_weight <= 0.0 {
            return uncoated;
        }
        let f0_coat = f0_from_ior(self.coat_ior);
        let mu = cos_theta_o.clamp(0.0, 1.0);
        let schlick_transmit = (1.0 - f0_coat) * (1.0 - schlick_weight(mu));
        let coated = uncoated * self.coat_color * schlick_transmit;
        uncoated.lerp(coated, self.coat_weight)
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
    fn dispersive_glass_is_continuous_and_eval_consistent() {
        // Dispersive thick glass is a per-channel continuous BTDF: no delta
        // samples, and scatter_importance must agree with eval (three-channel
        // value, channel-averaged mixture pdf) on both hemispheres.
        let m = OpenPBR {
            specular_roughness: 0.25,
            transmission_dispersion_scale: 1.0,
            ..OpenPBR::glass(1.5)
        };
        let mut sampler = s();
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let r_in = Ray::new(Vec3A::new(0.3, -0.2, 1.0), Vec3A::new(-0.3, 0.2, -1.0).normalize());

        let mut transmitted = 0;
        for _ in 0..256 {
            if let Some(sample) = m.scatter_importance(&r_in, &rec, &mut sampler) {
                assert!(!sample.delta, "dispersive thick glass has no delta lobe");
                let wi = sample.ray.direction().normalize();
                if wi.z < 0.0 {
                    transmitted += 1;
                }
                let (ev, epdf) = m.eval(&r_in, &rec, wi).expect("dispersive glass is evaluable");
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
                // One-sample channel-mixture weights are bounded by roughly
                // 3× the non-dispersive Walter weight.
                let w = (sample.value / sample.pdf).max_element();
                assert!(w.is_finite() && w >= 0.0 && w < 30.0, "weight {w}");
            }
        }
        assert!(transmitted > 64, "dispersive glass should mostly refract: {transmitted}");
    }

    #[test]
    fn dispersion_separates_channels() {
        // Near-smooth dispersive glass: at the green-channel Snell direction
        // the BTDF must be green-dominated — red and blue refract to
        // measurably different directions.
        let m = OpenPBR {
            transmission_dispersion_scale: 1.0,
            ..OpenPBR::glass(1.5)
        };
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let dir_in = Vec3A::new(0.6, 0.0, -1.0).normalize();
        let r_in = Ray::new(-dir_in, dir_in);
        let l_green = refract_dir(-dir_in, Vec3A::Z, 1.0 / 1.5).unwrap().normalize();

        let (v, pdf) = m.eval(&r_in, &rec, l_green).expect("evaluable");
        assert!(pdf > 0.0, "pdf = {pdf}");
        assert!(v.y > 0.0, "green channel dark at its own Snell direction: {v}");
        assert!(v.y > v.x, "green {} not > red {} at green Snell direction", v.y, v.x);
        assert!(v.y > v.z, "green {} not > blue {} at green Snell direction", v.y, v.z);
    }

    #[test]
    fn eon_reduces_to_lambert_at_zero_roughness() {
        let rho = Vec3A::new(0.8, 0.5, 0.3);
        let v = Vec3A::new(0.3, 0.1, 0.9).normalize();
        let l = Vec3A::new(-0.2, 0.4, 0.8).normalize();
        let f = eon_diffuse(rho, 0.0, v, l);
        let lambert = rho / PI;
        assert!(
            (f - lambert).abs().max_element() < 1e-4,
            "EON at r=0 {f} != Lambert {lambert}"
        );
    }

    #[test]
    fn eon_is_reciprocal() {
        let rho = Vec3A::new(0.9, 0.6, 0.2);
        let v = Vec3A::new(0.5, -0.1, 0.6).normalize();
        let l = Vec3A::new(-0.3, 0.2, 0.9).normalize();
        for r in [0.2, 0.6, 1.0] {
            let a = eon_diffuse(rho, r, v, l);
            let b = eon_diffuse(rho, r, l, v);
            assert!((a - b).abs().max_element() < 1e-5, "r={r}: {a} vs {b}");
        }
    }

    #[test]
    fn eon_albedo_approx_matches_exact() {
        for i in 1..=20 {
            let mu = i as f32 / 20.0;
            for r in [0.0, 0.3, 0.7, 1.0] {
                let exact = eon_albedo_exact(mu, r);
                let approx = eon_albedo_approx(mu, r);
                assert!(
                    (exact - approx).abs() < 0.01,
                    "albedo mismatch at mu={mu}, r={r}: {exact} vs {approx}"
                );
            }
        }
    }

    #[test]
    fn eon_preserves_energy_at_high_roughness() {
        // The defining property: at rho = 1 the hemispherical albedo
        // (∫ f·cosθ dω) is 1 for any roughness — the single-scattering Fujii
        // lobe alone loses well over 10% at roughness 1; the
        // multiple-scattering lobe restores it. Quadrature over the
        // hemisphere for a few view angles.
        let n_theta = 128;
        let n_phi = 128;
        for view_z in [0.95f32, 0.6, 0.25] {
            let v = Vec3A::new((1.0 - view_z * view_z).sqrt(), 0.0, view_z);
            let mut integral = 0.0f32;
            for it in 0..n_theta {
                let theta = (it as f32 + 0.5) / n_theta as f32 * (PI / 2.0);
                let (sin_t, cos_t) = theta.sin_cos();
                for ip in 0..n_phi {
                    let phi = (ip as f32 + 0.5) / n_phi as f32 * (2.0 * PI);
                    let l = Vec3A::new(sin_t * phi.cos(), sin_t * phi.sin(), cos_t);
                    let f = eon_diffuse(Vec3A::ONE, 1.0, v, l).x;
                    integral += f * cos_t * sin_t;
                }
            }
            integral *= (PI / 2.0) / n_theta as f32 * (2.0 * PI) / n_phi as f32;
            assert!(
                (0.97..=1.03).contains(&integral),
                "white-furnace albedo {integral} at view_z {view_z}"
            );
        }
    }

    #[test]
    fn deep_transmission_interface_is_untinted() {
        // With transmission_depth > 0 the Beer-Lambert medium owns the color:
        // the interface BTDF must be untinted (channel-uniform), or the color
        // would apply twice. At zero depth the color is a surface tint.
        let color = Vec3A::new(0.9, 0.4, 0.2);
        let shallow = OpenPBR {
            specular_roughness: 0.25,
            transmission_color: color,
            ..OpenPBR::glass(1.5)
        };
        let deep = OpenPBR {
            transmission_depth: 1.0,
            ..shallow.clone()
        };
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let dir_in = Vec3A::new(0.4, 0.0, -1.0).normalize();
        let r_in = Ray::new(-dir_in, dir_in);
        let wi = refract_dir(-dir_in, Vec3A::Z, 1.0 / 1.5).unwrap().normalize();

        let (v_deep, _) = deep.eval(&r_in, &rec, wi).expect("evaluable");
        assert!(v_deep.y > 0.0, "no transmission at the Snell direction");
        assert!(
            (v_deep.x - v_deep.y).abs() < 1e-5 && (v_deep.y - v_deep.z).abs() < 1e-5,
            "deep transmission tinted at the interface: {v_deep}"
        );

        let (v_shallow, _) = shallow.eval(&r_in, &rec, wi).expect("evaluable");
        let ratio = v_shallow.x / v_shallow.y;
        assert!(
            (ratio - color.x / color.y).abs() < 1e-3,
            "zero-depth tint ratio {ratio} != color ratio {}",
            color.x / color.y
        );
    }

    #[test]
    fn f82_metal_edge_tint() {
        let f0 = Vec3A::new(0.9, 0.6, 0.3);
        let tint = Vec3A::new(1.0, 0.5, 0.25);
        // Normal incidence pins F0 regardless of tint.
        let f_n = fresnel_f82_tint(1.0, f0, tint);
        assert!((f_n - f0).abs().max_element() < 1e-5, "F(0°) = {f_n}");
        // Grazing incidence goes to white.
        let f_g = fresnel_f82_tint(0.0, f0, tint);
        assert!((f_g - Vec3A::ONE).abs().max_element() < 1e-5, "F(90°) = {f_g}");
        // At μ̄ = 1/7 the reflectance is exactly Schlick scaled by the tint.
        let mu_bar = 1.0 / 7.0;
        let with = fresnel_f82_tint(mu_bar, f0, tint);
        let without = fresnel_f82_tint(mu_bar, f0, Vec3A::ONE);
        assert!((with.y / without.y - tint.y).abs() < 1e-3, "{with} vs {without}");
        assert!((with.z / without.z - tint.z).abs() < 1e-3, "{with} vs {without}");
        assert!((with.x - without.x).abs() < 1e-5, "untinted channel moved");
    }

    #[test]
    fn coat_color_tints_substrate_not_coat_reflection() {
        let m = OpenPBR {
            coat_weight: 1.0,
            coat_color: Vec3A::new(0.9, 0.2, 0.2),
            coat_darkening: 0.0, // isolate the absorption tint
            ..OpenPBR::default()
        };
        // The coat reflection lobe itself is untinted.
        let v = Vec3A::new(0.3, 0.0, 1.0).normalize();
        let c = eval_coat(&m, v, v, v, 0.1, 0.1);
        assert!(
            (c.x - c.y).abs() < 1e-6 && (c.y - c.z).abs() < 1e-6,
            "coat reflection tinted: {c}"
        );
        // The substrate attenuation carries the coat_color absorption.
        let atten = coat_attenuation(&m, 0.9);
        assert!(
            (atten.x / atten.y - m.coat_color.x / m.coat_color.y).abs() < 1e-3,
            "substrate attenuation not coat_color-tinted: {atten}"
        );
    }

    #[test]
    fn coated_emission_dims_and_tints() {
        let uncoated = OpenPBR {
            emission_luminance: 100.0,
            ..OpenPBR::default()
        };
        // No coat: directional emission equals the isotropic EDF.
        assert_eq!(uncoated.emitted_directional(0.3), uncoated.emitted());

        let coated = OpenPBR {
            coat_weight: 1.0,
            coat_color: Vec3A::new(1.0, 0.2, 0.2),
            ..uncoated.clone()
        };
        let e_n = coated.emitted_directional(1.0);
        // Dimmed by the coat's Fresnel transmission (1 - F0 at normal).
        assert!(e_n.x < 100.0, "coated emission not dimmed: {e_n}");
        // Tinted by coat_color.
        assert!((e_n.y / e_n.x - 0.2).abs() < 1e-3, "not coat-tinted: {e_n}");
        // Grazing angles transmit less than normal incidence.
        let e_g = coated.emitted_directional(0.05);
        assert!(e_g.x < e_n.x, "grazing {e_g} not dimmer than normal {e_n}");
    }

    #[test]
    fn aniso_matches_reference_remap() {
        // The open_pbr_anisotropy graph: ax = r²·√(2/(1+(1−a)²)), ay = (1−a)·ax.
        let (r, a) = (0.5f32, 0.8f32);
        let (ax, ay) = roughness_to_alpha_aniso(r, a);
        let inv = 1.0 - a;
        let expect_ax = r * r * (2.0 / (1.0 + inv * inv)).sqrt();
        assert!((ax - expect_ax).abs() < 1e-6, "ax {ax} != {expect_ax}");
        assert!((ay - inv * expect_ax).abs() < 1e-6, "ay {ay} != {}", inv * expect_ax);
    }

    #[test]
    fn thin_film_on_metal_is_bounded_and_active() {
        let f0 = Vec3A::new(0.9, 0.7, 0.4);
        let r = thin_film_fresnel_metal(0.8, 1.0, 1.4, f0, 500.0);
        for c in [r.x, r.y, r.z] {
            assert!((0.0..=1.0).contains(&c), "out of range: {r}");
        }
        // A visible film must actually change the metal Fresnel somewhere.
        let plain = fresnel_f82_tint(0.8, f0, Vec3A::ONE);
        assert!((r - plain).abs().max_element() > 1e-3, "film had no effect");
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
