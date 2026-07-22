use crate::buffer::Buffer;
use crate::bvh::Bvh;
use crate::guiding::{GuidingConfig, GuidingField, SampleData, luminance};
use crate::hittable::{HitRecord, Hittable};
use crate::material::{Material, ScatterSample};
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

/// Russian roulette: paths may terminate stochastically once they carry at
/// least this many vertices; the survival probability tracks the path
/// throughput but never drops below the floor, so weights stay bounded.
const RR_START_BOUNCE: usize = 3;
const RR_MIN_PROB: f32 = 0.05;

/// Per-pass guiding state handed down the integrator.
struct GuidingContext<'a> {
    field: &'a GuidingField,
    /// Record `SampleData` for field training during this pass?
    training: bool,
}

/// Parameters of one full-frame render pass.
#[derive(Clone, Copy)]
struct PassConfig {
    spp: u32,
    seed: u32,
    tiled: bool,
    adaptive: bool,
    progress: bool,
}

pub struct Renderer {
    pub camera: Camera,
    /// Top-level BVH over every scene object (meshes carry their own
    /// nested BVH over triangles, built at import).
    pub world: Bvh,
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
        let object_count = world.count();
        let world = Bvh::new(world.into_objects());
        info!("built top-level BVH over {} scene objects", object_count);
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
        self.render_pass(self.final_pass_config(false), None).0
    }

    pub fn render_with_tiles(&self) -> Buffer {
        if self.settings.guiding {
            return self.render_guided(true);
        }
        self.render_pass(self.final_pass_config(true), None).0
    }

    /// Config of a final (image-quality) pass: full budget, adaptive
    /// sampling, visible progress.
    fn final_pass_config(&self, tiled: bool) -> PassConfig {
        PassConfig {
            spp: self.settings.samples_per_pixel,
            seed: self.settings.frame as u32,
            tiled,
            adaptive: true,
            progress: true,
        }
    }

    /// Progressive path-guided rendering: training passes with geometrically
    /// growing sample budgets (1, 2, 4, … spp) build the guiding field, then
    /// the full per-pixel budget renders with the frozen field. Every pass is
    /// an unbiased image of the same scene, so instead of discarding the
    /// training passes the final image blends all of them weighted by
    /// inverse variance — passes rendered before the field converged simply
    /// receive small weights.
    fn render_guided(&self, tiled: bool) -> Buffer {
        let bounds = match self.world.bounding_box() {
            Some(b) => b,
            None => {
                warn!("path guiding enabled but the scene has no bounding box; rendering unguided");
                return self.render_pass(self.final_pass_config(tiled), None).0;
            }
        };
        let cfg = GuidingConfig {
            train_iterations: self.settings.guiding_train_iterations,
            guide_prob: self.settings.guiding_prob,
            ..GuidingConfig::default()
        };
        let mut field = GuidingField::new(bounds, cfg);
        let base_seed = self.settings.frame as u32;
        let mut passes: Vec<(Buffer, f64)> = Vec::new();

        for k in 0..cfg.train_iterations {
            let spp = 1u32 << k.min(16);
            // Decorrelate the Sobol sequences between passes.
            let seed = base_seed.wrapping_add((k + 1).wrapping_mul(0x9E37_79B9));
            let gctx = GuidingContext {
                field: &field,
                training: true,
            };
            let train_cfg = PassConfig {
                spp,
                seed,
                tiled,
                adaptive: false,
                progress: false,
            };
            let (buffer, samples, variance) = self.render_pass(train_cfg, Some(&gctx));
            drop(gctx);
            info!(
                "path guiding: training pass {}/{} at {} spp — {} samples, variance {:.3e}",
                k + 1,
                cfg.train_iterations,
                spp,
                samples.len(),
                variance
            );
            field.update(&samples, k + 1);
            passes.push((buffer, variance));
        }

        info!(
            "path guiding: final pass at {} spp",
            self.settings.samples_per_pixel
        );
        let gctx = GuidingContext {
            field: &field,
            training: false,
        };
        let (final_buffer, _, final_variance) =
            self.render_pass(self.final_pass_config(tiled), Some(&gctx));
        passes.push((final_buffer, final_variance));

        self.blend_passes(passes)
    }

    /// Inverse-variance blend of independent unbiased passes. Passes whose
    /// variance could not be estimated (spp < 2) get zero weight; if nothing
    /// is weightable, the last (final) pass is returned as-is.
    fn blend_passes(&self, mut passes: Vec<(Buffer, f64)>) -> Buffer {
        let weights: Vec<f64> = passes
            .iter()
            .map(|(_, var)| {
                if var.is_finite() && *var > 0.0 {
                    1.0 / var
                } else {
                    0.0
                }
            })
            .collect();
        let total: f64 = weights.iter().sum();
        if total <= 0.0 {
            return passes.pop().expect("at least the final pass exists").0;
        }
        info!(
            "path guiding: blending {} passes, weight shares {:?}",
            passes.len(),
            weights
                .iter()
                .map(|w| (w / total * 100.0).round() as i32)
                .collect::<Vec<_>>()
        );
        let (width, height) = (self.settings.width, self.settings.height);
        let mut out = Buffer::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let mut c = Vec3A::ZERO;
                for (pass, w) in passes.iter().zip(&weights) {
                    c += pass.0.get_pixel(x, y) * (*w / total) as f32;
                }
                out.set_pixel(x, y, c);
            }
        }
        out
    }

    /// One full-frame pass at `spp` samples per pixel. Returns the image,
    /// whatever training samples the pass recorded (empty unless a training
    /// `GuidingContext` is supplied), and the pass's mean per-pixel variance
    /// of the pixel estimate — the inverse-variance blending weight
    /// (`f64::INFINITY` when spp < 2 makes estimation impossible).
    fn render_pass(
        &self,
        cfg: PassConfig,
        gctx: Option<&GuidingContext>,
    ) -> (Buffer, Vec<SampleData>, f64) {
        let mut buffer = Buffer::new(self.settings.width, self.settings.height);
        let mut all_samples = Vec::new();
        let mut variance_sum = 0.0f64;
        let pixel_count = (self.settings.width * self.settings.height) as f64;

        if cfg.tiled {
            let tiles = generate_tiles(self.settings.width, self.settings.height, 16); // tile size: 16x16
            let bar = progress_bar(tiles.len() as u64, cfg.progress);
            let results: Vec<(Vec<(usize, usize, Vec3A)>, Vec<SampleData>, f64)> = tiles
                .into_par_iter()
                .map(|tile| {
                    let mut pixels = Vec::with_capacity(tile.width * tile.height);
                    let mut samples = Vec::new();
                    let mut var = 0.0f64;
                    for j in tile.y..tile.y + tile.height {
                        for i in tile.x..tile.x + tile.width {
                            let (color, mut s, v) = self.render_pixel(i, j, &cfg, gctx);
                            pixels.push((i, j, color));
                            samples.append(&mut s);
                            var += v;
                        }
                    }
                    bar.inc(1);
                    (pixels, samples, var)
                })
                .collect();
            for (pixels, samples, var) in results {
                for (i, j, color) in pixels {
                    buffer.set_pixel(i, j, color);
                }
                all_samples.extend(samples);
                variance_sum += var;
            }
            bar.finish();
        } else {
            let bar = progress_bar(self.settings.height as u64, cfg.progress);
            for j in (0..self.settings.height).rev() {
                let row: Vec<(Vec3A, Vec<SampleData>, f64)> = (0..self.settings.width)
                    .into_par_iter()
                    .map(|i| self.render_pixel(i, j, &cfg, gctx))
                    .collect();
                for (i, (color, samples, var)) in row.into_iter().enumerate() {
                    buffer.set_pixel(i, j, color);
                    all_samples.extend(samples);
                    variance_sum += var;
                }
                bar.inc(1);
            }
            bar.finish();
        }

        (buffer, all_samples, variance_sum / pixel_count)
    }

    fn render_pixel(
        &self,
        i: usize,
        j: usize,
        cfg: &PassConfig,
        gctx: Option<&GuidingContext>,
    ) -> (Vec3A, Vec<SampleData>, f64) {
        let mut sampler = SobolSampler::new(cfg.seed);
        sampler.start_pixel(i as u32, j as u32);
        let mut sum = Vec3A::ZERO;
        let mut samples = Vec::new();
        let mut lum_sum = 0.0f64;
        let mut lum_sq = 0.0f64;

        let threshold = self.settings.variance_threshold as f64;
        let min_spp = self.settings.min_samples_per_pixel.max(2);
        let mut taken = 0u32;

        for sample in 0..cfg.spp {
            sampler.start_sample(sample);
            let jitter = sampler.next_2d();
            let lens_uv = sampler.next_2d();
            let u = ((i as f32) + jitter[0]) / (self.settings.width - 1) as f32;
            let v = ((j as f32) + jitter[1]) / (self.settings.height - 1) as f32;
            let r = self.camera.get_ray(u, v, lens_uv);
            let color = trace_path(
                &r,
                &self.world,
                &self.lights,
                self.settings.max_depth as i32,
                &mut sampler,
                gctx,
                &mut samples,
            );
            sum += color;
            let lum = luminance(color) as f64;
            lum_sum += lum;
            lum_sq += lum * lum;
            taken = sample + 1;

            // Adaptive early stop: once past the minimum budget, quit as soon
            // as the relative standard error of the pixel mean is below the
            // threshold. Checked every 4th sample to amortize the cost.
            if cfg.adaptive && threshold > 0.0 && taken >= min_spp && taken % 4 == 0 {
                let n = taken as f64;
                let var_of_mean =
                    ((lum_sq - lum_sum * lum_sum / n) / (n - 1.0) / n).max(0.0);
                let mean = (lum_sum / n).max(1e-4);
                if var_of_mean.sqrt() / mean < threshold {
                    break;
                }
            }
        }

        // Unbiased variance of the pixel-mean luminance.
        let n = taken as f64;
        let variance = if taken >= 2 {
            ((lum_sq - lum_sum * lum_sum / n) / (n - 1.0) / n).max(0.0)
        } else {
            f64::INFINITY
        };
        (sum / taken as f32, samples, variance)
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
    // Adaptive sampling: a pixel may stop early once it has taken at least
    // `min_samples_per_pixel` samples and the relative standard error of its
    // mean drops below `variance_threshold` (0 disables early stopping).
    min_samples_per_pixel: u32,
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

    /// Override the samples-per-pixel count (e.g. from a CLI flag). Clamped to >= 1.
    pub fn with_samples_per_pixel(mut self, spp: u32) -> Self {
        self.samples_per_pixel = spp.max(1);
        self
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
    trace_path(r, world, lights, depth, sampler, None, &mut no_training)
}

/// Choose the bounce direction and the pdf its contribution is divided by.
///
/// With guiding this is one-sample MIS between the guiding distribution and
/// the material's *continuous* component: pick the guide with probability α,
/// the BSDF otherwise, and divide continuous samples by the mixture pdf
/// `α·p_guide + (1-α)·p_bsdf` — used iff guiding is available at the vertex
/// (trained field + evaluable material), independent of which branch the
/// coin picked. Delta samples (transmission) are a singular component the
/// guide can never produce: they keep their placeholder pdf, are never mixed
/// with a continuous density, and their value is divided by `1-α` to
/// compensate for the coin reducing the delta lobe's selection probability.
fn sample_bounce_direction(
    r: &Ray,
    rec: &HitRecord,
    mat: &dyn Material,
    guiding: Option<&GuidingContext>,
    sampler: &mut dyn Sampler,
) -> Option<ScatterSample> {
    let g = match guiding {
        Some(g) if g.field.trained_at(rec.p) => g,
        _ => return mat.scatter_importance(r, rec, sampler),
    };
    let alpha = g.field.config().guide_prob;

    if sampler.next_1d() < alpha {
        // Guide branch: draw from the field; the material's continuous
        // component supplies the value and the BSDF side of the mixture pdf.
        if let Some((wi, p_guide)) = g.field.sample(rec.p, sampler) {
            if let Some((value, p_bsdf)) = mat.eval(r, rec, wi) {
                let pdf = (alpha * p_guide + (1.0 - alpha) * p_bsdf).max(1e-4);
                return Some(ScatterSample {
                    ray: mat.make_ray(rec, wi),
                    value,
                    pdf,
                    delta: false,
                });
            }
        }
        // Material with no continuous component: pure BSDF sampling.
        mat.scatter_importance(r, rec, sampler)
    } else {
        // BSDF branch.
        let mut sample = mat.scatter_importance(r, rec, sampler)?;
        if sample.delta {
            // Only this branch can reach the delta lobe, so the coin scaled
            // its selection probability by 1-α.
            sample.value /= 1.0 - alpha;
            return Some(sample);
        }
        let wi = sample.ray.direction().normalize();
        if mat.eval(r, rec, wi).is_some() {
            let p_guide = g.field.pdf(rec.p, wi);
            sample.pdf = (alpha * p_guide + (1.0 - alpha) * sample.pdf).max(1e-4);
        }
        Some(sample)
    }
}

/// Everything recorded at one path vertex during the forward walk. The
/// backward gather reconstructs the radiance estimate from these exactly as
/// the old recursion did: `R = atten · (emit_here + nee + factor ·
/// (next_emit·next_emit_weight + R_incoming))`.
struct VertexRec {
    /// Beer-Lambert transmittance over the segment that arrived at this
    /// vertex (`ONE` for volume-scatter vertices, whose segment the
    /// estimator does not attenuate).
    atten: Vec3A,
    /// Emission counted at this vertex itself: primary and post-scatter
    /// vertices only. Emission at bounce-arrival vertices is owned by the
    /// previous vertex via `next_emit`/`next_emit_weight`.
    emit_here: Vec3A,
    /// Direct lighting gathered by NEE at this vertex.
    nee: Vec3A,
    /// Local continuation factor toward the next vertex: `value·cos/pdf`
    /// for surface bounces (already compensated for Russian roulette), the
    /// medium albedo for volume scatters, zero when the path was absorbed
    /// or roulette-killed.
    factor: Vec3A,
    /// Raw emission of the surface the continuation ray hit, and the MIS
    /// weight it carries in this vertex's estimator. Patched when the next
    /// vertex is processed; the raw value is kept separate because guiding
    /// training records the emission unweighted.
    next_emit: Vec3A,
    next_emit_weight: f32,
    /// Guiding-training info (continuous surface bounces in training passes).
    train: Option<TrainRec>,
}

struct TrainRec {
    pos: Vec3A,
    dir: Vec3A,
    cos: f32,
}

/// The state of the previous surface bounce that the next vertex needs to
/// MIS-weight its emission: the sampling context (`ray`/`rec`/`mat`/`dir`
/// for the lazy NEE-capability check) and the bounce density.
struct PrevBounce<'a> {
    ray: Ray,
    rec: HitRecord,
    mat: &'a dyn Material,
    dir: Vec3A,
    pdf: f32,
    delta: bool,
}

