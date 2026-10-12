//! Disk and cylinder lights through the integrator, under every value of
//! `CRUST_DISK_SAMPLING` and `CRUST_TUBE_SAMPLING`: the `lighting` spec's
//! "Disk and cylinder sampling is unbiased" scenario.
//!
//! A diffuse floor is lit by a sheared, non-uniformly scaled disk and an
//! elliptical tube lying just above it. Light-only, BSDF-only and power-MIS
//! estimates of the floor agree within noise whichever strategy samples
//! either light, and the difference between a new strategy and area
//! sampling falls as `1/√N` — noise, not bias, which would plateau.

use std::sync::Arc;

use crust_core::rt::{Geometry, SceneBuilder};
use crust_core::{
    AffineShape, AreaLight, DiskSampling, Emissive, LightList, MASK_INDIRECT, MASK_SHADOW, OpenPBR,
    PathSampler, Ray, SamplingStrategy, TubeSampling, UnitShape, Vec3A, Volumes, WorldBuilder,
    ray_color,
};
use glam::{Affine3A, Mat3, Quat, Vec3};

/// A disk emitting downward, sheared and squashed into an ellipse.
fn disk_placement() -> Affine3A {
    // Local −Z (the emitting side) to world −Y, then tilted and sheared.
    let turn = Mat3::from_quat(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2));
    let tilt = Mat3::from_quat(Quat::from_rotation_z(0.3));
    let shape = Mat3::from_cols(
        Vec3::new(0.9, 0.0, 0.0),
        Vec3::new(0.25, 0.5, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
    );
    Affine3A::from_mat3_translation(tilt * turn * shape, Vec3::new(-1.2, 1.2, 0.2))
}

/// A tube of elliptical section lying a little above the floor.
fn tube_placement() -> Affine3A {
    Affine3A::from_scale_rotation_translation(
        Vec3::new(2.0, 0.12, 0.08),
        Quat::from_euler(glam::EulerRot::YXZ, 0.4, 0.3, 0.15),
        Vec3::new(1.0, 0.5, -0.3),
    )
}

fn unit_geometry(unit: UnitShape) -> Geometry {
    match unit {
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
        UnitShape::Sphere => unreachable!(),
    }
}

struct Stage {
    world: crust_core::World,
    lights: LightList,
}

/// The floor, the disk and the tube, each light sampled as told.
fn stage(tube: TubeSampling, disk: DiskSampling) -> Stage {
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
    for (unit, placement, radiance) in [
        (UnitShape::Disk, disk_placement(), 6.0),
        (UnitShape::Cylinder, tube_placement(), 10.0),
    ] {
        let emitter = Arc::new(Emissive::light(Vec3A::splat(radiance), None));
        let mut proto = SceneBuilder::new();
        proto.attach(unit_geometry(unit));
        let id = world.attach_masked(
            Geometry::Instance {
                scene: Arc::new(proto.commit()),
                transform: placement,
                transform_end: None,
            },
            emitter.clone(),
            MASK_SHADOW | MASK_INDIRECT,
        );
        let shape = AffineShape::new(unit, placement)
            .unwrap()
            .with_sampling(tube, disk);
        lights.add(AreaLight::new(shape, emitter, id));
    }
    Stage {
        world: world.commit(),
        lights,
    }
}

/// Floor points under the disk, beside and under the tube, and between.
const FLOOR: [Vec3A; 6] = [
    Vec3A::new(-1.2, 0.0, 0.2),
    Vec3A::new(-0.5, 0.0, -0.6),
    Vec3A::new(0.3, 0.0, 0.4),
    Vec3A::new(1.0, 0.0, -0.3),
    Vec3A::new(1.8, 0.0, 0.5),
    Vec3A::new(0.9, 0.0, 0.6),
];

/// The mean and its standard error over `n` paths through the floor point
/// `k`, from sample index `first` on.
fn estimate(stage: &Stage, s: SamplingStrategy, k: usize, first: u32, n: u32) -> (f64, f64) {
    let target = FLOOR[k];
    // From the side, so no camera ray crosses a light's dark back.
    let origin = target + Vec3A::new(0.0, 4.0, 4.0);
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
            PathSampler::new(k as i32, 7, 0, i as i32),
        );
        let y = c.x as f64;
        sum += y;
        sum2 += y * y;
    }
    let mean = sum / n as f64;
    let var = (sum2 / n as f64 - mean * mean).max(0.0);
    (mean, (var / n as f64).sqrt())
}

const TUBES: [TubeSampling; 3] = [
    TubeSampling::Area,
    TubeSampling::Arc,
    TubeSampling::Equiangular,
];
const DISKS: [DiskSampling; 2] = [DiskSampling::Area, DiskSampling::Ellipse];

/// Every strategy, under every switch value, estimates the same floor: each
/// against power MIS with both switches at `area`, within four standard
/// errors of the two together.
#[test]
fn disk_and_cylinder_sampling_is_unbiased() {
    let reference = stage(TubeSampling::Area, DiskSampling::Area);
    let truth: Vec<(f64, f64)> = (0..FLOOR.len())
        .map(|k| estimate(&reference, SamplingStrategy::PowerMis, k, 1 << 20, 16_384))
        .collect();
    for (k, (m, _)) in truth.iter().enumerate() {
        assert!(*m > 0.01, "floor point {k} is unlit: {m}");
    }
    for tube in TUBES {
        for disk in DISKS {
            let stage = stage(tube, disk);
            for (s, n) in [
                (SamplingStrategy::PowerMis, 4096),
                (SamplingStrategy::LightOnly, 4096),
                (SamplingStrategy::BsdfOnly, 16_384),
            ] {
                for (k, &(want, want_se)) in truth.iter().enumerate() {
                    let (got, se) = estimate(&stage, s, k, 0, n);
                    let tol = 4.0 * (se * se + want_se * want_se).sqrt() + 1e-3 * want;
                    assert!(
                        (got - want).abs() <= tol,
                        "tube {tube}, disk {disk}, {s:?}, floor point {k}: {got} vs {want} \
                         (tolerance {tol})"
                    );
                }
            }
        }
    }
}

/// The light-only difference between each new strategy and area sampling,
/// root-mean-square over the floor points and independent runs, falls by
/// about `√16 = 4` from 64 to 1024 samples. A bias would leave a floor under
/// it.
#[test]
fn the_difference_from_area_sampling_falls_as_one_over_root_n() {
    const RUNS: u32 = 8;
    let area = stage(TubeSampling::Area, DiskSampling::Area);
    for (tube, disk) in [
        (TubeSampling::Arc, DiskSampling::Ellipse),
        (TubeSampling::Equiangular, DiskSampling::Ellipse),
    ] {
        let new = stage(tube, disk);
        let rms = |n: u32| {
            let mut sum2 = 0.0f64;
            for run in 0..RUNS {
                for k in 0..FLOOR.len() {
                    // Disjoint sample ranges: the two sides are independent.
                    let s = SamplingStrategy::LightOnly;
                    let (a, _) = estimate(&area, s, k, (2 * run) * n + (1 << 16), n);
                    let (b, _) = estimate(&new, s, k, (2 * run + 1) * n + (1 << 16), n);
                    sum2 += (a - b) * (a - b);
                }
            }
            (sum2 / (RUNS as usize * FLOOR.len()) as f64).sqrt()
        };
        let (coarse, fine) = (rms(64), rms(1024));
        assert!(
            coarse / fine > 2.5,
            "tube {tube}, disk {disk}: RMS difference {coarse} at 64 spp, {fine} at 1024 spp \
             (ratio {}, expected about 4)",
            coarse / fine
        );
    }
}
