use criterion::{criterion_group, criterion_main, Criterion};
use ray_tracing::vec3::Vec3; // adjust your crate name as needed

fn bench_dot(c: &mut Criterion) {
    let vec1 = Vec3::new(1.0, 2.0, 3.0);
    let vec2 = Vec3::new(4.0, 5.0, 6.0);

    c.bench_function("vec3 dot", |b| {
        b.iter(|| {
            let _ = ray_tracing::vec3::dot(vec1, vec2);
        })
    });
}

criterion_group!(benches, bench_dot);
criterion_main!(benches);
