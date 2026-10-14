//! The occlusion-proxy measurement shared by `ray_throughput --neural` and
//! crust-render's `neural_probe` (included by each with `#[path]`, so both
//! print the same columns from the same method).
//!
//! For one proxy and a set of shadow rays it reports exact and proxied
//! occlusion throughput (min of N, single-threaded, the two interleaved
//! within each repeat so machine load lands on both), the false-positive
//! (spurious shadow) and false-negative (light leak) rates against the exact
//! answer, and a sweep of the decision threshold τ.

#![allow(dead_code)] // each including target uses a different subset

use crust_rt::{MASK_SHADOW, Piece, ProxyReport, Ray, Scene};
use glam::Vec3A;
use openqmc::pcg::Rng;
use std::time::Instant;

/// Where the renderer starts a shadow ray from a shading point
/// (`crust_core::TRACE_T_MIN`).
pub const SHADOW_T_MIN: f32 = 0.001;

/// The thresholds every sweep reports.
pub const TAUS: [f32; 5] = [0.1, 0.3, 0.5, 0.7, 0.9];

/// Shadow rays from `n` area-uniform points on the scene's triangles, in
/// uniform directions, unbounded: the query a light leak at contact shows
/// in. Samples from at most `cap` triangles (every k-th past it). Empty for
/// a scene without triangles.
pub fn surface_rays(scene: &Scene, n: usize, seed: u32, cap: usize) -> Vec<Ray> {
    let total = scene.proxy_triangle_count().unwrap_or(0).max(1);
    let stride = total.div_ceil(cap).max(1);
    let mut tris: Vec<[Vec3A; 3]> = Vec::new();
    let mut k = 0usize;
    scene.visit(0..u32::MAX, MASK_SHADOW, |_, p| {
        if let Piece::Triangle(t) = p {
            if k.is_multiple_of(stride) {
                tris.push(t);
            }
            k += 1;
        }
    });
    if tris.is_empty() {
        return Vec::new();
    }
    let mut acc = 0f64;
    let cdf: Vec<f64> = tris
        .iter()
        .map(|[a, b, c]| {
            acc += 0.5 * (*b - *a).cross(*c - *a).length() as f64;
            acc
        })
        .collect();
    let mut rng = Rng::new(seed);
    (0..n)
        .map(|_| {
            let u = rng.next_f32() as f64 * acc;
            let i = cdf.partition_point(|&c| c <= u).min(tris.len() - 1);
            let [a, b, c] = tris[i];
            let (mut s, mut t) = (rng.next_f32(), rng.next_f32());
            if s + t > 1.0 {
                (s, t) = (1.0 - s, 1.0 - t);
            }
            let z = 1.0 - 2.0 * rng.next_f32();
            let r = (1.0 - z * z).max(0.0).sqrt();
            let phi = std::f32::consts::TAU * rng.next_f32();
            Ray::new(
                a + s * (b - a) + t * (c - a),
                Vec3A::new(r * phi.cos(), r * phi.sin(), z),
            )
            .with_mask(MASK_SHADOW)
        })
        .collect()
}

/// One ray set's comparison.
pub struct Row {
    pub exact_mrays: f64,
    pub proxied_mrays: f64,
    pub n: usize,
    /// Rays the exact kernel calls occluded.
    pub occluded: usize,
    pub false_pos: usize,
    pub false_neg: usize,
}

impl Row {
    pub fn fp(&self) -> f64 {
        100.0 * self.false_pos as f64 / self.n as f64
    }
    pub fn fneg(&self) -> f64 {
        100.0 * self.false_neg as f64 / self.n as f64
    }
}

/// `exact` against `proxied` on `rays`, single-threaded, min of `repeats`.
pub fn compare(
    exact: &dyn Fn(&Ray) -> bool,
    proxied: &dyn Fn(&Ray) -> bool,
    rays: &[Ray],
    repeats: usize,
) -> Row {
    let e: Vec<bool> = rays.iter().map(exact).collect();
    let p: Vec<bool> = rays.iter().map(proxied).collect();
    let (mut best_e, mut best_p) = (f64::INFINITY, f64::INFINITY);
    for _ in 0..repeats {
        for (f, best) in [(exact, &mut best_e), (proxied, &mut best_p)] {
            let t = Instant::now();
            let mut h = 0usize;
            for r in rays {
                h += f(r) as usize;
            }
            *best = best.min(t.elapsed().as_secs_f64());
            std::hint::black_box(h);
        }
    }
    let n = rays.len();
    Row {
        exact_mrays: n as f64 / best_e / 1e6,
        proxied_mrays: n as f64 / best_p / 1e6,
        n,
        occluded: e.iter().filter(|&&x| x).count(),
        false_pos: e.iter().zip(&p).filter(|(e, p)| !**e && **p).count(),
        false_neg: e.iter().zip(&p).filter(|(e, p)| **e && !**p).count(),
    }
}

