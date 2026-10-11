//! End-to-end renders of tiny hand-assembled scenes: settings plumbing,
//! determinism, the row and tile paths, progress reporting, ray counters,
//! and two analytic radiance checks through the integrator.

use crust_core::rt::Geometry;
use crust_core::{
    AreaLight, Buffer, Camera, DistantLight, DomeLight, Emissive, LightList, LightSelection,
    MASK_INDIRECT, MASK_SHADOW, OpenPBR, PathSampler, PdfSolidAngle, PixelFilter, Ray,
    RenderSettings, Renderer, SamplingStrategy, Scene, SphereShape, Vec3A, Volumes, WorldBuilder,
    ray_color,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

fn approx(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol
}

fn buffer_sum(b: &Buffer, w: usize, h: usize) -> f64 {
    let mut s = 0.0;
    for y in 0..h {
        for x in 0..w {
            let c = b.get_pixel(x, y);
            s += (c.x + c.y + c.z) as f64;
        }
    }
    s
}

fn buffers_equal(a: &Buffer, b: &Buffer, w: usize, h: usize) -> bool {
    (0..h).all(|y| (0..w).all(|x| a.get_pixel(x, y) == b.get_pixel(x, y)))
}

/// An emissive sphere of radiance `l` filling the middle of the frame, seen
/// by a camera at z = 5 looking down -Z.
fn emissive_ball_scene(l: f32, w: usize, h: usize, spp: u32) -> Renderer {
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        Arc::new(Emissive::new(Vec3A::splat(l))),
    );
    let camera = Camera::new(
        Vec3A::new(0.0, 0.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        40.0,
        w as f32 / h as f32,
        0.0,
        5.0,
    );
    let settings = RenderSettings::default()
        .with_resolution(w, h)
        .with_samples_per_pixel(spp)
        .with_max_depth(4)
        .with_adaptive_sampling(spp, 0.0);
    Renderer::new(camera, world.commit(), LightList::new(), settings)
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[test]
fn render_settings_report_what_they_were_given() {
    let s = RenderSettings::default()
        .with_resolution(320, 200)
        .with_samples_per_pixel(16)
        .with_max_depth(7)
        .with_adaptive_sampling(4, 0.02)
        .with_frame(3);
    assert_eq!(s.get_dimensions(), (320, 200));
    assert_eq!(s.samples_per_pixel(), 16);
    assert_eq!(s.max_depth(), 7);
    assert_eq!(s.sampling_strategy(), SamplingStrategy::PowerMis);
    assert_eq!(s.pixel_filter(), PixelFilter::Triangle { radius: 1.0 });
    assert_eq!(s.light_selection(), LightSelection::Power);
    assert_eq!(
        s.indirect_clamp(),
        Some(10.0),
        "firefly clamp on by default"
    );
}

#[test]
fn samples_per_pixel_override_is_floored_at_one() {
    let s = RenderSettings::default()
        .with_resolution(8, 8)
        .with_samples_per_pixel(16)
        .with_max_depth(7)
        .with_adaptive_sampling(4, 0.02);
    assert_eq!(s.with_samples_per_pixel(0).samples_per_pixel(), 1);
    assert_eq!(s.with_samples_per_pixel(9).samples_per_pixel(), 9);
    // Unrelated fields are untouched.
    assert_eq!(s.with_samples_per_pixel(9).max_depth(), 7);
}

#[test]
fn strategy_and_filter_builders_replace_their_field() {
    let s = RenderSettings::default()
        .with_resolution(8, 8)
        .with_samples_per_pixel(1)
        .with_max_depth(1)
        .with_adaptive_sampling(1, 0.0)
        .with_sampling_strategy(SamplingStrategy::LightOnly)
        .with_pixel_filter(PixelFilter::Mitchell { radius: 2.0 });
    assert_eq!(s.sampling_strategy(), SamplingStrategy::LightOnly);
    assert_eq!(s.pixel_filter(), PixelFilter::Mitchell { radius: 2.0 });
    assert_eq!(s.samples_per_pixel(), 1);
    let s = s.with_sampling_strategy(SamplingStrategy::BsdfOnly);
    assert_eq!(s.sampling_strategy(), SamplingStrategy::BsdfOnly);
}

#[test]
fn sampling_strategies_partition_unity_for_every_pair() {
    for s in [
        SamplingStrategy::PowerMis,
        SamplingStrategy::BalanceMis,
        SamplingStrategy::LightOnly,
        SamplingStrategy::BsdfOnly,
    ] {
        for (a, b) in [(1.0f32, 1.0f32), (0.1, 7.0), (50.0, 0.5)] {
            let (pa, pb) = (
                PdfSolidAngle::new(a).unwrap(),
                PdfSolidAngle::new(b).unwrap(),
            );
            let sum = s.light_weight(pa, pb) + s.bounce_weight(pb, pa);
            assert!(approx(sum, 1.0, 1e-4), "{s:?} ({a},{b}) = {sum}");
        }
    }
    assert_eq!(SamplingStrategy::default(), SamplingStrategy::PowerMis);
    assert!(SamplingStrategy::PowerMis.samples_lights());
    assert!(SamplingStrategy::BalanceMis.samples_lights());
    assert!(SamplingStrategy::LightOnly.samples_lights());
    assert!(!SamplingStrategy::BsdfOnly.samples_lights());
}

// ---------------------------------------------------------------------------
// Renders
// ---------------------------------------------------------------------------

#[test]
fn an_emissive_ball_renders_its_radiance_in_the_centre() {
    let (w, h) = (9, 9);
    let r = emissive_ball_scene(3.0, w, h, 4);
    let buf = r.render();
    let centre = buf.get_pixel(4, 4);
    assert!(
        centre.abs_diff_eq(Vec3A::splat(3.0), 1e-4),
        "centre pixel {centre}"
    );
    // The corners see past the ball to the background, which is dimmer.
    let corner = buf.get_pixel(0, 0);
    assert!(corner.max_element() < 3.0);
    assert!(corner.min_element() >= 0.0);
}

#[test]
fn rows_and_tiles_render_the_same_image() {
    // Whole-buffer comparisons render at 16 spp, the count the repository
    // fixes for image regressions (see scripts/check_images.sh).
    let (w, h) = (20, 12);
    let r = emissive_ball_scene(1.0, w, h, 16);
    let rows = r.render();
    let tiles = r.render_with_tiles();
    assert!(
        buffers_equal(&rows, &tiles, w, h),
        "tile and row paths diverged"
    );
}

#[test]
fn rendering_is_deterministic() {
    let (w, h) = (12, 8);
    let r = emissive_ball_scene(1.0, w, h, 16);
    let a = r.render();
    let b = r.render();
    assert!(buffers_equal(&a, &b, w, h));
    assert!(buffer_sum(&a, w, h) > 0.0);
}

#[test]
fn a_different_frame_changes_the_noise_but_not_the_mean_much() {
    let (w, h) = (8, 8);
    let camera = Camera::new(
        Vec3A::new(0.0, 0.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        40.0,
        1.0,
        0.0,
        5.0,
    );
    let mk = |frame: isize| {
        let mut b = WorldBuilder::new();
        b.attach(
            Geometry::Sphere {
                center: Vec3A::ZERO,
                radius: 1.0,
            },
            Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
        );
        // A uniform sky, so the ball's shading is sampled noise.
        let mut lights = LightList::new();
        lights.add(DomeLight::new(Vec3A::ONE, None, glam::Mat3A::IDENTITY));
        Renderer::new(
            camera,
            b.commit(),
            lights,
            RenderSettings::default()
                .with_resolution(w, h)
                .with_samples_per_pixel(16)
                .with_max_depth(3)
                .with_adaptive_sampling(16, 0.0)
                .with_frame(frame),
        )
    };
    let a = mk(0).render();
    let b = mk(1).render();
    assert!(
        !buffers_equal(&a, &b, w, h),
        "different frames must decorrelate"
    );
    let (sa, sb) = (buffer_sum(&a, w, h), buffer_sum(&b, w, h));
    assert!((sa - sb).abs() / sa.max(1e-6) < 0.5, "{sa} vs {sb}");
}

#[test]
fn progress_callback_reaches_the_total() {
    let (w, h) = (16, 20);
    // One step per unit per sample per pixel: at 1 spp the first sweep is
    // one stage; at 8 (adaptive sampling off) it is four — 1, 2, 4 and 8 —
    // and their steps add up to the budget.
    for spp in [1, 8] {
        progress_reaches_the_total(w, h, spp);
    }
}

fn progress_reaches_the_total(w: usize, h: usize, spp: u32) {
    let r = emissive_ball_scene(1.0, w, h, spp);
    for tiled in [false, true] {
        let last = AtomicU64::new(0);
        let total = AtomicU64::new(0);
        let calls = AtomicU64::new(0);
        let cb = |done: u64, all: u64| {
            // Reports are never concurrent and always one more than the
            // last — tiles finish on many threads, but the engine serialises
            // what it tells the host (see `ProgressCallback`).
            let before = last.swap(done, Ordering::SeqCst);
            assert!(
                done == before + 1 || done == 1,
                "tiled={tiled}: {before} then {done}"
            );
            total.store(all, Ordering::SeqCst);
            calls.fetch_add(1, Ordering::SeqCst);
        };
        let buf = r.render_with_progress(tiled, &cb);
        assert!(buffer_sum(&buf, w, h) > 0.0);
        let t = total.load(Ordering::SeqCst);
        assert!(t > 0, "tiled={tiled}");
        assert_eq!(
            last.load(Ordering::SeqCst),
            t,
            "tiled={tiled}: progress must finish at total"
        );
        assert!(calls.load(Ordering::SeqCst) >= t, "one report per step");
        if !tiled {
            assert_eq!(
                t,
                h as u64 * spp as u64,
                "row rendering reports each scanline's samples per pixel"
            );
        } else {
            assert_eq!(
                t,
                (w as u64).div_ceil(16) * (h as u64).div_ceil(16) * spp as u64,
                "each 16x16 tile's samples per pixel"
            );
        }
    }
}

#[test]
fn ray_stats_count_every_camera_ray() {
    let (w, h, spp) = (6, 5, 3);
    let r = emissive_ball_scene(1.0, w, h, spp);
    let (buf, stats) = r.render_with_stats(false, &|_, _| {});
    assert!(buffer_sum(&buf, w, h) > 0.0);
    assert_eq!(stats.camera_rays, (w * h) as u64 * spp as u64);
    assert!(stats.closest_hit >= stats.camera_rays);
    assert_eq!(stats.shadow_rays, 0, "no lights, no NEE");
    assert!(stats.total_rays() >= stats.camera_rays);
    assert!(stats.mean_path_length() >= 0.0);
    // Every camera path ends by escaping or on the emitter; none hit the
    // depth cap at depth 4 in a scene that never scatters.
    assert_eq!(stats.ended_depth, 0);
    // ...and each ends exactly one way, which is what lets the report print
    // the four endings as shares of the primary rays.
    assert_eq!(
        stats.ended_escaped + stats.ended_absorbed + stats.rr_killed + stats.ended_depth,
        stats.camera_rays
    );
    assert_eq!(stats.volume_scatters + stats.medium_scatters, 0);
    assert_eq!(stats.surface_vertices(), stats.vertices);
    assert_eq!(stats.light_samples, 0);
    assert_eq!(stats.bounce_rays(), stats.closest_hit - stats.camera_rays);
    // The renderer's final pass is adaptive, so every pixel is counted, and
    // with the threshold at 0 none stops early.
    assert_eq!(stats.adaptive_pixels, (w * h) as u64);
    assert_eq!(stats.early_stopped, 0);
    assert_eq!((stats.spp_min, stats.spp_max), (spp, spp));
}

#[test]
fn adaptive_sampling_takes_fewer_camera_rays_on_a_flat_image() {
    let (w, h) = (6, 6);
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 100.0,
        },
        Arc::new(Emissive::new(Vec3A::ONE)),
    );
    let camera = Camera::new(Vec3A::ZERO, -Vec3A::Z, Vec3A::Y, 40.0, 1.0, 0.0, 5.0);
    // 64 spp allowed, minimum 4, and a zero-variance image: every pixel
    // stops at the first check past the minimum.
    let settings = RenderSettings::default()
        .with_resolution(w, h)
        .with_samples_per_pixel(64)
        .with_max_depth(2)
        .with_adaptive_sampling(4, 0.01);
    let r = Renderer::new(camera, world.commit(), LightList::new(), settings);
    let (buf, stats) = r.render_with_stats(false, &|_, _| {});
    assert!(
        stats.camera_rays < (w * h * 64) as u64,
        "early stop never fired: {}",
        stats.camera_rays
    );
    assert!(stats.camera_rays >= (w * h * 4) as u64);
    assert!(buf.get_pixel(3, 3).abs_diff_eq(Vec3A::ONE, 1e-5));
    assert_eq!(stats.early_stopped, (w * h) as u64, "every pixel stopped");
    assert_eq!(stats.adaptive_samples, stats.camera_rays);
    assert!(stats.spp_min >= 4 && stats.spp_max < 64, "{stats:?}");
}

#[test]
fn scene_new_and_with_volumes_assemble_a_renderer() {
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        Arc::new(Emissive::new(Vec3A::ONE)),
    );
    let camera = Camera::new(
        Vec3A::new(0.0, 0.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        40.0,
        1.0,
        0.0,
        5.0,
    );
    let scene = Scene::new(
        camera,
        world.commit(),
        LightList::new(),
        RenderSettings::default()
            .with_resolution(4, 4)
            .with_samples_per_pixel(1)
            .with_max_depth(2)
            .with_adaptive_sampling(1, 0.0),
    )
    .with_volumes(Vec::new());
    assert!(scene.volumes.is_empty());
    assert!(
        scene.stats.phases.is_empty(),
        "a hand-built scene has no import phases"
    );
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, scene.settings)
        .with_volumes(scene.volumes);
    assert!(renderer.volumes.is_empty());
    let buf = renderer.render();
    assert!(buf.get_pixel(2, 2).x > 0.0);
}

