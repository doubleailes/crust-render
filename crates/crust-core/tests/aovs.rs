//! The AOV film: what `Renderer::render_with_aovs` fills, and the guarantees
//! that tie it to the beauty — the beauty never changes because an AOV was
//! asked for, every channel is the same whichever order the pixels were
//! rendered in, and data AOVs never blend two surfaces.

use crust_core::rt::Geometry;
use crust_core::{
    Accumulation, AovFilm, AovProduct, AovRequest, AovSource, AovVar, Buffer, Camera, DomeLight,
    LightList, OpenPBR, Precision, RenderSettings, Renderer, Vec3A, WorldBuilder,
};
use std::sync::Arc;

const W: usize = 24;
const H: usize = 16;

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
    }
}

fn request(vars: Vec<AovVar>) -> AovRequest {
    AovRequest {
        products: vec![AovProduct {
            prim_path: "/Render/p".into(),
            name: "p.exr".into(),
            vars,
            attributes: Vec::new(),
        }],
    }
}

fn every_source() -> Vec<AovVar> {
    let mut beauty = var("beauty", AovSource::Color);
    beauty.components = 4;
    let mut filtered_depth = var("depth_filtered", AovSource::Depth);
    filtered_depth.accumulation = Accumulation::Filtered;
    vec![
        beauty,
        var("alpha", AovSource::Alpha),
        var("depth", AovSource::Depth),
        filtered_depth,
        var("distance", AovSource::Distance),
        var("P", AovSource::P),
        var("Peye", AovSource::Peye),
        var("N", AovSource::Normal),
        var("Neye", AovSource::Neye),
        var("st", AovSource::St),
        var("sampleCount", AovSource::SampleCount),
        var("variance", AovSource::Variance),
    ]
}

/// A grey ball at the origin in front of a far wall (a huge sphere whose
/// near side sits at z = −5), lit by a uniform dome; the camera at z = 5
/// looks down −Z with a frame wide enough to see past the wall's edge.
fn scene(spp: u32, variance: f32, guiding: bool, wall: bool) -> Renderer {
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.0, 0.0, 2.0),
            radius: 1.0,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
    if wall {
        world.attach(
            Geometry::Sphere {
                center: Vec3A::new(-104.0, 0.0, -105.0),
                radius: 100.0,
            },
            Arc::new(OpenPBR::diffuse(Vec3A::splat(0.3))),
        );
    }
    let mut lights = LightList::new();
    lights.add(DomeLight::new(Vec3A::ONE, None, glam::Mat3A::IDENTITY));
    let camera = Camera::new(
        Vec3A::new(0.0, 0.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        60.0,
        W as f32 / H as f32,
        0.0,
        5.0,
    );
    // One training iteration: with two or more, whether the final pass is
    // guided depends on their wall-clock efficiency (`ΔEff`), and a render
    // is not repeatable at all — let alone comparable with another one.
    let settings =
        RenderSettings::new(spp, 4, W, H, spp.min(8), variance, 0).with_guiding(guiding, 1, 0.5);
    Renderer::new(camera, world.commit(), lights, settings)
}

fn render(r: &Renderer, tiled: bool, req: &AovRequest) -> (Buffer, AovFilm) {
    let (buffer, film, _) = r.render_with_aovs(tiled, &|_, _| {}, req);
    (buffer, film)
}

fn bits(b: &Buffer) -> Vec<[u32; 3]> {
    (0..H)
        .flat_map(|y| (0..W).map(move |x| (x, y)))
        .map(|(x, y)| {
            let c = b.get_pixel(x, y);
            [c.x.to_bits(), c.y.to_bits(), c.z.to_bits()]
        })
        .collect()
}

fn channel_bits(film: &AovFilm, beauty: &Buffer, vars: &[AovVar]) -> Vec<Vec<u32>> {
    vars.iter()
        .flat_map(|v| film.var_channels(beauty, v))
        .map(|plane| plane.iter().map(|x| x.to_bits()).collect())
        .collect()
}

