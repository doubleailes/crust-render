//! The go/no-go gate on `neural` light selection's cost (design D9 of the
//! `neural-visibility-light-selection` change): one network evaluation
//! against one shadow ray, and against one `learned` lookup, on the same
//! receivers.
//!
//! `neural` buys fewer wasted shadow rays with one inference per NEE vertex,
//! so the inference has to cost no more than a shadow ray or two to pay for
//! itself at equal time. This trains the network as a render does (through a
//! `Renderer`), casts a grid of camera rays, and at each first hit times, one
//! call at a time on one thread:
//!
//! - **neural**: `LightList::pick_index_at` under `neural`, at a new position
//!   each call, so the thread's memo always misses: a full `(pmf, cdf)`
//!   evaluation, which is what NEE pays at every vertex;
//! - **shadow**: one shadow ray toward a light sample from that receiver;
//! - **shadow after neural**: the two interleaved, less the neural time — a
//!   shadow ray made slower by the network's tables crowding its BVH out of
//!   the cache shows here;
//! - **learned**: `pick_index_at` under `learned`;
//! - **power**: `pick_index_at` under `power`, today's default.
//!
//! Each is the min (and mean) over `--repeat` runs, in nanoseconds per call.
//! For instruction counts, count the whole program under callgrind with
//! `--only <phase>` at two `--repeat`s: every other phase, the import and
//! both pre-passes run once either way, so the difference over the extra
//! runs and calls is the phase's instructions per call
//! (`openspec/specs/cli/design.md`, command cookbook):
//!
//! ```sh
//! cargo run --release -p crust-render --example nvc_bench -- \
//!     scene.usda [--frame F] [--camera /path] [--grid 160x90] [--repeat 7] \
//!     [--only power|learned|neural|shadow]
//! ```

use crust_assets::FileAssets;
use crust_core::{
    Light, LightList, LightSelection, MASK_SHADOW, Ray, Renderer, TRACE_T_MIN, UsdImportOptions,
    Vec3A, World,
};
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A small counter-based hash, so the probe needs no RNG dependency.
fn hash01(mut x: u64) -> f32 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// One receiver: where NEE picks from, its pick coordinate, and the shadow
/// ray toward one light sample from it (`None` when that sample carries
/// nothing and the integrator would cast no ray).
struct Receiver {
    p: Vec3A,
    u: f32,
    shadow: Option<(Ray, f32)>,
}

/// Picks a light at every receiver.
#[inline(always)]
fn picks(lights: &LightList, receivers: &[Receiver]) -> usize {
    let mut sum = 0usize;
    for r in receivers {
        if let Some((index, _)) = lights.pick_index_at(black_box(r.p), r.u) {
            sum += index;
        }
    }
    sum
}

// One out-of-line function per selection, so callgrind can collect each
// alone (`--toggle-collect=*phase_neural*`). The constants keep LLVM from
// merging the three identical bodies into one symbol.

#[inline(never)]
fn phase_power(lights: &LightList, receivers: &[Receiver]) -> usize {
    picks(lights, receivers) ^ 1
}

#[inline(never)]
fn phase_learned(lights: &LightList, receivers: &[Receiver]) -> usize {
    picks(lights, receivers) ^ 2
}

#[inline(never)]
fn phase_neural(lights: &LightList, receivers: &[Receiver]) -> usize {
    picks(lights, receivers) ^ 3
}

/// Casts every receiver's shadow ray.
#[inline(never)]
fn phase_shadow(world: &World, receivers: &[Receiver]) -> usize {
    let mut blocked = 0usize;
    for r in receivers {
        if let Some((ray, t)) = &r.shadow {
            blocked += world.occluded(black_box(ray), TRACE_T_MIN, *t) as usize;
        }
    }
    blocked
}

/// A pick and a shadow ray per receiver, interleaved as NEE runs them.
#[inline(never)]
fn phase_pick_then_shadow(world: &World, lights: &LightList, receivers: &[Receiver]) -> usize {
    let mut sum = 0usize;
    for r in receivers {
        if let Some((index, _)) = lights.pick_index_at(black_box(r.p), r.u) {
            sum += index;
        }
        if let Some((ray, t)) = &r.shadow {
            sum += world.occluded(black_box(ray), TRACE_T_MIN, *t) as usize;
        }
    }
    sum
}