// ---------------------------------------------------------------------------
// Analytic radiance through the integrator
// ---------------------------------------------------------------------------

#[test]
fn ray_color_returns_emission_for_a_direct_hit() {
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        Arc::new(Emissive::new(Vec3A::new(0.25, 0.5, 4.0))),
    );
    let world = world.commit();
    let lights = LightList::new();
    let volumes = Volumes::default();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    for i in 0..8 {
        let c = ray_color(
            &ray,
            &world,
            &lights,
            &volumes,
            4,
            SamplingStrategy::PowerMis,
            PathSampler::new(0, 0, 0, i),
        );
        assert!(c.abs_diff_eq(Vec3A::new(0.25, 0.5, 4.0), 1e-5), "{c}");
    }
}

/// With nothing at infinity there is no sky: an escaping ray is black in
/// every direction.
#[test]
fn ray_color_of_an_escaping_ray_without_lights_is_black() {
    let world = WorldBuilder::new().commit();
    let lights = LightList::new();
    let volumes = Volumes::default();
    for dir in [Vec3A::Y, -Vec3A::Y, Vec3A::X] {
        let c = ray_color(
            &Ray::new(Vec3A::ZERO, dir),
            &world,
            &lights,
            &volumes,
            4,
            SamplingStrategy::PowerMis,
            PathSampler::new(0, 0, 0, 0),
        );
        assert_eq!(c, Vec3A::ZERO, "{dir}");
    }
}

