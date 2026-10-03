//! Light path expression AOVs and the albedo: the guarantees that tie them
//! to the beauty — `C.*[LO]` is the beauty bit for bit, a partition of
//! expressions sums to it (with the firefly clamp too), the per-lobe split is
//! unbiased, light groups select their lights, and NEE-only and BSDF-only
//! renders agree per expression.

use crust_core::rt::Geometry;
use crust_core::{
    Accumulation, AovFilm, AovProduct, AovRequest, AovSource, AovVar, AreaLight, Buffer, Camera,
    DomeLight, Emissive, LightList, MASK_INDIRECT, MASK_SHADOW, OpenPBR, PixelFilter, Precision,
    RenderSettings, Renderer, SamplingStrategy, SphereShape, Vec3A, WorldBuilder,
};
use std::sync::Arc;

const W: usize = 24;
const H: usize = 16;

fn lpe(expr: &str) -> AovVar {
    AovVar {
        prim_path: format!("/Render/Vars/{expr}"),
        name: expr.to_owned(),
        channel_prefix: None,
        source: AovSource::Lpe,
        components: 3,
        precision: Precision::Float,
        accumulation: Accumulation::Filtered,
        clear: 0.0,
        expression: Some(expr.to_owned()),
        raw: false,
    }
}

fn raw(name: &str, source: AovSource) -> AovVar {
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

#[derive(Clone)]
struct Opts {
    spp: u32,
    depth: u32,
    clamp: f32,
    strategy: SamplingStrategy,
    ball: OpenPBR,
    glass: bool,
    /// An emissive sphere that is not a light: `O` events.
    glow: bool,
    /// Two area lights tagged `key` and `fill`.
    lights: bool,
    /// Keep the `fill` light (with `lights`).
    fill: bool,
    dome: bool,
    wall: bool,
    /// Where the wall (the near side of a radius-100 sphere) is centred.
    wall_center: Vec3A,
    guiding: bool,
    filter: PixelFilter,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            spp: 16,
            depth: 6,
            clamp: 0.0,
            strategy: SamplingStrategy::PowerMis,
            ball: OpenPBR {
                specular_roughness: 0.3,
                coat_weight: 0.5,
                coat_roughness: 0.0,
                ..OpenPBR::diffuse(Vec3A::new(0.7, 0.4, 0.2))
            },
            glass: true,
            glow: true,
            lights: true,
            fill: true,
            dome: true,
            wall: true,
            wall_center: Vec3A::new(-104.0, 0.0, -105.0),
            guiding: false,
            filter: PixelFilter::default(),
        }
    }
}

fn scene(o: &Opts) -> Renderer {
    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.0, 0.0, 2.0),
            radius: 1.0,
        },
        Arc::new(o.ball.clone()),
    );
    if o.wall {
        world.attach(
            Geometry::Sphere {
                center: o.wall_center,
                radius: 100.0,
            },
            Arc::new(OpenPBR::diffuse(Vec3A::new(0.2, 0.6, 0.3))),
        );
    }
    if o.glass {
        world.attach(
            Geometry::Sphere {
                center: Vec3A::new(1.7, -0.3, 2.6),
                radius: 0.5,
            },
            Arc::new(OpenPBR {
                transmission_weight: 1.0,
                specular_roughness: 0.05,
                ..OpenPBR::default()
            }),
        );
    }
    if o.glow {
        world.attach(
            Geometry::Sphere {
                center: Vec3A::new(-1.6, 0.8, 2.2),
                radius: 0.3,
            },
            Arc::new(Emissive::new(Vec3A::new(3.0, 2.0, 1.0))),
        );
    }
    if o.lights {
        for (center, radiance, tag) in [
            (Vec3A::new(-2.0, 3.0, 4.0), 20.0, "key"),
            (Vec3A::new(3.0, 1.0, 4.0), 6.0, "fill"),
        ] {
            if tag == "fill" && !o.fill {
                continue;
            }
            let radius = 0.6;
            let emitter = Arc::new(Emissive::new(Vec3A::splat(radiance)));
            let id = world.attach_masked(
                Geometry::Sphere { center, radius },
                emitter.clone(),
                MASK_SHADOW | MASK_INDIRECT,
            );
            lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
            let index = lights.count() - 1;
            lights.set_lpe_tag(index, Some(tag));
        }
    }
    if o.dome {
        lights.add(DomeLight::new(
            Vec3A::new(0.3, 0.35, 0.5),
            None,
            glam::Mat3A::IDENTITY,
        ));
    }
    let camera = Camera::new(
        Vec3A::new(0.0, 0.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        60.0,
        W as f32 / H as f32,
        0.0,
        5.0,
    );
    let settings = RenderSettings::new(o.spp, o.depth, W, H, o.spp, 0.0, 0)
        .with_indirect_clamp(o.clamp)
        .with_sampling_strategy(o.strategy)
        .with_guiding(o.guiding, 1, 0.5)
        .with_pixel_filter(o.filter);
    Renderer::new(camera, world.commit(), lights, settings)
}

