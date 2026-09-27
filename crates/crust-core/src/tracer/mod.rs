use glam::Vec3A;
use rayon::prelude::*;
use tracing::{debug, info, warn};
use utils::luminance;

use crate::buffer::Buffer;
use crate::camera::Camera;
use crate::filter::FilterSampler;
use crate::guiding::{GuidingConfig, GuidingField, SampleData};
use crate::profile::{self, Section};
use crate::rt_world::World;
use crate::stats::RayStats;
use crate::volume::Volumes;
use crate::{LightList, LightSelection, PathSampler};

mod path;
mod settings;

use path::{K_CAMERA, K_TIME, ray_cones_enabled, trace_path};

pub use path::ray_color;
pub use settings::{DEFAULT_INDIRECT_CLAMP, RenderSettings, SamplingStrategy};

pub(crate) use path::PathScratch;

/// Render-progress callback: invoked with `(completed, total)` work units
/// (scanline rows, or tiles under bucket rendering) as a pass advances.
/// Called from worker threads, hence `Sync`, but never concurrently and always
/// with `completed` increasing by one — a host can show the last value it was
/// given. Presentation (progress bars,
/// logging) is the caller's concern — the engine has no UI dependencies.
pub type ProgressCallback<'a> = &'a (dyn Fn(u64, u64) + Sync);

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
}

/// Image-quality statistics of one render pass.
struct PassStats {
    /// Mean per-pixel variance of the pixel estimate — the inverse-variance
    /// blending weight (`f64::INFINITY` when spp < 2 makes estimation
    /// impossible).
    variance: f64,
    /// Per-pixel variance of the pixel-mean luminance, row-major. Feeds the
    /// guiding efficiency estimate, which normalizes it against a reference
    /// image shared by every pass being compared (`mean_relative_error`).
    var_map: Vec<f64>,
    /// Integrator work this pass did.
    rays: RayStats,
}

pub struct Renderer {
    pub camera: Camera,
    /// The committed world: the `crust-rt` kernel scene plus the material
    /// table hits resolve through (by `geom_id`).
    pub world: World,
    pub lights: LightList,
    pub settings: RenderSettings,
    /// Participating-media regions, sampled outside the BVH (see
    /// `volume.rs`). Empty for scenes without volumes — every volume code
    /// path short-circuits then.
    pub volumes: Volumes,
}

impl Renderer {
    pub fn new(
        camera: Camera,
        world: World,
        mut lights: LightList,
        settings: RenderSettings,
    ) -> Self {
        // Built here rather than by the importer so that every way of
        // assembling a scene — USD, the procedural fallback, a test — gets
        // the selection its settings ask for.
        lights.select_by(settings.light_selection());
        if settings.light_selection() == LightSelection::Learned {
            let started = std::time::Instant::now();
            match crate::light_cache::train(
                &world,
                &camera,
                &lights,
                settings.width,
                settings.height,
                settings.frame,
            ) {
                Some(cache) => {
                    debug!(
                        "learned light selection: {} receivers, {} trained cells, in {:?}",
                        cache.receivers,
                        cache.trained_cells,
                        started.elapsed()
                    );
                    lights.set_cache(cache);
                }
                None => debug!("learned light selection: nothing to learn, picking by power"),
            }
        }
        debug!(
            "Renderer over {} geometries, {} light(s) picked by {:?}, {} rayon thread(s)",
            world.count(),
            lights.count(),
            lights.selection(),
            rayon::current_num_threads()
        );
        Renderer {
            camera,
            world,
            lights,
            settings,
            volumes: Volumes::default(),
        }
    }

    pub fn with_volumes(mut self, regions: Vec<crate::volume::VolumeRegion>) -> Self {
        if !regions.is_empty() {
            // Only when there are any: `volumes.is_empty()` short-circuits
            // every volume code path, and a line saying "0 regions" on every
            // ordinary render would say nothing.
            debug!("{} volume region(s) attached", regions.len());
        }
        self.volumes = Volumes::new(regions);
        self
    }

