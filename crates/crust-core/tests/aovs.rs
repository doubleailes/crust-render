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
        variance: false,
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
fn scene(spp: u32, variance: f32, wall: bool) -> Renderer {
    scene_exposed(spp, variance, wall, 1.0)
}

/// [`scene`] through a camera whose exposure scale is `exposure`.
fn scene_exposed(spp: u32, variance: f32, wall: bool, exposure: f32) -> Renderer {
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
    let settings = RenderSettings::default()
        .with_resolution(W, H)
        .with_samples_per_pixel(spp)
        .with_max_depth(4)
        .with_adaptive_sampling(spp.min(8), variance)
        .with_exposure_scale(exposure);
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
fn the_beauty_is_bit_identical_with_and_without_aovs() {
    // Adaptive on (variance 0.05) and off: AOVs observe the samples
    // whatever decides how many there are.
    for variance in [0.0, 0.05] {
        let r = scene(16, variance, true);
        let (plain, _) = r.render_with_stats(true, &|_, _| {});
        let req = request(every_source());
        let (with_aovs, _) = render(&r, true, &req);
        assert!(bits(&plain) == bits(&with_aovs), "variance {variance}");
    }
}

/// `var_pixel` reads, at every pixel, exactly what `var_channels` lays out
/// there, for every source: the one-pixel reader and the planes cannot drift.
#[test]
fn var_pixel_is_var_channels_at_every_pixel() {
    let r = scene(4, 0.0, true);
    let vars = every_source();
    let req = request(vars.clone());
    let (beauty, film) = render(&r, true, &req);
    let (w, h) = film.dimensions();
    for v in &vars {
        let planes = film.var_channels(&beauty, v);
        for y in 0..h {
            for x in 0..w {
                let pixel = film.var_pixel(&beauty, v, x, y);
                let laid: Vec<f32> = planes.iter().map(|p| p[y * w + x]).collect();
                assert_eq!(
                    pixel.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                    laid.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                    "{} at ({x}, {y})",
                    v.prim_path
                );
            }
        }
    }
}

#[test]
fn every_channel_is_bit_identical_across_tiles_and_scanlines() {
    {
        // 16 spp, as every image comparison here (CLAUDE.md, "Measuring a
        // change"), with adaptive sampling on: `spp.min(8)` is the minimum.
        let r = scene(16, 0.05, true);
        let vars = every_source();
        let req = request(vars.clone());
        let (tb, tf) = render(&r, true, &req);
        let (sb, sf) = render(&r, false, &req);
        assert!(bits(&tb) == bits(&sb));
        assert!(channel_bits(&tf, &tb, &vars) == channel_bits(&sf, &sb, &vars));
    }
}

#[test]
fn closest_depth_is_never_blended_across_an_edge() {
    let r = scene(16, 0.0, true);
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
    let r = scene(8, 0.0, false);
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
    let r = scene(8, 0.0, false);
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
    let r = scene(8, 0.0, true);
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
    let r = scene(8, 0.0, true);
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
    let r = scene(16, 0.0, false);
    let req = request(vec![var("beauty", AovSource::Color)]);
    assert!(!req.needs_film());
    let (b, film) = render(&r, true, &req);
    assert_eq!(film.dimensions(), (W, H));
    let plain = r.render_with_tiles();
    assert!(bits(&b) == bits(&plain));
    // The beauty's channels still come out of an empty film.
    assert_eq!(film.var_channels(&b, &req.products[0].vars[0]).len(), 3);
}

/// A thin-walled window filling the frame one unit in front of the camera:
/// a path passes it straight through or meets it, but either way it is the
/// camera's first hit — glass, not a hole — so every pixel's depth is the
/// window's, closest and filtered alike, and its alpha is full.
#[test]
fn a_thin_window_is_the_first_hit_whether_passed_or_met() {
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.0, 0.0, 2.0),
            radius: 1.0,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
    world.attach(
        Geometry::TriangleMesh {
            vertices: vec![
                [-50.0, -50.0, 4.0],
                [50.0, -50.0, 4.0],
                [50.0, 50.0, 4.0],
                [-50.0, 50.0, 4.0],
            ],
            indices: vec![[0, 1, 2], [0, 2, 3]],
            normals: None,
        },
        Arc::new(OpenPBR {
            geometry_thin_walled: true,
            transmission_color: Vec3A::new(0.9, 0.7, 0.5),
            ..OpenPBR::glass(1.5)
        }),
    );
    let world = world.commit();
    assert!(world.has_straight_transmission());
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
    let settings = RenderSettings::default()
        .with_resolution(W, H)
        .with_samples_per_pixel(16)
        .with_max_depth(4)
        .with_adaptive_sampling(16, 0.0);
    let r = Renderer::new(camera, world, lights, settings);
    let mut filtered = var("depth_filtered", AovSource::Depth);
    filtered.accumulation = Accumulation::Filtered;
    let vars = vec![
        var("depth", AovSource::Depth),
        filtered,
        var("alpha", AovSource::Alpha),
    ];
    let (beauty, film) = render(&r, true, &request(vars.clone()));
    for v in &vars[..2] {
        for &d in &film.var_channels(&beauty, v)[0] {
            assert!((d - 1.0).abs() < 1e-4, "{}: {d}", v.name);
        }
    }
    assert!(
        film.var_channels(&beauty, &vars[2])[0]
            .iter()
            .all(|&a| a == 1.0)
    );
    // And the ball behind it is seen through it.
    let centre = (H / 2) * W + W / 2;
    assert!(beauty.get_pixel(W / 2, H / 2).x > 0.0, "{centre}");
}

