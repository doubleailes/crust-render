mod color;
mod ray;
mod vec3;

use color::Color;
use exr::prelude::*;
use ray::Ray;
use vec3::{Point3, Vec3};

const ASPECT_RATIO: f32 = 16.0 / 9.0;
const IMAGE_WIDTH: usize = 400;
const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;

fn hit_sphere(center: Point3, radius: f32, r: &Ray) -> bool {
    let oc = r.origin() - center;
    let a = vec3::dot(r.direction(), r.direction());
    let b = 2.0 * vec3::dot(oc, r.direction());
    let c = vec3::dot(oc, oc) - radius * radius;
    let discriminant = b * b - 4.0 * a * c;
    discriminant >= 0.0
}

fn ray_color(r: &Ray) -> Color {
    if hit_sphere(Point3::new(0.0, 0.0, -1.0), 0.5, r) {
        return Color::new(1.0, 0.0, 0.0);
    }
    let unit_direction: Vec3 = vec3::unit_vector(r.direction());
    let t: f32 = 0.5 * (unit_direction.y() + 1.0);
    (1.0 - t) * Color::new(1.0, 1.0, 1.0) + t * Color::new(0.5, 0.7, 1.0)
}

fn get_color(
    x: usize,
    y: usize,
    origin: Vec3,
    lower_left_corner: Vec3,
    horizontal: Vec3,
    vertical: Vec3,
) -> (f32, f32, f32, f32) {
    let u = x as f32 / (IMAGE_WIDTH - 1) as f32;
    let v = y as f32 / (IMAGE_HEIGHT - 1) as f32;
    let r = Ray::new(
        origin,
        lower_left_corner + u * horizontal + v * vertical - origin,
    );
    let pixel_color = ray_color(&r);
    (pixel_color.x(), pixel_color.y(), pixel_color.z(), 1.0)
}
fn main() {

    // Camera

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
        get_color(x, y, origin, lower_left_corner, horizontal, vertical)
    })
    .expect("writing image");
}
