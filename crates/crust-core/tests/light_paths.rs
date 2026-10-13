//! The light path inspector (`openspec/specs/light-path-inspection`) on
//! real scenes: the records partition the beauty, sample by sample and pixel
//! by pixel, with the clamp and without; the region is the frame's pixels;
//! the report is the same bytes with any thread count; a rare caustic ranks
//! first; and every expression, authored as an LPE AOV, routes its path.

use std::path::PathBuf;
use std::sync::Arc;

use crust_core::inspect::{self, GroupBy, Options, Report};
use crust_core::rt::Geometry;
use crust_core::{
    Accumulation, AovProduct, AovRequest, AovSource, AovVar, AreaLight, Camera, Emissive,
    LightList, MASK_INDIRECT, MASK_SHADOW, OpenPBR, PixelFilter, PixelRect, Precision,
    RenderSettings, Renderer, Scene, SphereShape, Vec3A, WorldBuilder,
};

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

/// A sample stage's renderer, its settings changed by `f`.
fn load(name: &str, f: impl FnOnce(RenderSettings) -> RenderSettings) -> Renderer {
    let scene = Scene::from_usd(&sample(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    let settings = f(scene.settings);
    Renderer::new(scene.camera, scene.world, scene.lights, settings).with_volumes(scene.volumes)
}

/// `spp` samples in every pixel, without the clamp.
fn fixed(spp: u32) -> impl FnOnce(RenderSettings) -> RenderSettings {
    move |s| {
        s.with_samples_per_pixel(spp)
            .with_adaptive_sampling(spp, 0.0)
            .with_indirect_clamp(0.0)
    }
}

/// The 10×10 pixels at the frame's centre.
fn centre(r: &Renderer) -> PixelRect {
    let (w, h) = r.settings.get_dimensions();
    PixelRect::new(w / 2 - 5, h / 2 - 5, w / 2 + 5, h / 2 + 5)
}

fn inspect(r: &mut Renderer, region: PixelRect, group_by: GroupBy, top: usize) -> Report {
    inspect::run(
        r,
        region,
        &Options {
            group_by,
            top,
            ..Options::default()
        },
    )
    .unwrap_or_else(|e| panic!("{region}: {e}"))
}

/// A floor lit by a small sphere light, beside a mirror ball: a diffuse
/// bounce off the floor finds the light in the ball only rarely, and
/// brightly — a caustic NEE cannot sample.
fn caustic(spp: u32) -> Renderer {
    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.0, -101.0, 0.0),
            radius: 100.0,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.6))),
    );
    world.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.8, -0.4, 0.0),
            radius: 0.6,
        },
        Arc::new(OpenPBR::metal(Vec3A::splat(0.95), 0.0)),
    );
    let (center, radius) = (Vec3A::new(-0.5, 2.5, 0.0), 0.2);
    let emitter = Arc::new(Emissive::new(Vec3A::splat(100.0)));
    let id = world.attach_masked(
        Geometry::Sphere { center, radius },
        emitter.clone(),
        MASK_SHADOW | MASK_INDIRECT,
    );
    lights.add(AreaLight::new(SphereShape { center, radius }, emitter, id));
    lights.set_name(0, "/lights/bulb");
    let camera = Camera::new(
        Vec3A::new(0.0, 2.0, 6.0),
        Vec3A::new(0.0, -0.5, 0.0),
        Vec3A::Y,
        40.0,
        4.0 / 3.0,
        0.0,
        6.0,
    );
    let settings = fixed(spp)(RenderSettings::default().with_resolution(64, 48));
    Renderer::new(camera, world.commit(), lights, settings)
}

