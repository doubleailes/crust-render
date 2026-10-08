//! Render regions: a crop renders exactly the pixels the full frame would.
//! Every pixel of a region — the beauty, an LPE, depth and `sampleCount` —
//! is bit-identical to the same pixel of a full-frame render whenever its
//! sample count does not depend on its neighbours, and a region is a
//! scheduling choice like tiles and scanlines.

use std::path::PathBuf;
use std::sync::Arc;

use crust_core::rt::Geometry;
use crust_core::{
    Accumulation, AovFilm, AovProduct, AovRequest, AovSource, AovVar, Buffer, Camera, DomeLight,
    LightList, OpenPBR, PixelRect, Precision, RenderSettings, Renderer, Scene, Vec3A, WorldBuilder,
};

/// The crop the change's design names, inside a frame large enough for it
/// and away from the frame's origin, so an offset bug cannot hide.
const REGION: PixelRect = PixelRect::new(37, 21, 101, 77);
const W: usize = 128;
const H: usize = 96;

fn var(name: &str, source: AovSource, expression: Option<&str>) -> AovVar {
    AovVar {
        prim_path: format!("/Render/Vars/{name}"),
        name: name.to_owned(),
        channel_prefix: None,
        source,
        components: if source == AovSource::Lpe {
            3
        } else {
            source.components()
        },
        precision: Precision::Float,
        accumulation: if source == AovSource::Lpe {
            Accumulation::Filtered
        } else {
            source.default_accumulation()
        },
        clear: source.default_clear(),
        expression: expression.map(str::to_owned),
        raw: false,
    }
}

fn vars() -> Vec<AovVar> {
    vec![
        var("beauty", AovSource::Color, None),
        var("direct_diffuse", AovSource::Lpe, Some("C<RD>L")),
        var("depth", AovSource::Depth, None),
        var("sampleCount", AovSource::SampleCount, None),
    ]
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

/// Every channel of every var, each a top-down plane of the film's size,
/// as bits.
fn planes(film: &AovFilm, beauty: &Buffer, vars: &[AovVar]) -> Vec<Vec<u32>> {
    vars.iter()
        .flat_map(|v| film.var_channels(beauty, v))
        .map(|plane| plane.iter().map(|x| x.to_bits()).collect())
        .collect()
}

/// The pixels of `region` cut out of full-frame planes `w` pixels wide.
fn crop(full: &[Vec<u32>], w: usize, region: PixelRect) -> Vec<Vec<u32>> {
    full.iter()
        .map(|plane| {
            (region.y0..region.y1)
                .flat_map(|y| (region.x0..region.x1).map(move |x| plane[y * w + x]))
                .collect()
        })
        .collect()
}

fn cornellbox() -> Scene {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join("cornellbox.usda");
    Scene::from_usd(&path).expect("load cornellbox.usda")
}

/// The Cornell box at 16 spp, once full frame and once over `REGION`,
/// with the stage's own adaptive settings: at 16 spp every pixel takes
/// exactly 16 samples, so no pixel's count depends on a neighbour.
fn render_cornellbox(region: Option<PixelRect>, tiled: bool) -> (Buffer, AovFilm) {
    let scene = cornellbox();
    let mut settings = scene
        .settings
        .with_resolution(W, H)
        .with_samples_per_pixel(16)
        .with_max_depth(6);
    if let Some(r) = region {
        settings = settings.with_region(r).expect("inside the frame");
    }
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings);
    let (buffer, film, _) = renderer.render_with_aovs(tiled, &|_, _| {}, &request(vars()));
    (buffer, film)
}