fn render(o: &Opts, vars: &[AovVar]) -> (Buffer, AovFilm) {
    let r = scene(o);
    let (b, f, _) = r.render_with_aovs(true, &|_, _| {}, &request(vars.to_vec()));
    (b, f)
}

/// The beauty's R, G, B planes, in the film's (top-down) order.
fn beauty_planes(film: &AovFilm, beauty: &Buffer) -> Vec<Vec<f32>> {
    film.var_channels(beauty, &raw("beauty", AovSource::Color))
}

fn bits(planes: &[Vec<f32>]) -> Vec<Vec<u32>> {
    planes
        .iter()
        .map(|p| p.iter().map(|x| x.to_bits()).collect())
        .collect()
}

fn mean(plane: &[f32]) -> f64 {
    plane.iter().map(|&x| x as f64).sum::<f64>() / plane.len() as f64
}

#[test]
fn the_full_path_expression_is_the_beauty_bitwise() {
    for (clamp, guiding) in [(0.0, false), (10.0, false), (0.5, false), (0.0, true)] {
        let o = Opts {
            clamp,
            guiding,
            ..Opts::default()
        };
        let all = lpe("C.*[LO]");
        // A second expression in the same DFA must not disturb the first.
        let (beauty, film) = render(&o, &[all.clone(), lpe("C<RD>L")]);
        let channel = film.var_channels(&beauty, &all);
        assert!(
            bits(&channel) == bits(&beauty_planes(&film, &beauty)),
            "clamp {clamp}, guiding {guiding}"
        );
        // Asking for expressions changes nothing in the beauty.
        let plain = scene(&o).render_with_tiles();
        let (pb, pf) = (plain, &film);
        assert!(bits(&beauty_planes(pf, &pb)) == bits(&beauty_planes(&film, &beauty)));
    }
}

/// The first event after the camera is exactly one of these.
const PARTITION: &[&str] = &[
    "C<RD>[LO]",
    "C<RD>.+[LO]",
    "C<RG>[LO]",
    "C<RG>.+[LO]",
    "C<T.>.*[LO]",
    "C<RS>.*[LO]",
    "C<V.>.*[LO]",
    "C[LO]",
];

fn assert_sums(parts: &[Vec<Vec<f32>>], whole: &[Vec<f32>], what: &str) {
    for c in 0..3 {
        for q in 0..W * H {
            let sum: f64 = parts.iter().map(|p| p[c][q] as f64).sum();
            let b = whole[c][q] as f64;
            assert!(
                (sum - b).abs() <= 1e-4 * b.abs().max(1.0),
                "{what}: channel {c} pixel {q}: parts sum to {sum}, beauty {b}"
            );
        }
    }
}

