use std::sync::Arc;

use super::*;
use crate::Vec3A;
use crate::light::learned::DEFENSIVE;
use crate::rt::Geometry;
use crate::{AreaLight, Emissive, LightSelection, OpenPBR, SphereShape, WorldBuilder};

const NEAR: Vec3A = Vec3A::new(-3.0, 1.0, 0.0);
const FAR: Vec3A = Vec3A::new(3.0, 1.0, 0.0);
const SIDE: usize = 256;

/// Two rooms side by side, seen from above: one floor, a wall along x = 0
/// taller than the lights, and one sphere light in each room.
fn two_rooms() -> (World, Camera, LightList) {
    let mut world = WorldBuilder::new();
    let grey = || Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5)));
    world.attach(
        Geometry::TriangleMesh {
            vertices: vec![
                [-6.0, 0.0, -6.0],
                [6.0, 0.0, -6.0],
                [6.0, 0.0, 6.0],
                [-6.0, 0.0, 6.0],
            ],
            indices: vec![[0, 2, 1], [0, 3, 2]],
            normals: None,
        },
        grey(),
    );
    world.attach(
        Geometry::TriangleMesh {
            vertices: vec![
                [0.0, 0.0, -6.0],
                [0.0, 0.0, 6.0],
                [0.0, 3.0, 6.0],
                [0.0, 3.0, -6.0],
            ],
            indices: vec![[0, 1, 2], [0, 2, 3]],
            normals: None,
        },
        grey(),
    );
    let mut lights = LightList::new();
    for center in [NEAR, FAR] {
        let emitter = Arc::new(Emissive::new(Vec3A::splat(20.0)));
        let id = world.attach_masked(
            Geometry::Sphere {
                center,
                radius: 0.3,
            },
            emitter.clone(),
            crate::MASK_SHADOW | crate::MASK_INDIRECT,
        );
        lights.add(AreaLight::new(
            SphereShape {
                center,
                radius: 0.3,
            },
            emitter,
            id,
        ));
    }
    lights.select_by(LightSelection::Power);
    let camera = Camera::new(
        Vec3A::new(0.0, 14.0, 0.01),
        Vec3A::ZERO,
        Vec3A::Y,
        55.0,
        1.0,
        0.0,
        14.0,
    );
    (world.commit(), camera, lights)
}

/// Trained on the two rooms, the network calls each room's own light
/// visible from its floor and the other room's hidden behind the wall.
#[test]
fn learns_the_wall_between_two_rooms() {
    let (world, camera, lights) = two_rooms();
    let cache = train(&world, &camera, &lights, SIDE, SIDE, 0).expect("both rooms are lit");
    assert!(cache.trained > 1000, "{} receivers trained", cache.trained);
    for (p, near, far) in [
        // Room by room, away from the wall: the light in the room, then
        // the one behind the wall.
        (Vec3A::new(-4.0, 0.0, 2.0), 0, 1),
        (Vec3A::new(-2.5, 0.0, -3.0), 0, 1),
        (Vec3A::new(4.0, 0.0, -2.0), 1, 0),
        (Vec3A::new(2.5, 0.0, 3.0), 1, 0),
    ] {
        let v = cache.visibility(p).expect("inside the trained bounds");
        assert!(
            v[near] > 0.9,
            "at {p:?}, the room's own light: V̂ {}",
            v[near]
        );
        assert!(
            v[far] < 0.1,
            "at {p:?}, the light behind the wall: V̂ {}",
            v[far]
        );
    }
}

/// The weights are a function of the scene alone: trained on one thread and
/// on the default pool, they are the same bits.
#[test]
fn training_is_the_same_at_any_thread_count() {
    let (world, camera, lights) = two_rooms();
    let bits = |threads: Option<usize>| {
        let run = || {
            let cache = train(&world, &camera, &lights, 96, 96, 3).expect("lit");
            (
                cache
                    .net
                    .params()
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>(),
                cache.loss.to_bits(),
            )
        };
        match threads {
            Some(t) => rayon::ThreadPoolBuilder::new()
                .num_threads(t)
                .build()
                .expect("pool")
                .install(run),
            None => run(),
        }
    };
    let one = bits(Some(1));
    assert_eq!(one, bits(None));
    assert_eq!(one, bits(Some(3)));
}

/// A cache whose network predicts the same logit for each light everywhere
/// (the last layer's weights zeroed, its biases the logits), over `power`,
/// bounded by the unit cube.
fn constant(power: &[f32], logits: &[f32]) -> VisibilityCache {
    let n = power.len();
    let mut seed = 5u32;
    let mut net = Network::<f32>::new(&shape(n), || {
        seed = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
        (seed >> 8) as f32 / (1u32 << 24) as f32
    });
    let params = net.params_mut();
    let len = params.len();
    let last = &mut params[len - (32 * n + n)..];
    last[..32 * n].fill(0.0);
    last[32 * n..].copy_from_slice(logits);
    let live: Vec<bool> = power.iter().map(|&p| p > 0.0).collect();
    let n_live = live.iter().filter(|&&l| l).count().max(1);
    VisibilityCache {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        lo: Vec3A::ZERO,
        hi: Vec3A::ONE,
        inv_extent: 1.0,
        net,
        power: power.to_vec(),
        live,
        n_live,
        receivers: 0,
        trained: 0,
        loss: 0.0,
    }
}

/// The power table and the logits of `n` lights, every seventh dead (zero
/// power) and every third predicted hidden, uneven otherwise.
fn uneven(n: usize) -> (Vec<f32>, Vec<f32>) {
    let mut power: Vec<f32> = (0..n)
        .map(|k| {
            if k % 7 == 3 {
                0.0
            } else {
                1.0 + (k as f32).powf(1.7)
            }
        })
        .collect();
    let total: f32 = power.iter().sum();
    for p in &mut power {
        *p /= total;
    }
    let logits = (0..n)
        .map(|k| {
            if k % 3 == 0 {
                -200.0
            } else {
                (k as f32 * 0.37).sin() * 6.0
            }
        })
        .collect();
    (power, logits)
}