/// A convex diffuse ball inside a large purely emissive enclosure: the
/// incident radiance is `L` from every direction and the ball cannot see
/// itself, so the outgoing radiance should be exactly albedo × L.
///
/// This was ignored for a long time, as the record of a real bug: materials
/// return `brdf · |cos|` and the integrator multiplied by the cosine *again*,
/// so every bounce carried `brdf · cos²` and a Lambertian surface reflected
/// 2/3 of its albedo — this scene measured 0.64 × L. NEE applied the same
/// extra factor, so the two strategies stayed consistent with each other and
/// every `--strategy` agreed on the dimmed value; only a furnace could see it.
/// The integrator no longer applies the second cosine.
///
/// The remaining 4% is the flat `1 − F_avg` dielectric coupling on a material
/// whose `specular_weight` is 0 (see `world_material.rs`), which is why the
/// tolerance is against `0.96 · albedo · L` rather than `albedo · L`.
#[test]
fn a_diffuse_ball_in_a_white_furnace_reflects_albedo_times_radiance() {
    let albedo = 0.6f32;
    let l = 2.0f32;
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        Arc::new(OpenPBR {
            specular_ior: 1.0,
            ..OpenPBR::diffuse(Vec3A::splat(albedo))
        }),
    );
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 50.0,
        },
        Arc::new(Emissive::new(Vec3A::splat(l))),
    );
    let world = world.commit();
    let lights = LightList::new();
    let volumes = Volumes::default();
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let n = 4096;
    let mut sum = 0.0f64;
    for i in 0..n {
        let c = ray_color(
            &ray,
            &world,
            &lights,
            &volumes,
            4,
            SamplingStrategy::PowerMis,
            PathSampler::new(3, 7, 0, i),
        );
        sum += c.x as f64;
    }
    let mean = sum / n as f64;
    // `OpenPBR::diffuse` leaves `specular_weight` at 0, and `eval_diffuse`
    // still takes the flat `1 − f0_from_ior(specular_ior)` off the diffuse —
    // here `specular_ior = 1.0`, so f0 is 0 and the expectation is exact.
    let expected = (albedo * l) as f64;
    assert!(
        (mean - expected).abs() < 0.03 * expected,
        "furnace mean {mean} vs {expected}"
    );
}

