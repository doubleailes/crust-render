//! Traversal benchmarks for the intersection kernel.
//!
//! These exist to make the SIMD work in `bvh/` / `triangle.rs`
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

mod fixtures;

use criterion::{Criterion, criterion_group, criterion_main};
use crust_rt::{Scene, SceneBuilder};
use fixtures::{instance_scene, ray_batch, sphere_grid_scene, triangle_scene};
use std::hint::black_box;

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
    bench_scene(c, "tri_spheres", triangle_scene(SceneBuilder::commit), 6.0);
    bench_scene(
        c,
        "sphere_grid",
        sphere_grid_scene(SceneBuilder::commit),
        14.0,
    );
    bench_scene(c, "instances", instance_scene(SceneBuilder::commit), 7.0);
}

fn bench_build(c: &mut Criterion) {
    c.bench_function("build tri_spheres", |b| {
        b.iter(|| black_box(triangle_scene(SceneBuilder::commit).primitive_count()))
    });
}

criterion_group!(benches, bench_traversal, bench_build);
criterion_main!(benches);
