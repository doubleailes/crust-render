use crate::buffer::Buffer;
use crate::hittable::{HitRecord, Hittable};
use crate::ray::{self, Ray};
use crate::sampler::generate_cmj_2d;
use crate::{LightList, camera::Camera, hittable_list::HittableList};
use glam::Vec3A;
use indicatif::ProgressBar;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub struct Renderer {
    pub camera: Camera,
    pub world: HittableList,
    pub lights: LightList,
    pub settings: RenderSettings,
}

impl Renderer {
    pub fn new(
        camera: Camera,
        world: HittableList,
        lights: LightList,
        settings: RenderSettings,
    ) -> Self {
        Renderer {
            camera,
            world,
            lights,
            settings,
        }
    }

    pub fn render(&self) -> Buffer {
        let mut buffer = Buffer::new(self.settings.width, self.settings.height);
        let samples_sqrt = (self.settings.samples_per_pixel as f32).sqrt().ceil() as usize;
        let cmj_samples = generate_cmj_2d(samples_sqrt);
        let pixels_count =
            self.settings.height * self.settings.width * self.settings.samples_per_pixel as usize;
        let bar = ProgressBar::new(pixels_count as u64);
        bar.set_style(
            indicatif::ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
                )
                .unwrap(),
        );
        let mut d: Vec<(usize, usize, Ray)> = Vec::with_capacity(pixels_count);
        for j in (0..self.settings.height).rev() {
            for i in 0..self.settings.width {
                for sample in 0..self.settings.samples_per_pixel {
                    let (dx, dy) = if (sample as usize) < cmj_samples.len() {
                        cmj_samples[sample as usize]
                    } else {
                        (utils::random(), utils::random())
                    };

                    let u = (i as f32 + dx) / (self.settings.width - 1) as f32;
                    let v = (j as f32 + dy) / (self.settings.height - 1) as f32;

                    let ray = self.camera.get_ray(u, v);
                    d.push((i, j, ray));
                }
            }
        }
        let color: Vec<(usize, usize, Vec3A)> = d
            .into_par_iter()
            .map(|(i, j, r)| {
                let col: Vec3A = ray_color(
                    &r,
                    &self.world,
                    &self.lights,
                    self.settings.max_depth as i32,
                );
                bar.inc(1);
                (i, j, col)
            })
            .collect();
        for (i, j, col) in color {
            buffer.set_mut_pixel(i, j, col / self.settings.samples_per_pixel as f32);
        }
        bar.finish();
        buffer
    }

    pub fn render_with_tiles(&self) -> Buffer {
        let cmj_samples =
            generate_cmj_2d((self.settings.samples_per_pixel as f32).sqrt().ceil() as usize);
        let tiles = generate_tiles(self.settings.width, self.settings.height, 16); // tile size: 16x16
        let bar = ProgressBar::new(tiles.len() as u64);
        bar.set_style(
            indicatif::ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
                )
                .unwrap(),
        );
        // Collect all pixels in parallel from tiles
        let pixels: Vec<(usize, usize, Vec3A)> = tiles
            .into_par_iter()
            .flat_map(|tile| {
                let mut local = Vec::with_capacity(tile.width * tile.height);

                for j in tile.y..tile.y + tile.height {
                    for i in tile.x..tile.x + tile.width {
                        let mut color = Vec3A::new(0.0, 0.0, 0.0);

                        for sample in 0..self.settings.samples_per_pixel {
                            let (dx, dy) = if (sample as usize) < cmj_samples.len() {
                                cmj_samples[sample as usize]
                            } else {
                                (utils::random(), utils::random())
                            };

                            let u = (i as f32 + dx) / (self.settings.width - 1) as f32;
                            let v = (j as f32 + dy) / (self.settings.height - 1) as f32;

                            let ray = self.camera.get_ray(u, v);
                            color += ray_color(
                                &ray,
                                &self.world,
                                &self.lights,
                                self.settings.max_depth as i32,
                            );
                        }

                        let final_color = color / self.settings.samples_per_pixel as f32;
                        local.push((i, j, final_color));
                    }
                }

                bar.inc(1);
                local
            })
            .collect();
        bar.finish();
        // Combine results into a buffer
        let mut buffer = Buffer::new(self.settings.width, self.settings.height);
        for (i, j, color) in pixels {
            buffer.set_pixel(i, j, color);
        }

        buffer
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RenderSettings {
    samples_per_pixel: u32,
    max_depth: u32,
    width: usize,
    height: usize,
    min_samples_per_pixel: u32,
    variance_threshold: f32,
    frame: isize,
}
impl RenderSettings {
    pub fn new(
        samples_per_pixel: u32,
        max_depth: u32,
        width: usize,
        height: usize,
        min_samples_per_pixel: u32,
        variance_threshold: f32,
        frame: isize,
    ) -> Self {
        RenderSettings {
            samples_per_pixel,
            max_depth,
            width,
            height,
            min_samples_per_pixel,
            variance_threshold,
            frame,
        }
    }
    pub fn get_dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }
}

