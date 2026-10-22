use std::sync::Arc;

use glam::{Affine3A, Mat3A, Quat, Vec3, Vec3A};

use crate::lux::Shaping;
use crate::material::Emissive;

use super::super::shape::{AffineShape, LightShape, SphereShape, UnitShape};
use super::super::{
    AreaLight, AreaShape, DistantLight, Light, LightList, LightSelection, RectShape,
    TREE_DEFENSIVE_SHARE,
};
use super::*;

/// A deterministic stream of unit numbers (splitmix64), for test inputs only.
struct Stream(u64);

impl Stream {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }

    fn vec(&mut self, scale: f32) -> Vec3A {
        Vec3A::new(self.next(), self.next(), self.next()) * (2.0 * scale) - Vec3A::splat(scale)
    }

    fn unit(&mut self) -> Vec3A {
        loop {
            let v = self.vec(1.0);
            if v.length_squared() > 1e-3 && v.length_squared() <= 1.0 {
                return v.normalize();
            }
        }
    }
}

fn light(shape: impl Into<AreaShape>, emissive: Emissive) -> AreaLight {
    AreaLight::new(shape, Arc::new(emissive), 0)
}

fn shaped(light_to_world: Mat3A, cone_deg: f32) -> Option<Shaping> {
    let mut s = Shaping::new(light_to_world);
    s.cone_angle_deg = cone_deg;
    Some(s)
}

/// A placement with a rotation, a non-uniform scale and a translation.
fn squashed() -> Affine3A {
    Affine3A::from_scale_rotation_translation(
        Vec3::new(0.5, 2.0, 1.25),
        Quat::from_euler(glam::EulerRot::XYZ, 0.3, -0.7, 1.1),
        Vec3::new(1.0, -2.0, 0.5),
    )
}

/// Every kind of light `AreaLight::bounds` distinguishes.
fn every_bounded_light() -> Vec<(&'static str, AreaLight)> {
    let white = Vec3A::ONE;
    let rect = || {
        RectShape::new(
            Vec3A::new(-1.0, -0.5, 2.0),
            Vec3A::new(2.0, 0.0, 0.0),
            Vec3A::new(0.0, 1.0, 0.0),
            Vec3A::new(0.0, 0.0, -1.0),
        )
    };
    let place = squashed();
    vec![
        (
            "rect, one-sided",
            light(rect(), Emissive::light(white, None)),
        ),
        ("rect, two-sided", light(rect(), Emissive::new(white))),
        (
            "rect, 30° cone",
            light(
                rect(),
                Emissive::light(white, shaped(Mat3A::IDENTITY, 30.0)),
            ),
        ),
        (
            "sphere",
            light(
                SphereShape {
                    center: Vec3A::new(0.5, 1.0, -1.0),
                    radius: 0.25,
                },
                Emissive::light(white, None),
            ),
        ),
        (
            "sphere, 40° cone",
            light(
                SphereShape {
                    center: Vec3A::new(0.5, 1.0, -1.0),
                    radius: 0.25,
                },
                Emissive::light(white, shaped(Mat3A::from_rotation_x(0.4), 40.0)),
            ),
        ),
        (
            "sphere, 130° cone",
            light(
                SphereShape {
                    center: Vec3A::ZERO,
                    radius: 1.0,
                },
                Emissive::light(white, shaped(Mat3A::from_rotation_y(-0.8), 130.0)),
            ),
        ),
        (
            "affine sphere",
            light(
                AffineShape::new(UnitShape::Sphere, place).unwrap(),
                Emissive::light(white, None),
            ),
        ),
        (
            "affine disk",
            light(
                AffineShape::new(UnitShape::Disk, place).unwrap(),
                Emissive::light(white, None),
            ),
        ),
        (
            "affine disk, 20° cone",
            light(
                AffineShape::new(UnitShape::Disk, place).unwrap(),
                Emissive::light(white, shaped(place.matrix3, 20.0)),
            ),
        ),
        (
            "affine tube",
            light(
                AffineShape::new(UnitShape::Cylinder, place).unwrap(),
                Emissive::light(white, None),
            ),
        ),
    ]
}

