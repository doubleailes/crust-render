//! Lights and light shapes through the public API: sampling geometry, the
//! MIS pairing between `sample_li` and `pdf_at_point` / `escaped`, the
//! normalisation conventions of the infinite lights, and the light list.

use crust_core::{
    AreaLight, DistantLight, DomeLight, Emissive, EnvironmentMap, Light, LightList, LightShape,
    RectShape, SphereShape, Vec3A, projected_cone_solid_angle,
};
use glam::Mat3A;
use openqmc::pcg::Rng;
use std::sync::Arc;

fn approx(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

#[test]
fn sphere_shape_area_and_surface_points() {
    let s = SphereShape {
        center: Vec3A::new(1.0, -2.0, 3.0),
        radius: 2.0,
    };
    assert!(approx(s.area(), 4.0 * std::f32::consts::PI * 4.0, 1e-4));
    let mut rng = Rng::new(3);
    for _ in 0..500 {
        let p = s.sample_point(rng.next_f32(), rng.next_f32());
        assert!(approx((p - s.center).length(), 2.0, 1e-4), "{p}");
        let n = s.normal_at(p);
        assert!(approx(n.length(), 1.0, 1e-5));
        assert!(n.dot(p - s.center) > 0.0, "normal points outward");
    }
}

#[test]
fn sphere_shape_sampling_covers_both_hemispheres() {
    let s = SphereShape {
        center: Vec3A::ZERO,
        radius: 1.0,
    };
    let mut rng = Rng::new(9);
    let (mut up, mut down) = (0, 0);
    let mut mean = Vec3A::ZERO;
    for _ in 0..20_000 {
        let p = s.sample_point(rng.next_f32(), rng.next_f32());
        if p.z > 0.0 {
            up += 1
        } else {
            down += 1
        }
        mean += p;
    }
    assert!((up as f32 / 20_000.0 - 0.5).abs() < 0.02);
    assert!(down > 0);
    assert!(
        (mean / 20_000.0).length() < 0.03,
        "uniform by area has zero mean"
    );
}

#[test]
fn rect_shape_area_normal_and_corners() {
    let r = RectShape::new(
        Vec3A::new(-1.0, 0.0, -0.5),
        Vec3A::new(2.0, 0.0, 0.0),
        Vec3A::new(0.0, 0.0, 1.0),
        Vec3A::new(0.0, 5.0, 0.0), // unnormalized on purpose
    );
    assert!(approx(r.area(), 2.0, 1e-6));
    assert_eq!(r.normal, Vec3A::Y, "the constructor normalises the normal");
    assert_eq!(r.sample_point(0.0, 0.0), r.origin);
    assert_eq!(r.sample_point(1.0, 1.0), r.origin + r.edge_u + r.edge_v);
    assert_eq!(r.sample_point(0.5, 0.5), Vec3A::new(0.0, 0.0, 0.0));
    assert_eq!(
        r.normal_at(Vec3A::new(7.0, 7.0, 7.0)),
        Vec3A::Y,
        "flat: same normal everywhere"
    );
}

#[test]
fn rect_shape_samples_lie_in_the_parallelogram() {
    let r = RectShape::new(
        Vec3A::ZERO,
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(0.5, 1.0, 0.0),
        Vec3A::Z,
    );
    let mut rng = Rng::new(4);
    for _ in 0..500 {
        let (u, v) = (rng.next_f32(), rng.next_f32());
        let p = r.sample_point(u, v);
        // Invert the parallelogram parameterisation.
        assert!(approx(p.y, v, 1e-6));
        assert!(approx(p.x - 0.5 * v, u, 1e-6));
        assert_eq!(p.z, 0.0);
    }
    assert!(
        approx(r.area(), 1.0, 1e-6),
        "shear does not change the area"
    );
}

// ---------------------------------------------------------------------------
// Area lights
// ---------------------------------------------------------------------------

fn sphere_light(center: Vec3A, radius: f32, color: Vec3A, geom_id: u32) -> AreaLight {
    AreaLight::new(
        Box::new(SphereShape { center, radius }),
        Arc::new(Emissive::new(color)),
        geom_id,
    )
}

#[test]
fn area_light_sample_aims_at_a_point_on_its_surface() {
    let center = Vec3A::new(0.0, 5.0, 0.0);
    let light = sphere_light(center, 0.5, Vec3A::splat(10.0), 7);
    let from = Vec3A::ZERO;
    let mut rng = Rng::new(5);
    for _ in 0..200 {
        let s = light
            .sample_li(from, rng.next_f32(), rng.next_f32())
            .expect("reachable");
        assert!(approx(s.direction.length(), 1.0, 1e-5));
        let p = from + s.direction * s.distance;
        assert!(
            approx((p - center).length(), 0.5, 1e-3),
            "sampled point off the sphere: {p}"
        );
        assert_eq!(s.radiance, Vec3A::splat(10.0));
        assert!(s.pdf > 0.0 && s.pdf.is_finite());
        assert!(s.distance.is_finite());
    }
    assert_eq!(light.geom_id(), Some(7));
    assert!(
        light.escaped(from, Vec3A::Y).is_none(),
        "finite lights are never 'escaped to'"
    );
}

#[test]
fn area_light_pdf_at_point_matches_its_own_sample() {
    let light = sphere_light(Vec3A::new(2.0, 3.0, -1.0), 0.7, Vec3A::ONE, 0);
    let from = Vec3A::new(0.5, -1.0, 2.0);
    let mut rng = Rng::new(6);
    for _ in 0..200 {
        let s = light
            .sample_li(from, rng.next_f32(), rng.next_f32())
            .unwrap();
        let p = from + s.direction * s.distance;
        let pdf = light.pdf_at_point(from, p);
        assert!(
            approx(pdf, s.pdf, 1e-3 * s.pdf.max(1.0)),
            "{pdf} vs {}",
            s.pdf
        );
    }
}

#[test]
fn rect_light_pdf_is_distance_squared_over_cosine_area() {
    let rect = RectShape::new(
        Vec3A::new(-0.5, -0.5, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(0.0, 1.0, 0.0),
        Vec3A::Z,
    );
    let light = AreaLight::new(Box::new(rect), Arc::new(Emissive::new(Vec3A::ONE)), 1);
    // Straight above the centre at distance 2: cos = 1, area = 1 → pdf ≈ 4.
    let pdf = light.pdf_at_point(Vec3A::new(0.0, 0.0, 2.0), Vec3A::ZERO);
    assert!(approx(pdf, 4.0, 1e-3), "{pdf}");
    // At 45°: the same point seen from (2, 0, 2): dist² = 8, cos = 1/√2.
    let pdf = light.pdf_at_point(Vec3A::new(2.0, 0.0, 2.0), Vec3A::ZERO);
    assert!(approx(pdf, 8.0 * 2f32.sqrt(), 1e-2), "{pdf}");
}

#[test]
fn rect_light_is_effectively_one_sided() {
    let rect = RectShape::new(
        Vec3A::new(-0.5, -0.5, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(0.0, 1.0, 0.0),
        Vec3A::Z,
    );
    let light = AreaLight::new(Box::new(rect), Arc::new(Emissive::new(Vec3A::ONE)), 1);
    let front = light
        .sample_li(Vec3A::new(0.0, 0.0, 1.0), 0.5, 0.5)
        .unwrap();
    let back = light
        .sample_li(Vec3A::new(0.0, 0.0, -1.0), 0.5, 0.5)
        .unwrap();
    // Behind the emitting side the cosine clamps to zero and the pdf
    // explodes, which is how MIS drives the contribution to nothing.
    assert!(
        back.pdf > 1000.0 * front.pdf,
        "front {} back {}",
        front.pdf,
        back.pdf
    );
}

#[test]
fn area_light_declines_a_coincident_shading_point() {
    let light = sphere_light(Vec3A::ZERO, 1.0, Vec3A::ONE, 0);
    // u = 0, v = 0 samples the +Z pole; shading from exactly there is a
    // zero-length connection.
    let pole = SphereShape {
        center: Vec3A::ZERO,
        radius: 1.0,
    }
    .sample_point(0.0, 0.0);
    assert!(light.sample_li(pole, 0.0, 0.0).is_none());
}

/// Area sampling's density in solid angle is `d² / (cos · A)`, so twice as
/// far is four times the pdf. (A rect light, because a sphere seen from
/// outside is no longer area-sampled — see the cone tests below.)
#[test]
fn area_light_pdf_falls_with_the_inverse_square_of_distance() {
    let rect = RectShape::new(
        Vec3A::new(-0.05, -0.05, 0.0),
        Vec3A::new(0.1, 0.0, 0.0),
        Vec3A::new(0.0, 0.1, 0.0),
        Vec3A::Z,
    );
    let light = AreaLight::new(Box::new(rect), Arc::new(Emissive::new(Vec3A::ONE)), 0);
    let near = light.pdf_at_point(Vec3A::new(0.0, 0.0, 2.0), Vec3A::ZERO);
    let far = light.pdf_at_point(Vec3A::new(0.0, 0.0, 4.0), Vec3A::ZERO);
    assert!(approx(far / near, 4.0, 0.01), "{}", far / near);
}

// ---------------------------------------------------------------------------
// Sphere lights: sampled by the cone they subtend
// ---------------------------------------------------------------------------

/// The cone a sphere subtends from `from`, in f64: its axis and `cos θ_max`.
fn subtended_cone(center: Vec3A, radius: f32, from: Vec3A) -> (Vec3A, f64) {
    let d = (center - from).length() as f64;
    let sin_max = radius as f64 / d;
    (
        (center - from).normalize(),
        (1.0 - sin_max * sin_max).sqrt(),
    )
}

/// The three regimes: a sphere nearly touching the shading point, an ordinary
/// one, and one far enough (`sin² θ_max ≈ 2.8e-6`) to take the small-angle
/// branch, where `1 − cos θ_max` cancels to nothing in f32.
const CONE_CASES: [(Vec3A, f32, Vec3A); 3] = [
    (Vec3A::new(0.3, 1.2, -0.1), 1.0, Vec3A::new(0.3, 0.0, -0.1)),
    (Vec3A::new(2.0, 3.0, -1.0), 0.7, Vec3A::new(0.5, -1.0, 2.0)),
    (Vec3A::new(0.0, 300.0, 0.0), 0.5, Vec3A::ZERO),
];

/// Every point lies on the sphere, on the cap that faces the shading point
/// (so none is spent where the sphere occludes its own shadow ray), inside the
/// subtended cone, and carries the cone's uniform density.
#[test]
fn sphere_light_samples_only_the_cap_facing_the_shading_point() {
    for (center, radius, from) in CONE_CASES {
        let shape = SphereShape { center, radius };
        let light = sphere_light(center, radius, Vec3A::ONE, 0);
        let (axis, cos_max) = subtended_cone(center, radius, from);
        let expected_pdf = 1.0 / (2.0 * std::f64::consts::PI * (1.0 - cos_max));
        let mut rng = Rng::new(11);
        for _ in 0..2000 {
            let (u, v) = (rng.next_f32(), rng.next_f32());
            let (p, pdf) = shape
                .sample_solid_angle(from, u, v)
                .expect("outside the sphere there is always a cone");
            assert!(
                approx((p - center).length(), radius, 1e-4 * radius),
                "off the sphere: {p}"
            );
            let facing = (p - center).normalize().dot((from - p).normalize());
            assert!(facing >= -1e-3, "sampled the far side: cos = {facing}");
            let cos_theta = (p - from).normalize().dot(axis) as f64;
            assert!(
                cos_theta >= cos_max - 1e-5,
                "outside the cone: {cos_theta} < {cos_max}"
            );
            assert!(
                ((pdf as f64) - expected_pdf).abs() <= 1e-3 * expected_pdf,
                "pdf {pdf} vs the cone's {expected_pdf}"
            );
            // And the light hands NEE exactly that point and density.
            let s = light.sample_li(from, u, v).expect("reachable");
            assert_eq!(s.pdf, pdf);
            assert!(s.direction.dot((p - from).normalize()) > 1.0 - 1e-5);
        }
    }
}

/// Uniform in solid angle: `cos θ` off the axis is uniform on
/// `[cos θ_max, 1]` and the azimuth is uniform on `[0, 2π)`. A sampler that
/// put the right points on the cap with the wrong density would pass the
/// geometry test above and bias every render.
#[test]
fn sphere_light_cone_samples_are_uniform_in_solid_angle() {
    let (center, radius, from) = CONE_CASES[1];
    let shape = SphereShape { center, radius };
    let (axis, cos_max) = subtended_cone(center, radius, from);
    // Any frame around the axis: the azimuth is measured in it.
    let t = axis.any_orthonormal_vector();
    let b = axis.cross(t);
    const N: usize = 80_000;
    const BINS: usize = 10;
    let (mut theta_bins, mut phi_bins) = ([0usize; BINS], [0usize; BINS]);
    let mut rng = Rng::new(12);
    for _ in 0..N {
        let (p, _) = shape
            .sample_solid_angle(from, rng.next_f32(), rng.next_f32())
            .unwrap();
        let w = (p - from).normalize();
        let x = (w.dot(axis) as f64 - cos_max) / (1.0 - cos_max);
        theta_bins[((x * BINS as f64) as usize).min(BINS - 1)] += 1;
        let phi = (w.dot(b) as f64)
            .atan2(w.dot(t) as f64)
            .rem_euclid(std::f64::consts::TAU);
        phi_bins[((phi / std::f64::consts::TAU * BINS as f64) as usize).min(BINS - 1)] += 1;
    }
    let expected = (N / BINS) as f64;
    for (name, bins) in [("cos θ", theta_bins), ("φ", phi_bins)] {
        for (i, &n) in bins.iter().enumerate() {
            // 8 000 per bin; 5% is over four standard deviations.
            assert!(
                ((n as f64) - expected).abs() < 0.05 * expected,
                "{name} bin {i}: {n} samples, expected {expected}"
            );
        }
    }
}

/// End to end: a unit-radiance sphere seen from a surface facing its centre
/// delivers `E = π sin² θ_max`, and the cone estimator `L cos θ / pdf`
/// recovers it — near, far, and in the small-angle branch.
#[test]
fn sphere_light_cone_estimates_the_analytic_irradiance() {
    for (center, radius, from) in CONE_CASES {
        let light = sphere_light(center, radius, Vec3A::ONE, 0);
        let (axis, cos_max) = subtended_cone(center, radius, from);
        let exact = std::f64::consts::PI * (1.0 - cos_max * cos_max);
        const K: usize = 128;
        let mut sum = 0.0f64;
        for i in 0..K {
            for j in 0..K {
                let u = (i as f32 + 0.5) / K as f32;
                let v = (j as f32 + 0.5) / K as f32;
                let s = light.sample_li(from, u, v).unwrap();
                let cos = s.direction.dot(axis).max(0.0);
                sum += (s.radiance.x * cos / s.pdf) as f64;
            }
        }
        let estimate = sum / (K * K) as f64;
        assert!(
            (estimate - exact).abs() <= 2e-3 * exact,
            "radius {radius} at distance {}: {estimate} vs π sin²θ = {exact}",
            (center - from).length()
        );
    }
}

/// Inside the sphere there is no cone: it falls back to area sampling, and
/// both MIS sides agree that it did.
#[test]
fn sphere_light_from_inside_falls_back_to_area_sampling() {
    let center = Vec3A::new(0.0, 1.0, 0.0);
    let shape = SphereShape {
        center,
        radius: 2.0,
    };
    let from = Vec3A::new(0.5, 0.5, 0.0);
    assert!(shape.sample_solid_angle(from, 0.3, 0.6).is_none());
    assert!(
        shape
            .solid_angle_pdf(from, center + Vec3A::Y * 2.0)
            .is_none()
    );

    let light = sphere_light(center, 2.0, Vec3A::ONE, 0);
    let mut rng = Rng::new(13);
    for _ in 0..200 {
        let s = light
            .sample_li(from, rng.next_f32(), rng.next_f32())
            .unwrap();
        let p = from + s.direction * s.distance;
        assert!(approx((p - center).length(), 2.0, 1e-3));
        let pdf = light.pdf_at_point(from, p);
        assert!(
            approx(pdf, s.pdf, 1e-3 * s.pdf.max(1.0)),
            "{pdf} vs {}",
            s.pdf
        );
    }
}

// ---------------------------------------------------------------------------
// Distant light
// ---------------------------------------------------------------------------

#[test]
fn distant_light_samples_lie_in_its_cone_at_infinity() {
    let toward = Vec3A::new(0.0, -1.0, 0.0); // light travels straight down
    let light = DistantLight::new(toward, Vec3A::splat(3.0), 10.0);
    let cos_half = 5f32.to_radians().cos();
    let mut rng = Rng::new(8);
    for _ in 0..500 {
        let s = light
            .sample_li(Vec3A::ZERO, rng.next_f32(), rng.next_f32())
            .unwrap();
        assert!(approx(s.direction.length(), 1.0, 1e-5));
        assert!(
            s.direction.dot(-toward) >= cos_half - 1e-5,
            "outside the cone: {}",
            s.direction
        );
        assert_eq!(s.distance, f32::INFINITY);
        assert!(s.pdf > 0.0);
    }
    assert!(light.geom_id().is_none());
}

#[test]
fn distant_light_pdf_is_the_inverse_cone_solid_angle() {
    let light = DistantLight::new(-Vec3A::Y, Vec3A::ONE, 10.0);
    let omega = 2.0 * std::f32::consts::PI * (1.0 - 5f32.to_radians().cos());
    let s = light.sample_li(Vec3A::ZERO, 0.3, 0.3).unwrap();
    assert!(
        approx(s.pdf, 1.0 / omega, 1e-3 / omega),
        "{} vs {}",
        s.pdf,
        1.0 / omega
    );
}

#[test]
fn distant_light_new_takes_irradiance_with_radiance_takes_nits() {
    // `new`: radiance × the cone's cosine-weighted solid angle (π sin²θ) is
    // the authored irradiance, so widening the cone dims the source without
    // changing what lands on a surface facing it.
    let e = Vec3A::new(2.0, 1.0, 0.5);
    for angle in [0.5f32, 5.0, 30.0] {
        let light = DistantLight::new(-Vec3A::Y, e, angle);
        let s = light.sample_li(Vec3A::ZERO, 0.5, 0.5).unwrap();
        let half = 0.5 * angle.to_radians();
        let back = s.radiance * projected_cone_solid_angle(half);
        assert!(
            back.abs_diff_eq(e, 1e-3 * e.max_element()),
            "angle {angle}: {back}"
        );
    }
    // Measured from the sampler's own cone rather than the helper: the pdf is
    // 1/Ω, Ω = 2π(1 − c) names the cone's cosine c, and uniform-in-cosine
    // directions average (1 + c)/2 against a facing surface. What the light
    // delivers must be the authored irradiance to f32 precision — including
    // at the sun's size, where rounding the cone's cosine is 2.4e-4 of Ω.
    for angle in [0.0f32, 0.53, 1.5, 30.0] {
        let light = DistantLight::new(-Vec3A::Y, e, angle);
        let s = light.sample_li(Vec3A::ZERO, 0.5, 0.5).unwrap();
        let omega = 1.0 / s.pdf as f64;
        let c = 1.0 - omega / (2.0 * std::f64::consts::PI);
        let delivered = s.radiance.x as f64 * omega * (1.0 + c) / 2.0;
        assert!(
            (delivered / e.x as f64 - 1.0).abs() < 2e-5,
            "angle {angle}: delivers {delivered}, authored {}",
            e.x
        );
    }
    // `with_radiance`: the argument is the radiance itself, at any angle.
    let l = DistantLight::with_radiance(-Vec3A::Y, e, 5.0);
    assert_eq!(l.sample_li(Vec3A::ZERO, 0.5, 0.5).unwrap().radiance, e);
}

#[test]
fn distant_light_zero_angle_is_widened_not_singular() {
    let light = DistantLight::new(-Vec3A::Y, Vec3A::ONE, 0.0);
    let s = light.sample_li(Vec3A::ZERO, 0.5, 0.5).unwrap();
    assert!(s.pdf.is_finite());
    assert!(s.radiance.is_finite());
    // The documented floor is an angular diameter of 0.05 degrees.
    let floor = 2.0 * std::f32::consts::PI * (1.0 - (0.5f32 * 0.05).to_radians().cos());
    assert!(
        approx(s.pdf, 1.0 / floor, 1e-2 / floor),
        "{} vs {}",
        s.pdf,
        1.0 / floor
    );
}

#[test]
fn distant_light_escaped_agrees_with_sampling_inside_the_cone_only() {
    let toward = Vec3A::new(1.0, -1.0, 0.0).normalize();
    let light = DistantLight::new(toward * 3.0, Vec3A::splat(4.0), 20.0);
    let s = light.sample_li(Vec3A::ZERO, 0.2, 0.7).unwrap();
    let (radiance, pdf) = light
        .escaped(Vec3A::ZERO, s.direction)
        .expect("a sampled direction is covered");
    assert_eq!(radiance, s.radiance);
    assert_eq!(pdf, s.pdf);
    // Straight back along the light is the cone axis.
    assert!(light.escaped(Vec3A::ZERO, -toward).is_some());
    // Perpendicular and opposite directions are outside.
    assert!(light.escaped(Vec3A::ZERO, Vec3A::Z).is_none());
    assert!(light.escaped(Vec3A::ZERO, toward).is_none());
    // Just past the half angle.
    let half = 10f32.to_radians();
    let tilted = (-toward) * (half + 0.01).cos() + Vec3A::Z * (half + 0.01).sin();
    assert!(light.escaped(Vec3A::ZERO, tilted).is_none());
    let inside = (-toward) * (half - 0.01).cos() + Vec3A::Z * (half - 0.01).sin();
    assert!(light.escaped(Vec3A::ZERO, inside).is_some());
}

// ---------------------------------------------------------------------------
// Dome light
// ---------------------------------------------------------------------------

#[test]
fn uniform_dome_samples_the_whole_sphere_uniformly() {
    let tint = Vec3A::new(0.5, 0.6, 0.7);
    let dome = DomeLight::new(tint, None, Mat3A::IDENTITY);
    let quarter_pi_inv = 1.0 / (4.0 * std::f32::consts::PI);
    let mut rng = Rng::new(10);
    let mut below = 0;
    for _ in 0..2000 {
        let s = dome
            .sample_li(Vec3A::ZERO, rng.next_f32(), rng.next_f32())
            .unwrap();
        assert!(approx(s.direction.length(), 1.0, 1e-5));
        assert!(approx(s.pdf, quarter_pi_inv, 1e-7));
        assert_eq!(s.radiance, tint);
        assert_eq!(s.distance, f32::INFINITY);
        if s.direction.y < 0.0 {
            below += 1;
        }
    }
    assert!(
        (below as f32 / 2000.0 - 0.5).abs() < 0.05,
        "a dome is a full sphere, not a hemisphere"
    );
    assert!(dome.geom_id().is_none());
    assert_eq!(dome.pdf_at_point(Vec3A::ZERO, Vec3A::Y), 0.0);
}

#[test]
fn uniform_dome_answers_every_escaping_ray() {
    let tint = Vec3A::splat(2.0);
    let dome = DomeLight::new(tint, None, Mat3A::IDENTITY);
    for d in [Vec3A::X, -Vec3A::Y, Vec3A::new(0.3, 0.4, -0.5).normalize()] {
        let (r, pdf) = dome
            .escaped(Vec3A::ZERO, d)
            .expect("a dome covers every direction");
        assert_eq!(r, tint);
        assert!(approx(pdf, 1.0 / (4.0 * std::f32::consts::PI), 1e-7));
    }
}

/// A 2×1 map: the left texel red, the right texel green.
fn two_texel_map() -> Arc<EnvironmentMap> {
    Arc::new(
        EnvironmentMap::new(
            2,
            1,
            vec![Vec3A::new(1.0, 0.0, 0.0), Vec3A::new(0.0, 1.0, 0.0)],
        )
        .unwrap(),
    )
}

#[test]
fn textured_dome_looks_up_the_map_by_direction() {
    let dome = DomeLight::new(Vec3A::splat(2.0), Some(two_texel_map()), Mat3A::IDENTITY);
    // -Z is the centre of the lat-long image → the right-hand texel.
    let (minus_z, _) = dome.escaped(Vec3A::ZERO, -Vec3A::Z).unwrap();
    assert_eq!(minus_z, Vec3A::new(0.0, 2.0, 0.0), "tint × green");
    // +Z wraps to the seam → the left-hand texel.
    let (plus_z, _) = dome.escaped(Vec3A::ZERO, Vec3A::Z).unwrap();
    assert_eq!(plus_z, Vec3A::new(2.0, 0.0, 0.0), "tint × red");
}

#[test]
fn rotating_the_dome_rotates_the_sky() {
    let rot = Mat3A::from_rotation_y(std::f32::consts::PI);
    let dome = DomeLight::new(Vec3A::ONE, Some(two_texel_map()), rot);
    // The 180° turn swaps what +Z and -Z see.
    let (minus_z, _) = dome.escaped(Vec3A::ZERO, -Vec3A::Z).unwrap();
    assert_eq!(minus_z, Vec3A::new(1.0, 0.0, 0.0));
    let (plus_z, _) = dome.escaped(Vec3A::ZERO, Vec3A::Z).unwrap();
    assert_eq!(plus_z, Vec3A::new(0.0, 1.0, 0.0));
}

#[test]
fn textured_dome_sample_and_escaped_share_one_density() {
    let map = Arc::new(EnvironmentMap::new(1, 1, vec![Vec3A::splat(3.0)]).unwrap());
    let dome = DomeLight::new(Vec3A::splat(0.5), Some(map), Mat3A::from_rotation_x(0.4));
    let mut rng = Rng::new(11);
    for _ in 0..300 {
        let s = dome
            .sample_li(Vec3A::ZERO, rng.next_f32(), rng.next_f32())
            .unwrap();
        assert!(approx(s.direction.length(), 1.0, 1e-4));
        assert_eq!(s.radiance, Vec3A::splat(1.5));
        let (r, pdf) = dome.escaped(Vec3A::ZERO, s.direction).unwrap();
        assert_eq!(r, s.radiance);
        assert!(approx(pdf, s.pdf, 2e-2 * s.pdf), "{pdf} vs {}", s.pdf);
    }
}

#[test]
fn textured_dome_importance_samples_a_bright_sun() {
    let (w, h) = (16, 8);
    let mut px = vec![Vec3A::splat(0.01); w * h];
    px[3 * w + 5] = Vec3A::splat(1000.0);
    let map = Arc::new(EnvironmentMap::new(w, h, px).unwrap());
    let dome = DomeLight::new(Vec3A::ONE, Some(map), Mat3A::IDENTITY);
    let mut rng = Rng::new(12);
    let mut sun = 0;
    for _ in 0..1000 {
        let s = dome
            .sample_li(Vec3A::ZERO, rng.next_f32(), rng.next_f32())
            .unwrap();
        if s.radiance.x > 1.0 {
            sun += 1;
            // One texel of a 16x8 map near the equator covers ~0.15 sr,
            // so a sample landing on it carries a density around 6.5.
            assert!(
                s.pdf > 5.0,
                "the sun texel must carry a high pdf: {}",
                s.pdf
            );
        }
    }
    assert!(sun > 900, "only {sun} of 1000 samples found the sun");
}

#[test]
fn a_black_textured_dome_cannot_be_sampled() {
    let map = Arc::new(EnvironmentMap::new(2, 2, vec![Vec3A::ZERO; 4]).unwrap());
    let dome = DomeLight::new(Vec3A::ONE, Some(map), Mat3A::IDENTITY);
    assert!(dome.sample_li(Vec3A::ZERO, 0.5, 0.5).is_none());
    // But an escaping ray still gets an answer (black, pdf 0).
    let (r, pdf) = dome.escaped(Vec3A::ZERO, Vec3A::X).unwrap();
    assert_eq!(r, Vec3A::ZERO);
    assert_eq!(pdf, 0.0);
}

// ---------------------------------------------------------------------------
// Light list
// ---------------------------------------------------------------------------

#[test]
fn empty_light_list_picks_nothing() {
    let l = LightList::new();
    assert_eq!(l.count(), 0);
    assert!(l.pick(0.0).is_none());
    assert!(l.pick(0.99).is_none());
    assert!(l.find_by_geom(0).is_none());
    let d = LightList::default();
    assert_eq!(d.count(), 0);
}

#[test]
fn light_list_picks_uniformly_by_index() {
    let mut l = LightList::new();
    for id in 0..4 {
        l.add(Arc::new(sphere_light(Vec3A::ZERO, 1.0, Vec3A::ONE, id)));
    }
    assert_eq!(l.count(), 4);
    assert_eq!(l.pick(0.0).unwrap().geom_id(), Some(0));
    assert_eq!(l.pick(0.26).unwrap().geom_id(), Some(1));
    assert_eq!(l.pick(0.5).unwrap().geom_id(), Some(2));
    assert_eq!(l.pick(0.99).unwrap().geom_id(), Some(3));
    // u rounding up to len must still pick the last light.
    assert_eq!(l.pick(1.0).unwrap().geom_id(), Some(3));
}

#[test]
fn light_list_finds_lights_by_geometry_id() {
    let mut l = LightList::new();
    l.add(Arc::new(sphere_light(Vec3A::ZERO, 1.0, Vec3A::ONE, 12)));
    l.add(Arc::new(DistantLight::new(-Vec3A::Y, Vec3A::ONE, 1.0)));
    l.add(Arc::new(sphere_light(Vec3A::ZERO, 1.0, Vec3A::ONE, 40)));
    assert!(l.find_by_geom(12).is_some());
    assert!(l.find_by_geom(40).is_some());
    assert!(
        l.find_by_geom(13).is_none(),
        "an unrelated geometry is not a light"
    );
    assert_eq!(l.find_by_geom(40).unwrap().geom_id(), Some(40));
}

#[test]
fn light_list_exposes_its_vector() {
    let mut l = LightList::new();
    l.add(Arc::new(DomeLight::new(Vec3A::ONE, None, Mat3A::IDENTITY)));
    assert_eq!(l.lights.len(), 1);
    assert!(l.lights[0].geom_id().is_none());
}
