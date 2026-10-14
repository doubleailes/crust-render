//! Deterministic ray-throughput probe: min-of-N wall time per scene and
//! query type, printed as Mray/s.
//!
//! Criterion reports means, which drift by 10%+ on a loaded or shared
//! machine. For comparing two versions of the intersection kernel the
//! *minimum* over repeats is the more useful statistic — noise only ever
//! adds time, so the floor is the closest thing to the true cost.
//!
//! ```text
//! cargo run --release -p crust-rt --example ray_throughput
//! cargo run --release -p crust-rt --example ray_throughput -- --large [MTRIS]
//! cargo run --release -p crust-rt --example ray_throughput -- --layout indexed [--large [MTRIS]]
//! ```
//!
//! `--layout gathered|indexed|auto` commits every scene with that triangle
//! packet layout (default `auto`), which is how the two are A/B'd per tree
//! size.
//!
//! `--neural [--cut size|refined] [--steps N]` (a build with the `neural-bvh`
//! feature) measures learned occlusion proxies against the exact kernel
//! instead: each eligible scene is committed with a proxy, and the probe
//! prints exact and proxied shadow-ray Mray/s, false-positive and
//! false-negative rates (on the fixture rays and on rays leaving surfaces,
//! where a leak at contact shows), the training time, the proxy bytes and a
//! sweep of the threshold τ. With `--large`, the soup holds one proxy over
//! its whole tree (out of cache) and the field's prototypes hold one each.
//!
//! ```text
//! cargo run --release -p crust-rt --features neural-bvh --example ray_throughput -- --neural [--large [MTRIS]]
//! ```
//!
//! The default scenes are small enough that their whole BVH stays in cache,
//! so they measure the node test's arithmetic. `--large` adds two scenes
//! sized to defeat the last-level cache (MTRIS million triangles, default 8,
//! roughly 1-2 GiB of kernel data), which is the regime a production stage
//! like the Moana island traverses in: a sparse triangle soup (baked
//! foliage), and a field of instances whose top-level tree is out of cache
//! while each prototype's tree is not. Rays are incoherent, like secondary
//! bounces, and there are more of them so the touched working set is large
//! too. Building takes tens of seconds and a few GiB.

#[path = "../fixtures/mod.rs"]
mod fixtures;
#[cfg(feature = "neural-bvh")]
#[path = "../fixtures/neural.rs"]
mod neural;

use crust_rt::{CommitOptions, Geometry, PacketLayout, Scene, SceneBuilder};
use fixtures::{
    T_MIN, in_cube, instance_scene, ray_batch, sphere_grid_scene, triangle_scene, uv_sphere,
};
use glam::{Affine3A, Vec3A};
use openqmc::pcg::Rng;
use std::sync::Arc;
use std::time::Instant;

/// Half-extent of the large scenes' cube.
const LARGE_HALF: f32 = 50.0;

/// `n` random triangles in a cube, sized so a ray's mean free path is about
/// a quarter of the cube: rays penetrate and the traversal reaches deep
/// into the tree, rather than stopping on the first layer of a dense wall.
/// With `n` triangles of area `a` the mean free path is `2V / (n a)`.
fn soup_scene(n: usize) -> Scene {
    soup_scene_with(n, commit)
}

fn soup_scene_with(n: usize, commit: impl Fn(SceneBuilder) -> Scene) -> Scene {
    let side = 2.0 * LARGE_HALF;
    let area = 8.0 * side * side / n as f32;
    let edge = (2.0 * area).sqrt();
    let mut rng = Rng::new(0x9E37_79B9);
    let mut vertices = Vec::with_capacity(3 * n);
    let mut indices = Vec::with_capacity(n);
    for i in 0..n {
        let c = in_cube(&mut rng, LARGE_HALF);
        for _ in 0..3 {
            vertices.push(c + in_cube(&mut rng, 0.5 * edge));
        }
        let b = 3 * i as u32;
        indices.push([b, b + 1, b + 2]);
    }
    let mut b = SceneBuilder::new();
    b.attach(Geometry::TriangleMesh {
        vertices: vertices.iter().map(|v: &Vec3A| v.to_array()).collect(),
        indices,
        normals: None,
    });
    commit(b)
}

/// `count` instances of eight distinct sphere prototypes (1-4 k triangles
/// each), randomly placed, rotated and scaled, with the same quarter-cube
/// mean free path as the soup. The shape of an instanced production stage:
/// the top-level tree is far out of cache, the prototypes' trees are not.
fn instance_field_scene(count: usize) -> Scene {
    instance_field_scene_with(count, commit)
}

