//! A shaped rect light through the integrator with `CRUST_CONE_CLIP` off and
//! on: the `lighting` spec's "Cone-clipped rect sampling is unbiased"
//! scenario.
//!
//! A diffuse floor is lit by a 4 × 4 panel 1 unit above it, shaped by a 20°
//! cone softened over its outer 40% and focused, so a floor point sees only
//! a circle of radius ~0.36 of it lit. Light-only, BSDF-only and power-MIS
//! estimates of the floor agree within noise with the clip on or off, and
//! the light-only difference between the two falls as `1/√N`: noise, not
//! bias, which would plateau.

use std::sync::Arc;

use crust_core::rt::{Geometry, SceneBuilder};
use crust_core::{
    AreaLight, Emissive, LightList, MASK_INDIRECT, MASK_SHADOW, OpenPBR, PathSampler, Ray,
    RectShape, SamplingStrategy, Shaping, Vec3A, Volumes, WorldBuilder, ray_color,
};
use glam::Mat3A;

struct Stage {
    world: crust_core::World,
    lights: LightList,
}

/// The floor and the panel, sampled with the cone clip on or off.
fn stage(clip: bool) -> Stage {
    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();
    world.attach(
        Geometry::TriangleMesh {
            vertices: vec![
                [-50.0, 0.0, -50.0],
                [50.0, 0.0, -50.0],
                [50.0, 0.0, 50.0],
                [-50.0, 0.0, 50.0],
            ],
            indices: vec![[0, 2, 1], [0, 3, 2]],
            normals: None,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
    // Local −Z (the emitting axis) to world −Y.
    let mut shaping = Shaping::new(Mat3A::from_rotation_x(-std::f32::consts::FRAC_PI_2));
    shaping.cone_angle_deg = 20.0;
    shaping.cone_softness = 0.4;
    shaping.focus = 2.0;
    let emitter = Arc::new(Emissive::light(Vec3A::splat(8.0), Some(shaping)));
    let (origin, edge_u, edge_v) = (
        Vec3A::new(-2.0, 1.0, -2.0),
        Vec3A::new(4.0, 0.0, 0.0),
        Vec3A::new(0.0, 0.0, 4.0),
    );
    // `edge_u × edge_v` is −Y, the emitting normal: wound as the importer
    // winds a rect light, so a bounce hit from below is on its front.
    let mut proto = SceneBuilder::new();
    proto.attach(Geometry::TriangleMesh {
        vertices: vec![
            origin.to_array(),
            (origin + edge_u).to_array(),
            (origin + edge_u + edge_v).to_array(),
            (origin + edge_v).to_array(),
        ],
        indices: vec![[0, 1, 2], [0, 2, 3]],
        normals: None,
    });
    let id = world.attach_masked(
        Geometry::Instance {
            scene: Arc::new(proto.commit()),
            transform: glam::Affine3A::IDENTITY,
            transform_end: None,
        },
        emitter.clone(),
        MASK_SHADOW | MASK_INDIRECT,
    );
    let rect = RectShape::new(origin, edge_u, edge_v, -Vec3A::Y).with_cone_clip(clip);
    let light = AreaLight::new(rect, emitter, id);
    match light.shape() {
        crust_core::AreaShape::Rect(r) => assert!(r.cone().is_some(), "the panel takes its cone"),
        _ => unreachable!(),
    }
    lights.add(light);
    Stage {
        world: world.commit(),
        lights,
    }
}

/// Floor points under the middle, near an edge (where the cutoff square is
/// cut by the panel), near a corner, and just past an edge.
const FLOOR: [Vec3A; 4] = [
    Vec3A::new(0.0, 0.0, 0.0),
    Vec3A::new(1.8, 0.0, 0.3),
    Vec3A::new(-1.9, 0.0, 1.85),
    Vec3A::new(2.15, 0.0, -0.4),
];

/// The mean and its standard error over `n` paths through the floor point
/// `k`, from sample index `first` on.
fn estimate(stage: &Stage, s: SamplingStrategy, k: usize, first: u32, n: u32) -> (f64, f64) {
    let target = FLOOR[k];
    let origin = target + Vec3A::new(0.0, 0.5, 3.0);
    let ray = Ray::new(origin, target - origin);
    let volumes = Volumes::default();
    let (mut sum, mut sum2) = (0.0f64, 0.0f64);
    for i in first..first + n {
        let c = ray_color(
            &ray,
            &stage.world,
            &stage.lights,
            &volumes,
            2,
            s,
            PathSampler::new(k as i32, 11, 0, i as i32),
        );
        let y = c.x as f64;
        sum += y;
        sum2 += y * y;
    }
    let mean = sum / n as f64;
    let var = (sum2 / n as f64 - mean * mean).max(0.0);
    (mean, (var / n as f64).sqrt())
}

/// Every strategy, clip on or off, estimates the same floor: each against
/// power MIS with the clip off, within four standard errors of the two
/// together.
#[test]
fn cone_clipped_rect_sampling_is_unbiased() {
    let reference = stage(false);
    let truth: Vec<(f64, f64)> = (0..FLOOR.len())
        .map(|k| estimate(&reference, SamplingStrategy::PowerMis, k, 1 << 20, 65_536))
        .collect();
    for (k, (m, _)) in truth.iter().enumerate() {
        assert!(*m > 0.01, "floor point {k} is unlit: {m}");
    }
    for clip in [false, true] {
        let stage = stage(clip);
        for (s, n) in [
            (SamplingStrategy::PowerMis, 4096),
            (SamplingStrategy::LightOnly, 4096),
            (SamplingStrategy::BsdfOnly, 65_536),
        ] {
            for (k, &(want, want_se)) in truth.iter().enumerate() {
                let (got, se) = estimate(&stage, s, k, 0, n);
                let tol = 4.0 * (se * se + want_se * want_se).sqrt() + 1e-3 * want;
                assert!(
                    (got - want).abs() <= tol,
                    "clip {clip}, {s:?}, floor point {k}: {got} vs {want} (tolerance {tol})"
                );
            }
        }
    }
}

/// The light-only difference between the clipped and unclipped estimates,
/// root-mean-square over the floor points and independent runs, falls by
/// at least `√16 = 4` from 64 to 1024 samples. A bias would leave a floor under
/// it. And the clip is the less noisy of the two: it spends no sample on
/// the part of the panel the cone darkens.
#[test]
fn the_clip_differs_by_noise_alone_and_less_of_it() {
    const RUNS: u32 = 8;
    let (off, on) = (stage(false), stage(true));
    let rms = |n: u32| {
        let mut sum = 0.0;
        for run in 0..RUNS {
            for k in 0..FLOOR.len() {
                // Disjoint sample ranges: the two sides are independent.
                let s = SamplingStrategy::LightOnly;
                let a = estimate(&off, s, k, (2 * run) * n + (1 << 16), n).0;
                let b = estimate(&on, s, k, (2 * run + 1) * n + (1 << 16), n).0;
                sum += (a - b) * (a - b);
            }
        }
        (sum / (RUNS as usize * FLOOR.len()) as f64).sqrt()
    };
    let (coarse, fine) = (rms(64), rms(1024));
    // At least as fast as 1/√N: stratified samples converge faster still.
    assert!(
        coarse / fine > 2.5,
        "rms difference {coarse} at 64 spp, {fine} at 1024 (ratio {}, expected about 4)",
        coarse / fine
    );
    for k in 0..FLOOR.len() {
        let (_, se_off) = estimate(&off, SamplingStrategy::LightOnly, k, 0, 4096);
        let (_, se_on) = estimate(&on, SamplingStrategy::LightOnly, k, 0, 4096);
        assert!(
            se_on < se_off,
            "floor point {k}: clipped error {se_on} vs {se_off} unclipped"
        );
    }
}