#[test]
fn every_sampling_strategy_agrees_on_direct_lighting() {
    // A diffuse floor lit by a small sphere light: the four estimators
    // differ only in variance, so their means must agree.
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
    let emitter = Arc::new(Emissive::new(Vec3A::splat(40.0)));
    let (center, radius) = (Vec3A::new(0.0, 3.0, 0.0), 0.5);
    let id = world.attach_masked(
        Geometry::Sphere { center, radius },
        emitter.clone(),
        MASK_SHADOW | MASK_INDIRECT,
    );
    lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
    let world = world.commit();
    let volumes = Volumes::default();
    let ray = Ray::new(Vec3A::new(0.5, 2.0, 0.5), Vec3A::new(-0.5, -2.0, -0.5));

    let mean_for = |s: SamplingStrategy, n: i32| {
        let mut sum = 0.0f64;
        for i in 0..n {
            sum += ray_color(
                &ray,
                &world,
                &lights,
                &volumes,
                3,
                s,
                PathSampler::new(1, 2, 0, i),
            )
            .x as f64;
        }
        sum / n as f64
    };
    let reference = mean_for(SamplingStrategy::PowerMis, 8192);
    assert!(reference > 0.0);
    for (s, n, tol) in [
        (SamplingStrategy::BalanceMis, 8192, 0.05),
        (SamplingStrategy::LightOnly, 8192, 0.05),
        // BSDF sampling alone has to find a small light by chance: noisier.
        (SamplingStrategy::BsdfOnly, 65_536, 0.12),
    ] {
        let m = mean_for(s, n);
        assert!(
            (m - reference).abs() < tol * reference,
            "{s:?}: {m} vs {reference}"
        );
    }
}

