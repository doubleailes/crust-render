use glam::Vec3A;
use std::f32::consts::PI;

pub fn fresnel_schlick(cos_theta: f32, f0: Vec3A) -> Vec3A {
    f0 + (Vec3A::new(1.0, 1.0, 1.0) - f0) * f32::powf(1.0 - cos_theta, 5.0)
}

pub fn schlick_weight(cos_theta: f32) -> f32 {
    (1.0 - cos_theta).powf(5.0)
}

// Clearcoat Fresnel approx
pub fn fresnel_schlick_scalar(cos_theta: f32, f0: f32) -> f32 {
    f0 + (1.0 - f0) * (1.0 - cos_theta).powf(5.0)
}

// -------- OpenPBR helpers --------
//
// The helpers below extend `brdf.rs` for the OpenPBR Surface shading model.
// They are intentionally additive: existing helpers (isotropic GGX / VNDF)
// are the `ax == ay` fast path used by the anisotropic wrappers.

/// Schlick reflectance at normal incidence for a dielectric interface with
/// relative IOR `ior` (assumes the exterior IOR is 1). Uses
/// `((ior - 1) / (ior + 1))^2`.
pub fn f0_from_ior(ior: f32) -> f32 {
    let r = (ior - 1.0) / (ior + 1.0);
    r * r
}

/// Convert an isotropic roughness in [0, 1] plus an anisotropy in [0, 1] to
/// two microfacet slope alphas `(ax, ay)`, stretching highlights along the
/// surface tangent. This is the `open_pbr_anisotropy` graph from the OpenPBR
/// MaterialX reference:
///   `ax = r² · √(2 / (1 + (1 − a)²))`,  `ay = (1 − a) · ax`
/// which reduces to `ax = ay = r²` at zero anisotropy.
pub fn roughness_to_alpha_aniso(roughness: f32, anisotropy: f32) -> (f32, f32) {
    let a = roughness * roughness;
    let inv = 1.0 - anisotropy.clamp(0.0, 1.0);
    let ax = a * (2.0 / (1.0 + inv * inv)).sqrt();
    let ay = inv * ax;
    (ax.max(1e-4), ay.max(1e-4))
}

/// Anisotropic GGX NDF, evaluated in the tangent frame where `h` is expressed
/// via `n·h`, `h·t`, `h·b` (all cosines).
pub fn ggx_d_aniso(n_dot_h: f32, h_dot_t: f32, h_dot_b: f32, ax: f32, ay: f32) -> f32 {
    let tx = h_dot_t / ax;
    let ty = h_dot_b / ay;
    let term = tx * tx + ty * ty + n_dot_h * n_dot_h;
    1.0 / (PI * ax * ay * term * term)
}

/// Smith's Lambda function for anisotropic GGX (Heitz 2014).
pub fn ggx_lambda_aniso(v_dot_n: f32, v_dot_t: f32, v_dot_b: f32, ax: f32, ay: f32) -> f32 {
    let vt = v_dot_t * ax;
    let vb = v_dot_b * ay;
    let a2 = vt * vt + vb * vb;
    let n2 = (v_dot_n * v_dot_n).max(1e-8);
    (-1.0 + (1.0 + a2 / n2).sqrt()) * 0.5
}

/// Smith G2 masking-shadowing for anisotropic GGX (uncorrelated form).
#[allow(clippy::too_many_arguments)]
pub fn ggx_g2_smith_aniso(
    v_dot_n: f32,
    v_dot_t: f32,
    v_dot_b: f32,
    l_dot_n: f32,
    l_dot_t: f32,
    l_dot_b: f32,
    ax: f32,
    ay: f32,
) -> f32 {
    let lv = ggx_lambda_aniso(v_dot_n, v_dot_t, v_dot_b, ax, ay);
    let ll = ggx_lambda_aniso(l_dot_n, l_dot_t, l_dot_b, ax, ay);
    1.0 / (1.0 + lv + ll)
}