    pub fn render(&self) -> Buffer {
        self.render_impl(false, None).0
    }

    pub fn render_with_tiles(&self) -> Buffer {
        self.render_impl(true, None).0
    }

    /// Renders with a progress callback — see [`ProgressCallback`]. With
    /// guiding enabled, only the final pass reports (training passes are
    /// silent, as before).
    pub fn render_with_progress(&self, tiled: bool, progress: ProgressCallback) -> Buffer {
        self.render_impl(tiled, Some(progress)).0
    }

    /// As [`Renderer::render_with_progress`], also returning what the
    /// integrator did — see [`RayStats`]. Counting is unconditional and
    /// costs an increment per ray, so this is the same render either way;
    /// the other entry points simply discard the numbers.
    ///
    /// With guiding enabled the counters cover **every** pass, training
    /// included, since all of them spend time.
    pub fn render_with_stats(&self, tiled: bool, progress: ProgressCallback) -> (Buffer, RayStats) {
        self.render_impl(tiled, Some(progress))
    }

    fn render_impl(&self, tiled: bool, progress: Option<ProgressCallback>) -> (Buffer, RayStats) {
        if self.settings.guiding {
            return self.render_guided(tiled, progress);
        }
        let (buf, _, pass) = self.render_pass(self.final_pass_config(tiled), None, progress);
        (buf, pass.rays)
    }

    /// Config of a final (image-quality) pass: full budget, adaptive
    /// sampling.
    fn final_pass_config(&self, tiled: bool) -> PassConfig {
        PassConfig {
            spp: self.settings.samples_per_pixel,
            seed: self.settings.frame as u32,
            tiled,
            adaptive: true,
        }
    }