fn instance_field_scene_with(count: usize, commit_proto: impl Fn(SceneBuilder) -> Scene) -> Scene {
    let protos: Vec<Arc<Scene>> = (0..8)
        .map(|k| {
            let mut p = SceneBuilder::new();
            p.attach(uv_sphere(Vec3A::ZERO, 1.0, 32 + 8 * k, 16 + 4 * k));
            Arc::new(commit_proto(p))
        })
        .collect();
    let side = 2.0 * LARGE_HALF;
    // Cross-section pi r^2 per instance: mean free path V / (count pi r^2).
    let radius = (4.0 * side * side / (count as f32 * std::f32::consts::PI)).sqrt();
    let mut rng = Rng::new(0x85EB_CA6B);
    let mut b = SceneBuilder::new();
    for i in 0..count {
        let axis = (in_cube(&mut rng, 1.0) + Vec3A::splat(1e-3)).normalize();
        let scale = radius * (0.5 + rng.next_f32());
        let transform = Affine3A::from_scale_rotation_translation(
            glam::Vec3::splat(scale),
            glam::Quat::from_axis_angle(axis.into(), rng.next_f32() * std::f32::consts::TAU),
            in_cube(&mut rng, LARGE_HALF).into(),
        );
        b.attach(Geometry::Instance {
            scene: Arc::clone(&protos[i % protos.len()]),
            transform,
            transform_end: None,
        });
    }
    commit(b)
}

const RAYS: usize = 4096;
const REPEATS: usize = 40;
/// The large scenes trace more rays, so the set of nodes one pass touches
/// is itself far larger than a cache, and repeat less, since each pass is
/// long enough to average over scheduler noise.
const LARGE_RAYS: usize = 1 << 18;
const LARGE_REPEATS: usize = 3;

fn probe(name: &str, scene: &Scene, extent: f32) {
    probe_with(name, scene, extent, RAYS, REPEATS);
}

fn probe_with(name: &str, scene: &Scene, extent: f32, n_rays: usize, repeats: usize) {
    let rays = ray_batch(n_rays, extent);

    for (kind, closest) in [("intersect", true), ("occluded", false)] {
        #[cfg(feature = "traversal-stats")]
        crust_rt::traversal_stats::reset();
        let mut best = f64::INFINITY;
        let mut hits = 0usize;
        for _ in 0..repeats {
            let start = Instant::now();
            let mut h = 0usize;
            for r in &rays {
                let hit = if closest {
                    scene.intersect(r, T_MIN, f32::INFINITY).is_some()
                } else {
                    scene.occluded(r, T_MIN, f32::INFINITY)
                };
                if hit {
                    h += 1;
                }
            }
            let secs = start.elapsed().as_secs_f64();
            hits = h;
            best = best.min(secs);
        }
        println!(
            "{name:<12} {kind:<10} {:>8.3} ms   {:>6.2} Mray/s   ({hits}/{n_rays} hit)",
            best * 1e3,
            n_rays as f64 / best / 1e6,
        );
        // With the counters on (timings then relative only), where the
        // time went: nodes and leaves per ray, top level and inside
        // instances. Only closest-hit traversal is instrumented.
        #[cfg(feature = "traversal-stats")]
        if closest {
            let per = (n_rays * repeats) as f64;
            let (_, n0, l0, _, _) = crust_rt::traversal_stats::read_level(0);
            let (_, n1, l1, _, _) = crust_rt::traversal_stats::read_level(1);
            println!(
                "{:<23} nodes/ray {:>7.1} + {:>7.1} instanced   leaves/ray {:>6.1} + {:>6.1}",
                "",
                n0 as f64 / per,
                n1 as f64 / per,
                l0 as f64 / per,
                l1 as f64 / per,
            );
        }
    }
}

/// The packet layout every scene below commits with (`--layout`).
static LAYOUT: std::sync::OnceLock<PacketLayout> = std::sync::OnceLock::new();

fn commit(b: SceneBuilder) -> Scene {
    b.commit_with(CommitOptions::with_layout(
        *LAYOUT.get().unwrap_or(&PacketLayout::Auto),
    ))
}

fn main() {
    // `--layout` is read before any scene is built, so the default scenes
    // commit with it too.
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("--layout") {
        args.next();
        let layout = match args.next().as_deref() {
            Some("gathered") => PacketLayout::Gathered,
            Some("indexed") => PacketLayout::Indexed,
            Some("auto") => PacketLayout::Auto,
            other => panic!("--layout wants gathered|indexed|auto, got {other:?}"),
        };
        LAYOUT.set(layout).expect("set once");
    }
    if args.peek().map(String::as_str) == Some("--neural") {
        args.next();
        #[cfg(feature = "neural-bvh")]
        return neural_main(args);
        #[cfg(not(feature = "neural-bvh"))]
        panic!("--neural needs a build with `--features neural-bvh`");
    }

    let tri = triangle_scene(commit);
    println!("tri_spheres: {} triangles", tri.primitive_count());
    probe("tri_spheres", &tri, 6.0);
    probe("sphere_grid", &sphere_grid_scene(commit), 14.0);
    probe("instances", &instance_scene(commit), 7.0);
    if args.next().as_deref() == Some("--large") {
        let mtris: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(8.0);
        let n = (mtris * 1e6) as usize;

        let t = Instant::now();
        let soup = soup_scene(n);
        let mem = soup.memory_footprint();
        println!(
            "soup: {} triangles, {:.0} MiB kernel, built in {:.1} s",
            soup.primitive_count(),
            mem.total() as f64 / (1 << 20) as f64,
            t.elapsed().as_secs_f64()
        );
        probe_with("soup", &soup, LARGE_HALF, LARGE_RAYS, LARGE_REPEATS);
        drop(soup);

        // A quarter as many instances as the soup has triangles: 2 M at the
        // default, a top level the size of a production stage's.
        let t = Instant::now();
        let field = instance_field_scene(n / 4);
        let mem = field.memory_footprint();
        println!(
            "inst_field: {} instances, {:.0} MiB kernel, built in {:.1} s",
            field.primitive_count(),
            mem.total() as f64 / (1 << 20) as f64,
            t.elapsed().as_secs_f64()
        );
        probe_with("inst_field", &field, LARGE_HALF, LARGE_RAYS, LARGE_REPEATS);
    }
}

