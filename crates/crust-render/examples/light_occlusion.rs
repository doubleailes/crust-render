//! Why next-event estimation fails: per light, what happens to its samples.
//!
//! `--stats` reports how many shadow rays were occluded, which says NEE is
//! failing but not why, and the fix depends on why. A light seen from behind
//! wants orientation-aware light selection. A receiver in a cavity is simply
//! dark. A light enclosed by its own fixture needs the fixture handled, and if
//! that fixture is glass, shadow rays have to pass through it.
//!
//! So this renders nothing. It casts a grid of camera rays, and at each first
//! hit it samples **every** light, not just the one NEE would pick. Each
//! sample runs through the integrator's three tests in its order: the light's
//! radiance, the BSDF, then the shadow ray. For an occluded shadow ray it walks
//! every surface between the point and the light, and records how many there
//! are, whether they are all transmissive, and how close the last one sits to
//! the light.
//!
//! ```sh
//! cargo run --release -p crust-render --example light_occlusion -- \
//!     scene.usda [--frame F] [--camera /path] [--grid 160x90] [--samples 4]
//! ```

use crust_assets::FileAssets;
use crust_core::{MASK_SHADOW, Material, Ray, ShadingPoint, UsdImportOptions, Vec3A, World};
use std::path::PathBuf;

/// How one light sample ended, in the integrator's test order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// `sample_li` refused: below a dome's horizon, a degenerate point.
    Unreachable,
    /// Zero radiance toward the point: a one-sided light seen from behind, or
    /// outside a shaping cone.
    Backfacing,
    /// The BSDF carries nothing toward the light: it is below the horizon.
    BelowHorizon,
    /// Visible.
    Visible,
    /// Occluded, and every blocker is transmissive.
    GlassOnly,
    /// Occluded by at least one opaque surface.
    Opaque,
}

#[derive(Default, Clone)]
struct PerLight {
    samples: u64,
    unreachable: u64,
    backfacing: u64,
    below: u64,
    visible: u64,
    glass_only: u64,
    opaque: u64,
    /// Occluded samples whose last blocker lies within `ENCLOSED` of the light
    /// (relative to the shadow ray's length): the light's own fixture.
    enclosed: u64,
    /// Occluded samples whose first blocker lies within `ENCLOSED` of the
    /// receiver: the receiver's own cavity.
    local: u64,
    crossings: u64,
}

/// "Near", as a fraction of the shadow ray's length.
const ENCLOSED: f32 = 0.1;
/// Surfaces walked along one occluded shadow ray before giving up.
const MAX_CROSSINGS: usize = 32;

/// Whether the surface at a hit transmits light: what a shadow ray through it
/// would have to account for. Resolves the material at the hit, since ALab's
/// glass is a textured `UsdPreviewSurface` whose `opacity` drives
/// `transmission_weight`.
fn transmissive(mat: &dyn Material, ray: &Ray, hit: &crust_core::HitRecord) -> bool {
    let cos = ray.direction().normalize().dot(hit.normal).abs();
    match mat.resolve(ray, hit, cos) {
        Some(r) => r.bsdf.transmission_weight > 0.5,
        // An untextured material reports nothing; treat as opaque. On ALab
        // every shaded material is a textured PreviewSurface.
        None => false,
    }
}

/// Walks every surface between `from` and `dist` along `dir`, returning
/// (crossings, all transmissive, first t, last t).
fn walk(world: &World, from: Vec3A, dir: Vec3A, dist: f32) -> (usize, bool, f32, f32) {
    let ray = Ray::new(from, dir).with_mask(MASK_SHADOW);
    let (mut t0, mut n, mut glass) = (0.001f32, 0, true);
    let (mut first, mut last) = (f32::NAN, f32::NAN);
    while n < MAX_CROSSINGS {
        let Some(h) = world.intersect(&ray, t0, dist - 0.001) else {
            break;
        };
        if n == 0 {
            first = h.rec.t;
        }
        last = h.rec.t;
        glass &= transmissive(h.mat, &ray, &h.rec);
        n += 1;
        // Step past the hit by a relative epsilon, so a far wall is not hit
        // again by rounding.
        t0 = h.rec.t + 1e-4 * (1.0 + h.rec.t);
    }
    (n, glass, first, last)
}

