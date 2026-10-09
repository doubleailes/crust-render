use std::f32::consts::PI;

use glam::Vec3A;

use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::material::Material;
use crate::medium::Medium;
use crate::ray::Ray;

use super::lobes::*;
use super::transmission::*;
use super::*;

/// A tiny test helper yielding a fresh QMC sampler domain per draw, so a
/// loop of `scatter_importance` calls sees independent samples (the
/// production integrator derives an analogous per-vertex domain).
struct S(i32);
impl S {
    fn next(&mut self) -> PathSampler {
        self.0 += 1;
        PathSampler::new(0, 0, 0, self.0)
    }
}
fn s() -> S {
    S(0)
}

#[test]
fn eval_matches_scatter_importance() {
    let m = OpenPBR::default();
    let mut sampler = s();
    let mut rec = HitRecord::new();
    rec.p = Vec3A::ZERO;
    rec.normal = Vec3A::Z;
    rec.front_face = true;
    let r_in = Ray::new(
        Vec3A::new(0.3, -0.2, 1.0),
        Vec3A::new(-0.3, 0.2, -1.0).normalize(),
    );

    let mut checked = 0;
    for _ in 0..128 {
        if let Some(sample) = m.scatter_importance(&r_in, &rec, sampler.next()) {
            assert!(!sample.delta, "opaque OpenPBR has no delta lobe");
            let wi = sample.ray.direction().normalize();
            let (ev, epdf) = m
                .eval(&r_in, &rec, wi)
                .expect("opaque OpenPBR is evaluable");
            let tol = 1e-3 * (1.0 + sample.value.max_element().abs());
            assert!(
                (ev - sample.value).abs().max_element() < tol,
                "{ev} vs {:?}",
                sample.value
            );
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
    let r_in = Ray::new(
        Vec3A::new(0.3, -0.2, 1.0),
        Vec3A::new(-0.3, 0.2, -1.0).normalize(),
    );

    let (mut transmitted, mut reflected) = (0, 0);
    for _ in 0..256 {
        if let Some(sample) = m.scatter_importance(&r_in, &rec, sampler.next()) {
            assert!(
                !sample.delta,
                "thick non-dispersive glass has no delta lobe"
            );
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
            assert!(w.is_finite() && (0.0..10.0).contains(&w), "weight {w}");
        }
    }
    assert!(
        transmitted > 64,
        "glass should mostly refract: {transmitted}"
    );
    assert!(reflected >= 0);

    // Near-smooth glass: pointwise agreement degrades to float noise but
    // the estimator weight value/pdf must stay bounded and the pdfs must
    // agree within a loose relative tolerance.
    let smooth = OpenPBR::glass(1.5);
    for _ in 0..128 {
        if let Some(sample) = smooth.scatter_importance(&r_in, &rec, sampler.next()) {
            let wi = sample.ray.direction().normalize();
            let (_, epdf) = smooth.eval(&r_in, &rec, wi).expect("evaluable");
            assert!(
                (epdf - sample.pdf).abs() < 0.05 * (1.0 + sample.pdf),
                "{epdf} vs {}",
                sample.pdf
            );
            let w = (sample.value / sample.pdf).max_element();
            assert!(w.is_finite() && (0.0..10.0).contains(&w), "weight {w}");
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
    let expected = refract_dir(-dir_in, Vec3A::Z, 1.0 / 1.5)
        .unwrap()
        .normalize();

    let mut checked = 0;
    for _ in 0..128 {
        if let Some(sample) = m.scatter_importance(&r_in, &rec, sampler.next()) {
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
    let r_in = Ray::new(
        Vec3A::new(0.3, -0.2, 1.0),
        Vec3A::new(-0.3, 0.2, -1.0).normalize(),
    );
    let mut deltas = 0;
    for _ in 0..128 {
        if let Some(sample) = m.scatter_importance(&r_in, &rec, sampler.next())
            && sample.ray.direction().z < 0.0
        {
            assert!(sample.delta, "thin-walled transmission must stay delta");
            deltas += 1;
        }
    }
    assert!(deltas > 16, "thin-walled never transmitted: {deltas}");
    // And its eval must report zero continuous density below the horizon.
    let (ev, _) = m.eval(&r_in, &rec, -Vec3A::Z).unwrap();
    assert_eq!(ev, Vec3A::ZERO);
}

#[test]
fn thin_wall_window_transmittance_matches_formula() {
    // Clear thin glass: throughput must be exactly (1−R)/(1+R) at the
    // view angle, and the R/T window pair must sum to unit energy.
    let m = OpenPBR {
        geometry_thin_walled: true,
        ..OpenPBR::glass(1.5)
    };
    let mut rec = HitRecord::new();
    rec.p = Vec3A::ZERO;
    rec.normal = Vec3A::Z;
    rec.front_face = true;
    let dir = Vec3A::new(0.6, 0.0, -1.0).normalize();
    let ray = Ray::new(-dir, dir);

    let (_, thr, _) = sample_transmission_thin(&m, &ray, &rec);
    let cos_i = -dir.z / dir.length();
    let f = fresnel_dielectric(cos_i, 1.0, 1.5);
    let expected = (1.0 - f) / (1.0 + f);
    assert!(
        (thr - Vec3A::splat(expected)).abs().max_element() < 1e-5,
        "throughput {thr} != window transmittance {expected}"
    );
    // Energy: window reflectance + window transmittance = 1.
    let r_window = 2.0 * f / (1.0 + f);
    assert!((r_window + expected - 1.0).abs() < 1e-6);

    // Grazing rays transmit less than normal-incidence rays.
    let down = Ray::new(Vec3A::Z, -Vec3A::Z);
    let (_, thr_n, _) = sample_transmission_thin(&m, &down, &rec);
    let grazing_dir = Vec3A::new(6.0, 0.0, -1.0).normalize();
    let grazing = Ray::new(-grazing_dir, grazing_dir);
    let (_, thr_g, _) = sample_transmission_thin(&m, &grazing, &rec);
    assert!(thr_g.x < thr_n.x, "grazing {thr_g} not < normal {thr_n}");
}

#[test]
fn thin_wall_tint_darkens_with_angle() {
    // transmission_color is the normal-incidence transmittance; slanted
    // paths travel 1/cosθ_t through the sheet, so darker channels darken
    // faster with angle than lighter ones.
    let m = OpenPBR {
        geometry_thin_walled: true,
        transmission_color: Vec3A::new(0.4, 0.8, 0.9),
        ..OpenPBR::glass(1.5)
    };
    let mut rec = HitRecord::new();
    rec.p = Vec3A::ZERO;
    rec.normal = Vec3A::Z;
    rec.front_face = true;

    let down = Ray::new(Vec3A::Z, -Vec3A::Z);
    let (_, thr_n, _) = sample_transmission_thin(&m, &down, &rec);
    // Normal incidence: path length 1, tint is the authored color.
    assert!(
        (thr_n.x / thr_n.z - 0.4 / 0.9).abs() < 1e-4,
        "normal-incidence tint ratio off: {thr_n}"
    );
    let oblique_dir = Vec3A::new(2.0, 0.0, -1.0).normalize();
    let oblique = Ray::new(-oblique_dir, oblique_dir);
    let (_, thr_o, _) = sample_transmission_thin(&m, &oblique, &rec);
    assert!(
        thr_o.x / thr_o.z < thr_n.x / thr_n.z,
        "oblique tint {thr_o} not relatively darker in the dark channel than {thr_n}"
    );
}

#[test]
fn thin_wall_reflection_boosted_by_internal_bounces() {
    // A thin-walled transmissive sheet reflects 2R/(1+R) — strictly more
    // than the single-interface R of the same thick glass.
    let thick = OpenPBR {
        specular_roughness: 0.25,
        ..OpenPBR::glass(1.5)
    };
    let thin = OpenPBR {
        geometry_thin_walled: true,
        ..thick.clone()
    };
    let v = Vec3A::new(0.4, 0.0, 1.0).normalize();
    let l = Vec3A::new(-0.4, 0.0, 1.0).normalize(); // mirror direction
    let r_thick = eval_all(&thick, v, l, true);
    let r_thin = eval_all(&thin, v, l, true);
    assert!(
        r_thin.x > r_thick.x * 1.5,
        "thin-wall reflection {r_thin} not boosted over {r_thick}"
    );
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
    for &r in &[0.0f32, 0.05, 0.4, 1.0] {
        for &nv in &[0.0f32, 0.1, 0.3, 0.5, 0.9, 1.0] {
            let lobe = ZeltnerSheen::new(r, nv);
            let v = Vec3A::new((1.0 - nv * nv).sqrt(), 0.0, nv);
            for &nl in &[0.0f32, 0.1, 0.3, 0.5, 0.9, 1.0] {
                for &phi in &[0.0f32, 1.0, 3.0] {
                    let s = (1.0 - nl * nl).sqrt();
                    let l = Vec3A::new(s * phi.cos(), s * phi.sin(), nl);
                    let d = lobe.density(v, l);
                    assert!(
                        d.is_finite() && d >= 0.0,
                        "sheen({r},{nv},{nl},{phi}) = {d}"
                    );
                }
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

/// The coat's substrate-albedo darkening must not reach the base dielectric
/// highlight.
///
/// Δ is derived from `base_color`, so it is as coloured as the substrate
/// is. The base dielectric interface reflects ~4% and reflects it white;
/// running it through Δ turned a clearcoated red plastic's white highlight
/// into a dim pink one. `coat_attenuation` — what every lobe under the coat
/// pays — must therefore be achromatic whenever `coat_color` is.
#[test]
fn a_coat_does_not_tint_what_passes_through_it() {
    let m = OpenPBR {
        coat_weight: 1.0,
        coat_color: Vec3A::ONE,
        coat_darkening: 1.0,
        base_color: Vec3A::new(0.8, 0.05, 0.05),
        ..OpenPBR::default()
    };
    let a = coat_attenuation(&m, 1.0, 1.0);
    assert!(
        (a.x - a.y).abs() < 1e-6 && (a.y - a.z).abs() < 1e-6,
        "a white coat tinted the passage: {a}"
    );
    // And the darkening itself is still chromatic — it just lives apart.
    let d = coat_darkening(&m);
    assert!(
        (d.x - d.y).abs() > 1e-3,
        "the darkening lost its substrate colour: {d}"
    );
}

/// Splitting Δ out of `coat_attenuation` must not disable it. A coated
/// diffuse base is still darker with the darkening on than off.
#[test]
fn a_coat_still_darkens_a_diffuse_base() {
    let base = OpenPBR {
        coat_weight: 1.0,
        coat_color: Vec3A::ONE,
        base_color: Vec3A::splat(0.2),
        specular_weight: 0.0,
        ..OpenPBR::default()
    };
    let v = Vec3A::new(0.3, 0.0, 0.954).normalize();
    let l = Vec3A::new(-0.2, 0.2, 0.959).normalize();

    let on = eval_all(
        &OpenPBR {
            coat_darkening: 1.0,
            ..base.clone()
        },
        v,
        l,
        true,
    );
    let off = eval_all(
        &OpenPBR {
            coat_darkening: 0.0,
            ..base
        },
        v,
        l,
        true,
    );
    assert!(off.length() > 1e-6, "test is vacuous: material is black");
    assert!(
        on.length() < off.length() * 0.99,
        "the darkening stopped darkening: {on} vs {off}"
    );
}

/// The metal lobe must not read `specular_weight`.
///
/// The two halves of `eval_specular` carry independent coverage —
/// `base_metalness` for the metal, `(1 - base_metalness) * specular_weight`
/// for the dielectric — and that independence is what lets the MaterialX
/// reduction hand through a conductor and a dielectric mixed at unequal
/// weights. While both halves were scaled by `specular_weight`, one pool's
/// coverage multiplied the other's.
#[test]
fn the_metal_lobe_does_not_scale_with_specular_weight() {
    let v = Vec3A::new(0.6, 0.0, 0.8).normalize();
    let l = Vec3A::new(-0.5, 0.3, 0.81).normalize();
    let h = (v + l).normalize();

    let at = |w: f32| {
        let m = OpenPBR {
            base_metalness: 1.0,
            specular_weight: w,
            ..OpenPBR::default()
        };
        let (ax, ay) = roughness_to_alpha_aniso(m.specular_roughness, 0.0);
        eval_specular(&m, v, l, h, ax, ay).1
    };
    let full = at(1.0);
    assert!(full.length() > 1e-6, "test is vacuous: metal lobe is black");
    assert_eq!(
        at(0.4),
        full,
        "the metal lobe moved with a parameter that belongs to the dielectric base"
    );
    assert_eq!(at(0.0), full, "a pure metal lost its lobe entirely");
}

/// `eval_all` must not skip `eval_specular` for a pure metal. Its
/// `specular_weight` is legitimately 0 — there is no dielectric interface —
/// and the metal half is gated by `base_metalness` instead.
#[test]
fn a_pure_metal_is_still_shaded_with_no_dielectric_interface() {
    let m = OpenPBR {
        base_metalness: 1.0,
        specular_weight: 0.0,
        specular_roughness: 0.3,
        ..OpenPBR::default()
    };
    let v = Vec3A::new(0.4, 0.0, 0.917).normalize();
    let l = Vec3A::new(-0.35, 0.2, 0.915).normalize();
    assert!(
        eval_all(&m, v, l, true).length() > 1e-6,
        "a pure metal shaded black because it had no dielectric interface"
    );
}

/// A `specular_weight` of zero must leave no dielectric lobe at all.
///
/// The regression: `specular_weight` used to be folded into F0, and Schlick
/// is `F0 + (1 − F0)(1 − cosθ)⁵`, which returns **1.0 at grazing** however
/// small F0 is. So `OpenPBR::diffuse()` — every unbound prim, and every
/// MaterialX body whose dielectrics were all promoted to the coat — carried
/// a full-strength white glossy rim at the fallback roughness 0.3, on a
/// material with no specular interface whatsoever.
#[test]
fn a_zero_specular_weight_has_no_dielectric_lobe() {
    let m = OpenPBR::diffuse(Vec3A::splat(0.5));
    assert_eq!(m.specular_weight, 0.0, "preset changed under the test");
    // Grazing on both sides, which is where the old form peaked.
    let v = Vec3A::new(0.999, 0.0, 0.0447).normalize();
    let l = Vec3A::new(-0.999, 0.0, 0.0447).normalize();
    let h = (v + l).normalize();
    let (ax, ay) = roughness_to_alpha_aniso(m.specular_roughness, 0.0);
    let (diel, metal) = eval_specular(&m, v, l, h, ax, ay);
    assert_eq!(
        diel,
        Vec3A::ZERO,
        "a white rim survived on a material with no specular"
    );
    assert_eq!(metal, Vec3A::ZERO, "a diffuse preset grew a metal lobe");
}

/// `specular_weight` scales the lobe, so the lobe is linear in it. Folding
/// it into F0 did not satisfy this at any angle off normal — which is the
/// shortest statement of what the bug was.
#[test]
fn the_dielectric_lobe_is_linear_in_specular_weight() {
    let mut m = OpenPBR {
        base_metalness: 0.0,
        ..OpenPBR::default()
    };
    let v = Vec3A::new(0.6, 0.0, 0.8).normalize();
    let l = Vec3A::new(-0.5, 0.3, 0.81).normalize();
    let h = (v + l).normalize();
    let (ax, ay) = roughness_to_alpha_aniso(m.specular_roughness, 0.0);

    m.specular_weight = 1.0;
    let full = eval_specular(&m, v, l, h, ax, ay).0;
    m.specular_weight = 0.5;
    let half = eval_specular(&m, v, l, h, ax, ay).0;
    assert!(full.length() > 1e-6, "test is vacuous: lobe is black");
    assert!(
        (half * 2.0 - full).length() < 1e-5,
        "not linear in specular_weight: {half} vs {full}"
    );
}

#[test]
fn coat_darkening_identity_at_zero() {
    // darkening = 0 → returns Vec3A::ONE regardless of base_color / ior.
    let v = coat_darkening_factor(Vec3A::new(0.7, 0.3, 0.2), 1.6, 1.0, 0.0);
    assert!((v - Vec3A::ONE).length() < 1e-4);
}

#[test]
fn coat_darkening_is_one_for_a_white_base() {
    // Every bounce a white base sends back up eventually escapes, so the
    // geometric series sums to exactly the uncoated albedo.
    let v = coat_darkening_factor(Vec3A::ONE, 1.5, 1.0, 1.0);
    assert!((v - Vec3A::ONE).abs().max_element() < 1e-5, "{v}");
}

#[test]
fn coat_darkening_does_not_square_a_dark_base() {
    // The regression this pins: the factor is a ratio bounded below by
    // 1 − K̄ (≈ 0.43 at η = 1.5), applied on top of the base colour the
    // lobes already carry. The old form tended to the base colour itself
    // for dark bases, so a 0.05 red base was multiplied by ~0.055 and a
    // car-paint substrate went nearly black under its clearcoat.
    let e = 0.05;
    let v = coat_darkening_factor(Vec3A::splat(e), 1.5, 1.0, 1.0).x;
    let f0 = f0_from_ior(1.5);
    let k = 1.0 - (1.0 - f0) / (1.5 * 1.5);
    let expected = (1.0 - k) / (1.0 - k * e);
    assert!((v - expected).abs() < 1e-5, "{v} != {expected}");
    assert!(
        v > 0.4 && v < 0.5,
        "dark base factor {v} out of the physical range"
    );
    assert!(v > 4.0 * e, "the base colour was applied twice: {v}");
    // Darker bases darken more, but monotonically toward 1 − K̄.
    let mid = coat_darkening_factor(Vec3A::splat(0.5), 1.5, 1.0, 1.0).x;
    assert!(mid > v && mid < 1.0, "mid-grey factor {mid}");
}

#[test]
fn coat_darkening_fades_with_coat_weight() {
    let full = coat_darkening_factor(Vec3A::splat(0.3), 1.5, 1.0, 1.0);
    let half = coat_darkening_factor(Vec3A::splat(0.3), 1.5, 0.5, 1.0);
    let none = coat_darkening_factor(Vec3A::splat(0.3), 1.5, 0.0, 1.0);
    assert!((none - Vec3A::ONE).abs().max_element() < 1e-6, "{none}");
    let expected = Vec3A::ONE.lerp(full, 0.5);
    assert!(
        (half - expected).abs().max_element() < 1e-5,
        "{half} != {expected}"
    );
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
    let ray = Ray::new(
        Vec3A::new(0.5, 0.0, 1.0),
        Vec3A::new(-0.5, 0.0, -1.0).normalize(),
    );
    let mut got_positive = false;
    let mut smp = s();
    for _ in 0..128 {
        if let Some(sample) = m.scatter_importance(&ray, &rec, smp.next())
            && sample.value.length_squared() > 0.0
        {
            got_positive = true;
            break;
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
fn cauchy_fit_hits_abbe_definition() {
    // The fit must reproduce n_d exactly at the d line and have exactly
    // the requested Abbe number V_d = (n_d − 1)/(n_F − n_C).
    let (n_d, v_d) = (1.5f32, 30.0f32);
    let n_at_d = cauchy_ior(n_d, v_d, FRAUNHOFER_D_NM);
    assert!((n_at_d - n_d).abs() < 1e-6, "n(λ_d) = {n_at_d}");
    let n_f = cauchy_ior(n_d, v_d, FRAUNHOFER_F_NM);
    let n_c = cauchy_ior(n_d, v_d, FRAUNHOFER_C_NM);
    let abbe = (n_d - 1.0) / (n_f - n_c);
    assert!(
        (abbe - v_d).abs() < 0.05,
        "fit Abbe {abbe} != requested {v_d}"
    );
}

#[test]
fn dispersion_scale_scales_spread_linearly() {
    // The effective Abbe is abbe/scale, and the Cauchy B term (hence the
    // per-channel spread) is proportional to 1/V_d — so doubling the
    // scale doubles the R↔B spread.
    let full = dispersive_ior(1.5, 40.0, 1.0);
    let half = dispersive_ior(1.5, 40.0, 0.5);
    let ratio = (full.z - full.x) / (half.z - half.x);
    assert!((ratio - 2.0).abs() < 1e-3, "spread ratio {ratio}");
    // And scale 1 gives the physical glass: green stays near n_d.
    assert!((full.y - 1.5).abs() < 0.01, "green {} far from n_d", full.y);
}

#[test]
fn dispersive_ior_below_one_uses_reciprocal() {
    // η < 1 disperses via its reciprocal: dispersive(1/n) = 1/dispersive(n)
    // per channel, so the model is symmetric across the interface.
    let n = dispersive_ior(1.5, 30.0, 1.0);
    let inv = dispersive_ior(1.0 / 1.5, 30.0, 1.0);
    for c in 0..3 {
        assert!(
            (inv[c] - 1.0 / n[c]).abs() < 1e-5,
            "channel {c}: {} vs 1/{}",
            inv[c],
            n[c]
        );
    }
    // Blue still bends more: reciprocal of a larger index is smaller.
    assert!(inv.z < inv.y && inv.y < inv.x, "{inv}");
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
    let r_in = Ray::new(
        Vec3A::new(0.3, -0.2, 1.0),
        Vec3A::new(-0.3, 0.2, -1.0).normalize(),
    );

    let mut transmitted = 0;
    for _ in 0..256 {
        if let Some(sample) = m.scatter_importance(&r_in, &rec, sampler.next()) {
            assert!(!sample.delta, "dispersive thick glass has no delta lobe");
            let wi = sample.ray.direction().normalize();
            if wi.z < 0.0 {
                transmitted += 1;
            }
            let (ev, epdf) = m
                .eval(&r_in, &rec, wi)
                .expect("dispersive glass is evaluable");
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
            assert!(w.is_finite() && (0.0..30.0).contains(&w), "weight {w}");
        }
    }
    assert!(
        transmitted > 64,
        "dispersive glass should mostly refract: {transmitted}"
    );
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
    // The green channel's own IOR under the Cauchy fit (545 nm sits
    // slightly blue of the d line where n_d is defined).
    let eta_g = transmission_iors(&m).y;
    let l_green = refract_dir(-dir_in, Vec3A::Z, 1.0 / eta_g)
        .unwrap()
        .normalize();

    let (v, pdf) = m.eval(&r_in, &rec, l_green).expect("evaluable");
    assert!(pdf > 0.0, "pdf = {pdf}");
    assert!(
        v.y > 0.0,
        "green channel dark at its own Snell direction: {v}"
    );
    assert!(
        v.y > v.x,
        "green {} not > red {} at green Snell direction",
        v.y,
        v.x
    );
    assert!(
        v.y > v.z,
        "green {} not > blue {} at green Snell direction",
        v.y,
        v.z
    );
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
    let wi = refract_dir(-dir_in, Vec3A::Z, 1.0 / 1.5)
        .unwrap()
        .normalize();

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
    assert!(
        (f_g - Vec3A::ONE).abs().max_element() < 1e-5,
        "F(90°) = {f_g}"
    );
    // At μ̄ = 1/7 the reflectance is exactly Schlick scaled by the tint.
    let mu_bar = 1.0 / 7.0;
    let with = fresnel_f82_tint(mu_bar, f0, tint);
    let without = fresnel_f82_tint(mu_bar, f0, Vec3A::ONE);
    assert!(
        (with.y / without.y - tint.y).abs() < 1e-3,
        "{with} vs {without}"
    );
    assert!(
        (with.z / without.z - tint.z).abs() < 1e-3,
        "{with} vs {without}"
    );
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
    // The substrate attenuation carries the coat_color absorption. At
    // normal incidence the in + out passages (√color each) recover the
    // authored round-trip color exactly.
    let atten = coat_attenuation(&m, 1.0, 1.0);
    assert!(
        (atten.x / atten.y - m.coat_color.x / m.coat_color.y).abs() < 1e-3,
        "substrate attenuation not coat_color-tinted: {atten}"
    );
}

#[test]
fn coat_round_trip_recovers_authored_color() {
    // coat_color is the round-trip absorption at normal incidence: the
    // two passages must give exactly color · (1 − F0)² there.
    let m = OpenPBR {
        coat_weight: 1.0,
        coat_color: Vec3A::new(0.9, 0.4, 0.16),
        coat_darkening: 0.0,
        ..OpenPBR::default()
    };
    let atten = coat_attenuation(&m, 1.0, 1.0);
    let f0 = f0_from_ior(m.coat_ior);
    let expected = m.coat_color * (1.0 - f0) * (1.0 - f0);
    assert!(
        (atten - expected).abs().max_element() < 1e-4,
        "{atten} != {expected}"
    );
}

#[test]
fn coat_passage_darkens_and_saturates_at_grazing() {
    // Slanted passages travel further through the coat (1/cosθ_t) and
    // lose more to Fresnel: every channel dims, and the tinted channel
    // dims relatively faster (color saturates with angle).
    let m = OpenPBR {
        coat_weight: 1.0,
        coat_color: Vec3A::new(0.9, 0.3, 0.3),
        ..OpenPBR::default()
    };
    let p_n = coat_passage(&m, 1.0);
    let p_g = coat_passage(&m, 0.2);
    assert!(
        p_g.x < p_n.x && p_g.y < p_n.y,
        "{p_g} not dimmer than {p_n}"
    );
    assert!(
        p_g.y / p_g.x < p_n.y / p_n.x,
        "tinted channel not saturating with angle: {p_g} vs {p_n}"
    );
    // And the full attenuation is view-dependent through both passages.
    let a_n = coat_attenuation(&m, 1.0, 1.0);
    let a_g = coat_attenuation(&m, 0.2, 0.2);
    assert!(a_g.y < a_n.y);
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
    // Tinted by one outbound coat passage: √coat_color at normal
    // incidence (the authored color is the round-trip absorption).
    assert!(
        (e_n.y / e_n.x - 0.2f32.sqrt()).abs() < 1e-3,
        "not coat-tinted: {e_n}"
    );
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
    assert!(
        (ay - inv * expect_ax).abs() < 1e-6,
        "ay {ay} != {}",
        inv * expect_ax
    );
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

/// Draw transmission samples until one refracts into the surface, and
/// return the interior medium it carries (None if the ray has none).
fn sample_interior_medium(m: &OpenPBR) -> Option<Medium> {
    let mut sampler = s();
    let mut rec = HitRecord::new();
    rec.p = Vec3A::ZERO;
    rec.normal = Vec3A::Z;
    rec.front_face = true;
    let r_in = Ray::new(
        Vec3A::new(0.3, -0.2, 1.0),
        Vec3A::new(-0.3, 0.2, -1.0).normalize(),
    );
    for _ in 0..256 {
        if let Some(sample) = m.scatter_importance(&r_in, &rec, sampler.next())
            && sample.ray.direction().z < 0.0
        {
            return sample.ray.medium().copied();
        }
    }
    panic!("material never transmitted");
}

/// The interior medium is built once per material and shared by every
/// refraction into it; a clone — made to be changed — starts empty and
/// builds its own from its own parameters.
#[test]
fn interior_medium_is_built_once_and_not_cloned() {
    let m = OpenPBR {
        transmission_color: Vec3A::new(0.5, 0.7, 0.9),
        transmission_depth: 2.0,
        ..OpenPBR::glass(1.5)
    };
    assert!(m.interior.0.get().is_none());
    let a = sample_interior_medium(&m).expect("deep glass carries a medium");
    assert_eq!(
        m.interior.0.get(),
        Some(&Some(a)),
        "the first refraction fills the cache"
    );
    let b = sample_interior_medium(&m).expect("deep glass carries a medium");
    assert_eq!(a, b);
    let mut changed = m.clone();
    assert!(changed.interior.0.get().is_none(), "a clone starts empty");
    changed.transmission_depth = 4.0;
    let c = sample_interior_medium(&changed).expect("deep glass carries a medium");
    assert_eq!(c.sigma_a, a.sigma_a * 0.5, "the clone kept the old medium");
}

#[test]
fn transmission_scatter_wires_into_interior_medium() {
    // transmission_scatter/depth becomes the interior σₛ, the extinction
    // stays -ln(color)/depth, and the anisotropy carries through.
    let m = OpenPBR {
        specular_roughness: 0.25,
        transmission_color: Vec3A::new(0.5, 0.7, 0.9),
        transmission_depth: 2.0,
        // Kept below the per-channel extinction so σₐ = σₜ − σₛ stays
        // non-negative without triggering the spec's gray shift.
        transmission_scatter: Vec3A::new(0.2, 0.2, 0.1),
        transmission_scatter_anisotropy: 0.5,
        ..OpenPBR::glass(1.5)
    };
    let medium = sample_interior_medium(&m).expect("scattering glass must carry a medium");
    assert!(medium.is_scattering());
    let expected_sigma_s = Vec3A::new(0.2, 0.2, 0.1) / 2.0;
    assert!(
        (medium.sigma_s - expected_sigma_s).abs().max_element() < 1e-5,
        "sigma_s {:?}",
        medium.sigma_s
    );
    let expected_ext = Vec3A::new(-0.5f32.ln(), -0.7f32.ln(), -0.9f32.ln()) / 2.0;
    let ext = medium.sigma_a + medium.sigma_s;
    assert!(
        (ext - expected_ext).abs().max_element() < 1e-5,
        "extinction {ext:?} != {expected_ext:?}"
    );
    assert_eq!(medium.g, 0.5);
}

#[test]
fn transmission_scatter_negative_absorption_shifts_to_gray() {
    // White transmission color → zero extinction; with scatter > 0 the
    // raw σₐ = -σₛ is negative, and the spec shifts it by enough gray to
    // be non-negative.
    let m = Medium::from_transmission(Vec3A::ONE, 1.0, Vec3A::new(0.1, 0.2, 0.4), 0.0);
    assert!(m.sigma_a.min_element() >= 0.0, "σₐ {:?}", m.sigma_a);
    // The largest-scatter channel ends at zero absorption after the shift.
    assert!(m.sigma_a.z.abs() < 1e-6, "σₐ {:?}", m.sigma_a);
    assert!(m.sigma_a.x > m.sigma_a.y && m.sigma_a.y > m.sigma_a.z);
}

#[test]
fn van_de_hulst_inversion_boosts_single_scatter_albedo() {
    // An observed (multi-scattering) albedo of 0.5 requires a
    // single-scattering albedo of ≈ 0.91 (van de Hulst); the naive
    // σₛ = σₜ·A mapping would give 0.5 and badly under-scatter.
    let m = Medium::from_subsurface(Vec3A::splat(0.5), 1.0, Vec3A::ONE, 0.0);
    let a = m.albedo();
    assert!(
        (a.x - 0.9117).abs() < 5e-3,
        "α_ss {a} for observed 0.5, expected ≈ 0.9117"
    );
    // Monotonic in the observed albedo, saturating toward 1.
    let hi = Medium::from_subsurface(Vec3A::splat(0.95), 1.0, Vec3A::ONE, 0.0).albedo();
    let lo = Medium::from_subsurface(Vec3A::splat(0.05), 1.0, Vec3A::ONE, 0.0).albedo();
    assert!(hi.x > 0.99, "α_ss({}) = {}", 0.95, hi.x);
    assert!(lo.x < a.x && a.x < hi.x);
    // Forward anisotropy raises the required single-scattering albedo.
    let fwd = Medium::from_subsurface(Vec3A::splat(0.5), 1.0, Vec3A::ONE, 0.9).albedo();
    assert!(fwd.x > a.x, "g=0.9 albedo {} not > g=0 {}", fwd.x, a.x);
    // Extinction is the reciprocal mean free path.
    assert!(((m.sigma_a + m.sigma_s).x - 1.0).abs() < 1e-5);
}

#[test]
fn interior_medium_blends_subsurface_into_glass() {
    // Transmission + subsurface: the interior blends both volumes by
    // their dielectric fractions (t and (1-t)·s), and the phase
    // anisotropy comes from the scattering (subsurface) component.
    let m = OpenPBR {
        specular_roughness: 0.25,
        transmission_depth: 1.0,
        subsurface_weight: 1.0,
        subsurface_color: Vec3A::splat(0.5),
        subsurface_radius: 0.1,
        subsurface_radius_scale: Vec3A::ONE,
        subsurface_scatter_anisotropy: 0.3,
        ..OpenPBR::glass(1.5)
    };
    let m = OpenPBR {
        transmission_weight: 0.5,
        ..m
    };
    let medium = sample_interior_medium(&m).expect("sss interior must carry a medium");
    // Clear transmission volume contributes nothing; the sss half does.
    assert!(medium.is_scattering());
    assert!((medium.g - 0.3).abs() < 1e-5, "g {}", medium.g);
    // Half the sss volume's scattering (sss fraction (1-0.5)·1 = 0.5).
    let full_sss = Medium::from_subsurface(Vec3A::splat(0.5), 0.1, Vec3A::ONE, 0.3);
    assert!(
        (medium.sigma_s - full_sss.sigma_s * 0.5)
            .abs()
            .max_element()
            < 1e-3,
        "sigma_s {:?}",
        medium.sigma_s
    );
}

#[test]
fn inert_glass_attaches_no_medium() {
    // Zero-depth clear glass has nothing to absorb or scatter: the
    // refracted ray should carry no medium at all.
    let m = OpenPBR {
        specular_roughness: 0.25,
        ..OpenPBR::glass(1.5)
    };
    assert!(
        sample_interior_medium(&m).is_none(),
        "inert interior should not carry a medium"
    );
}

#[test]
fn medium_transmittance_full_at_zero_depth() {
    let m = Medium::from_transmission(Vec3A::new(0.5, 0.5, 0.5), 0.0, Vec3A::ZERO, 0.0);
    let t = m.transmittance(1.0);
    // Zero-depth medium: no absorption, transmittance identically 1.
    assert!((t - Vec3A::ONE).length() < 1e-4);
}

#[test]
fn medium_transmittance_attenuates_with_distance() {
    let m = Medium::from_transmission(Vec3A::new(0.5, 0.7, 0.9), 1.0, Vec3A::ZERO, 0.0);
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
        if let Some(sample) = m_sss.scatter_importance(&ray, &rec, smp.next()) {
            sum_sss += sample.value;
        }
        if let Some(sample) = m_no_sss.scatter_importance(&ray, &rec, smp.next()) {
            sum_no += sample.value;
        }
    }
    assert!(
        sum_sss.y < sum_no.y,
        "SSS green {} not < baseline {}",
        sum_sss.y,
        sum_no.y
    );
    assert!(
        sum_sss.z < sum_no.z,
        "SSS blue {} not < baseline {}",
        sum_sss.z,
        sum_no.z
    );
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
        if let Some(sample) = m.scatter_importance(&ray, &rec, smp.next())
            && sample.ray.direction().y < 0.0
        {
            assert!(sample.delta, "transmission must be flagged delta");
            got_downward = true;
            break;
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
    let ray = Ray::new(
        Vec3A::new(0.0, 1.0, 1.0),
        Vec3A::new(0.0, -1.0, -1.0).normalize(),
    );
    // Run many samples: none should be NaN or negative.
    let mut smp = s();
    for _ in 0..64 {
        if let Some(sample) = m.scatter_importance(&ray, &rec, smp.next()) {
            assert!(
                sample.pdf.is_finite() && sample.pdf > 0.0,
                "pdf = {}",
                sample.pdf
            );
            assert!(sample.value.is_finite(), "value = {:?}", sample.value);
            assert!(
                sample.value.x >= 0.0 && sample.value.y >= 0.0 && sample.value.z >= 0.0,
                "value = {:?}",
                sample.value
            );
        }
    }
}

#[test]
fn every_lobe_has_an_event() {
    use crate::lpe::{LobeLabel, Scatter};
    let m = OpenPBR {
        specular_roughness: 0.0,
        coat_roughness: 0.4,
        ..OpenPBR::default()
    };
    let events: Vec<_> = Lobe::ALL
        .iter()
        .map(|l| {
            let e = l.event(&m);
            (e.transmit, e.scatter, e.label)
        })
        .collect();
    assert_eq!(
        events,
        [
            (false, Scatter::Diffuse, LobeLabel::Diffuse),
            (false, Scatter::Singular, LobeLabel::Specular),
            (false, Scatter::Glossy, LobeLabel::Coat),
            (false, Scatter::Glossy, LobeLabel::Sheen),
            (true, Scatter::Singular, LobeLabel::Transmission),
        ]
    );
}

#[test]
fn the_lobe_split_sums_to_eval_within_rounding() {
    use crate::lpe::LobeSplit;
    let materials = [
        OpenPBR::default(),
        OpenPBR::diffuse(Vec3A::new(0.8, 0.3, 0.2)),
        OpenPBR {
            base_metalness: 1.0,
            coat_weight: 1.0,
            coat_roughness: 0.1,
            fuzz_weight: 0.5,
            ..OpenPBR::default()
        },
        OpenPBR {
            transmission_weight: 1.0,
            specular_roughness: 0.2,
            ..OpenPBR::default()
        },
    ];
    let mut split = LobeSplit::default();
    let mut rec = HitRecord::new();
    rec.normal = Vec3A::Z;
    rec.front_face = true;
    let r_in = Ray::new(
        Vec3A::new(0.3, -0.2, 1.0),
        Vec3A::new(-0.3, 0.2, -1.0).normalize(),
    );
    for (i, m) in materials.iter().enumerate() {
        for k in 0..64 {
            let phi = k as f32 * 0.37;
            let z = 1.0 - 2.0 * ((k as f32 + 0.5) / 64.0);
            let s = (1.0 - z * z).sqrt();
            let wi = Vec3A::new(s * phi.cos(), s * phi.sin(), z);
            let Some((value, _)) = m.eval(&r_in, &rec, wi) else {
                continue;
            };
            assert!(m.eval_lobes_resolved(&r_in, &rec, wi, &mut split));
            let total = split.total();
            let tol = 4.0 * f32::EPSILON * value.max_element().abs().max(1e-30);
            assert!(
                (total - value).abs().max_element() <= tol,
                "material {i}, direction {k}: {total} vs {value}"
            );
        }
        for cos_v in [0.05, 0.5, 1.0] {
            let a = m.albedo(cos_v);
            assert!(a.min_element() >= 0.0 && a.max_element() <= 1.0);
        }
    }
    // A diffuse material's albedo is its colour.
    let d = OpenPBR::diffuse(Vec3A::new(0.8, 0.3, 0.2)).albedo(0.7);
    assert!(
        (d - Vec3A::new(0.8, 0.3, 0.2)).abs().max_element() < 0.05,
        "{d}"
    );
}

#[test]
fn scatter_split_draws_the_same_direction() {
    use crate::lpe::LobeSplit;
    let materials = [
        OpenPBR {
            coat_weight: 1.0,
            coat_roughness: 0.0,
            specular_roughness: 0.3,
            fuzz_weight: 0.3,
            ..OpenPBR::default()
        },
        OpenPBR {
            transmission_weight: 1.0,
            specular_roughness: 0.1,
            ..OpenPBR::default()
        },
    ];
    let mut rec = HitRecord::new();
    rec.normal = Vec3A::Z;
    rec.front_face = true;
    let r_in = Ray::new(
        Vec3A::new(0.3, -0.2, 1.0),
        Vec3A::new(-0.3, 0.2, -1.0).normalize(),
    );
    let mut split = LobeSplit::default();
    for m in &materials {
        let mut sampler = s();
        for _ in 0..256 {
            let dom = sampler.next();
            let plain = m.scatter_resolved(&r_in, &rec, dom);
            let with = m.scatter_split(&r_in, &rec, dom, &mut split);
            let (Some(plain), Some(with)) = (plain, with) else {
                continue;
            };
            assert_eq!(
                plain.ray.direction().to_array().map(f32::to_bits),
                with.ray.direction().to_array().map(f32::to_bits)
            );
            assert_eq!(
                plain.value.to_array().map(f32::to_bits),
                with.value.to_array().map(f32::to_bits)
            );
            if plain.delta {
                continue;
            }
            // The shares sum to the value even where a zero-roughness lobe
            // makes it a needle: evaluated at the direction drawn.
            let total = split.total();
            let tol = 4.0 * f32::EPSILON * plain.value.max_element().abs().max(1e-30);
            assert!(
                (total - plain.value).abs().max_element() <= tol,
                "{total} vs {}",
                plain.value
            );
        }
    }
}

#[test]
fn the_diffuse_filter_is_the_colour_with_its_layer_weights() {
    let c = Vec3A::new(0.8, 0.4, 0.1);
    let f = 1.0 - f0_from_ior(OpenPBR::diffuse(c).specular_ior);
    let plain = OpenPBR::diffuse(c).diffuse_filter(0.7);
    assert!((plain - c * f).abs().max_element() < 1e-6, "{plain}");

    // A coat dims it by its (colour-dependent, view-independent) darkening,
    // never by its directional passage, which belongs to the light.
    let coated = OpenPBR {
        coat_weight: 1.0,
        ..OpenPBR::diffuse(c)
    };
    let expected = c * f * coat_darkening(&coated);
    assert!((coated.diffuse_filter(0.7) - expected).abs().max_element() < 1e-6);

    // No diffuse lobe, no filter.
    let glass = OpenPBR {
        transmission_weight: 1.0,
        ..OpenPBR::default()
    };
    assert_eq!(glass.diffuse_filter(0.7), Vec3A::ZERO);
    let metal = OpenPBR {
        base_metalness: 1.0,
        ..OpenPBR::default()
    };
    assert_eq!(metal.diffuse_filter(0.7), Vec3A::ZERO);
}

/// Thin-walled sheets the straight-transmission tests share: clear and
/// tinted glass, one partly diffuse under a coat, and one rough.
fn thin_sheets() -> Vec<(&'static str, OpenPBR)> {
    let glass = OpenPBR {
        geometry_thin_walled: true,
        ..OpenPBR::glass(1.5)
    };
    vec![
        ("clear", glass.clone()),
        (
            "tinted",
            OpenPBR {
                transmission_color: Vec3A::new(0.9, 0.5, 0.2),
                ..glass.clone()
            },
        ),
        (
            "coated, half diffuse",
            OpenPBR {
                transmission_weight: 0.5,
                base_color: Vec3A::new(0.6, 0.3, 0.2),
                coat_weight: 0.7,
                ..glass.clone()
            },
        ),
        (
            "rough",
            OpenPBR {
                specular_roughness: 0.4,
                transmission_color: Vec3A::new(0.3, 0.8, 0.6),
                ..glass.clone()
            },
        ),
        (
            "fuzzy",
            OpenPBR {
                fuzz_weight: 0.8,
                fuzz_roughness: 0.7,
                ..glass
            },
        ),
    ]
}

fn reduced(m: &OpenPBR) -> ResolvedOpenPBR {
    let mut r = ResolvedOpenPBR::of_plain(m);
    r.exclude_straight();
    r
}

/// `T` is the value a delta sample carries before its lobe-selection
/// compensation, exactly — the weight the BSDF gives the straight line.
#[test]
fn straight_transmittance_is_the_delta_samples_weight() {
    let rec = HitRecord {
        normal: Vec3A::Z,
        front_face: true,
        ..HitRecord::new()
    };
    for (name, m) in thin_sheets() {
        for theta in [0.0f32, 0.6, 1.2] {
            let p_select =
                LobePmf::selecting::<true>(&m, theta.cos())[Lobe::Transmission].max(1e-4);
            let d = Vec3A::new(theta.sin(), 0.0, -theta.cos());
            let r_in = Ray::new(-d, d);
            let t = m.straight_transmittance(&r_in, &rec);
            let mut sampler = s();
            let mut seen = 0;
            for _ in 0..256 {
                if let Some(x) = m.scatter_importance(&r_in, &rec, sampler.next())
                    && x.delta
                {
                    let w = x.value * p_select;
                    assert!(
                        (w - t).abs().max_element() < 1e-5,
                        "{name} at {theta}: {w} vs {t}"
                    );
                    seen += 1;
                }
            }
            assert!(seen > 0, "{name} at {theta}: never transmitted");
        }
    }
}

/// `T` plus the directional albedo of the lobe set without it is the full
/// lobe set's albedo: the meet scatters through exactly what the pass does
/// not carry.
#[test]
fn straight_transmittance_plus_the_rest_is_the_whole_bsdf() {
    let rec = HitRecord {
        normal: Vec3A::Z,
        front_face: true,
        ..HitRecord::new()
    };
    let n = 1 << 14;
    for (name, m) in thin_sheets() {
        let rest = reduced(&m);
        for theta in [0.0f32, 0.7, 1.3] {
            let d = Vec3A::new(theta.sin(), 0.0, -theta.cos());
            let r_in = Ray::new(-d, d);
            let (mut full, mut part) = (Vec3A::ZERO, Vec3A::ZERO);
            let mut sampler = s();
            for _ in 0..n {
                let k = sampler.next();
                if let Some(x) = m.scatter_importance(&r_in, &rec, k) {
                    full += x.value / x.pdf;
                }
                if let Some(x) = rest.scatter(&r_in, &rec, k) {
                    assert!(!x.delta, "{name}: the straight lobe was excluded");
                    part += x.value / x.pdf;
                }
            }
            let (full, part) = (full / n as f32, part / n as f32);
            let t = m.straight_transmittance(&r_in, &rec);
            let err = (t + part - full).abs().max_element();
            assert!(
                err < 0.01,
                "{name} at {theta}: T {t} + rest {part} vs full {full}"
            );
        }
    }
}

/// Without its straight lobe, a thin wall's continuous samples carry the
/// density `eval` reports — NEE at a met wall weighs against exactly the
/// sampling the bounce does.
#[test]
fn the_reduced_lobe_set_samples_what_eval_reports() {
    let rec = HitRecord {
        normal: Vec3A::Z,
        front_face: true,
        ..HitRecord::new()
    };
    let r_in = Ray::new(
        Vec3A::new(0.3, -0.2, 1.0),
        Vec3A::new(-0.3, 0.2, -1.0).normalize(),
    );
    for (name, m) in thin_sheets() {
        let rest = reduced(&m);
        let mut sampler = s();
        let mut checked = 0;
        for _ in 0..256 {
            let Some(x) = rest.scatter(&r_in, &rec, sampler.next()) else {
                continue;
            };
            let (v, pdf) = rest.eval(&r_in, &rec, x.ray.direction()).unwrap();
            assert!((v - x.value).abs().max_element() < 1e-3 * (1.0 + v.max_element()));
            assert!(
                (pdf - x.pdf).abs() < 1e-3 * (1.0 + pdf),
                "{name}: {pdf} vs {}",
                x.pdf
            );
            // The values are the full set's: a delta lobe has none to drop.
            let (full_v, full_pdf) = m.eval(&r_in, &rec, x.ray.direction()).unwrap();
            assert_eq!(full_v, v, "{name}");
            assert!(pdf >= full_pdf, "{name}: renormalised over fewer lobes");
            checked += 1;
        }
        assert!(checked > 16, "{name}: {checked}");
    }
}

#[test]
fn only_a_thin_transmissive_wall_has_straight_transmission() {
    assert!(!OpenPBR::default().has_straight_transmission());
    assert!(!OpenPBR::glass(1.5).has_straight_transmission());
    assert!(
        OpenPBR {
            geometry_thin_walled: true,
            ..OpenPBR::glass(1.5)
        }
        .has_straight_transmission()
    );
    assert!(
        !OpenPBR {
            geometry_thin_walled: true,
            ..OpenPBR::default()
        }
        .has_straight_transmission()
    );
}

// ---------------------------------------------------------------------------
// Fuzz: Zeltner's sheen over a view-dependent attenuation of the base
// ---------------------------------------------------------------------------

/// The ray toward a shading point at the origin (normal +Z) from view
/// direction `v`, and its hit.
fn fuzz_hit(v: Vec3A) -> (Ray, HitRecord) {
    let rec = HitRecord {
        normal: Vec3A::Z,
        front_face: true,
        ..HitRecord::new()
    };
    (Ray::new(v, -v), rec)
}

fn view_at(cos: f32) -> Vec3A {
    let s = (1.0 - cos * cos).max(0.0).sqrt();
    Vec3A::new(s * 0.8, s * 0.6, cos)
}

/// `∫ g(ω) dω` over the hemisphere above the surface, by the midpoint rule
/// in `(t, φ)` with `cos θ = t²`, which crowds the nodes toward the horizon
/// where a low-roughness sheen lives.
fn over_hemisphere(g: impl Fn(Vec3A) -> Vec3A) -> Vec3A {
    const NT: usize = 384;
    const NP: usize = 192;
    let mut sum = glam::DVec3::ZERO;
    for i in 0..NT {
        let t = (i as f32 + 0.5) / NT as f32;
        let cos = t * t;
        let sin = (1.0 - cos * cos).max(0.0).sqrt();
        for j in 0..NP {
            let phi = 2.0 * PI * (j as f32 + 0.5) / NP as f32;
            let w = Vec3A::new(sin * phi.cos(), sin * phi.sin(), cos);
            sum += g(w).as_dvec3() * (2.0 * t as f64);
        }
    }
    (sum / (NT as f64) * (2.0 * std::f64::consts::PI / NP as f64)).as_vec3a()
}

/// The directional albedo of `m` at view cosine `cos`: `∫ f·cos dω`.
fn directional_albedo(m: &OpenPBR, cos: f32) -> Vec3A {
    let (r_in, rec) = fuzz_hit(view_at(cos));
    over_hemisphere(|l| m.eval(&r_in, &rec, l).map_or(Vec3A::ZERO, |(f, _)| f))
}

fn fuzz_only(roughness: f32) -> OpenPBR {
    OpenPBR {
        base_weight: 0.0,
        specular_weight: 0.0,
        fuzz_weight: 1.0,
        fuzz_roughness: roughness,
        ..OpenPBR::default()
    }
}

/// The materials spec's numbers: a white fuzz alone reflects the table's R.
#[test]
fn a_fuzz_reflects_its_view_dependent_albedo() {
    let a = directional_albedo(&fuzz_only(0.3), 1.0).x;
    assert!((a - 0.0008).abs() < 2e-4, "smooth fuzz head-on: {a}");
    let a = directional_albedo(&fuzz_only(0.3), 0.25).x;
    assert!((a - 0.166).abs() < 2e-3, "smooth fuzz at cos 0.25: {a}");
    let a = directional_albedo(&fuzz_only(1.0), 1.0).x;
    assert!((a - 0.342).abs() < 2e-3, "rough fuzz head-on: {a}");
}

/// A white diffuse under a white fuzz of any weight and roughness reflects
/// no more than a white furnace gives it, at any view: the fuzz takes
/// `w · R` and passes `1 − w · R` to a base that reflects at most all of it.
#[test]
fn a_fuzz_over_a_white_diffuse_conserves_energy() {
    for w in [0.3, 1.0] {
        for r in [0.0, 0.3, 1.0] {
            let m = OpenPBR {
                base_color: Vec3A::ONE,
                specular_weight: 0.0,
                fuzz_weight: w,
                fuzz_roughness: r,
                ..OpenPBR::default()
            };
            for cos in [0.05, 0.3, 0.7, 1.0] {
                let a = directional_albedo(&m, cos).max_element();
                assert!(a <= 1.0 + 2e-3, "w {w} r {r} cos {cos}: {a}");
            }
        }
    }
}

/// The samples of a fuzz alone weigh the fuzz's albedo, `fuzz_color · R`:
/// the LTC is sampled exactly. Not every one of them: the absent lobes keep
/// floored selection weights (`LobePmf::selecting`, its KNOWN ISSUE), so a
/// sample near an absent specular or coat lobe's mirror direction meets its
/// density too. The median sample, away from those, weighs `R` exactly;
/// that the mean is unbiased is `a_fuzz_samples_with_the_density_it_reports`.
#[test]
fn fuzz_samples_weigh_its_albedo() {
    for r in [0.1, 0.5, 1.0] {
        for cos in [0.2, 0.6, 1.0] {
            let m = OpenPBR {
                fuzz_color: Vec3A::new(0.9, 0.5, 0.2),
                ..fuzz_only(r)
            };
            let albedo = ZeltnerSheen::new(r, cos).albedo();
            if albedo < 0.05 {
                continue;
            }
            let (r_in, rec) = fuzz_hit(view_at(cos));
            let mut sampler = s();
            let mut weights: Vec<f32> = (0..256)
                .filter_map(|_| m.scatter_importance(&r_in, &rec, sampler.next()))
                .map(|x| x.value.x / x.pdf / m.fuzz_color.x)
                .collect();
            weights.sort_by(f32::total_cmp);
            let median = weights[weights.len() / 2];
            assert!(
                (median - albedo).abs() <= 2e-3 * albedo,
                "r {r} cos {cos}: median weight {median}, R = {albedo}"
            );
        }
    }
}

/// The sampler's density is the one `eval` reports, now that the fuzz's
/// selection weight depends on the view: the density integrates to the
/// share of samples that land (an LTC can shear some below the plane, where
/// a sample is lost), and every sample carries the density `eval` gives its
/// direction.
#[test]
fn a_fuzz_samples_with_the_density_it_reports() {
    let materials = [
        fuzz_only(0.4),
        OpenPBR {
            fuzz_weight: 0.7,
            fuzz_roughness: 0.6,
            ..OpenPBR::default()
        },
        OpenPBR {
            coat_weight: 1.0,
            coat_roughness: 0.2,
            fuzz_weight: 0.5,
            fuzz_roughness: 0.2,
            ..OpenPBR::default()
        },
    ];
    for (i, m) in materials.iter().enumerate() {
        for cos in [0.15, 0.5, 0.95] {
            let (r_in, rec) = fuzz_hit(view_at(cos));
            let mass =
                over_hemisphere(|l| Vec3A::splat(m.eval(&r_in, &rec, l).map_or(0.0, |(_, p)| p))).x;
            let n = 4096;
            let mut sampler = s();
            let mut landed = 0;
            for _ in 0..n {
                if let Some(x) = m.scatter_importance(&r_in, &rec, sampler.next()) {
                    landed += 1;
                    let (_, p) = m
                        .eval(&r_in, &rec, x.ray.direction())
                        .expect("an opaque surface evaluates");
                    assert!(
                        (p - x.pdf).abs() <= 1e-3 * x.pdf.max(1.0),
                        "material {i} cos {cos}: sampled pdf {} vs eval {p}",
                        x.pdf
                    );
                }
            }
            let landed = landed as f32 / n as f32;
            assert!(
                (mass - landed).abs() < 0.02,
                "material {i} cos {cos}: density mass {mass}, samples landed {landed}"
            );
        }
    }
}

/// Emission passes the fuzz as the light beneath it does: dimmed by
/// `1 − fuzz_weight · R(ω_o)`. The materials spec's number.
#[test]
fn emission_dims_behind_a_fuzz() {
    let m = OpenPBR {
        emission_luminance: 1.0,
        ..fuzz_only(1.0)
    };
    let e = m.emitted_directional(1.0).x;
    assert!((e - (1.0 - 0.342)).abs() < 2e-3, "{e}");
    // Without a fuzz, exactly what it was.
    let bare = OpenPBR {
        fuzz_weight: 0.0,
        ..m.clone()
    };
    assert_eq!(
        bare.emitted_directional(0.4),
        bare.emission_color * bare.emission_luminance
    );
}

/// The diffuse filter is still what `eval_split`'s diffuse share is made
/// of, now that the fuzz's attenuation depends on the view: that share
/// divided by the filter is the same EON shape the fuzz-free material has.
#[test]
fn the_diffuse_filter_follows_the_fuzz() {
    let c = Vec3A::new(0.7, 0.5, 0.3);
    let fuzzy = OpenPBR {
        specular_weight: 0.0,
        fuzz_weight: 0.8,
        fuzz_roughness: 0.6,
        ..OpenPBR::diffuse(c)
    };
    let bare = OpenPBR {
        fuzz_weight: 0.0,
        ..fuzzy.clone()
    };
    for cos in [0.2, 0.7] {
        let v = view_at(cos);
        let l = Vec3A::new(-0.3, 0.2, 0.9).normalize();
        let share = |m: &OpenPBR| {
            let mut out = crate::lpe::LobeSplit::default();
            eval_split(m, v, l, true, &mut out);
            out.iter()
                .find(|(e, _)| e.label == crate::lpe::LobeLabel::Diffuse)
                .map(|(_, f)| f)
                .expect("a diffuse share")
        };
        let shape_fuzzy = share(&fuzzy) / fuzzy.diffuse_filter(cos);
        let shape_bare = share(&bare) / bare.diffuse_filter(cos);
        assert!(
            (shape_fuzzy - shape_bare).abs().max_element() < 1e-5,
            "cos {cos}: {shape_fuzzy} vs {shape_bare}"
        );
    }
}

/// The fuzz covers the transmission too: light passing through a fuzzy glass
/// pays the `1 − w · R(ω_o)` every other layer beneath the fuzz pays. Without
/// it the glass reflected the fuzz's `w · R` *and* transmitted everything, so
/// a white furnace returned more than it was given.
#[test]
fn a_fuzz_dims_what_passes_through_the_glass_beneath_it() {
    let glass = OpenPBR {
        specular_roughness: 0.3,
        fuzz_weight: 1.0,
        fuzz_roughness: 1.0,
        ..OpenPBR::glass(1.5)
    };
    for cos in [0.3, 0.7, 1.0] {
        let (r_in, rec) = fuzz_hit(view_at(cos));
        let eval = |l: Vec3A| glass.eval(&r_in, &rec, l).map_or(Vec3A::ZERO, |(f, _)| f);
        let reflected = over_hemisphere(eval);
        let transmitted = over_hemisphere(|l| eval(Vec3A::new(l.x, l.y, -l.z)));
        let total = (reflected + transmitted).max_element();
        assert!(
            total <= 1.0 + 5e-3,
            "cos {cos}: {reflected} + {transmitted} = {total}"
        );
    }

    // A thin sheet's straight transmission, which a delta sample carries,
    // pays the same factor.
    let sheet = OpenPBR {
        geometry_thin_walled: true,
        ..glass.clone()
    };
    let bare = OpenPBR {
        fuzz_weight: 0.0,
        ..sheet.clone()
    };
    let rec = HitRecord {
        normal: Vec3A::Z,
        front_face: true,
        ..HitRecord::new()
    };
    for cos in [0.3f32, 0.7, 1.0] {
        let d = -view_at(cos);
        let r_in = Ray::new(-d, d);
        let want = bare.straight_transmittance(&r_in, &rec)
            * (1.0 - ZeltnerSheen::new(sheet.fuzz_roughness, cos).albedo());
        let got = sheet.straight_transmittance(&r_in, &rec);
        assert!(
            (got - want).abs().max_element() < 1e-5,
            "cos {cos}: {got} vs {want}"
        );
    }
}
