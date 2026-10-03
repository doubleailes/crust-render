//! Scenes and rays shared by the kernel's benchmark and probes: the
//! criterion bench (`benches/traversal.rs`) and the `ray_throughput` and
//! `traversal_probe` examples, which include this file with
//! `#[path = "../benches/fixtures/mod.rs"] mod fixtures;`.
//!
//! The scene builders take the commit to use, so `ray_throughput` can
//! commit every scene with the packet layout it was asked for while the
//! bench commits with the defaults.

// Each consumer uses a subset.
#![allow(dead_code)]

use crust_rt::{Geometry, Ray, Scene, SceneBuilder};
use glam::{Affine3A, Vec3A};
use std::sync::Arc;

/// A UV sphere mesh with `2 * segs * rings` triangles.
pub fn uv_sphere(center: Vec3A, radius: f32, segs: usize, rings: usize) -> Geometry {
    uv_sphere_placed(segs, rings, |p| center + radius * p)
}

/// A UV sphere of radius 1 at the origin whose vertices are the unit-sphere
/// points themselves. Not `uv_sphere(Vec3A::ZERO, 1.0, ..)`: `0.0 + -0.0`
/// is `+0.0`, so that one flips the sign of the zero coordinates.
pub fn unit_uv_sphere(segs: usize, rings: usize) -> Geometry {
    uv_sphere_placed(segs, rings, |p| p)
}

/// The UV sphere tessellation, each unit-sphere vertex mapped by `place`.
fn uv_sphere_placed(segs: usize, rings: usize, place: impl Fn(Vec3A) -> Vec3A) -> Geometry {
    let mut vertices = Vec::with_capacity((segs + 1) * (rings + 1));
    for r in 0..=rings {
        let phi = (r as f32 / rings as f32) * std::f32::consts::PI;
        for s in 0..=segs {
            let theta = (s as f32 / segs as f32) * std::f32::consts::TAU;
            vertices.push(place(Vec3A::new(
                phi.sin() * theta.cos(),
                phi.cos(),
                phi.sin() * theta.sin(),
            )));
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

/// Triangle-heavy scene: a 3×3×3 arrangement of subdivided spheres
/// (~43k triangles).
pub fn triangle_scene(commit: impl Fn(SceneBuilder) -> Scene) -> Scene {
    let mut b = SceneBuilder::new();
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                let c = Vec3A::new(x as f32, y as f32, z as f32) * 2.5;
                b.attach(uv_sphere(c, 1.0, 40, 20));
            }
        }
    }
    commit(b)
}

/// Analytic spheres only — isolates node tests from triangle work.
pub fn sphere_grid_scene(commit: impl Fn(SceneBuilder) -> Scene) -> Scene {
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

/// One mesh, instanced across a grid: two-level traversal.
pub fn instance_scene(commit: impl Fn(SceneBuilder) -> Scene) -> Scene {
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

/// A deterministic fan of rays aimed through the scene's bounds — a mix of
/// hits and misses, and of coherent and divergent directions. Seeded by a
/// small LCG so the batch is identical run to run.
pub fn ray_batch(count: usize, extent: f32) -> Vec<Ray> {
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
