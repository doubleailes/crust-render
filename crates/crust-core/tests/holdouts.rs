//! Holdouts (a render pass's `matte`) in the integrator and the film: a camera
//! ray that reaches one ends with nothing — zero colour, zero alpha, every
//! light path expression zero and every data AOV at its clear value — while
//! every other ray meets it as the geometry it is. Built on hand-assembled
//! worlds, flag by flag; the import side is `render_passes.rs`.

use crust_core::rt::Geometry;
use crust_core::{
    Accumulation, AovFilm, AovProduct, AovRequest, AovSource, AovVar, AreaLight, Buffer, Camera,
    DensityField, Emissive, LightList, MASK_CAMERA, OpenPBR, PathSampler, PixelFilter, Precision,
    Ray, RenderSettings, Renderer, SamplingStrategy, SphereShape, Vec3A, VolumeRegion, Volumes,
    World, WorldBuilder, ray_color,
};
use std::sync::Arc;

/// A quad in the plane z = `z`, over `x0..x1` × `-h..h`, facing +Z.
fn wall(z: f32, x0: f32, x1: f32, h: f32) -> Geometry {
    Geometry::TriangleMesh {
        vertices: vec![[x0, -h, z], [x1, -h, z], [x1, h, z], [x0, h, z]],
        indices: vec![[0, 1, 2], [0, 2, 3]],
        normals: None,
    }
}

/// A floor quad at y = 0.
fn floor() -> Geometry {
    Geometry::TriangleMesh {
        vertices: vec![
            [-50.0, 0.0, -50.0],
            [50.0, 0.0, -50.0],
            [50.0, 0.0, 50.0],
            [-50.0, 0.0, 50.0],
        ],
        indices: vec![[0, 2, 1], [0, 3, 2]],
        normals: None,
    }
}

fn grey() -> Arc<OpenPBR> {
    Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5)))
}

/// A white emissive wall of radiance 1 that is also a diffuse surface of
/// albedo 0.8, so the albedo AOV has something to read there.
fn glowing() -> Arc<OpenPBR> {
    Arc::new(OpenPBR {
        emission_luminance: 1.0,
        emission_color: Vec3A::ONE,
        ..OpenPBR::diffuse(Vec3A::splat(0.8))
    })
}

fn var(name: &str, source: AovSource) -> AovVar {
    AovVar {
        prim_path: format!("/Render/Vars/{name}"),
        name: name.to_owned(),
        channel_prefix: None,
        source,
        components: source.components(),
        precision: Precision::Float,
        accumulation: source.default_accumulation(),
        clear: source.default_clear(),
        expression: None,
        raw: false,
        variance: false,
    }
}

fn lpe(expr: &str) -> AovVar {
    AovVar {
        expression: Some(expr.to_owned()),
        accumulation: Accumulation::Filtered,
        components: 3,
        clear: 0.0,
        ..var(expr, AovSource::Lpe)
    }
}

fn request(vars: Vec<AovVar>) -> AovRequest {
    AovRequest {
        precomp: None,
        products: vec![AovProduct {
            prim_path: "/Render/p".into(),
            name: "p.exr".into(),
            vars,
            attributes: Vec::new(),
        }],
    }
}

/// A camera at the origin looking down −Z, `w`×`h` pixels, box-filtered,
/// `spp` samples per pixel on every pixel.
fn renderer(world: World, lights: LightList, w: usize, h: usize, vfov: f32, spp: u32) -> Renderer {
    let camera = Camera::new(
        Vec3A::ZERO,
        -Vec3A::Z,
        Vec3A::Y,
        vfov,
        w as f32 / h as f32,
        0.0,
        1.0,
    );
    let settings = RenderSettings::default()
        .with_resolution(w, h)
        .with_samples_per_pixel(spp)
        .with_adaptive_sampling(spp, 0.0)
        .with_max_depth(4)
        .with_indirect_clamp(0.0)
        .with_pixel_filter("box".parse::<PixelFilter>().unwrap());
    Renderer::new(camera, world, lights, settings)
}