/// The angle between `a` and `b`, both unit.
fn angle(a: Vec3A, b: Vec3A) -> f32 {
    a.dot(b).clamp(-1.0, 1.0).acos()
}

/// Every point a light can sample lies in its box, and every direction it
/// emits into lies within `θo + θe` of its axis.
#[test]
fn bounds_contain_every_point_and_direction_a_light_emits() {
    for (name, l) in every_bounded_light() {
        let b = l
            .bounds(utils::Luma::REC709)
            .expect("a finite light has bounds");
        let slack = Vec3A::splat(1e-5) * (1.0 + b.max.abs().max(b.min.abs()));
        let spread = b.cos_theta_o.clamp(-1.0, 1.0).acos() + b.cos_theta_e.clamp(-1.0, 1.0).acos();
        const N: usize = 64;
        let mut emitting = 0;
        for k in 0..N * N {
            let u = ((k / N) as f32 + 0.5) / N as f32;
            let v = ((k % N) as f32 + 0.5) / N as f32;
            let p = l.shape().sample_point(u, v);
            assert!(
                p.cmpge(b.min - slack).all() && p.cmple(b.max + slack).all(),
                "{name}: {p} outside {:?}..{:?}",
                b.min,
                b.max
            );
            // A spread of directions from this point.
            let n = l.shape().normal_at(p, 0);
            for j in 0..32 {
                let z = 1.0 - 2.0 * (j as f32 + 0.5) / 32.0;
                let r = (1.0 - z * z).sqrt();
                let phi = 2.399_963 * (k * 32 + j) as f32;
                let dir = Vec3A::new(r * phi.cos(), r * phi.sin(), z);
                let front = n.dot(dir) > 0.0;
                if l.emitter().radiance_toward(p, dir, front) == Vec3A::ZERO {
                    continue;
                }
                emitting += 1;
                let off = if b.two_sided {
                    angle(b.w, dir).min(angle(-b.w, dir))
                } else {
                    angle(b.w, dir)
                };
                assert!(
                    off <= spread + 1e-4,
                    "{name}: emits {:.3} rad off its axis, bound {spread:.3}",
                    off
                );
            }
        }
        assert!(
            emitting > 0,
            "{name}: the sweep found no emitting direction"
        );
    }
}

