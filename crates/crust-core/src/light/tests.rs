use std::sync::Arc;

use glam::Affine3A;

use crate::material::Emissive;

use super::shape::*;
use super::*;

/// The small-cone branch must draw `1 − cos θ = u (1 − cos θ_max)` — what
/// its constant pdf claims — and not pbrt-v4's `sin² θ = u sin² θ_max`,
/// which is off by up to `sin² θ_max / 4`: 7.5e-5 at `u = ½` just under
/// the threshold, far above f32's resolution here.
#[test]
fn small_cone_samples_are_uniform_in_solid_angle() {
    // sin² θ_max = 6e-4, just under `SMALL_CONE_SIN2`.
    let cone = SubtendedCone::new(Vec3A::ZERO, 6e-4f32.sqrt(), Vec3A::new(0.0, 0.0, 1.0))
        .expect("outside the sphere");
    assert!(cone.sin2_max < SMALL_CONE_SIN2);
    for u in [0.1f32, 0.25, 0.5, 0.75, 0.9, 1.0] {
        let (sin2, cos) = cone.sample(u);
        // `sin² / (1 + cos)` is `1 − cos θ` without the cancellation.
        let ratio = (sin2 / (1.0 + cos)) / (u * cone.one_minus_cos_max);
        assert!((ratio - 1.0).abs() < 1e-5, "u = {u}: ratio {ratio}");
        assert!((sin2 + cos * cos - 1.0).abs() < 1e-6);
    }
    // Both branches agree on `1 − cos θ_max` across the threshold.
    let exact = 1.0 - (1.0 - 6e-4f64).sqrt();
    assert!(((cone.one_minus_cos_max as f64) / exact - 1.0).abs() < 1e-6);
}

/// A cone too thin for its pdf to be finite is no cone: both hooks fall
/// back to area sampling rather than hand MIS an infinite density, whose
/// square is `inf / inf = NaN` in the power heuristic.
#[test]
fn a_cone_whose_pdf_overflows_is_refused() {
    let shape = SphereShape {
        center: Vec3A::new(0.0, 0.0, 10.0),
        radius: 1e-20,
    };
    assert!(shape.sample_solid_angle(Vec3A::ZERO, 0.5, 0.5).is_none());
    assert!(shape.solid_angle_pdf(Vec3A::ZERO, shape.center).is_none());
    // ...while an ordinary tiny, distant sphere still samples its cone,
    // at a finite density.
    let small = SphereShape {
        center: Vec3A::new(0.0, 0.0, 1e3),
        radius: 1e-3,
    };
    let (_, pdf) = small.sample_solid_angle(Vec3A::ZERO, 0.5, 0.5).unwrap();
    assert!(pdf.is_finite() && pdf > 0.0);
    assert_eq!(small.solid_angle_pdf(Vec3A::ZERO, small.center), Some(pdf));
}

#[test]
fn sphere_shape_samples_lie_on_surface() {
    let shape = SphereShape {
        center: Vec3A::new(1.0, 2.0, 3.0),
        radius: 0.5,
    };
    for (u, v) in [(0.0, 0.0), (0.25, 0.75), (0.99, 0.5), (0.5, 0.01)] {
        let p = shape.sample_point(u, v);
        let d = (p - shape.center).length();
        assert!((d - shape.radius).abs() < 1e-5, "sample off surface: {d}");
        let n = shape.normal_at(p);
        assert!((n.length() - 1.0).abs() < 1e-5);
    }
}

#[test]
fn rect_shape_samples_lie_in_rect() {
    let shape = RectShape::new(
        Vec3A::new(-1.0, 5.0, -2.0),
        Vec3A::new(2.0, 0.0, 0.0),
        Vec3A::new(0.0, 0.0, 4.0),
        Vec3A::new(0.0, -1.0, 0.0),
    );
    assert!((shape.area() - 8.0).abs() < 1e-5);
    let p = shape.sample_point(0.5, 0.5);
    assert!((p - Vec3A::new(0.0, 5.0, 0.0)).length() < 1e-5);
    assert_eq!(shape.normal_at(p), Vec3A::new(0.0, -1.0, 0.0));
}

