//! `crust:lightSelection = "tree"`: the light BVH.
//!
//! A diffuse floor lit by sphere lights at different heights and distances,
//! a one-sided panel facing it and one facing away, a sphere light below the
//! floor (which an opaque floor can never see), and a dome. Power selection
//! spends its rays by flux alone; the tree ranks them from each receiver.

use crust_core::rt::Geometry;
use crust_core::{
    AreaLight, Buffer, Camera, DomeLight, Emissive, LightList, LightSelection, MASK_INDIRECT,
    MASK_SHADOW, OpenPBR, PathSampler, Ray, RectShape, RenderSettings, Renderer, SamplingStrategy,
    SphereShape, Vec3A, Volumes, WorldBuilder, ray_color,
};
use std::sync::Arc;

const W: usize = 64;
const H: usize = 36;

/// The renderer under `selection`, and a list over the same lights picking
/// by power, for comparison.
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
    let mut lights = LightList::new();
    // Spheres: a near dim one, a far bright one, one below the floor.
    for (center, radiance) in [
        (Vec3A::new(-1.5, 1.2, 0.5), 20.0),
        (Vec3A::new(4.0, 6.0, -5.0), 200.0),
        (Vec3A::new(1.0, 3.0, 2.0), 40.0),
        (Vec3A::new(0.0, -2.0, 0.0), 500.0),
    ] {
        let emitter = Arc::new(Emissive::light(Vec3A::splat(radiance), None));
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
    // Two-sided panels: one above the floor tilted toward it, one standing
    // edge-on to it.
    for (origin, eu, ev, radiance) in [
        (
            Vec3A::new(-0.5, 2.5, -2.0),
            Vec3A::new(1.0, 0.0, 0.0),
            Vec3A::new(0.0, 0.3, 0.8),
            15.0,
        ),
        (
            Vec3A::new(2.5, 0.5, 1.0),
            Vec3A::new(0.0, 1.0, 0.0),
            Vec3A::new(0.0, 0.0, 1.0),
            25.0,
        ),
    ] {
        let normal = eu.cross(ev).normalize();
        let emitter = Arc::new(Emissive::new(Vec3A::splat(radiance)));
        let (a, b, c, d) = (origin, origin + eu, origin + eu + ev, origin + ev);
        let id = world.attach_masked(
            Geometry::TriangleMesh {
                vertices: [a, b, c, d].map(|v| v.to_array()).to_vec(),
                indices: vec![[0, 1, 2], [0, 2, 3]],
                normals: None,
            },
            emitter.clone(),
            MASK_SHADOW | MASK_INDIRECT,
        );
        lights.add(AreaLight::new(
            RectShape::new(origin, eu, ev, normal),
            emitter,
            id,
        ));
    }
    lights.add(DomeLight::new(
        Vec3A::splat(0.2),
        None,
        glam::Mat3A::IDENTITY,
    ));
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
    let settings = RenderSettings::default()
        .with_resolution(W, H)
        .with_samples_per_pixel(spp)
        .with_max_depth(4)
        .with_adaptive_sampling(spp, 0.0)
        .with_light_selection(selection);
    (
        Renderer::new(camera, world.commit(), lights, settings),
        by_power,
    )
}

/// The mean and variance of a floor point's estimate.
fn moments(r: &Renderer, list: &LightList, s: SamplingStrategy, n: i32) -> (f64, f64) {
    let volumes = Volumes::default();
    let ray = Ray::new(Vec3A::new(0.5, 4.0, 3.0), Vec3A::new(-0.5, -4.0, -3.0));
    let (mut sum, mut sq) = (0.0f64, 0.0f64);
    for i in 0..n {
        let v = ray_color(
            &ray,
            r.world(),
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
}

#[test]
fn the_tree_is_installed() {
    let (r, _) = scene(LightSelection::Tree, 1);
    assert_eq!(r.lights().selection(), LightSelection::Tree);
}

/// Light-only, BSDF-only and MIS estimates of a floor point agree under the
/// tree, and with power's: one strategy on both MIS sides, and unbiased. The
/// tree also spends fewer rays on lights the floor cannot see, so its
/// light-only variance is no worse than power's.
#[test]
fn the_strategies_agree_on_a_lit_floor() {
    let (r, by_power) = scene(LightSelection::Tree, 1);
    let n = 32_768;
    let (reference, _) = moments(&r, &by_power, SamplingStrategy::PowerMis, n);
    assert!(reference > 0.0);
    for s in [
        SamplingStrategy::PowerMis,
        SamplingStrategy::LightOnly,
        SamplingStrategy::BsdfOnly,
    ] {
        let (mean, var) = moments(&r, r.lights(), s, n);
        let (_, var_power) = moments(&r, &by_power, s, n);
        eprintln!(
            "{s:?}: tree {mean:.5} against power-MIS {reference:.5}; variance ratio tree / power {:.3}",
            var / var_power
        );
        // BSDF-only finds the small lights rarely: a looser bound.
        let tolerance = if s == SamplingStrategy::BsdfOnly {
            0.08
        } else {
            0.03
        };
        assert!(
            (mean - reference).abs() < tolerance * reference,
            "{s:?}: tree {mean} vs power {reference}"
        );
        if s == SamplingStrategy::LightOnly {
            assert!(var < var_power, "tree variance {var} vs power {var_power}");
        }
    }
}

fn same(a: &Buffer, b: &Buffer) -> bool {
    (0..H).all(|y| (0..W).all(|x| a.get_pixel(x, y).to_array() == b.get_pixel(x, y).to_array()))
}

/// The tree is built from the lights alone and frozen before the first pass,
/// so the render mode and the thread count stay scheduling choices.
#[test]
fn tree_renders_are_deterministic_and_scheduling_free() {
    let (a, _) = scene(LightSelection::Tree, 16);
    let (b, _) = scene(LightSelection::Tree, 16);
    let tiles = a.render_with_tiles();
    assert!(same(&tiles, &a.render()), "tiles vs scanlines");
    assert!(
        same(&tiles, &b.render_with_tiles()),
        "two builds of the tree"
    );
    let one_thread = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap()
        .install(|| b.render_with_tiles());
    assert!(same(&tiles, &one_thread), "one thread vs the default");
}

/// A light below an opaque floor keeps only its defensive share there; one
/// above it keeps more than its share.
#[test]
fn a_light_below_an_opaque_floor_keeps_only_its_defensive_share() {
    let (r, by_power) = scene(LightSelection::Tree, 1);
    let floor = crust_core::Receiver::surface(Vec3A::new(0.3, 0.0, 0.2), Vec3A::Y, true);
    let below = 3;
    let share = crust_core::TREE_DEFENSIVE_SHARE * by_power.pmf(below);
    assert!(
        (r.lights().pmf_at(floor, below) - share).abs() < 1e-7,
        "{} against {share}",
        r.lights().pmf_at(floor, below)
    );
    // A transmissive floor sees it.
    let glass = crust_core::Receiver::surface(floor.p, Vec3A::Y, false);
    assert!(r.lights().pmf_at(glass, below) > 1.5 * share);
    // The near light above it gets more than power gives it.
    assert!(r.lights().pmf_at(floor, 0) > by_power.pmf(0));
}
