use crate::buffer::Buffer;
use crate::color::Color;
use crate::common;
use crate::hittable::{HitRecord, Hittable};
use crate::ray::Ray;
use crate::vec3;
use rayon::prelude::*;
use crate::{camera::Camera, hittable_list::HittableList, LightList};
use crate::rayx8::Rayx8;

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
    
            let row_pixels: Vec<_> = (0..self.settings.width)
                .step_by(8)
                .into_par_iter()
                .map(|i_start| {
                    let mut colors = [Color::zero(); 8];
    
                    // Prepare batch of 8 rays
                    let mut origins = [vec3::Point3::default(); 8];
                    let mut directions = [vec3::Vec3::default(); 8];
    
                    for k in 0..8 {
                        let i = i_start + k;
                        if i >= self.settings.width {
                            continue; // padding
                        }
    
                        let u = (i as f32 + 0.5) / (self.settings.width - 1) as f32;
                        let v = (j as f32 + 0.5) / (self.settings.height - 1) as f32;
                        let ray = self.camera.get_ray(u, v);
                        origins[k] = ray.origin();
                        directions[k] = ray.direction();
                    }
    
                    // Convert to Vec3x8
                    let origin_x8 = Vec3x8::from_array(origins);
                    let direction_x8 = Vec3x8::from_array(directions);
                    let rayx8 = Rayx8::new(origin_x8, direction_x8);
    
                    // Trace all 8 rays
                    let result_x8 = ray_color_x8(&rayx8, &self.world, &self.lights, self.settings.max_depth as i32);
    
                    // Unpack Vec3x8 into colors
                    result_x8.to_array()
                })
                .flatten()
                .collect();
    
            // Write results
            for (i, pixel_color) in row_pixels.into_iter().enumerate() {
                if i < self.settings.width {
                    buffer.set_pixel(i, j, pixel_color);
                }
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

pub fn ray_color_x8(ray: &Rayx8, world: &dyn Hittable, lights: &LightList, depth: i32) -> Vec3x8 {
    use wide::CmpGt;

    if depth <= 0 {
        return Vec3x8::zero();
    }

    // Perform 8-ray intersection
    let mut recs = HitRecordx8::new();
    let hits = world.hit_x8(ray, 0.001, f32::INFINITY, &mut recs);

    // Initialize result colors to background
    let background = Vec3::new(0.5, 0.7, 1.0);
    let mut result = Vec3x8::splat(background);

    for i in 0..8 {
        if !hits[i] {
            continue;
        }

        let rec = recs.get(i);
        let emitted = rec.mat.as_ref().unwrap().emitted();
        let mut total_light = emitted;

        // === Direct Lighting ===
        for light in &lights.lights {
            let light_point = light.sample();
            let light_dir = light_point - rec.p;
            let light_distance = light_dir.length();
            let light_dir_unit = unit_vector_x8(Vec3x8::splat(light_dir));
            let shadow_ray = Rayx8::new(Vec3x8::splat(rec.p), light_dir_unit);

            let mut shadow_recs = HitRecordx8::new();
            let shadow_hits = world.hit_x8(&shadow_ray, 0.001, light_distance - 0.001, &mut shadow_recs);

            if !shadow_hits[i] {
                let cosine = f32::max(dot_x8(Vec3x8::splat(rec.normal), light_dir_unit).extract(i), 0.0);
                let light_pdf = light.pdf(rec.p, light_point);

                if let Some((_, brdf_val, brdf_pdf)) = rec.mat.as_ref().unwrap().scatter_importance(&ray.get(i), &rec) {
                    let weight = crate::common::balance_heuristic(light_pdf, brdf_pdf);
                    total_light += light.color() * brdf_val * cosine * weight / light_pdf;
                }
            }
        }

        // === BRDF Sampling ===
        if let Some((scattered, brdf_val, brdf_pdf)) = rec.mat.as_ref().unwrap().scatter_importance(&ray.get(i), &rec) {
            let cosine = f32::max(dot_x8(Vec3x8::splat(rec.normal), unit_vector_x8(Vec3x8::splat(scattered.direction()))).extract(i), 0.0);
            let mut light_hit = HitRecordx8::new();

            let single_ray = Rayx8::splat(scattered);
            let light_hits = world.hit_x8(&single_ray, 0.001, f32::INFINITY, &mut light_hit);

            if light_hits[i] {
                let light_rec = light_hit.get(i);
                let emitted = light_rec.mat.as_ref().unwrap().emitted();

                if emitted.length_squared() > 0.0 {
                    let light_pdf_sum: f32 = lights.lights.iter().map(|l| l.pdf(rec.p, light_rec.p)).sum();
                    let light_pdf = (light_pdf_sum / lights.lights.len() as f32).max(1e-4);
                    let weight = crate::common::balance_heuristic(brdf_pdf, light_pdf);
                    total_light += emitted * brdf_val * cosine * weight / brdf_pdf;
                }
            } else {
                total_light += brdf_val * ray_color_x8(&Rayx8::splat(scattered), world, lights, depth - 1).extract(i) * cosine / brdf_pdf;
            }
        }

        result.set(i, total_light);
    }

    result
}