#[test]
fn area_light_pdf_is_positive_facing_side() {
    let light = AreaLight::new(
        Box::new(SphereShape {
            center: Vec3A::new(0.0, 5.0, 0.0),
            radius: 1.0,
        }),
        Arc::new(Emissive::new(Vec3A::splat(10.0))),
        0,
    );
    // Nearest point on the sphere as seen from below.
    let pdf = light.pdf_at_point(Vec3A::ZERO, Vec3A::new(0.0, 4.0, 0.0));
    assert!(pdf.is_finite() && pdf > 0.0);

    // The sampled connection agrees: it aims upward at the light, stops
    // at a finite distance, and reports the same emission and a pdf of
    // the same shape.
    let s = light
        .sample_li(Vec3A::ZERO, 0.3, 0.7)
        .expect("a sphere overhead is always reachable");
    assert!(s.direction.is_normalized());
    assert!(s.distance.is_finite() && s.distance > 0.0);
    assert_eq!(s.radiance, Vec3A::splat(10.0));
    assert!(s.pdf.is_finite() && s.pdf > 0.0);

    // `sample_li` and `pdf_at_point` are the two MIS sides of one
    // strategy and must agree on the density of the same direction.
    let point = Vec3A::ZERO + s.direction * s.distance;
    let from_point = light.pdf_at_point(Vec3A::ZERO, point);
    assert!(
        (s.pdf - from_point).abs() <= 1e-3 * s.pdf.max(from_point),
        "MIS sides disagree: sample_li {} vs pdf_at_point {}",
        s.pdf,
        from_point
    );

    // An area light has geometry and no escaped-ray contribution.
    assert_eq!(light.geom_id(), Some(0));
    assert!(light.escaped(Vec3A::ZERO, Vec3A::Y).is_none());
}

/// An area-sampled light's pdf is exactly `d² / (cos θ_l · A)`, with no
/// epsilon in the denominator. The `+1e-4` it used to carry was an NEE
/// bias of `1 + 1e-4/(cos θ_l · A)`: about +4.5% on this light, a
/// 0.1 × 0.04 ellipse at 45°, whose `cos θ_l · A` is about 2.2e-3.
#[test]
fn area_pdf_has_no_epsilon() {
    // A small unit disk (local XY, emitting along −Z) squashed to an
    // ellipse and tilted 45° about X, at z = 3: area-sampled, since only
    // a sphere has a solid-angle strategy on `AffineShape`.
    let placement = Affine3A::from_translation(Vec3A::new(0.0, 0.0, 3.0).into())
        * Affine3A::from_rotation_x(std::f32::consts::FRAC_PI_4)
        * Affine3A::from_scale(glam::Vec3::new(0.05, 0.02, 1.0));
    let shape = AffineShape::new(UnitShape::Disk, placement).unwrap();
    let area = shape.area();
    let light = AreaLight::new(Box::new(shape), Arc::new(Emissive::new(Vec3A::ONE)), 0);
    for (u, v) in [(0.1, 0.2), (0.5, 0.5), (0.9, 0.7)] {
        let s = light.sample_li(Vec3A::ZERO, u, v).expect("front-facing");
        let p = s.direction * s.distance;
        let n = placement
            .matrix3
            .inverse()
            .transpose()
            .mul_vec3a(-Vec3A::Z)
            .normalize();
        let cos_l = n.dot(-s.direction);
        let expected = s.distance * s.distance / (cos_l * area);
        assert!(
            (s.pdf - expected).abs() <= 1e-4 * expected,
            "pdf {} vs d²/(cos·A) {}",
            s.pdf,
            expected
        );
        let bounce = light.pdf_at_point(Vec3A::ZERO, p);
        assert!((bounce - s.pdf).abs() <= 1e-4 * s.pdf);
    }
}