/// A floor under a small, very hot sphere light, a sun and a dome — every way
/// direct light reaches the primary vertex: NEE, a bounce that hits the
/// emitter, a bounce that escapes to a light at infinity. With `bounce_wall`
/// a large diffuse sphere stands beside the light, so light also arrives
/// after a second vertex (floor → sphere → light), which is indirect.
fn clamp_scene(bounce_wall: bool, clamp: f32) -> Renderer {
    let mut world = WorldBuilder::new();
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
    if bounce_wall {
        world.attach(
            Geometry::Sphere {
                center: Vec3A::new(1.5, 1.0, 0.0),
                radius: 1.0,
            },
            Arc::new(OpenPBR::diffuse(Vec3A::splat(0.8))),
        );
    }
    let mut lights = LightList::new();
    let (center, radius) = (Vec3A::new(-0.5, 1.5, 0.0), 0.1);
    let emitter = Arc::new(Emissive::new(Vec3A::new(5000.0, 3000.0, 1000.0)));
    let id = world.attach_masked(
        Geometry::Sphere { center, radius },
        emitter.clone(),
        MASK_SHADOW | MASK_INDIRECT,
    );
    lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
    lights.add(DistantLight::new(
        Vec3A::new(0.3, -1.0, 0.2),
        Vec3A::splat(2.0),
        5.0,
    ));
    lights.add(DomeLight::new(
        Vec3A::splat(0.2),
        None,
        glam::Mat3A::IDENTITY,
    ));
    let camera = Camera::new(
        Vec3A::new(0.0, 3.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        50.0,
        4.0 / 3.0,
        0.0,
        5.0,
    );
    // Adaptive stopping off, so every setting takes the same samples and the
    // renders differ by the clamp alone.
    let settings = RenderSettings::default()
        .with_resolution(32, 24)
        .with_samples_per_pixel(16)
        .with_max_depth(6)
        .with_adaptive_sampling(16, 0.0)
        .with_indirect_clamp(clamp);
    Renderer::new(camera, world.commit(), lights, settings)
}

/// Direct light — NEE, a bounce that hits an emitter, a bounce that escapes
/// to the sun or the dome — is never clamped, however low the limit: both
/// MIS halves must keep full energy or their weights no longer partition it.
/// With no second surface nothing in this scene is indirect, so a clamp of
/// 1e-3 against radiance in the thousands must change no pixel at all.
#[test]
fn indirect_clamp_never_touches_direct_light() {
    let (w, h) = (32, 24);
    let off = clamp_scene(false, 0.0).render();
    let clamped = clamp_scene(false, 1e-3).render();
    assert!(buffer_sum(&off, w, h) > 0.0);
    assert!(
        buffers_equal(&off, &clamped, w, h),
        "direct light was clamped"
    );
}

/// Light that arrives past the primary vertex is capped per sample, so the
/// image darkens monotonically as the limit falls — pixel by pixel, since the
/// clamp changes no sample — and a limit nothing reaches is the unclamped
/// render. Disabled (the default) is the unbiased estimator.
#[test]
fn indirect_clamp_caps_light_found_past_the_first_bounce() {
    let (w, h) = (32, 24);
    let render = |clamp| clamp_scene(true, clamp).render();
    let off = render(0.0);
    let loose = render(1e30);
    let mid = render(1.0);
    let tight = render(0.05);

    let sum_off = buffer_sum(&off, w, h);
    assert!(
        (buffer_sum(&loose, w, h) - sum_off).abs() < 1e-5 * sum_off,
        "a limit nothing reaches is the unclamped render"
    );
    for y in 0..h {
        for x in 0..w {
            let (t, m, l) = (
                tight.get_pixel(x, y),
                mid.get_pixel(x, y),
                loose.get_pixel(x, y),
            );
            assert!(
                t.cmple(m).all() && m.cmple(l).all(),
                "({x}, {y}): {t} / {m} / {l} not monotone in the limit"
            );
        }
    }
    assert!(
        buffer_sum(&tight, w, h) < 0.99 * sum_off,
        "a tight limit removes indirect energy"
    );
}

/// Picking lights by power changes which light each shadow ray goes to, and
/// the pick's probability enters both MIS halves (NEE divides by it, the
/// bounce side weighs found emission with it), so a pmf routed wrongly on
/// either side shows up as a wrong mean. A bright and a dim sphere, a sun and
/// a dome — lights at infinity take the `escaped` path — under power and
/// uniform selection must agree, with MIS and with light sampling alone.
#[test]
fn power_light_selection_agrees_with_uniform_in_expectation() {
    let mut world = WorldBuilder::new();
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
    let mut lights = LightList::new();
    for (center, radius, radiance) in [
        (Vec3A::new(0.0, 3.0, 0.0), 0.5, 40.0),
        (Vec3A::new(2.0, 1.0, 1.0), 0.3, 0.5),
    ] {
        let emitter = Arc::new(Emissive::new(Vec3A::splat(radiance)));
        let id = world.attach_masked(
            Geometry::Sphere { center, radius },
            emitter.clone(),
            MASK_SHADOW | MASK_INDIRECT,
        );
        lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
    }
    lights.add(DistantLight::new(
        Vec3A::new(0.3, -1.0, 0.2),
        Vec3A::splat(2.0),
        5.0,
    ));
    lights.add(DomeLight::new(
        Vec3A::splat(0.2),
        None,
        glam::Mat3A::IDENTITY,
    ));
    let world = world.commit();
    let volumes = Volumes::default();
    let ray = Ray::new(Vec3A::new(0.5, 2.0, 0.5), Vec3A::new(-0.5, -2.0, -0.5));

    let mut by_power = LightList::new();
    for l in lights.lights() {
        by_power.add(l.clone());
    }
    by_power.select_by(LightSelection::Power);
    assert_eq!(by_power.selection(), LightSelection::Power);
    assert!(
        by_power.pmf(0) > 2.0 * by_power.pmf(1),
        "the bright sphere should take more rays than the dim one"
    );
    // The sun and the dome keep their uniform share.
    assert_eq!(by_power.pmf(2), 0.25);
    assert_eq!(by_power.pmf(3), 0.25);

    let mean_for = |list: &LightList, s: SamplingStrategy, n: i32| {
        (0..n)
            .map(|i| {
                ray_color(
                    &ray,
                    &world,
                    list,
                    &volumes,
                    3,
                    s,
                    PathSampler::new(1, 2, 0, i),
                )
                .x as f64
            })
            .sum::<f64>()
            / n as f64
    };
    for s in [SamplingStrategy::PowerMis, SamplingStrategy::LightOnly] {
        let uniform = mean_for(&lights, s, 32_768);
        let power = mean_for(&by_power, s, 32_768);
        assert!(uniform > 0.0);
        assert!(
            (power - uniform).abs() < 0.03 * uniform,
            "{s:?}: power selection {power} vs uniform {uniform}"
        );
    }
}

#[test]
fn light_geometry_hidden_from_camera_rays_still_lights_the_scene() {
    let (w, h) = (7, 7);
    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.0, -101.0, 0.0),
            radius: 100.0,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.8))),
    );
    let emitter = Arc::new(Emissive::new(Vec3A::splat(30.0)));
    let (center, radius) = (Vec3A::new(0.0, 0.0, 2.0), 0.4);
    let id = world.attach_masked(
        Geometry::Sphere { center, radius },
        emitter.clone(),
        MASK_SHADOW | MASK_INDIRECT,
    );
    lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
    // The camera looks straight at the light: its geometry must not show.
    let camera = Camera::new(
        Vec3A::new(0.0, 0.0, 6.0),
        Vec3A::new(0.0, 0.0, 2.0),
        Vec3A::Y,
        30.0,
        1.0,
        0.0,
        4.0,
    );
    let r = Renderer::new(
        camera,
        world.commit(),
        lights,
        RenderSettings::default()
            .with_resolution(w, h)
            .with_samples_per_pixel(8)
            .with_max_depth(3)
            .with_adaptive_sampling(8, 0.0),
    );
    let buf = r.render();
    let centre = buf.get_pixel(3, 3);
    assert!(
        centre.max_element() < 5.0,
        "the light source is visible to the camera: {centre}"
    );
    // The floor below is lit.
    let floor = buf.get_pixel(3, 0);
    assert!(floor.max_element() > 0.0);
}