/// Min and mean of `repeat` runs of `f`, per call over `calls` calls, in ns.
fn time(repeat: usize, calls: usize, mut f: impl FnMut() -> usize) -> (f64, f64) {
    let mut runs = Vec::with_capacity(repeat);
    for _ in 0..repeat {
        let start = Instant::now();
        black_box(f());
        runs.push(start.elapsed());
    }
    let per = |d: Duration| d.as_secs_f64() * 1e9 / calls.max(1) as f64;
    let min = runs.iter().copied().min().unwrap_or_default();
    let mean = runs.iter().sum::<Duration>() / runs.len().max(1) as u32;
    (per(min), per(mean))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!(
            "usage: nvc_bench <scene.usd[a]> [--frame F] [--camera /path] [--grid WxH] [--repeat N] \
             [--only power|learned|neural|shadow]"
        );
        std::process::exit(2);
    };
    let (mut frame, mut camera, mut grid, mut repeat) = (None, None, (160usize, 90usize), 7usize);
    let mut only: Option<String> = None;
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        match flag.as_str() {
            "--frame" => match value.parse::<f64>() {
                Ok(f) if f.is_finite() => frame = Some(f),
                _ => {
                    eprintln!("--frame {value:?} is not a finite number");
                    std::process::exit(2);
                }
            },
            "--camera" => camera = Some(value),
            "--grid" => {
                let (w, h) = value.split_once('x').expect("--grid WxH");
                grid = (w.parse().expect("width"), h.parse().expect("height"));
            }
            "--repeat" => repeat = value.parse::<usize>().expect("--repeat N").max(1),
            "--only" => match value.as_str() {
                "power" | "learned" | "neural" | "shadow" => only = Some(value),
                _ => {
                    eprintln!("--only {value:?}: one of power, learned, neural, shadow");
                    std::process::exit(2);
                }
            },
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
    // Through a `Renderer`, so each selection is built exactly as a render
    // builds it; `reconfigure` switches it on the same world.
    let base = scene.settings;
    let mut r = Renderer::new(
        scene.camera,
        scene.world,
        scene.lights,
        base.with_light_selection(LightSelection::Power),
    );

    // The receivers: first hits of a grid of camera rays, each with one light
    // sample's shadow ray (light and point on it from a hash).
    let (gw, gh) = grid;
    let mut receivers = Vec::new();
    {
        let (world, lights) = (r.world(), r.lights());
        let n = lights.count().max(1);
        for j in 0..gh {
            for i in 0..gw {
                let (u, v) = ((i as f32 + 0.5) / gw as f32, (j as f32 + 0.5) / gh as f32);
                let ray = r.camera().get_ray(u, v, [0.5, 0.5], 0.0);
                let Some(hit) = world.intersect(&ray, TRACE_T_MIN, f32::INFINITY) else {
                    continue;
                };
                let p = hit.rec.p;
                let seed = (j * gw + i) as u64;
                let k = ((hash01(seed * 3 + 1) * n as f32) as usize).min(n - 1);
                let shadow = lights.lights().get(k).and_then(|light| {
                    let s = light.sample_li(p, hash01(seed * 3 + 2), hash01(seed * 3 + 3))?;
                    (s.radiance != Vec3A::ZERO).then(|| {
                        (
                            Ray::new(p, s.direction).with_mask(MASK_SHADOW),
                            (s.distance - TRACE_T_MIN).min(f32::MAX),
                        )
                    })
                });
                receivers.push(Receiver {
                    p,
                    u: hash01(seed * 3 + 4),
                    shadow,
                });
            }
        }
    }
    let rays = receivers.iter().filter(|r| r.shadow.is_some()).count();
    let calls = receivers.len();
    println!(
        "{}: {} lights, {calls} receivers ({gw}x{gh} grid), {rays} shadow rays, \
         {repeat} runs, one thread, ns per call (min / mean)\n",
        path.display(),
        r.lights().count(),
    );
    if calls == 0 {
        eprintln!("no receivers: the camera sees nothing");
        std::process::exit(1);
    }

    // Whether to time `phase`: all of them, or the one `--only` names (each
    // still runs once, so the setup a callgrind count subtracts is the same).
    let timed = |phase: &str| {
        if only.as_deref().is_none_or(|o| o == phase) {
            repeat
        } else {
            1
        }
    };
    let power = time(timed("power"), calls, || {
        phase_power(r.lights(), &receivers)
    });
    let shadow = time(timed("shadow"), rays, || {
        phase_shadow(r.world(), &receivers)
    });

    let setup = r.reconfigure(base.with_light_selection(LightSelection::Learned));
    let learned_on = r.lights().selection() == LightSelection::Learned;
    let learned = time(timed("learned"), calls, || {
        phase_learned(r.lights(), &receivers)
    });
    let learned_setup = setup;

    let setup = r.reconfigure(base.with_light_selection(LightSelection::Neural));
    let neural_on = r.lights().selection() == LightSelection::Neural;
    if !neural_on {
        eprintln!("neural selection fell back to power (see the log): nothing to time");
        std::process::exit(1);
    }
    let neural = time(timed("neural"), calls, || {
        phase_neural(r.lights(), &receivers)
    });
    let both = time(timed("both"), calls, || {
        phase_pick_then_shadow(r.world(), r.lights(), &receivers)
    });
    let cache = r.lights().visibility_cache().expect("neural is installed");

    let row = |name: &str, (min, mean): (f64, f64)| {
        println!("  {name:<22} {min:>9.1} {mean:>9.1}");
    };
    row("power pick", power);
    if learned_on {
        row("learned pick", learned);
    } else {
        println!("  {:<22} {:>9}", "learned pick", "n/a (fell back to power)");
    }
    row("neural pick", neural);
    row("shadow ray", shadow);
    // Per receiver, so in the same unit as the picks: the interleaved loop
    // casts a ray at `rays` of the `calls` receivers.
    let after = (
        (both.0 - neural.0) * calls as f64 / rays.max(1) as f64,
        (both.1 - neural.1) * calls as f64 / rays.max(1) as f64,
    );
    row("shadow after neural", after);
    let ratio = neural.0 / shadow.0;
    println!(
        "\nneural / shadow ray: {ratio:.2} (min over min); learned pre-pass {:.2?}, \
         neural pre-pass {:.2?} ({} receivers, {} trained on, final loss {:.4})",
        learned_setup, setup, cache.receivers, cache.trained, cache.loss
    );
    // Design D9: go if an evaluation costs at most about two shadow rays,
    // stop if over about five.
    let verdict = if ratio <= 2.0 {
        "go (≤ 2 shadow rays)"
    } else if ratio <= 5.0 {
        "marginal (between 2 and 5 shadow rays)"
    } else {
        "no-go (> 5 shadow rays): shrink the network first"
    };
    println!("gate (design D9): {verdict}");
}
