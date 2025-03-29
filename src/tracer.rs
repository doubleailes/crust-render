use crate::buffer::Buffer;
use crate::color::Color;
use crate::common;
use crate::hittable::{HitRecord, Hittable};
use crate::ray::Ray;
use crate::vec3;
use rayon::prelude::*;
use crate::{camera::Camera, hittable_list::HittableList, LightList};

pub struct Renderer {
    pub camera: Camera,
    pub world: HittableList,
    pub lights: LightList,
    pub settings: RenderSettings,
}

impl Renderer{
    pub fn new(camera: Camera, world: HittableList, lights: LightList, settings: RenderSettings) -> Self {
        Renderer {
            camera,
            world,
            lights,
            settings,
        }
    }

    pub fn render(&self) -> Buffer {
        let mut buffer = Buffer::new(self.settings.width, self.settings.height);
        for j in (0..self.settings.height).rev() {
            eprint!("\rScanlines remaining: {} ", j);
            let pixel_colors: Vec<_> = (0..self.settings.width)
                .into_par_iter()
                .map(|i| {
                    let mut sum = Color::new(0.0, 0.0, 0.0);
                    let mut sum_sq = Color::new(0.0, 0.0, 0.0);
                    let mut samples = 0;
    
                    let final_color = loop {
                        let u = ((i as f32) + common::random()) / (self.settings.width - 1) as f32;
                        let v = ((j as f32) + common::random()) / (self.settings.height - 1) as f32;
                        let r = self.camera.get_ray(u, v);
                        let col = ray_color(&r, &self.world, &self.lights, self.settings.max_depth as i32);
    
                        sum += col;
                        sum_sq += col * col;
                        samples += 1;
    
                        if samples >= self.settings.min_samples_per_pixel {
                            let mean = sum / samples as f32;
                            let mean_sq = sum_sq / samples as f32;
                            let variance = mean_sq - mean * mean;
    
                            if variance.max_component() < self.settings.variance_threshold
                                || samples >= self.settings.samples_per_pixel
                            {
                                break mean; // Use `mean` as final_color and break early
                            }
                        }
    
                        if samples >= self.settings.samples_per_pixel {
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
        buffer
    }
}

pub struct RenderSettings {
    samples_per_pixel: u32,
    max_depth: u32,
    width: usize,
    height: usize,
    min_samples_per_pixel: u32,
    variance_threshold: f32,
    
}
impl RenderSettings {
    pub fn new(
        samples_per_pixel: u32,
        max_depth: u32,
        width: usize,
        height: usize,
        min_samples_per_pixel: u32,
        variance_threshold: f32,
    ) -> Self {
        RenderSettings {
            samples_per_pixel,
            max_depth,
            width,
            height,
            min_samples_per_pixel,
            variance_threshold,
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