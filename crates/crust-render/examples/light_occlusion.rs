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
//! Pick probabilities are the ones the render would use. The scene is
//! assembled into a `Renderer`, so `learned` selection trains its cache exactly
//! as a render does. The weighted totals use each receiver's own pmf, since
//! under `learned` it varies by position. `--light-selection` overrides the
//! scene's, to compare strategies on the same receivers.
//!
//! ```sh
//! cargo run --release -p crust-render --example light_occlusion -- \
//!     scene.usda [--frame F] [--camera /path] [--grid 160x90] [--samples 4] \
//!     [--light-selection power|uniform|learned]
//! ```

use crust_assets::FileAssets;
use crust_core::{
    Light, LightSelection, MASK_SHADOW, Material, Ray, Renderer, ShadingPoint, TRACE_T_MIN,
    UsdImportOptions, Vec3A, World,
};
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
    /// Occluded by at least one opaque surface, or by more surfaces than the
    /// walk follows (`MAX_CROSSINGS`), which cannot be shown to be glass.
    Opaque,
}

impl Outcome {
    const ALL: [Outcome; 6] = [
        Outcome::Unreachable,
        Outcome::Backfacing,
        Outcome::BelowHorizon,
        Outcome::Visible,
        Outcome::GlassOnly,
        Outcome::Opaque,
    ];
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
    /// Occluded samples toward a light at a finite distance: the denominator
    /// of `enclosed` and `local`, which a light at infinity has no length for.
    occluded_finite: u64,
    /// Of those, the ones whose last blocker lies within `ENCLOSED` of the light
    /// (relative to the shadow ray's length): the light's own fixture.
    enclosed: u64,
    /// Of those, the ones whose first blocker lies within `ENCLOSED` of the
    /// receiver: the receiver's own cavity.
    local: u64,
    crossings: u64,
    /// Occluded samples whose walk hit `MAX_CROSSINGS` before the light.
    truncated: u64,
    /// Summed pick probability over receivers, for the mean pmf column.
    pmf_sum: f64,
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
        Some(r) => r.openpbr().map_or_else(
            || r.closure_bsdf().is_some_and(|c| c.transmits()),
            |m| m.params().transmission_weight > 0.5,
        ),
        // An untextured material reports nothing; treat as opaque. On ALab
        // every shaded material is a textured PreviewSurface.
        None => false,
    }
}