#[test]
fn a_crop_matches_the_full_render_bit_for_bit() {
    let vars = vars();
    let (full_b, full_f) = render_cornellbox(None, true);
    let (crop_b, crop_f) = render_cornellbox(Some(REGION), true);
    assert_eq!(crop_b.size(), (REGION.width(), REGION.height()));
    assert_eq!(crop_b.frame_size(), (W, H));
    assert_eq!(crop_b.region(), REGION);
    assert_eq!(crop_f.dimensions(), (REGION.width(), REGION.height()));
    let full = crop(&planes(&full_f, &full_b, &vars), W, REGION);
    let cropped = planes(&crop_f, &crop_b, &vars);
    assert_eq!(full.len(), cropped.len());
    // Not vacuous: the crop sees the scene, and every pixel took 16 samples.
    assert!(cropped[0].iter().any(|&b| f32::from_bits(b) > 0.0));
    assert!(
        cropped
            .last()
            .unwrap()
            .iter()
            .all(|&b| f32::from_bits(b) == 16.0)
    );
    for (k, (a, b)) in full.iter().zip(&cropped).enumerate() {
        assert!(a == b, "channel {k} differs between the crop and the frame");
    }
}

#[test]
fn tiles_and_scanlines_agree_on_a_region() {
    let vars = vars();
    let (tb, tf) = render_cornellbox(Some(REGION), true);
    let (sb, sf) = render_cornellbox(Some(REGION), false);
    assert!(planes(&tf, &tb, &vars) == planes(&sf, &sb, &vars));
}

/// A grey ball in a uniform dome, small enough to render often.
fn ball(region: Option<PixelRect>, tolerance: f32) -> Renderer {
    const BW: usize = 40;
    const BH: usize = 24;
    let mut world = WorldBuilder::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::ZERO,
            radius: 1.0,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
    let mut lights = LightList::new();
    lights.add(DomeLight::new(Vec3A::ONE, None, glam::Mat3A::IDENTITY));
    let camera = Camera::new(
        Vec3A::new(0.0, 0.0, 4.0),
        Vec3A::ZERO,
        Vec3A::Y,
        45.0,
        BW as f32 / BH as f32,
        0.0,
        4.0,
    );
    let mut settings = RenderSettings::default()
        .with_resolution(BW, BH)
        .with_samples_per_pixel(64)
        .with_max_depth(4)
        .with_adaptive_sampling(8, 0.05)
        .with_adaptive_neighbour_tolerance(tolerance);
    if let Some(r) = region {
        settings = settings.with_region(r).expect("inside the frame");
    }
    Renderer::new(camera, world.commit(), lights, settings)
}

/// With adaptive sampling on and the neighbour hold off, each pixel stops
/// on its own test alone, so a region still matches the frame — sample
/// counts included — even where pixels stop early.
#[test]
fn an_adaptive_crop_without_the_neighbour_hold_matches_the_frame() {
    let region = PixelRect::new(5, 3, 29, 19);
    let vars = vec![
        var("beauty", AovSource::Color, None),
        var("sampleCount", AovSource::SampleCount, None),
    ];
    let req = request(vars.clone());
    let (fb, ff, _) = ball(None, -1.0).render_with_aovs(true, &|_, _| {}, &req);
    let (cb, cf, rays) = ball(Some(region), -1.0).render_with_aovs(true, &|_, _| {}, &req);
    assert!(
        rays.early_stopped > 0,
        "the test needs pixels that stop early"
    );
    let (fw, _) = ff.dimensions();
    assert!(crop(&planes(&ff, &fb, &vars), fw, region) == planes(&cf, &cb, &vars));
}

/// The neighbour hold over a region: nothing outside it is sampled, so a
/// border pixel is never held by it — the render still covers the region
/// and every pixel takes between the minimum and the budget.
#[test]
fn the_neighbour_hold_renders_a_region() {
    let region = PixelRect::new(5, 3, 29, 19);
    let r = ball(Some(region), 1.0);
    let (buffer, rays) = r.render_with_stats(true, &|_, _| {});
    assert_eq!(buffer.size(), (24, 16));
    assert_eq!(rays.adaptive_pixels, 24 * 16);
    assert!(rays.spp_min >= 8 && rays.spp_max <= 64);
}
