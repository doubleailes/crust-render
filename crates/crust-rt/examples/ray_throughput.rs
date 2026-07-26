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
//! ```

use crust_rt::{Geometry, Ray, Scene, SceneBuilder};
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
        vertices,
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
    b.commit()
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
    b.commit()
}

fn instance_scene() -> Scene {
    let mut inner = SceneBuilder::new();
    inner.attach(uv_sphere(Vec3A::ZERO, 1.0, 24, 12));
    let inner = Arc::new(inner.commit());
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
    b.commit()
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

fn probe(name: &str, scene: &Scene, extent: f32) {
    let rays = ray_batch(RAYS, extent);

    for (kind, closest) in [("intersect", true), ("occluded", false)] {
        let mut best = f64::INFINITY;
        let mut hits = 0usize;
        for _ in 0..REPEATS {
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
            "{name:<12} {kind:<10} {:>8.3} ms   {:>6.2} Mray/s   ({hits}/{RAYS} hit)",
            best * 1e3,
            RAYS as f64 / best / 1e6,
        );
    }
}

fn main() {
    let tri = triangle_scene();
    println!("tri_spheres: {} triangles", tri.primitive_count());
    probe("tri_spheres", &tri, 6.0);
    probe("sphere_grid", &sphere_grid_scene(), 14.0);
    probe("instances", &instance_scene(), 7.0);
}