#[test]
fn guided_renders_repeat() {
    let r = scene(16, 0.0, true, true);
    let a = r.render_with_stats(true, &|_, _| {}).0;
    let b = r.render_with_stats(true, &|_, _| {}).0;
    assert!(bits(&a) == bits(&b));
}

#[test]
fn the_beauty_is_bit_identical_with_and_without_aovs() {
    // Adaptive on (variance 0.05) and off, guided and not: AOVs observe the
    // samples whatever decides how many there are.
    for (variance, guiding) in [(0.0, false), (0.05, false), (0.0, true)] {
        let r = scene(16, variance, guiding, true);
        let (plain, _) = r.render_with_stats(true, &|_, _| {});
        let req = request(every_source());
        let (with_aovs, _) = render(&r, true, &req);
        assert!(
            bits(&plain) == bits(&with_aovs),
            "variance {variance}, guiding {guiding}"
        );
    }
}

#[test]
fn every_channel_is_bit_identical_across_tiles_and_scanlines() {
    for guiding in [false, true] {
        // 16 spp, as every image comparison here (CLAUDE.md, "Measuring a
        // change"), with adaptive sampling on: `spp.min(8)` is the minimum.
        let r = scene(16, 0.05, guiding, true);
        let vars = every_source();
        let req = request(vars.clone());
        let (tb, tf) = render(&r, true, &req);
        let (sb, sf) = render(&r, false, &req);
        assert!(bits(&tb) == bits(&sb));
        assert!(
            channel_bits(&tf, &tb, &vars) == channel_bits(&sf, &sb, &vars),
            "guiding {guiding}"
        );
    }
}

#[test]
fn closest_depth_is_never_blended_across_an_edge() {
    let r = scene(16, 0.0, false, true);
    let vars = vec![var("depth", AovSource::Depth), {
        let mut v = var("depth_filtered", AovSource::Depth);
        v.accumulation = Accumulation::Filtered;
        v
    }];
    let (beauty, film) = render(&r, true, &request(vars.clone()));
    let closest = &film.var_channels(&beauty, &vars[0])[0];
    let filtered = &film.var_channels(&beauty, &vars[1])[0];
    // The ball's visible side spans depth 2..3 (camera at 5, ball at 2 with
    // radius 1), the wall sits at depth 10 and a bit beyond, and the sky
    // is +inf.
    let on_ball = |d: f32| (2.0..=3.0).contains(&d);
    // The wall is the near side of a huge sphere: depth 10 on the axis,
    // more towards the frame's edge.
    let on_wall = |d: f32| d.is_finite() && d >= 10.0;
    for &d in closest {
        assert!(on_ball(d) || on_wall(d) || d == f32::INFINITY, "{d}");
    }
    assert!(closest.iter().any(|&d| on_ball(d)));
    assert!(closest.iter().any(|&d| on_wall(d)));
    assert!(closest.contains(&f32::INFINITY));
    // The filtered twin does blend at the ball's silhouette — which is
    // exactly why depth defaults to closest.
    assert!(
        filtered.iter().any(|&d| d > 3.0 && d < 10.0),
        "the filtered depth should blend somewhere"
    );
}

#[test]
fn the_world_normal_of_a_sphere_spans_minus_one_to_one() {
    let r = scene(8, 0.0, false, false);
    let vars = vec![var("N", AovSource::Normal), var("Neye", AovSource::Neye)];
    let (beauty, film) = render(&r, true, &request(vars.clone()));
    let n = film.var_channels(&beauty, &vars[0]);
    let neye = film.var_channels(&beauty, &vars[1]);
    let (nx, nz) = (&n[0], &n[2]);
    assert!(nx.iter().any(|&x| x < -0.5) && nx.iter().any(|&x| x > 0.5));
    for q in 0..W * H {
        let len = (nx[q] * nx[q] + n[1][q] * n[1][q] + nz[q] * nz[q]).sqrt();
        assert!(len <= 1.0 + 1e-4, "{len}");
    }
    // The pixel at the frame's centre looks straight at the ball: its
    // normal points back at the camera, +Z in world and in camera space
    // alike (the camera looks down −Z with no rotation).
    let centre = (H / 2) * W + W / 2;
    assert!(nz[centre] > 0.9, "{}", nz[centre]);
    assert!(neye[2][centre] > 0.9, "{}", neye[2][centre]);
    // Sky pixels keep the clear value.
    assert_eq!(nx[0], 0.0);
}