/// `--neural`: proxies against the exact kernel, scene by scene.
#[cfg(feature = "neural-bvh")]
fn neural_main(mut args: std::iter::Peekable<impl Iterator<Item = String>>) {
    use crust_rt::{MASK_SHADOW, PROXY_DEFAULT_STEPS, ProxyCut, ProxyRequest};

    // Above every fixture prototype (the `instances` sphere has 576
    // triangles), so only the scenes measured hold a proxy and their
    // `occluded_exact` stays exact all the way down.
    let mut req = ProxyRequest::shadow(10_000);
    req.steps = PROXY_DEFAULT_STEPS;
    let mut large = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cut" => {
                req.cut = match args.next().as_deref() {
                    Some("size") => ProxyCut::Size,
                    Some("refined") => ProxyCut::Refined,
                    other => panic!("--cut wants size|refined, got {other:?}"),
                }
            }
            "--steps" => req.steps = args.next().and_then(|s| s.parse().ok()).expect("--steps N"),
            "--large" => {
                large = Some(
                    args.next_if(|s| !s.starts_with("--"))
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(8.0),
                )
            }
            other => panic!("unknown argument {other}"),
        }
    }
    let proxied_with = |req: ProxyRequest| {
        move |b: SceneBuilder| {
            let mut o = CommitOptions::with_layout(*LAYOUT.get().unwrap_or(&PacketLayout::Auto));
            o.occlusion_proxy = Some(req);
            b.commit_with(o)
        }
    };
    let proxied = proxied_with(req);
    println!("proxy request: {req:?}");
    let shadow = |rays: Vec<crust_rt::Ray>| -> Vec<crust_rt::Ray> {
        rays.into_iter().map(|r| r.with_mask(MASK_SHADOW)).collect()
    };
    let one = |name: &str, scene: &Scene, extent: f32, n: usize, repeats: usize| {
        let through = shadow(ray_batch(n, extent));
        let surface = neural::surface_rays(scene, n, 0x5EED, 1 << 22);
        neural::report_scene(
            name,
            scene,
            &[("through", &through), ("surface", &surface)],
            repeats,
        );
    };
    one("tri_spheres", &triangle_scene(proxied), 6.0, RAYS, REPEATS);
    one(
        "sphere_grid",
        &sphere_grid_scene(proxied),
        14.0,
        RAYS,
        REPEATS,
    );
    one("instances", &instance_scene(proxied), 7.0, RAYS, REPEATS);
    let Some(mtris) = large else { return };
    let n = (mtris * 1e6) as usize;

    let t = Instant::now();
    let soup = soup_scene_with(n, proxied);
    println!(
        "soup: {} triangles, {:.0} MiB kernel, built and trained in {:.1} s",
        soup.primitive_count(),
        soup.memory_footprint().total() as f64 / (1 << 20) as f64,
        t.elapsed().as_secs_f64()
    );
    one("soup", &soup, LARGE_HALF, LARGE_RAYS, LARGE_REPEATS);
    drop(soup);

    // The field's top level stays exact; each prototype holds a proxy, so
    // the exact side is the same field committed without them.
    let exact = instance_field_scene(n / 4);
    // The field's prototypes are 2-4 k triangles: below the threshold above.
    let field = instance_field_scene_with(
        n / 4,
        proxied_with(ProxyRequest {
            min_triangles: 1,
            ..req
        }),
    );
    for proto in field.instanced_scenes().iter().take(1) {
        if let Some(p) = proto.occlusion_proxy() {
            neural::print_proxy("inst_proto", p);
        }
    }
    let through = shadow(ray_batch(LARGE_RAYS, LARGE_HALF));
    neural::print_set(
        "inst_field",
        "through",
        &|r| exact.occluded(r, neural::SHADOW_T_MIN, f32::INFINITY),
        &|r| field.occluded(r, neural::SHADOW_T_MIN, f32::INFINITY),
        None,
        &through,
        LARGE_REPEATS,
    );
}