#[test]
fn a_partition_sums_to_the_beauty() {
    for clamp in [0.0, 1.0] {
        let o = Opts {
            clamp,
            ..Opts::default()
        };
        let vars: Vec<AovVar> = PARTITION.iter().map(|e| lpe(e)).collect();
        let (beauty, film) = render(&o, &vars);
        let parts: Vec<_> = vars.iter().map(|v| film.var_channels(&beauty, v)).collect();
        assert_sums(
            &parts,
            &beauty_planes(&film, &beauty),
            &format!("clamp {clamp}"),
        );
        // Every part of this scene carries light somewhere.
        for (v, p) in vars.iter().zip(&parts) {
            if v.name != "C<V.>.*[LO]" {
                assert!(mean(&p[0]) + mean(&p[1]) > 0.0, "{} is empty", v.name);
            }
        }
    }
}

#[test]
fn light_groups_select_their_lights() {
    let vars = vec![
        lpe("C.*<L.'key'>"),
        lpe("C.*<L.'fill'>"),
        lpe("C.*<L.[^'key' 'fill']>"),
        lpe("C.*O"),
    ];
    let (beauty, film) = render(&Opts::default(), &vars);
    let parts: Vec<_> = vars.iter().map(|v| film.var_channels(&beauty, v)).collect();
    assert_sums(&parts, &beauty_planes(&film, &beauty), "groups");
    for p in &parts {
        assert!(mean(&p[1]) > 0.0);
    }

    // The key group is, in expectation, the image the key light makes on
    // its own: the same scene with nothing else lit.
    let o = Opts {
        spp: 256,
        dome: false,
        glow: false,
        ..Opts::default()
    };
    let (b, f) = render(&o, &vars[..1]);
    let key = mean(&f.var_channels(&b, &vars[0])[1]);
    let (b, f) = render(&Opts { fill: false, ..o }, &[lpe("C.*L")]);
    let alone = mean(&f.var_channels(&b, &lpe("C.*L"))[1]);
    assert!(
        (key - alone).abs() <= 0.05 * key.abs(),
        "key group {key} vs the key light alone {alone}"
    );
}

/// RMS difference between two planes.
fn rms(a: &[f32], b: &[f32]) -> f64 {
    (a.iter()
        .zip(b)
        .map(|(x, y)| ((x - y) as f64).powi(2))
        .sum::<f64>()
        / a.len() as f64)
        .sqrt()
}

/// Labelling a bounce by the lobe that was picked would be biased under
/// crust's one-sample mixture; the per-lobe split is not. Each lobe's direct
/// light converges to that of a material carrying the lobe alone.
#[test]
fn the_lobe_split_converges_to_each_lobe_alone() {
    let mixed = OpenPBR {
        specular_roughness: 0.3,
        ..OpenPBR::diffuse(Vec3A::new(0.7, 0.4, 0.2))
    };
    let mixed = OpenPBR {
        specular_weight: 1.0,
        ..mixed
    };
    let base = Opts {
        depth: 1,
        glass: false,
        glow: false,
        dome: false,
        wall: false,
        ..Opts::default()
    };
    let at = |ball: &OpenPBR, spp: u32, var: &AovVar| {
        let (b, f) = render(
            &Opts {
                spp,
                ball: ball.clone(),
                ..base.clone()
            },
            std::slice::from_ref(var),
        );
        f.var_channels(&b, var)[1].clone()
    };
    let diffuse_only = OpenPBR {
        specular_weight: 0.0,
        ..mixed.clone()
    };
    let specular_only = OpenPBR {
        base_weight: 0.0,
        ..mixed.clone()
    };
    for (expr, alone) in [("C<RD>L", &diffuse_only), ("C<RG>L", &specular_only)] {
        let var = lpe(expr);
        let reference = at(alone, 1024, &lpe("C.*L"));
        let coarse = rms(&at(&mixed, 16, &var), &reference);
        let fine = rms(&at(&mixed, 256, &var), &reference);
        assert!(coarse > 0.0, "{expr}");
        // 16× the samples: noise falls 4×; a bias would plateau.
        assert!(
            fine < coarse / 2.0,
            "{expr}: rms {coarse} at 16 spp, {fine} at 256 spp"
        );
    }
}

