//! Learned occlusion proxies on real prototypes: does a proxy answer shadow
//! rays faster than the exact kernel, and how often is it wrong?
//!
//! `ray_throughput --neural` asks this of synthetic fixtures. This asks it of
//! a USD stage: it imports the stage as a render does, finds the instanced
//! prototypes made only of triangles (directly or through static instances),
//! takes the largest, trains a proxy for each beside the committed prototype
//! and compares the two on shadow rays in the prototype's own space:
//!
//! - `surface`: from area-uniform points on the prototype's triangles, in
//!   uniform directions — the shadow ray of a shading point on it, and where
//!   a light leak at contact shows;
//! - `through`: from outside the prototype's box through a point inside it —
//!   a shadow ray from elsewhere crossing it.
//!
//! The columns are `ray_throughput --neural`'s (`crust-rt/fixtures/neural.rs`):
//! exact and proxied Mray/s (single-threaded, min of N, interleaved), FP and
//! FN percentages, training seconds, proxy bytes, and a τ sweep.
//!
//! ```sh
//! cargo run --release -p crust-render --features neural-bvh --example neural_probe -- \
//!     stage.usda [--top 3] [--min-tris 1000000] [--steps 2000] [--cut size|refined] \
//!     [--rays 262144] [--repeats 3]
//! ```

#[path = "../../crust-rt/fixtures/neural.rs"]
mod neural;

use crust_assets::FileAssets;
use crust_core::UsdImportOptions;
use crust_rt::{MASK_SHADOW, PROXY_DEFAULT_STEPS, ProxyCut, ProxyRequest, Ray, Scene};
use glam::Vec3A;
use openqmc::pcg::Rng;
use std::path::PathBuf;
use std::time::Instant;

/// Rays from a box twice the prototype's, through a point inside it.
fn through_rays(scene: &Scene, n: usize, seed: u32) -> Vec<Ray> {
    let b = scene.bounds().expect("a prototype has bounds");
    let (c, h) = (0.5 * (b.minimum + b.maximum), 0.5 * (b.maximum - b.minimum));
    let mut rng = Rng::new(seed);
    let mut cube = |scale: f32| {
        c + h * scale * (2.0 * Vec3A::new(rng.next_f32(), rng.next_f32(), rng.next_f32()) - 1.0)
    };
    (0..n)
        .map(|_| {
            let o = cube(2.0);
            let t = cube(1.0);
            Ray::new(o, (t - o).normalize_or(Vec3A::X)).with_mask(MASK_SHADOW)
        })
        .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!(
            "usage: neural_probe <stage.usd[a]> [--top N] [--min-tris M] [--steps S] \
             [--cut size|refined] [--rays R] [--repeats K]"
        );
        std::process::exit(2);
    };
    let (mut top, mut min_tris, mut n_rays, mut repeats) =
        (3usize, 1_000_000usize, 1usize << 18, 3);
    let mut req = ProxyRequest::shadow(1);
    req.steps = PROXY_DEFAULT_STEPS;
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        match flag.as_str() {
            "--top" => top = value.parse().expect("--top N"),
            "--min-tris" => min_tris = value.parse().expect("--min-tris M"),
            "--steps" => req.steps = value.parse().expect("--steps S"),
            "--rays" => n_rays = value.parse().expect("--rays R"),
            "--repeats" => repeats = value.parse().expect("--repeats K"),
            "--cut" => {
                req.cut = match value.as_str() {
                    "size" => ProxyCut::Size,
                    "refined" => ProxyCut::Refined,
                    v => panic!("--cut wants size|refined, got {v}"),
                }
            }
            other => {
                eprintln!("unknown flag {other}");
                std::process::exit(2);
            }
        }
    }

    let t = Instant::now();
    let assets = FileAssets::new();
    let options = UsdImportOptions {
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
    println!(
        "{}: imported in {:.1} s",
        path.display(),
        t.elapsed().as_secs_f64()
    );

    // Every distinct prototype, largest first; the eligible ones are those
    // with a triangle count (triangles only, no motion).
    let mut protos: Vec<(usize, std::sync::Arc<Scene>)> = scene
        .world
        .kernel()
        .instanced_scenes()
        .into_iter()
        .filter_map(|s| s.proxy_triangle_count().map(|n| (n, s)))
        .collect();
    protos.sort_by(|a, b| b.0.cmp(&a.0));
    println!(
        "{} eligible prototypes; largest: {:?}",
        protos.len(),
        protos.iter().take(8).map(|(n, _)| *n).collect::<Vec<_>>()
    );
    // What `CRUST_NEURAL_OCCLUSION` would train at this threshold.
    let above = protos.iter().filter(|(n, _)| *n >= min_tris).count();
    println!("{above} eligible prototypes hold at least {min_tris} triangles (--min-tris)");
    let chosen: Vec<_> = protos
        .into_iter()
        .filter(|(n, _)| *n >= min_tris)
        .take(top)
        .collect();
    if chosen.is_empty() {
        println!("no eligible prototype holds {min_tris} triangles (--min-tris)");
        return;
    }
    println!("proxy request: {req:?}");

    for (k, (tris, proto)) in chosen.iter().enumerate() {
        let name = format!("proto{k}");
        let f = proto.memory_footprint();
        println!(
            "{name:<12} {tris} triangles, {:.0} MiB kernel",
            f.total() as f64 / (1 << 20) as f64
        );
        let Some(proxy) = proto.train_occlusion_proxy(&req) else {
            println!("{name:<12} no proxy (not eligible)");
            continue;
        };
        neural::print_proxy(&name, proxy.report());
        let t_min = neural::SHADOW_T_MIN;
        let exact = |r: &Ray| proto.occluded(r, t_min, f32::INFINITY);
        let proxied = |r: &Ray| proto.occluded_by(&proxy, r, t_min, f32::INFINITY, 0.5);
        let at = |r: &Ray, tau: f32| proto.occluded_by(&proxy, r, t_min, f32::INFINITY, tau);
        let surface = neural::surface_rays(proto, n_rays, 0x5EED, 1 << 22);
        let through = through_rays(proto, n_rays, 0x7EED);
        for (label, rays) in [("surface", &surface), ("through", &through)] {
            neural::print_set(&name, label, &exact, &proxied, Some(&at), rays, repeats);
        }
    }
}