/// Where the caustic lands on the floor.
const CAUSTIC: PixelRect = PixelRect::new(42, 24, 52, 34);

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Every sample's records sum to its radiance, and every pixel's weighted
/// records to the pixel: on a box of diffuse walls, glossy plates under
/// lights of four sizes, a volume, and thin-walled glass a bounce passes
/// straight through.
#[test]
fn records_rebuild_every_sample_and_pixel() {
    let cases: [(&str, Option<PixelRect>, u32, &str); 4] = [
        ("cornellbox.usda", None, 16, "C<RD'diffuse'>"),
        ("veach_mis.usda", None, 16, "<RG'specular'>"),
        ("fog.usda", None, 16, "CV"),
        (
            "thin_window.usda",
            Some(PixelRect::new(100, 130, 110, 140)),
            256,
            "<TS'transmission'>",
        ),
    ];
    for (name, region, spp, event) in cases {
        let mut r = load(name, fixed(spp));
        let region = region.unwrap_or_else(|| centre(&r));
        let report = inspect(&mut r, region, GroupBy::Expr, 32);
        let c = &report.consistency;
        assert!(
            c.max_sample_relative_error <= 1e-5 && c.max_relative_error <= 1e-5,
            "{name}: {c:?}"
        );
        assert!(report.truncated.of > 0, "{name}: no record");
        assert!(
            report
                .groups
                .iter()
                .any(|g| g.expression.as_deref().is_some_and(|e| e.contains(event))),
            "{name}: no path through {event}"
        );
    }
}

/// With the clamp on, the records' clamped contributions rebuild the
/// clamped beauty; the clamp leaves the primary vertex's records whole, and
/// the caustic's records are cut and say so.
#[test]
fn clamped_records_sum_to_the_clamped_beauty() {
    let mut r = caustic(256);
    r.settings = r.settings.with_indirect_clamp(10.0);
    let report = inspect(&mut r, CAUSTIC, GroupBy::ExprLight, 64);
    let c = &report.consistency;
    assert!(
        c.max_sample_relative_error <= 1e-5 && c.max_relative_error <= 1e-5,
        "{c:?}"
    );
    assert!(report.clamp_removed_share > 0.0, "{report:#?}");
    let top = &report.records[0];
    assert_eq!(top.expression, "C<RD'diffuse'><RS'specular'>L");
    assert!(
        luma(top.contribution) > luma(top.contribution_clamped),
        "{top:?}"
    );
    for record in &report.records {
        if record.depth <= 1 {
            assert_eq!(
                record.contribution, record.contribution_clamped,
                "{record:?}"
            );
        }
    }
    assert!(
        report.hints.iter().any(|h| h.code == "clamp_hides_path"),
        "{:?}",
        report.hints
    );
}

/// A sample's film coordinates are where the filter placed it: inside its
/// pixel under a box filter, within the radius of its centre otherwise.
#[test]
fn coordinates_are_where_the_sample_was_taken() {
    for (filter, reach) in [
        (PixelFilter::BoxFilter { radius: 0.5 }, 0.5f32),
        (PixelFilter::Gaussian { radius: 1.5 }, 1.5),
    ] {
        let mut r = load("cornellbox.usda", |s| fixed(8)(s.with_pixel_filter(filter)));
        let region = centre(&r);
        let report = inspect(&mut r, region, GroupBy::None, 100_000);
        assert_eq!(report.truncated.kept, report.truncated.of);
        for rec in &report.records {
            let [x, y] = rec.pixel.map(|p| p as f32 + 0.5);
            let [cx, cy] = rec.coordinates;
            assert!(
                (cx - x).abs() <= reach && (cy - y).abs() <= reach,
                "{filter:?}: {:?} at {:?}",
                rec.pixel,
                rec.coordinates
            );
        }
    }
}

/// The units' records merge in unit order, so the report is the same bytes
/// with one thread or eight — and run after run.
#[test]
fn the_report_is_the_same_bytes_with_any_thread_count() {
    let json = |threads: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("a pool");
        pool.install(|| {
            let mut r = load("cornellbox.usda", fixed(16));
            // Across the 16-pixel tile grid: four units.
            inspect(
                &mut r,
                PixelRect::new(11, 11, 21, 21),
                GroupBy::ExprLight,
                16,
            )
            .to_json()
        })
    };
    let one = json(1);
    assert_eq!(one, json(8));
    assert_eq!(one, json(8));
    assert!(
        one.starts_with("{\n  \"format\": \"crust-light-paths/1\""),
        "{one}"
    );
}