/// Sample the half-vector in the tangent frame (t, b, n) using Heitz 2018
/// VNDF sampling for anisotropic GGX. `v_local` is the view direction in the
/// local frame with n along +z. Returns the sampled half-vector in the same
/// local frame.
pub fn sample_vndf_ggx_aniso_local(v_local: Vec3A, ax: f32, ay: f32, uv: [f32; 2]) -> Vec3A {
    // Stretch to hemispherical config.
    let vh = Vec3A::new(ax * v_local.x, ay * v_local.y, v_local.z).normalize();

    let lensq = vh.x * vh.x + vh.y * vh.y;
    let t1 = if lensq > 0.0 {
        Vec3A::new(-vh.y, vh.x, 0.0) / lensq.sqrt()
    } else {
        Vec3A::X
    };
    let t2 = vh.cross(t1);

    let (u1, u2) = (uv[0], uv[1]);
    let r = u1.sqrt();
    let phi = 2.0 * PI * u2;
    let t1c = r * phi.cos();
    let t2c_pre = r * phi.sin();
    let s = 0.5 * (1.0 + vh.z);
    let t2c = (1.0 - s) * (1.0 - t1c * t1c).max(0.0).sqrt() + s * t2c_pre;

    let nh = t1 * t1c + t2 * t2c + vh * (1.0 - t1c * t1c - t2c * t2c).max(0.0).sqrt();

    Vec3A::new(ax * nh.x, ay * nh.y, nh.z.max(0.0)).normalize()
}

/// PDF for the reflected direction `l` sampled via VNDF, in the tangent
/// frame. Uses the standard reflection Jacobian `1 / (4 |v·h|)`.
pub fn pdf_vndf_ggx_aniso_local(v_local: Vec3A, h_local: Vec3A, ax: f32, ay: f32) -> f32 {
    let n_dot_v = v_local.z.max(1e-6);
    let n_dot_h = h_local.z.max(1e-6);

    let d = ggx_d_aniso(n_dot_h, h_local.x, h_local.y, ax, ay);
    let lambda_v = ggx_lambda_aniso(n_dot_v, v_local.x, v_local.y, ax, ay);
    let g1 = 1.0 / (1.0 + lambda_v);

    // p(h) = D * G1 * |v.h| / |v.n|   →   p(l) = p(h) / (4 |v.h|)
    d * g1 / (4.0 * n_dot_v)
}

/// Raw half-vector density of VNDF sampling, `p(h) = D · G1 · |v·h| / |v·n|`,
/// in the tangent frame. Combine with the transform Jacobian of the mapping
/// h → outgoing direction (reflection: `1/(4|v·h|)`; refraction: Walter et
/// al. 2007 eq. 17) to get an outgoing-direction pdf.
pub fn pdf_vndf_h_aniso_local(v_local: Vec3A, h_local: Vec3A, ax: f32, ay: f32) -> f32 {
    let n_dot_v = v_local.z.max(1e-6);
    let v_dot_h = v_local.dot(h_local).max(0.0);
    let d = ggx_d_aniso(h_local.z.max(1e-6), h_local.x, h_local.y, ax, ay);
    let lambda_v = ggx_lambda_aniso(n_dot_v, v_local.x, v_local.y, ax, ay);
    let g1 = 1.0 / (1.0 + lambda_v);
    d * g1 * v_dot_h / n_dot_v
}

/// "F82-tint" conductor Fresnel (Kutz et al., as adopted by the OpenPBR
/// metal slab / MaterialX `generalized_schlick_bsdf` with `color0`/`color82`).
/// Plain Schlick pinned at `f0` for normal incidence and white at grazing,
/// with the reflectance at μ̄ = cos 82° ≈ 1/7 scaled by `tint` — the
/// `specular_color` edge tint:
///   `F(μ) = F_s(μ) − μ (1 − μ)⁶ · F_s(μ̄) (1 − tint) / (μ̄ (1 − μ̄)⁶)`
pub fn fresnel_f82_tint(cos_theta: f32, f0: Vec3A, tint: Vec3A) -> Vec3A {
    const MU_BAR: f32 = 1.0 / 7.0;
    let mu = cos_theta.clamp(0.0, 1.0);
    let f_schlick = |m: f32| f0 + (Vec3A::ONE - f0) * (1.0 - m).powf(5.0);
    let denom = MU_BAR * (1.0 - MU_BAR).powi(6);
    let a = f_schlick(MU_BAR) * (Vec3A::ONE - tint) / denom;
    (f_schlick(mu) - a * mu * (1.0 - mu).powi(6)).clamp(Vec3A::ZERO, Vec3A::ONE)
}

