use std::sync::Arc;

use glam::Affine3A;

use crate::material::Emissive;
use crate::pdf::PdfSolidAngle;

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
    assert!(PdfSolidAngle::new(pdf.get()).is_some());
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
        SphereShape {
            center: Vec3A::new(0.0, 5.0, 0.0),
            radius: 1.0,
        },
        Arc::new(Emissive::new(Vec3A::splat(10.0))),
        0,
    );
    // Nearest point on the sphere as seen from below.
    let pdf = light
        .pdf_at_point(Vec3A::ZERO, Vec3A::new(0.0, 4.0, 0.0))
        .map_or(0.0, |p| p.get());
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
    assert!(s.pdf.get().is_finite() && s.pdf.get() > 0.0);

    // `sample_li` and `pdf_at_point` are the two MIS sides of one
    // strategy and must agree on the density of the same direction.
    let point = Vec3A::ZERO + s.direction * s.distance;
    let from_point = light
        .pdf_at_point(Vec3A::ZERO, point)
        .map_or(0.0, |p| p.get());
    assert!(
        (s.pdf.get() - from_point).abs() <= 1e-3 * s.pdf.get().max(from_point),
        "MIS sides disagree: sample_li {} vs pdf_at_point {}",
        s.pdf.get(),
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
    let light = AreaLight::new(shape, Arc::new(Emissive::new(Vec3A::ONE)), 0);
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
            (s.pdf.get() - expected).abs() <= 1e-4 * expected,
            "pdf {} vs d²/(cos·A) {}",
            s.pdf.get(),
            expected
        );
        let bounce = light.pdf_at_point(Vec3A::ZERO, p).map_or(0.0, |p| p.get());
        assert!((bounce - s.pdf.get()).abs() <= 1e-4 * s.pdf.get());
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
        AffineShape::new(UnitShape::Disk, placement).unwrap(),
        Arc::new(Emissive::new(Vec3A::ONE)),
        0,
    );
    // The disk emits along −Z, toward the origin; z = 6 is behind it.
    let (front, behind) = (Vec3A::ZERO, Vec3A::new(0.0, 0.0, 6.0));
    for (u, v) in [(0.1, 0.2), (0.5, 0.5), (0.9, 0.7)] {
        let f = light.sample_li(front, u, v).expect("front");
        let b = light.sample_li(behind, u, v).expect("behind");
        assert!(
            (b.pdf.get() - f.pdf.get()).abs() <= 1e-4 * f.pdf.get(),
            "{} vs {}",
            b.pdf.get(),
            f.pdf.get()
        );
        assert_eq!(b.radiance, Vec3A::ONE);
        let p = behind + b.direction * b.distance;
        assert!(
            (light.pdf_at_point(behind, p).map_or(0.0, |p| p.get()) - b.pdf.get()).abs()
                <= 1e-4 * b.pdf.get()
        );
    }
    // Edge-on (in the disk's own plane) is refused on both sides.
    let edge_on = Vec3A::new(5.0, 0.0, 3.0);
    let on_disk = Vec3A::new(0.2, -0.1, 3.0);
    assert!(light.sample_li(edge_on, 0.5, 0.5).is_none());
    assert_eq!(
        light
            .pdf_at_point(edge_on, on_disk)
            .map_or(0.0, |p| p.get()),
        0.0
    );
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
        let pdf = pdf.expect("NEE sampled this direction").get();
        assert_eq!(radiance, s.radiance);
        assert!(
            (pdf - s.pdf.get()).abs() < 1e-3 * s.pdf.get(),
            "MIS sides disagree on the pdf: {} vs {}",
            s.pdf.get(),
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
    assert!(
        s.pdf.get().is_finite() && s.pdf.get() > 0.0,
        "pdf = {}",
        s.pdf.get()
    );
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
    assert_eq!(
        light
            .pdf_at_point(Vec3A::ZERO, Vec3A::Y)
            .map_or(0.0, |p| p.get()),
        0.0
    );
}