// ---------------------------------------------------------------------------
// Adaptive sampling: the stop rule
// ---------------------------------------------------------------------------

/// A flat emissive image of radiance `l` (a sphere enclosing the camera),
/// adaptive on: `spp` allowed, authored minimum `min`, threshold 0.01,
/// neighbour tolerance `t`.
fn flat_adaptive_scene(l: f32, w: usize, h: usize, spp: u32, min: u32, t: f32) -> Renderer {
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 100.0,
        },
        Arc::new(Emissive::new(Vec3A::splat(l))),
    );
    let camera = Camera::new(Vec3A::ZERO, -Vec3A::Z, Vec3A::Y, 40.0, 1.0, 0.0, 5.0);
    let settings = RenderSettings::default()
        .with_resolution(w, h)
        .with_samples_per_pixel(spp)
        .with_max_depth(2)
        .with_adaptive_sampling(min, 0.01)
        .with_adaptive_neighbour_tolerance(t);
    Renderer::new(camera, world.commit(), LightList::new(), settings)
}

/// A pixel whose every sample is exactly zero has a measured variance of
/// zero, which is not convergence: nothing has been observed. An empty
/// world must therefore take its whole budget everywhere.
#[test]
fn adaptive_sampling_never_stops_a_pixel_that_has_seen_no_light() {
    let (w, h, spp) = (6, 5, 64);
    let world = WorldBuilder::new();
    let camera = Camera::new(Vec3A::ZERO, -Vec3A::Z, Vec3A::Y, 40.0, 1.0, 0.0, 5.0);
    let settings = RenderSettings::default()
        .with_resolution(w, h)
        .with_samples_per_pixel(spp)
        .with_max_depth(2)
        .with_adaptive_sampling(4, 0.01);
    let r = Renderer::new(camera, world.commit(), LightList::new(), settings);
    let (buf, stats) = r.render_with_stats(false, &|_, _| {});
    assert_eq!(buffer_sum(&buf, w, h), 0.0, "an empty world is black");
    assert_eq!(stats.early_stopped, 0, "{stats:?}");
    assert_eq!((stats.spp_min, stats.spp_max), (spp, spp), "{stats:?}");
    assert_eq!(stats.camera_rays, (w * h) as u64 * spp as u64);
}