/// Exact unpolarized dielectric Fresnel reflectance for an interface with
/// incident-side IOR `eta_i` and transmitted-side IOR `eta_t`. `cos_i` is
/// the (positive) cosine between the incident direction and the facet
/// normal. Returns 1.0 under total internal reflection.
pub fn fresnel_dielectric(cos_i: f32, eta_i: f32, eta_t: f32) -> f32 {
    let cos_i = cos_i.clamp(0.0, 1.0);
    let sin2_t = (eta_i / eta_t) * (eta_i / eta_t) * (1.0 - cos_i * cos_i);
    if sin2_t >= 1.0 {
        return 1.0;
    }
    let cos_t = (1.0 - sin2_t).sqrt();
    let r_par = (eta_t * cos_i - eta_i * cos_t) / (eta_t * cos_i + eta_i * cos_t);
    let r_perp = (eta_i * cos_i - eta_t * cos_t) / (eta_i * cos_i + eta_t * cos_t);
    0.5 * (r_par * r_par + r_perp * r_perp)
}

/// Build an orthonormal tangent frame (t, b) around the surface normal `n`
/// with no assumed UV parametrisation. Uses the Duff et al. 2017 branchless
/// method — stable everywhere, including near the poles.
pub fn tangent_frame(n: Vec3A) -> (Vec3A, Vec3A) {
    let sign = if n.z >= 0.0 { 1.0_f32 } else { -1.0 };
    let a = -1.0 / (sign + n.z);
    let b = n.x * n.y * a;
    let t = Vec3A::new(1.0 + sign * n.x * n.x * a, sign * b, -sign * n.x);
    let bt = Vec3A::new(b, sign + n.y * n.y * a, -n.y);
    (t, bt)
}

/// Convert a world-space vector into the tangent frame `(t, b, n)`.
pub fn to_tangent(v: Vec3A, t: Vec3A, b: Vec3A, n: Vec3A) -> Vec3A {
    Vec3A::new(v.dot(t), v.dot(b), v.dot(n))
}

/// Convert a vector from the tangent frame back to world space.
pub fn from_tangent(v_local: Vec3A, t: Vec3A, b: Vec3A, n: Vec3A) -> Vec3A {
    t * v_local.x + b * v_local.y + n * v_local.z
}

/// Estevez–Kulla "Charlie" sheen distribution, as used by glTF and OpenPBR
/// fuzz. `roughness` is the fuzz roughness in [0, 1].
pub fn sheen_charlie_d(n_dot_h: f32, roughness: f32) -> f32 {
    let alpha = roughness.max(0.05);
    let inv_alpha = 1.0 / alpha;
    let sin_theta_h_sq = (1.0 - n_dot_h * n_dot_h).max(0.0);
    (2.0 + inv_alpha) * sin_theta_h_sq.powf(inv_alpha * 0.5) / (2.0 * PI)
}

/// Sony Imageworks visibility approximation for Charlie sheen.
pub fn sheen_charlie_v(n_dot_v: f32, n_dot_l: f32) -> f32 {
    1.0 / (4.0 * (n_dot_l + n_dot_v - n_dot_l * n_dot_v).max(1e-4))
}

/// Full analytic Charlie sheen BRDF value (D * V, no Fresnel).
pub fn sheen_charlie(n_dot_v: f32, n_dot_l: f32, n_dot_h: f32, roughness: f32) -> f32 {
    sheen_charlie_d(n_dot_h, roughness) * sheen_charlie_v(n_dot_v, n_dot_l)
}

/// A Fresnel-averaged approximation of the darkening a physical coat
/// produces on the base layer through multi-bounce internal reflection. This
/// is the closed-form energy-compensation form recommended in the OpenPBR
/// spec. `coat_darkening ∈ [0, 1]` fades between "no darkening" (`0`, useful
/// for artistic mixes) and "full physical darkening" (`1`).
pub fn coat_darkening_factor(base_color: Vec3A, coat_ior: f32, darkening: f32) -> Vec3A {
    let f_avg = f0_from_ior(coat_ior) + (1.0 - f0_from_ior(coat_ior)) * 0.05;
    let dark = base_color / (Vec3A::ONE - f_avg * (Vec3A::ONE - base_color)).max(Vec3A::splat(1e-4));
    let one = Vec3A::ONE;
    one * (1.0 - darkening) + dark * darkening
}

// -------- Thin-film interference (Belcour & Barla 2017, simplified) --------
//
// Three-layer Airy-summation reflectance for a single dielectric film of
// thickness `d` (nm) and index `η_film` sandwiched between an outer medium
// of index `η_1` and a base of index `η_2`. Evaluated at the CIE sRGB
// primaries (R = 615 nm, G = 545 nm, B = 465 nm) — a 3-wavelength
// approximation that captures the characteristic soap-bubble / oil-slick
// look without full spectral rendering.
const LAMBDA_RGB: [f32; 3] = [615.0, 545.0, 465.0];