/// Walks every surface between `from` and `dist` along `dir`, returning
/// (crossings, all transmissive, first t, last t, truncated). `truncated` means
/// the walk stopped at `MAX_CROSSINGS` with surfaces still ahead, so "all
/// transmissive" covers only the ones it saw.
fn walk(world: &World, from: Vec3A, dir: Vec3A, dist: f32) -> (usize, bool, f32, f32, bool) {
    let ray = Ray::new(from, dir).with_mask(MASK_SHADOW);
    let (mut t0, mut n, mut glass) = (TRACE_T_MIN, 0, true);
    let (mut first, mut last) = (f32::NAN, f32::NAN);
    while n < MAX_CROSSINGS {
        let Some(h) = world.intersect(&ray, t0, dist - TRACE_T_MIN) else {
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
    let truncated = n == MAX_CROSSINGS && world.intersect(&ray, t0, dist - TRACE_T_MIN).is_some();
    (n, glass, first, last, truncated)
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
             [--grid WxH] [--samples N] [--light-selection power|uniform|learned]"
        );
        std::process::exit(2);
    };
    let (mut frame, mut camera, mut grid, mut per) = (None, None, (160usize, 90usize), 4u64);
    let mut selection = None;
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        match flag.as_str() {
            "--frame" => match value.parse::<f64>() {
                // A typo must not silently probe the stage's default time.
                Ok(f) if f.is_finite() => frame = Some(f),
                _ => {
                    eprintln!("--frame {value:?} is not a finite number");
                    std::process::exit(2);
                }
            },
            "--light-selection" => {
                selection = Some(value.parse::<LightSelection>().unwrap_or_else(|e| {
                    eprintln!("--light-selection: {e}");
                    std::process::exit(2);
                }))
            }
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
        ..UsdImportOptions::default()
    };
    let scene = match crust_core::Scene::from_usd_with_options(&path, &assets, &options) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    // Through a `Renderer`, so the selection (a learned cache included) is
    // built exactly as a render builds it.
    let settings = match selection {
        Some(s) => scene.settings.with_light_selection(s),
        None => scene.settings,
    };
    let r = Renderer::new(scene.camera, scene.world, scene.lights, settings);
    let (world, lights) = (&r.world, &r.lights);
    let list = lights.lights();
    let mut stats = vec![PerLight::default(); list.len()];
    let mut receivers = 0u64;
    // Expected NEE outcome, as a sum of each sample's pick probability at its
    // own receiver; normalised once the receivers are counted.
    let mut expect = [0.0f64; 6];

    let (gw, gh) = grid;
    for j in 0..gh {
        for i in 0..gw {
            let (u, v) = ((i as f32 + 0.5) / gw as f32, (j as f32 + 0.5) / gh as f32);
            let ray = r.camera.get_ray(u, v, [0.5, 0.5], 0.0);
            let Some(hit) = world.intersect(&ray, TRACE_T_MIN, f32::INFINITY) else {
                continue;
            };
            receivers += 1;
            let cos = ray.direction().normalize().dot(hit.rec.normal).abs();
            let sp = ShadingPoint::new(hit.mat, &ray, &hit.rec, cos);
            let p = hit.rec.p;
            for (k, light) in list.iter().enumerate() {
                let s = &mut stats[k];
                let pmf = lights.pmf_at(p, k) as f64;
                s.pmf_sum += pmf;
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
                            } else if !world.occluded(
                                &shadow,
                                TRACE_T_MIN,
                                ls.distance - TRACE_T_MIN,
                            ) {
                                Outcome::Visible
                            } else {
                                // A light at infinity is walked to a far bound,
                                // but has no length to measure "near" against.
                                let finite = ls.distance.is_finite();
                                let d = if finite { ls.distance } else { 1e7 };
                                let (n, glass, first, last, truncated) =
                                    walk(world, p, ls.direction, d);
                                s.crossings += n as u64;
                                s.truncated += truncated as u64;
                                if finite {
                                    s.occluded_finite += 1;
                                    if last > (1.0 - ENCLOSED) * d {
                                        s.enclosed += 1;
                                    }
                                    if first < ENCLOSED * d {
                                        s.local += 1;
                                    }
                                }
                                if glass && !truncated {
                                    Outcome::GlassOnly
                                } else {
                                    Outcome::Opaque
                                }
                            }
                        }
                    };
                    let slot = Outcome::ALL.iter().position(|&o| o == outcome).unwrap();
                    expect[slot] += pmf / per as f64;
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
    // "n/a" rather than 0.0 where there is nothing to divide: no occluded
    // sample toward a finite light.
    let pct_or_na = |n: u64, d: u64| {
        if d == 0 {
            format!("{:>6}", "n/a")
        } else {
            format!("{:>6.1}", pct(n, d))
        }
    };
    println!(
        "{receivers} receivers ({gw}x{gh} grid), {per} samples per light per receiver, \
         selection {:?}\n\
         outcomes as % of that light's samples; 'encl' / 'local' as % of its occluded \
         samples toward a finite light; pmf is the mean over receivers\n",
        lights.selection()
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
    let mut truncated = 0u64;
    for (k, light) in list.iter().enumerate() {
        let s = &stats[k];
        let occ = s.glass_only + s.opaque;
        truncated += s.truncated;
        println!(
            "{:>3} {:<16} {:>6.3} | {:>6.1} {:>6.1} {:>6.1} {:>6.1} | {:>6.1} {:>6.1} | {} {} {:>5.1}",
            k,
            light.kind(),
            s.pmf_sum / receivers.max(1) as f64,
            pct(s.unreachable, s.samples),
            pct(s.backfacing, s.samples),
            pct(s.below, s.samples),
            pct(s.visible, s.samples),
            pct(s.glass_only, s.samples),
            pct(s.opaque, s.samples),
            pct_or_na(s.enclosed, s.occluded_finite),
            pct_or_na(s.local, s.occluded_finite),
            s.crossings as f64 / occ.max(1) as f64,
        );
    }
    let e: Vec<f64> = expect
        .iter()
        .map(|&x| 100.0 * x / receivers.max(1) as f64)
        .collect();
    println!(
        "\nNEE, weighted by pick probability: unreachable {:.1}%  backfacing {:.1}%  \
         below horizon {:.1}%  VISIBLE {:.1}%  glass-only {:.1}%  opaque {:.1}%",
        e[0], e[1], e[2], e[3], e[4], e[5],
    );
    if truncated > 0 {
        println!(
            "{truncated} occluded samples crossed more than {MAX_CROSSINGS} surfaces; \
             counted as opaque"
        );
    }
}