/// The minimum grows with the budget: an authored minimum of 8 at 1024 spp
/// is floored at √1024 = 32, even on an image that converges at once.
#[test]
fn adaptive_minimum_is_floored_at_sqrt_spp() {
    let (w, h) = (4, 4);
    let r = flat_adaptive_scene(1.0, w, h, 1024, 8, -1.0);
    let (_, stats) = r.render_with_stats(false, &|_, _| {});
    assert_eq!(stats.early_stopped, (w * h) as u64, "{stats:?}");
    assert!(stats.spp_min >= 32, "{stats:?}");
    assert!(stats.spp_max < 1024, "{stats:?}");
}

/// A flat image with one noisy pixel: a small emitter that only the centre
/// pixel's samples sometimes hit. At `t = 1` its four cross neighbours wait
/// for it; at `t = -1` they stop as soon as they converge.
fn one_noisy_pixel_scene(w: usize, h: usize, spp: u32, t: f32) -> Renderer {
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 100.0,
        },
        Arc::new(Emissive::new(Vec3A::ONE)),
    );
    // A small, very bright ball in front of the centre pixel, covering
    // about a quarter of it: the pixel's samples split between 1 and 400.
    world.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.0, 0.0, -10.0),
            radius: 0.3,
        },
        Arc::new(Emissive::new(Vec3A::splat(400.0))),
    );
    let camera = Camera::new(Vec3A::ZERO, -Vec3A::Z, Vec3A::Y, 40.0, 1.0, 0.0, 5.0);
    let settings = RenderSettings::default()
        .with_resolution(w, h)
        .with_samples_per_pixel(spp)
        .with_max_depth(2)
        .with_adaptive_sampling(4, 0.01)
        .with_pixel_filter(PixelFilter::BoxFilter { radius: 0.5 })
        .with_adaptive_neighbour_tolerance(t);
    Renderer::new(camera, world.commit(), LightList::new(), settings)
}