fn render(r: &Renderer, vars: &[AovVar]) -> (Buffer, AovFilm) {
    let (beauty, film, _) = r.render_with_aovs(true, &|_, _| {}, &request(vars.to_vec()));
    (beauty, film)
}

/// The emissive wall at z = −10 behind a holdout at z = −2 covering
/// `x0..x1` (`opacity` its presence). The holdout is black, so the wall's
/// light does not come back off it: what the camera sees of the wall is the
/// wall's emission alone, whatever the coverage.
fn holdout_world(x0: f32, x1: f32, opacity: f32) -> World {
    let mut b = WorldBuilder::new();
    b.attach(wall(-10.0, -100.0, 100.0, 100.0), glowing());
    let id = b.attach(
        wall(-2.0, x0, x1, 50.0),
        Arc::new(OpenPBR {
            geometry_opacity: opacity,
            ..OpenPBR::diffuse(Vec3A::ZERO)
        }),
    );
    b.set_holdout(id, true);
    b.commit()
}

/// rendering spec, "A holdout in front of an emitter": every beauty pixel is
/// exactly 0, and so is its alpha.
#[test]
fn an_opaque_holdout_before_an_emitter_renders_exactly_zero() {
    let world = holdout_world(-50.0, 50.0, 1.0);
    assert!(world.has_holdouts());
    let r = renderer(world, LightList::new(), 8, 6, 40.0, 4);
    let alpha = var("alpha", AovSource::Alpha);
    let (beauty, film) = render(&r, std::slice::from_ref(&alpha));
    let a = film.var_channels(&beauty, &alpha).remove(0);
    for y in 0..6 {
        for x in 0..8 {
            assert_eq!(beauty.get_pixel(x, y), Vec3A::ZERO, "({x}, {y})");
        }
    }
    assert!(a.iter().all(|&v| v == 0.0), "{a:?}");
}

/// rendering spec, "A half-present holdout": a cutout holdout is met with its
/// opacity's probability, so beauty and alpha both converge to 0.5 of the
/// unobstructed wall.
#[test]
fn a_half_present_holdout_leaves_half_the_wall() {
    let world = holdout_world(-50.0, 50.0, 0.5);
    let r = renderer(world, LightList::new(), 4, 4, 30.0, 256);
    let alpha = var("alpha", AovSource::Alpha);
    let (beauty, film) = render(&r, std::slice::from_ref(&alpha));
    let a = film.var_channels(&beauty, &alpha).remove(0);
    let open = {
        let world = holdout_world(-50.0, 50.0, 0.0);
        renderer(world, LightList::new(), 4, 4, 30.0, 16)
            .render()
            .get_pixel(0, 0)
            .x
    };
    assert!(open > 0.5, "the wall glows: {open}");
    let mean_beauty = (0..16)
        .map(|i| beauty.get_pixel(i % 4, i / 4).x)
        .sum::<f32>()
        / 16.0
        / open;
    let mean_alpha = a.iter().sum::<f32>() / a.len() as f32;
    assert!((mean_beauty - 0.5).abs() < 0.03, "beauty {mean_beauty}");
    assert!((mean_alpha - 0.5).abs() < 0.03, "alpha {mean_alpha}");
}

/// A sphere standing on a floor under a sphere light; the sphere a holdout or
/// not.
fn shadow_world(holdout: bool, mirror: bool) -> (World, LightList) {
    let mut b = WorldBuilder::new();
    b.attach(floor(), grey());
    let ball = b.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.0, 1.0, 0.0),
            radius: 1.0,
        },
        grey(),
    );
    b.set_holdout(ball, holdout);
    if mirror {
        // Facing the ball from behind it, along −Z.
        b.attach(
            wall(-4.0, -3.0, 3.0, 3.0),
            Arc::new(OpenPBR::metal(Vec3A::ONE, 0.0)),
        );
    }
    let mut lights = LightList::new();
    let emitter = Arc::new(Emissive::light(Vec3A::splat(40.0), None));
    let shape = SphereShape {
        center: Vec3A::new(0.0, 5.0, 2.0),
        radius: 0.3,
    };
    let id = b.attach_masked(
        Geometry::Sphere {
            center: shape.center,
            radius: shape.radius,
        },
        emitter.clone(),
        crust_core::MASK_INDIRECT,
    );
    b.set_transparent_emitter(id, true);
    lights.add(AreaLight::new(shape, emitter, id));
    (b.commit(), lights)
}