/// Behind an area-sampled light the density is finite: the cosine is
/// taken unsigned, as the area-to-solid-angle Jacobian requires. A
/// two-sided emitter is therefore sampled from behind as from in front,
/// and both MIS sides agree on the density. Only edge-on, where the
/// density is infinite, is refused: `sample_li` returns `None` and the
/// bounce side reports 0, meaning "NEE never delivers this".
#[test]
fn area_samples_are_refused_only_edge_on() {
    let placement = Affine3A::from_translation(glam::Vec3::new(0.0, 0.0, 3.0));
    let light = AreaLight::new(
        Box::new(AffineShape::new(UnitShape::Disk, placement).unwrap()),
        Arc::new(Emissive::new(Vec3A::ONE)),
        0,
    );
    // The disk emits along −Z, toward the origin; z = 6 is behind it.
    let (front, behind) = (Vec3A::ZERO, Vec3A::new(0.0, 0.0, 6.0));
    for (u, v) in [(0.1, 0.2), (0.5, 0.5), (0.9, 0.7)] {
        let f = light.sample_li(front, u, v).expect("front");
        let b = light.sample_li(behind, u, v).expect("behind");
        assert!(
            (b.pdf - f.pdf).abs() <= 1e-4 * f.pdf,
            "{} vs {}",
            b.pdf,
            f.pdf
        );
        assert_eq!(b.radiance, Vec3A::ONE);
        let p = behind + b.direction * b.distance;
        assert!((light.pdf_at_point(behind, p) - b.pdf).abs() <= 1e-4 * b.pdf);
    }
    // Edge-on (in the disk's own plane) is refused on both sides.
    let edge_on = Vec3A::new(5.0, 0.0, 3.0);
    let on_disk = Vec3A::new(0.2, -0.1, 3.0);
    assert!(light.sample_li(edge_on, 0.5, 0.5).is_none());
    assert_eq!(light.pdf_at_point(edge_on, on_disk), 0.0);
}

/// The cone convention: every sampled direction lies inside the
/// source cone, and `escaped` agrees about exactly which directions
/// those are. Disagreement here would mean NEE and the bounce side
/// find the light in different sets of directions.
#[test]
fn distant_light_cone_is_consistent() {
    let dir = Vec3A::new(0.3, -1.0, 0.2).normalize();
    let light = DistantLight::new(dir, Vec3A::splat(2.0), 10.0);

    let mut rng = openqmc::pcg::Rng::new(7);
    for _ in 0..2000 {
        let s = light
            .sample_li(Vec3A::ZERO, rng.next_f32(), rng.next_f32())
            .expect("a distant light is reachable from anywhere");
        assert!(s.direction.is_normalized());
        assert!(
            s.distance.is_infinite(),
            "a light at infinity cannot be occluded by anything in the scene"
        );
        // Sampled directions must be ones `escaped` also covers.
        let (radiance, pdf) = light
            .escaped(Vec3A::ZERO, s.direction)
            .expect("sample_li produced a direction escaped() does not cover");
        assert_eq!(radiance, s.radiance);
        assert!(
            (pdf - s.pdf).abs() < 1e-3 * s.pdf,
            "MIS sides disagree on the pdf: {} vs {}",
            s.pdf,
            pdf
        );
    }

    // And nothing outside the cone is covered: the opposite hemisphere
    // and a direction just past the half-angle both miss.
    assert!(light.escaped(Vec3A::ZERO, dir).is_none());
    let outside = utils::align_to_normal(
        Vec3A::new(20f32.to_radians().sin(), 0.0, 20f32.to_radians().cos()),
        -dir,
    )
    .normalize();
    assert!(
        light.escaped(Vec3A::ZERO, outside).is_none(),
        "a direction 20° off-axis is outside a 10° cone"
    );
}

/// `DistantLight::new`'s energy convention: its argument is the
/// *irradiance* on a surface facing the light, and radiance is derived
/// over the cone. So widening the angle must soften shadows without
/// changing exposure — `L · π sin²θ` stays put. (Whether an authored
/// `intensity` means that or a radiance is the importer's decision:
/// `inputs:normalize`.)
#[test]
fn distant_light_irradiance_is_angle_invariant() {
    let dir = -Vec3A::Y;
    let e = Vec3A::new(3.0, 2.0, 1.0);
    for angle in [0.0f32, 0.53, 5.0, 30.0] {
        let light = DistantLight::new(dir, e, angle);
        let s = light.sample_li(Vec3A::ZERO, 0.4, 0.6).expect("reachable");
        // Radiance integrated against the facing surface's cosine over
        // the cone returns the authored irradiance, whatever the angle.
        let half = 0.5 * DistantLight::clamp_diameter(angle).to_radians();
        let recovered = s.radiance * projected_cone_solid_angle(half);
        assert!(
            (recovered - e).length() < 1e-3 * e.length(),
            "angle {angle}°: irradiance {recovered:?} != authored {e:?}"
        );
    }
}

