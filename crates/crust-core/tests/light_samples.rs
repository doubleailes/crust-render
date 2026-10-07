//! Several light samples per vertex (`crust:lightSamples` /
//! `crust:lightSamplesIndirect`): every MIS pair must move with the count,
//! or the extra samples look like plausible extra light. Each test here is
//! an estimator-agreement check — light sampling alone, BSDF sampling alone
//! and power MIS must agree on the same scene at four samples per vertex,
//! and with the one-sample renderer.

use crust_core::rt::Geometry;
use crust_core::{
    AreaLight, DomeLight, Emissive, LightList, MASK_INDIRECT, MASK_SHADOW, OpenPBR, PathSampler,
    Ray, Renderer, SamplingStrategy, Scene, SphereShape, Vec3A, Volumes, WorldBuilder,
    ray_color_with_light_samples,
};
use std::sync::Arc;

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1e-6)
}

/// A diffuse floor under a sphere light, a rect light and a dome: every
/// kind of light NEE can sample and a bounce can find — by hitting the
/// sphere or the rect, or by escaping to the dome.
fn floor_scene() -> (crust_core::World, LightList) {
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
    let emitter = Arc::new(Emissive::new(Vec3A::splat(30.0)));
    let (center, radius) = (Vec3A::new(2.0, 2.5, 1.0), 0.4);
    let id = world.attach_masked(
        Geometry::Sphere { center, radius },
        emitter.clone(),
        MASK_SHADOW | MASK_INDIRECT,
    );
    lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
    // A rect light facing down, built as the importer builds one: its mesh
    // wound so the front face is the emitting side.
    let (origin, edge_u, edge_v) = (
        Vec3A::new(-1.0, 3.5, -1.0),
        Vec3A::new(2.0, 0.0, 0.0),
        Vec3A::new(0.0, 0.0, 2.0),
    );
    let normal = -Vec3A::Y;
    assert!(edge_u.cross(edge_v).dot(normal) > 0.0);
    let emitter = Arc::new(Emissive::light(Vec3A::splat(3.0), None));
    let id = world.attach_masked(
        Geometry::TriangleMesh {
            vertices: vec![
                origin.to_array(),
                (origin + edge_u).to_array(),
                (origin + edge_u + edge_v).to_array(),
                (origin + edge_v).to_array(),
            ],
            indices: vec![[0, 1, 2], [0, 2, 3]],
            normals: None,
        },
        emitter.clone(),
        MASK_SHADOW | MASK_INDIRECT,
    );
    lights.add(AreaLight::new(
        crust_core::RectShape::new(origin, edge_u, edge_v, normal),
        emitter,
        id,
    ));
    lights.add(DomeLight::new(
        Vec3A::splat(0.2),
        None,
        crust_core::Mat3A::IDENTITY,
    ));
    (world.commit(), lights)
}

/// Spec: "Several light samples keep the strategies consistent". Light
/// sampling alone, BSDF sampling alone and power MIS agree on the floor at
/// four light samples per vertex, and with the one-sample renderer.
#[test]
fn strategies_agree_on_a_floor_with_four_light_samples() {
    let (world, lights) = floor_scene();
    let volumes = Volumes::default();
    let ray = Ray::new(Vec3A::new(0.5, 2.0, 0.5), Vec3A::new(-0.5, -2.0, -0.5));
    let mean_for = |s: SamplingStrategy, counts: (u32, u32), n: i32| {
        let mut sum = 0.0f64;
        for i in 0..n {
            sum += ray_color_with_light_samples(
                &ray,
                &world,
                &lights,
                &volumes,
                3,
                s,
                counts,
                PathSampler::new(1, 2, 0, i),
            )
            .x as f64;
        }
        sum / n as f64
    };
    let reference = mean_for(SamplingStrategy::PowerMis, (1, 1), 16_384);
    assert!(reference > 0.0);
    for (s, counts, n, tol) in [
        (SamplingStrategy::PowerMis, (4, 4), 8192, 0.03),
        (SamplingStrategy::BalanceMis, (4, 4), 8192, 0.03),
        (SamplingStrategy::LightOnly, (4, 4), 8192, 0.03),
        (SamplingStrategy::PowerMis, (4, 1), 8192, 0.03),
        (SamplingStrategy::PowerMis, (1, 4), 8192, 0.03),
        (SamplingStrategy::PowerMis, (3, 2), 8192, 0.03),
        // BSDF sampling alone has to find the lights by chance: noisier.
        (SamplingStrategy::BsdfOnly, (4, 4), 65_536, 0.10),
    ] {
        let m = mean_for(s, counts, n);
        assert!(
            rel(m, reference) < tol,
            "{s:?} at {counts:?}: {m} vs {reference}"
        );
    }
}

/// The same agreement at a volume scatter vertex: `samples/fog.usda` (a
/// grey room full of homogeneous fog under a ceiling rect light) at four
/// light samples per indirect vertex, where every vertex after the camera
/// ray's first scatter is a `volume_nee` vertex paired with a phase bounce.
#[test]
fn strategies_agree_in_fog_with_four_indirect_light_samples() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples/fog.usda");
    let mean = |strategy: SamplingStrategy, counts: (u32, u32), spp: u32| {
        let scene = Scene::from_usd(std::path::Path::new(path)).expect("fog.usda loads");
        assert!(!scene.volumes.is_empty());
        let settings = scene
            .settings
            .with_resolution(6, 6)
            .with_samples_per_pixel(spp)
            .with_adaptive_sampling(spp, 0.0)
            .with_max_depth(8)
            .with_indirect_clamp(0.0)
            .with_sampling_strategy(strategy)
            .with_light_samples(counts.0, counts.1);
        let b = Renderer::new(scene.camera, scene.world, scene.lights, settings)
            .with_volumes(scene.volumes)
            .render();
        let mut sum = Vec3A::ZERO;
        for y in 0..6 {
            for x in 0..6 {
                sum += b.get_pixel(x, y);
            }
        }
        (sum / 36.0).x as f64
    };
    let reference = mean(SamplingStrategy::PowerMis, (1, 1), 2048);
    assert!(reference > 0.0);
    for (s, counts, spp, tol) in [
        (SamplingStrategy::PowerMis, (1, 4), 1024, 0.03),
        (SamplingStrategy::LightOnly, (1, 4), 1024, 0.03),
        (SamplingStrategy::PowerMis, (4, 4), 1024, 0.03),
        (SamplingStrategy::BsdfOnly, (1, 4), 4096, 0.08),
    ] {
        let m = mean(s, counts, spp);
        assert!(
            rel(m, reference) < tol,
            "{s:?} at {counts:?}: {m} vs {reference}"
        );
    }
}