/// A light path expression var over `expr`, and its `crust:aov:variance` twin.
fn lpe(expr: &str) -> AovVar {
    AovVar {
        expression: Some(expr.to_owned()),
        components: 3,
        accumulation: Accumulation::Filtered,
        ..var(expr, AovSource::Lpe)
    }
}

fn lpe_variance(expr: &str) -> AovVar {
    AovVar {
        name: format!("{expr} variance"),
        components: 1,
        variance: true,
        ..lpe(expr)
    }
}

/// One stop of camera exposure doubles every radiance channel exactly (the
/// beauty's colour, a light path expression), quadruples every variance (the
/// `variance` source and an expression's variance), and leaves everything that
/// is not light alone — the beauty's alpha included.
#[test]
fn exposure_scales_radiance_only() {
    let mut vars = every_source();
    vars.push(lpe("C.*[LO]"));
    vars.push(lpe_variance("C.*[LO]"));
    let req = request(vars.clone());
    let (base_buf, base) = render(&scene_exposed(16, 0.0, true, 1.0), true, &req);
    let (lit_buf, lit) = render(&scene_exposed(16, 0.0, true, 2.0), true, &req);
    for v in &vars {
        let (a, b) = (
            base.var_channels(&base_buf, v),
            lit.var_channels(&lit_buf, v),
        );
        for (c, (a, b)) in a.iter().zip(&b).enumerate() {
            let power = if v.variance {
                2
            } else if v.source == AovSource::Color && c == 3 {
                0
            } else {
                v.source.exposure_power()
            };
            let factor = 2.0_f32.powi(power);
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                assert_eq!(
                    (x * factor).to_bits(),
                    y.to_bits(),
                    "{} channel {c} pixel {i}: {x} × {factor} != {y}",
                    v.name
                );
            }
        }
    }
}

/// The exposure multiplies the resolved image only: an adaptive render takes
/// the same samples in every pixel at any exposure.
#[test]
fn exposure_does_not_change_the_samples_taken() {
    let req = request(vec![var("sampleCount", AovSource::SampleCount)]);
    let count = |exposure: f32| {
        let (buffer, film) = render(&scene_exposed(32, 0.05, true, exposure), true, &req);
        film.var_channels(&buffer, &req.products[0].vars[0])
    };
    assert_eq!(count(1.0), count(8.0));
}