/// A zero angle is widened rather than made singular, so the pdf stays
/// finite and the integrator needs no delta-light path.
#[test]
fn distant_light_zero_angle_stays_finite() {
    let light = DistantLight::new(-Vec3A::Y, Vec3A::ONE, 0.0);
    let s = light.sample_li(Vec3A::ZERO, 0.5, 0.5).expect("reachable");
    assert!(s.pdf.is_finite() && s.pdf > 0.0, "pdf = {}", s.pdf);
    assert!(
        s.radiance.is_finite(),
        "radiance must stay finite: {:?}",
        s.radiance
    );
    // Still a *tight* cone: a degree off-axis is outside it.
    let off = utils::align_to_normal(
        Vec3A::new(1f32.to_radians().sin(), 0.0, 1f32.to_radians().cos()),
        Vec3A::Y,
    )
    .normalize();
    assert!(light.escaped(Vec3A::ZERO, off).is_none());
}

/// A distant light has no scene geometry, so bounce rays must never try
/// to attribute a *hit* to it.
#[test]
fn distant_light_has_no_geometry() {
    let light = DistantLight::new(-Vec3A::Y, Vec3A::ONE, 1.0);
    assert_eq!(light.geom_id(), None);
    assert_eq!(light.pdf_at_point(Vec3A::ZERO, Vec3A::Y), 0.0);
}

#[test]
fn find_by_geom_matches_by_id() {
    let mat = Arc::new(Emissive::new(Vec3A::splat(1.0)));
    let mut lights = LightList::new();
    lights.add(Arc::new(AreaLight::new(
        Box::new(SphereShape {
            center: Vec3A::ZERO,
            radius: 1.0,
        }),
        mat,
        7,
    )));

    assert!(lights.find_by_geom(7).is_some());
    assert!(lights.find_by_geom(8).is_none());
}

/// An escaping ray asks only the lights at infinity, and must get exactly
/// what asking every light would: the same lights with the same pick
/// probabilities, in list order, under both uniform and power selection.
#[test]
fn infinite_at_is_iter_at_filtered_to_escaped() {
    let mat = Arc::new(Emissive::new(Vec3A::splat(1.0)));
    let mut lights = LightList::new();
    let area = |c: f32, id: u32| {
        Arc::new(AreaLight::new(
            Box::new(SphereShape {
                center: Vec3A::new(c, 0.0, 0.0),
                radius: 1.0,
            }),
            mat.clone(),
            id,
        ))
    };
    lights.add(area(0.0, 0));
    lights.add(Arc::new(DistantLight::new(-Vec3A::Y, Vec3A::ONE, 1.0)));
    lights.add(area(3.0, 1));
    lights.add(Arc::new(DomeLight::new(
        Vec3A::ONE,
        None,
        glam::Mat3A::IDENTITY,
    )));
    for selection in [LightSelection::Uniform, LightSelection::Power] {
        lights.select_by(selection);
        let from = Vec3A::new(0.0, 5.0, 0.0);
        let dir = Vec3A::Y;
        let every: Vec<(usize, f32)> = lights
            .iter_at(from)
            .enumerate()
            .filter(|(_, (l, _))| l.escaped(from, dir).is_some())
            .map(|(i, (_, pmf))| (i, pmf))
            .collect();
        let infinite: Vec<f32> = lights.infinite_at(from).map(|(_, pmf)| pmf).collect();
        assert_eq!(every.iter().map(|&(i, _)| i).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(every.iter().map(|&(_, p)| p).collect::<Vec<_>>(), infinite);
        for (light, _) in lights.iter() {
            assert_eq!(light.at_infinity(), light.escaped(from, dir).is_some());
        }
    }
}
