//! Transmission: thin-walled (a delta lobe) and thick, a Walter et al. 2007
//! microfacet BTDF with per-channel Cauchy dispersion.

use glam::Vec3A;

use crate::hittable::HitRecord;
use crate::material::brdf::*;
use crate::ray::Ray;

use super::OpenPBR;
use super::lobes::Lobe;

/// Per-channel IOR for a dispersive dielectric, `(η_R, η_G, η_B)` at the
/// sRGB primary wavelengths, via the Cauchy/Abbe fit (`cauchy_ior`).
/// `transmission_dispersion_scale` divides the authored Abbe number — the
/// effective Abbe is `abbe / scale`, so scale 1 is the physical dispersion
/// of a glass with that Abbe number, larger scales exaggerate it linearly,
/// and 0 collapses to `(η_D, η_D, η_D)`. An IOR below 1 (interior less
/// dense than the exterior) disperses via its reciprocal, keeping the
/// model symmetric across the interface.
pub(super) fn dispersive_ior(n_d: f32, abbe: f32, dispersion_scale: f32) -> Vec3A {
    if dispersion_scale <= 0.0 || n_d == 1.0 {
        return Vec3A::splat(n_d);
    }
    let inverted = n_d < 1.0;
    let n_above_one = if inverted { 1.0 / n_d } else { n_d };
    // Realistic glasses span V_d ≈ 20–95; the floor keeps authored extremes
    // (tiny Abbe, huge scale) from producing a runaway Cauchy B term.
    let v_d = (abbe.max(1.0) / dispersion_scale).max(1.0);
    let mut out = [0.0f32; 3];
    for (c, lambda) in LAMBDA_RGB.into_iter().enumerate() {
        let n = cauchy_ior(n_above_one, v_d, lambda);
        out[c] = if inverted { 1.0 / n } else { n };
    }
    Vec3A::from_array(out)
}

/// Refract `v` (unit, pointing away from the surface) across the surface with
/// unit outward normal `n`, using relative index `eta = η_incident / η_transmitted`.
/// Returns None on total internal reflection. (Analytic Snell reference,
/// used by tests to cross-check the sampled BTDF directions.)
#[cfg(test)]
pub(super) fn refract_dir(v: Vec3A, n: Vec3A, eta: f32) -> Option<Vec3A> {
    let cos_i = v.dot(n).clamp(-1.0, 1.0);
    let sin2_t = eta * eta * (1.0 - cos_i * cos_i);
    if sin2_t >= 1.0 {
        return None;
    }
    let cos_t = (1.0 - sin2_t).sqrt();
    // Transmitted direction, using the standard vector form of Snell.
    Some(-v * eta + n * (eta * cos_i - cos_t))
}

