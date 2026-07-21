use crate::buffer::Buffer;
use crate::guiding::{GuidingConfig, GuidingField, SampleData, luminance};
use crate::hittable::{HitRecord, Hittable};
use crate::medium::sample_henyey_greenstein;
use crate::ray::Ray;
use crate::{LightList, camera::Camera, hittable_list::HittableList};
use glam::Vec3A;
use indicatif::ProgressBar;
use rayon::prelude::*;
use sampler::{Sampler, SobolSampler};
use tracing::{info, warn};

/// Training-only clamp on recorded radiance so a single firefly cannot
/// dominate a directional distribution. Affects the guiding field, never the
/// image estimator.
const TRAIN_RADIANCE_CLAMP: f32 = 1e3;

/// Per-pass guiding state handed down the integrator recursion.
struct GuidingContext<'a> {
    field: &'a GuidingField,
    /// Record `SampleData` for field training during this pass?
    training: bool,
}

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
        if self.settings.guiding {
            return self.render_guided(false);
        }
        self.render_pass(
            self.settings.samples_per_pixel,
            self.settings.frame as u32,
            false,
            None,
            true,
        )
        .0
    }

    pub fn render_with_tiles(&self) -> Buffer {
        if self.settings.guiding {
            return self.render_guided(true);
        }
        self.render_pass(
            self.settings.samples_per_pixel,
            self.settings.frame as u32,
            true,
            None,
            true,
        )
        .0
    }

    /// Progressive path-guided rendering: training passes with geometrically
    /// growing sample budgets (1, 2, 4, … spp) build the guiding field, then
    /// the full per-pixel budget renders the final image with the frozen
    /// field. The final image comes from the final pass alone.
    fn render_guided(&self, tiled: bool) -> Buffer {
        let bounds = match self.world.bounding_box() {
            Some(b) => b,
            None => {
                warn!("path guiding enabled but the scene has no bounding box; rendering unguided");
                return self
                    .render_pass(
                        self.settings.samples_per_pixel,
                        self.settings.frame as u32,
                        tiled,
                        None,
                        true,
                    )
                    .0;
            }
        };
        let cfg = GuidingConfig {
            train_iterations: self.settings.guiding_train_iterations,
            guide_prob: self.settings.guiding_prob,
            ..GuidingConfig::default()
        };
        let mut field = GuidingField::new(bounds, cfg);
        let base_seed = self.settings.frame as u32;

        for k in 0..cfg.train_iterations {
            let spp = 1u32 << k.min(16);
            // Decorrelate the Sobol sequences between passes.
            let seed = base_seed.wrapping_add((k + 1).wrapping_mul(0x9E37_79B9));
            info!(
                "path guiding: training pass {}/{} at {} spp",
                k + 1,
                cfg.train_iterations,
                spp
            );
            let gctx = GuidingContext {
                field: &field,
                training: true,
            };
            let (_, samples) = self.render_pass(spp, seed, tiled, Some(&gctx), false);
            drop(gctx);
            info!("path guiding: splatting {} samples", samples.len());
            field.update(&samples, k + 1);
        }

        info!(
            "path guiding: final pass at {} spp",
            self.settings.samples_per_pixel
        );
        let gctx = GuidingContext {
            field: &field,
            training: false,
        };
        self.render_pass(
            self.settings.samples_per_pixel,
            base_seed,
            tiled,
            Some(&gctx),
            true,
        )
        .0
    }

    /// One full-frame pass at `spp` samples per pixel. Returns the image and
    /// whatever training samples the pass recorded (empty unless a training
    /// `GuidingContext` is supplied).
    fn render_pass(
        &self,
        spp: u32,
        pass_seed: u32,
        tiled: bool,
        gctx: Option<&GuidingContext>,
        progress: bool,
    ) -> (Buffer, Vec<SampleData>) {
        let mut buffer = Buffer::new(self.settings.width, self.settings.height);
        let mut all_samples = Vec::new();

        if tiled {
            let tiles = generate_tiles(self.settings.width, self.settings.height, 16); // tile size: 16x16
            let bar = progress_bar(tiles.len() as u64, progress);
            let results: Vec<(Vec<(usize, usize, Vec3A)>, Vec<SampleData>)> = tiles
                .into_par_iter()
                .map(|tile| {
                    let mut pixels = Vec::with_capacity(tile.width * tile.height);
                    let mut samples = Vec::new();
                    for j in tile.y..tile.y + tile.height {
                        for i in tile.x..tile.x + tile.width {
                            let (color, mut s) = self.render_pixel(i, j, spp, pass_seed, gctx);
                            pixels.push((i, j, color));
                            samples.append(&mut s);
                        }
                    }
                    bar.inc(1);
                    (pixels, samples)
                })
                .collect();
            for (pixels, samples) in results {
                for (i, j, color) in pixels {
                    buffer.set_pixel(i, j, color);
                }
                all_samples.extend(samples);
            }
            bar.finish();
        } else {
            let bar = progress_bar(self.settings.height as u64, progress);
            for j in (0..self.settings.height).rev() {
                let row: Vec<(Vec3A, Vec<SampleData>)> = (0..self.settings.width)
                    .into_par_iter()
                    .map(|i| self.render_pixel(i, j, spp, pass_seed, gctx))
                    .collect();
                for (i, (color, samples)) in row.into_iter().enumerate() {
                    buffer.set_pixel(i, j, color);
                    all_samples.extend(samples);
                }
                bar.inc(1);
            }
            bar.finish();
        }

        (buffer, all_samples)
    }

    fn render_pixel(
        &self,
        i: usize,
        j: usize,
        spp: u32,
        pass_seed: u32,
        gctx: Option<&GuidingContext>,
    ) -> (Vec3A, Vec<SampleData>) {
        let mut sampler = SobolSampler::new(pass_seed);
        sampler.start_pixel(i as u32, j as u32);
        let mut sum = Vec3A::ZERO;
        let mut samples = Vec::new();

        for sample in 0..spp {
            sampler.start_sample(sample);
            let jitter = sampler.next_2d();
            let lens_uv = sampler.next_2d();
            let u = ((i as f32) + jitter[0]) / (self.settings.width - 1) as f32;
            let v = ((j as f32) + jitter[1]) / (self.settings.height - 1) as f32;
            let r = self.camera.get_ray(u, v, lens_uv);
            sum += ray_color_inner(
                &r,
                &self.world,
                &self.lights,
                self.settings.max_depth as i32,
                &mut sampler,
                gctx,
                &mut samples,
                false,
            );
        }
        (sum / spp as f32, samples)
    }
}

