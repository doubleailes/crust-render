mod buffer;
mod camera;
mod color;
mod common;
mod convert;
mod hittable;
mod hittable_list;
mod light;
mod material;
mod ray;
mod sphere;
mod vec3;

use camera::Camera;
use clap::Parser;
use color::Color;
use exr::prelude::*;
use hittable::{HitRecord, Hittable};
use hittable_list::HittableList;
use light::Light;
use material::{CookTorrance, Dielectric, Emissive, Lambertian, Metal};
use ray::Ray;
use rayon::prelude::*;
use sphere::Sphere;
use std::sync::Arc;
use vec3::Point3;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// The number of samples per pixel
    #[arg(short, long, default_value_t = 100)]
    samples_per_pixel: i32,

    /// The maximum depth of the ray tracing
    #[arg(short, long, default_value_t = 50)]
    max_depth: i32,
}

// Constants

const ASPECT_RATIO: f32 = 16.0 / 9.0;
const IMAGE_WIDTH: usize = 400;
const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;
const MIN_SAMPLES: i32 = 8;
const VARIANCE_THRESHOLD: f32 = 0.0005; // You can tweak this!

pub struct LightList {
    pub lights: Vec<Arc<dyn Light>>,
}

impl LightList {
    pub fn new() -> Self {
        Self { lights: Vec::new() }
    }

    pub fn add(&mut self, light: Arc<dyn Light>) {
        self.lights.push(light);
    }

    pub fn sample(&self) -> Option<&Arc<dyn Light>> {
        if self.lights.is_empty() {
            None
        } else {
            let i = (crate::common::random() * self.lights.len() as f32) as usize;
            self.lights.get(i)
        }
    }
}

fn ray_color(r: &Ray, world: &dyn Hittable, lights: &LightList, depth: i32) -> Color {
    if depth <= 0 {
        return Color::zero();
    }

    let mut rec = HitRecord::new();
    if world.hit(r, 0.001, f32::INFINITY, &mut rec) {
        let emitted = rec.mat.as_ref().unwrap().emitted();
        let mut total_light = emitted;

        // === 1. Light sampling for direct lighting ===
        for light in &lights.lights {
            let light_point = light.sample();
            let light_dir = light_point - rec.p;
            let light_distance = light_dir.length();
            let light_dir_unit = vec3::unit_vector(light_dir);

            let shadow_ray = Ray::new(rec.p, light_dir_unit);
            let mut shadow_hit = HitRecord::new();

            if !world.hit(&shadow_ray, 0.001, light_distance - 0.001, &mut shadow_hit) {
                let cosine = f32::max(vec3::dot(rec.normal, light_dir_unit), 0.0);
                let light_pdf = light.pdf(rec.p, light_point);

                if let Some((_, brdf_value, brdf_pdf)) =
                    rec.mat.as_ref().unwrap().scatter_importance(r, &rec)
                {
                    let weight = common::balance_heuristic(light_pdf, brdf_pdf);
                    total_light += light.color() * brdf_value * cosine * weight / light_pdf;
                }
            }
        }

        // === 2. BRDF sampling ===
        if let Some((scattered, brdf_value, brdf_pdf)) =
            rec.mat.as_ref().unwrap().scatter_importance(r, &rec)
        {
            let cosine = f32::max(
                vec3::dot(rec.normal, vec3::unit_vector(scattered.direction())),
                0.0,
            );

            // Check if the BRDF sample hits any light
            let mut light_hit = HitRecord::new();
            if world.hit(&scattered, 0.001, f32::INFINITY, &mut light_hit) {
                let emitted = light_hit.mat.as_ref().unwrap().emitted();

                if emitted.length_squared() > 0.0 {
                    // Compute light PDF at the hit point across all lights
                    let light_pdf_sum: f32 = lights
                        .lights
                        .iter()
                        .map(|light| light.pdf(rec.p, light_hit.p))
                        .sum();

                    let light_pdf = (light_pdf_sum / lights.lights.len() as f32).max(1e-4);
                    let weight = common::balance_heuristic(brdf_pdf, light_pdf);

                    total_light += emitted * brdf_value * cosine * weight / brdf_pdf;
                    return total_light;
                }
            }

            // If not hitting a light, keep bouncing
            total_light +=
                brdf_value * ray_color(&scattered, world, lights, depth - 1) * cosine / brdf_pdf;
        }

        return total_light;
    }

    // Background
    let unit_direction = vec3::unit_vector(r.direction());
    let t = 0.5 * (unit_direction.y() + 1.0);
    (1.0 - t) * Color::new(1.0, 1.0, 1.0) + t * Color::new(0.5, 0.7, 1.0)
}

