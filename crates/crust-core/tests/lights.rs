//! Lights and light shapes through the public API: sampling geometry, the
//! MIS pairing between `sample_li` and `pdf_at_point` / `escaped`, the
//! normalisation conventions of the infinite lights, and the light list.

use crust_core::{
    AffineShape, AreaLight, DistantLight, DomeLight, Emissive, EnvironmentMap, Light, LightList,
    LightSelection, LightShape, LightTexture, RectShape, RectTexture, Shaping, SphereShape,
    UnitShape, Vec3A, projected_cone_solid_angle,
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

/// A sheared parallelogram is not a rectangle, so it keeps area sampling,
/// whose solid-angle density is `d² / (cos · A)`.
#[test]
fn sheared_rect_light_pdf_is_distance_squared_over_cosine_area() {
    let rect = RectShape::new(
        Vec3A::new(-0.75, -0.5, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(0.5, 1.0, 0.0),
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
/// far is four times the pdf. (A sheared rect light, because neither a
/// sphere nor a true rectangle seen from its front is area-sampled any more.)
#[test]
fn area_light_pdf_falls_with_the_inverse_square_of_distance() {
    let rect = RectShape::new(
        Vec3A::new(-0.075, -0.05, 0.0),
        Vec3A::new(0.1, 0.0, 0.0),
        Vec3A::new(0.05, 0.1, 0.0),
        Vec3A::Z,
    );
    let light = AreaLight::new(Box::new(rect), Arc::new(Emissive::new(Vec3A::ONE)), 0);
    let near = light.pdf_at_point(Vec3A::new(0.0, 0.0, 2.0), Vec3A::ZERO);
    let far = light.pdf_at_point(Vec3A::new(0.0, 0.0, 4.0), Vec3A::ZERO);
    assert!(approx(far / near, 4.0, 0.01), "{}", far / near);
}

// ---------------------------------------------------------------------------
// Rect lights: sampled by the spherical rectangle they subtend
// ---------------------------------------------------------------------------

/// The solid angle of the triangle `(a, b, c)` seen from the origin, by Van
/// Oosterom & Strackee's formula — independent of the Ureña map under test.
fn triangle_solid_angle(a: glam::DVec3, b: glam::DVec3, c: glam::DVec3) -> f64 {
    let (la, lb, lc) = (a.length(), b.length(), c.length());
    let num = a.dot(b.cross(c)).abs();
    let den = la * lb * lc + a.dot(b) * lc + a.dot(c) * lb + b.dot(c) * la;
    2.0 * num.atan2(den)
}

/// The solid angle, seen from `from`, of the part `[s0, s1] × [t0, t1]` of the
/// parallelogram `origin + s·eu + t·ev`, as two triangles.
fn rect_solid_angle(
    from: Vec3A,
    origin: Vec3A,
    eu: Vec3A,
    ev: Vec3A,
    (s0, s1): (f64, f64),
    (t0, t1): (f64, f64),
) -> f64 {
    let d = |v: Vec3A| glam::DVec3::new(v.x as f64, v.y as f64, v.z as f64);
    let (o, eu, ev) = (d(origin - from), d(eu), d(ev));
    let p = |s: f64, t: f64| o + s * eu + t * ev;
    triangle_solid_angle(p(s0, t0), p(s1, t0), p(s1, t1))
        + triangle_solid_angle(p(s0, t0), p(s1, t1), p(s0, t1))
}

/// A 2 x 1 panel in the z = 0 plane, emitting toward +Z, and a shading point
/// near it and off its axis — where area sampling is at its worst.
fn near_rect() -> (Vec3A, Vec3A, Vec3A, Vec3A) {
    (
        Vec3A::new(-1.0, -0.5, 0.0),
        Vec3A::new(2.0, 0.0, 0.0),
        Vec3A::new(0.0, 1.0, 0.0),
        Vec3A::new(0.7, -0.2, 0.35),
    )
}

#[test]
fn rect_light_pdf_is_the_inverse_subtended_solid_angle() {
    let rect = RectShape::new(
        Vec3A::new(-0.5, -0.5, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(0.0, 1.0, 0.0),
        Vec3A::Z,
    );
    let light = AreaLight::new(Box::new(rect), Arc::new(Emissive::new(Vec3A::ONE)), 1);
    // Above the centre of a 2a x 2b rectangle at height h the solid angle is
    // 4 asin(ab / √((a² + h²)(b² + h²))).
    let omega = 4.0 * (0.25f64 / (4.25f64 * 4.25).sqrt()).asin();
    let pdf = light.pdf_at_point(Vec3A::new(0.0, 0.0, 2.0), Vec3A::ZERO);
    assert!(
        ((pdf as f64) * omega - 1.0).abs() < 1e-5,
        "{pdf} vs {}",
        1.0 / omega
    );
    // Off axis, against the two-triangle formula.
    let (origin, eu, ev, from) = near_rect();
    let light = AreaLight::new(
        Box::new(RectShape::new(origin, eu, ev, Vec3A::Z)),
        Arc::new(Emissive::new(Vec3A::ONE)),
        1,
    );
    let omega = rect_solid_angle(from, origin, eu, ev, (0.0, 1.0), (0.0, 1.0));
    let pdf = light.pdf_at_point(from, Vec3A::ZERO);
    assert!(
        ((pdf as f64) * omega - 1.0).abs() < 1e-5,
        "{pdf} vs {omega}"
    );
    // From beyond each edge and corner, where the corner angles change sign,
    // and far enough that the solid angle is just above the point where area
    // sampling takes over and `Σg − 2π` would cancel in f32.
    for from in [
        Vec3A::new(3.0, 0.2, 0.5),
        Vec3A::new(-2.5, -1.5, 0.2),
        Vec3A::new(0.3, 2.0, 1.5),
        Vec3A::new(-1.2, 0.9, 0.05),
        Vec3A::new(40.0, -30.0, 120.0),
    ] {
        let omega = rect_solid_angle(from, origin, eu, ev, (0.0, 1.0), (0.0, 1.0));
        let pdf = light.pdf_at_point(from, Vec3A::ZERO);
        assert!(
            ((pdf as f64) * omega - 1.0).abs() < 1e-5,
            "from {from}: {pdf} vs {omega}"
        );
    }
    // And the bounce side agrees with every sample's own density.
    let mut rng = Rng::new(21);
    for _ in 0..200 {
        let s = light
            .sample_li(from, rng.next_f32(), rng.next_f32())
            .unwrap();
        let p = from + s.direction * s.distance;
        assert!(p.z.abs() < 1e-5 && p.x.abs() <= 1.0 + 1e-5 && p.y.abs() <= 0.5 + 1e-5);
        assert_eq!(s.pdf, light.pdf_at_point(from, p));
    }
}

/// Uniform in solid angle means each cell of the rectangle catches samples in
/// proportion to the solid angle *it* subtends — measured with an independent
/// formula, from a point close enough that the cells' solid angles differ by
/// an order of magnitude.
#[test]
fn rect_light_samples_are_uniform_in_solid_angle() {
    let (origin, eu, ev, from) = near_rect();
    let rect = RectShape::new(origin, eu, ev, Vec3A::Z);
    const N: usize = 4;
    const SAMPLES: usize = 400_000;
    let mut counts = [[0usize; N]; N];
    let mut rng = Rng::new(5);
    for _ in 0..SAMPLES {
        let (p, _) = rect
            .sample_solid_angle(from, rng.next_f32(), rng.next_f32())
            .unwrap();
        let s = ((p - origin).dot(eu) / eu.length_squared()).clamp(0.0, 0.999_999);
        let t = ((p - origin).dot(ev) / ev.length_squared()).clamp(0.0, 0.999_999);
        counts[(s * N as f32) as usize][(t * N as f32) as usize] += 1;
    }
    let total = rect_solid_angle(from, origin, eu, ev, (0.0, 1.0), (0.0, 1.0));
    let cell = 1.0 / N as f64;
    let (mut smallest, mut largest) = (f64::MAX, 0.0f64);
    for (i, row) in counts.iter().enumerate() {
        for (j, &count) in row.iter().enumerate() {
            let (s0, t0) = (i as f64 * cell, j as f64 * cell);
            let expected =
                rect_solid_angle(from, origin, eu, ev, (s0, s0 + cell), (t0, t0 + cell)) / total;
            smallest = smallest.min(expected);
            largest = largest.max(expected);
            let got = count as f64 / SAMPLES as f64;
            // Five binomial standard deviations.
            let tol = 5.0 * (expected / SAMPLES as f64).sqrt();
            assert!(
                (got - expected).abs() < tol,
                "cell ({i}, {j}): {got} vs {expected}"
            );
        }
    }
    assert!(largest > 10.0 * smallest, "{smallest} .. {largest}");
}

/// A unit-radiance rect light estimates the irradiance on a tilted receiver:
/// through the light's own sampler, against plain area quadrature.
#[test]
fn rect_light_estimates_the_irradiance() {
    let (origin, eu, ev, from) = near_rect();
    let light = AreaLight::new(
        Box::new(RectShape::new(origin, eu, ev, Vec3A::Z)),
        Arc::new(Emissive::light(Vec3A::ONE, None)),
        1,
    );
    let receiver = Vec3A::new(-0.4, 0.2, -1.0).normalize();
    let mut quadrature = 0.0f64;
    const Q: usize = 800;
    let area = (eu.cross(ev).length() / (Q * Q) as f32) as f64;
    for i in 0..Q {
        for j in 0..Q {
            let p =
                origin + eu * ((i as f32 + 0.5) / Q as f32) + ev * ((j as f32 + 0.5) / Q as f32);
            let w = p - from;
            let r2 = w.length_squared() as f64;
            let w = w.normalize();
            let cos_x = receiver.dot(w).max(0.0) as f64;
            let cos_l = (-w).dot(Vec3A::Z).max(0.0) as f64;
            quadrature += cos_x * cos_l * area / r2;
        }
    }
    let mut estimate = 0.0f64;
    const M: usize = 256;
    for i in 0..M {
        for j in 0..M {
            let s = light
                .sample_li(
                    from,
                    (i as f32 + 0.5) / M as f32,
                    (j as f32 + 0.5) / M as f32,
                )
                .unwrap();
            estimate += (s.radiance.x * receiver.dot(s.direction).max(0.0) / s.pdf) as f64;
        }
    }
    estimate /= (M * M) as f64;
    assert!(
        (estimate / quadrature - 1.0).abs() < 2e-3,
        "{estimate} vs {quadrature}"
    );
}

/// The map samples an exact rectangle and places the point through the
/// light's own edges, so a shear it accepts is a bias. One far below the
/// tolerance — what f32 leaves on a rotated light — still takes the map and
/// reports the solid angle of the parallelogram actually sampled; one just
/// above it is area-sampled on both hooks.
#[test]
fn rect_light_takes_the_map_only_when_the_edges_are_perpendicular() {
    let (origin, eu, ev, from) = near_rect();
    let slight = ev + 2e-7 * eu;
    let light = AreaLight::new(
        Box::new(RectShape::new(origin, eu, slight, Vec3A::Z)),
        Arc::new(Emissive::new(Vec3A::ONE)),
        1,
    );
    let omega = rect_solid_angle(from, origin, eu, slight, (0.0, 1.0), (0.0, 1.0));
    let pdf = light.pdf_at_point(from, Vec3A::ZERO);
    assert!(
        ((pdf as f64) * omega - 1.0).abs() < 1e-5,
        "{pdf} vs {omega}"
    );

    let sheared = RectShape::new(origin, eu, ev + 2e-6 * eu, Vec3A::Z);
    assert!(sheared.sample_solid_angle(from, 0.3, 0.6).is_none());
    assert!(sheared.solid_angle_pdf(from, Vec3A::ZERO).is_none());
}

/// Area sampling stays wherever the map does not apply or does not pay: a
/// sheared parallelogram, a shading point behind the one-sided light or on
/// its plane, and a light too small to be worth it. Both MIS sides fall back
/// together.
#[test]
fn rect_light_falls_back_to_area_sampling() {
    let (origin, eu, ev, from) = near_rect();
    let square = RectShape::new(origin, eu, ev, Vec3A::Z);
    let sheared = RectShape::new(origin, eu, ev + 0.3 * eu, Vec3A::Z);
    let tiny = RectShape::new(
        Vec3A::ZERO,
        Vec3A::new(0.01, 0.0, 0.0),
        Vec3A::new(0.0, 0.01, 0.0),
        Vec3A::Z,
    );
    let behind = Vec3A::new(0.1, 0.2, -0.5);
    let on_plane = Vec3A::new(3.0, 0.0, 0.0);
    let far = Vec3A::new(0.0, 0.0, 5.0);
    for (shape, from) in [
        (&sheared, from),
        (&square, behind),
        (&square, on_plane),
        (&tiny, far),
    ] {
        assert!(shape.sample_solid_angle(from, 0.3, 0.6).is_none());
        assert!(shape.solid_angle_pdf(from, Vec3A::ZERO).is_none());
    }
    assert!(square.sample_solid_angle(from, 0.3, 0.6).is_some());
    // The same tiny light, near enough to subtend more than the threshold.
    assert!(
        tiny.sample_solid_angle(Vec3A::new(0.0, 0.0, 0.5), 0.3, 0.6)
            .is_some()
    );
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

// ---------------------------------------------------------------------------
// Squashed sphere lights: the unit sphere's cone, mapped through the placement
// ---------------------------------------------------------------------------

/// `usdlux.usda`'s ellipsoid (a 0.25 sphere scaled (3, 0.4, 0.4)), tilted so
/// no axis lines up with the world's.
fn ellipsoid_placement() -> glam::Affine3A {
    glam::Affine3A::from_scale_rotation_translation(
        glam::Vec3::new(0.75, 0.1, 0.1),
        glam::Quat::from_euler(glam::EulerRot::XYZ, 0.3, -0.5, 0.8),
        glam::Vec3::new(0.0, 2.0, -1.0),
    )
}

fn ellipsoid_light() -> (AffineShape, AreaLight) {
    let m = ellipsoid_placement();
    let shape = AffineShape::new(UnitShape::Sphere, m).expect("invertible");
    let light = AreaLight::new(
        Box::new(AffineShape::new(UnitShape::Sphere, m).unwrap()),
        Arc::new(Emissive::new(Vec3A::ONE)),
        0,
    );
    (shape, light)
}

/// Shading points near the long side, off the tip, and far away.
const ELLIPSOID_VIEWS: [Vec3A; 3] = [
    Vec3A::new(0.1, 2.3, -0.8),
    Vec3A::new(1.2, 2.4, -1.5),
    Vec3A::new(-6.0, 9.0, 4.0),
];

/// Every sample lies on the ellipsoid, on the side facing the shading point,
/// and both MIS sides agree on its (now point-dependent) density.
#[test]
fn ellipsoid_light_samples_only_its_visible_side() {
    let (shape, light) = ellipsoid_light();
    let to_local = ellipsoid_placement().inverse();
    for from in ELLIPSOID_VIEWS {
        let mut rng = Rng::new(21);
        for _ in 0..2000 {
            let (u, v) = (rng.next_f32(), rng.next_f32());
            let (p, pdf) = shape
                .sample_solid_angle(from, u, v)
                .expect("outside the ellipsoid there is always a cone");
            let local = to_local.transform_point3a(p);
            assert!(
                approx(local.length(), 1.0, 1e-4),
                "off the surface: local |p| = {}",
                local.length()
            );
            let facing = shape.normal_at(p).dot((from - p).normalize());
            assert!(facing >= -1e-3, "sampled the far side: cos = {facing}");
            let bounce = light.pdf_at_point(from, p);
            assert!(
                approx(bounce, pdf, 1e-3 * pdf),
                "MIS sides disagree: {pdf} vs {bounce}"
            );
            let s = light.sample_li(from, u, v).unwrap();
            assert_eq!(s.pdf, pdf);
        }
    }
}

/// The Jacobian is right: the solid angle the ellipsoid subtends
/// (`E[1/pdf]`) and the irradiance on a tilted receiver (`E[cos⁺/pdf]`) come
/// out the same from the cone sampler as from plain area quadrature over the
/// facing side. A wrong `|Mω|³ / |det M|` factor changes both.
#[test]
fn ellipsoid_light_cone_matches_area_quadrature() {
    let (shape, light) = ellipsoid_light();
    let receiver = Vec3A::new(0.3, 1.0, 0.2).normalize();
    for from in ELLIPSOID_VIEWS {
        const K: usize = 128;
        let (mut omega_cone, mut e_cone) = (0.0f64, 0.0f64);
        for i in 0..K {
            for j in 0..K {
                let u = (i as f32 + 0.5) / K as f32;
                let v = (j as f32 + 0.5) / K as f32;
                let s = light.sample_li(from, u, v).unwrap();
                omega_cone += 1.0 / s.pdf as f64;
                e_cone += (s.direction.dot(receiver).max(0.0) / s.pdf) as f64;
            }
        }
        let n = (K * K) as f64;
        let (omega_cone, e_cone) = (omega_cone / n, e_cone / n);

        // dω = cos θ_l dA / r², over the part of the surface facing `from`.
        const A: usize = 512;
        let (mut omega_area, mut e_area) = (0.0f64, 0.0f64);
        for i in 0..A {
            for j in 0..A {
                let u = (i as f32 + 0.5) / A as f32;
                let v = (j as f32 + 0.5) / A as f32;
                let p = shape.sample_point(u, v);
                let d = from - p;
                let r2 = d.length_squared();
                let w = d / r2.sqrt();
                let cos_l = shape.normal_at(p).dot(w);
                if cos_l <= 0.0 {
                    continue;
                }
                let dw = (cos_l * shape.inv_pdf_area(p) / r2) as f64;
                omega_area += dw;
                e_area += dw * (-w).dot(receiver).max(0.0) as f64;
            }
        }
        let a = (A * A) as f64;
        let (omega_area, e_area) = (omega_area / a, e_area / a);

        assert!(
            (omega_cone - omega_area).abs() <= 5e-3 * omega_area,
            "from {from}: solid angle {omega_cone} (cone) vs {omega_area} (area)"
        );
        assert!(
            (e_cone - e_area).abs() <= 5e-3 * e_area.max(1e-6),
            "from {from}: irradiance {e_cone} (cone) vs {e_area} (area)"
        );
    }
}

/// Inside the ellipsoid, and for the flat and tubular unit shapes, there is
/// no cone: area sampling, on both MIS sides.
#[test]
fn affine_shapes_without_a_cone_fall_back_to_area_sampling() {
    let (shape, light) = ellipsoid_light();
    let inside = ellipsoid_placement().transform_point3a(Vec3A::new(0.3, 0.2, -0.1));
    assert!(shape.sample_solid_angle(inside, 0.4, 0.6).is_none());
    assert!(shape.solid_angle_pdf(inside, Vec3A::ZERO).is_none());
    let mut rng = Rng::new(22);
    for _ in 0..200 {
        let s = light
            .sample_li(inside, rng.next_f32(), rng.next_f32())
            .unwrap();
        let p = inside + s.direction * s.distance;
        let bounce = light.pdf_at_point(inside, p);
        assert!(approx(bounce, s.pdf, 1e-3 * s.pdf.max(1.0)));
    }

    for unit in [UnitShape::Disk, UnitShape::Cylinder] {
        let flat = AffineShape::new(unit, ellipsoid_placement()).unwrap();
        let from = Vec3A::new(-6.0, 9.0, 4.0);
        assert!(flat.sample_solid_angle(from, 0.5, 0.5).is_none());
        assert!(flat.solid_angle_pdf(from, Vec3A::ZERO).is_none());
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
    assert_eq!(l.selection(), LightSelection::Uniform);
    assert_eq!(l.pick(0.0).unwrap().0.geom_id(), Some(0));
    assert_eq!(l.pick(0.26).unwrap().0.geom_id(), Some(1));
    assert_eq!(l.pick(0.5).unwrap().0.geom_id(), Some(2));
    assert_eq!(l.pick(0.99).unwrap().0.geom_id(), Some(3));
    // u rounding up to len must still pick the last light.
    assert_eq!(l.pick(1.0).unwrap().0.geom_id(), Some(3));
    assert_eq!(l.pick(0.5).unwrap().1, 0.25);
}

/// A rect light facing −Z (its emitting side), of the given radiance.
fn rect_light(radiance: f32, shaping: Option<Shaping>, geom_id: u32) -> AreaLight {
    let rect = RectShape::new(
        Vec3A::new(-0.5, -1.0, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(0.0, 2.0, 0.0),
        -Vec3A::Z,
    );
    AreaLight::new(
        Box::new(rect),
        Arc::new(Emissive::light(Vec3A::splat(radiance), shaping)),
        geom_id,
    )
}

/// Power is flux: `π A L` for a one-sided Lambertian emitter of any shape,
/// twice that for a two-sided one. Lights at infinity have none to compare.
#[test]
fn light_power_is_the_flux_it_emits() {
    let pi = std::f32::consts::PI;
    let rel = |a: Option<f32>, b: f32| (a.expect("finite light") - b).abs() / b;
    // A 1 × 2 rect, one-sided.
    assert!(rel(rect_light(3.0, None, 0).power(), pi * 2.0 * 3.0) < 1e-5);
    // A unit-radius sphere: `sphere_light` is a plain (two-sided) emitter.
    let s = sphere_light(Vec3A::ZERO, 1.0, Vec3A::splat(2.0), 0);
    assert!(rel(s.power(), 2.0 * pi * 4.0 * pi * 2.0) < 1e-5);
    let sun = DistantLight::new(-Vec3A::Y, Vec3A::splat(5.0), 0.53);
    assert_eq!(sun.power(), None);
    let dome = DomeLight::new(Vec3A::splat(0.5), None, Mat3A::IDENTITY);
    assert_eq!(dome.power(), None);
}

/// A shaped light's flux is the cone it emits into: `π A L sin² θ` for a hard
/// cone of half-angle θ about the normal. The quadrature stops at the cone
/// angle, so a 2° spot is resolved as well as a 60° one — a fixed direction
/// grid would step over it and report no power at all.
#[test]
fn shaped_light_power_is_the_cone_it_emits_into() {
    let pi = std::f32::consts::PI;
    for cone in [60.0f32, 30.0, 2.0] {
        let mut shaping = Shaping::new(Mat3A::IDENTITY);
        shaping.cone_angle_deg = cone;
        let light = rect_light(1.0, Some(shaping), 0);
        let exact = pi * 2.0 * cone.to_radians().sin().powi(2);
        let got = light.power().unwrap();
        assert!(
            (got - exact).abs() <= 0.01 * exact,
            "{cone}° cone: power {got} vs {exact}"
        );
    }
}

/// A textured card's power is its texture's area average times the rest.
#[test]
fn textured_light_power_uses_the_mean_texel() {
    let image =
        Arc::new(LightTexture::new(2, 1, vec![Vec3A::splat(4.0), Vec3A::ZERO]).expect("valid"));
    let (origin, edge_u, edge_v) = (
        Vec3A::new(-0.5, -1.0, 0.0),
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(0.0, 2.0, 0.0),
    );
    let texture = RectTexture::new(image, origin, edge_u, edge_v).unwrap();
    let light = AreaLight::new(
        Box::new(RectShape::new(origin, edge_u, edge_v, -Vec3A::Z)),
        Arc::new(Emissive::light(Vec3A::ONE, None).with_texture(texture)),
        0,
    );
    let expected = std::f32::consts::PI * 2.0 * 2.0; // mean texel 2
    assert!((light.power().unwrap() - expected).abs() < 1e-4 * expected);
}

/// Power selection gives lights at infinity their uniform share, splits the
/// rest between the finite lights half evenly and half by power, never picks
/// a dark one, and every accessor reports the same probability for the same
/// light — the MIS contract between NEE and the bounce side.
#[test]
fn power_selection_picks_by_power_defensively() {
    let mut l = LightList::new();
    l.add(Arc::new(rect_light(1.0, None, 10)));
    l.add(Arc::new(rect_light(0.0, None, 11))); // emits nothing
    l.add(Arc::new(rect_light(3.0, None, 12)));
    l.add(Arc::new(DomeLight::new(Vec3A::ONE, None, Mat3A::IDENTITY)));
    l.select_by(LightSelection::Power);
    assert_eq!(l.selection(), LightSelection::Power);
    // Three lights can be picked: the dome gets 1/3, and the two lit rects
    // split 2/3 as `½ · ½ + ½ · power share` each: (¼ + ⅛, ¼ + ⅜) of it.
    let expected = [2.0 / 3.0 * 0.375, 0.0, 2.0 / 3.0 * 0.625, 1.0 / 3.0];
    for (i, &p) in expected.iter().enumerate() {
        assert!(
            approx(l.pmf(i), p, 1e-6),
            "pmf({i}) = {}, expected {p}",
            l.pmf(i)
        );
    }
    for i in [0u32, 1, 2] {
        let (_, found) = l.find_by_geom(10 + i).unwrap();
        assert_eq!(found, l.pmf(i as usize), "find_by_geom disagrees with pmf");
    }
    let pmfs: Vec<f32> = l.iter().map(|(_, p)| p).collect();
    assert_eq!(pmfs, (0..4).map(|i| l.pmf(i)).collect::<Vec<_>>());

    // Stratified u: the picks land in exactly those proportions, and the
    // dark light is never one of them.
    const N: usize = 12_000;
    let mut counts = [0usize; 4];
    for k in 0..N {
        let (light, pmf) = l.pick((k as f32 + 0.5) / N as f32).unwrap();
        let i = light.geom_id().map_or(3, |id| (id - 10) as usize);
        assert_eq!(pmf, l.pmf(i));
        counts[i] += 1;
    }
    assert_eq!(counts[1], 0, "a light with no power was picked");
    for i in [0, 2, 3] {
        let got = counts[i] as f32 / N as f32;
        assert!(
            (got - expected[i]).abs() < 1e-3,
            "light {i}: {got} vs {}",
            expected[i]
        );
    }
    // The top of the range still lands on a light that can be picked.
    assert_eq!(l.pick(1.0 - f32::EPSILON).unwrap().0.geom_id(), None);
    assert_eq!(l.pick(0.0).unwrap().0.geom_id(), Some(10));
    // The strategy's density is the product, on both MIS sides.
    assert_eq!(l.density(2.0, l.pmf(2)), 2.0 * l.pmf(2));
}

/// Under uniform selection the density is the division it always was, so
/// the default renders bit-identically to the renderer before selection was
/// a choice (`x · (1/3)` and `x / 3` round differently).
#[test]
fn uniform_density_is_the_historical_division() {
    let mut l = LightList::new();
    for id in 0..3 {
        l.add(Arc::new(rect_light(1.0, None, id)));
    }
    let x = 0.7f32;
    assert_eq!(l.density(x, l.pmf(0)), x / 3.0);
}

/// Uniform, all-dark and stale selections all fall back to one in N, so a
/// `LightList` never reports a probability for a table it does not have.
#[test]
fn light_selection_falls_back_to_uniform() {
    let mut l = LightList::new();
    l.add(Arc::new(rect_light(1.0, None, 0)));
    l.add(Arc::new(rect_light(9.0, None, 1)));
    l.select_by(LightSelection::Uniform);
    assert_eq!(l.selection(), LightSelection::Uniform);
    assert_eq!(l.pmf(1), 0.5);

    l.select_by(LightSelection::Power);
    // ½ · ½ even + ½ · 0.9 by power.
    assert!(approx(l.pmf(1), 0.7, 1e-6));
    // Adding a light invalidates the table built over the old list.
    l.add(Arc::new(rect_light(1.0, None, 2)));
    assert_eq!(l.selection(), LightSelection::Uniform);
    assert!(approx(l.pmf(2), 1.0 / 3.0, 1e-6));

    let mut dark = LightList::new();
    dark.add(Arc::new(rect_light(0.0, None, 0)));
    dark.add(Arc::new(rect_light(0.0, None, 1)));
    dark.select_by(LightSelection::Power);
    assert_eq!(dark.selection(), LightSelection::Uniform);
    assert_eq!(dark.pmf(0), 0.5);
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
    assert_eq!(l.find_by_geom(40).unwrap().0.geom_id(), Some(40));
}

#[test]
fn light_list_exposes_its_vector() {
    let mut l = LightList::new();
    l.add(Arc::new(DomeLight::new(Vec3A::ONE, None, Mat3A::IDENTITY)));
    assert_eq!(l.lights().len(), 1);
    assert!(l.lights()[0].geom_id().is_none());
}