/// A small counter-based hash, so the probe needs no RNG dependency.
fn hash01(mut x: u64) -> f32 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!(
            "usage: light_occlusion <scene.usd[a]> [--frame F] [--camera /path] \
             [--grid WxH] [--samples N]"
        );
        std::process::exit(2);
    };
    let (mut frame, mut camera, mut grid, mut per) = (None, None, (160usize, 90usize), 4u64);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        match flag.as_str() {
            "--frame" => frame = value.parse::<f64>().ok(),
            "--camera" => camera = Some(value),
            "--grid" => {
                let (w, h) = value.split_once('x').expect("--grid WxH");
                grid = (w.parse().expect("width"), h.parse().expect("height"));
            }
            "--samples" => per = value.parse().expect("--samples N"),
            other => {
                eprintln!("unknown flag {other}");
                std::process::exit(2);
            }
        }
    }

    let assets = FileAssets::new();
    let options = UsdImportOptions {
        frame,
        camera,
        skip_stage_teardown: true,
    };
    let scene = match crust_core::Scene::from_usd_with_options(&path, &assets, &options) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let mut lights = scene.lights;
    lights.select_by(scene.settings.light_selection());
    let world = &scene.world;
    let list: Vec<_> = lights.iter().map(|(l, pmf)| (l.clone(), pmf)).collect();
    let mut stats = vec![PerLight::default(); list.len()];
    let mut receivers = 0u64;

    let (gw, gh) = grid;
    for j in 0..gh {
        for i in 0..gw {
            let (u, v) = ((i as f32 + 0.5) / gw as f32, (j as f32 + 0.5) / gh as f32);
            let ray = scene.camera.get_ray(u, v, [0.5, 0.5], 0.0);
            let Some(hit) = world.intersect(&ray, 0.001, f32::INFINITY) else {
                continue;
            };
            receivers += 1;
            let cos = ray.direction().normalize().dot(hit.rec.normal).abs();
            let sp = ShadingPoint::new(hit.mat, &ray, &hit.rec, cos);
            let p = hit.rec.p;
            for (k, (light, _)) in list.iter().enumerate() {
                let s = &mut stats[k];
                for n in 0..per {
                    let seed = ((j * gw + i) as u64) << 24 | (k as u64) << 8 | n;
                    let (a, b) = (hash01(seed * 2 + 1), hash01(seed * 2 + 2));
                    s.samples += 1;
                    let outcome = match light.sample_li(p, a, b) {
                        None => Outcome::Unreachable,
                        Some(ls) if ls.radiance == Vec3A::ZERO => Outcome::Backfacing,
                        Some(ls) => {
                            let carries = sp
                                .eval(&ray, ls.direction)
                                .is_some_and(|(f, _)| ls.radiance * f != Vec3A::ZERO);
                            let shadow = Ray::new(p, ls.direction).with_mask(MASK_SHADOW);
                            if !carries {
                                Outcome::BelowHorizon
                            } else if !world.occluded(&shadow, 0.001, ls.distance - 0.001) {
                                Outcome::Visible
                            } else {
                                let d = if ls.distance.is_finite() {
                                    ls.distance
                                } else {
                                    1e7
                                };
                                let (n, glass, first, last) = walk(world, p, ls.direction, d);
                                s.crossings += n as u64;
                                if last > (1.0 - ENCLOSED) * d {
                                    s.enclosed += 1;
                                }
                                if first < ENCLOSED * d {
                                    s.local += 1;
                                }
                                if glass {
                                    Outcome::GlassOnly
                                } else {
                                    Outcome::Opaque
                                }
                            }
                        }
                    };
                    match outcome {
                        Outcome::Unreachable => s.unreachable += 1,
                        Outcome::Backfacing => s.backfacing += 1,
                        Outcome::BelowHorizon => s.below += 1,
                        Outcome::Visible => s.visible += 1,
                        Outcome::GlassOnly => s.glass_only += 1,
                        Outcome::Opaque => s.opaque += 1,
                    }
                }
            }
        }
    }

    let pct = |n: u64, d: u64| 100.0 * n as f64 / d.max(1) as f64;
    println!(
        "{receivers} receivers ({gw}x{gh} grid), {per} samples per light per receiver\n\
         outcomes as % of that light's samples; 'encl' / 'local' as % of its occluded samples\n"
    );
    println!(
        "{:>3} {:<16} {:>6} | {:>6} {:>6} {:>6} {:>6} | {:>6} {:>6} | {:>6} {:>6} {:>5}",
        "#",
        "kind",
        "pmf",
        "unrch",
        "back",
        "horiz",
        "VIS",
        "glass",
        "opaque",
        "encl",
        "local",
        "cross"
    );
    // Expected NEE outcome: each light weighted by how often NEE picks it.
    let mut expect = [0.0f64; 6];
    for (k, (light, pmf)) in list.iter().enumerate() {
        let s = &stats[k];
        let occ = s.glass_only + s.opaque;
        println!(
            "{:>3} {:<16} {:>6.3} | {:>6.1} {:>6.1} {:>6.1} {:>6.1} | {:>6.1} {:>6.1} | {:>6.1} {:>6.1} {:>5.1}",
            k,
            light.kind(),
            pmf,
            pct(s.unreachable, s.samples),
            pct(s.backfacing, s.samples),
            pct(s.below, s.samples),
            pct(s.visible, s.samples),
            pct(s.glass_only, s.samples),
            pct(s.opaque, s.samples),
            pct(s.enclosed, occ),
            pct(s.local, occ),
            s.crossings as f64 / occ.max(1) as f64,
        );
        let f = |n: u64| *pmf as f64 * n as f64 / s.samples.max(1) as f64;
        for (e, n) in expect.iter_mut().zip([
            s.unreachable,
            s.backfacing,
            s.below,
            s.visible,
            s.glass_only,
            s.opaque,
        ]) {
            *e += f(n);
        }
    }
    println!(
        "\nNEE, weighted by pick probability: unreachable {:.1}%  backfacing {:.1}%  \
         below horizon {:.1}%  VISIBLE {:.1}%  glass-only {:.1}%  opaque {:.1}%",
        100.0 * expect[0],
        100.0 * expect[1],
        100.0 * expect[2],
        100.0 * expect[3],
        100.0 * expect[4],
        100.0 * expect[5],
    );
}