/// The per-sample radiances of `n` camera paths along `ray`, as bits.
fn trace(world: &World, lights: &LightList, volumes: &Volumes, ray: &Ray, n: i32) -> Vec<[u32; 3]> {
    (0..n)
        .map(|i| {
            ray_color(
                ray,
                world,
                lights,
                volumes,
                6,
                SamplingStrategy::PowerMis,
                PathSampler::new(5, 3, 0, i),
            )
            .to_array()
            .map(f32::to_bits)
        })
        .collect()
}

fn mean(bits: &[[u32; 3]]) -> f32 {
    bits.iter().map(|c| f32::from_bits(c[0])).sum::<f32>() / bits.len() as f32
}

/// rendering spec, "A holdout shadows the floor": camera rays that reach the
/// floor in the holdout's shadow see exactly what they see without it.
#[test]
fn a_holdout_shadows_the_floor_as_the_geometry_does() {
    let (with, lights) = shadow_world(true, false);
    let (without, _) = shadow_world(false, false);
    let none = Volumes::default();
    for x in [0.6f32, 0.9, 1.4, 3.0] {
        // Straight down onto the floor, from just above it and outside the ball.
        let ray = Ray::new(Vec3A::new(x, 0.05, 0.4), -Vec3A::Y).with_mask(MASK_CAMERA);
        assert!(
            trace(&with, &lights, &none, &ray, 64) == trace(&without, &lights, &none, &ray, 64),
            "x = {x}"
        );
    }
    // And the shadow is there: under the ball is darker than far from it.
    let at = |x: f32| {
        let ray = Ray::new(Vec3A::new(x, 0.05, 0.4), -Vec3A::Y).with_mask(MASK_CAMERA);
        mean(&trace(&with, &lights, &none, &ray, 64))
    };
    assert!(at(0.6) < 0.5 * at(3.0), "{} vs {}", at(0.6), at(3.0));
}

/// rendering spec, "A holdout in a mirror": the mirror shows the ball shaded
/// exactly as without the pass; the ball seen directly is 0.
#[test]
fn a_mirror_reflects_the_holdout_as_shaded() {
    let (with, lights) = shadow_world(true, true);
    let (without, _) = shadow_world(false, true);
    let none = Volumes::default();
    // From in front of the ball, past it, at the mirror's reflection of it.
    let eye = Vec3A::new(0.0, 1.0, 6.0);
    let at_mirror = Ray::new(eye, Vec3A::new(2.0, 1.0, -4.0) - eye).with_mask(MASK_CAMERA);
    let reflected = trace(&with, &lights, &none, &at_mirror, 32);
    assert!(reflected == trace(&without, &lights, &none, &at_mirror, 32));
    assert!(mean(&reflected) > 0.0);
    let at_ball = Ray::new(eye, -Vec3A::Z).with_mask(MASK_CAMERA);
    assert!(mean(&trace(&without, &lights, &none, &at_ball, 32)) > 0.0);
    assert!(
        trace(&with, &lights, &none, &at_ball, 32)
            .iter()
            .all(|c| *c == [0; 3])
    );
}