/// Light sampling only and BSDF sampling only are both unbiased, so every
/// expression has the same expectation under either. Every lobe here is
/// rough: a near-mirror lobe (the default ball's coat at roughness 0, GGX
/// α = 1e-4) is one NEE practically never lands in, and light-only renders
/// it black at any sample count a test can afford — unbiased, but not
/// comparable.
#[test]
fn nee_only_and_bsdf_only_agree_per_expression() {
    let rough = OpenPBR {
        specular_roughness: 0.3,
        coat_weight: 0.5,
        coat_roughness: 0.4,
        ..OpenPBR::diffuse(Vec3A::new(0.7, 0.4, 0.2))
    };
    let vars: Vec<AovVar> = ["C<RD>L", "C<RG'coat'>L", "C<RD>.+L", "C.*<L.'key'>"]
        .iter()
        .map(|e| lpe(e))
        .collect();
    let at = |strategy| {
        let (b, f) = render(
            &Opts {
                spp: 2048,
                strategy,
                glow: false,
                glass: false,
                ball: rough.clone(),
                ..Opts::default()
            },
            &vars,
        );
        vars.iter()
            .map(|v| mean(&f.var_channels(&b, v)[1]))
            .collect::<Vec<f64>>()
    };
    let light = at(SamplingStrategy::LightOnly);
    let bsdf = at(SamplingStrategy::BsdfOnly);
    for ((v, l), b) in vars.iter().zip(&light).zip(&bsdf) {
        assert!(
            (l - b).abs() <= 0.1 * l.abs().max(b.abs()),
            "{}: light only {l}, BSDF only {b}",
            v.name
        );
    }
}

#[test]
fn albedo_is_the_first_non_delta_surface_through_glass() {
    let pane = OpenPBR {
        transmission_weight: 1.0,
        geometry_thin_walled: true,
        specular_roughness: 0.0,
        ..OpenPBR::default()
    };
    let o = Opts {
        ball: pane,
        glass: false,
        glow: false,
        lights: false,
        // Straight behind the pane.
        wall_center: Vec3A::new(0.0, 0.0, -105.0),
        ..Opts::default()
    };
    let albedo = raw("albedo", AovSource::Albedo);
    let (beauty, film) = render(&o, std::slice::from_ref(&albedo));
    let a = film.var_channels(&beauty, &albedo);
    // Through the middle of the pane: the wall's green, dimmed by the pane.
    let centre = (H / 2) * W + W / 2;
    let (r, g, b) = (a[0][centre], a[1][centre], a[2][centre]);
    assert!(
        g > 1.5 * r && g > 1.5 * b,
        "albedo through glass ({r}, {g}, {b})"
    );
    for c in &a {
        assert!(c.iter().all(|x| (0.0..=1.0).contains(x)));
    }
}

fn raw_lpe(expr: &str) -> AovVar {
    AovVar {
        raw: true,
        name: format!("raw {expr}"),
        ..lpe(expr)
    }
}

fn diffuse_filter_var() -> AovVar {
    AovVar {
        prim_path: "/Render/Vars/diffuse_albedo".into(),
        name: "diffuse_albedo".into(),
        channel_prefix: None,
        source: AovSource::DiffuseFilter,
        components: 3,
        precision: Precision::Float,
        accumulation: Accumulation::Filtered,
        clear: 0.0,
        expression: None,
        raw: false,
    }
}

