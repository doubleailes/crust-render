use criterion::{criterion_group, criterion_main, Criterion};
use ray_tracing::{
    camera::Camera,
    ray::Ray,
    vec3::{Point3},
    hittable_list::HittableList,
    light::Light,
    material::*,
    sphere::Sphere,
    color::Color,
};

use std::sync::Arc;

/// Dummy scene for benchmarking (just 2–3 objects)
fn make_benchmark_scene() -> (HittableList, Vec<Arc<dyn Light>>, Camera) {
    let mut world = HittableList::new();
    let mut lights = Vec::new();

    let material_ground = Arc::new(Lambertian::new(Color::new(0.8, 0.8, 0.0)));
    world.add(Box::new(Sphere::new(Point3::new(0.0, -100.5, -1.0), 100.0, material_ground)));

    let material_center = Arc::new(CookTorrance::new(Color::new(0.7, 0.3, 0.3), 0.2, 0.0));
    world.add(Box::new(Sphere::new(Point3::new(0.0, 0.0, -1.0), 0.5, material_center)));

    let light = Arc::new(Emissive::new(Color::new(5.0, 5.0, 5.0), Point3::new(0.0, 2.0, -1.0), 0.2));
    world.add(Box::new(Sphere::new(light.position(), light.radius(), light.clone())));
    lights.push(light);

    let camera = Camera::new(
        Point3::new(0.0, 0.0, 1.0),
        Point3::new(0.0, 0.0, -1.0),
        Point3::new(0.0, 1.0, 0.0),
        90.0,
        1.0,
        0.0,
        1.0,
    );

    (world, lights, camera)
}

/// Bench one pixel's path tracing
fn bench_path_tracer(c: &mut Criterion) {
    let (world, lights, camera) = make_benchmark_scene();
    let spp = 32;
    let max_depth = 8;

    c.bench_function("trace 4 pixels", |b| {
        b.iter(|| {
            let mut color = Color::new(0.0, 0.0, 0.0);
            for j in 0..2 {
                for i in 0..2 {
                    for _ in 0..spp {
                        let u = (i as f32 + rand::random::<f32>()) / 2.0;
                        let v = (j as f32 + rand::random::<f32>()) / 2.0;
                        let ray = camera.get_ray(u, v);
                        color += ray_tracing::ray_color(&ray, &world, &lights, max_depth);
                    }
                }
            }
            color / (4.0 * spp as f32)
        });
    });
}

criterion_group!(benches, bench_path_tracer);
criterion_main!(benches);