/// Thin-walled delta transmission — the window model of the Adobe OpenPBR
/// reference: straight through with no bending (the front and back
/// refractions of an infinitesimally thin sheet cancel), but with proper
/// window energy. The transmitted fraction accounts for both interfaces
/// *plus all internal bounces*: `T = 1 − 2R/(1+R) = (1−R)/(1+R)` with `R`
/// the single-interface dielectric Fresnel at the view angle
/// (`openpbr_thin_wall_fresnel`). The authored `transmission_color` — the
/// transmittance at *normal* incidence — is raised to the relative in-sheet
/// path length `1/cos θ_refracted` (Beer-Lambert along the slanted path).
/// Returns (world-space scattered ray, throughput, placeholder pdf).
pub(super) fn sample_transmission_thin(
    m: &OpenPBR,
    r_in: &Ray,
    rec: &HitRecord,
) -> (Ray, Vec3A, f32) {
    let dir = r_in.direction().normalize();
    let l_world = dir;
    // Thin walls are double-sided: always treated as hit from outside.
    let cos_i = (-dir).dot(rec.normal).clamp(0.0, 1.0);
    let eta = m.specular_ior.max(1e-4);

    // Refracted angle inside the sheet (Snell); η < 1 sheets can TIR.
    let sin2_t = (1.0 - cos_i * cos_i) / (eta * eta);
    if sin2_t >= 1.0 {
        return (Ray::new(rec.p, l_world), Vec3A::ZERO, 1.0);
    }
    let cos_t = (1.0 - sin2_t).sqrt();

    // Window transmittance: both surfaces and every internal bounce.
    let f = fresnel_dielectric(cos_i, 1.0, eta);
    let window_transmittance = (1.0 - f) / (1.0 + f);

    // View-dependent absorption along the refracted path.
    let path_length = 1.0 / cos_t.max(1e-4);
    let tint = m
        .transmission_color
        .clamp(Vec3A::ZERO, Vec3A::ONE)
        .powf(path_length);

    // No cosine in a delta lobe; we ape the codebase convention by
    // returning the throughput directly (the tracer's cosine multiply is
    // strictly incorrect for delta lobes but matches the rest of the
    // renderer's estimator).
    let throughput = tint * (window_transmittance * m.transmission_weight);
    // Delta pdf: use 1.0 so tracer's `brdf / pdf` returns the throughput
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
pub(super) fn transmission_is_continuous(m: &OpenPBR) -> bool {
    m.transmission_weight > 0.0 && !m.geometry_thin_walled
}

/// Per-channel interior IORs of the transmission lobe (all three equal when
/// dispersion is off).
pub(super) fn transmission_iors(m: &OpenPBR) -> Vec3A {
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
/// The angular width a sampled lobe adds to the path's texture-filtering
/// cone (see [`crate::RayCone`]).
///
/// A GGX lobe of perceptual roughness `r` reflects over roughly its alpha
/// `r²`, so `2·alpha` is the diameter measure a cone wants; the anisotropic
/// case takes the mean of the two alphas, matching the cone's own isotropy.
/// A cosine lobe covers the hemisphere and saturates the cone outright —
/// after a diffuse bounce there is no footprint left worth tracking, which is
/// why indirect illumination reads a coarse mip in every renderer that does
/// this.
// Forced inline: once `scatter_with` grew its `STRAIGHT` instantiations LLVM
// kept this out of line (+0.1% of cornellbox's instructions as a call).
#[inline(always)]
pub(super) fn lobe_spread(m: &OpenPBR, lobe: Lobe) -> f32 {
    let from_alpha = |(ax, ay): (f32, f32)| (ax + ay).min(crate::RayCone::MAX_SPREAD);
    match lobe {
        Lobe::Diffuse | Lobe::Fuzz => crate::RayCone::MAX_SPREAD,
        Lobe::Specular => from_alpha(roughness_to_alpha_aniso(
            m.specular_roughness,
            m.specular_roughness_anisotropy,
        )),
        Lobe::Coat => from_alpha(roughness_to_alpha_aniso(
            super::lobes::coat_roughness(m),
            m.coat_roughness_anisotropy,
        )),
        Lobe::Transmission => from_alpha(transmission_alphas(m)),
    }
}

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
    let btdf =
        (v_dot_h * -l_dot_h) / (n_dot_v * n_dot_l) * (eta_t * eta_t * (1.0 - f) * d * g / denom2);

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
pub(super) fn eval_transmission(
    m: &OpenPBR,
    v_local: Vec3A,
    l_local: Vec3A,
    entering: bool,
) -> (Vec3A, f32) {
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
pub(super) fn sample_transmission_rough(
    m: &OpenPBR,
    v_local: Vec3A,
    entering: bool,
    dispersion_u: f32,
    vndf_uv: [f32; 2],
) -> Option<Vec3A> {
    let iors = transmission_iors(m);
    let ior = if m.transmission_dispersion_scale > 0.0 {
        if dispersion_u < 1.0 / 3.0 {
            iors.x
        } else if dispersion_u < 2.0 / 3.0 {
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

    let h = sample_vndf_ggx_aniso_local(v_local, ax, ay, vndf_uv);
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
