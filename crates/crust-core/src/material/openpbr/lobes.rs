//! The reflection lobes: the local shading frame, the discrete lobe pmf, each
//! lobe's evaluation (diffuse, specular, coat, fuzz) and the mixture pdf.

use std::f32::consts::PI;

use glam::Vec3A;
use utils::luminance;

use crate::material::brdf::*;

use super::OpenPBR;
use super::transmission::{eval_transmission, transmission_is_continuous};

// ---------------------------------------------------------------------------
// Internal shading state
// ---------------------------------------------------------------------------

/// A local shading frame plus cached view / half / light vectors in world
/// space. Kept small so it can move by value.
pub(super) struct Frame {
    pub(super) n: Vec3A,
    pub(super) t: Vec3A,
    pub(super) b: Vec3A,
}

impl Frame {
    pub(super) fn new(n: Vec3A) -> Self {
        let (t, b) = tangent_frame(n);
        Self { n, t, b }
    }
    pub(super) fn to_local(&self, v: Vec3A) -> Vec3A {
        to_tangent(v, self.t, self.b, self.n)
    }
    pub(super) fn to_world(&self, v_local: Vec3A) -> Vec3A {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Lobe {
    Diffuse,
    Specular,
    Coat,
    Fuzz,
    Transmission,
}

impl Lobe {
    /// Every lobe, in selection order: [`LobePmf::pick`]'s CDF runs over
    /// them in this order.
    pub(super) const ALL: [Lobe; 5] = [
        Lobe::Diffuse,
        Lobe::Specular,
        Lobe::Coat,
        Lobe::Fuzz,
        Lobe::Transmission,
    ];
}

/// The probability of selecting each [`Lobe`], indexed by it — one slot per
/// variant, so adding a lobe cannot leave the pmf without an entry for it.
pub(super) struct LobePmf([f32; Lobe::ALL.len()]);

impl std::ops::Index<Lobe> for LobePmf {
    type Output = f32;

    #[inline(always)]
    fn index(&self, lobe: Lobe) -> &f32 {
        &self.0[lobe as usize]
    }
}

impl LobePmf {
    /// Fresnel-averaged energy heuristic. Not perfect, but stable and cheap
    /// — the mixture PDF gets the direction right regardless of the exact
    /// per-lobe weights we sample by.
    pub(super) fn from_params(m: &OpenPBR) -> Self {
        let f0_diel = f0_from_ior(m.specular_ior);
        let f0_coat = f0_from_ior(m.coat_ior);
        let base_luma = luminance(m.base_color).max(0.02);
        let spec_luma = luminance(m.specular_color).max(0.02);
        let fuzz_luma = luminance(m.fuzz_color).max(0.02);

        // Metal reflectivity is base_color · base_weight, covered by
        // base_metalness. No `specular_weight` here, matching `eval_specular`:
        // a pure metal carries no dielectric interface, so its
        // `specular_weight` is legitimately 0 and weighting by it would leave
        // the specular lobe essentially unsampled while `eval_all` still
        // returned its full energy — fireflies on every metal.
        let w_metal = m.base_metalness * luminance(m.base_color * m.base_weight).max(0.02);
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
        //
        // KNOWN ISSUE (variance, not bias). These floors keep `total > 0` for
        // a fully black material, but they also keep an *absent* lobe in the
        // mixture: at `coat_weight == 0` the coat still holds
        // `p_coat ≈ 1e-6/total` of the selection mass, and because the
        // default `coat_roughness` of 0 floors its α at 1e-4, `pdf_coat`
        // reaches ~3e7 near the mirror direction. So a zero-energy lobe both
        // gets picked one sample in a million (contributing black) and
        // inflates the mixture density that every specular sample divides by.
        // The estimator stays unbiased — sampling really does have that
        // density — but samples are wasted and near-mirror specular is
        // slightly darkened. The fix is to set absent weights to exactly 0.0
        // and guard `total` instead; that changes the image, so it needs a
        // converged A/B against `eval_matches_scatter_importance` rather than
        // being folded into a bit-identical change.
        let w_coat = (m.coat_weight * f0_coat).max(1e-6);
        let w_fuzz = (m.fuzz_weight * fuzz_luma).max(1e-6);

        // Transmission: dominant when weight is high. When enabled it
        // steals energy from the dielectric-specular / diffuse pathway.
        let trans_luma = luminance(m.transmission_color).max(0.02);
        let w_transmission = if m.transmission_weight > 0.0 {
            ((1.0 - m.base_metalness) * m.transmission_weight * trans_luma).max(1e-4)
        } else {
            0.0
        };

        let total = w_diffuse + w_specular + w_coat + w_fuzz + w_transmission;
        // In `Lobe::ALL` order.
        Self([
            w_diffuse / total,
            w_specular / total,
            w_coat / total,
            w_fuzz / total,
            w_transmission / total,
        ])
    }

    /// The lobe whose slice of the CDF `u` falls in; the last lobe takes
    /// whatever rounding leaves above the running sum.
    pub(super) fn pick(&self, u: f32) -> Lobe {
        let (last, rest) = Lobe::ALL.split_last().expect("five lobes");
        // `0.0 + p` is `p` exactly, so this is the running sum it always was.
        let mut acc = 0.0;
        for &lobe in rest {
            acc += self[lobe];
            if u < acc {
                return lobe;
            }
        }
        *last
    }
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

    // The presence weights scale the entire lobe, so when they cancel it
    // there is nothing to evaluate. Skipping is exact rather than
    // approximate: `eon_diffuse` at ρ = 0 returns `f_ss + f_ms` where both
    // terms are products containing ρ, so both are ±0 and their sum is +0.
    // Worth the branch — it removes two quartic albedo fits and a `Vec3A`
    // divide for every pure metal and every fully transmissive surface.
    let presence = m.base_weight * (1.0 - m.base_metalness) * (1.0 - m.transmission_weight);
    if presence <= 0.0 {
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
    let rho = diffuse_color * presence;

    // Energy left after specular reflection: `1 - F_dielectric_avg`. Using
    // the directional Fresnel here would double-count with the specular
    // lobe's own Fresnel — the average avoids that.
    eon_diffuse(rho, m.base_diffuse_roughness, v_local, l_local) * (1.0 - f_avg_diel)
}

/// The base specular lobe, returned as `(dielectric, metal)` rather than their
/// sum because the coat treats them differently: the substrate-albedo darkening
/// applies to the metal slab and not to the white dielectric interface. See
/// [`coat_darkening`]. Callers with no coat simply add the two.
pub(super) fn eval_specular(
    m: &OpenPBR,
    v_local: Vec3A,
    l_local: Vec3A,
    h_local: Vec3A,
    ax: f32,
    ay: f32,
) -> (Vec3A, Vec3A) {
    let n_dot_v = v_local.z.max(1e-4);
    let n_dot_l = l_local.z.max(1e-4);
    let n_dot_h = h_local.z.max(1e-4);
    let v_dot_h = v_local.dot(h_local).max(1e-4);

    let d = ggx_d_aniso(n_dot_h, h_local.x, h_local.y, ax, ay);
    let g = ggx_g2_smith_aniso(
        n_dot_v, v_local.x, v_local.y, n_dot_l, l_local.x, l_local.y, ax, ay,
    );

    let f0_diel_scalar = f0_from_ior(m.specular_ior);

    // OpenPBR spec places thin-film between coat and base; when there is
    // no coat the outer medium is air.
    let outer_ior = if m.coat_weight > 0.0 { m.coat_ior } else { 1.0 };
    // OpenPBR thickness is in μm; the thin-film helpers want nm.
    let tf_thickness_nm = m.thin_film_thickness * 1000.0;

    // `base_metalness` blends two *independent* Fresnel models, so at either
    // end of the blend one of them is multiplied by exactly zero. Computing
    // it anyway is what made a plain diffuse surface pay for the whole F82
    // metal chain (three `powi` on `Vec3A`) and a pure metal pay for the
    // thin-film/thin-wall dielectric chain. Skipping is bit-identical: both
    // Fresnels are non-negative and finite (`fresnel_f82_tint` clamps to
    // [0,1]), so the dropped term is exactly +0.0 and `x + 0.0 == x`.

    // Dielectric-specular path: F_dielectric · brdf · (1 − metalness) ·
    // specular_weight.
    //
    // `specular_weight` scales the finished lobe and is deliberately *not*
    // folded into F0. Schlick is `F0 + (1 − F0)(1 − cosθ)⁵`, so an F0 of zero
    // still returns 1.0 at grazing: scaling F0 left a surface with no specular
    // interface at all — `OpenPBR::diffuse()`, and every MaterialX body whose
    // dielectrics were all promoted to the coat — emitting a full-strength
    // white glossy rim at the fallback roughness. Scaling the term instead
    // makes the lobe linear in the weight, which is what the parameter means.
    let diel_term = if m.base_metalness < 1.0 {
        let f0_diel_base = m.specular_color * f0_diel_scalar;

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

        // Thin-walled window reflectance: the transmissive fraction of a thin
        // sheet reflects from both surfaces including all internal bounces —
        // `R_window = 2R/(1+R)` (Adobe reference `openpbr_thin_wall_fresnel`)
        // instead of the single-interface `R`. Scale the dielectric Fresnel by
        // the physical boost `2/(1+R)`, blended by how transmissive the sheet
        // is; together with the `(1−R)/(1+R)` window transmittance a clear
        // sheet reflects + transmits exactly unit energy.
        let f_diel = if m.geometry_thin_walled && m.transmission_weight > 0.0 {
            let f_phys = fresnel_schlick_scalar(v_dot_h, f0_diel_scalar);
            let boost = 2.0 / (1.0 + f_phys);
            f_diel * (1.0 + (boost - 1.0) * m.transmission_weight)
        } else {
            f_diel
        };
        f_diel * (1.0 - m.base_metalness) * m.specular_weight
    } else {
        Vec3A::ZERO
    };

    // Metal slab per the MaterialX reference `generalized_schlick_bsdf`:
    // F0 = base_color · base_weight, F82 edge tint = specular_color, coverage
    // from base_metalness — with its own thin-film variant (`metal_bsdf_tf`)
    // blended in by thin_film_weight.
    //
    // Deliberately *not* scaled by `specular_weight`. That parameter belongs to
    // the dielectric base — it is how much of that base carries a specular
    // interface — and a metal has no dielectric interface to weigh. While both
    // halves were scaled by it the two could not hold independent coverage,
    // which is exactly what the MaterialX reduction needs: a conductor and a
    // dielectric mixed at unequal weights had each multiply the other, and a
    // mix with no diffuse under it pinned `base_metalness` to 1 and dropped
    // the dielectric half outright.
    let metal_term = if m.base_metalness > 0.0 {
        let metal_f0 = m.base_color * m.base_weight;
        let f_metal_base = fresnel_f82_tint(v_dot_h, metal_f0, m.specular_color);
        let f_metal = if m.thin_film_weight > 0.0 {
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
        };
        f_metal * m.base_metalness
    } else {
        Vec3A::ZERO
    };

    let brdf = d * g / (4.0 * n_dot_v * n_dot_l);
    (diel_term * brdf, metal_term * brdf)
}

pub(super) fn eval_coat(
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

/// One-way passage factor through the coat for a direction making cosine
/// `cos_theta` with the normal — the Adobe reference's
/// `openpbr_coat_passage_color_multiplier × (1 − proportion reflected)`.
/// The authored `coat_color` is defined as the *round-trip* absorption at
/// normal incidence, so a single passage applies `√coat_color` raised to
/// the relative in-coat path length `1/cos θ_refracted` (Snell at the coat
/// IOR — slanted passages absorb more). The Fresnel term removes the
/// energy the coat interface reflected away from this direction's passage.
/// Both terms fade with `coat_weight` for partial coverage.
pub(super) fn coat_passage(m: &OpenPBR, cos_theta: f32) -> Vec3A {
    let cos_i = cos_theta.clamp(1e-4, 1.0);
    // Refracted angle inside the coat.
    let eta = m.coat_ior.max(1e-4);
    let sin2_t = (1.0 - cos_i * cos_i) / (eta * eta);
    let cos_t = (1.0 - sin2_t).max(0.0).sqrt();
    let path_length = 1.0 / cos_t.max(1e-3);

    let one_passage = m
        .coat_color
        .clamp(Vec3A::ZERO, Vec3A::ONE)
        .powf(0.5 * path_length);
    let absorb = Vec3A::ONE.lerp(one_passage, m.coat_weight);

    let f_coat = fresnel_schlick_scalar(cos_i, f0_from_ior(m.coat_ior));
    absorb * (1.0 - m.coat_weight * f_coat)
}

/// Directional attenuation the coat imposes on the layers beneath it:
/// light passes through the coat twice — in along the view direction, out
/// along the light direction — and each passage pays its own
/// Fresnel-weighted transmission and view-dependent absorption
/// (`coat_passage`). Applied as a per-channel multiplier to **everything**
/// under the coat. This is the incoming × outgoing base-layer scale of the
/// Adobe reference coating lobe; at normal incidence the two passages
/// recover exactly the authored round-trip `coat_color`.
///
/// The multi-bounce darkening is deliberately *not* here — see
/// [`coat_darkening`], which `eval_all` applies to a narrower set of lobes.
pub(super) fn coat_attenuation(m: &OpenPBR, cos_v: f32, cos_l: f32) -> Vec3A {
    if m.coat_weight <= 0.0 {
        return Vec3A::ONE;
    }
    coat_passage(m, cos_v) * coat_passage(m, cos_l)
}

/// The coat's multi-bounce darkening factor Δ, or `ONE` when there is no coat.
///
/// Kept apart from [`coat_attenuation`] because the two apply to different
/// things. The `(1 − F)` passage is geometry: every photon reaching the
/// substrate pays it, whatever lobe it then meets. Δ is a *substrate albedo*
/// term — it is derived from `base_color` — so it belongs only to the lobes
/// whose reflectance `base_color` actually describes.
///
/// Concretely it must not touch the base **dielectric** lobe. That lobe's
/// reflectance is ~4% and white; multiplying it by a Δ computed from a
/// saturated `base_color` both dimmed and tinted it, turning a clearcoated red
/// plastic's white highlight into a dim pink one. It does still apply to the
/// metal lobe, where `base_color` *is* the slab's F0
/// (`metal_f0 = base_color · base_weight`) and the bounce series is genuinely
/// that colour, and to emission, which originates inside the substrate.
pub(super) fn coat_darkening(m: &OpenPBR) -> Vec3A {
    if m.coat_weight <= 0.0 {
        return Vec3A::ONE;
    }
    coat_darkening_factor(m.base_color, m.coat_ior, m.coat_weight, m.coat_darkening)
}

fn eval_fuzz(m: &OpenPBR, v_local: Vec3A, l_local: Vec3A, h_local: Vec3A) -> Vec3A {
    let n_dot_v = v_local.z.max(1e-4);
    let n_dot_l = l_local.z.max(1e-4);
    let n_dot_h = h_local.z.max(0.0);
    m.fuzz_color * m.fuzz_weight * sheen_charlie(n_dot_v, n_dot_l, n_dot_h, m.fuzz_roughness)
}

pub(super) fn eval_all(m: &OpenPBR, v_local: Vec3A, l_local: Vec3A, entering: bool) -> Vec3A {
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
    let f_avg_diel = f0_from_ior(m.specular_ior);

    let diffuse = eval_diffuse(m, v_local, l_local, f_avg_diel);
    // Skippable only when *neither* half can contribute: the dielectric ends in
    // a multiply by `specular_weight` and the metal in one by `base_metalness`,
    // so both must be zero for the call to be exactly +0.0 — the same
    // bit-identity argument as the coat and fuzz below. Testing
    // `specular_weight` alone would silently shade a pure metal black, since
    // that is precisely the material with no dielectric interface to weigh.
    // Worth the branch: every unbound prim and every `UsdPreviewSurface`-less
    // material reduces to `OpenPBR::diffuse()`, which is the whole of
    // cornellbox and Kitchen_set.
    let (spec_diel, spec_metal) = if m.specular_weight > 0.0 || m.base_metalness > 0.0 {
        eval_specular(m, v_local, l_local, h_local, ax, ay)
    } else {
        (Vec3A::ZERO, Vec3A::ZERO)
    };

    // Absent layers are skipped, not multiplied by zero. Both lobes end in a
    // multiply by their weight, and both are finite for every input — every
    // GGX denominator is floored (`roughness_to_alpha_aniso` clamps α ≥ 1e-4,
    // the cosines at 1e-4) and `sheen_charlie` is bounded because
    // `sheen_charlie_d` clamps α ≥ 0.05, so its `powf` argument stays in
    // [0,1]. A finite value times 0.0 is exactly +0.0, and `+0.0 + x == x`,
    // so this is bit-identical rather than merely close.
    //
    // It is also where most of the default material's cost was: `eval_fuzz`
    // holds the only unconditional `powf` on that path (85M instructions on
    // cornellbox, one call per `eval_all`), and `eval_coat` a full
    // anisotropic GGX D + Smith G2 with two `sqrt`s — both for `fuzz_weight`
    // and `coat_weight` of zero, which is the default.
    let coat = if m.coat_weight > 0.0 {
        let (ax_coat, ay_coat) =
            roughness_to_alpha_aniso(m.coat_roughness, m.coat_roughness_anisotropy);
        eval_coat(m, v_local, l_local, h_local, ax_coat, ay_coat)
    } else {
        Vec3A::ZERO
    };
    let fuzz = if m.fuzz_weight > 0.0 {
        eval_fuzz(m, v_local, l_local, h_local)
    } else {
        Vec3A::ZERO
    };

    // Layered composition (top→bottom): fuzz over coat over base.
    //  throughput = fuzz + (1 - fuzz_weight) ·
    //               (coat + coat_atten · (dark · (diffuse + metal) + diel))
    // The coat attenuation is per-direction (view in, light out), evaluated
    // against the normal cosines, not the half-vector, and every lobe under the
    // coat pays it. The substrate-albedo darkening `dark` is narrower: it is
    // derived from `base_color`, so it applies to the lobes that colour
    // describes — diffuse and the metal slab — and not to the base dielectric
    // interface, whose ~4% reflectance is white. See `coat_darkening`.
    let coat_atten = coat_attenuation(m, v_local.z, l_local.z);
    let dark = coat_darkening(m);
    let base_atten = (1.0 - m.fuzz_weight).clamp(0.0, 1.0);
    fuzz + base_atten * (coat + coat_atten * (dark * (diffuse + spec_metal) + spec_diel))
}

// ---------------------------------------------------------------------------
// Mixture PDF: p(l) = Σ p_lobe · pdf_lobe(l)
// ---------------------------------------------------------------------------

pub(super) fn pdf_all(
    m: &OpenPBR,
    pmf: &LobePmf,
    v_local: Vec3A,
    l_local: Vec3A,
    entering: bool,
) -> f32 {
    if v_local.z <= 0.0 {
        return 0.0;
    }
    if l_local.z <= 0.0 {
        if !transmission_is_continuous(m) {
            return 0.0;
        }
        return pmf[Lobe::Transmission] * eval_transmission(m, v_local, l_local, entering).1;
    }
    let h_local = (v_local + l_local).normalize();

    let (ax, ay) = roughness_to_alpha_aniso(m.specular_roughness, m.specular_roughness_anisotropy);
    let (ax_coat, ay_coat) =
        roughness_to_alpha_aniso(m.coat_roughness, m.coat_roughness_anisotropy);

    // Diffuse and fuzz are both sampled cosine-weighted, so their densities
    // are the same expression — evaluate it once. (Not folded into
    // `(p_diffuse + p_fuzz) * cosine`: `a·x + b·x` and `(a+b)·x` differ in
    // float, and the point here is to keep the value identical.)
    let pdf_cosine = l_local.z.max(0.0) / PI;
    let pdf_specular = pdf_vndf_ggx_aniso_local(v_local, h_local, ax, ay);
    // The coat density is *not* skippable the way `eval_coat` is: even at
    // `coat_weight == 0`, `LobePmf::from_params` floors the coat's selection
    // weight at 1e-6, so `p_coat` is nonzero and this term genuinely belongs
    // in the mixture density that sampling divides by. Dropping it would
    // change the image — see the note in `from_params`.
    let pdf_coat = pdf_vndf_ggx_aniso_local(v_local, h_local, ax_coat, ay_coat);

    pmf[Lobe::Diffuse] * pdf_cosine
        + pmf[Lobe::Specular] * pdf_specular
        + pmf[Lobe::Coat] * pdf_coat
        + pmf[Lobe::Fuzz] * pdf_cosine
}
