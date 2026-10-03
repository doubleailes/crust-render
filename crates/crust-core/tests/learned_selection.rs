//! `crust:lightSelection = "learned"`: visibility-aware light selection.
//!
//! The scene is the failure it exists for, in miniature. A floor is lit by a
//! dim sphere light it can see, and by a far more powerful one it cannot,
//! sealed inside an opaque shell. Power selection sends most shadow rays to
//! the sealed light; every one of them is occluded.

use crust_core::rt::Geometry;
use crust_core::{
    AreaLight, Buffer, Camera, Emissive, Light, LightList, LightSelection, MASK_INDIRECT,
    MASK_SHADOW, OpenPBR, PathSampler, Ray, RenderSettings, Renderer, SamplingStrategy,
    SphereShape, Vec3A, Volumes, WorldBuilder, ray_color,
};
use std::sync::Arc;

const VISIBLE: Vec3A = Vec3A::new(0.0, 3.0, 0.0);
const SEALED: Vec3A = Vec3A::new(3.0, 1.0, 0.0);
const W: usize = 64;
const H: usize = 36;

/// Returns the renderer, whose light list is learned (or not), and a list
/// over the same lights picking by power, for comparison.
fn scene(selection: LightSelection, spp: u32) -> (Renderer, LightList) {
    let mut world = WorldBuilder::new();
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
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
    // The shell sealing the powerful light.
    world.attach(
        Geometry::Sphere {
            center: SEALED,
            radius: 0.8,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
    let mut lights = LightList::new();
    for (center, radiance) in [(VISIBLE, 5.0), (SEALED, 400.0)] {
        let emitter = Arc::new(Emissive::new(Vec3A::splat(radiance)));
        let id = world.attach_masked(
            Geometry::Sphere {
                center,
                radius: 0.3,
            },
            emitter.clone(),
            MASK_SHADOW | MASK_INDIRECT,
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
    let mut by_power = LightList::new();
    for l in lights.lights() {
        by_power.add(l.clone());
    }
    by_power.select_by(LightSelection::Power);
    let camera = Camera::new(
        Vec3A::new(0.0, 5.0, 7.0),
        Vec3A::ZERO,
        Vec3A::Y,
        50.0,
        W as f32 / H as f32,
        0.0,
        8.0,
    );
    let settings = RenderSettings::new(spp, 4, W, H, spp, 0.0, 0).with_light_selection(selection);
    (
        Renderer::new(camera, world.commit(), lights, settings),
        by_power,
    )
}

#[test]
fn a_sealed_light_is_learned_down_to_the_defensive_share() {
    let (r, by_power) = scene(LightSelection::Learned, 1);
    assert_eq!(r.lights.selection(), LightSelection::Learned);
    // Power alone favours the sealed light, which is the whole problem.
    assert!(by_power.pmf(1) > by_power.pmf(0));

    let p = Vec3A::new(0.0, 0.0, 0.0);
    let (visible, sealed) = (r.lights.pmf_at(p, 0), r.lights.pmf_at(p, 1));
    // Nothing ever reached the floor from the sealed light, so all it keeps
    // is the defensive share: 0.3 over two lights.
    assert!((sealed - 0.15).abs() < 1e-6, "sealed light pmf {sealed}");
    assert!((visible + sealed - 1.0).abs() < 1e-6);

    // The bounce side reads the same numbers the pick reports.
    for index in 0..2 {
        let geom = r.lights.lights()[index].geom_id().unwrap();
        let (_, pmf) = r.lights.find_by_geom_at(geom, p).unwrap();
        assert_eq!(pmf, r.lights.pmf_at(p, index));
        let (_, it) = r.lights.iter_at(p).nth(index).unwrap();
        assert_eq!(it, pmf);
    }
    // And the pick lands on each light as often as its pmf says.
    let n = 10_000;
    let picked_sealed = (0..n)
        .filter(|&i| {
            let (light, pmf) = r.lights.pick_at(p, (i as f32 + 0.5) / n as f32).unwrap();
            assert_eq!(
                pmf,
                r.lights.pmf_at(
                    p,
                    if std::ptr::eq(light, &r.lights.lights()[0]) {
                        0
                    } else {
                        1
                    }
                )
            );
            std::ptr::eq(light, &r.lights.lights()[1])
        })
        .count();
    assert!((picked_sealed as f32 / n as f32 - sealed).abs() < 1e-3);

    // Far outside anything trained, the global power table answers.
    let far = Vec3A::splat(1e4);
    assert_eq!(r.lights.pmf_at(far, 1), by_power.pmf(1));
}

/// Unbiased (same mean as power selection) and less noisy, at a floor point:
/// every shadow ray power spends on the sealed light is wasted. The path runs
/// three bounces, so the indirect variance both strategies share bounds the
/// ratio from below (measured 0.60 under both MIS and light sampling alone).
#[test]
fn learned_selection_agrees_with_power_in_expectation_with_less_variance() {
    let (r, by_power) = scene(LightSelection::Learned, 1);
    let volumes = Volumes::default();
    let ray = Ray::new(Vec3A::new(0.5, 4.0, 3.0), Vec3A::new(-0.5, -4.0, -3.0));
    let moments = |list: &LightList, s: SamplingStrategy| {
        let n = 32_768;
        let (mut sum, mut sq) = (0.0f64, 0.0f64);
        for i in 0..n {
            let v = ray_color(
                &ray,
                &r.world,
                list,
                &volumes,
                3,
                s,
                PathSampler::new(3, 4, 0, i),
            )
            .x as f64;
            sum += v;
            sq += v * v;
        }
        let mean = sum / n as f64;
        (mean, sq / n as f64 - mean * mean)
    };
    for s in [SamplingStrategy::PowerMis, SamplingStrategy::LightOnly] {
        let (learned, var_learned) = moments(&r.lights, s);
        let (power, var_power) = moments(&by_power, s);
        eprintln!(
            "{s:?}: variance ratio learned / power = {:.3}",
            var_learned / var_power
        );
        assert!(power > 0.0);
        assert!(
            (learned - power).abs() < 0.03 * power,
            "{s:?}: learned {learned} vs power {power}"
        );
        assert!(
            var_learned < 0.75 * var_power,
            "{s:?}: learned variance {var_learned} vs power {var_power}"
        );
    }
}

fn same(a: &Buffer, b: &Buffer) -> bool {
    (0..H).all(|y| (0..W).all(|x| a.get_pixel(x, y).to_array() == b.get_pixel(x, y).to_array()))
}

/// The cache is frozen before the first pass and built from the scene alone,
/// so the render mode stays a scheduling choice and reruns agree.
#[test]
fn learned_renders_are_deterministic_and_scheduling_free() {
    // At 16 spp, as every image comparison in the repository is: with the
    // minimum at the budget no pixel stops adaptively, so an exact match is a
    // statement about scheduling alone.
    let (a, _) = scene(LightSelection::Learned, 16);
    let (b, _) = scene(LightSelection::Learned, 16);
    let tiles = a.render_with(crust_core::RenderOrder::Tiles, None).0;
    assert!(same(&tiles, &a.render()), "tiles vs scanlines");
    assert!(
        same(
            &tiles,
            &b.render_with(crust_core::RenderOrder::Tiles, None).0
        ),
        "two builds of the cache"
    );
}

/// With too few lights to choose between, there is nothing to learn, and the
/// selection stays what power would have been.
#[test]
fn one_light_learns_nothing() {
    let mut world = WorldBuilder::new();
    let emitter = Arc::new(Emissive::new(Vec3A::ONE));
    let id = world.attach(
        Geometry::Sphere {
            center: VISIBLE,
            radius: 0.3,
        },
        emitter.clone(),
    );
    let mut lights = LightList::new();
    lights.add(AreaLight::new(
        SphereShape {
            center: VISIBLE,
            radius: 0.3,
        },
        emitter,
        id,
    ));
    let camera = Camera::new(
        Vec3A::new(0.0, 5.0, 7.0),
        Vec3A::ZERO,
        Vec3A::Y,
        50.0,
        1.0,
        0.0,
        8.0,
    );
    let settings =
        RenderSettings::new(1, 2, 8, 8, 1, 0.0, 0).with_light_selection(LightSelection::Learned);
    let r = Renderer::new(camera, world.commit(), lights, settings);
    assert_eq!(r.lights.selection(), LightSelection::Power);
}
