//! Thin-walled transmission is a pass-through: a shadow ray is attenuated by
//! a thin wall's straight transmittance instead of blocked, and a path passes
//! the wall with it or meets the rest of the BSDF. Every scenario of the
//! rendering spec's thin-wall requirements, checked in numbers.

use crust_core::materialx;
use crust_core::rt::Geometry;
use crust_core::{
    AreaLight, Emissive, HitRecord, LightList, MASK_INDIRECT, MASK_SHADOW, Material, OpenPBR,
    PathSampler, Ray, SamplingStrategy, ShadingPoint, SphereShape, Vec3A, Volumes, World,
    WorldBuilder, ray_color,
};
use std::path::PathBuf;
use std::sync::Arc;

fn quad(y: f32, half: f32) -> Geometry {
    Geometry::TriangleMesh {
        vertices: vec![
            [-half, y, -half],
            [half, y, -half],
            [half, y, half],
            [-half, y, half],
        ],
        indices: vec![[0, 2, 1], [0, 3, 2]],
        normals: None,
    }
}

fn thin_glass(tint: Vec3A) -> OpenPBR {
    OpenPBR {
        geometry_thin_walled: true,
        transmission_color: tint,
        ..OpenPBR::glass(1.5)
    }
}

fn mtlx(node: &str) -> Arc<dyn Material> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/materialx_thin_walled.mtlx");
    materialx::load(
        &path,
        Some(node),
        &crust_core::mtlx::Host::new(&|_, _| None),
    )
    .unwrap_or_else(|e| panic!("{node}: {e:?}"))
    .material
}

