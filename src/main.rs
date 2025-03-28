mod camera;
mod color;
mod common;
mod convert;
mod hittable;
mod hittable_list;
mod material;
mod ray;
mod sphere;
mod vec3;

use camera::Camera;
use color::Color;
use exr::prelude::*;
use hittable::{HitRecord, Hittable};
use hittable_list::HittableList;
use ray::Ray;
use sphere::Sphere;
use vec3::{Point3, Vec3};
use material::{Lambertian, Metal};
use std::sync::Arc;


// Constants

const ASPECT_RATIO: f32 = 16.0 / 9.0;
const IMAGE_WIDTH: usize = 400;
const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;
const SAMPLES_PER_PIXEL: i32 = 100;
const MAX_DEPTH: i32 = 50;

fn ray_color(r: &Ray, world: &dyn Hittable, depth: i32) -> Color {
    // If we've exceeded the ray bounce limit, no more light is gathered
    if depth <= 0 {
        return Color::new(0.0, 0.0, 0.0);
    }
    let mut rec = HitRecord::new();
    if world.hit(r, 0.001, common::INFINITY, &mut rec) {
        let mut attenuation = Color::default();
        let mut scattered = Ray::default();
        if rec
            .mat
            .as_ref()
            .unwrap()
            .scatter(r, &rec, &mut attenuation, &mut scattered)
        {
            return attenuation * ray_color(&scattered, world, depth - 1);
        }
        return Color::new(0.0, 0.0, 0.0);
    }
    let unit_direction: Vec3 = vec3::unit_vector(r.direction());
    let t: f32 = 0.5 * (unit_direction.y() + 1.0);
    (1.0 - t) * Color::new(1.0, 1.0, 1.0) + t * Color::new(0.5, 0.7, 1.0)
}

fn get_color(
    world: &dyn Hittable,
    x: usize,
    y: usize,
    origin: Vec3,
    lower_left_corner: Vec3,
    horizontal: Vec3,
    vertical: Vec3,
) -> (f32, f32, f32, f32) {
    let mut pixel_color = Color::new(0.0, 0.0, 0.0);
    for _ in 0..SAMPLES_PER_PIXEL {
        let u = (x as f32 + common::random()) / (IMAGE_WIDTH - 1) as f32;
        let v = (y as f32 + common::random()) / (IMAGE_HEIGHT - 1) as f32;
        let r = Ray::new(
            origin,
            lower_left_corner + u * horizontal + v * vertical - origin,
        );
        pixel_color += ray_color(&r, world, MAX_DEPTH);
    }
    (
        pixel_color.x() / SAMPLES_PER_PIXEL as f32,
        pixel_color.y() / SAMPLES_PER_PIXEL as f32,
        pixel_color.z() / SAMPLES_PER_PIXEL as f32,
        1.0,
    )
}
fn main() {
    // World

    let mut world = HittableList::new();
    let material_ground = Arc::new(Lambertian::new(Color::new(0.8, 0.8, 0.0)));
    let material_center = Arc::new(Lambertian::new(Color::new(0.7, 0.3, 0.3)));
    let material_left = Arc::new(Metal::new(Color::new(0.8, 0.8, 0.8), 0.2));
    let material_right = Arc::new(Metal::new(Color::new(0.8, 0.6, 0.2), 1.0));
 
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, -100.5, -1.0),
        100.0,
        material_ground,
    )));
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, 0.0, -1.0),
        0.5,
        material_center,
    )));
    world.add(Box::new(Sphere::new(
        Point3::new(-1.0, 0.0, -1.0),
        0.5,
        material_left,
    )));
    world.add(Box::new(Sphere::new(
        Point3::new(1.0, 0.0, -1.0),
        0.5,
        material_right,
    )));

    // Camera

    let cam = Camera::new();

    let viewport_height = 2.0;
    let viewport_width = ASPECT_RATIO * viewport_height;
    let focal_length = 1.0;

    let origin = Point3::new(0.0, 0.0, 0.0);
    let horizontal = Vec3::new(viewport_width, 0.0, 0.0);
    // Viewport is flipped vertically to start from the top-left corner
    // Due to exr crate's coordinate system
    let vertical = Vec3::new(0.0, -viewport_height, 0.0);
    let lower_left_corner =
        origin - horizontal / 2.0 - vertical / 2.0 - Vec3::new(0.0, 0.0, focal_length);

    // Render
    write_rgba_file("output.exr", IMAGE_WIDTH, IMAGE_HEIGHT, |x, y| {
        get_color(
            &world,
            x,
            y,
            origin,
            lower_left_corner,
            horizontal,
            vertical,
        )
    })
    .expect("writing image");
    convert::convert();
}
