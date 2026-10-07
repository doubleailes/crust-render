use glam::Vec3A;
use rayon::prelude::*;
use tracing::{debug, info, warn};

use crate::aov::{AovFilm, AovLayout, AovRequest, CameraFrame, SampleExtras, UnitAov};
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
pub(crate) use path::surface_visibility;
mod route;
mod settings;

use path::{K_CAMERA, K_TIME, PathContext, ray_cones_enabled, trace_path};

pub use path::{ray_color, ray_color_with_light_samples};
pub use settings::{
    DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE, DEFAULT_INDIRECT_CLAMP, RenderSettings, SamplingStrategy,
};

pub(crate) use path::PathScratch;
pub(crate) use route::RouteCtx;

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
/// A pass's guiding training samples, where the workers left them: one
/// buffer per work unit (tile or row), and the order to read them in.
///
/// The SD-tree accumulates its samples in floating point, so they must reach
/// it in scanline order whichever order the pixels were rendered in. Rather
/// than copy every sample into one frame-order vector, the pass records, per
/// pixel in scanline order, which buffer holds its samples and where; reading
/// through [`PassSamples::iter`] visits them in exactly that order.
#[derive(Default)]
struct PassSamples {
    buffers: Vec<Vec<SampleData>>,
    /// `(buffer, start, end)` runs in scanline order.
    order: Vec<(u32, u32, u32)>,
}

impl PassSamples {
    fn len(&self) -> usize {
        self.buffers.iter().map(Vec::len).sum()
    }

    /// Appends the run `buffer[start..end]`, extending the last run when it
    /// continues it (the next pixel of the same tile row).
    fn push_run(&mut self, buffer: u32, start: u32, end: u32) {
        match self.order.last_mut() {
            Some((b, _, e)) if *b == buffer && *e == start => *e = end,
            _ => self.order.push((buffer, start, end)),
        }
    }

