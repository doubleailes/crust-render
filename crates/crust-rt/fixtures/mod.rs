//! Scene and ray fixtures shared by crust-rt's throughput probes and its
//! criterion bench (`examples/ray_throughput.rs`, `examples/traversal_probe.rs`,
//! `benches/traversal.rs`), included by each with `#[path]` so they time the
//! same geometry and the same rays.
//!
//! Randomness comes from `openqmc::pcg::Rng` (a dev-dependency), the
//! repository's one source of seeded draws outside the renderer's samplers.

#![allow(dead_code)] // each including target uses a different subset

use crust_rt::{Geometry, Ray, Scene, SceneBuilder};
use glam::{Affine3A, Vec3A};
use openqmc::pcg::Rng;
use std::sync::Arc;

/// The near bound every query here asks for — the renderer's
/// `crust_core::TRACE_T_MIN`, so the kernel is timed with the constant it is
/// specialised for.
pub const T_MIN: f32 = 0.001;

/// A UV sphere mesh with `2 * segs * rings` triangles — a stand-in for real
/// mesh geometry at a chosen size.
pub fn uv_sphere(center: Vec3A, radius: f32, segs: usize, rings: usize) -> Geometry {
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

/// Triangle-heavy: a 3×3×3 arrangement of subdivided spheres (~86k
/// triangles), several per leaf, which is what the 4-wide leaf intersector
/// targets. Committed by `commit`, so a caller can choose the options.
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

/// Analytic spheres only — isolates the node slab test and traversal
/// ordering from triangle work.
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

/// One mesh instanced across a 5×5×5 grid: two-level traversal through
/// transformed sub-scenes.
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

/// A uniform point in the cube of half-extent `half` about the origin.
pub fn in_cube(rng: &mut Rng, half: f32) -> Vec3A {
    let [x, y] = rng.next_2d();
    Vec3A::new(x - 0.5, y - 0.5, rng.next_f32() - 0.5) * (2.0 * half)
}

/// A deterministic batch of rays aimed through the scene's bounds — a mix of
/// hits and misses, and of coherent and divergent directions: origins in a
/// cube of half-extent `2 · extent`, targets in one of `extent / 2`.
pub fn ray_batch(count: usize, extent: f32) -> Vec<Ray> {
    let mut rng = Rng::new(0x2545_F491);
    (0..count)
        .map(|_| {
            let origin = in_cube(&mut rng, 2.0 * extent);
            let target = in_cube(&mut rng, 0.5 * extent);
            Ray::new(origin, (target - origin).normalize())
        })
        .collect()
}