#[test]
fn alpha_is_zero_where_only_the_dome_is_seen() {
    let r = scene(8, 0.0, false, false);
    let mut beauty_var = var("beauty", AovSource::Color);
    beauty_var.components = 4;
    let vars = vec![beauty_var, var("alpha", AovSource::Alpha)];
    let (beauty, film) = render(&r, true, &request(vars.clone()));
    let rgba = film.var_channels(&beauty, &vars[0]);
    let alpha = &film.var_channels(&beauty, &vars[1])[0];
    assert_eq!(rgba.len(), 4);
    assert_eq!(&rgba[3], alpha, "color4f's A is the alpha source");
    // The corner sees the dome: colour, no coverage.
    assert!(rgba[0][0] > 0.0);
    assert_eq!(alpha[0], 0.0);
    let centre = (H / 2) * W + W / 2;
    assert_eq!(alpha[centre], 1.0);
    assert!(alpha.iter().all(|a| (0.0..=1.0).contains(a)));
}

#[test]
fn sample_count_is_the_budget_with_adaptive_sampling_off() {
    let r = scene(8, 0.0, false, true);
    let vars = vec![
        var("sampleCount", AovSource::SampleCount),
        var("variance", AovSource::Variance),
    ];
    let (beauty, film) = render(&r, true, &request(vars.clone()));
    let n = &film.var_channels(&beauty, &vars[0])[0];
    assert!(n.iter().all(|&n| n == 8.0));
    let v = &film.var_channels(&beauty, &vars[1])[0];
    assert!(v.iter().all(|v| v.is_finite() && *v >= 0.0));
    assert!(v.iter().any(|&v| v > 0.0));
}

#[test]
fn world_and_camera_positions_agree_with_the_distances() {
    let r = scene(8, 0.0, false, true);
    let vars = vec![
        var("P", AovSource::P),
        var("Peye", AovSource::Peye),
        var("depth", AovSource::Depth),
        var("distance", AovSource::Distance),
    ];
    let (beauty, film) = render(&r, true, &request(vars.clone()));
    let p = film.var_channels(&beauty, &vars[0]);
    let peye = film.var_channels(&beauty, &vars[1]);
    let depth = &film.var_channels(&beauty, &vars[2])[0];
    let distance = &film.var_channels(&beauty, &vars[3])[0];
    for q in 0..W * H {
        if !depth[q].is_finite() {
            assert_eq!(distance[q], f32::INFINITY);
            continue;
        }
        // All four are the same (closest) sample.
        assert!((-peye[2][q] - depth[q]).abs() < 1e-4);
        let d = Vec3A::new(p[0][q], p[1][q], p[2][q] - 5.0).length();
        assert!((d - distance[q]).abs() < 1e-3, "{d} vs {}", distance[q]);
    }
}

#[test]
fn a_beauty_only_request_returns_an_empty_film() {
    let r = scene(16, 0.0, false, false);
    let req = request(vec![var("beauty", AovSource::Color)]);
    assert!(!req.needs_film());
    let (b, film) = render(&r, true, &req);
    assert_eq!(film.dimensions(), (W, H));
    let plain = r.render_with_tiles();
    assert!(bits(&b) == bits(&plain));
    // The beauty's channels still come out of an empty film.
    assert_eq!(film.var_channels(&b, &req.products[0].vars[0]).len(), 3);
}
