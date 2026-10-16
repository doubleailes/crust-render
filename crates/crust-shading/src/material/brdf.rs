use glam::Vec3A;
use std::f32::consts::PI;

// `powi(5)` rather than `powf(_, 5.0)`: an integer exponent lowers to a
// multiply chain in registers, where `powf` is an opaque libm call that
// costs tens of cycles *and* acts as a barrier the surrounding vector code
// cannot be optimized across. Same idiom as the `powi(6)` in
// `fresnel_f82_tint` below. Fresnel is evaluated on every glossy lobe of
// every shading event, so this sits in the hottest loop in the renderer.
pub fn fresnel_schlick(cos_theta: f32, f0: Vec3A) -> Vec3A {
    f0 + (Vec3A::ONE - f0) * (1.0 - cos_theta).powi(5)
}

// Clearcoat Fresnel approx
pub fn fresnel_schlick_scalar(cos_theta: f32, f0: f32) -> f32 {
    f0 + (1.0 - f0) * (1.0 - cos_theta).powi(5)
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

// -------- EON (energy-preserving Oren-Nayar) diffuse --------
//
// Portsmouth et al., "EON: A practical energy-preserving rough diffuse
// BRDF" — the model the OpenPBR spec names for the base diffuse slab,
// following the formulation in Adobe's OpenPBR BSDF reference: Fujii's
// Oren-Nayar variant (single-scattering) plus an analytic
// multiple-scattering lobe that restores the energy the single-scattering
// term loses at high roughness. At `rho = 1` the total hemispherical albedo
// is exactly 1 for any roughness; at zero roughness it reduces to Lambert.

/// Fujii Oren-Nayar `A` constant: `1/(1 + A·r)` normalises the lobe.
const EON_A: f32 = 0.5 - 2.0 / (3.0 * PI);
/// Constant in the average (cosine-weighted) directional albedo
/// `Ē = (1 + B·r) / (1 + A·r)`.
const EON_B: f32 = 2.0 / 3.0 - 28.0 / (15.0 * PI);

/// Fujii Oren-Nayar directional albedo, exact closed form. (Reference for
/// the approximation below; used by tests to pin the fit.)
#[cfg(test)]
pub fn eon_albedo_exact(mu: f32, roughness: f32) -> f32 {
    let mu = mu.clamp(1e-4, 1.0);
    let af = 1.0 / (1.0 + EON_A * roughness);
    let bf = roughness * af;
    let si = (1.0 - mu * mu).max(0.0).sqrt();
    let g = si * (mu.acos() - si * mu) + (2.0 / 3.0) * ((si / mu) * (1.0 - si * si * si) - si);
    af + (bf / PI) * g
}

/// Fujii Oren-Nayar directional albedo, quartic polynomial fit (the runtime
/// form used by the Adobe reference; agrees with the exact form to ~1e-2).
pub fn eon_albedo_approx(mu: f32, roughness: f32) -> f32 {
    let mucomp = 1.0 - mu.clamp(0.0, 1.0);
    const G1: f32 = 0.057_108_53;
    const G2: f32 = 0.491_881_87;
    const G3: f32 = -0.332_181_44;
    const G4: f32 = 0.071_442_99;
    let g_over_pi = mucomp * (G1 + mucomp * (G2 + mucomp * (G3 + mucomp * G4)));
    (1.0 + roughness * g_over_pi) / (1.0 + EON_A * roughness)
}

/// EON BRDF value (no cosine). `rho` is the single-scattering albedo
/// (clamped to [0, 1] — the multiple-scattering series diverges beyond),
/// `v_local`/`l_local` are unit view/light directions in the tangent frame.
/// Reciprocal in `v`/`l`.
pub fn eon_diffuse(rho: Vec3A, roughness: f32, v_local: Vec3A, l_local: Vec3A) -> Vec3A {
    let rho = rho.clamp(Vec3A::ZERO, Vec3A::ONE);
    let mu_i = v_local.z;
    let mu_o = l_local.z;

    // Single-scattering Fujii lobe.
    let s = v_local.dot(l_local) - mu_i * mu_o;
    let s_over_t = if s > 0.0 {
        s / mu_i.max(mu_o).max(1e-6)
    } else {
        s
    };
    let af = 1.0 / (1.0 + EON_A * roughness);
    let f_ss = rho * (af / PI) * (1.0 + roughness * s_over_t);

    // Multiple-scattering compensation lobe: shaped like
    // (1 − E(μ_o))(1 − E(μ_i)) / (1 − Ē), scaled by the geometric-series
    // multi-bounce albedo ρ_ms — integrates to exactly the missing energy.
    let e_o = eon_albedo_approx(mu_o, roughness);
    let e_i = eon_albedo_approx(mu_i, roughness);
    let avg_e = af * (1.0 + EON_B * roughness);
    let rho_ms = (rho * rho) * avg_e / (Vec3A::ONE - rho * (1.0 - avg_e));
    const EPS: f32 = 1.0e-7;
    let f_ms = rho_ms
        * (1.0 / PI)
        * ((1.0 - e_o).max(EPS) * (1.0 - e_i).max(EPS) / (1.0 - avg_e).max(EPS));

    f_ss + f_ms
}

/// "F82-tint" conductor Fresnel (Kutz et al., as adopted by the OpenPBR
/// metal slab / MaterialX `generalized_schlick_bsdf` with `color0`/`color82`).
/// Plain Schlick pinned at `f0` for normal incidence and white at grazing,
/// with the reflectance at μ̄ = cos 82° ≈ 1/7 scaled by `tint` — the
/// `specular_color` edge tint:
///   `F(μ) = F_s(μ) − μ (1 − μ)⁶ · F_s(μ̄) (1 − tint) / (μ̄ (1 − μ̄)⁶)`
///
/// MaterialX's generalisation (free `f90` and exponent) is
/// `closure::mx::fresnel_hoffman_schlick`, a line-by-line GLSL port kept
/// separate; a fix to the formula here likely belongs there too.
pub fn fresnel_f82_tint(cos_theta: f32, f0: Vec3A, tint: Vec3A) -> Vec3A {
    const MU_BAR: f32 = 1.0 / 7.0;
    let mu = cos_theta.clamp(0.0, 1.0);
    let f_schlick = |m: f32| f0 + (Vec3A::ONE - f0) * (1.0 - m).powi(5);
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

/// Zeltner, Burley and Chiang's sheen ("Practical Multiple-Scattering Sheen
/// Using Linearly Transformed Cosines", 2022) for one view direction: the
/// linearly transformed cosine fitted to a volumetric layer of fibres, and
/// its directional albedo `R`. This is Adobe's OpenPBR fuzz lobe
/// (`impl/openpbr_fuzz_lobe.h`), transcribed operation for operation, with
/// Disney's "Volume" table ([`super::ltc_sheen_table`]).
///
/// All directions are in a z-up local frame with the view `wo` above the
/// plane. The value toward `wi` is `R · density(wi)`: the BRDF times the
/// cosine, which the LTC already contains. The density is the lobe's own pdf
/// and [`ZeltnerSheen::sample`] draws from it exactly, so every sample's
/// weight is `R`.
#[derive(Clone, Copy, Debug)]
pub struct ZeltnerSheen {
    a_inv: f32,
    b_inv: f32,
    r: f32,
}

impl ZeltnerSheen {
    /// The lobe at sheen roughness `roughness` (the LTC α, `√σ` of the
    /// fibres' SGGX cross-section, which OpenPBR's `fuzz_roughness` is) seen
    /// at `cos_theta_o`. Both are clamped into the table, as Adobe's array
    /// lookup clamps them; there is no roughness floor.
    pub fn new(roughness: f32, cos_theta_o: f32) -> Self {
        use super::ltc_sheen_table::{LTC_SHEEN_VOLUME as T, N};
        // Adobe's `OpenPBR_LargestFloatBelowOne`: keeps `i + 1` in the table.
        const BELOW_ONE: f32 = 0.999_999_94;
        let row = roughness.clamp(0.0, BELOW_ONE) * (N - 1) as f32;
        let col = cos_theta_o.clamp(0.0, BELOW_ONE) * (N - 1) as f32;
        let (r, c) = (row.floor(), col.floor());
        let (rf, cf) = (row - r, col - c);
        let (ri, ci) = (r as usize, c as usize);
        // GLSL's `mix`, `x·(1 − t) + y·t`, in Adobe's order.
        let mix = |x: [f32; 3], y: [f32; 3], t: f32| {
            [
                x[0] * (1.0 - t) + y[0] * t,
                x[1] * (1.0 - t) + y[1] * t,
                x[2] * (1.0 - t) + y[2] * t,
            ]
        };
        let at = |i: usize, j: usize| T[i * N + j];
        let k = mix(
            mix(at(ri, ci), at(ri, ci + 1), cf),
            mix(at(ri + 1, ci), at(ri + 1, ci + 1), cf),
            rf,
        );
        Self {
            a_inv: k[0],
            b_inv: k[1],
            r: k[2],
        }
    }

    /// The directional albedo `R`: how much of the light arriving over the
    /// hemisphere the lobe reflects toward `wo`.
    #[inline]
    pub fn albedo(&self) -> f32 {
        self.r
    }

    /// The LTC density toward `wi` (cosine included), which is both the
    /// lobe's shape and its sampling pdf. Zero below the plane or with `wo`
    /// at or below it.
    pub fn density(&self, wo: Vec3A, wi: Vec3A) -> f32 {
        if wo.z <= 0.0 || wi.z <= 0.0 {
            return 0.0;
        }
        // Into the frame where `wo` has azimuth 0, the one the LTC is fitted in.
        let wi = rotate_azimuth(wi, wo.x, -wo.y);
        let a_inv = self.a_inv;
        let original = Vec3A::new(a_inv * wi.x + self.b_inv * wi.z, a_inv * wi.y, wi.z);
        let z = wi.z.max(0.0);
        let len_squared = original.dot(original);
        if len_squared == 0.0 || a_inv == 0.0 || z == 0.0 {
            return 0.0;
        }
        // Adobe's factorisation, which keeps tiny grazing densities
        // representable: `(1/π) · z · a⁻² / |M⁻¹ wi|⁴`.
        let a_inv_over_len_squared = a_inv / len_squared;
        // Adobe's `OpenPBR_RcpPi` literal rounds to this `f32`.
        std::f32::consts::FRAC_1_PI * (z * a_inv_over_len_squared) * a_inv_over_len_squared
    }

    /// A direction drawn with [`ZeltnerSheen::density`] from the 2D sample
    /// `u`, or `None` when it falls below the plane (Adobe's failed sample) or
    /// the lobe has no extent there (`a⁻¹ = 0`, where the table holds `R = 0`).
    pub fn sample(&self, wo: Vec3A, u: [f32; 2]) -> Option<Vec3A> {
        if wo.z <= 0.0 || self.a_inv == 0.0 {
            return None;
        }
        // Adobe's `openpbr_sample_unit_hemisphere_cosine`.
        let phi = 2.0 * PI * u[0];
        let z = u[1].sqrt();
        let s = (1.0 - u[1]).sqrt();
        let original = Vec3A::new(phi.cos() * s, phi.sin() * s, z);
        let a = 1.0 / self.a_inv;
        let wi = Vec3A::new(
            original.x * a - original.z * self.b_inv * a,
            original.y * a,
            original.z,
        )
        .normalize();
        let wi = rotate_azimuth(wi, wo.x, wo.y).normalize();
        (wi.z > 0.0).then_some(wi)
    }
}

/// Rotates `v` about +z by the azimuth of `(x, y)`; `v` itself when that is
/// the zero vector. Adobe's `openpbr_disney_sheen_rotate_vector`.
fn rotate_azimuth(v: Vec3A, x: f32, y: f32) -> Vec3A {
    let r2 = x * x + y * y;
    if r2 == 0.0 {
        return v;
    }
    let inv_r = 1.0 / r2.sqrt();
    let (sin_phi, cos_phi) = (y * inv_r, x * inv_r);
    Vec3A::new(
        cos_phi * v.x + sin_phi * -v.y,
        cos_phi * v.y + sin_phi * v.x,
        v.z,
    )
}

/// The darkening a physical coat produces on the base beneath it through
/// internal reflection — the OpenPBR spec's closed form.
///
/// Light the base reflects back up meets the coat's underside, where a
/// fraction `K̄` returns down (Fresnel plus, dominantly, total internal
/// reflection) and `1 − K̄` escapes; summing the bounces against a base of
/// albedo `E` gives an effective albedo `E·(1 − K̄)/(1 − K̄·E)`, i.e. a
/// factor `Δ = (1 − K̄)/(1 − K̄·E)` on the base. `K̄` is the coat underside's
/// hemispherical reflectance, `1 − (1 − F0)/η²` — the `1/η²` is the TIR
/// cone, which is why it is ~0.57 at η = 1.5 and not the ~0.04 of the outer
/// Fresnel. A white base is not darkened at all (`Δ = 1`); a dark one is
/// darkened toward `1 − K̄`, never toward zero — the factor is a *ratio*,
/// applied on top of the base colour the lobes already carry, not a darkened
/// albedo (an earlier form here returned `E/(1 − K̄(1 − E))`, which tends to
/// `E` for dark bases and so applied the base colour twice).
///
/// `coat_weight` fades the factor for partial coverage and `darkening ∈
/// [0, 1]` is the artistic dial between "no darkening" and full physics,
/// both as `1 + coat_weight·darkening·(Δ − 1)`.
pub fn coat_darkening_factor(
    base_color: Vec3A,
    coat_ior: f32,
    coat_weight: f32,
    darkening: f32,
) -> Vec3A {
    let f0 = f0_from_ior(coat_ior);
    let eta = coat_ior.max(1.0);
    let k = 1.0 - (1.0 - f0) / (eta * eta);
    let e = base_color.clamp(Vec3A::ZERO, Vec3A::ONE);
    let delta = (1.0 - k) / (Vec3A::ONE - k * e).max(Vec3A::splat(1e-4));
    let t = (coat_weight * darkening).clamp(0.0, 1.0);
    Vec3A::ONE + (delta - Vec3A::ONE) * t
}

// -------- Thin-film interference (Belcour & Barla 2017, simplified) --------
//
// Three-layer Airy-summation reflectance for a single dielectric film of
// thickness `d` (nm) and index `η_film` sandwiched between an outer medium
// of index `η_1` and a base of index `η_2`. Evaluated at the CIE sRGB
// primaries (R = 615 nm, G = 545 nm, B = 465 nm) — a 3-wavelength
// approximation that captures the characteristic soap-bubble / oil-slick
// look without full spectral rendering.
/// Representative wavelengths (nm) of the sRGB primaries, shared by the
/// thin-film interference and dispersion models so per-channel spectral
/// effects stay consistent.
pub const LAMBDA_RGB: [f32; 3] = [615.0, 545.0, 465.0];

// -------- Physical dispersion (Cauchy / Abbe) --------
//
// Following Adobe's OpenPBR BSDF reference (openpbr_dispersion_utils.h): a
// dielectric is specified by its index n_d at the Fraunhofer d line plus an
// Abbe number V_d = (n_d − 1)/(n_F − n_C), and the wavelength dependence is
// reconstructed with the two-term Cauchy equation n(λ) = A + B/λ² — the
// best fit available without partial-dispersion data.

/// Fraunhofer C line (hydrogen), 656.3 nm — the long/red reference.
pub const FRAUNHOFER_C_NM: f32 = 656.3;
/// Fraunhofer d line (helium), 587.6 nm — where `n_d` is defined.
pub const FRAUNHOFER_D_NM: f32 = 587.6;
/// Fraunhofer F line (hydrogen), 486.1 nm — the short/blue reference.
pub const FRAUNHOFER_F_NM: f32 = 486.1;

/// Cauchy-fit refractive index at `lambda_nm` for a dielectric with index
/// `n_d` (> 1) at the d line and Abbe number `v_d`. A and B are chosen so
/// that `n(λ_d) = n_d` exactly and the fit's Abbe number is exactly `v_d`.
pub fn cauchy_ior(n_d: f32, v_d: f32, lambda_nm: f32) -> f32 {
    let b = (n_d - 1.0)
        / (v_d
            * (1.0 / (FRAUNHOFER_F_NM * FRAUNHOFER_F_NM)
                - 1.0 / (FRAUNHOFER_C_NM * FRAUNHOFER_C_NM)));
    let a = n_d - b / (FRAUNHOFER_D_NM * FRAUNHOFER_D_NM);
    a + b / (lambda_nm * lambda_nm)
}

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

#[cfg(test)]
mod zeltner_tests {
    use super::*;

    /// The midpoint nodes and solid-angle weights of the upper hemisphere:
    /// the midpoint rule in `(t, φ)` with `cos θ = t²`, which crowds the
    /// nodes toward the horizon where a low-roughness sheen lives.
    fn nodes() -> impl Iterator<Item = (Vec3A, f64)> {
        const NT: usize = 1024;
        const NP: usize = 512;
        (0..NT * NP).map(|k| {
            let t = ((k / NP) as f32 + 0.5) / NT as f32;
            let cos = t * t;
            let sin = (1.0 - cos * cos).max(0.0).sqrt();
            let phi = 2.0 * PI * ((k % NP) as f32 + 0.5) / NP as f32;
            // dω = d(cos θ) dφ = 2t dt dφ.
            let dw = 2.0 * t as f64 / NT as f64 * (2.0 * std::f64::consts::PI / NP as f64);
            (Vec3A::new(sin * phi.cos(), sin * phi.sin(), cos), dw)
        })
    }

    /// `∫ g(ω) dω` over the upper hemisphere.
    fn integrate(g: impl Fn(Vec3A) -> f32) -> f64 {
        nodes().map(|(w, dw)| g(w) as f64 * dw).sum()
    }

    fn view(cos: f32) -> Vec3A {
        // Off the x axis, so the azimuth rotation is exercised.
        let s = (1.0 - cos * cos).max(0.0).sqrt();
        Vec3A::new(s * 0.6, s * 0.8, cos)
    }

    /// Stratified 2D samples on a `n × n` grid.
    fn grid(n: usize) -> impl Iterator<Item = [f32; 2]> {
        (0..n * n).map(move |k| {
            [
                ((k / n) as f32 + 0.5) / n as f32,
                ((k % n) as f32 + 0.5) / n as f32,
            ]
        })
    }

    const ROUGHNESS: [f32; 7] = [0.0, 0.05, 0.1, 0.3, 0.5, 0.8, 1.0];
    const COS: [f32; 4] = [0.05, 0.25, 0.5, 1.0];

    /// R at the points the materials spec states.
    #[test]
    fn the_albedo_is_the_tables() {
        let r = |a: f32, c: f32| ZeltnerSheen::new(a, c).albedo();
        assert!((r(0.3, 1.0) - 0.0008).abs() < 2e-4, "{}", r(0.3, 1.0));
        assert!((r(0.3, 0.25) - 0.166).abs() < 2e-3, "{}", r(0.3, 0.25));
        assert!((r(1.0, 1.0) - 0.342).abs() < 2e-3, "{}", r(1.0, 1.0));
    }

    /// The density is a pdf: its mass above the plane is the probability that
    /// a sample lands there, and at most 1 (an LTC can shear some of its mass
    /// below the plane, where a sample fails).
    #[test]
    fn the_density_is_the_samplers_pdf() {
        for a in ROUGHNESS {
            for c in COS {
                let lobe = ZeltnerSheen::new(a, c);
                if lobe.albedo() == 0.0 {
                    continue;
                }
                let wo = view(c);
                let mass = integrate(|wi| lobe.density(wo, wi));
                let n = 256;
                let landed =
                    grid(n).filter_map(|u| lobe.sample(wo, u)).count() as f64 / (n * n) as f64;
                assert!(mass <= 1.0 + 2e-3, "α {a} μ {c}: mass {mass}");
                assert!(
                    (mass - landed).abs() < 5e-3,
                    "α {a} μ {c}: mass {mass} but {landed} of the samples land"
                );
            }
        }
    }

    /// Samples fall where the density says: a coarse histogram over
    /// `(cos θ, φ)` against the density's integral over each bin.
    #[test]
    fn samples_follow_the_density() {
        const BT: usize = 8;
        const BP: usize = 16;
        let bin = |w: Vec3A| {
            let t = w.z.max(0.0).sqrt().min(0.999_999);
            let phi = w.y.atan2(w.x).rem_euclid(2.0 * PI);
            (t * BT as f32) as usize * BP + ((phi / (2.0 * PI) * BP as f32) as usize).min(BP - 1)
        };
        for a in [0.1, 0.3, 0.6, 1.0] {
            for c in [0.25, 0.7] {
                let lobe = ZeltnerSheen::new(a, c);
                let wo = view(c);
                let n = 256;
                let total = (n * n) as f64;
                let mut counts = vec![0.0f64; BT * BP];
                for wi in grid(n).filter_map(|u| lobe.sample(wo, u)) {
                    counts[bin(wi)] += 1.0 / total;
                }
                let mut wants = vec![0.0f64; BT * BP];
                for (wi, dw) in nodes() {
                    wants[bin(wi)] += lobe.density(wo, wi) as f64 * dw;
                }
                for (k, (&got, &want)) in counts.iter().zip(&wants).enumerate() {
                    assert!(
                        (got - want).abs() < 4e-3 + 0.02 * want,
                        "α {a} μ {c} bin {k}: sampled {got:.5}, density {want:.5}"
                    );
                }
            }
        }
    }

    /// The value is `R · density` and the density is the pdf, so a sample's
    /// weight is exactly `R`: no variance from the lobe's shape.
    #[test]
    fn every_sample_weighs_the_albedo() {
        for a in ROUGHNESS {
            for c in COS {
                let lobe = ZeltnerSheen::new(a, c);
                let wo = view(c);
                for wi in grid(16).filter_map(|u| lobe.sample(wo, u)) {
                    let p = lobe.density(wo, wi);
                    if p > 0.0 {
                        let w = lobe.albedo() * p / p;
                        assert!(
                            (w - lobe.albedo()).abs() <= 1e-6 * lobe.albedo(),
                            "α {a} μ {c}"
                        );
                    }
                }
            }
        }
    }

    /// A white sheen alone in a white furnace reflects `R` times the mass the
    /// LTC keeps above the plane — never more than `R`, never more than 1 —
    /// down to roughness 0. So crust needs no roughness floor (BSDL clamps at
    /// 0.02 because its sampled albedo came from a separate table; here the
    /// weight and the value share `R`).
    #[test]
    fn a_sheen_never_gains_energy() {
        for a in [0.0, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.4, 0.7, 1.0] {
            for c in [0.01, 0.05, 0.1, 0.25, 0.5, 0.75, 1.0] {
                let lobe = ZeltnerSheen::new(a, c);
                let wo = view(c);
                let albedo = integrate(|wi| lobe.albedo() * lobe.density(wo, wi));
                assert!(
                    albedo <= lobe.albedo() as f64 + 2e-3,
                    "α {a} μ {c}: {albedo}"
                );
                assert!(albedo <= 1.0, "α {a} μ {c}: {albedo}");
            }
        }
    }
}