    /// Progressive path-guided rendering: training passes with geometrically
    /// growing sample budgets (2, 2, 4, … spp — the schedule is floored at
    /// 2 spp so every pass can estimate its own variance) build the guiding
    /// field, then the full per-pixel budget renders with the frozen field.
    /// Every pass is an unbiased image of the same scene, so instead of
    /// discarding the training passes the final image blends all of them
    /// weighted by inverse variance — passes rendered before the field
    /// converged simply receive small weights.
    ///
    /// The training passes double as a guiding efficiency estimate
    /// (Li et al. 2026, "Path Guiding in Disney's Zootopia 2"): efficiency
    /// is `E = 1/(cost · variance)`, with wall-clock cost and MRSE variance
    /// (`mean_relative_error`). The first pass runs before the field has
    /// trained — effectively an unguided render — and gives `E_pg−`; the
    /// last training pass, with the field at its most trained, gives
    /// `E_pg+`. Variance scales as 1/spp while cost scales as spp, so the
    /// product is comparable across passes with different budgets. If
    /// `ΔEff = E_pg+/E_pg− < 1`, guiding costs more than the variance it
    /// removes here, and the final pass renders unguided instead (the
    /// training passes still blend in — they are unbiased either way).
    fn render_guided(&self, tiled: bool, progress: Option<ProgressCallback>) -> (Buffer, RayStats) {
        // Every pass costs time, training included, so the counters cover
        // all of them rather than the final pass alone.
        let mut rays = RayStats::default();
        let bounds = match self.world.bounds() {
            Some(b) => b,
            None => {
                warn!("path guiding enabled but the scene has no bounding box; rendering unguided");
                let (buf, _, pass) =
                    self.render_pass(self.final_pass_config(tiled), None, progress);
                return (buf, pass.rays);
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
        // (per-pixel variance map, seconds) of the first pass (untrained
        // field → effectively unguided) and of the last training pass
        // (most-trained field) — the two endpoints of the efficiency
        // estimate. The first pass does carry the sample-recording overhead,
        // which slightly understates the unguided efficiency, i.e. errs
        // toward keeping guiding on.
        let mut eff_unguided: Option<(Vec<f64>, f64)> = None;
        let mut eff_guided: Option<(Vec<f64>, f64)> = None;

        for k in 0..cfg.train_iterations {
            let spp = (1u32 << k.min(16)).max(2);
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
            };
            let start = std::time::Instant::now();
            let (buffer, samples, stats) = self.render_pass(train_cfg, Some(&gctx), None);
            rays.merge(&stats.rays);
            let secs = start.elapsed().as_secs_f64();
            debug!(
                "path guiding: training pass {}/{} at {} spp — {} samples, variance {:.3e}, {:.2}s",
                k + 1,
                cfg.train_iterations,
                spp,
                samples.len(),
                stats.variance,
                secs
            );
            if k == 0 {
                eff_unguided = Some((stats.var_map, secs));
            } else if k == cfg.train_iterations - 1 {
                eff_guided = Some((stats.var_map, secs));
            }
            field.update(&samples, k + 1);
            debug!(
                "path guiding: field now holds {} spatial leaf/leaves after pass {}/{}",
                field.leaf_count(),
                k + 1,
                cfg.train_iterations
            );
            passes.push((buffer, stats.variance));
        }

        let guide_final = match (&eff_unguided, &eff_guided) {
            (Some((var_pt, cost_pt)), Some((var_pg, cost_pg))) if *cost_pg > 0.0 => {
                // Reference image for relative error: the blend of all
                // training passes — our stand-in for the paper's denoised
                // accumulated image, and crucially the *same* image for both
                // sides of the ratio.
                let ref_lum = blend_luminance(&passes, self.settings.width, self.settings.height);
                let mrse_pt = mean_relative_error(var_pt, &ref_lum);
                let mrse_pg = mean_relative_error(var_pg, &ref_lum);
                if mrse_pt.is_finite() && mrse_pg.is_finite() && mrse_pt > 0.0 && mrse_pg > 0.0 {
                    let delta_eff = (cost_pt * mrse_pt) / (cost_pg * mrse_pg);
                    info!(
                        "path guiding: estimated efficiency improvement ΔEff = {:.2} (>1 means guiding pays off)",
                        delta_eff
                    );
                    if delta_eff < 1.0 {
                        info!(
                            "path guiding: ΔEff < 1 — guiding costs more than the variance it removes here; rendering the final pass unguided"
                        );
                    }
                    delta_eff >= 1.0
                } else {
                    true
                }
            }
            // A single training iteration never runs a guided pass, and
            // degenerate statistics give no basis to overrule the scene's
            // explicit opt-in — keep guiding.
            _ => true,
        };

        debug!(
            "path guiding: final pass at {} spp ({})",
            self.settings.samples_per_pixel,
            if guide_final { "guided" } else { "unguided" }
        );
        let gctx = GuidingContext {
            field: &field,
            training: false,
        };
        let final_gctx = if guide_final { Some(&gctx) } else { None };
        let (final_buffer, _, final_stats) =
            self.render_pass(self.final_pass_config(tiled), final_gctx, progress);
        rays.merge(&final_stats.rays);
        passes.push((final_buffer, final_stats.variance));

        (self.blend_passes(passes), rays)
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
        debug!(
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
    /// `GuidingContext` is supplied), and the pass's [`PassStats`].
    fn render_pass(
        &self,
        cfg: PassConfig,
        gctx: Option<&GuidingContext>,
        progress: Option<ProgressCallback>,
    ) -> (Buffer, Vec<SampleData>, PassStats) {
        let mut buffer = Buffer::new(self.settings.width, self.settings.height);
        let mut all_samples = Vec::new();
        let mut variance_sum = 0.0f64;
        let mut rays = RayStats::default();
        let mut var_map = vec![0.0f64; self.settings.width * self.settings.height];
        let pixel_count = (self.settings.width * self.settings.height) as f64;
        // One tabulation per pass, shared read-only by every worker.
        let filter = FilterSampler::new(self.settings.pixel_filter);
        // Read once per pass and dispatched to one of two monomorphisations
        // of the integrator (see `profile::scope_if`), so an unprofiled
        // render carries no trace of the profiler.
        let profiling = profile::enabled();
        // Per *pass*, never per pixel or per ray: a guided render runs a
        // handful of these and an ordinary one exactly one, so the whole
        // block costs nothing an integrator would notice.
        let pass_start = std::time::Instant::now();
        debug!(
            "pass: {} spp, seed {}, {}, adaptive {} (min {} spp, variance threshold {}), \
             filter {} radius {}, strategy {:?}, guiding {}",
            cfg.spp,
            cfg.seed,
            if cfg.tiled {
                "16x16 tiles"
            } else {
                "scanlines"
            },
            cfg.adaptive,
            self.settings.min_samples_per_pixel,
            self.settings.variance_threshold,
            self.settings.pixel_filter.name(),
            self.settings.pixel_filter.radius(),
            self.settings.sampling_strategy,
            match gctx {
                Some(g) if g.training => "training",
                Some(_) => "guided",
                None => "off",
            },
        );

        if cfg.tiled {
            let tiles = generate_tiles(self.settings.width, self.settings.height, TILE); // tile size: 16x16
            let total = tiles.len() as u64;
            // Incremented and reported under one lock, so the callback sees
            // completions in increasing order even though tiles finish on
            // many threads at once (see `ProgressCallback`). Taken once per
            // tile, which no render will notice.
            let done = std::sync::Mutex::new(0u64);
            type TileOut = (Vec<(Vec3A, f64, Vec<SampleData>)>, RayStats);
            let results: Vec<TileOut> = tiles
                .par_iter()
                .map(|tile| {
                    let mut pixels = Vec::with_capacity(tile.width * tile.height);
                    // Private to this tile, so no two threads share a
                    // counter and there is nothing to synchronise. The path
                    // scratch has the same ownership story: one buffer serves
                    // every sample of every pixel in the tile.
                    let mut tile_rays = RayStats::default();
                    let mut scratch = PathScratch::new(self.settings.max_depth as usize);
                    for j in tile.y..tile.y + tile.height {
                        for i in tile.x..tile.x + tile.width {
                            let (color, s, v) = if profiling {
                                self.render_pixel::<true>(
                                    i,
                                    j,
                                    &cfg,
                                    &filter,
                                    gctx,
                                    &mut scratch,
                                    &mut tile_rays,
                                )
                            } else {
                                self.render_pixel::<false>(
                                    i,
                                    j,
                                    &cfg,
                                    &filter,
                                    gctx,
                                    &mut scratch,
                                    &mut tile_rays,
                                )
                            };
                            pixels.push((color, v, s));
                        }
                    }
                    // Once per tile, and a no-op unless `--profile` is on.
                    profile::flush();
                    if let Some(cb) = progress {
                        let mut n = done.lock().unwrap_or_else(|e| e.into_inner());
                        *n += 1;
                        cb(*n, total);
                    }
                    (pixels, tile_rays)
                })
                .collect();
            // Tiles finish in tile order, but what the pass hands on — the
            // guiding field's training samples and the pass variance, an f64
            // sum — is gathered in *scanline* order (rows top-down, pixels
            // left to right), exactly as the scanline path gathers it. Both
            // are order-dependent in floating point (the SD-tree accumulates
            // the samples it is given), so this is what keeps a guided render
            // bit-identical whichever order the pixels were rendered in.
            //
            // The tile results are replayed in that order straight from the
            // tile grid (`generate_tiles` emits tile rows by increasing `y`,
            // each left to right, so walking it backwards by row gives rows
            // in scanline order), and nothing full-frame is allocated to do
            // it.
            let w = self.settings.width;
            let mut results = results;
            for (_, tile_rays) in &results {
                rays.merge(tile_rays);
            }
            let tiles_x = w.div_ceil(TILE);
            for ty in (0..tiles.len() / tiles_x.max(1)).rev() {
                let row = ty * tiles_x..(ty + 1) * tiles_x;
                let (y0, rows) = (tiles[row.start].y, tiles[row.start].height);
                for j in (y0..y0 + rows).rev() {
                    for k in row.clone() {
                        let tile = &tiles[k];
                        let pixels = &mut results[k].0;
                        for i in tile.x..tile.x + tile.width {
                            let (color, var, s) =
                                &mut pixels[(j - tile.y) * tile.width + (i - tile.x)];
                            buffer.set_pixel(i, j, *color);
                            var_map[j * w + i] = *var;
                            variance_sum += *var;
                            all_samples.append(s);
                        }
                    }
                }
            }
        } else {
            // Rows are the work unit, and each worker writes its row of the
            // buffer and of the variance map in place: `par_chunks_mut` hands
            // out disjoint `&mut` rows, so there is no lock, no atomic and no
            // per-row fork/join barrier — the borrow checker is what proves
            // two workers never touch the same pixel.
            let w = self.settings.width;
            let total = self.settings.height as u64;
            // Incremented and reported under one lock, as the tiles do, so the
            // callback sees rows complete in increasing count.
            let done = std::sync::Mutex::new(0u64);
            let rows: Vec<(Vec<SampleData>, RayStats)> = buffer
                .pixels_mut()
                .par_chunks_mut(w)
                .zip(var_map.par_chunks_mut(w))
                .enumerate()
                .map(|(j, (pixels, vars))| {
                    let mut scratch = PathScratch::new(self.settings.max_depth as usize);
                    let mut row_rays = RayStats::default();
                    let mut samples = Vec::new();
                    for i in 0..w {
                        let (c, s, v) = if profiling {
                            self.render_pixel::<true>(
                                i,
                                j,
                                &cfg,
                                &filter,
                                gctx,
                                &mut scratch,
                                &mut row_rays,
                            )
                        } else {
                            self.render_pixel::<false>(
                                i,
                                j,
                                &cfg,
                                &filter,
                                gctx,
                                &mut scratch,
                                &mut row_rays,
                            )
                        };
                        pixels[i] = c;
                        vars[i] = v;
                        samples.extend(s);
                    }
                    // Once per row, and a no-op unless `--profile` is on.
                    profile::flush();
                    if let Some(cb) = progress {
                        let mut n = done.lock().unwrap_or_else(|e| e.into_inner());
                        *n += 1;
                        cb(*n, total);
                    }
                    (samples, row_rays)
                })
                .collect();
            // What is order-dependent in floating point — the pass variance,
            // an f64 sum, and the training samples the SD-tree accumulates —
            // is gathered serially in scanline order (rows top-down, pixels
            // left to right), exactly as the tiled path gathers it.
            for (j, (samples, row_rays)) in rows.into_iter().enumerate().rev() {
                rays.merge(&row_rays);
                // Pixel by pixel into the one running sum: a per-row partial
                // would round differently.
                for &v in &var_map[j * w..(j + 1) * w] {
                    variance_sum += v;
                }
                all_samples.extend(samples);
            }
        }

        let elapsed = pass_start.elapsed();
        debug!(
            "pass done in {:?}: {} camera rays, {} closest-hit, {} shadow, {} vertices, \
             {:.2} rays/camera ray, roulette killed {}/{}, ended {} escaped / {} at depth, \
             mean variance {:.3e}",
            elapsed,
            rays.camera_rays,
            rays.closest_hit,
            rays.shadow_rays,
            rays.vertices,
            if rays.camera_rays > 0 {
                rays.total_rays() as f64 / rays.camera_rays as f64
            } else {
                0.0
            },
            rays.rr_killed,
            rays.rr_tested,
            rays.ended_escaped,
            rays.ended_depth,
            variance_sum / pixel_count,
        );
        (
            buffer,
            all_samples,
            PassStats {
                variance: variance_sum / pixel_count,
                var_map,
                rays,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_pixel<const PROFILE: bool>(
        &self,
        i: usize,
        j: usize,
        cfg: &PassConfig,
        filter: &FilterSampler,
        gctx: Option<&GuidingContext>,
        scratch: &mut PathScratch,
        stats: &mut RayStats,
    ) -> (Vec3A, Vec<SampleData>, f64) {
        let _main = profile::scope_if::<PROFILE>(Section::MainLoop);
        let mut sum = Vec3A::ZERO;
        // FIS weight sum (see `filter.rs`): the pixel estimate is the
        // weighted average Σwᵢ·Lᵢ / Σwᵢ. For box and triangle every wᵢ is
        // exactly 1.0, so the sum is exactly `taken as f32` and the estimate
        // is the plain mean — box at radius 0.5 stays bit-identical to the
        // historical unweighted, unfiltered estimator.
        let mut weight_sum = 0.0f32;
        let mut samples = Vec::new();
        let mut lum_sum = 0.0f64;
        let mut lum_sq = 0.0f64;

        let threshold = self.settings.variance_threshold as f64;
        let min_spp = self.settings.min_samples_per_pixel.max(2);
        let mut taken = 0u32;

        // OpenQMC decorrelates pixels within a 256×256 tile; distinguish tiles
        // with an extra domain so images wider/taller than 256 stay fully
        // decorrelated (the frame seed alone is constant within one render).
        let tile = (i >> 8) as i32 + ((j >> 8) as i32) * 4096;

        // Is the shutter coordinate worth sampling at all? `ray.time` is read
        // by exactly one thing — a moving instance interpolating its
        // transform — so on a scene where nothing moves, every value of it
        // produces the same image and drawing one is pure waste. It is not
        // cheap waste: `draw_sample_f32::<N>` computes a whole 4-dimensional
        // Owen-scrambled Sobol block whatever `N` is, which measured 4.2% of
        // the render on cornellbox, one block per camera ray for one float.
        //
        // Skipping the draw cannot perturb the other dimensions: `new_domain`
        // is a pure function of the parent state and takes `&self`, so a
        // domain that is never derived leaves `root` untouched.
        let motion = self.world.has_motion();

        // One pixel's world-space width, for the primary ray's cone. Hoisted
        // out of the sample loop: it depends only on the camera and the
        // resolution, neither of which moves within a render.
        let pixel_span = ray_cones_enabled().then(|| {
            self.camera
                .pixel_span(self.settings.width, self.settings.height)
        });

        for sample in 0..cfg.spp {
            let primary = profile::scope_if::<PROFILE>(Section::GeneratePrimary);
            let root = PathSampler::new(i as i32, j as i32, cfg.seed as i32, sample as i32)
                .new_domain(tile);
            let cam = root.new_domain(K_CAMERA).draw_sample_f32::<4>();
            // Warp the in-pixel jitter through the reconstruction filter's
            // distribution (filter importance sampling): the offset places
            // the sample inside the filter footprint (possibly reaching into
            // neighboring pixels' area), the weight is f/p. For box at
            // radius 0.5 this is exactly `(cam[k], 1.0)`.
            let (fx, wx) = filter.sample(cam[0]);
            let (fy, wy) = filter.sample(cam[1]);
            // Raster → NDC divides by the full resolution: pixel i covers
            // [i/w, (i+1)/w) and its center sits at (i+0.5)/w, tiling [0, 1)
            // exactly. The historical `/ (w-1)` divisor stretched the pixel
            // grid over a plane 1 pixel too wide — a sub-pixel zoom of ~1/w
            // that also let the last row and column sample past v = 1.
            let u = ((i as f32) + fx) / self.settings.width as f32;
            let v = ((j as f32) + fy) / self.settings.height as f32;
            // `Ray::new` defaults `time` to 0.0, and `transforms_at` takes the
            // start transform at time 0, so this is the value a static scene
            // was already effectively using.
            let time = if motion {
                root.new_domain(K_TIME).draw_sample_f32::<1>()[0]
            } else {
                0.0
            };
            let mut r = self.camera.get_ray(u, v, [cam[2], cam[3]], time);
            if let Some(span) = pixel_span {
                // `pixel_span` is the width one pixel covers at ray parameter
                // 1; the cone wants it per world unit, and the direction is
                // unnormalized, so divide by its length. An aperture is
                // deliberately ignored: a non-negative cone cannot express a
                // footprint that *converges* to the focus plane, and defocus
                // is resolved by sampling rather than by filtering anyway.
                let spread = span / r.direction().length().max(1e-9);
                r = r.with_cone(crate::RayCone { width: 0.0, spread });
            }
            drop(primary);
            stats.camera_rays += 1;
            let color = trace_path::<PROFILE>(
                &r,
                &self.world,
                &self.lights,
                &self.volumes,
                self.settings.max_depth as i32,
                self.settings.sampling_strategy,
                self.settings.indirect_clamp,
                root,
                gctx,
                &mut samples,
                scratch,
                stats,
            ) * (wx * wy);
            sum += color;
            weight_sum += wx * wy;
            let lum = luminance(color) as f64;
            lum_sum += lum;
            lum_sq += lum * lum;
            taken = sample + 1;

            // Adaptive early stop: once past the minimum budget, quit as soon
            // as the relative standard error of the pixel mean is below the
            // threshold. Checked every 4th sample to amortize the cost.
            if cfg.adaptive && threshold > 0.0 && taken >= min_spp && taken.is_multiple_of(4) {
                let n = taken as f64;
                let var_of_mean = ((lum_sq - lum_sum * lum_sum / n) / (n - 1.0) / n).max(0.0);
                let mean = (lum_sum / n).max(1e-4);
                if var_of_mean.sqrt() / mean < threshold {
                    break;
                }
            }
        }

        if cfg.adaptive {
            stats.adaptive_pixels += 1;
            stats.adaptive_samples += taken as u64;
            if taken < cfg.spp {
                stats.early_stopped += 1;
            }
            stats.spp_min = if stats.adaptive_pixels == 1 {
                taken
            } else {
                stats.spp_min.min(taken)
            };
            stats.spp_max = stats.spp_max.max(taken);
        }

        // Unbiased variance of the pixel-mean luminance.
        let n = taken as f64;
        let variance = if taken >= 2 {
            ((lum_sq - lum_sum * lum_sum / n) / (n - 1.0) / n).max(0.0)
        } else {
            f64::INFINITY
        };
        // Weighted-average film estimator. A Mitchell pixel whose few
        // samples all landed on negative lobes could zero the denominator;
        // the plain mean is the sane fallback there.
        let mean = if weight_sum > 0.0 {
            sum / weight_sum
        } else {
            sum / taken as f32
        };
        (mean, samples, variance)
    }
}

/// Per-pixel luminance of the inverse-variance blend of `passes` — the
/// reference image the guiding efficiency estimate normalizes against.
/// Un-weightable passes (non-finite or zero variance) contribute nothing;
/// if no pass is weightable the result is black and the floor in
/// `mean_relative_error` takes over.
fn blend_luminance(passes: &[(Buffer, f64)], width: usize, height: usize) -> Vec<f64> {
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
    let total: f64 = weights.iter().sum::<f64>().max(f64::MIN_POSITIVE);
    let mut out = vec![0.0f64; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut c = Vec3A::ZERO;
            for ((pass, _), w) in passes.iter().zip(&weights) {
                c += pass.get_pixel(x, y) * (*w / total) as f32;
            }
            out[y * width + x] = luminance(c) as f64;
        }
    }
    out
}

/// Mean relative squared error of a pass (Rousselle et al. 2011): per-pixel
/// variance over the squared luminance of a reference image, floored at
/// 1e-4 so directly visible light sources don't dominate. The reference
/// must be *shared* by every pass being compared — normalizing a low-spp
/// pass by its own noisy mean correlates numerator and denominator and
/// breaks the 1/spp scaling the efficiency ratio relies on.
fn mean_relative_error(var_map: &[f64], ref_lum: &[f64]) -> f64 {
    let n = var_map.len().max(1) as f64;
    var_map
        .iter()
        .zip(ref_lum)
        .map(|(v, d)| v / (d * d).max(1e-4))
        .sum::<f64>()
        / n
}

struct Tile {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Edge length of a render tile, in pixels.
const TILE: usize = 16;

/// The tile grid, in tile rows from `y = 0` up, each row left to right —
/// the order `render_pass` relies on to replay tiles in scanline order.
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

#[cfg(test)]
mod tests;