/// rendering spec, "Fog in front of a holdout": the fog's in-scatter renders,
/// while the samples that cross it unscattered end on the wall with nothing.
#[test]
fn fog_in_front_of_a_holdout_keeps_its_in_scatter() {
    let mut b = WorldBuilder::new();
    let id = b.attach(wall(-4.0, -50.0, 50.0, 50.0), grey());
    b.set_holdout(id, true);
    let mut lights = LightList::new();
    let emitter = Arc::new(Emissive::light(Vec3A::splat(40.0), None));
    let shape = SphereShape {
        center: Vec3A::new(0.0, 3.0, -2.0),
        radius: 0.3,
    };
    let lid = b.attach_masked(
        Geometry::Sphere {
            center: shape.center,
            radius: shape.radius,
        },
        emitter.clone(),
        crust_core::MASK_INDIRECT,
    );
    b.set_transparent_emitter(lid, true);
    lights.add(AreaLight::new(shape, emitter, lid));
    let world = b.commit();
    let fog = Volumes::new(vec![VolumeRegion::new(
        glam::Mat4::from_translation(glam::Vec3::new(0.0, 0.0, -2.0)),
        Vec3A::new(3.0, 3.0, 1.5),
        Vec3A::splat(0.3),
        Vec3A::ZERO,
        0.0,
        Vec3A::ZERO,
        1.0,
        DensityField::Homogeneous,
    )]);
    let ray = Ray::new(Vec3A::ZERO, -Vec3A::Z).with_mask(MASK_CAMERA);
    let clear = trace(&world, &lights, &Volumes::default(), &ray, 64);
    assert!(clear.iter().all(|c| *c == [0; 3]), "no fog: nothing");
    let foggy = trace(&world, &lights, &fog, &ray, 256);
    assert!(mean(&foggy) > 0.0, "the fog's in-scatter");
    // Some samples crossed it unscattered and ended on the holdout with nothing.
    assert!(foggy.contains(&[0; 3]));
}

/// The aovs spec's holdout scenarios on a 3×1 frame: the left pixel is wholly
/// held out, the middle one half, the right one sees the glowing wall at
/// depth 10 (the holdout is at depth 2).
#[test]
fn aovs_see_a_holdout_sample_as_an_escape() {
    let world = holdout_world(-100.0, 0.0, 1.0);
    let r = renderer(world, LightList::new(), 3, 1, 10.0, 256);
    let mut beauty4 = var("beauty", AovSource::Color);
    beauty4.components = 4;
    let alpha = var("alpha", AovSource::Alpha);
    let depth = var("depth", AovSource::Depth);
    let normal = var("N", AovSource::Normal);
    let p = var("P", AovSource::P);
    let albedo = var("albedo", AovSource::Albedo);
    let all = lpe("C.*[LO]");
    let vars = [
        beauty4.clone(),
        alpha.clone(),
        depth.clone(),
        normal.clone(),
        p.clone(),
        albedo.clone(),
        all.clone(),
    ];
    let (beauty, film) = render(&r, &vars);
    let px = |v: &AovVar, x: usize| film.var_pixel(&beauty, v, x, 0);
    let wall = beauty.get_pixel(2, 0).x;
    assert!(wall > 0.5, "{wall}");

    // Wholly held out: what an escape leaves.
    assert_eq!(beauty.get_pixel(0, 0), Vec3A::ZERO);
    assert_eq!(px(&alpha, 0), [0.0]);
    assert_eq!(px(&depth, 0), [f32::INFINITY], "the clear value");
    assert_eq!(px(&normal, 0), [0.0; 3]);
    assert_eq!(px(&p, 0), [0.0; 3]);
    assert_eq!(px(&albedo, 0), [0.0; 3]);
    assert_eq!(px(&beauty4, 0), [0.0; 4]);

    // Half: premultiplied coverage, and the depth of the visible surface.
    let half = beauty.get_pixel(1, 0).x / wall;
    assert!((half - 0.5).abs() < 0.03, "beauty {half}");
    assert!((px(&alpha, 1)[0] - 0.5).abs() < 0.03, "{:?}", px(&alpha, 1));
    assert!((px(&beauty4, 1)[3] - 0.5).abs() < 0.03);
    let d = px(&depth, 1)[0];
    assert!((d - 10.0).abs() < 1e-3, "depth {d}");
    assert!((px(&p, 1)[2] + 10.0).abs() < 1e-3);
    let full_albedo = px(&albedo, 2)[0];
    assert!((px(&albedo, 1)[0] / full_albedo - 0.5).abs() < 0.03);

    // `C.*[LO]` is the beauty to the bit, holdouts and all.
    let lpe = film.var_channels(&beauty, &all);
    for x in 0..3 {
        let b = beauty.get_pixel(x, 0).to_array();
        for (c, plane) in lpe.iter().enumerate() {
            assert_eq!(plane[x].to_bits(), b[c].to_bits(), "pixel {x}, channel {c}");
        }
    }
}

