//! `Medium`: the participating-medium coefficients OpenPBR's transmission
//! and subsurface parameters reduce to.

use crust_core::{Medium, Vec3A};

fn approx(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn zero_depth_is_an_inert_medium() {
    let m = Medium::from_transmission(Vec3A::new(0.9, 0.2, 0.1), 0.0, Vec3A::splat(3.0), 0.7);
    assert_eq!(m.sigma_a, Vec3A::ZERO);
    assert_eq!(m.sigma_s, Vec3A::ZERO);
    assert_eq!(m.g, 0.0);
    assert!(!m.is_scattering());
    assert_eq!(m.sigma_t_max(), 0.0);
    assert_eq!(m.transmittance(100.0), Vec3A::ONE);
}

#[test]
fn transmission_colour_is_reached_at_one_depth() {
    let tint = Vec3A::new(0.5, 0.25, 0.8);
    let m = Medium::from_transmission(tint, 1.0, Vec3A::ZERO, 0.0);
    assert!(m.transmittance(1.0).abs_diff_eq(tint, 1e-5));
    // Beer-Lambert compounds: at twice the depth the tint squares.
    assert!(m.transmittance(2.0).abs_diff_eq(tint * tint, 1e-5));
    assert_eq!(m.transmittance(0.0), Vec3A::ONE);
    assert!(!m.is_scattering());
}

#[test]
fn depth_scales_the_extinction_inversely() {
    let tint = Vec3A::splat(0.5);
    let shallow = Medium::from_transmission(tint, 0.5, Vec3A::ZERO, 0.0);
    let deep = Medium::from_transmission(tint, 2.0, Vec3A::ZERO, 0.0);
    assert!(approx(shallow.sigma_a.x, 2.0f32.ln() / 0.5, 1e-5));
    assert!(approx(deep.sigma_a.x, 2.0f32.ln() / 2.0, 1e-5));
    // The tint is reached at each medium's own depth.
    assert!(shallow.transmittance(0.5).abs_diff_eq(tint, 1e-5));
    assert!(deep.transmittance(2.0).abs_diff_eq(tint, 1e-5));
}

#[test]
fn a_white_tint_has_no_extinction() {
    let m = Medium::from_transmission(Vec3A::ONE, 1.0, Vec3A::ZERO, 0.0);
    assert!(m.sigma_a.abs_diff_eq(Vec3A::ZERO, 1e-6));
    assert_eq!(m.sigma_t_max(), 0.0);
}

#[test]
fn a_black_tint_is_clamped_to_a_finite_extinction() {
    let m = Medium::from_transmission(Vec3A::ZERO, 1.0, Vec3A::ZERO, 0.0);
    assert!(m.sigma_a.is_finite());
    assert!(m.sigma_a.x > 5.0, "very dark, but finite: {}", m.sigma_a.x);
    assert!(m.transmittance(1.0).x < 1e-3);
}

#[test]
fn scatter_splits_extinction_into_scattering_and_absorption() {
    let tint = Vec3A::splat(0.5);
    let m = Medium::from_transmission(tint, 1.0, Vec3A::splat(0.2), 0.0);
    let sigma_t = 2.0f32.ln();
    assert!(approx(m.sigma_s.x, 0.2, 1e-6));
    assert!(approx(m.sigma_a.x, sigma_t - 0.2, 1e-5));
    assert!(m.is_scattering());
    // Total extinction, and hence the transmittance, is unchanged by the split.
    assert!(m.transmittance(1.0).abs_diff_eq(tint, 1e-5));
}

#[test]
fn excess_scatter_shifts_absorption_to_stay_non_negative() {
    // σₛ larger than the extinction on one channel: σₐ would go negative,
    // so it is shifted by grey until every channel is ≥ 0.
    let m = Medium::from_transmission(Vec3A::new(0.5, 0.5, 0.9), 1.0, Vec3A::splat(1.0), 0.0);
    assert!(m.sigma_a.min_element() >= -1e-6, "{}", m.sigma_a);
    assert!(
        approx(m.sigma_a.min_element(), 0.0, 1e-5),
        "shifted exactly to zero: {}",
        m.sigma_a
    );
    // The shift is grey: channel differences are preserved.
    let expected_diff = (-0.5f32.ln()) - (-0.9f32.ln());
    assert!(approx(m.sigma_a.x - m.sigma_a.z, expected_diff, 1e-5));
}

#[test]
fn negative_scatter_is_clamped_to_zero() {
    let m = Medium::from_transmission(Vec3A::splat(0.5), 1.0, Vec3A::splat(-3.0), 0.0);
    assert_eq!(m.sigma_s, Vec3A::ZERO);
}

#[test]
fn anisotropy_is_clamped_below_one() {
    assert!(Medium::from_transmission(Vec3A::splat(0.5), 1.0, Vec3A::ZERO, 5.0).g < 1.0);
    assert!(Medium::from_transmission(Vec3A::splat(0.5), 1.0, Vec3A::ZERO, -5.0).g > -1.0);
    assert!(approx(
        Medium::from_transmission(Vec3A::splat(0.5), 1.0, Vec3A::ZERO, 0.3).g,
        0.3,
        1e-6
    ));
    assert!(Medium::from_subsurface(Vec3A::splat(0.5), 1.0, Vec3A::ONE, 2.0).g < 1.0);
}

#[test]
fn sigma_t_max_is_the_largest_channel_extinction() {
    let m = Medium {
        sigma_a: Vec3A::new(0.1, 0.5, 0.2),
        sigma_s: Vec3A::new(0.3, 0.1, 0.9),
        g: 0.0,
    };
    assert!(approx(m.sigma_t_max(), 1.1, 1e-6));
}

#[test]
fn albedo_is_scattering_over_extinction() {
    let m = Medium {
        sigma_a: Vec3A::new(1.0, 3.0, 0.0),
        sigma_s: Vec3A::new(1.0, 1.0, 2.0),
        g: 0.0,
    };
    let a = m.albedo();
    assert!(approx(a.x, 0.5, 1e-6));
    assert!(approx(a.y, 0.25, 1e-6));
    assert!(approx(a.z, 1.0, 1e-6));
    // A vacuum has a defined (zero) albedo rather than NaN.
    let v = Medium {
        sigma_a: Vec3A::ZERO,
        sigma_s: Vec3A::ZERO,
        g: 0.0,
    };
    assert!(v.albedo().is_finite());
    assert_eq!(v.albedo(), Vec3A::ZERO);
}

#[test]
fn is_scattering_ignores_negligible_scatter() {
    let m = Medium {
        sigma_a: Vec3A::ONE,
        sigma_s: Vec3A::splat(1e-9),
        g: 0.0,
    };
    assert!(!m.is_scattering());
    let m = Medium {
        sigma_a: Vec3A::ONE,
        sigma_s: Vec3A::new(0.0, 0.0, 1e-3),
        g: 0.0,
    };
    assert!(m.is_scattering(), "any channel scattering counts");
}

#[test]
fn subsurface_extinction_is_the_inverse_mean_free_path() {
    let m = Medium::from_subsurface(Vec3A::splat(0.5), 2.0, Vec3A::new(1.0, 0.5, 0.25), 0.0);
    let sigma_t = m.sigma_a + m.sigma_s;
    assert!(approx(sigma_t.x, 0.5, 1e-4));
    assert!(approx(sigma_t.y, 1.0, 1e-4));
    assert!(approx(sigma_t.z, 2.0, 1e-4));
    assert!(m.is_scattering());
}

#[test]
fn subsurface_albedo_inversion_hits_the_known_points() {
    // Black observed albedo → no scattering; white → no absorption.
    let black = Medium::from_subsurface(Vec3A::ZERO, 1.0, Vec3A::ONE, 0.0);
    assert!(black.sigma_s.x.abs() < 1e-3, "{}", black.sigma_s);
    let white = Medium::from_subsurface(Vec3A::ONE, 1.0, Vec3A::ONE, 0.0);
    assert!(white.sigma_a.x.abs() < 1e-3, "{}", white.sigma_a);
    // The documented example: an observed albedo of 0.5 needs a
    // single-scattering albedo of about 0.91.
    let half = Medium::from_subsurface(Vec3A::splat(0.5), 1.0, Vec3A::ONE, 0.0);
    assert!(approx(half.albedo().x, 0.91, 0.02), "{}", half.albedo().x);
}

#[test]
fn subsurface_albedo_inversion_is_monotone() {
    let mut prev = -1.0;
    for i in 0..=10 {
        let a = i as f32 / 10.0;
        let m = Medium::from_subsurface(Vec3A::splat(a), 1.0, Vec3A::ONE, 0.0);
        let ss = m.albedo().x;
        assert!(ss >= prev - 1e-5, "not monotone at {a}: {ss} < {prev}");
        assert!((0.0..=1.0 + 1e-5).contains(&ss));
        prev = ss;
    }
}

#[test]
fn subsurface_radius_is_floored_against_zero() {
    let m = Medium::from_subsurface(Vec3A::splat(0.5), 0.0, Vec3A::ONE, 0.0);
    assert!(m.sigma_t_max().is_finite());
    assert!(
        m.sigma_t_max() > 100.0,
        "a tiny mean free path is a dense medium"
    );
}

#[test]
fn blend_combines_coefficients_linearly() {
    let a = Medium {
        sigma_a: Vec3A::splat(1.0),
        sigma_s: Vec3A::splat(2.0),
        g: 0.0,
    };
    let b = Medium {
        sigma_a: Vec3A::splat(3.0),
        sigma_s: Vec3A::splat(4.0),
        g: 0.0,
    };
    let m = Medium::blend(&a, 0.25, &b, 0.75);
    assert!(m.sigma_a.abs_diff_eq(Vec3A::splat(2.5), 1e-6));
    assert!(m.sigma_s.abs_diff_eq(Vec3A::splat(3.5), 1e-6));
}

#[test]
fn blend_weights_anisotropy_by_scattering() {
    let scattering = Medium {
        sigma_a: Vec3A::ZERO,
        sigma_s: Vec3A::splat(1.0),
        g: 0.8,
    };
    let absorbing = Medium {
        sigma_a: Vec3A::splat(5.0),
        sigma_s: Vec3A::ZERO,
        g: 0.0,
    };
    // The non-scattering component must not drag g toward zero.
    let m = Medium::blend(&scattering, 0.5, &absorbing, 0.5);
    assert!(approx(m.g, 0.8, 1e-6), "{}", m.g);
    // Two scatterers: g is their scattering-weighted mean.
    let other = Medium {
        sigma_a: Vec3A::ZERO,
        sigma_s: Vec3A::splat(3.0),
        g: 0.0,
    };
    let m = Medium::blend(&scattering, 1.0, &other, 1.0);
    assert!(approx(m.g, 0.2, 1e-5), "{}", m.g);
    // No scattering anywhere: g is zero, not NaN.
    let m = Medium::blend(&absorbing, 1.0, &absorbing, 1.0);
    assert_eq!(m.g, 0.0);
}

#[test]
fn blend_with_zero_weight_is_the_other_medium() {
    let a = Medium {
        sigma_a: Vec3A::splat(1.0),
        sigma_s: Vec3A::splat(2.0),
        g: 0.3,
    };
    let b = Medium {
        sigma_a: Vec3A::splat(9.0),
        sigma_s: Vec3A::splat(9.0),
        g: -0.5,
    };
    let m = Medium::blend(&a, 1.0, &b, 0.0);
    assert_eq!(m.sigma_a, a.sigma_a);
    assert_eq!(m.sigma_s, a.sigma_s);
    assert!(approx(m.g, a.g, 1e-6));
}

#[test]
fn transmittance_decreases_monotonically_with_distance() {
    let m = Medium::from_transmission(Vec3A::new(0.3, 0.6, 0.9), 1.0, Vec3A::splat(0.1), 0.0);
    let mut prev = Vec3A::splat(2.0);
    for i in 0..20 {
        let t = m.transmittance(i as f32 * 0.5);
        assert!(t.x <= prev.x && t.y <= prev.y && t.z <= prev.z);
        assert!(t.min_element() >= 0.0 && t.max_element() <= 1.0);
        prev = t;
    }
}

#[test]
fn medium_is_cloneable_and_debuggable() {
    let m = Medium::from_transmission(Vec3A::splat(0.5), 1.0, Vec3A::ZERO, 0.0);
    let c = m.clone();
    assert_eq!(c.sigma_a, m.sigma_a);
    assert!(format!("{m:?}").contains("sigma_a"));
}
