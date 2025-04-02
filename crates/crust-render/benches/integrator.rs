use criterion::{Criterion, criterion_group, criterion_main};
use crust_render::Camera;
use crust_render::simple_scene;
use crust_render::{RenderSettings, Renderer};
use utils::{Point3, Vec3};

const ASPECT_RATIO: f32 = 16.0 / 9.0;
const IMAGE_WIDTH: usize = 400;
const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;
const MIN_SAMPLES: u32 = 8;
const VARIANCE_THRESHOLD: f32 = 0.0005; // You can tweak this!

fn bench_dot(c: &mut Criterion) {
    let vec1 = Vec3::new(1.0, 2.0, 3.0);
    let vec2 = Vec3::new(4.0, 5.0, 6.0);

    c.bench_function("vec3 dot", |b| {
        b.iter(|| {
            let _ = utils::dot(vec1, vec2);
        })
    });
}

fn bench_simple_world(c: &mut Criterion) {
    c.bench_function("simple world", |b| {
        b.iter(|| {
            let (world, lights) = simple_scene();
            let lookfrom = Point3::new(13.0, 2.0, 3.0);
            let lookat = Point3::new(0.0, 0.0, 0.0);
            let vup = Point3::new(0.0, 1.0, 0.0);
            let dist_to_focus = 10.0;
            let aperture = 0.1;

            let cam = Camera::new(
                lookfrom,
                lookat,
                vup,
                20.0,
                ASPECT_RATIO,
                aperture,
                dist_to_focus,
            );
            let render_settings = RenderSettings::new(
                10,
                20,
                IMAGE_WIDTH,
                IMAGE_HEIGHT,
                MIN_SAMPLES,
                VARIANCE_THRESHOLD,
            );
            let renderer = Renderer::new(cam, world, lights, render_settings);
            let _ = renderer.render();
        })
    });
}

criterion_group!(name = benches;config = Criterion::default(); targets= bench_dot,bench_simple_world);
criterion_main!(benches);