/// Every grouping partitions the region: the luminance shares and the
/// second-moment shares each sum to one, the clamped shares to what the
/// clamp left.
#[test]
fn shares_partition_the_region() {
    for group_by in [GroupBy::Light, GroupBy::Expr, GroupBy::ExprLight] {
        let mut r = load("cornellbox.usda", |s| {
            s.with_samples_per_pixel(16)
                .with_adaptive_sampling(16, 0.0)
                .with_indirect_clamp(1.0)
        });
        let region = centre(&r);
        let report = inspect(&mut r, region, group_by, 8);
        let sum = |f: fn(&inspect::Group) -> f64| report.groups.iter().map(f).sum::<f64>();
        assert!((sum(|g| g.luma_share) - 1.0).abs() < 1e-6, "{group_by}");
        assert!(
            (sum(|g| g.second_moment_share) - 1.0).abs() < 1e-6,
            "{group_by}"
        );
        assert!(
            (sum(|g| g.luma_share_clamped) - (1.0 - report.clamp_removed_share)).abs() < 1e-6,
            "{group_by}"
        );
        assert!(
            report
                .groups
                .iter()
                .all(|g| (0.0..=1.0).contains(&g.hit_rate))
        );
        assert_eq!(
            report.groups.iter().map(|g| g.records).sum::<u64>(),
            report.truncated.of
        );
        if group_by == GroupBy::Light {
            assert!(report.groups.iter().all(|g| g.expression.is_none()));
        }
    }
    let mut r = load("cornellbox.usda", fixed(4));
    let region = centre(&r);
    let report = inspect(&mut r, region, GroupBy::None, 8);
    assert!(report.groups.is_empty());
    assert!(report.records.iter().all(|r| r.group.is_none()));
    assert!(
        report.summary.starts_with("The brightest record is"),
        "{}",
        report.summary
    );
}

/// A caustic one sample in a few thousand takes holds nearly all of the
/// region's second moment with a sliver of its luminance: it ranks first,
/// its records lead the list, and the report says it is rare and bright.
#[test]
fn a_rare_caustic_ranks_first_by_its_second_moment() {
    let mut r = caustic(256);
    let report = inspect(&mut r, CAUSTIC, GroupBy::ExprLight, 32);
    let first = &report.groups[0];
    assert_eq!(
        first.expression.as_deref(),
        Some("C<RD'diffuse'><RS'specular'>L")
    );
    assert_eq!(first.light.as_deref(), Some("/lights/bulb"));
    assert!(first.hit_rate < 1e-2, "{first:?}");
    assert!(first.second_moment_share > 0.5, "{first:?}");
    assert!(first.luma_share < first.second_moment_share, "{first:?}");
    assert_eq!(report.records[0].group, Some(0));
    assert_eq!(first.top_records.first(), Some(&0));
    let rare = report
        .hints
        .iter()
        .find(|h| h.code == "rare_bright_path")
        .expect("a rare bright path");
    assert_eq!(rare.group, Some(0));
    assert!(
        report
            .summary
            .contains("C<RD'diffuse'><RS'specular'>L via /lights/bulb")
    );
}