#[test]
fn a_less_converged_cross_neighbour_holds_a_pixel() {
    let (w, h, spp) = (7, 7, 1024);
    let (buf, on) = one_noisy_pixel_scene(w, h, spp, 1.0).render_with_stats(false, &|_, _| {});
    let (_, off) = one_noisy_pixel_scene(w, h, spp, -1.0).render_with_stats(false, &|_, _| {});
    let centre = buf.get_pixel(3, 3);
    assert!(
        centre.x > 1.5,
        "the centre pixel must see the bright ball: {centre}"
    );
    assert!(
        buf.get_pixel(1, 1).abs_diff_eq(Vec3A::ONE, 1e-5),
        "the rest of the image is flat"
    );
    // Off: the 48 flat pixels stop at the first check past the effective
    // minimum (32 at 1024 spp); only the centre keeps going.
    assert!(off.early_stopped >= (w * h) as u64 - 1, "{off:?}");
    assert_eq!(off.neighbour_held, 0, "{off:?}");
    // On: the four cross neighbours of the noisy pixel are held while it
    // is still sampling, so the pass spends more samples, and exactly those
    // four pixels are counted as held.
    assert_eq!(on.neighbour_held, 4, "{on:?}");
    assert!(
        on.adaptive_samples > off.adaptive_samples,
        "held neighbours take more samples: {} vs {}",
        on.adaptive_samples,
        off.adaptive_samples
    );
}

/// The neighbour comparison reads a buffer no pixel writes during the
/// decision, so the schedule cannot leak into it: tiles and scanlines agree
/// bit for bit with the comparison on.
#[test]
fn tiles_and_scanlines_agree_under_the_neighbour_comparison() {
    let (w, h, spp) = (20, 18, 256);
    let rows = one_noisy_pixel_scene(w, h, spp, 1.0);
    let (a, sa) = rows.render_with_stats(false, &|_, _| {});
    let (b, sb) = rows.render_with_stats(true, &|_, _| {});
    assert!(buffers_equal(&a, &b, w, h));
    assert_eq!(sa, sb);
    assert!(sa.neighbour_held > 0, "{sa:?}");
}