#[test]
fn find_by_geom_matches_by_id() {
    let mat = Arc::new(Emissive::new(Vec3A::splat(1.0)));
    let mut lights = LightList::new();
    lights.add(AreaLight::new(
        SphereShape {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        mat,
        7,
    ));

    assert!(lights.index_of_geom(7).is_some());
    assert!(lights.index_of_geom(8).is_none());
}

/// An escaping ray asks only the lights at infinity, and must get exactly
/// what asking every light would: the same lights with the same pick
/// probabilities, in list order, under both uniform and power selection.
#[test]
fn infinite_at_is_every_escaping_light() {
    let mat = Arc::new(Emissive::new(Vec3A::splat(1.0)));
    let mut lights = LightList::new();
    let area = |c: f32, id: u32| {
        AreaLight::new(
            SphereShape {
                center: Vec3A::new(c, 0.0, 0.0),
                radius: 1.0,
            },
            mat.clone(),
            id,
        )
    };
    lights.add(area(0.0, 0));
    lights.add(DistantLight::new(-Vec3A::Y, Vec3A::ONE, 1.0));
    lights.add(area(3.0, 1));
    lights.add(DomeLight::new(Vec3A::ONE, None, glam::Mat3A::IDENTITY));
    for selection in [LightSelection::Uniform, LightSelection::Power] {
        lights.select_by(selection);
        let from = Vec3A::new(0.0, 5.0, 0.0);
        let dir = Vec3A::Y;
        let every: Vec<(usize, f32)> = (0..lights.count())
            .filter(|&i| lights.light(i).escaped(from, dir).is_some())
            .map(|i| (i, lights.pmf_at(from, i)))
            .collect();
        let infinite: Vec<f32> = lights.infinite_at(from).map(|(_, pmf)| pmf).collect();
        assert_eq!(every.iter().map(|&(i, _)| i).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(every.iter().map(|&(_, p)| p).collect::<Vec<_>>(), infinite);
        for (light, _) in lights.iter() {
            assert_eq!(light.at_infinity(), light.escaped(from, dir).is_some());
        }
    }
}

/// A backdrop is not a selectable light: adding one leaves every other
/// light's pick probability bit-equal, and nothing iterates over it.
#[test]
fn a_backdrop_changes_no_selection() {
    let mat = Arc::new(Emissive::new(Vec3A::splat(1.0)));
    let build = |backdrop: bool| {
        let mut lights = LightList::new();
        lights.add(AreaLight::new(
            SphereShape {
                center: Vec3A::ZERO,
                radius: 1.0,
            },
            mat.clone(),
            0,
        ));
        lights.add(DomeLight::new(Vec3A::ONE, None, glam::Mat3A::IDENTITY));
        if backdrop {
            lights.add_backdrop(DomeLight::new(Vec3A::X, None, glam::Mat3A::IDENTITY));
        }
        lights.select_by(LightSelection::Power);
        lights
    };
    let (with, without) = (build(true), build(false));
    assert_eq!(with.count(), without.count());
    let pmfs = |l: &LightList| l.iter().map(|(_, p)| p.to_bits()).collect::<Vec<_>>();
    assert_eq!(pmfs(&with), pmfs(&without));
    assert_eq!(with.infinite_at(Vec3A::ZERO).count(), 1);
    assert_eq!(with.backdrops().len(), 1);
    assert!(with.escapes_to_backdrop(crate::ray::MASK_CAMERA));
    assert!(!with.escapes_to_backdrop(crate::ray::MASK_INDIRECT));
    assert!(!without.escapes_to_backdrop(crate::ray::MASK_CAMERA));
}

/// Removing a light leaves the list exactly as if it had never been added:
/// the geometry index, the infinite-light table and their masks all shift.
#[test]
fn remove_is_as_if_never_added() {
    use crate::ray::{MASK_ALL, MASK_CAMERA};
    let mat = Arc::new(Emissive::new(Vec3A::splat(1.0)));
    let area = |id: u32| {
        AreaLight::new(
            SphereShape {
                center: Vec3A::new(id as f32 * 3.0, 0.0, 0.0),
                radius: 1.0,
            },
            mat.clone(),
            id,
        )
    };
    let hidden = crate::RayMask(MASK_ALL.0 & !MASK_CAMERA.0);
    let dome = || DomeLight::new(Vec3A::ONE, None, glam::Mat3A::IDENTITY);
    let mut removed = LightList::new();
    removed.add(area(0));
    removed.add_masked(dome(), hidden);
    removed.add(area(1));
    removed.add(DistantLight::new(-Vec3A::Y, Vec3A::ONE, 1.0));
    let (light, mask) = removed.remove(1);
    assert!(light.at_infinity());
    assert_eq!(mask, hidden);
    let (light, mask) = removed.remove(0);
    assert_eq!(light.geom_id(), Some(0));
    assert_eq!(mask, MASK_ALL);

    let mut never = LightList::new();
    never.add(area(1));
    never.add(DistantLight::new(-Vec3A::Y, Vec3A::ONE, 1.0));
    for l in [&mut removed, &mut never] {
        l.select_by(LightSelection::Power);
    }
    assert_eq!(removed.count(), never.count());
    assert!(removed.index_of_geom(0).is_none());
    assert_eq!(
        removed.index_of_geom(1).map(|i| removed.pmf(i).to_bits()),
        never.index_of_geom(1).map(|i| never.pmf(i).to_bits())
    );
    let seen = |l: &LightList| {
        l.infinite_indexed_seen_by(Vec3A::ZERO, MASK_CAMERA)
            .map(|(_, _, p)| p.to_bits())
            .collect::<Vec<_>>()
    };
    assert_eq!(seen(&removed), seen(&never));
}

/// The pair behind the bounce-side estimate of a shadow-linked light
/// (`LightShape::hits`): fired from many origins, a light's analytic hits and
/// the kernel's hits on the light's own geometry — built as the importer
/// builds it — agree on hit or miss, and on the distance to the kernel's
/// rounding. Both the nearest hit and the one past it.
#[test]
fn analytic_light_hits_match_the_kernel() {
    use crate::ray::{Ray, TRACE_T_MIN};
    use crate::rt_world::WorldBuilder;
    use crust_rt::Geometry;

    let unit_geometry = |unit: UnitShape| match unit {
        UnitShape::Sphere => Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        UnitShape::Disk => Geometry::Disk {
            center: Vec3A::ZERO,
            normal: -Vec3A::Z,
            radius: 1.0,
        },
        UnitShape::Cylinder => Geometry::Cylinder {
            p0: Vec3A::new(-0.5, 0.0, 0.0),
            p1: Vec3A::new(0.5, 0.0, 0.0),
            radius: 1.0,
        },
    };
    // A non-uniform, rotated placement: the instanced unit primitive.
    let squash = Affine3A::from_scale_rotation_translation(
        glam::Vec3::new(1.5, 0.6, 0.9),
        glam::Quat::from_euler(glam::EulerRot::XYZ, 0.4, -0.7, 0.2),
        glam::Vec3::new(0.3, -0.2, 0.5),
    );
    let rect = RectShape::new(
        Vec3A::new(-0.7, -0.4, 0.2),
        Vec3A::new(1.2, 0.3, 0.0),
        Vec3A::new(-0.15, 0.6, 0.5),
        Vec3A::new(1.2, 0.3, 0.0).cross(Vec3A::new(-0.15, 0.6, 0.5)),
    );
    let (c00, c10, c11, c01) = (
        rect.origin,
        rect.origin + rect.edge_u,
        rect.origin + rect.edge_u + rect.edge_v,
        rect.origin + rect.edge_v,
    );
    let mut cases: Vec<(&str, AreaShape, Geometry)> = vec![
        (
            "sphere",
            SphereShape {
                center: Vec3A::new(0.2, 0.1, -0.3),
                radius: 0.8,
            }
            .into(),
            Geometry::Sphere {
                center: Vec3A::new(0.2, 0.1, -0.3),
                radius: 0.8,
            },
        ),
        (
            "rect",
            rect.clone().into(),
            Geometry::TriangleMesh {
                vertices: vec![
                    c00.to_array(),
                    c10.to_array(),
                    c11.to_array(),
                    c01.to_array(),
                ],
                indices: vec![[0, 1, 2], [0, 2, 3]],
                normals: None,
            },
        ),
    ];
    for unit in [UnitShape::Sphere, UnitShape::Disk, UnitShape::Cylinder] {
        let mut b = crust_rt::SceneBuilder::new();
        b.attach(unit_geometry(unit));
        cases.push((
            match unit {
                UnitShape::Sphere => "affine sphere",
                UnitShape::Disk => "affine disk",
                UnitShape::Cylinder => "affine cylinder",
            },
            AffineShape::new(unit, squash).unwrap().into(),
            Geometry::Instance {
                scene: Arc::new(b.commit_with(crate::commit_options())),
                transform: squash,
                transform_end: None,
            },
        ));
    }

    let material = Arc::new(Emissive::light(Vec3A::ONE, None));
    let mut rng = openqmc::pcg::Rng::new(17);
    let mut rand = |lo: f32, hi: f32| lo + (hi - lo) * rng.next_f32();
    for (name, shape, geometry) in cases {
        let mut world = WorldBuilder::new();
        world.attach(geometry, material.clone());
        let world = world.commit();
        let (mut hits, mut disagree) = (0, 0);
        for _ in 0..4000 {
            let origin = Vec3A::new(rand(-4.0, 4.0), rand(-4.0, 4.0), rand(-4.0, 4.0));
            // Aimed near a point of the shape, so most rays meet it and
            // some pass its edges; unit length, as the integrator asks.
            let on = shape.sample_point(rand(0.0, 1.0), rand(0.0, 1.0));
            let target = on + Vec3A::new(rand(-0.3, 0.3), rand(-0.3, 0.3), rand(-0.3, 0.3));
            let dir = (target - origin).normalize();
            let analytic = shape.hits(origin, dir);
            let ray = Ray::new(origin, dir);
            let kernel = world.intersect(&ray, TRACE_T_MIN, f32::INFINITY);
            let mut kernel_ts = Vec::new();
            if let Some(h) = &kernel {
                kernel_ts.push(h.rec.t);
                let past = h.rec.t + h.rec.t.max(1.0) * 1e-3;
                if let Some(h2) = world.intersect(&ray, past, f32::INFINITY) {
                    kernel_ts.push(h2.rec.t);
                }
            }
            let analytic_ts: Vec<f32> = analytic.iter().collect();
            if analytic_ts.len() != kernel_ts.len() {
                // Only a ray grazing an edge or a silhouette may land on
                // either side of it.
                disagree += 1;
                continue;
            }
            for (a, k) in analytic_ts.iter().zip(&kernel_ts) {
                hits += 1;
                assert!(
                    (a - k).abs() <= 1e-4 * k.max(1.0),
                    "{name}: analytic t {a} vs kernel t {k} from {origin} along {dir}"
                );
            }
        }
        assert!(hits > 1000, "{name}: only {hits} hits");
        assert!(
            disagree <= 4,
            "{name}: {disagree} rays disagree on hit or miss"
        );
    }
}

/// A restricted dome is NEE-only even when the links handed to
/// `LightList::set_links` say otherwise: a dome has no link twin
/// (`LightKind::found_along` finds nothing), so twinning it would drop its
/// bounce side's share of the light.
#[test]
fn a_restricted_dome_is_nee_only_whoever_builds_the_links() {
    use crate::ray::MASK_SHADOW;
    let mut lights = LightList::new();
    lights.add(DomeLight::new(Vec3A::ONE, None, glam::Mat3A::IDENTITY));
    lights.add(DistantLight::new(-Vec3A::Y, Vec3A::ONE, 1.0));
    lights.set_links(LightLinks {
        illuminates: vec![None; 2],
        shadow_masks: vec![MASK_SHADOW; 2],
        restricted: vec![true; 2],
        nee_only: vec![false; 2],
    });
    assert!(lights.nee_only(0), "the restricted dome is NEE-only");
    assert!(!lights.nee_only(1), "the restricted sun keeps its twin");
    assert_eq!(lights.twinned_lights(), &[1]);
}

/// A sphere or tube under a rotation and a uniform scale of its curved axes
/// takes the closed-form area rather than the grid, and that area is the
/// exact one: 4πs² for the sphere, 2π·r·L for the tube.
#[test]
fn similarity_placed_shapes_have_closed_form_area() {
    let rot = glam::Quat::from_euler(glam::EulerRot::XYZ, 0.3, -1.1, 2.0);
    let sphere = Affine3A::from_scale_rotation_translation(
        glam::Vec3::splat(0.03),
        rot,
        glam::Vec3::new(5.0, -2.0, 7.0),
    );
    let area = AffineShape::new(UnitShape::Sphere, sphere).unwrap().area();
    let exact = 4.0 * std::f64::consts::PI * 0.03f64 * 0.03;
    assert!(
        (area as f64 / exact - 1.0).abs() < 1e-6,
        "{area} vs {exact}"
    );

    let tube = Affine3A::from_scale_rotation_translation(
        glam::Vec3::new(2.5, 0.2, 0.2),
        rot,
        glam::Vec3::ZERO,
    );
    let area = AffineShape::new(UnitShape::Cylinder, tube).unwrap().area();
    let exact = 2.0 * std::f64::consts::PI * 0.2 * 2.5;
    assert!(
        (area as f64 / exact - 1.0).abs() < 1e-6,
        "{area} vs {exact}"
    );
}

// ---------------------------------------------------------------------------
// Disk and tube solid-angle strategies (`CRUST_DISK_SAMPLING`,
// `CRUST_TUBE_SAMPLING`)
// ---------------------------------------------------------------------------

use crate::config::{DiskSampling, TubeSampling};

/// A disk or tube light under `placement`, sampled as told, with a one-sided
/// (`Emissive::light`) or two-sided (`Emissive::new`) emitter.
fn round_light(
    unit: UnitShape,
    placement: Affine3A,
    tube: TubeSampling,
    disk: DiskSampling,
    one_sided: bool,
) -> AreaLight {
    let material = if one_sided {
        Emissive::light(Vec3A::ONE, None)
    } else {
        Emissive::new(Vec3A::ONE)
    };
    let shape = AffineShape::new(unit, placement)
        .unwrap()
        .with_sampling(tube, disk);
    AreaLight::new(shape, Arc::new(material), 0)
}

fn affine(light: &AreaLight) -> &AffineShape {
    match &light.shape {
        AreaShape::Affine(s) => s,
        _ => unreachable!("an affine light"),
    }
}

/// A 1 × 0.01 tube (length × radius), rotated off the world axes.
fn thin_tube() -> Affine3A {
    Affine3A::from_scale_rotation_translation(
        glam::Vec3::new(1.0, 0.01, 0.01),
        glam::Quat::from_euler(glam::EulerRot::XYZ, 0.4, 0.9, -0.2),
        glam::Vec3::new(0.3, 1.0, -0.5),
    )
}

/// A tube as thick as it is long.
fn thick_tube() -> Affine3A {
    Affine3A::from_scale_rotation_translation(
        glam::Vec3::new(1.0, 0.5, 0.5),
        glam::Quat::from_rotation_z(0.3),
        glam::Vec3::ZERO,
    )
}

/// A sheared tube with an elliptical cross-section.
fn sheared_tube() -> Affine3A {
    Affine3A::from_mat3_translation(
        glam::Mat3::from_cols(
            glam::Vec3::new(1.5, 0.2, 0.0),
            glam::Vec3::new(0.1, 0.3, 0.05),
            glam::Vec3::new(0.0, -0.1, 0.2),
        ),
        glam::Vec3::new(-0.2, 0.4, 0.1),
    )
}

/// A sheared, non-uniformly scaled disk.
fn sheared_disk() -> Affine3A {
    Affine3A::from_mat3_translation(
        glam::Mat3::from_cols(
            glam::Vec3::new(0.9, 0.1, 0.0),
            glam::Vec3::new(0.3, 0.5, 0.1),
            glam::Vec3::new(0.1, -0.2, 0.7),
        ),
        glam::Vec3::new(0.2, -0.1, 0.3),
    )
}

/// `local` placed by `m`.
fn at(m: Affine3A, local: Vec3A) -> Vec3A {
    m.transform_point3a(local)
}

/// A strategy's samples against its own claimed density. The expected
/// share of each bin is `∫ pdf_Ω cos θ_l / r² dA` over the bin, by
/// quadrature over the shape's area sampler (a fine grid of its points,
/// each weighted by the area it stands for); the shares must sum to one —
/// the density is normalised — and the samples, a stratified grid of
/// `(u, v)`, must fill the bins in the same shares.
fn check_density(name: &str, light: &AreaLight, from: Vec3A, bin: impl Fn(Vec3A) -> usize) {
    const A: usize = 1024;
    const K: usize = 256;
    const BINS: usize = 32;
    let shape = affine(light);
    let sampler = shape
        .solid_angle_sampler(from)
        .unwrap_or_else(|| panic!("{name}: no strategy from {from}"));
    let mut expected = [0.0f64; BINS];
    for i in 0..A {
        for j in 0..A {
            let p = shape.sample_point((i as f32 + 0.5) / A as f32, (j as f32 + 0.5) / A as f32);
            let Some(pdf) = sampler.pdf(p) else {
                continue;
            };
            let to = from - p;
            let r2 = to.length_squared();
            let cos = shape.normal_at(p).dot(to / r2.sqrt()).abs();
            expected[bin(p)] += pdf.get() as f64 * (cos * shape.inv_pdf_area(p).get() / r2) as f64;
        }
    }
    for e in &mut expected {
        *e /= (A * A) as f64;
    }
    let total: f64 = expected.iter().sum();
    assert!(
        (total - 1.0).abs() < 5e-3,
        "{name}: the density integrates to {total}"
    );

    let mut counts = [0usize; BINS];
    let mut refused = 0;
    for i in 0..K {
        for j in 0..K {
            let (u, v) = ((i as f32 + 0.5) / K as f32, (j as f32 + 0.5) / K as f32);
            match sampler.sample(u, v) {
                Some((p, pdf)) => {
                    // Both MIS halves answer the same number, bit for bit.
                    assert_eq!(sampler.pdf(p), Some(pdf), "{name}");
                    counts[bin(p)] += 1;
                }
                None => refused += 1,
            }
        }
    }
    assert!(refused <= 2, "{name}: {refused} samples refused");
    for b in 0..BINS {
        let got = counts[b] as f64 / (K * K) as f64;
        assert!(
            (got - expected[b]).abs() < 4e-3 + 0.02 * expected[b],
            "{name}: bin {b}: {got} sampled vs {} expected",
            expected[b]
        );
    }
}

/// Tube bins: eight azimuths across the arc that faces `from`, by four
/// axial slabs.
fn tube_bins(m: Affine3A, from: Vec3A) -> impl Fn(Vec3A) -> usize {
    let inv = m.inverse();
    let f = inv.transform_point3a(from);
    let phi0 = f.z.atan2(f.y);
    let half = (1.0 / (f.y * f.y + f.z * f.z).sqrt()).acos();
    move |p| {
        let l = inv.transform_point3a(p);
        let d = (l.z.atan2(l.y) - phi0 + 3.0 * PI).rem_euclid(2.0 * PI) - PI;
        let a = (((d / half + 1.0) * 0.5 * 8.0) as isize).clamp(0, 7) as usize;
        let x = (((l.x + 0.5) * 4.0) as isize).clamp(0, 3) as usize;
        a * 4 + x
    }
}

use std::f32::consts::PI;

#[test]
fn tube_samples_follow_their_density() {
    let cases = [
        // Thin: 0.3 from its axis, opposite its middle and off one end.
        ("thin, beside", thin_tube(), Vec3A::new(0.1, 30.0, 10.0)),
        (
            "thin, off the end",
            thin_tube(),
            Vec3A::new(0.8, 20.0, -15.0),
        ),
        // Thick, up close: 0.6 from an axis of radius 0.5.
        ("thick, close", thick_tube(), Vec3A::new(0.2, 0.0, 1.2)),
        ("sheared", sheared_tube(), Vec3A::new(-0.3, 2.0, 1.5)),
    ];
    for (name, m, f) in cases {
        let from = at(m, f);
        for tube in [TubeSampling::Arc, TubeSampling::Equiangular] {
            let light = round_light(UnitShape::Cylinder, m, tube, DiskSampling::Area, true);
            check_density(
                &format!("{name} ({tube})"),
                &light,
                from,
                tube_bins(m, from),
            );
        }
    }
}

/// From a thousand points outside the tube, every light sample lands on the
/// part of the wall that faces the point and carries its radiance.
#[test]
fn tube_samples_face_the_shading_point() {
    let mut rng = openqmc::pcg::Rng::new(29);
    for m in [thin_tube(), thick_tube(), sheared_tube()] {
        for tube in [TubeSampling::Arc, TubeSampling::Equiangular] {
            let light = round_light(UnitShape::Cylinder, m, tube, DiskSampling::Area, true);
            let shape = affine(&light);
            let mut points = 0;
            while points < 1000 {
                let mut r = || 8.0 * rng.next_f32() - 4.0;
                let local = Vec3A::new(r() * 0.5, r(), r());
                if local.y * local.y + local.z * local.z <= 1.0 {
                    continue;
                }
                points += 1;
                let from = at(m, local);
                assert!(shape.solid_angle_sampler(from).is_some());
                for _ in 0..4 {
                    let (u, v) = (rng.next_f32(), rng.next_f32());
                    let Some(s) = light.sample_li(from, u, v) else {
                        continue;
                    };
                    let p = from + s.direction * s.distance;
                    let facing = shape.normal_at(p).dot(-s.direction);
                    assert!(facing > -1e-4, "{tube}: a back-facing sample, cos {facing}");
                    assert_ne!(s.radiance, Vec3A::ZERO, "{tube}: a dark sample");
                }
            }
        }
    }
}

/// The disk's spherical ellipse, placed by a shear and a non-uniform scale,
/// samples its own world density, and every sample lies on the disk.
#[test]
fn sheared_disk_samples_follow_their_density() {
    let m = sheared_disk();
    let inv = m.inverse();
    let light = round_light(
        UnitShape::Disk,
        m,
        TubeSampling::Area,
        DiskSampling::Ellipse,
        true,
    );
    for f in [
        Vec3A::new(0.0, 0.0, -1.0),
        Vec3A::new(1.5, -0.8, -0.6),
        Vec3A::new(0.2, 0.3, -0.15),
    ] {
        let from = at(m, f);
        let bins = |p: Vec3A| {
            let l = inv.transform_point3a(p);
            let r = (((l.x * l.x + l.y * l.y) * 4.0) as usize).min(3);
            let a = ((l.y.atan2(l.x) + PI) / (2.0 * PI) * 8.0) as usize;
            a.min(7) * 4 + r
        };
        check_density(&format!("disk from {f}"), &light, from, bins);
        let shape = affine(&light);
        let sampler = shape.solid_angle_sampler(from).unwrap();
        let mut rng = openqmc::pcg::Rng::new(5);
        for _ in 0..2000 {
            let (p, _) = sampler.sample(rng.next_f32(), rng.next_f32()).unwrap();
            let l = inv.transform_point3a(p);
            assert!(
                l.z.abs() < 1e-5 && l.x * l.x + l.y * l.y <= 1.0 + 1e-5,
                "{l}"
            );
        }
    }
}

/// Where a strategy does not apply the light is area-sampled, exactly as
/// with the switch at `area`: a disk seen from behind or edge-on, a tube seen
/// from inside, and any two-sided tube.
#[test]
fn round_lights_fall_back_to_area_sampling_bit_for_bit() {
    let mut rng = openqmc::pcg::Rng::new(41);
    // With both switches at `area`, no disk or tube has a strategy from
    // anywhere — in front, behind, beside, inside — so `AreaLight` takes the
    // area path these lights always took, unchanged (the goldens pin the
    // whole render).
    for (unit, m) in [
        (UnitShape::Disk, sheared_disk()),
        (UnitShape::Cylinder, sheared_tube()),
        (UnitShape::Cylinder, thin_tube()),
    ] {
        let plain = round_light(unit, m, TubeSampling::Area, DiskSampling::Area, true);
        for _ in 0..500 {
            let mut r = || 6.0 * rng.next_f32() - 3.0;
            let from = at(m, Vec3A::new(r(), r(), r()));
            assert!(affine(&plain).solid_angle_sampler(from).is_none());
        }
    }
    let same = |a: &AreaLight, b: &AreaLight, from: Vec3A, rng: &mut openqmc::pcg::Rng| {
        assert!(affine(a).solid_angle_sampler(from).is_none(), "from {from}");
        for _ in 0..64 {
            let (u, v) = (rng.next_f32(), rng.next_f32());
            let (sa, sb) = (a.sample_li(from, u, v), b.sample_li(from, u, v));
            assert_eq!(sa.map(|s| s.pdf), sb.map(|s| s.pdf));
            assert_eq!(sa.map(|s| s.direction), sb.map(|s| s.direction));
            assert_eq!(sa.map(|s| s.radiance), sb.map(|s| s.radiance));
            if let Some(s) = sa {
                let p = from + s.direction * s.distance;
                assert_eq!(a.pdf_at_point(from, p), b.pdf_at_point(from, p));
            }
        }
    };
    let area = |unit, m, one_sided| {
        round_light(unit, m, TubeSampling::Area, DiskSampling::Area, one_sided)
    };

    let m = sheared_disk();
    let disk = round_light(
        UnitShape::Disk,
        m,
        TubeSampling::Area,
        DiskSampling::Ellipse,
        true,
    );
    let plain = area(UnitShape::Disk, m, true);
    for f in [Vec3A::new(0.2, 0.1, 0.8), Vec3A::new(3.0, 0.0, 0.0)] {
        same(&disk, &plain, at(m, f), &mut rng);
    }

    for tube in [TubeSampling::Arc, TubeSampling::Equiangular] {
        let m = sheared_tube();
        let inside = round_light(UnitShape::Cylinder, m, tube, DiskSampling::Area, true);
        same(
            &inside,
            &area(UnitShape::Cylinder, m, true),
            at(m, Vec3A::new(0.1, 0.3, -0.4)),
            &mut rng,
        );
        // Two-sided, from beyond an open end on the axis, where its inner wall
        // shows: the outer arc would never sample it.
        let two = round_light(UnitShape::Cylinder, m, tube, DiskSampling::Area, false);
        let plain = area(UnitShape::Cylinder, m, false);
        for f in [Vec3A::new(1.5, 0.0, 0.0), Vec3A::new(0.0, 3.0, 1.0)] {
            same(&two, &plain, at(m, f), &mut rng);
        }
    }
}

/// A two-sided tube seen through its open end: its light-only estimate of
/// the solid angle it fills — inner wall included — agrees with a
/// direction-sampled one.
#[test]
fn a_two_sided_tube_through_its_open_end_is_estimated_without_bias() {
    let m = sheared_tube();
    let from = at(m, Vec3A::new(1.2, 0.0, 0.0));
    let light = round_light(
        UnitShape::Cylinder,
        m,
        TubeSampling::Equiangular,
        DiskSampling::Area,
        false,
    );
    // Light-only: E[1 / pdf] over samples that reach the light unoccluded
    // by the tube itself.
    const K: usize = 512;
    let mut nee = 0.0f64;
    for i in 0..K {
        for j in 0..K {
            let (u, v) = ((i as f32 + 0.5) / K as f32, (j as f32 + 0.5) / K as f32);
            let Some(s) = light.sample_li(from, u, v) else {
                continue;
            };
            let first = light
                .shape
                .hits(from, s.direction)
                .first()
                .unwrap_or(f32::INFINITY);
            if first >= s.distance * (1.0 - 1e-4) && s.radiance != Vec3A::ZERO {
                nee += 1.0 / s.pdf.get() as f64;
            }
        }
    }
    nee /= (K * K) as f64;
    // Direction-only: the fraction of uniform directions that meet it.
    const D: usize = 2048;
    let mut hit = 0usize;
    for i in 0..D {
        for j in 0..D {
            let z = 1.0 - 2.0 * (i as f32 + 0.5) / D as f32;
            let phi = 2.0 * PI * (j as f32 + 0.5) / D as f32;
            let r = (1.0 - z * z).max(0.0).sqrt();
            let dir = Vec3A::new(r * phi.cos(), r * phi.sin(), z);
            hit += usize::from(light.shape.hits(from, dir).first().is_some());
        }
    }
    let bsdf = 4.0 * std::f64::consts::PI * hit as f64 / (D * D) as f64;
    assert!(
        (nee - bsdf).abs() < 0.01 * bsdf,
        "light-only {nee} vs direction-sampled {bsdf} sr"
    );
}

/// The disk and tube strategies keep the sampler no larger than the rect's
/// already made it. A sampler sized by a 200-byte ellipse cost every sphere
/// and rect light sample a copy of it: +14% instructions in `sample_li` on
/// `veach_mis`, which has neither a disk nor a tube.
#[test]
fn round_strategies_do_not_grow_the_sampler() {
    let rect = std::mem::size_of::<super::rect::SphericalRect>() + 2 * std::mem::size_of::<usize>();
    let sampler = std::mem::size_of::<SolidAngleSampler<'_>>();
    assert!(
        sampler <= rect,
        "SolidAngleSampler is {sampler} bytes, the rect strategy {rect}"
    );
}