/// The inspected region is the frame's pixels, bit for bit, at a fixed
/// sample count; under guiding, or adaptive sampling's neighbour hold, the
/// report says its samples are not the frame's, and why.
#[test]
fn the_region_is_the_frame_bit_for_bit() {
    for name in ["cornellbox.usda", "veach_mis.usda"] {
        // A small frame (debug builds trace slowly); bit-identity holds at
        // any resolution.
        let mut r = load(name, |s| {
            s.with_resolution(160, 90).with_samples_per_pixel(16)
        });
        let frame = r.render();
        let (_, h) = r.settings.get_dimensions();
        let region = centre(&r);
        let report = inspect(&mut r, region, GroupBy::ExprLight, 4);
        assert!(report.reproduces_frame, "{name}");
        assert_eq!(report.pixels.len(), region.area());
        for p in &report.pixels {
            let [x, y] = p.pixel;
            let want = frame.get_pixel(x, h - 1 - y).to_array().map(f32::to_bits);
            assert_eq!(p.beauty.map(f32::to_bits), want, "{name} at {x},{y}");
        }
        // The renderer is left as it was: the frame renders again.
        assert!(r.settings.is_full_frame());
    }
    let mut guided = load("cornellbox_guided.usda", |s| {
        s.with_resolution(160, 90).with_samples_per_pixel(4)
    });
    let region = centre(&guided);
    let report = inspect(&mut guided, region, GroupBy::ExprLight, 4);
    assert!(!report.reproduces_frame);
    assert!(report.reproduces_frame_reason.unwrap().contains("guiding"));
    let mut adaptive = load("cornellbox.usda", |s| {
        s.with_resolution(160, 90).with_samples_per_pixel(64)
    });
    let region = centre(&adaptive);
    let report = inspect(&mut adaptive, region, GroupBy::ExprLight, 4);
    assert!(!report.reproduces_frame);
    assert!(report.reproduces_frame_reason.unwrap().contains("adaptive"));
}

fn lpe_var(expr: &str) -> AovVar {
    AovVar {
        prim_path: "/Render/Vars/path".into(),
        name: "path".into(),
        channel_prefix: None,
        source: AovSource::Lpe,
        components: 3,
        precision: Precision::Float,
        accumulation: Accumulation::Filtered,
        clear: 0.0,
        expression: Some(expr.to_owned()),
        raw: false,
        variance: false,
    }
}

/// Every record's expression is one the LPE grammar compiles, and authored
/// as an `lpe` var over the region at the same sample count, it is non-zero
/// at the record's pixel: it routes the path it names.
#[test]
fn every_expression_routes_its_path() {
    let mut r = load("veach_mis.usda", fixed(16));
    let region = centre(&r);
    let report = inspect(&mut r, region, GroupBy::ExprLight, 64);
    for rec in &report.records {
        crust_core::lpe::Lpe::compile(&[rec.expression.as_str()])
            .unwrap_or_else(|e| panic!("{}: {e}", rec.expression));
    }
    r.settings = r.settings.with_region(region).expect("inside");
    for rec in report.records.iter().take(8) {
        let var = lpe_var(&rec.expression);
        let request = AovRequest {
            products: vec![AovProduct {
                prim_path: "/Render/p".into(),
                name: "p.exr".into(),
                vars: vec![var.clone()],
                attributes: Vec::new(),
            }],
        };
        let (beauty, film, _) = r.render_with_aovs(true, &|_, _| {}, &request);
        let planes = film.var_channels(&beauty, &var);
        // Top-down over the region.
        let at = (rec.pixel[1] - region.y0) * region.width() + (rec.pixel[0] - region.x0);
        assert!(
            planes.iter().any(|p| p[at] != 0.0),
            "{} is black at {:?}",
            rec.expression,
            rec.pixel
        );
    }
}