    /// Every sample, in scanline order.
    fn iter(&self) -> impl Iterator<Item = &SampleData> {
        self.order
            .iter()
            .flat_map(|&(b, s, e)| &self.buffers[b as usize][s as usize..e as usize])
    }
}

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
        self.render_impl(false, None, None).0
    }

    pub fn render_with_tiles(&self) -> Buffer {
        self.render_impl(true, None, None).0
    }

    /// Renders with a progress callback — see [`ProgressCallback`]. With
    /// guiding enabled, only the final pass reports (training passes are
    /// silent, as before).
    pub fn render_with_progress(&self, tiled: bool, progress: ProgressCallback) -> Buffer {
        self.render_impl(tiled, Some(progress), None).0
    }

    /// As [`Renderer::render_with_progress`], also returning what the
    /// integrator did — see [`RayStats`]. Counting is unconditional and
    /// costs an increment per ray, so this is the same render either way;
    /// the other entry points simply discard the numbers.
    ///
    /// With guiding enabled the counters cover **every** pass, training
    /// included, since all of them spend time.
    pub fn render_with_stats(&self, tiled: bool, progress: ProgressCallback) -> (Buffer, RayStats) {
        let (buffer, _, rays) = self.render_impl(tiled, Some(progress), None);
        (buffer, rays)
    }

    /// As [`Renderer::render_with_stats`], also filling the AOVs `request`
    /// asks for — see [`AovFilm`]. The beauty is bit-identical to
    /// `render_with_stats`'s: the AOVs observe the same camera samples and
    /// change none of them. A request that needs no film (no products, or
    /// only a 3-channel beauty) takes the beauty-only path and returns an
    /// empty film.
    pub fn render_with_aovs(
        &self,
        tiled: bool,
        progress: ProgressCallback,
        request: &AovRequest,
    ) -> (Buffer, AovFilm, RayStats) {
        let (w, h) = (self.settings.width, self.settings.height);
        if !request.needs_film() {
            let (buffer, _, rays) = self.render_impl(tiled, Some(progress), None);
            return (buffer, AovFilm::empty(w, h), rays);
        }
        let mut layout = AovLayout::new(request);
        if !layout.lpes.is_empty() || layout.albedo || layout.diffuse_filter {
            // One DFA for every expression of the render, over the lights'
            // tags; shared read-only by every worker.
            let tags: Vec<Option<&str>> = (0..self.lights.count())
                .map(|i| self.lights.lpe_tag(i))
                .collect();
            let ctx =
                route::RouteCtx::new(&layout.lpes, &tags, layout.albedo, layout.diffuse_filter);
            debug!(
                "AOVs: {} light path expression(s){}, albedo {}",
                layout.lpes.len(),
                ctx.describe(),
                if layout.albedo { "on" } else { "off" }
            );
            layout.route = Some(std::sync::Arc::new(ctx));
        }
        let (buffer, film, rays) = self.render_impl(tiled, Some(progress), Some(&layout));
        (
            buffer,
            film.expect("a pass with a layout returns a film"),
            rays,
        )
    }

    fn render_impl(
        &self,
        tiled: bool,
        progress: Option<ProgressCallback>,
        layout: Option<&AovLayout>,
    ) -> (Buffer, Option<AovFilm>, RayStats) {
        if self.settings.guiding {
            return self.render_guided(tiled, progress, layout);
        }
        let (buf, film, _, pass) =
            self.render_pass(self.final_pass_config(tiled), None, progress, layout);
        (buf, film, pass.rays)
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
    fn render_guided(
        &self,
        tiled: bool,
        progress: Option<ProgressCallback>,
        layout: Option<&AovLayout>,
    ) -> (Buffer, Option<AovFilm>, RayStats) {
        // Every pass costs time, training included, so the counters cover
        // all of them rather than the final pass alone.
        let mut rays = RayStats::default();
        let bounds = match self.world.bounds() {
            Some(b) => b,
            None => {
                warn!("path guiding enabled but the scene has no bounding box; rendering unguided");
                let (buf, film, _, pass) =
                    self.render_pass(self.final_pass_config(tiled), None, progress, layout);
                return (buf, film, pass.rays);
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
        // Each pass's AOVs, blended with the beauty's own weights at the end.
        let mut films: Vec<AovFilm> = Vec::new();
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
            let (buffer, film, samples, stats) =
                self.render_pass(train_cfg, Some(&gctx), None, layout);
            films.extend(film);
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
            field.update(samples.iter(), k + 1);
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
                let ref_lum = blend_luminance(
                    &passes,
                    self.settings.width,
                    self.settings.height,
                    self.lights.luma(),
                );
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
        let (final_buffer, final_film, _, final_stats) =
            self.render_pass(self.final_pass_config(tiled), final_gctx, progress, layout);
        rays.merge(&final_stats.rays);
        passes.push((final_buffer, final_stats.variance));
        films.extend(final_film);

        let film = (!films.is_empty()).then(|| {
            let (weights, total) = blend_weights(&passes);
            if total <= 0.0 {
                films.pop().expect("checked non-empty")
            } else {
                AovFilm::blend(films, &weights, total)
            }
        });
        (self.blend_passes(passes), film, rays)
    }

    /// Inverse-variance blend of independent unbiased passes. Passes whose
    /// variance could not be estimated (spp < 2) get zero weight; if nothing
    /// is weightable, the last (final) pass is returned as-is.
    fn blend_passes(&self, mut passes: Vec<(Buffer, f64)>) -> Buffer {
        let (weights, total) = blend_weights(&passes);
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
    /// its AOVs (when `layout` asks for any), whatever training samples the
    /// pass recorded (empty unless a training `GuidingContext` is supplied),
    /// and the pass's [`PassStats`].
    ///
    /// The work unit is a 16×16 tile or a `width`×1 row; the two differ in
    /// nothing but the tile list, and a render mode is scheduling only. A
    /// non-adaptive (training) pass is one sweep of every unit to `spp`. An
    /// adaptive pass first sweeps every pixel to the first check point, then
    /// runs **rounds** over a full-frame convergence-index buffer: each
    /// round freezes every pixel's index and whether it is still sampling,
    /// decides against that frozen buffer which pixels stop (their own
    /// test, and the cross-neighbour rule of [`held_by_neighbour`]), then
    /// traces the next batch of samples ([`batch_schedule`]) for the pixels
    /// still active. Nothing writes the buffer while a decision reads it,
    /// so no decision depends on the order the units run in, and tiles and
    /// scanlines stay bit-identical with the comparison on. Each pixel
    /// draws the same sample indices, and checks at the same `taken`
    /// values, as it would if it ran alone.
    fn render_pass(
        &self,
        cfg: PassConfig,
        gctx: Option<&GuidingContext>,
        progress: Option<ProgressCallback>,
        layout: Option<&AovLayout>,
    ) -> (Buffer, Option<AovFilm>, PassSamples, PassStats) {
        let (w, h) = (self.settings.width, self.settings.height);
        let mut buffer = Buffer::new(w, h);
        let mut all_samples = PassSamples::default();
        let mut variance_sum = 0.0f64;
        let mut rays = RayStats::default();
        let mut var_map = vec![0.0f64; w * h];
        let pixel_count = (w * h) as f64;
        // One tabulation per pass, shared read-only by every worker.
        let filter = FilterSampler::new(self.settings.pixel_filter);
        // Read once per pass and dispatched to one of two monomorphisations
        // of the integrator (see `profile::scope_if`), so an unprofiled
        // render carries no trace of the profiler.
        let profiling = profile::enabled();
        // The same for the film: the AOV instantiation runs only when a
        // product asks for something beyond the beauty.
        let cam = self.camera.frame();
        let aov = layout.is_some();

        let threshold = self.settings.variance_threshold as f64;
        let tolerance = self.settings.adaptive_neighbour_tolerance;
        let adaptive = cfg.adaptive && threshold > 0.0;
        // The minimum grows with the budget: an authored minimum of 8 at
        // 1024 spp takes at least 32. A floor, not a default, so that the
        // unauthored 32 still keeps a 16 spp render (the goldens) from ever
        // stopping early. Two at least, or there is no variance to test.
        let min_spp = self
            .settings
            .min_samples_per_pixel
            .max((cfg.spp as f64).sqrt().ceil() as u32)
            .max(2);
        // Checks happen every 4th sample from the minimum on, so the first
        // one is at the smallest multiple of 4 that is at least `min_spp` —
        // if the budget reaches that far at all. Checked: a minimum within 3
        // of `u32::MAX` must saturate, not wrap to a check point of 0 that
        // would let every pixel stop after 4 samples.
        let first_check = min_spp.checked_next_multiple_of(4).unwrap_or(u32::MAX);
        let sweep_to = if adaptive {
            cfg.spp.min(first_check)
        } else {
            cfg.spp
        };
        // Rounds: one decision plus one batch each, until the budget is
        // spent. None when adaptive sampling is off or never gets to check.
        let schedule = if adaptive {
            batch_schedule(cfg.spp, first_check)
        } else {
            Vec::new()
        };
        let rounds = schedule.len();

        // Per *pass*, never per pixel or per ray: a guided render runs a
        // handful of these and an ordinary one exactly one, so the whole
        // block costs nothing an integrator would notice.
        let pass_start = std::time::Instant::now();
        debug!(
            "pass: {} spp, seed {}, {}, adaptive {} (min {} spp, variance threshold {}, \
             neighbour tolerance {}, {} rounds), filter {} radius {}, strategy {:?}, guiding {}",
            cfg.spp,
            cfg.seed,
            if cfg.tiled {
                "16x16 tiles"
            } else {
                "scanlines"
            },
            cfg.adaptive,
            min_spp,
            self.settings.variance_threshold,
            tolerance,
            rounds,
            self.settings.pixel_filter.name(),
            self.settings.pixel_filter.radius(),
            self.settings.sampling_strategy,
            match gctx {
                Some(g) if g.training => "training",
                Some(_) => "guided",
                None => "off",
            },
        );

        let tiles = if cfg.tiled {
            generate_tiles(w, h, TILE)
        } else {
            generate_rows(w, h)
        };
        let mut units: Vec<Unit> = tiles
            .into_iter()
            .map(|tile| Unit::new(tile, layout, cam))
            .collect();
        let total = units.len() as u64 + rounds as u64;
        // Incremented and reported under one lock, so the callback sees
        // completions in increasing order even though units finish on many
        // threads at once (see `ProgressCallback`). Taken once per unit,
        // which no render will notice.
        let done = std::sync::Mutex::new(0u64);
        let report = |n: &mut u64| {
            *n += 1;
            if let Some(cb) = progress {
                cb(*n, total);
            }
        };
        let route_ctx = layout.and_then(|l| l.route.clone());
        let scratch = || {
            let mut s = PathScratch::new(self.settings.max_depth as usize);
            s.route_ctx = route_ctx.clone();
            s
        };

        // First sweep: every pixel to the first check point (or to the
        // budget). The path scratch is held per rayon worker rather than
        // per unit; one buffer serves every sample of every pixel it sees.
        units
            .par_iter_mut()
            .for_each_init(scratch, |scratch, unit| {
                // Stamped once per `AOV` and chosen per unit, not per pixel:
                // a per-pixel branch on the film cost the beauty-only render
                // 0.02% of its instructions (callgrind, cornellbox at 2 spp).
                macro_rules! sweep {
                    ($aov:literal) => {
                        unit.for_each_pixel(|i, j, p, work, st| {
                            self.advance::<$aov>(
                                profiling, i, j, p, &cfg, &filter, gctx, work, scratch, st,
                                sweep_to,
                            );
                            st.finish_round(cfg.spp, threshold);
                        })
                    };
                }
                if aov {
                    sweep!(true)
                } else {
                    sweep!(false)
                }
                // Once per unit, and a no-op unless `--profile` is on.
                profile::flush();
                report(&mut done.lock().unwrap_or_else(|e| e.into_inner()));
            });

        // The frozen buffers the decisions read: every pixel's index and
        // whether it is still sampling, in image order.
        let mut index = vec![f32::INFINITY; w * h];
        let mut active = vec![false; w * h];
        for &target in &schedule {
            let mut any_active = false;
            for unit in &units {
                unit.for_each_pixel_ref(|i, j, st| {
                    index[j * w + i] = st.index;
                    active[j * w + i] = !st.stopped;
                    any_active |= !st.stopped;
                });
            }
            if !any_active {
                break;
            }
            let (index, active) = (&index, &active);
            units
                .par_iter_mut()
                .for_each_init(scratch, |scratch, unit| {
                    // Per unit, as in the first sweep.
                    macro_rules! round {
                        ($aov:literal) => {
                            unit.for_each_pixel(|i, j, p, work, st| {
                                if st.stopped {
                                    return;
                                }
                                // The stop rule: past the minimum (always, by
                                // now), its own test, and no still-sampling
                                // cross neighbour much less converged than it is.
                                if st.converged {
                                    if held_by_neighbour(index, active, w, h, i, j, tolerance) {
                                        st.held = true;
                                    } else {
                                        st.stopped = true;
                                        return;
                                    }
                                }
                                self.advance::<$aov>(
                                    profiling, i, j, p, &cfg, &filter, gctx, work, scratch, st,
                                    target,
                                );
                                st.finish_round(cfg.spp, threshold);
                            })
                        };
                    }
                    if aov {
                        round!(true)
                    } else {
                        round!(false)
                    }
                    profile::flush();
                });
            report(&mut done.lock().unwrap_or_else(|e| e.into_inner()));
        }
        // An early finish still walks the callback to the total, one step at
        // a time, as the contract says.
        {
            let mut n = done.lock().unwrap_or_else(|e| e.into_inner());
            while *n < total {
                report(&mut n);
            }
        }

        // Units finish in unit order, but what the pass hands on — the
        // guiding field's training samples and the pass variance, an f64
        // sum — is gathered in *scanline* order (rows top-down, pixels left
        // to right), whichever the unit shape. Both are order-dependent in
        // floating point (the SD-tree accumulates the samples it is given),
        // so this is what keeps a guided render bit-identical whichever
        // order the pixels were rendered in.
        //
        // The unit results are replayed in that order straight from the
        // tile grid (`generate_tiles` and `generate_rows` emit tile rows by
        // increasing `y`, each left to right, so walking it backwards by
        // row gives rows in scanline order). Nothing full-frame is copied
        // to do it: the samples stay in the unit buffers, and only their
        // scanline-order runs are recorded, one per unit per row.
        // The AOVs need no ordering: each pixel's planes were accumulated in
        // its own sample order, so copying them in any order is exact.
        let film = layout.map(|layout| {
            let mut film = AovFilm::new(layout, w, h);
            for unit in &units {
                let aov = unit
                    .work
                    .aov
                    .as_ref()
                    .expect("a layout gives every unit planes");
                unit.for_each_pixel_ref(|i, j, st| {
                    let p = (j - unit.tile.y) * unit.tile.width + (i - unit.tile.x);
                    film.store(aov, p, i, j, st.weight_sum, st.taken, st.estimate().1);
                });
            }
            film
        });
        let training = units.iter().any(|u| !u.work.samples.is_empty());
        let tiles_x = units.iter().take_while(|u| u.tile.y == 0).count().max(1);
        for unit in &units {
            rays.merge(&unit.work.rays);
        }
        for ty in (0..units.len() / tiles_x).rev() {
            let row = ty * tiles_x..(ty + 1) * tiles_x;
            let (y0, rows) = (units[row.start].tile.y, units[row.start].tile.height);
            for j in (y0..y0 + rows).rev() {
                for k in row.clone() {
                    let unit = &units[k];
                    let tile = &unit.tile;
                    for i in tile.x..tile.x + tile.width {
                        let p = (j - tile.y) * tile.width + (i - tile.x);
                        let st = &unit.pixels[p];
                        let (color, var) = st.estimate();
                        buffer.set_pixel(i, j, color);
                        var_map[j * w + i] = var;
                        variance_sum += var;
                        if cfg.adaptive {
                            rays.adaptive_pixels += 1;
                            rays.adaptive_samples += st.taken as u64;
                            if st.taken < cfg.spp {
                                rays.early_stopped += 1;
                            }
                            if st.held {
                                rays.neighbour_held += 1;
                            }
                            rays.spp_min = if rays.adaptive_pixels == 1 {
                                st.taken
                            } else {
                                rays.spp_min.min(st.taken)
                            };
                            rays.spp_max = rays.spp_max.max(st.taken);
                        }
                        // Where this pixel's samples sit in its unit's
                        // buffer: from where the previous pixel's ended.
                        let start = if p == 0 {
                            0
                        } else {
                            unit.pixels[p - 1].samples_end
                        };
                        let end = st.samples_end;
                        if training && end > start {
                            all_samples.push_run(k as u32, start, end);
                        }
                    }
                }
            }
        }
        all_samples.buffers = units.into_iter().map(|u| u.work.samples).collect();

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
            film,
            all_samples,
            PassStats {
                variance: variance_sum / pixel_count,
                var_map,
                rays,
            },
        )
    }

    /// [`Renderer::advance_pixel`], profiled or not: `profiling` is read
    /// once per pass and `AOV` chosen once per unit, so each sample runs a
    /// function that carries neither. With `AOV`, first points the unit's
    /// film planes at pixel `p`.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn advance<const AOV: bool>(
        &self,
        profiling: bool,
        i: usize,
        j: usize,
        p: usize,
        cfg: &PassConfig,
        filter: &FilterSampler,
        gctx: Option<&GuidingContext>,
        work: &mut UnitWork,
        scratch: &mut PathScratch,
        st: &mut PixelState,
        target: u32,
    ) {
        if AOV && let Some(planes) = work.aov.as_mut() {
            planes.pixel = p;
        }
        if profiling {
            self.advance_pixel::<true, AOV>(i, j, cfg, filter, gctx, work, scratch, st, target);
        } else {
            self.advance_pixel::<false, AOV>(i, j, cfg, filter, gctx, work, scratch, st, target);
        }
    }

    /// Traces pixel `(i, j)`'s samples from `state.taken` up to `target`,
    /// accumulating into `state` and the unit's sample buffer and counters.
    /// A sample depends only on `(i, j, seed, sample index)`, never on when
    /// it is traced, so advancing in steps is the same as one loop.
    ///
    /// With `AOV`, each sample's first hit also goes into the unit's AOV
    /// planes (at the pixel `advance_dispatch` set), with the sample's own
    /// film offset and weight.
    #[allow(clippy::too_many_arguments)]
    fn advance_pixel<const PROFILE: bool, const AOV: bool>(
        &self,
        i: usize,
        j: usize,
        cfg: &PassConfig,
        filter: &FilterSampler,
        gctx: Option<&GuidingContext>,
        unit: &mut UnitWork,
        scratch: &mut PathScratch,
        state: &mut PixelState,
        target: u32,
    ) {
        let _main = profile::scope_if::<PROFILE>(Section::MainLoop);

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

        let path_cx = PathContext {
            world: &self.world,
            lights: &self.lights,
            volumes: &self.volumes,
            depth: self.settings.max_depth as i32,
            strategy: self.settings.sampling_strategy,
            indirect_clamp: self.settings.indirect_clamp,
            guiding: gctx,
            light_samples: self.settings.light_samples,
            light_samples_indirect: self.settings.light_samples_indirect,
        };
        for sample in state.taken..target {
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
            unit.rays.camera_rays += 1;
            let color = trace_path::<PROFILE, AOV>(
                &r,
                &path_cx,
                root,
                &mut unit.samples,
                scratch,
                &mut unit.rays,
            ) * (wx * wy);
            if AOV && let Some(planes) = unit.aov.as_mut() {
                let extras = SampleExtras {
                    lpe: &scratch.route.out,
                    albedo: scratch.route.albedo,
                    diffuse_filter: scratch.route.diffuse_filter,
                };
                planes.add(&scratch.first, &extras, fx, fy, wx * wy);
            }
            state.sum += color;
            state.weight_sum += wx * wy;
            let lum = self.lights.luma().of(color) as f64;
            state.lum_sum += lum;
            state.lum_sq += lum * lum;
        }
        state.taken = target;
        state.samples_end = unit.samples.len() as u32;
    }
}

/// One pixel's accumulators across the rounds of a pass, plus where it
/// stands in the adaptive stop rule.
#[derive(Clone, Copy)]
struct PixelState {
    sum: Vec3A,
    /// FIS weight sum (see `filter.rs`): the pixel estimate is the
    /// weighted average Σwᵢ·Lᵢ / Σwᵢ. For box and triangle every wᵢ is
    /// exactly 1.0, so the sum is exactly `taken as f32` and the estimate
    /// is the plain mean — box at radius 0.5 stays bit-identical to the
    /// historical unweighted, unfiltered estimator.
    weight_sum: f32,
    lum_sum: f64,
    lum_sq: f64,
    taken: u32,
    /// Where this pixel's training samples end in its unit's buffer.
    samples_end: u32,
    /// Convergence index at `taken` samples: relative standard error of the
    /// mean over the threshold, `+∞` before the pixel has seen light. The
    /// value its neighbours compare against — f32, so every decision is a
    /// function of one stored number per pixel.
    index: f32,
    /// Passed its own test at `taken` samples — the f64 comparison, kept
    /// apart from the rounded index so a pixel at the boundary decides
    /// exactly as it did when it ran alone.
    converged: bool,
    stopped: bool,
    /// Held back by a less converged neighbour at least once.
    held: bool,
}

impl PixelState {
    fn new() -> Self {
        PixelState {
            sum: Vec3A::ZERO,
            weight_sum: 0.0,
            lum_sum: 0.0,
            lum_sq: 0.0,
            taken: 0,
            samples_end: 0,
            index: f32::INFINITY,
            converged: false,
            stopped: false,
            held: false,
        }
    }

    /// Unbiased variance of the pixel-mean luminance over `taken` samples.
    fn var_of_mean(&self) -> f64 {
        let n = self.taken as f64;
        ((self.lum_sq - self.lum_sum * self.lum_sum / n) / (n - 1.0) / n).max(0.0)
    }

    /// After a round's samples: out of budget stops the pixel; otherwise
    /// its own test and its index are recomputed for the next decision.
    fn finish_round(&mut self, spp: u32, threshold: f64) {
        if self.taken >= spp {
            self.stopped = true;
            return;
        }
        // Zero-signal gate. A pixel whose every sample so far is exactly
        // zero has a measured variance of zero, which the test below would
        // read as perfect convergence — but nothing has been observed, and
        // the pixel would be written black whatever the budget (ALab's
        // glassware, where most paths legitimately carry nothing). The
        // gate is on `lum_sq`, not `lum_sum`: Mitchell's negative filter
        // lobes can leave a lit pixel with `lum_sum <= 0`, whereas a sum of
        // squares is zero exactly when every sample's luminance was.
        if self.taken < 2 || self.lum_sq <= 0.0 {
            self.index = f32::INFINITY;
            self.converged = false;
            return;
        }
        let n = self.taken as f64;
        let mean = (self.lum_sum / n).max(1e-4);
        let rel = self.var_of_mean().sqrt() / mean;
        self.converged = rel < threshold;
        self.index = (rel / threshold) as f32;
    }

    /// The pixel's colour and the variance of its mean luminance.
    fn estimate(&self) -> (Vec3A, f64) {
        let variance = if self.taken >= 2 {
            self.var_of_mean()
        } else {
            f64::INFINITY
        };
        // Weighted-average film estimator. A Mitchell pixel whose few
        // samples all landed on negative lobes could zero the denominator;
        // the plain mean is the sane fallback there.
        let mean = if self.weight_sum > 0.0 {
            self.sum / self.weight_sum
        } else {
            self.sum / self.taken as f32
        };
        (mean, variance)
    }
}

/// The `taken` count every active pixel reaches after each round of an
/// adaptive pass at `spp`, once the first sweep has brought it to
/// `first_check`. Empty when the budget never reaches the first check.
///
/// Batches grow 25% a round — `max(4, taken / 4)` — so a 1024 spp pass from
/// 32 runs 16 rounds rather than 248: each round is a fork/join whose tail
/// (the last unit finishing while every other worker idles) cost 8% of a
/// cornellbox render at a fixed batch of 4, with callgrind counting fewer
/// instructions, not more. The price is overshoot: a pixel that converges
/// mid-batch stops at the batch's end, at most 25% past what it had taken.
/// A pure function of its inputs, computed once per pass, so the round
/// count — and the progress total — is known before the pass starts.
fn batch_schedule(spp: u32, first_check: u32) -> Vec<u32> {
    let mut schedule = Vec::new();
    let mut taken = first_check;
    while taken < spp {
        // Add the smaller of the batch and what is left, never the batch
        // and then a cap: `taken + batch` can overflow near `u32::MAX`.
        taken += (taken / 4).max(4).min(spp - taken);
        schedule.push(taken);
    }
    schedule
}

/// The cross-neighbour rule: is pixel `(x, y)` held back by one of its up,
/// down, left or right neighbours that is still sampling and whose
/// convergence index exceeds its own by more than `tolerance`? One-sided —
/// a more converged neighbour never holds — and absolute, in index units.
/// A stopped neighbour never holds: converged or out of budget, more
/// samples here would not change it. Diagonals are not compared, and a
/// neighbour outside the image does not exist. A negative tolerance skips
/// the comparison altogether, so "off" is the per-pixel stop with no
/// dependence on neighbour values. `+∞ − finite = +∞` holds; two `+∞`
/// pixels never get here, since each fails its own test.
fn held_by_neighbour(
    index: &[f32],
    active: &[bool],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    tolerance: f32,
) -> bool {
    if tolerance < 0.0 {
        return false;
    }
    let own = index[y * width + x];
    let holds = |q: usize| active[q] && index[q] - own > tolerance;
    (x > 0 && holds(y * width + x - 1))
        || (x + 1 < width && holds(y * width + x + 1))
        || (y > 0 && holds((y - 1) * width + x))
        || (y + 1 < height && holds((y + 1) * width + x))
}

/// What a work unit's samples write besides the pixel accumulators: its
/// training samples and its counters. Private to the unit, so no two
/// threads share a counter and there is nothing to synchronise.
#[derive(Default)]
struct UnitWork {
    samples: Vec<SampleData>,
    rays: RayStats,
    /// The unit's AOV planes, beside (not inside) its `PixelState`s, so a
    /// render without AOVs keeps the pixel state it always had.
    aov: Option<UnitAov>,
}

/// One work unit of a pass — a tile or a row — and its pixels' state.
struct Unit {
    tile: Tile,
    /// Row-major within the tile.
    pixels: Vec<PixelState>,
    work: UnitWork,
}

impl Unit {
    fn new(tile: Tile, layout: Option<&AovLayout>, cam: CameraFrame) -> Self {
        let pixels = tile.width * tile.height;
        Unit {
            pixels: vec![PixelState::new(); pixels],
            tile,
            work: UnitWork {
                aov: layout.map(|l| UnitAov::new(l, cam, pixels)),
                ..UnitWork::default()
            },
        }
    }

    /// Every pixel of the unit, with its image coordinates and its index
    /// within the unit, alongside the unit's own buffers.
    fn for_each_pixel(
        &mut self,
        mut f: impl FnMut(usize, usize, usize, &mut UnitWork, &mut PixelState),
    ) {
        let tile = &self.tile;
        for (p, st) in self.pixels.iter_mut().enumerate() {
            let (i, j) = (tile.x + p % tile.width, tile.y + p / tile.width);
            f(i, j, p, &mut self.work, st);
        }
    }

    fn for_each_pixel_ref(&self, mut f: impl FnMut(usize, usize, &PixelState)) {
        for (p, st) in self.pixels.iter().enumerate() {
            let (i, j) = (
                self.tile.x + p % self.tile.width,
                self.tile.y + p / self.tile.width,
            );
            f(i, j, st);
        }
    }
}

/// The inverse-variance weight of each pass and their sum — what
/// [`Renderer::blend_passes`] and [`AovFilm::blend`] both apply, so a guided
/// render's AOVs are the same combination of passes as its beauty. A pass
/// whose variance could not be estimated weighs nothing.
fn blend_weights(passes: &[(Buffer, f64)]) -> (Vec<f64>, f64) {
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
    let total = weights.iter().sum();
    (weights, total)
}

/// Per-pixel luminance of the inverse-variance blend of `passes` — the
/// reference image the guiding efficiency estimate normalizes against.
/// Un-weightable passes (non-finite or zero variance) contribute nothing;
/// if no pass is weightable the result is black and the floor in
/// `mean_relative_error` takes over.
fn blend_luminance(
    passes: &[(Buffer, f64)],
    width: usize,
    height: usize,
    luma: utils::Luma,
) -> Vec<f64> {
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
            out[y * width + x] = luma.of(c) as f64;
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

/// The scanline work units: one `width`×1 tile per row, in the same order
/// `generate_tiles` emits (rows by increasing `y`), so both unit shapes
/// replay in scanline order through the one gather.
fn generate_rows(image_width: usize, image_height: usize) -> Vec<Tile> {
    (0..image_height)
        .map(|y| Tile {
            x: 0,
            y,
            width: image_width,
            height: 1,
        })
        .collect()
}

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