/// MIS weight for emission reached by the previous vertex's bounce ray.
/// Delta samples are invisible to light sampling (their lobe is excluded
/// from eval), so the bounce carries the emission whole — likewise at
/// vertices where NEE is inactive, and for emissive geometry with no
/// light-list entry, which NEE can never sample. Otherwise the competing
/// density is the same strategy the NEE side uses: uniform 1-of-N pick
/// times the hit light's area-sampling pdf.
fn bounce_emission_weight(prev: &PrevBounce, lights: &LightList, hit: &crate::hittable::Hit) -> f32 {
    if prev.delta || prev.mat.eval(&prev.ray, &prev.rec, prev.dir).is_none() {
        return 1.0;
    }
    match lights.find_by_material(hit.mat) {
        Some(light) => {
            let light_pdf =
                (light.pdf(prev.rec.p, hit.rec.p) / lights.count() as f32).max(1e-6);
            utils::balance_heuristic(prev.pdf, light_pdf)
        }
        None => 1.0,
    }
}

/// The integrator: an iterative path tracer in two passes. The forward walk
/// traces one segment per bounce (each hit serves both as the previous
/// vertex's potential light hit and as the next vertex — the old recursion
/// intersected every segment twice), records a `VertexRec` per vertex, and
/// applies Russian roulette past `RR_START_BOUNCE`. The backward gather
/// then folds the records into the radiance estimate and emits guiding
/// training samples, which need the radiance arriving from the rest of the
/// path and therefore cannot be computed forward.
fn trace_path(
    r: &Ray,
    world: &dyn Hittable,
    lights: &LightList,
    depth: i32,
    sampler: &mut dyn Sampler,
    guiding: Option<&GuidingContext>,
    train_out: &mut Vec<SampleData>,
) -> Vec3A {
    let training = guiding.is_some_and(|g| g.training);
    let mut records: Vec<VertexRec> = Vec::with_capacity(depth.max(0) as usize);
    let mut ray = r.clone();
    let mut remaining = depth;
    // Set after every surface bounce; `None` at the primary vertex and
    // after volume scatters, where the next vertex's emission counts fully.
    let mut prev: Option<PrevBounce> = None;
    // Running throughput. Only drives the roulette survival probability —
    // the estimate itself is rebuilt by the backward gather.
    let mut beta = Vec3A::ONE;
    // Radiance entering the path from beyond the last vertex.
    let mut terminal = Vec3A::ZERO;

    loop {
        if remaining <= 0 {
            // Depth exhausted. The old recursion still counted bounce-hit
            // emission at the last vertex (its `add_emission` term traced
            // the ray itself) but never the background — reproduce both.
            if let Some(p) = &prev {
                if let Some(hit) = world.hit(&ray, 0.001, f32::INFINITY) {
                    let emitted = hit.mat.emitted();
                    if emitted.length_squared() > 0.0 {
                        let last = records.last_mut().expect("prev implies a record");
                        last.next_emit = emitted;
                        last.next_emit_weight = bounce_emission_weight(p, lights, &hit);
                    }
                }
            }
            break;
        }

        // Fresh dimension window for this bounce — a no-op today, a hook for
        // padded-Sobol later.
        sampler.advance_bounce();

        let Some(hit) = world.hit(&ray, 0.001, f32::INFINITY) else {
            // === Background ===
            let unit_direction = Vec3A::normalize(ray.direction());
            let t = 0.5 * (unit_direction.y + 1.0);
            terminal = (1.0 - t) * Vec3A::new(1.0, 1.0, 1.0) + t * Vec3A::new(0.5, 0.7, 1.0);
            break;
        };
        let rec: HitRecord = hit.rec;
        let mat = hit.mat;

        // Volume interaction sampling for scattering media (subsurface,
        // participating volumes). If the sampled distance is closer than
        // the surface hit, kick a scattering event and short-circuit the
        // surface interaction.
        if let Some(medium) = ray.medium() {
            if medium.is_scattering() {
                let sigma_t_max = medium.sigma_t_max().max(1e-4);
                let t_scatter = -(sampler.next_1d().ln()) / sigma_t_max;
                if t_scatter < rec.t {
                    let pos = ray.at(t_scatter);
                    let phase_uv = sampler.next_2d();
                    let dir = sample_henyey_greenstein(
                        ray.direction().normalize(),
                        medium.g,
                        phase_uv[0],
                        phase_uv[1],
                    );
                    let albedo = medium.albedo();
                    let medium = medium.clone();
                    // Volume scattering events are not trained on — the
                    // field guides surface bounces only. No emission
                    // bookkeeping happened along this phase-scattered ray,
                    // so the next hit's emission must count.
                    records.push(VertexRec {
                        atten: Vec3A::ONE,
                        emit_here: Vec3A::ZERO,
                        nee: Vec3A::ZERO,
                        factor: albedo,
                        next_emit: Vec3A::ZERO,
                        next_emit_weight: 1.0,
                        train: None,
                    });
                    beta *= albedo;
                    ray = Ray::new_in_medium(pos, dir, medium);
                    remaining -= 1;
                    prev = None;
                    continue;
                }
            }
        }

        // Beer-Lambert attenuation across the segment travelled inside a
        // participating medium (transmissive OpenPBR surfaces mark rays with
        // `Some(medium)` on refraction; free-space rays are unaffected).
        let atten = match ray.medium() {
            Some(m) => m.transmittance(rec.t),
            None => Vec3A::ONE,
        };

        // Emission accounting: a vertex reached by a surface bounce hands
        // its emission to the previous vertex's record, MIS-weighted —
        // counting it here too would double it. At the primary vertex and
        // after volume scatters it counts here, in full.
        let emitted = mat.emitted();
        let mut emit_here = Vec3A::ZERO;
        match &prev {
            Some(p) => {
                if emitted.length_squared() > 0.0 {
                    let last = records.last_mut().expect("prev implies a record");
                    last.next_emit = emitted;
                    last.next_emit_weight = bounce_emission_weight(p, lights, &hit);
                }
            }
            None => emit_here = emitted,
        }

        // Guide secondary bounces only: primary vertices vary per pixel far
        // below the guiding field's spatial resolution, so guiding them adds
        // parallax-mismatch variance instead of removing any.
        let guiding_here = if prev.is_some() { guiding } else { None };

        // === 1. Direct Lighting via Light Sampling ===
        // The light strategy is "pick one light uniformly, then sample a
        // point on it by area", so its solid-angle density is
        // `light.pdf / n_lights`. `bounce_emission_weight` evaluates the
        // same expression for a bounce-hit light — both MIS weights must
        // describe the same strategy or emission is double-counted.
        let mut nee = Vec3A::ZERO;
        if let Some(light) = lights.sample(sampler) {
            let n_lights = lights.count() as f32;
            let area_uv = sampler.next_2d();
            let light_point = light.sample_point(area_uv[0], area_uv[1]);
            let light_dir = light_point - rec.p;
            let light_distance = light_dir.length();
            let light_dir_unit = light_dir.normalize();

            let shadow_ray = Ray::new(rec.p, light_dir_unit);

            if world.hit(&shadow_ray, 0.001, light_distance - 0.001).is_none() {
                // Unsigned: lights behind the ray-facing normal are reachable
                // through a continuous transmission lobe (opaque materials
                // evaluate to zero there anyway).
                let cosine = rec.normal.dot(light_dir_unit).abs();
                let light_pdf = (light.pdf(rec.p, light_point) / n_lights).max(1e-6);

                // Evaluate the BSDF toward the light direction. Delta and
                // transmissive materials return None — they cannot see a
                // light-sampled direction and pick up emission via BSDF
                // sampling instead.
                if let Some((brdf_value, brdf_pdf)) = mat.eval(&ray, &rec, light_dir_unit) {
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
                    nee += light.emission() * brdf_value * cosine * weight / light_pdf;
                }
            }
        }

        let mut vrec = VertexRec {
            atten,
            emit_here,
            nee,
            factor: Vec3A::ZERO,
            next_emit: Vec3A::ZERO,
            next_emit_weight: 1.0,
            train: None,
        };

        // === 2. Indirect Lighting via BSDF (or guided) Sampling ===
        if let Some(sample) = sample_bounce_direction(&ray, &rec, mat, guiding_here, sampler) {
            let dir = sample.ray.direction().normalize();
            // The codebase convention multiplies the material's brdf*|cos|
            // value by the cosine again — unsigned, so continuous
            // transmission directions (behind the ray-facing normal) are not
            // zeroed. Delta samples carry their full throughput in `value`
            // and skip the factor entirely.
            let cosine = if sample.delta {
                1.0
            } else {
                rec.normal.dot(dir).abs()
            };
            let mut factor = sample.value * cosine / sample.pdf;

            // Russian roulette on the continuation: survive with probability
            // tracking the throughput, dividing it out on survival. Applies
            // to the whole continuation (bounce-hit emission included).
            beta *= atten * factor;
            let mut survived = true;
            if records.len() >= RR_START_BOUNCE {
                let p_survive = beta.max_element().clamp(RR_MIN_PROB, 1.0);
                if p_survive < 1.0 {
                    if sampler.next_1d() >= p_survive {
                        survived = false;
                    } else {
                        factor /= p_survive;
                        beta /= p_survive;
                    }
                }
            }

            if survived {
                // Training samples cover continuous surface bounces only —
                // the guide can never produce a delta direction. The
                // radiance is filled in by the backward gather.
                if training && !sample.delta {
                    vrec.train = Some(TrainRec {
                        pos: rec.p,
                        dir,
                        cos: rec.normal.dot(dir).abs(),
                    });
                }
                vrec.factor = factor;
                prev = Some(PrevBounce {
                    ray: ray.clone(),
                    rec,
                    mat,
                    dir,
                    pdf: sample.pdf,
                    delta: sample.delta,
                });
                records.push(vrec);
                ray = sample.ray;
                remaining -= 1;
                continue;
            }
        }

        // Absorbed or roulette-killed: this vertex's own gathers stand
        // (factor stays zero), the path ends here.
        records.push(vrec);
        break;
    }

    // Backward gather: fold the records into the estimate, deepest vertex
    // first, emitting guiding training samples along the way. `radiance` is
    // what the old recursion returned to each vertex from its continuation
    // (next vertex's emission suppressed — its MIS-weighted share enters
    // separately through `next_emit`).
    let mut radiance = terminal;
    for vrec in records.iter().rev() {
        if let Some(t) = &vrec.train {
            // The full incident radiance (reflected + the raw hit emission),
            // weighted by cos² to match this tracer's estimator, which
            // multiplies the codebase's brdf*|cos| material values by the
            // cosine again.
            train_out.push(SampleData {
                pos: t.pos,
                dir: t.dir,
                radiance: (luminance(radiance + vrec.next_emit) * t.cos * t.cos)
                    .min(TRAIN_RADIANCE_CLAMP),
            });
        }
        radiance = vrec.atten
            * (vrec.emit_here
                + vrec.nee
                + vrec.factor * (vrec.next_emit * vrec.next_emit_weight + radiance));
    }
    radiance
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