/// Design D2's floor: every emitting light keeps at least `D / n_live`,
/// whatever the network predicts — hidden lights included.
#[test]
fn every_emitting_light_keeps_the_defensive_share() {
    for n in [2, 5, 47, MAX_LIGHTS] {
        let (power, logits) = uneven(n);
        let cache = constant(&power, &logits);
        let (pmf, _) = cache
            .table_uncached(Vec3A::splat(0.5))
            .expect("some light is predicted visible");
        let floor = DEFENSIVE / cache.n_live as f32;
        for k in 0..n {
            if power[k] > 0.0 {
                assert!(
                    pmf[k] >= 0.999 * floor,
                    "{n} lights: light {k} gets {} < {floor}",
                    pmf[k]
                );
            } else {
                assert_eq!(pmf[k], 0.0, "{n} lights: dead light {k}");
            }
        }
    }
}

/// The pmf sums to one, and each entry is exactly its CDF interval, as in
/// `learned`'s `each_pmf_is_exactly_its_cdf_interval`: the probability NEE
/// divides by is the one its pick lands with.
#[test]
fn each_pmf_is_exactly_its_cdf_interval() {
    for n in [2, 5, 47, MAX_LIGHTS] {
        let (power, logits) = uneven(n);
        let cache = constant(&power, &logits);
        for p in [Vec3A::ZERO, Vec3A::splat(0.5), Vec3A::new(1.0, 0.2, 0.9)] {
            let (pmf, cdf) = cache.table_uncached(p).expect("inside");
            let mut prev = 0.0f32;
            for k in 0..n {
                assert_eq!(pmf[k], cdf[k] - prev, "{n} lights at {p:?}: light {k}");
                prev = cdf[k];
            }
            let last_live = (0..n).rev().find(|&k| power[k] > 0.0).unwrap();
            assert_eq!(cdf[last_live], 1.0);
            let sum: f64 = pmf.iter().map(|&p| p as f64).sum();
            assert!((sum - 1.0).abs() < 1e-6, "{n} lights at {p:?}: Σ pmf {sum}");
        }
    }
}

/// Where every light is predicted hidden, or outside the trained bounds, the
/// power table answers.
#[test]
fn power_answers_where_nothing_is_visible_or_untrained() {
    let power = [0.5, 0.3, 0.2];
    let hidden = constant(&power, &[-200.0; 3]);
    assert_eq!(hidden.table_uncached(Vec3A::splat(0.5)), None);
    hidden.with_table(Vec3A::splat(0.5), |t| assert!(t.is_none()));
    let seen = constant(&power, &[0.0; 3]);
    assert!(seen.table_uncached(Vec3A::splat(0.5)).is_some());
    for outside in [Vec3A::new(1.5, 0.5, 0.5), Vec3A::splat(-0.01), Vec3A::NAN] {
        assert_eq!(seen.table_uncached(outside), None, "{outside:?}");
    }
    // At equal predicted visibility, the pick is power mixed with uniform.
    let (pmf, _) = seen.table_uncached(Vec3A::splat(0.5)).unwrap();
    for k in 0..3 {
        let want = (1.0 - DEFENSIVE) * power[k] + DEFENSIVE / 3.0;
        assert!(
            (pmf[k] - want).abs() < 1e-6,
            "light {k}: {} vs {want}",
            pmf[k]
        );
    }
}

/// The memo serves the table NEE computed, bit for bit, hit or cold, and
/// never one cache's table to another.
#[test]
fn the_memo_is_the_function() {
    let (power, logits) = uneven(9);
    let a = constant(&power, &logits);
    let b = constant(&power, &logits.iter().map(|z| -z).collect::<Vec<_>>());
    let p = Vec3A::new(0.25, 0.5, 0.75);
    let want_a = a.table_uncached(p).unwrap();
    let want_b = b.table_uncached(p).unwrap();
    assert_ne!(want_a, want_b);
    let owned = |t: Option<(&[f32], &[f32])>| t.map(|(p, c)| (p.to_vec(), c.to_vec()));
    // Cold, then hit.
    assert_eq!(a.with_table(p, owned), Some(want_a.clone()));
    assert_eq!(a.with_table(p, owned), Some(want_a.clone()));
    // Another cache at the same position.
    assert_eq!(b.with_table(p, owned), Some(want_b));
    assert_eq!(a.with_table(p, owned), Some(want_a));
}

/// `learned` walks without labels and allocates none; its contributions are
/// the bits the labelled walk computes (PR #308 review).
#[test]
fn labels_are_collected_only_when_asked() {
    let (world, camera, lights) = two_rooms();
    let with = light_cache::walk(&world, &camera, &lights, 64, 64, 0, true);
    let without = light_cache::walk(&world, &camera, &lights, 64, 64, 0, false);
    assert!(!with.is_empty());
    assert_eq!(with.len(), without.len());
    for (a, b) in with.iter().zip(&without) {
        assert_eq!(
            a.p.to_array().map(f32::to_bits),
            b.p.to_array().map(f32::to_bits)
        );
        assert_eq!(
            a.contrib.iter().map(|c| c.to_bits()).collect::<Vec<_>>(),
            b.contrib.iter().map(|c| c.to_bits()).collect::<Vec<_>>()
        );
        assert_eq!(a.delivered.len(), lights.count());
        assert!(b.delivered.is_empty() && b.delivered.capacity() == 0);
    }
}
