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

    report(label, prims, hits, (4 * N * N) as u64);
}

/// Prints both tree levels. Per-ray figures divide by the number of
/// *top-level* queries, i.e. rays cast, so instance work is expressed as
/// what it adds per ray rather than averaged into a query count that the
/// descents themselves inflated.
fn report(label: &str, prims: usize, hits: u64, rays: u64) {
    let (q0, n0, l0, pk0, sc0) = crust_rt::traversal_stats::read_level(0);
    let (q1, n1, l1, pk1, sc1) = crust_rt::traversal_stats::read_level(1);
    let per = |n: u64| n as f64 / q0.max(1) as f64;
    println!(
        "{label:<22} prims {prims:>9}  rays {q0:>8}  hit {:>5.1}%",
        100.0 * hits as f64 / rays as f64
    );
    println!(
        "  {:<10} nodes/ray {:>7.1}  leaves/ray {:>6.1}  packets/ray {:>6.1}  scalar/ray {:>6.1}",
        "top-level",
        per(n0),
        per(l0),
        per(pk0),
        per(sc0)
    );
    if q1 > 0 {
        println!(
            "  {:<10} nodes/ray {:>7.1}  leaves/ray {:>6.1}  packets/ray {:>6.1}  scalar/ray {:>6.1}                descents/ray {:>5.2}",
            "instanced",
            per(n1),
            per(l1),
            per(pk1),
            per(sc1),
            per(q1)
        );
    }
}

/// The same triangle budget reached through N instances of one prototype.
///
/// Counters are split by tree level, so the `instanced` row is the work a
/// ray does *inside* prototypes and is directly comparable with the flat
/// probe's `top-level` row.
fn probe_instanced(label: &str, copies: usize, segs: usize, rings: usize) {
    let mut inner = SceneBuilder::new();
    inner.attach(uv_sphere(segs, rings));
    let proto = std::sync::Arc::new(inner.commit());

    // Spacing must exceed the prototype's diameter. Packing unit spheres
    // closer makes every instance box overlap every other, so a ray
    // descends into hundreds of them and the measurement says more about
    // the layout than about instancing.
    const SPACING: f32 = 2.5;
    let side = (copies as f64).cbrt().ceil() as i32;
    let extent = SPACING * side as f32;
    let centre = extent * 0.5;

    let mut b = SceneBuilder::new();
    let mut placed = 0usize;
    'outer: for z in 0..side {
        for y in 0..side {
            for x in 0..side {
                if placed == copies {
                    break 'outer;
                }
                let t = glam::Vec3::new(
                    x as f32 * SPACING - centre,
                    y as f32 * SPACING - centre,
                    z as f32 * SPACING - centre,
                );
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
    // Far enough back to see the whole grid, with the fan sized to it.
    let dist = extent * 1.6;
    let spread = (centre * 1.1) / dist;
    let mut hits = 0u64;
    let t0 = std::time::Instant::now();
    for y in -N..N {
        for x in -N..N {
            let dir = Vec3A::new(
                spread * x as f32 / N as f32,
                spread * y as f32 / N as f32,
                1.0,
            );
            let ray = Ray::new(Vec3A::new(0.0, 0.0, -dist), dir);
            if scene.intersect(&ray, 0.001, f32::INFINITY).is_some() {
                hits += 1;
            }
        }
    }
    let el = t0.elapsed();
    report(
        &format!("{label} [{} res]", unique.triangles + unique.instances),
        prims,
        hits,
        (4 * N * N) as u64,
    );
    println!("  {:<10} wall {:?} (counters on -- relative only)", "", el);
}

/// Identical geometry and layout, reached through two levels of instances
/// instead of one: `groups` sub-scenes each holding `per_group` placements.
///
/// This is the controlled form of a difference seen on real data, where
/// the same element imported with a nested structure rendered 4.7x slower
/// than the flat one at an identical closest-hit count. Every extra level
/// costs a scalar instance test, a ray transform, and a cold descent into
/// another tree's root.
fn probe_nested(label: &str, groups: usize, per_group: usize, segs: usize, rings: usize) {
    let mut leaf = SceneBuilder::new();
    leaf.attach(uv_sphere(segs, rings));
    let proto = std::sync::Arc::new(leaf.commit());

    const SPACING: f32 = 2.5;
    let total = groups * per_group;
    let side = (total as f64).cbrt().ceil() as i32;
    let extent = SPACING * side as f32;
    let centre = extent * 0.5;

    // Same global placement as the flat probe, just partitioned into
    // `groups` mid-level scenes.
    let mut placements: Vec<glam::Vec3> = Vec::with_capacity(total);
    'outer: for z in 0..side {
        for y in 0..side {
            for x in 0..side {
                if placements.len() == total {
                    break 'outer;
                }
                placements.push(glam::Vec3::new(
                    x as f32 * SPACING - centre,
                    y as f32 * SPACING - centre,
                    z as f32 * SPACING - centre,
                ));
            }
        }
    }

    let mut root = SceneBuilder::new();
    for chunk in placements.chunks(per_group) {
        let mut mid = SceneBuilder::new();
        for t in chunk {
            mid.attach(Geometry::Instance {
                scene: std::sync::Arc::clone(&proto),
                transform: glam::Affine3A::from_translation(*t),
                transform_end: None,
            });
        }
        root.attach(Geometry::Instance {
            scene: std::sync::Arc::new(mid.commit()),
            transform: glam::Affine3A::IDENTITY,
            transform_end: None,
        });
    }
    let scene = root.commit();
    let prims = scene.primitive_count();
    let unique = scene.unique_primitive_breakdown();

    crust_rt::traversal_stats::reset();
    const N: i32 = 200;
    let dist = extent * 1.6;
    let spread = (centre * 1.1) / dist;
    let mut hits = 0u64;
    let t0 = std::time::Instant::now();
    for y in -N..N {
        for x in -N..N {
            let dir = Vec3A::new(spread * x as f32 / N as f32, spread * y as f32 / N as f32, 1.0);
            let ray = Ray::new(Vec3A::new(0.0, 0.0, -dist), dir);
            if scene.intersect(&ray, 0.001, f32::INFINITY).is_some() {
                hits += 1;
            }
        }
    }
    let el = t0.elapsed();
    report(
        &format!("{label} [{} res]", unique.triangles + unique.instances),
        prims,
        hits,
        (4 * N * N) as u64,
    );
    println!("  {:<10} wall {:?} (counters on -- relative only)", "", el);
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

    // Flat vs nested, same 32768 placements of the same prototype.
    println!();
    probe_nested("nested 32x1024", 32, 1024, 32, 16);
    probe_nested("nested 1024x32", 1024, 32, 32, 16);
}