fn random_scene() -> (HittableList, LightList) {
    let mut world = HittableList::new();
    let mut lights = LightList::new();

    let ground_material = Arc::new(Lambertian::new(Color::new(0.5, 0.5, 0.5)));
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, -1000.0, 0.0),
        1000.0,
        ground_material,
    )));

    for a in -11..11 {
        for b in -11..11 {
            let choose_mat = common::random();
            let center = Point3::new(
                a as f32 + 0.9 * common::random(),
                0.2,
                b as f32 + 0.9 * common::random(),
            );

            if (center - Point3::new(4.0, 0.2, 0.0)).length() > 0.9 {
                if choose_mat < 0.3 {
                    // Diffuse
                    let albedo = Color::random() * Color::random();
                    let sphere_material = Arc::new(Lambertian::new(albedo));
                    world.add(Box::new(Sphere::new(center, 0.2, sphere_material)));
                } else if choose_mat < 0.8 {
                    // Cook-Torrance
                    let albedo = Color::random_range(0.5, 1.0);
                    let roughness = common::random_range(0.0, 0.5);
                    let metallic = common::random_range(0.0, 1.0);
                    let sphere_material = Arc::new(CookTorrance::new(albedo, roughness, metallic));
                    world.add(Box::new(Sphere::new(center, 0.2, sphere_material)));
                } else if choose_mat < 0.95 {
                    // Metal
                    let albedo = Color::random_range(0.5, 1.0);
                    let fuzz = common::random_range(0.0, 0.5);
                    let sphere_material = Arc::new(Metal::new(albedo, fuzz));
                    world.add(Box::new(Sphere::new(center, 0.2, sphere_material)));
                } else {
                    // Glass
                    let sphere_material = Arc::new(Dielectric::new(1.5));
                    world.add(Box::new(Sphere::new(center, 0.2, sphere_material)));
                }
            }
        }
    }

    let material1 = Arc::new(Dielectric::new(1.5));
    world.add(Box::new(Sphere::new(
        Point3::new(0.0, 1.0, 0.0),
        1.0,
        material1,
    )));

    let material2 = Arc::new(Lambertian::new(Color::new(0.4, 0.2, 0.1)));
    world.add(Box::new(Sphere::new(
        Point3::new(-4.0, 1.0, 0.0),
        1.0,
        material2,
    )));

    let material3 = Arc::new(Metal::new(Color::new(0.7, 0.6, 0.5), 0.0));
    world.add(Box::new(Sphere::new(
        Point3::new(4.0, 1.0, 0.0),
        1.0,
        material3,
    )));

    let light = Arc::new(Emissive::new(
        Color::new(10.0, 10.0, 10.0),
        Point3::new(0.0, 7.0, 0.0),
        1.0,
    ));
    world.add(Box::new(Sphere::new(
        light.position(),
        light.radius(),
        light.clone(),
    )));

    lights.add(light);
    let light2 = Arc::new(Emissive::new(
        Color::new(20.0, 10.0, 7.0),
        Point3::new(-4.0, 7.0, 0.0),
        1.0,
    ));
    world.add(Box::new(Sphere::new(
        light2.position(),
        light2.radius(),
        light2.clone(),
    )));
    lights.add(light2);

    (world, lights)
}
fn main() {
    let cli = Cli::parse();
    let samples_per_pixel: i32 = cli.samples_per_pixel;
    let max_depth: i32 = cli.max_depth;
    // World

    let (world, lights) = random_scene();

    // Camera

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
    let mut buffer = buffer::Buffer::new(IMAGE_WIDTH, IMAGE_HEIGHT);
    for j in (0..IMAGE_HEIGHT).rev() {
        eprint!("\rScanlines remaining: {} ", j);
        let pixel_colors: Vec<_> = (0..IMAGE_WIDTH)
            .into_par_iter()
            .map(|i| {
                let mut sum = Color::new(0.0, 0.0, 0.0);
                let mut sum_sq = Color::new(0.0, 0.0, 0.0);
                let mut samples = 0;

                let final_color = loop {
                    let u = ((i as f32) + common::random()) / (IMAGE_WIDTH - 1) as f32;
                    let v = ((j as f32) + common::random()) / (IMAGE_HEIGHT - 1) as f32;
                    let r = cam.get_ray(u, v);
                    let col = ray_color(&r, &world, &lights, max_depth);

                    sum += col;
                    sum_sq += col * col;
                    samples += 1;

                    if samples >= MIN_SAMPLES {
                        let mean = sum / samples as f32;
                        let mean_sq = sum_sq / samples as f32;
                        let variance = mean_sq - mean * mean;

                        if variance.max_component() < VARIANCE_THRESHOLD
                            || samples >= samples_per_pixel
                        {
                            break mean; // Use `mean` as final_color and break early
                        }
                    }

                    if samples >= samples_per_pixel {
                        break sum / samples as f32;
                    }
                };

                final_color
            })
            .collect();
        for (i, pixel_color) in pixel_colors.into_iter().enumerate() {
            buffer.set_pixel(i, j, pixel_color);
        }
    }
    // Render
    write_rgb_file("output.exr", IMAGE_WIDTH, IMAGE_HEIGHT, |x, y| {
        buffer.get_rgb(x, y)
    })
    .expect("writing image");
    convert::convert();
}