/// A light is named by its prim path, and its tag labels the expression's
/// `L`; an untagged light ends in a plain `L`; emission that is no light's
/// ends in `O` with no light.
#[test]
fn lights_are_named_by_path_and_tag() {
    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();
    world.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.0, -101.0, 0.0),
            radius: 100.0,
        },
        Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))),
    );
    world.attach(
        Geometry::Sphere {
            center: Vec3A::new(0.0, -0.6, 0.0),
            radius: 0.4,
        },
        Arc::new(Emissive::new(Vec3A::new(2.0, 1.0, 0.5))),
    );
    for (center, name, tag) in [
        (Vec3A::new(-1.5, 2.0, 0.5), "/lights/key", Some("key")),
        (Vec3A::new(1.5, 2.0, 0.5), "/lights/fill", None),
    ] {
        let emitter = Arc::new(Emissive::new(Vec3A::splat(10.0)));
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
        let i = lights.count() - 1;
        lights.set_name(i, name);
        lights.set_lpe_tag(i, tag);
    }
    let camera = Camera::new(
        Vec3A::new(0.0, 0.5, 4.0),
        Vec3A::new(0.0, -0.6, 0.0),
        Vec3A::Y,
        40.0,
        1.0,
        0.0,
        4.0,
    );
    let settings = fixed(16)(RenderSettings::default().with_resolution(40, 40));
    let mut r = Renderer::new(camera, world.commit(), lights, settings);
    // Over the emissive ball and the floor beside it.
    let report = inspect(
        &mut r,
        PixelRect::new(15, 20, 25, 30),
        GroupBy::ExprLight,
        100_000,
    );
    let mut seen = [false; 3];
    for rec in &report.records {
        let e = &rec.expression;
        match rec.light.as_deref() {
            Some("/lights/key") => {
                assert!(e.ends_with("<L.'key'>"), "{e}");
                seen[0] = true;
            }
            Some("/lights/fill") => {
                assert!(e.ends_with('L') && !e.ends_with("'>"), "{e}");
                seen[1] = true;
            }
            None => {
                assert!(e.ends_with('O'), "{e}");
                seen[2] = true;
            }
            other => panic!("{other:?}"),
        }
        assert!(rec.prims.is_none());
    }
    assert_eq!(seen, [true; 3]);
    assert!(report.prims_note.is_some());
}

/// Each vertex is labelled by the event carrying most of its luminance
/// toward the path: each lobe split sums to one, the expression names each
/// split's largest event in order, and a record is ambiguous exactly when a
/// named event carries less than 0.6.
#[test]
fn expressions_name_the_dominant_lobe() {
    let mut r = load("veach_mis.usda", fixed(16));
    let region = centre(&r);
    let report = inspect(&mut r, region, GroupBy::Expr, 2_000);
    let mut ambiguous = 0;
    for rec in &report.records {
        assert_eq!(rec.lobe_split.len(), rec.depth);
        let mut rest = rec.expression.as_str();
        let mut smallest = 1.0f32;
        for split in &rec.lobe_split {
            let total: f32 = split.iter().map(|s| s.share).sum();
            assert!((total - 1.0).abs() < 1e-5, "{split:?}");
            let top = split
                .iter()
                .max_by(|a, b| a.share.total_cmp(&b.share))
                .expect("a lobe");
            smallest = smallest.min(top.share);
            let at = rest
                .find(top.event.as_str())
                .unwrap_or_else(|| panic!("{} lacks {}", rec.expression, top.event));
            rest = &rest[at + top.event.len()..];
        }
        assert_eq!(rec.label_ambiguous, smallest < 0.6, "{rec:?}");
        ambiguous += usize::from(rec.label_ambiguous);
    }
    assert!((0.0..=1.0).contains(&report.ambiguous_luma_share));
    if ambiguous == 0 {
        assert_eq!(report.ambiguous_luma_share, 0.0);
    }
}

/// What no inspection runs with is refused, before anything renders.
#[test]
fn oversized_and_prim_requests_are_refused() {
    let big = PixelRect::new(0, 0, 20, 20);
    let err = inspect::check_request(big, GroupBy::ExprLight).unwrap_err();
    assert!(err.contains("100"), "{err}");
    assert!(inspect::check_request(PixelRect::new(3, 3, 3, 9), GroupBy::Expr).is_err());
    let err =
        inspect::check_request(PixelRect::new(0, 0, 5, 5), GroupBy::ExprPrimsLight).unwrap_err();
    assert!(err.contains("prim"), "{err}");
    assert!(inspect::check_request(PixelRect::new(0, 0, 1, 100), GroupBy::None).is_ok());
    let mut r = caustic(1);
    let outside = PixelRect::new(100, 100, 105, 105);
    assert!(inspect::run(&mut r, outside, &Options::default()).is_err());
}