/// Airy-summation reflectance of the film stack at a single wavelength, for
/// a base of (real) index `eta_2`. Returns 1.0 past a TIR boundary.
fn thin_film_reflectance_lambda(
    cos_theta_1: f32,
    eta_1: f32,
    eta_film: f32,
    eta_2: f32,
    thickness_nm: f32,
    lambda_nm: f32,
) -> f32 {
    let cos1 = cos_theta_1.clamp(0.0, 1.0);
    let sin2_1 = 1.0 - cos1 * cos1;

    let sin2_film = (eta_1 / eta_film).powi(2) * sin2_1;
    if sin2_film >= 1.0 {
        return 1.0;
    }
    let cos_film = (1.0 - sin2_film).sqrt();

    let sin2_base = (eta_film / eta_2).powi(2) * sin2_film;
    if sin2_base >= 1.0 {
        return 1.0;
    }
    let cos_base = (1.0 - sin2_base).sqrt();

    // Amplitude-space Fresnel at each interface (average of s and p — good
    // enough for unpolarised light, keeps the formula scalar-per-wavelength).
    let r_a = fresnel_amplitude(eta_1, eta_film, cos1, cos_film);
    let r_b = fresnel_amplitude(eta_film, eta_2, cos_film, cos_base);

    // Optical path difference inside the film.
    let opd = 2.0 * eta_film * thickness_nm * cos_film;

    let phi = 2.0 * PI * opd / lambda_nm;
    let cos_phi = phi.cos();
    let num = r_a * r_a + 2.0 * r_a * r_b * cos_phi + r_b * r_b;
    let den = 1.0 + 2.0 * r_a * r_b * cos_phi + (r_a * r_b).powi(2);
    (num / den.max(1e-8)).clamp(0.0, 1.0)
}

pub fn thin_film_fresnel(
    cos_theta_1: f32,
    eta_1: f32,
    eta_film: f32,
    eta_2: f32,
    thickness_nm: f32,
) -> Vec3A {
    let mut out = [0.0f32; 3];
    for (i, lambda) in LAMBDA_RGB.into_iter().enumerate() {
        out[i] =
            thin_film_reflectance_lambda(cos_theta_1, eta_1, eta_film, eta_2, thickness_nm, lambda);
    }
    Vec3A::from_array(out)
}

/// Thin-film reflectance over a metallic base described by its per-channel
/// normal-incidence reflectance `f0` (the OpenPBR metal slab's
/// `base_color · base_weight`). Each channel's F0 is converted to the
/// equivalent real IOR `η = (1 + √F0) / (1 − √F0)` and the Airy summation is
/// evaluated at that channel's wavelength — the same 3-wavelength
/// approximation as `thin_film_fresnel`, mirroring the MaterialX
/// `generalized_schlick_bsdf` thin-film variant used by `metal_bsdf_tf`.
pub fn thin_film_fresnel_metal(
    cos_theta_1: f32,
    eta_1: f32,
    eta_film: f32,
    f0: Vec3A,
    thickness_nm: f32,
) -> Vec3A {
    let mut out = [0.0f32; 3];
    for (i, lambda) in LAMBDA_RGB.into_iter().enumerate() {
        let f0_c = f0[i].clamp(0.0, 0.9999);
        let sqrt_f0 = f0_c.sqrt();
        let eta_2 = (1.0 + sqrt_f0) / (1.0 - sqrt_f0);
        out[i] =
            thin_film_reflectance_lambda(cos_theta_1, eta_1, eta_film, eta_2, thickness_nm, lambda);
    }
    Vec3A::from_array(out)
}

// Signed amplitude Fresnel — average of s/p, sign preserved (positive when
// going from lower to higher index at normal incidence).
fn fresnel_amplitude(eta_i: f32, eta_t: f32, cos_i: f32, cos_t: f32) -> f32 {
    let rs = (eta_i * cos_i - eta_t * cos_t) / (eta_i * cos_i + eta_t * cos_t);
    let rp = (eta_t * cos_i - eta_i * cos_t) / (eta_t * cos_i + eta_i * cos_t);
    0.5 * (rs + rp)
}