fn progress_bar(len: u64, visible: bool) -> ProgressBar {
    if !visible {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::new(len);
    bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .unwrap(),
    );
    bar
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
    // Path guiding (opt-in via `crust:pathGuiding`; see `with_guiding`).
    guiding: bool,
    guiding_train_iterations: u32,
    guiding_prob: f32,
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
            guiding: false,
            guiding_train_iterations: 4,
            guiding_prob: 0.5,
        }
    }

    /// Enable (or disable) path guiding with the given number of training
    /// iterations and guide-sampling probability α.
    pub fn with_guiding(mut self, enabled: bool, train_iterations: u32, guide_prob: f32) -> Self {
        self.guiding = enabled;
        self.guiding_train_iterations = train_iterations.max(1);
        self.guiding_prob = guide_prob.clamp(0.1, 0.9);
        self
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
    let mut no_training = Vec::new();
    ray_color_inner(r, world, lights, depth, sampler, None, &mut no_training, false)
}

/// Choose the bounce direction and the pdf its contribution is divided by.
///
/// With guiding this is one-sample MIS between the guiding distribution and
/// BSDF sampling: pick the guide with probability α, the BSDF otherwise, and
/// divide by the mixture pdf `α·p_guide + (1-α)·p_bsdf`. The mixture pdf is
/// used iff guiding was *available* at this vertex — the local distribution
/// is trained and the material is evaluable — independent of which branch
/// the coin picked; `Material::eval`'s contract (evaluability never depends
/// on the queried direction) is what keeps the two branches consistent, and
/// hence the estimator unbiased.
fn sample_bounce_direction(
    r: &Ray,
    rec: &HitRecord,
    guiding: Option<&GuidingContext>,
    sampler: &mut dyn Sampler,
) -> Option<(Ray, Vec3A, f32, bool)> {
    let mat = rec.mat.as_ref().unwrap();

    let g = match guiding {
        Some(g) if g.field.trained_at(rec.p) => g,
        _ => {
            let (scattered, value, pdf) = mat.scatter_importance(r, rec, sampler)?;
            let evaluable = mat
                .eval(r, rec, scattered.direction().normalize())
                .is_some();
            return Some((scattered, value, pdf, evaluable));
        }
    };
    let alpha = g.field.config().guide_prob;

    if sampler.next_1d() < alpha {
        // Guide branch. A trained field always yields a sample; delta and
        // transmissive materials (eval == None) cannot use it, so they drop
        // to the pure-BSDF estimator — exactly as they do in the BSDF branch.
        if let Some((wi, p_guide)) = g.field.sample(rec.p, sampler) {
            if let Some((value, p_bsdf)) = mat.eval(r, rec, wi) {
                let pdf = (alpha * p_guide + (1.0 - alpha) * p_bsdf).max(1e-4);
                return Some((Ray::new(rec.p, wi), value, pdf, true));
            }
        }
        let (scattered, value, pdf) = mat.scatter_importance(r, rec, sampler)?;
        Some((scattered, value, pdf, false))
    } else {
        // BSDF branch.
        let (scattered, value, p_bsdf) = mat.scatter_importance(r, rec, sampler)?;
        let wi = scattered.direction().normalize();
        if mat.eval(r, rec, wi).is_some() {
            let p_guide = g.field.pdf(rec.p, wi);
            let pdf = (alpha * p_guide + (1.0 - alpha) * p_bsdf).max(1e-4);
            Some((scattered, value, pdf, true))
        } else {
            Some((scattered, value, p_bsdf, false))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ray_color_inner(
    r: &Ray,
    world: &dyn Hittable,
    lights: &LightList,
    depth: i32,
    sampler: &mut dyn Sampler,
    guiding: Option<&GuidingContext>,
    train_out: &mut Vec<SampleData>,
    // The previous bounce already accounted for this vertex's emission via
    // its MIS-weighted `add_emission` term; adding it again here would count
    // bounce-hit emission twice.
    suppress_emission: bool,
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
                    // Volume scattering events are not recorded — the field
                    // guides surface bounces only. No emission bookkeeping
                    // happened along this phase-scattered ray, so the next
                    // hit's emission must count.
                    return albedo
                        * ray_color_inner(
                            &new_ray,
                            world,
                            lights,
                            depth - 1,
                            sampler,
                            guiding,
                            train_out,
                            false,
                        );
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
        let emitted = if suppress_emission {
            Vec3A::ZERO
        } else {
            rec.mat.as_ref().unwrap().emitted()
        };
        let mut total_light = emitted;

        // Guide secondary bounces only: primary vertices vary per pixel far
        // below the guiding field's spatial resolution, so guiding them adds
        // parallax-mismatch variance instead of removing any.
        let guiding_here = if suppress_emission { guiding } else { None };

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
                    // The competing strategy for this MIS weight is the
                    // bounce sampler, whose density toward the light is the
                    // guide/BSDF mixture whenever guiding is available at
                    // this vertex — using the plain BSDF pdf here while the
                    // bounce side weights with the mixture makes the two
                    // weights sum past one and double-counts emission.
                    let bounce_pdf = match guiding_here {
                        Some(g) if g.field.trained_at(rec.p) => {
                            let alpha = g.field.config().guide_prob;
                            alpha * g.field.pdf(rec.p, light_dir_unit)
                                + (1.0 - alpha) * brdf_pdf
                        }
                        _ => brdf_pdf,
                    };
                    let weight = utils::balance_heuristic(light_pdf, bounce_pdf);
                    total_light += light.color() * brdf_value * cosine * weight / light_pdf;
                }
            }
        }

        // === 2. Indirect Lighting via BSDF (or guided) Sampling ===
        if let Some((scattered, brdf_value, brdf_pdf, nee_capable)) =
            sample_bounce_direction(r, &rec, guiding_here, sampler)
        {
            let cosine = f32::max(rec.normal.dot(scattered.direction().normalize()), 0.0);

            let mut light_hit = HitRecord::new();
            let mut add_emission = Vec3A::new(0.0, 0.0, 0.0);
            let mut hit_emission = Vec3A::ZERO;

            if world.hit(&scattered, 0.001, f32::INFINITY, &mut light_hit) {
                let emitted = light_hit.mat.as_ref().unwrap().emitted();
                if emitted.length_squared() > 0.0 {
                    hit_emission = emitted;
                    // NEE-capable vertices share the emission between light
                    // sampling and the bounce via MIS; delta/transmissive
                    // vertices skipped NEE, so the bounce carries it whole.
                    let weight = if nee_capable {
                        let light_pdf_sum: f32 = lights
                            .lights
                            .iter()
                            .map(|light| light.pdf(rec.p, light_hit.p))
                            .sum();
                        let light_pdf = (light_pdf_sum / lights.lights.len() as f32).max(1e-4);
                        utils::balance_heuristic(brdf_pdf, light_pdf)
                    } else {
                        1.0
                    };

                    // Add the contribution of hitting the light via BRDF
                    add_emission = emitted * brdf_value * cosine * weight / brdf_pdf;
                }
            }

            // The recursion must not count the next vertex's emission again —
            // `add_emission` above owns that term.
            let incident = ray_color_inner(
                &scattered,
                world,
                lights,
                depth - 1,
                sampler,
                guiding,
                train_out,
                true,
            );

            // Record a training sample for the guiding field: the full
            // incident radiance (reflected + the suppressed hit emission),
            // weighted by cos² to match this tracer's estimator, which
            // multiplies the codebase's brdf*cos material values by the
            // cosine again.
            if let Some(g) = guiding {
                if g.training {
                    let dir = scattered.direction().normalize();
                    let cos = rec.normal.dot(dir).max(0.0);
                    train_out.push(SampleData {
                        pos: rec.p,
                        dir,
                        radiance: (luminance(incident + hit_emission) * cos * cos)
                            .min(TRAIN_RADIANCE_CLAMP),
                    });
                }
            }

            // Add both direct hit on light and recursive bounce
            total_light += add_emission;
            total_light += brdf_value * incident * cosine / brdf_pdf;
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
