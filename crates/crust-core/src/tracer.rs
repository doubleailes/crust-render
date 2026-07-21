use crate::buffer::Buffer;
use crate::hittable::{HitRecord, Hittable};
use crate::medium::sample_henyey_greenstein;
use crate::ray::Ray;
use crate::{LightList, camera::Camera, hittable_list::HittableList};
use glam::Vec3A;
use indicatif::ProgressBar;
use rayon::prelude::*;
use sampler::{Sampler, SobolSampler};

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
        let bar = ProgressBar::new(self.settings.height as u64);
        bar.set_style(
            indicatif::ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
                )
                .unwrap(),
        );
        let frame_seed = self.settings.frame as u32;
        for j in (0..self.settings.height).rev() {
            let pixel_colors: Vec<_> = (0..self.settings.width)
                .into_par_iter()
                .map(|i| {
                    let mut sampler = SobolSampler::new(frame_seed);
                    sampler.start_pixel(i as u32, j as u32);
                    let mut sum = Vec3A::new(0.0, 0.0, 0.0);

                    for sample in 0..self.settings.samples_per_pixel {
                        sampler.start_sample(sample);
                        let jitter = sampler.next_2d();
                        let lens_uv = sampler.next_2d();
                        let u = ((i as f32) + jitter[0]) / (self.settings.width - 1) as f32;
                        let v = ((j as f32) + jitter[1]) / (self.settings.height - 1) as f32;
                        let r = self.camera.get_ray(u, v, lens_uv);
                        let col = ray_color(
                            &r,
                            &self.world,
                            &self.lights,
                            self.settings.max_depth as i32,
                            &mut sampler,
                        );

                        sum += col;
                    }
                    sum / self.settings.samples_per_pixel as f32
                })
                .collect();
            for (i, pixel_color) in pixel_colors.into_iter().enumerate() {
                buffer.set_pixel(i, j, pixel_color);
            }
            bar.inc(1);
        }
        bar.finish();
        buffer
    }

    pub fn render_with_tiles(&self) -> Buffer {
        let tiles = generate_tiles(self.settings.width, self.settings.height, 16); // tile size: 16x16
        let bar = ProgressBar::new(tiles.len() as u64);
        bar.set_style(
            indicatif::ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
                )
                .unwrap(),
        );
        let frame_seed = self.settings.frame as u32;
        // Collect all pixels in parallel from tiles
        let pixels: Vec<(usize, usize, Vec3A)> = tiles
            .into_par_iter()
            .flat_map(|tile| {
                let mut local = Vec::with_capacity(tile.width * tile.height);

                for j in tile.y..tile.y + tile.height {
                    for i in tile.x..tile.x + tile.width {
                        let mut sampler = SobolSampler::new(frame_seed);
                        sampler.start_pixel(i as u32, j as u32);
                        let mut color = Vec3A::new(0.0, 0.0, 0.0);

                        for sample in 0..self.settings.samples_per_pixel {
                            sampler.start_sample(sample);
                            let jitter = sampler.next_2d();
                            let lens_uv = sampler.next_2d();

                            let u = (i as f32 + jitter[0]) / (self.settings.width - 1) as f32;
                            let v = (j as f32 + jitter[1]) / (self.settings.height - 1) as f32;

                            let ray = self.camera.get_ray(u, v, lens_uv);
                            color += ray_color(
                                &ray,
                                &self.world,
                                &self.lights,
                                self.settings.max_depth as i32,
                                &mut sampler,
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

#[derive(Debug, Clone, Copy)]
pub struct RenderSettings {
    samples_per_pixel: u32,
    max_depth: u32,
    width: usize,
    height: usize,
    // Adaptive sampling + animation controls parsed from USD but not yet
    // consumed by the tracer. Kept in the struct so the plumbing exists.
    #[allow(dead_code)]
    min_samples_per_pixel: u32,
    #[allow(dead_code)]
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

pub fn ray_color(
    r: &Ray,
    world: &dyn Hittable,
    lights: &LightList,
    depth: i32,
    sampler: &mut dyn Sampler,
) -> Vec3A {
    if depth <= 0 {
        return Vec3A::new(0.0, 0.0, 0.0); // recursion limit
    }

    // Fresh dimension window for this bounce — a no-op today, a hook for
    // padded-Sobol later.
    sampler.advance_bounce();

    let mut rec = HitRecord::new();

    if world.hit(r, 0.001, f32::INFINITY, &mut rec) {
        // Volume interaction sampling for scattering media (subsurface,
        // participating volumes). If the sampled distance is closer than
        // the surface hit, kick a scattering event and short-circuit the
        // surface interaction.
        if let Some(medium) = r.medium() {
            if medium.is_scattering() {
                let sigma_t_max = medium.sigma_t_max().max(1e-4);
                let t_scatter = -(sampler.next_1d().ln()) / sigma_t_max;
                if t_scatter < rec.t {
                    let pos = r.at(t_scatter);
                    let phase_uv = sampler.next_2d();
                    let dir = sample_henyey_greenstein(
                        r.direction().normalize(),
                        medium.g,
                        phase_uv[0],
                        phase_uv[1],
                    );
                    let new_ray = Ray::new_in_medium(pos, dir, medium.clone());
                    let albedo = medium.albedo();
                    return albedo * ray_color(&new_ray, world, lights, depth - 1, sampler);
                }
            }
        }

        // Beer-Lambert attenuation across the segment travelled inside a
        // participating medium (transmissive OpenPBR surfaces mark rays with
        // `Some(medium)` on refraction; free-space rays are unaffected).
        let medium_atten = match r.medium() {
            Some(m) => m.transmittance(rec.t),
            None => Vec3A::ONE,
        };
        let emitted = rec.mat.as_ref().unwrap().emitted();
        let mut total_light = emitted;

        // === 1. Direct Lighting via Light Sampling ===
        for light in lights.lights.iter() {
            let area_uv = sampler.next_2d();
            let light_point = light.sample_cmj(area_uv[0], area_uv[1]);
            let light_dir = light_point - rec.p;
            let light_distance = light_dir.length();
            let light_dir_unit = light_dir.normalize();

            let shadow_ray = Ray::new(rec.p, light_dir_unit);
            let mut shadow_hit = HitRecord::new();

            if !world.hit(&shadow_ray, 0.001, light_distance - 0.001, &mut shadow_hit) {
                let cosine = f32::max(rec.normal.dot(light_dir_unit), 0.0);
                let light_pdf = light.pdf(rec.p, light_point);

                // Evaluate the BSDF toward the light direction. Delta and
                // transmissive materials return None — they cannot see a
                // light-sampled direction and pick up emission via BSDF
                // sampling instead.
                if let Some((brdf_value, brdf_pdf)) =
                    rec.mat.as_ref().unwrap().eval(r, &rec, light_dir_unit)
                {
                    let weight = utils::balance_heuristic(light_pdf, brdf_pdf);
                    total_light += light.color() * brdf_value * cosine * weight / light_pdf;
                }
            }
        }

        // === 2. Indirect Lighting via BRDF Sampling ===
        if let Some((scattered, brdf_value, brdf_pdf)) =
            rec.mat.as_ref().unwrap().scatter_importance(r, &rec, sampler)
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
            total_light += brdf_value
                * ray_color(&scattered, world, lights, depth - 1, sampler)
                * cosine
                / brdf_pdf;
        }

        return total_light * medium_atten;
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
