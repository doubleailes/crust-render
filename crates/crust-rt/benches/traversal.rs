//! Traversal benchmarks for the intersection kernel.
//!
//! These exist to make the SIMD work in `bvh.rs` / `triangle.rs`
//! measurable: they time the two query entry points (`intersect` and
//! `occluded`) over a fixed, deterministic ray batch, so a change to the
//! slab test or the leaf intersector shows up directly instead of being
//! buried under a full render.
//!
//! Scenes deliberately differ in what they stress:
//! - `tri_sphere_*`: a subdivided sphere mesh — many small triangles,
//!   several per leaf, which is what the 4-wide leaf intersector targets.
//! - `sphere_grid_*`: analytic spheres — no triangle work at all, so it
//!   isolates the node slab test and traversal ordering.
//! - `instances_*`: an instanced grid — traversal that recurses through
//!   transformed sub-scenes.

use criterion::{Criterion, criterion_group, criterion_main};
use crust_rt::{Geometry, Ray, Scene, SceneBuilder};
use glam::{Affine3A, Vec3A};
use std::hint::black_box;
use std::sync::Arc;

/// A UV sphere mesh with `2 * segs * rings` triangles.
fn uv_sphere(center: Vec3A, radius: f32, segs: usize, rings: usize) -> Geometry {
    let mut vertices = Vec::with_capacity((segs + 1) * (rings + 1));
    for r in 0..=rings {
        let v = r as f32 / rings as f32;
        let phi = v * std::f32::consts::PI;
        for s in 0..=segs {
            let u = s as f32 / segs as f32;
            let theta = u * std::f32::consts::TAU;
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

/// Triangle-heavy scene: a 3×3×3 arrangement of subdivided spheres
/// (~86k triangles).
fn triangle_scene() -> Scene {
    let mut b = SceneBuilder::new();
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                let c = Vec3A::new(x as f32, y as f32, z as f32) * 2.5;
                b.attach(uv_sphere(c, 1.0, 40, 20));
            }
        }
    }
    b.commit()
}

/// Analytic spheres only — isolates node tests from triangle work.
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

/// One mesh, instanced across a grid: two-level traversal.
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

/// A deterministic fan of rays aimed through the scene's bounds — a mix of
/// hits and misses, and of coherent and divergent directions. Seeded by a
/// small LCG so the batch is identical run to run.
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

fn bench_scene(c: &mut Criterion, name: &str, scene: Scene, extent: f32) {
    let rays = ray_batch(RAYS, extent);

    c.bench_function(&format!("{name} intersect"), |b| {
        b.iter(|| {
            let mut hits = 0usize;
            for r in &rays {
                if scene.intersect(r, 0.001, f32::INFINITY).is_some() {
                    hits += 1;
                }
            }
            black_box(hits)
        })
    });

    c.bench_function(&format!("{name} occluded"), |b| {
        b.iter(|| {
            let mut hits = 0usize;
            for r in &rays {
                if scene.occluded(r, 0.001, f32::INFINITY) {
                    hits += 1;
                }
            }
            black_box(hits)
        })
    });
}

fn bench_traversal(c: &mut Criterion) {
    bench_scene(c, "tri_spheres", triangle_scene(), 6.0);
    bench_scene(c, "sphere_grid", sphere_grid_scene(), 14.0);
    bench_scene(c, "instances", instance_scene(), 7.0);
}

fn bench_build(c: &mut Criterion) {
    c.bench_function("build tri_spheres", |b| {
        b.iter(|| black_box(triangle_scene().primitive_count()))
    });
}

criterion_group!(benches, bench_traversal, bench_build);
criterion_main!(benches);
