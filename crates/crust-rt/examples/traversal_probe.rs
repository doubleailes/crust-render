//! Answers one question: how many BVH nodes does a ray touch?
//!
//! That distinguishes the two reasons traversal can be slow. If a ray
//! visits few nodes yet throughput is poor, each visit is expensive and
//! the fix is locality — smaller or better-ordered nodes. If it visits
//! very many, the tree itself is bad and the fix is the build.
//!
//! Build with the counters on, since they are compiled out by default:
//!   cargo run --release -p crust-rt --features traversal-stats \
//!       --example traversal_probe
//!
//! Trust the counts from this build, not its timings: the counters are
//! global atomics and contend across threads.

use crust_rt::{Geometry, Ray, SceneBuilder};
use glam::Vec3A;

/// A UV sphere, as a stand-in for real mesh geometry at a chosen size.
fn uv_sphere(segs: usize, rings: usize) -> Geometry {
    let mut vertices = Vec::new();
    for r in 0..=rings {
        let phi = (r as f32 / rings as f32) * std::f32::consts::PI;
        for s in 0..=segs {
            let th = (s as f32 / segs as f32) * std::f32::consts::TAU;
            vertices.push(Vec3A::new(
                phi.sin() * th.cos(),
                phi.cos(),
                phi.sin() * th.sin(),
            ));
        }
    }
    let row = segs + 1;
    let mut indices = Vec::new();
    for r in 0..rings {
        for s in 0..segs {
            let (a, b, c, d) = (
                (r * row + s) as u32,
                (r * row + s + 1) as u32,
                ((r + 1) * row + s + 1) as u32,
                ((r + 1) * row + s) as u32,
            );
            indices.push([a, b, c]);
            indices.push([a, c, d]);
        }
    }
    Geometry::TriangleMesh {
        vertices,
        indices,
        normals: None,
    }
}

fn probe(label: &str, geom: Geometry) {
    let mut b = SceneBuilder::new();
    b.attach(geom);
    let scene = b.commit();
    let prims = scene.primitive_count();

    crust_rt::traversal_stats::reset();

    // A fixed fan of rays aimed through the scene, so every configuration
    // is asked the same question.
    const N: i32 = 200;
    let mut hits = 0u64;
    for y in -N..N {
        for x in -N..N {
            // Narrow fan: the unit sphere at the origin subtends about
            // +/-0.25 from z=-4, so this keeps most rays on geometry.
            // A wide fan would average in misses that exit after one
            // node test and hide the real traversal depth.
            let dir = Vec3A::new(
                0.28 * x as f32 / N as f32,
                0.28 * y as f32 / N as f32,
                1.0,
            );
            let ray = Ray::new(Vec3A::new(0.0, 0.0, -4.0), dir);
            if scene.intersect(&ray, 0.001, f32::INFINITY).is_some() {
                hits += 1;
            }
        }
    }

    let (queries, nodes, leaves, packets, scalars) = crust_rt::traversal_stats::read();
    let per = |n: u64| n as f64 / queries.max(1) as f64;
    println!(
        "{label:<22} prims {prims:>9}  queries {queries:>8}  hit {:>5.1}%\n\
         {:<22} nodes/ray {:>7.1}  leaves/ray {:>6.1}  packet-tests/ray {:>6.1}  scalar/ray {:>6.1}",
        100.0 * hits as f64 / (4 * N * N) as f64,
        "",
        per(nodes),
        per(leaves),
        per(packets),
        per(scalars),
    );
}

/// The same triangle budget reached through N instances of one prototype.
///
/// **Read with care.** `QUERIES` counts every [`crust_rt::Scene::intersect`],
/// and an instance descent is one of those, so the per-ray figures below
/// average the outer tree and the inner ones together and are *not*
/// comparable with the flat probe. Left in because the scalar/ray column
/// still shows the instance tests a ray performs, but drawing conclusions
/// about instancing overhead from these needs the counters split by tree
/// level first.
fn probe_instanced(label: &str, copies: usize, segs: usize, rings: usize) {
    let mut inner = SceneBuilder::new();
    inner.attach(uv_sphere(segs, rings));
    let proto = std::sync::Arc::new(inner.commit());

    let mut b = SceneBuilder::new();
    let side = (copies as f64).cbrt().ceil() as i32;
    let mut placed = 0usize;
    'outer: for z in 0..side {
        for y in 0..side {
            for x in 0..side {
                if placed == copies {
                    break 'outer;
                }
                // Packed tightly around the origin so the same ray fan
                // still lands on geometry.
                let t = glam::Vec3::new(x as f32 * 0.05, y as f32 * 0.05, z as f32 * 0.05);
                b.attach(Geometry::Instance {
                    scene: std::sync::Arc::clone(&proto),
                    transform: glam::Affine3A::from_translation(t),
                    transform_end: None,
                });
                placed += 1;
            }
        }
    }
    let scene = b.commit();
    let prims = scene.primitive_count();
    let unique = scene.unique_primitive_breakdown();

    crust_rt::traversal_stats::reset();
    const N: i32 = 200;
    let mut hits = 0u64;
    for y in -N..N {
        for x in -N..N {
            let dir = Vec3A::new(0.28 * x as f32 / N as f32, 0.28 * y as f32 / N as f32, 1.0);
            let ray = Ray::new(Vec3A::new(0.0, 0.0, -4.0), dir);
            if scene.intersect(&ray, 0.001, f32::INFINITY).is_some() {
                hits += 1;
            }
        }
    }
    let (queries, nodes, leaves, packets, scalars) = crust_rt::traversal_stats::read();
    let per = |n: u64| n as f64 / queries.max(1) as f64;
    println!(
        "{label:<22} top {prims:>9}  resident {:>9}  hit {:>5.1}%\n\
         {:<22} nodes/ray {:>7.1}  leaves/ray {:>6.1}  packet-tests/ray {:>6.1}  scalar/ray {:>6.1}",
        unique.triangles + unique.instances,
        100.0 * hits as f64 / (4 * N * N) as f64,
        "",
        per(nodes),
        per(leaves),
        per(packets),
        per(scalars),
    );
}

fn main() {
    // Same shape at growing primitive counts: nodes/ray should climb only
    // logarithmically, so if throughput falls much faster than this does,
    // the cost is per-visit rather than per-node-count.
    probe("sphere 32x16", uv_sphere(32, 16));
    probe("sphere 128x64", uv_sphere(128, 64));
    probe("sphere 512x256", uv_sphere(512, 256));
    probe("sphere 1024x512", uv_sphere(1024, 512));

    // Same geometry reached through instances. `nodes/ray` here counts
    // only the outer tree's nodes -- the inner descent is a separate
    // Scene::intersect and shows up as another query -- so compare the
    // *query* count as well as the per-ray figures.
    println!();
    probe_instanced("inst 64 x 16k tris", 64, 128, 64);
    probe_instanced("inst 4096 x 1k tris", 4096, 32, 16);
    probe_instanced("inst 32768 x 1k tris", 32768, 32, 16);
}