/// A diffuse floor under a sphere light, with `sheet` (if any) hanging
/// between them.
fn scene(sheet: Option<Arc<dyn Material>>) -> (World, LightList) {
    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();
    world.attach(
        quad(0.0, 50.0),
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
    if let Some(sheet) = sheet {
        world.attach(quad(2.5, 50.0), sheet);
    }
    let emitter = Arc::new(Emissive::new(Vec3A::splat(40.0)));
    let (center, radius) = (Vec3A::new(0.0, 3.5, 0.0), 0.5);
    let id = world.attach_masked(
        Geometry::Sphere { center, radius },
        emitter.clone(),
        MASK_SHADOW | MASK_INDIRECT,
    );
    lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
    (world.commit(), lights)
}

/// The floor beneath the sheet, seen from under it: the mean of `n` paths.
fn floor(world: &World, lights: &LightList, s: SamplingStrategy, n: i32) -> Vec3A {
    let ray = Ray::new(Vec3A::new(0.5, 2.0, 0.5), Vec3A::new(-0.5, -2.0, -0.5));
    let mut sum = [0.0f64; 3];
    for i in 0..n {
        let c = ray_color(
            &ray,
            world,
            lights,
            &Volumes::default(),
            3,
            s,
            PathSampler::new(1, 2, 0, i),
        );
        for (acc, x) in sum.iter_mut().zip(c.to_array()) {
            *acc += x as f64;
        }
    }
    Vec3A::from_array(sum.map(|x| (x / n as f64) as f32))
}

/// Power-MIS, light-only and BSDF-only agree on the floor beneath `sheet`,
/// per channel, and light sampling alone finds the floor lit.
fn assert_strategies_agree(name: &str, sheet: Arc<dyn Material>) {
    let (world, lights) = scene(Some(sheet));
    assert!(world.has_straight_transmission(), "{name}");
    let reference = floor(&world, &lights, SamplingStrategy::PowerMis, 8192);
    assert!(reference.min_element() > 0.01, "{name}: {reference}");
    for (s, n, tol) in [
        (SamplingStrategy::LightOnly, 8192, 0.05),
        // BSDF sampling alone has to find a small light by chance: noisier.
        (SamplingStrategy::BsdfOnly, 65_536, 0.12),
    ] {
        let m = floor(&world, &lights, s, n);
        let err = ((m - reference).abs() / reference).max_element();
        assert!(err < tol, "{name}, {s:?}: {m} vs power-MIS {reference}");
    }
}

#[test]
fn light_through_a_window_is_found_by_every_strategy() {
    assert_strategies_agree(
        "OpenPBR tinted thin glass",
        Arc::new(thin_glass(Vec3A::new(0.9, 0.5, 0.2))),
    );
    assert_strategies_agree(
        "OpenPBR rough thin glass",
        Arc::new(OpenPBR {
            specular_roughness: 0.3,
            ..thin_glass(Vec3A::new(0.3, 0.8, 0.6))
        }),
    );
    assert_strategies_agree(
        "MaterialX open_pbr_surface window",
        mtlx("mtlx_openpbr_window"),
    );
    assert_strategies_agree(
        "MaterialX standard_surface film",
        mtlx("mtlx_standard_film"),
    );
}

/// A sheet that is both a cutout and a thin wall: shadow rays take
/// `(1 − α) + α · T`, and every strategy agrees on the floor beneath it.
#[test]
fn a_sheet_that_is_also_a_cutout() {
    assert_strategies_agree(
        "half-present thin glass",
        Arc::new(OpenPBR {
            geometry_opacity: 0.5,
            ..thin_glass(Vec3A::new(0.9, 0.5, 0.2))
        }),
    );
}

/// A thick dielectric bends what crosses it, so it has no straight
/// transmittance and stays an occluder for shadow rays.
#[test]
fn thick_glass_reports_no_straight_transmittance() {
    let thick = OpenPBR::glass(1.5);
    assert!(!thick.has_straight_transmission());
    let (world, _) = scene(Some(Arc::new(thick.clone())));
    assert!(!world.has_straight_transmission() && !world.has_pass_throughs());
    let rec = HitRecord {
        normal: Vec3A::Y,
        front_face: true,
        ..HitRecord::default()
    };
    let ray = Ray::new(Vec3A::new(0.0, 1.0, 0.0), -Vec3A::Y);
    let sp = ShadingPoint::new(&thick, &ray, &rec, 1.0);
    assert_eq!(
        sp.straight_transmittance(&ray, PathSampler::new(0, 0, 0, 0)),
        Vec3A::ZERO
    );
}

/// A camera looking through a clear tinted sheet (index 1: no reflection,
/// nothing for a met path to scatter) at an emitter of radiance 1: the mean
/// pass throughput is the sheet's straight transmittance — the tint — though
/// a single pass carries `P / q`, the tint over its largest channel.
#[test]
fn the_mean_pass_through_a_tinted_sheet_is_its_transmittance() {
    let tint = Vec3A::new(0.9, 0.5, 0.2);
    let mut world = WorldBuilder::new();
    world.attach(quad(0.0, 50.0), Arc::new(Emissive::new(Vec3A::ONE)));
    world.attach(
        quad(1.0, 50.0),
        Arc::new(OpenPBR {
            specular_ior: 1.0,
            ..thin_glass(tint)
        }),
    );
    let world = world.commit();
    let lights = LightList::new();
    let ray = Ray::new(Vec3A::new(0.0, 2.0, 0.0), -Vec3A::Y);
    let n = 16_384;
    let mut sum = Vec3A::ZERO;
    let mut passes = 0;
    for i in 0..n {
        let c = ray_color(
            &ray,
            &world,
            &lights,
            &Volumes::default(),
            2,
            SamplingStrategy::PowerMis,
            PathSampler::new(3, 4, 0, i),
        );
        if c != Vec3A::ZERO {
            assert!(
                (c - tint / tint.max_element()).abs().max_element() < 1e-5,
                "{c}"
            );
            passes += 1;
        }
        sum += c;
    }
    let mean = sum / n as f32;
    assert!((mean - tint).abs().max_element() < 0.02, "{mean} vs {tint}");
    let q = passes as f32 / n as f32;
    assert!((q - 0.9).abs() < 0.02, "passes with q = max(P): {q}");
}