/// A glowing, absorbing box between z = −3.5 and −0.5 (no scattering), in
/// front of where the holdout wall stands.
fn glow() -> Volumes {
    Volumes::new(vec![VolumeRegion::new(
        glam::Mat4::from_translation(glam::Vec3::new(0.0, 0.0, -2.0)),
        Vec3A::new(3.0, 3.0, 1.5),
        Vec3A::ZERO,
        Vec3A::splat(0.5),
        0.0,
        Vec3A::ONE,
        1.0,
        DensityField::Homogeneous,
    )])
}

/// A holdout wall at z = −4 behind the glowing box, or nothing there.
fn glow_world(holdout: bool) -> World {
    let mut b = WorldBuilder::new();
    if holdout {
        let id = b.attach(wall(-4.0, -50.0, 50.0, 50.0), grey());
        b.set_holdout(id, true);
    }
    b.commit()
}

/// The render-passes spec's "volumes render as without the pass": what a
/// glowing volume emits in front of a holdout stays, as it does in front of
/// an escape. The camera segment ends at the holdout with its emission, not
/// with nothing.
#[test]
fn a_glowing_volume_in_front_of_a_holdout_keeps_its_emission() {
    let ray = Ray::new(Vec3A::ZERO, -Vec3A::Z).with_mask(MASK_CAMERA);
    let lights = LightList::new();
    let held = mean(&trace(&glow_world(true), &lights, &glow(), &ray, 256));
    let open = mean(&trace(&glow_world(false), &lights, &glow(), &ray, 256));
    assert!(open > 0.1, "the box glows: {open}");
    assert!(
        (held / open - 1.0).abs() < 0.02,
        "held out {held}, escaped {open}"
    );

    // And light path expressions still route it: `C.*[LO]` in front of the
    // holdout carries what it carries in front of an escape.
    let all = lpe("C.*[LO]");
    let lpe_mean = |holdout: bool| {
        let r = renderer(glow_world(holdout), LightList::new(), 4, 3, 20.0, 64)
            .with_volumes(glow().into_regions());
        let (beauty, film) = render(&r, std::slice::from_ref(&all));
        let mut sum = 0.0;
        for y in 0..3 {
            for x in 0..4 {
                sum += film.var_pixel(&beauty, &all, x, y)[0];
            }
        }
        sum / 12.0
    };
    let (held, open) = (lpe_mean(true), lpe_mean(false));
    assert!(open > 0.1, "{open}");
    assert!(
        (held / open - 1.0).abs() < 0.03,
        "held out {held}, escaped {open}"
    );
}

/// A holdout's presence is its opacity, not its material: a thin-walled glass
/// matte in front of the glowing wall is a hole, alpha and beauty 0, never a
/// window onto the wall behind it.
#[test]
fn a_glass_holdout_is_a_hole_not_a_window() {
    let mut b = WorldBuilder::new();
    b.attach(wall(-10.0, -100.0, 100.0, 100.0), glowing());
    let id = b.attach(
        wall(-2.0, -50.0, 50.0, 50.0),
        Arc::new(OpenPBR {
            geometry_thin_walled: true,
            ..OpenPBR::glass(1.5)
        }),
    );
    b.set_holdout(id, true);
    let world = b.commit();
    assert!(world.has_straight_transmission());
    let r = renderer(world, LightList::new(), 4, 3, 30.0, 64);
    let alpha = var("alpha", AovSource::Alpha);
    let (beauty, film) = render(&r, std::slice::from_ref(&alpha));
    let a = film.var_channels(&beauty, &alpha).remove(0);
    for y in 0..3 {
        for x in 0..4 {
            assert_eq!(beauty.get_pixel(x, y), Vec3A::ZERO, "({x}, {y})");
        }
    }
    assert!(a.iter().all(|&v| v == 0.0), "{a:?}");
}