pub fn ray_color(r: &Ray, world: &dyn Hittable, lights: &LightList, depth: i32) -> Vec3A {
    if depth <= 0 {
        return Vec3A::new(0.0, 0.0, 0.0); // recursion limit
    }

    let mut rec = HitRecord::new();
    let cmj_samples = generate_cmj_2d(4); // Fixed number of samples for now

    if world.hit(r, 0.001, f32::INFINITY, &mut rec) {
        let emitted = rec.mat.as_ref().unwrap().emitted();
        let mut total_light = emitted;

        // === 1. Direct Lighting via Light Sampling ===
        for (light_idx, light) in lights.lights.iter().enumerate() {
            let (u, v) = cmj_samples[light_idx % cmj_samples.len()];
            let light_point = light.sample_cmj(u, v);
            let light_dir = light_point - rec.p;
            let light_distance = light_dir.length();
            let light_dir_unit = light_dir.normalize();

            let shadow_ray = Ray::new(rec.p, light_dir_unit);
            let mut shadow_hit = HitRecord::new();

            if !world.hit(&shadow_ray, 0.001, light_distance - 0.001, &mut shadow_hit) {
                let cosine = f32::max(rec.normal.dot(light_dir_unit), 0.0);
                let light_pdf = light.pdf(rec.p, light_point);

                if let Some((_, brdf_value, brdf_pdf)) =
                    rec.mat.as_ref().unwrap().scatter_importance(r, &rec)
                {
                    let weight = utils::balance_heuristic(light_pdf, brdf_pdf);
                    total_light += light.color() * brdf_value * cosine * weight / light_pdf;
                }
            }
        }

        // === 2. Indirect Lighting via BRDF Sampling ===
        if let Some((scattered, brdf_value, brdf_pdf)) =
            rec.mat.as_ref().unwrap().scatter_importance(r, &rec)
        {
            let cosine = f32::max(rec.normal.dot(scattered.direction().normalize()), 0.0);

            let mut light_hit = HitRecord::new();
            let mut add_emission = Vec3A::new(0.0, 0.0, 0.0);

            if world.hit(&scattered, 0.001, f32::INFINITY, &mut light_hit) {
                let emitted = light_hit.mat.as_ref().unwrap().emitted();
                if emitted.length_squared() > 0.0 {
                    let light_pdf_sum: f32 = lights
                        .lights
                        .iter()
                        .map(|light| light.pdf(rec.p, light_hit.p))
                        .sum();
                    let light_pdf = (light_pdf_sum / lights.lights.len() as f32).max(1e-4);
                    let weight = utils::balance_heuristic(brdf_pdf, light_pdf);

                    // Add the contribution of hitting the light via BRDF
                    add_emission = emitted * brdf_value * cosine * weight / brdf_pdf;
                }
            }

            // Add both direct hit on light and recursive bounce
            total_light += add_emission;
            total_light +=
                brdf_value * ray_color(&scattered, world, lights, depth - 1) * cosine / brdf_pdf;
        }

        return total_light;
    }

    // === Background ===
    let unit_direction = Vec3A::normalize(r.direction());
    let t = 0.5 * (unit_direction.y + 1.0);
    (1.0 - t) * Vec3A::new(1.0, 1.0, 1.0) + t * Vec3A::new(0.5, 0.7, 1.0)
}

struct Tile {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

fn generate_tiles(image_width: usize, image_height: usize, tile_size: usize) -> Vec<Tile> {
    let mut tiles = Vec::new();
    for y in (0..image_height).step_by(tile_size) {
        for x in (0..image_width).step_by(tile_size) {
            let w = (x + tile_size).min(image_width) - x;
            let h = (y + tile_size).min(image_height) - y;
            tiles.push(Tile {
                x,
                y,
                width: w,
                height: h,
            });
        }
    }
    tiles
}