/// Per camera sample, raw light times the diffuse filter is the light. At
/// one sample per pixel through a box filter a pixel *is* a sample, so the
/// identity holds in every pixel — at edges, through the clamp, everywhere
/// the filter is not black.
#[test]
fn raw_light_times_the_filter_is_the_light_per_sample() {
    for clamp in [0.0, 1.0] {
        let o = Opts {
            spp: 1,
            clamp,
            filter: PixelFilter::BoxFilter { radius: 0.5 },
            ..Opts::default()
        };
        let lit = lpe("C<RD>[LO]");
        let rawv = raw_lpe("C<RD>[LO]");
        let filter = diffuse_filter_var();
        let (beauty, film) = render(&o, &[lit.clone(), rawv.clone(), filter.clone()]);
        let (l, r, f) = (
            film.var_channels(&beauty, &lit),
            film.var_channels(&beauty, &rawv),
            film.var_channels(&beauty, &filter),
        );
        let mut checked = 0;
        for c in 0..3 {
            for q in 0..W * H {
                if f[c][q] < crust_core::aov::RAW_FILTER_FLOOR {
                    assert_eq!(r[c][q], 0.0, "raw where the filter is black");
                    continue;
                }
                let back = r[c][q] * f[c][q];
                assert!(
                    (back - l[c][q]).abs() <= 1e-5 * l[c][q].abs().max(1e-3),
                    "clamp {clamp}: {back} vs {}",
                    l[c][q]
                );
                checked += 1;
            }
        }
        assert!(checked > 100, "{checked}");
    }
}

/// Raw light is the light without the surface's colour: two balls of very
/// different colours, lit alike, have the same raw light (to EON's faint
/// nonlinearity in the colour), and very different lighting.
#[test]
fn raw_light_does_not_carry_the_surface_colour() {
    let at = |c: Vec3A| {
        let o = Opts {
            spp: 64,
            ball: OpenPBR::diffuse(c),
            glass: false,
            glow: false,
            dome: false,
            wall: false,
            ..Opts::default()
        };
        let vars = [lpe("C<RD>[LO]"), raw_lpe("C<RD>[LO]")];
        let (b, f) = render(&o, &vars);
        (
            mean(&f.var_channels(&b, &vars[0])[0]),
            mean(&f.var_channels(&b, &vars[1])[0]),
        )
    };
    let (lit_red, raw_red) = at(Vec3A::new(0.8, 0.2, 0.2));
    let (lit_green, raw_green) = at(Vec3A::new(0.2, 0.8, 0.2));
    assert!(lit_red > 3.0 * lit_green, "{lit_red} vs {lit_green}");
    assert!(
        (raw_red - raw_green).abs() <= 0.05 * raw_red,
        "raw {raw_red} vs {raw_green}"
    );
}

/// Adding raw and filter AOVs changes neither the beauty nor any other AOV,
/// and raw light is 0 wherever no diffuse surface was seen.
#[test]
fn raw_aovs_disturb_nothing_and_are_zero_off_diffuse_surfaces() {
    let o = Opts::default();
    let lit = lpe("C<RD>[LO]");
    let (b0, f0) = render(&o, std::slice::from_ref(&lit));
    let filter = diffuse_filter_var();
    let vars = [
        lit.clone(),
        raw_lpe("C<RD>[LO]"),
        raw_lpe("C<RD>.+[LO]"),
        filter.clone(),
    ];
    let (b1, f1) = render(&o, &vars);
    assert!(bits(&beauty_planes(&f0, &b0)) == bits(&beauty_planes(&f1, &b1)));
    assert!(bits(&f0.var_channels(&b0, &lit)) == bits(&f1.var_channels(&b1, &lit)));
    let f = f1.var_channels(&b1, &filter);
    for v in &vars[1..3] {
        let r = f1.var_channels(&b1, v);
        for c in 0..3 {
            for q in 0..W * H {
                if f[c][q] == 0.0 {
                    assert_eq!(r[c][q], 0.0, "{}: raw off a diffuse surface", v.name);
                }
            }
        }
    }
    // The glass ball and the sky are in frame: some pixels have no filter.
    assert!(f[1].contains(&0.0));
}