/// A light that emits nothing has no bounds, nor does a light at infinity.
#[test]
fn black_and_infinite_lights_have_no_bounds() {
    let luma = utils::Luma::REC709;
    let black = light(
        SphereShape {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        Emissive::light(Vec3A::ZERO, None),
    );
    assert!(black.bounds(luma).is_none());
    let sun = DistantLight::new(Vec3A::Z, Vec3A::ONE, 0.5);
    assert!(sun.bounds(luma).is_none());
}

/// A narrow spot is ranked by its peak, not by the sliver of flux its cone
/// lets out: the cone is already in its bounds' emission spread.
#[test]
fn a_spot_is_ranked_by_its_peak_not_its_cone_flux() {
    let luma = utils::Luma::REC709;
    let rect = || RectShape::new(Vec3A::ZERO, Vec3A::X, Vec3A::Y, -Vec3A::Z);
    let open = light(rect(), Emissive::light(Vec3A::ONE, None));
    let spot = light(
        rect(),
        Emissive::light(Vec3A::ONE, shaped(Mat3A::IDENTITY, 10.0)),
    );
    let (open_b, spot_b) = (open.bounds(luma).unwrap(), spot.bounds(luma).unwrap());
    assert_eq!(open_b.phi, spot_b.phi);
    // ...while its power, which picks by power, is the cone's sliver.
    assert!(spot.power(luma).unwrap() < 0.05 * open.power(luma).unwrap());
    // Under the spot, on its axis, it is as important as the open panel.
    let below = Receiver::at(Vec3A::new(0.5, 0.5, -3.0));
    assert!(
        (spot_b.importance(&below) - open_b.importance(&below)).abs()
            < 1e-6 * open_b.importance(&below)
    );
}

fn sphere_bounds(center: Vec3A, radius: f32, phi: f32) -> LightBounds {
    LightBounds {
        min: center - Vec3A::splat(radius),
        max: center + Vec3A::splat(radius),
        w: Vec3A::Z,
        phi,
        cos_theta_o: -1.0,
        cos_theta_e: 0.0,
        two_sided: false,
    }
}

/// A one-sided panel at `center` facing `normal`.
fn panel_bounds(center: Vec3A, normal: Vec3A, half: f32, phi: f32) -> LightBounds {
    LightBounds {
        min: center - Vec3A::splat(half),
        max: center + Vec3A::splat(half),
        w: normal,
        phi,
        cos_theta_o: 1.0,
        cos_theta_e: 0.0,
        two_sided: false,
    }
}

#[test]
fn importance_is_zero_below_an_opaque_horizon_only() {
    let below = sphere_bounds(Vec3A::new(0.3, 0.0, -2.0), 0.1, 1.0);
    let up = Vec3A::Z;
    assert_eq!(
        below.importance(&Receiver::surface(Vec3A::ZERO, up, true)),
        0.0
    );
    assert!(below.importance(&Receiver::surface(Vec3A::ZERO, up, false)) > 0.0);
    assert!(below.importance(&Receiver::at(Vec3A::ZERO)) > 0.0);
    // A box straddling the horizon keeps some importance.
    let straddling = sphere_bounds(Vec3A::new(2.0, 0.0, 0.0), 0.5, 1.0);
    assert!(straddling.importance(&Receiver::surface(Vec3A::ZERO, up, true)) > 0.0);
}

#[test]
fn importance_is_zero_outside_the_emission_cone() {
    // A panel above the receiver, facing up and away from it.
    let away = panel_bounds(Vec3A::new(0.0, 0.0, 2.0), Vec3A::Z, 0.1, 1.0);
    assert_eq!(away.importance(&Receiver::at(Vec3A::ZERO)), 0.0);
    let toward = panel_bounds(Vec3A::new(0.0, 0.0, 2.0), -Vec3A::Z, 0.1, 1.0);
    assert!(toward.importance(&Receiver::at(Vec3A::ZERO)) > 0.0);
    // Two-sided, the same panel reaches it from behind.
    let both = LightBounds {
        two_sided: true,
        ..away
    };
    assert!(both.importance(&Receiver::at(Vec3A::ZERO)) > 0.0);
}

#[test]
fn importance_falls_with_distance() {
    let r = Receiver::surface(Vec3A::ZERO, Vec3A::Z, true);
    let at = |d: f32| sphere_bounds(Vec3A::new(0.0, 0.0, d), 0.05, 1.0).importance(&r);
    assert!(at(1.0) > at(2.0) && at(2.0) > at(4.0) && at(4.0) > at(8.0));
    // A receiver inside a box is not divided by ~0.
    let inside = sphere_bounds(Vec3A::ZERO, 1.0, 1.0).importance(&Receiver::at(Vec3A::ZERO));
    assert!(inside.is_finite() && inside > 0.0);
}

/// `n` sphere lights scattered through a cube of side 20, flux over three
/// decades, in list order.
fn scattered_spheres(n: usize, seed: u64) -> Vec<(usize, LightBounds)> {
    let mut s = Stream(seed);
    (0..n)
        .map(|i| {
            let phi = 10f32.powf(3.0 * s.next());
            (i, sphere_bounds(s.vec(10.0), 0.05 + 0.2 * s.next(), phi))
        })
        .collect()
}

/// Panels facing every way, and spheres, mixed.
fn scattered_mix(n: usize, seed: u64) -> Vec<(usize, LightBounds)> {
    let mut s = Stream(seed);
    (0..n)
        .map(|i| {
            let c = s.vec(10.0);
            let phi = 10f32.powf(2.0 * s.next());
            let b = if i % 3 == 0 {
                sphere_bounds(c, 0.1, phi)
            } else {
                panel_bounds(c, s.unit(), 0.3, phi)
            };
            (i, b)
        })
        .collect()
}

#[test]
fn the_build_is_a_pure_function_of_the_lights() {
    let lights = scattered_mix(300, 7);
    let a = LightTree::build(300, &lights).unwrap();
    let b = LightTree::build(300, &lights).unwrap();
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    assert_eq!(a.node_count(), 2 * 300 - 1);
    for (light, _) in &lights {
        assert!(a.contains(*light));
    }
}

#[test]
fn many_lights_stay_within_a_trail() {
    let tree = LightTree::build(4096, &scattered_mix(4096, 11)).unwrap();
    assert!(tree.depth() < 64, "depth {}", tree.depth());
    // Every light in one place: no split separates anything, so the build
    // halves them, and the depth is logarithmic.
    let stacked: Vec<_> = (0..4096)
        .map(|i| (i, sphere_bounds(Vec3A::ONE, 0.1, 1.0)))
        .collect();
    assert_eq!(LightTree::build(4096, &stacked).unwrap().depth(), 12);
}

#[test]
fn fewer_than_two_lights_build_no_tree() {
    assert!(LightTree::build(3, &[]).is_none());
    assert!(LightTree::build(3, &scattered_spheres(1, 1)).is_none());
}

/// Random receivers: in the cube, with and without a normal, opaque and not.
fn receivers(seed: u64, count: usize) -> Vec<Receiver> {
    let mut s = Stream(seed);
    (0..count)
        .map(|i| {
            let p = s.vec(12.0);
            match i % 3 {
                0 => Receiver::at(p),
                1 => Receiver::surface(p, s.unit(), true),
                _ => Receiver::surface(p, s.unit(), false),
            }
        })
        .collect()
}

#[test]
fn the_pmf_sums_to_one_where_every_light_can_reach() {
    for n in [2, 7, 47, 1000] {
        let lights = scattered_spheres(n, n as u64);
        let tree = LightTree::build(n, &lights).unwrap();
        // With no normal, a sphere's importance is never zero.
        for r in receivers(3, 60).into_iter().filter(|r| r.n == Vec3::ZERO) {
            let sum: f64 = (0..n).map(|k| tree.pmf(&r, k) as f64).sum();
            assert!((sum - 1.0).abs() < 1e-5, "{n} lights: sum {sum}");
        }
    }
    // Panels and opaque receivers can zero a subtree: never above one.
    let lights = scattered_mix(200, 5);
    let tree = LightTree::build(200, &lights).unwrap();
    for r in receivers(4, 60) {
        let sum: f64 = (0..200).map(|k| tree.pmf(&r, k) as f64).sum();
        assert!(sum < 1.0 + 1e-5, "sum {sum}");
    }
}

#[test]
fn a_pick_reports_the_pmf_its_trail_gives_bit_for_bit() {
    for n in [2, 7, 47, 1000] {
        let lights = scattered_mix(n, 100 + n as u64);
        let tree = LightTree::build(n, &lights).unwrap();
        let mut s = Stream(n as u64);
        for r in receivers(n as u64, 50) {
            for _ in 0..20 {
                if let Some((k, pmf)) = tree.sample(&r, s.next()) {
                    assert!(pmf > 0.0);
                    assert_eq!(pmf.to_bits(), tree.pmf(&r, k).to_bits());
                }
            }
        }
    }
}

/// Each leaf's position in depth-first order.
fn leaf_order(tree: &LightTree) -> std::collections::HashMap<usize, usize> {
    tree.nodes
        .iter()
        .filter(|n| n.leaf)
        .enumerate()
        .map(|(order, n)| (n.second_or_light as usize, order))
        .collect()
}

#[test]
fn picks_are_monotone_in_u() {
    let lights = scattered_mix(47, 9);
    let tree = LightTree::build(47, &lights).unwrap();
    let order = leaf_order(&tree);
    for r in receivers(9, 30) {
        let mut last = 0;
        for i in 0..2000 {
            let u = (i as f32 + 0.5) / 2000.0;
            if let Some((k, _)) = tree.sample(&r, u) {
                assert!(order[&k] >= last, "u = {u} went back");
                last = order[&k];
            }
        }
    }
}

/// The oracle: with two lights the tree is one node, whose ratio is the
/// flat per-light importance; with more, the picks' frequencies are the
/// pmf.
#[test]
fn the_tree_agrees_with_a_flat_evaluation() {
    let lights = scattered_spheres(2, 21);
    let tree = LightTree::build(2, &lights).unwrap();
    for r in receivers(21, 30) {
        let i: Vec<f32> = lights.iter().map(|(_, b)| b.importance(&r)).collect();
        let sum = i[0] + i[1];
        if sum > 0.0 {
            assert_eq!(tree.pmf(&r, 0).to_bits(), (i[0] / sum).to_bits());
            assert_eq!(tree.pmf(&r, 1).to_bits(), (i[1] / sum).to_bits());
        }
    }
    let lights = scattered_mix(7, 22);
    let tree = LightTree::build(7, &lights).unwrap();
    const N: usize = 200_000;
    for r in receivers(22, 6) {
        let mut hits = [0usize; 7];
        for i in 0..N {
            if let Some((k, _)) = tree.sample(&r, (i as f32 + 0.5) / N as f32) {
                hits[k] += 1;
            }
        }
        for (k, &h) in hits.iter().enumerate() {
            let want = tree.pmf(&r, k);
            let got = h as f32 / N as f32;
            assert!(
                (got - want).abs() < 2e-3,
                "light {k}: {got} picked, pmf {want}"
            );
        }
    }
}

/// A scene's worth of lights in a `LightList` under `tree`: rect panels
/// facing every way, spheres, and a sun.
fn tree_list(n: usize, seed: u64) -> LightList {
    let mut s = Stream(seed);
    let mut lights = LightList::new();
    lights.add(DistantLight::new(
        Vec3A::new(0.2, 0.3, 1.0).normalize(),
        Vec3A::ONE,
        0.5,
    ));
    for i in 0..n {
        let c = s.vec(8.0);
        let color = Vec3A::splat(10f32.powf(2.0 * s.next()));
        if i % 2 == 0 {
            lights.add(light(
                SphereShape {
                    center: c,
                    radius: 0.2,
                },
                Emissive::light(color, None),
            ));
        } else {
            let n = s.unit();
            let t = n.any_orthonormal_vector();
            lights.add(light(
                RectShape::new(c, 0.5 * t, 0.5 * n.cross(t), n),
                Emissive::light(color, None),
            ));
        }
    }
    lights.select_by(LightSelection::Tree);
    assert_eq!(lights.selection(), LightSelection::Tree);
    lights
}

/// Every light keeps at least the defensive share of its power probability,
/// and a pick reports exactly what `pmf_at` gives the bounce side.
#[test]
fn the_list_keeps_a_defensive_share_and_one_pmf() {
    let mut power = tree_list(30, 2);
    let lights = tree_list(30, 2);
    power.select_by(LightSelection::Power);
    let mut s = Stream(31);
    for r in receivers(31, 40) {
        let mut sum = 0.0f64;
        for k in 0..lights.count() {
            let p = lights.pmf_at(r, k);
            sum += p as f64;
            assert!(
                p >= TREE_DEFENSIVE_SHARE * power.pmf(k) * (1.0 - 1e-6),
                "light {k}: {p} below the defensive share"
            );
        }
        assert!(sum < 1.0 + 1e-5, "sum {sum}");
        for _ in 0..50 {
            if let Some((k, pmf)) = lights.pick_index_at(r, s.next()) {
                assert_eq!(pmf.to_bits(), lights.pmf_at(r, k).to_bits(), "light {k}");
            }
        }
        // The sun keeps its uniform share, as under power.
        let (_, sun) = lights.infinite_at(r).next().unwrap();
        assert!((sun - power.pmf(0)).abs() < 1e-6);
    }
}

/// One finite light and a sun: nothing to choose, so the power table stays.
#[test]
fn one_finite_light_picks_by_power() {
    let mut lights = LightList::new();
    lights.add(DistantLight::new(Vec3A::Z, Vec3A::ONE, 0.5));
    let (_, l) = every_bounded_light().swap_remove(0);
    lights.add(l);
    lights.select_by(LightSelection::Tree);
    assert_eq!(lights.selection(), LightSelection::Power);
}