/// `(τ, FP %, FN %)` over `rays` at each of [`TAUS`].
pub fn tau_sweep(
    exact: &dyn Fn(&Ray) -> bool,
    proxied_at: &dyn Fn(&Ray, f32) -> bool,
    rays: &[Ray],
) -> Vec<(f32, f64, f64)> {
    let e: Vec<bool> = rays.iter().map(exact).collect();
    TAUS.iter()
        .map(|&tau| {
            let (mut fp, mut fneg) = (0usize, 0usize);
            for (r, &e) in rays.iter().zip(&e) {
                let p = proxied_at(r, tau);
                fp += (!e && p) as usize;
                fneg += (e && !p) as usize;
            }
            let n = rays.len() as f64;
            (tau, 100.0 * fp as f64 / n, 100.0 * fneg as f64 / n)
        })
        .collect()
}

pub fn print_proxy(name: &str, p: &ProxyReport) {
    println!(
        "{name:<12} proxy: {} tris, {} cut lanes, {} steps, trained in {:.1} s, {:.2} MiB; \
         validation FP {:.2}% FN {:.2}% (contact FN {:.2}%) over {}",
        p.triangles,
        p.cut_lanes,
        p.steps,
        p.train_seconds,
        p.bytes as f64 / (1 << 20) as f64,
        100.0 * p.false_positive,
        100.0 * p.false_negative,
        100.0 * p.contact_false_negative,
        p.validation,
    );
}

/// Prints one ray set's comparison row and, when `proxied_at` is given, its
/// τ sweep.
pub fn print_set(
    name: &str,
    label: &str,
    exact: &dyn Fn(&Ray) -> bool,
    proxied: &dyn Fn(&Ray) -> bool,
    proxied_at: Option<&dyn Fn(&Ray, f32) -> bool>,
    rays: &[Ray],
    repeats: usize,
) {
    if rays.is_empty() {
        return;
    }
    let r = compare(exact, proxied, rays, repeats);
    println!(
        "{name:<12} {label:<8} exact {:>7.2} Mray/s  proxied {:>7.2} Mray/s  ({:.2}x)  \
         FP {:>5.2}%  FN {:>5.2}%  FP+FN {:>5.2}%  ({}/{} occluded)",
        r.exact_mrays,
        r.proxied_mrays,
        r.proxied_mrays / r.exact_mrays,
        r.fp(),
        r.fneg(),
        r.fp() + r.fneg(),
        r.occluded,
        r.n,
    );
    if let Some(at) = proxied_at {
        let cells: Vec<String> = tau_sweep(exact, at, rays)
            .iter()
            .map(|(t, fp, fneg)| format!("τ={t:.1}: {fp:.2}/{fneg:.2}"))
            .collect();
        println!("{:<21} FP/FN % {}", "", cells.join("  "));
    }
}

/// A scene holding its own proxy, against its exact traversal.
pub fn report_scene(name: &str, scene: &Scene, sets: &[(&str, &[Ray])], repeats: usize) {
    let Some(p) = scene.occlusion_proxy() else {
        println!("{name:<12} no proxy (not eligible)");
        return;
    };
    print_proxy(name, p);
    let exact = |r: &Ray| scene.occluded_exact(r, SHADOW_T_MIN, f32::INFINITY);
    let proxied = |r: &Ray| scene.occluded(r, SHADOW_T_MIN, f32::INFINITY);
    let at = |r: &Ray, tau: f32| scene.occluded_with_threshold(r, SHADOW_T_MIN, f32::INFINITY, tau);
    for (label, rays) in sets {
        print_set(name, label, &exact, &proxied, Some(&at), rays, repeats);
    }
}
