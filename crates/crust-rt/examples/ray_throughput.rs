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
//! The default scenes are small enough that their whole BVH stays in cache,
//! so they measure the node test's arithmetic. `--large` adds two scenes
//! sized to defeat the last-level cache (MTRIS million triangles, default 8,
//! roughly 1-2 GiB of kernel data), which is the regime a production stage
//! like the Moana island traverses in: a sparse triangle soup (baked
//! foliage), and a field of instances whose top-level tree is out of cache
//! while each prototype's tree is not. Rays are incoherent, like secondary
//! bounces, and there are more of them so the touched working set is large
//! too. Building takes tens of seconds and a few GiB.

use crust_rt::{CommitOptions, Geometry, PacketLayout, Ray, Scene, SceneBuilder};
use glam::{Affine3A, Vec3A};
use std::sync::Arc;
use std::time::Instant;

fn uv_sphere(center: Vec3A, radius: f32, segs: usize, rings: usize) -> Geometry {
    let mut vertices = Vec::with_capacity((segs + 1) * (rings + 1));
    for r in 0..=rings {
        let phi = (r as f32 / rings as f32) * std::f32::consts::PI;
        for s in 0..=segs {
            let theta = (s as f32 / segs as f32) * std::f32::consts::TAU;
            vertices.push(
                center
                    + radius
                        * Vec3A::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin()),
            );
        }
    }
    let mut indices = Vec::with_capacity(2 * segs * rings);
    let row = segs + 1;
    for r in 0..rings {
        for s in 0..segs {
            let a = (r * row + s) as u32;
            let b = (r * row + s + 1) as u32;
            let c = ((r + 1) * row + s + 1) as u32;
            let d = ((r + 1) * row + s) as u32;
            indices.push([a, b, c]);
            indices.push([a, c, d]);
        }
    }
    Geometry::TriangleMesh {
        vertices: vertices.iter().map(|v: &Vec3A| v.to_array()).collect(),
        indices,
        normals: None,
    }
}

fn triangle_scene() -> Scene {
    let mut b = SceneBuilder::new();
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                b.attach(uv_sphere(
                    Vec3A::new(x as f32, y as f32, z as f32) * 2.5,
                    1.0,
                    40,
                    20,
                ));
            }
        }
    }
    commit(b)
}

fn sphere_grid_scene() -> Scene {
    let mut b = SceneBuilder::new();
    for x in 0..12 {
        for y in 0..12 {
            for z in 0..12 {
                b.attach(Geometry::Sphere {
                    center: Vec3A::new(x as f32, y as f32, z as f32) * 2.0 - Vec3A::splat(12.0),
                    radius: 0.6,
                });
            }
        }
    }
    commit(b)
}

fn instance_scene() -> Scene {
    let mut inner = SceneBuilder::new();
    inner.attach(uv_sphere(Vec3A::ZERO, 1.0, 24, 12));
    let inner = Arc::new(commit(inner));
    let mut b = SceneBuilder::new();
    for x in -2..=2 {
        for y in -2..=2 {
            for z in -2..=2 {
                b.attach(Geometry::Instance {
                    scene: Arc::clone(&inner),
                    transform: Affine3A::from_translation(
                        glam::Vec3::new(x as f32, y as f32, z as f32) * 2.5,
                    ),
                    transform_end: None,
                });
            }
        }
    }
    commit(b)
}

/// A tiny deterministic generator for the large scenes (the same LCG as
/// [`ray_batch`], seeded differently).
struct Lcg(u32);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }

    fn in_cube(&mut self, half: f32) -> Vec3A {
        Vec3A::new(self.next() - 0.5, self.next() - 0.5, self.next() - 0.5) * (2.0 * half)
    }
}

/// Half-extent of the large scenes' cube.
const LARGE_HALF: f32 = 50.0;

/// `n` random triangles in a cube, sized so a ray's mean free path is about
/// a quarter of the cube: rays penetrate and the traversal reaches deep
/// into the tree, rather than stopping on the first layer of a dense wall.
/// With `n` triangles of area `a` the mean free path is `2V / (n a)`.
fn soup_scene(n: usize) -> Scene {
    let side = 2.0 * LARGE_HALF;
    let area = 8.0 * side * side / n as f32;
    let edge = (2.0 * area).sqrt();
    let mut rng = Lcg(0x9E37_79B9);
    let mut vertices = Vec::with_capacity(3 * n);
    let mut indices = Vec::with_capacity(n);
    for i in 0..n {
        let c = rng.in_cube(LARGE_HALF);
        for _ in 0..3 {
            vertices.push(c + rng.in_cube(0.5 * edge));
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
    let protos: Vec<Arc<Scene>> = (0..8)
        .map(|k| {
            let mut p = SceneBuilder::new();
            p.attach(uv_sphere(Vec3A::ZERO, 1.0, 32 + 8 * k, 16 + 4 * k));
            Arc::new(commit(p))
        })
        .collect();
    let side = 2.0 * LARGE_HALF;
    // Cross-section pi r^2 per instance: mean free path V / (count pi r^2).
    let radius = (4.0 * side * side / (count as f32 * std::f32::consts::PI)).sqrt();
    let mut rng = Lcg(0x85EB_CA6B);
    let mut b = SceneBuilder::new();
    for i in 0..count {
        let axis = (rng.in_cube(1.0) + Vec3A::splat(1e-3)).normalize();
        let scale = radius * (0.5 + rng.next());
        let transform = Affine3A::from_scale_rotation_translation(
            glam::Vec3::splat(scale),
            glam::Quat::from_axis_angle(axis.into(), rng.next() * std::f32::consts::TAU),
            rng.in_cube(LARGE_HALF).into(),
        );
        b.attach(Geometry::Instance {
            scene: Arc::clone(&protos[i % protos.len()]),
            transform,
            transform_end: None,
        });
    }
    commit(b)
}

fn ray_batch(count: usize, extent: f32) -> Vec<Ray> {
    let mut state = 0x2545_F491u32;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as f32 / (1u32 << 24) as f32
    };
    (0..count)
        .map(|_| {
            let origin = Vec3A::new(next() - 0.5, next() - 0.5, next() - 0.5) * (4.0 * extent);
            let target = Vec3A::new(next() - 0.5, next() - 0.5, next() - 0.5) * extent;
            Ray::new(origin, (target - origin).normalize())
        })
        .collect()
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
                    scene.intersect(r, 0.001, f32::INFINITY).is_some()
                } else {
                    scene.occluded(r, 0.001, f32::INFINITY)
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
    b.commit_with(CommitOptions {
        layout: *LAYOUT.get().unwrap_or(&PacketLayout::Auto),
        ..Default::default()
    })
}

fn main() {
    let tri = triangle_scene();
    println!("tri_spheres: {} triangles", tri.primitive_count());
    probe("tri_spheres", &tri, 6.0);
    probe("sphere_grid", &sphere_grid_scene(), 14.0);
    probe("instances", &instance_scene(), 7.0);

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
