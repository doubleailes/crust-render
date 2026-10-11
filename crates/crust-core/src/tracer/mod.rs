use std::sync::atomic::{AtomicBool, Ordering};

use glam::Vec3A;
use rayon::prelude::*;
use tracing::debug;

use crate::aov::{AovFilm, AovLayout, AovRequest, CameraFrame, SampleExtras, UnitAov};
use crate::buffer::Buffer;
use crate::camera::Camera;
use crate::filter::FilterSampler;
use crate::profile::{self, Section};
use crate::rt_world::World;
use crate::stats::RayStats;
use crate::volume::Volumes;
use crate::{LightList, LightSelection, PathSampler};

mod control;
mod path;
pub(crate) use path::{past_medium_boundaries, surface_visibility};
mod region;
mod route;
mod settings;

use path::{K_CAMERA, K_TIME, PathContext, ray_cones_enabled, trace_path};

pub use control::{RenderControl, RenderOutcome};
pub use path::{ray_color, ray_color_with_light_samples};
pub use region::PixelRect;
pub use settings::{
    DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE, DEFAULT_INDIRECT_CLAMP, DEFAULT_LIGHT_SAMPLES,
    MAX_LIGHT_SAMPLES, RenderSettings, SamplingStrategy,
};

pub(crate) use path::PathScratch;
pub(crate) use route::RouteCtx;

/// Render-progress callback: invoked with `(completed, total)` steps as a
/// pass advances. Each work unit (scanline row, or tile under bucket
/// rendering) has one step per sample per pixel it is scheduled — or, past
/// 64 spp, 64 steps shared out in proportion to the samples — reported as
/// the unit finishes the stage of the first sweep or the adaptive round that
/// takes it there, so the steps track the scheduled work and `total` is
/// units × min(spp, 64). Called
/// from worker threads, hence `Sync`, but never concurrently and always
/// with `completed` increasing by one — a host can show the last value it was
/// given. A completed render reaches `total` (an adaptive render whose
/// pixels all stopped early walks the rest at once); a cancelled one stops
/// where it was. Presentation (progress bars,
/// logging) is the caller's concern — the engine has no UI dependencies.
pub type ProgressCallback<'a> = &'a (dyn Fn(u64, u64) + Sync);

/// What [`Renderer::render_with_control`] returns.
pub struct Rendered {
    /// The beauty, over the render's region.
    pub buffer: Buffer,
    /// The AOVs, when the call passed a request: an empty film when the
    /// request needs none, as [`Renderer::render_with_aovs`] returns.
    pub film: Option<AovFilm>,
    /// What the integrator did — every sample traced, a cancelled render's
    /// included.
    pub rays: RayStats,
    /// Whether the render completed or was cancelled.
    pub outcome: RenderOutcome,
}

/// Parameters of one full-frame render pass.
#[derive(Clone, Copy)]
struct PassConfig {
    spp: u32,
    seed: u32,
    tiled: bool,
    adaptive: bool,
    /// Whether camera rays draw a shutter time: something moves and motion
    /// blur is on (`RenderSettings::motion_blur`). Decided once per pass,
    /// not per pixel, so the beauty-only hot path reads one flag as it
    /// always did.
    shutter: bool,
    /// Whether the scene has a shadow-linked light with a link twin: which
    /// `trace_path` instantiation (`TWINS`) the pass runs. Read once here
    /// rather than per pixel.
    twins: bool,
    /// Time each work unit ([`Instruments::tile_times`]).
    timed: bool,
    /// The clamp whose effect this pass measures without applying it
    /// ([`Instruments::clamp`]).
    measure_clamp: Option<f32>,
    /// Sweep to the first check point in stages of 1, 2, 4, … spp
    /// ([`sweep_stages`]) rather than in one go. Scheduling only: the image
    /// is bit-identical either way. On for every pass; off only for the
    /// unstaged reference the tests pin staging against
    /// ([`Instruments::unstaged`]).
    staged: bool,
}

/// What a diagnostic render measures beside its image
/// ([`crate::diagnostic`]). The default measures nothing, and is what every
/// public render entry point passes: each instrument costs a branch per work
/// unit or per camera sample only while it is on.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Instruments {
    /// Wall-clock seconds per work unit (16×16 tile or row).
    pub(crate) tile_times: bool,
    /// While the render's own clamp is off: what this indirect clamp would
    /// remove, per pixel, measured on the final pass without applying it.
    pub(crate) clamp: Option<f32>,
    /// Return the per-pixel variance of the image.
    pub(crate) variance: bool,
    /// Sweep the final pass to its first check point in one go, as before
    /// the sweep was staged: the reference the staged sweep is pinned
    /// bit-identical against. For tests; no render sets it.
    pub(crate) unstaged: bool,
}

/// What [`Instruments::clamp`] measured on the pass it watched.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ClampMeasure {
    /// Σ over pixels of the luminance the clamp would remove from the pixel
    /// estimate (the beauty's own estimator over each sample's removal).
    pub(crate) removed_luminance: f64,
    /// Pixels at least one of whose samples the clamp would change.
    pub(crate) pixels_touched: u64,
    pub(crate) pixels: u64,
}

/// A render with its measurements — what [`Renderer::render_measured`]
/// returns. Every per-pixel plane covers the render's region in raster
/// space (rows bottom-up), indexed through [`PixelRect::index`].
pub(crate) struct Measured {
    pub(crate) buffer: Buffer,
    pub(crate) film: Option<AovFilm>,
    pub(crate) rays: RayStats,
    /// Variance of each pixel's luminance mean; empty unless
    /// [`Instruments::variance`] asked for it.
    pub(crate) var_map: Vec<f64>,
    /// Each work unit's rectangle (raster space) and seconds; empty unless
    /// [`Instruments::tile_times`] asked for them.
    pub(crate) tiles: Vec<(PixelRect, f64)>,
    pub(crate) clamp: ClampMeasure,
    /// Wall-clock seconds of the pass.
    pub(crate) render_s: f64,
    /// Whether a [`RenderControl`] cut the render short.
    pub(crate) outcome: RenderOutcome,
}

impl Measured {
    /// Multiplies the result by the render camera's exposure `scale`: the
    /// beauty, the radiance planes and the luminance the clamp counter saw it
    /// remove by `scale`, the variances (the AOV film's and the per-pixel
    /// `var_map`) by its square. Everything a caller divides by the beauty
    /// is then in the beauty's units, so `crust diagnostic`'s clamp share
    /// does not move with the exposure.
    fn apply_exposure(&mut self, scale: f32) {
        self.buffer.scale(scale);
        self.clamp.removed_luminance *= f64::from(scale);
        if let Some(film) = &mut self.film {
            film.apply_exposure(scale);
        }
        let square = f64::from(scale) * f64::from(scale);
        self.var_map.iter_mut().for_each(|v| *v *= square);
    }
}

/// Image-quality statistics of one render pass.
struct PassStats {
    /// Per-pixel variance of the pixel-mean luminance, row-major.
    var_map: Vec<f64>,
    /// Integrator work this pass did.
    rays: RayStats,
    /// Each unit's rectangle and seconds, when the pass was timed.
    tiles: Vec<(PixelRect, f64)>,
    clamp: ClampMeasure,
    /// A [`RenderControl`] stopped the pass before every pixel took what it
    /// was scheduled to.
    interrupted: bool,
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
    /// A renderer over the scene, configured for `settings` — exactly
    /// [`Renderer::reconfigure`] on a fresh one, so the two cannot drift.
    pub fn new(camera: Camera, world: World, lights: LightList, settings: RenderSettings) -> Self {
        let mut renderer = Renderer {
            camera,
            world,
            lights,
            settings,
            volumes: Volumes::default(),
        };
        renderer.reconfigure(settings);
        renderer
    }

    /// Switches to `settings`, keeping the camera, the world, the volumes
    /// and every asset cache: rebuilds what depends on the settings — the
    /// light selection, the `learned` pre-pass included — and nothing else.
    /// Every other setting is read per pass. Returns the time the light
    /// selection took to build (the `learned` pre-pass; next to nothing
    /// otherwise), which is setup rather than sampling.
    ///
    /// The selection is built here rather than by the importer so that
    /// every way of assembling a scene — USD, the procedural fallback, a
    /// test — gets the selection its settings ask for.
    pub fn reconfigure(&mut self, settings: RenderSettings) -> std::time::Duration {
        self.settings = settings;
        self.select_lights()
    }

    /// [`reconfigure`](Self::reconfigure), keeping the light selection when
    /// what it is built from is unchanged: the strategy, the resolution and
    /// the frame (the `learned` pre-pass is a deterministic function of
    /// those and the scene). A session that only changes the samples or the
    /// region then pays no second pre-pass, and renders bit-identically to a
    /// rebuild. `reconfigure` itself always rebuilds, because `crust
    /// diagnostic` reports that setup as each trial's cost.
    pub fn retune(&mut self, settings: RenderSettings) -> std::time::Duration {
        let builds_from = |s: &RenderSettings| (s.light_selection(), s.width, s.height, s.frame);
        let keep = builds_from(&self.settings) == builds_from(&settings);
        self.settings = settings;
        if keep {
            std::time::Duration::ZERO
        } else {
            self.select_lights()
        }
    }

    /// Builds the light selection the settings ask for (training the
    /// `learned` one), returning how long it took.
    fn select_lights(&mut self) -> std::time::Duration {
        let started = std::time::Instant::now();
        let settings = self.settings;
        self.lights.select_by(settings.light_selection());
        if settings.light_selection() == LightSelection::Learned {
            match crate::light_cache::train(
                &self.world,
                &self.camera,
                &self.lights,
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
                    self.lights.set_cache(cache);
                }
                None => debug!("learned light selection: nothing to learn, picking by power"),
            }
        }
        debug!(
            "Renderer over {} geometries, {} light(s) picked by {:?}, {} rayon thread(s)",
            self.world.count(),
            self.lights.count(),
            self.lights.selection(),
            rayon::current_num_threads()
        );
        started.elapsed()
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
        self.render_impl(false, None, None, Instruments::default(), None)
            .buffer
    }

    pub fn render_with_tiles(&self) -> Buffer {
        self.render_impl(true, None, None, Instruments::default(), None)
            .buffer
    }

    /// Renders with a progress callback — see [`ProgressCallback`].
    pub fn render_with_progress(&self, tiled: bool, progress: ProgressCallback) -> Buffer {
        self.render_impl(tiled, Some(progress), None, Instruments::default(), None)
            .buffer
    }

    /// As [`Renderer::render_with_progress`], also returning what the
    /// integrator did — see [`RayStats`]. Counting is unconditional and
    /// costs an increment per ray, so this is the same render either way;
    /// the other entry points simply discard the numbers.
    pub fn render_with_stats(&self, tiled: bool, progress: ProgressCallback) -> (Buffer, RayStats) {
        let m = self.render_impl(tiled, Some(progress), None, Instruments::default(), None);
        (m.buffer, m.rays)
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
        let layout = self.layout_for(request);
        let m = self.render_impl(
            tiled,
            Some(progress),
            layout.as_ref(),
            Instruments::default(),
            None,
        );
        let film = match m.film {
            Some(film) => film,
            None => AovFilm::new(&AovLayout::default(), self.settings.raster_region()),
        };
        (m.buffer, film, m.rays)
    }

    /// The render a host watches and can stop: as
    /// [`Renderer::render_with_aovs`] (or [`Renderer::render_with_stats`]
    /// without a `request`), publishing the beauty into `control` as it
    /// improves and stopping when `control` is cancelled — see
    /// [`RenderControl`] for the contract.
    ///
    /// A render that completes returns exactly what the other entry points
    /// return for the same settings, bit for bit. A cancelled one returns the
    /// image, the AOVs and the counters of the samples it traced, each pixel
    /// estimated from its own samples, a pixel that took none black (and its
    /// AOVs at their clear values). `progress` reports as it does for
    /// the other entry points, and stops where the render stopped.
    pub fn render_with_control(
        &self,
        tiled: bool,
        progress: Option<ProgressCallback>,
        request: Option<&AovRequest>,
        control: &RenderControl,
    ) -> Rendered {
        let layout = request.and_then(|r| self.layout_for(r));
        let m = self.render_impl(
            tiled,
            progress,
            layout.as_ref(),
            Instruments::default(),
            Some(control),
        );
        let film = request.map(|_| {
            m.film.unwrap_or_else(|| {
                AovFilm::new(&AovLayout::default(), self.settings.raster_region())
            })
        });
        Rendered {
            buffer: m.buffer,
            film,
            rays: m.rays,
            outcome: m.outcome,
        }
    }

    /// A tiled render of `request`'s AOVs (none when `None`), with the
    /// measurements `instruments` asks for — the diagnostic's one way to
    /// render. Silent: no progress is reported.
    pub(crate) fn render_measured(
        &self,
        request: Option<&AovRequest>,
        instruments: Instruments,
    ) -> Measured {
        let layout = request.and_then(|r| self.layout_for(r));
        self.render_impl(true, None, layout.as_ref(), instruments, None)
    }

    /// The film layout `request` needs, or `None` when it needs no film (no
    /// products, or only a 3-channel beauty): such a render takes the
    /// beauty-only path.
    fn layout_for(&self, request: &AovRequest) -> Option<AovLayout> {
        if !request.needs_film() {
            return None;
        }
        let mut layout = AovLayout::new(request);
        layout.luma = self.lights.luma();
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
        Some(layout)
    }

    fn render_impl(
        &self,
        tiled: bool,
        progress: Option<ProgressCallback>,
        layout: Option<&AovLayout>,
        instruments: Instruments,
        control: Option<&RenderControl>,
    ) -> Measured {
        let start = std::time::Instant::now();
        let cfg = self.final_pass_config(tiled, instruments);
        let (buffer, film, pass) = self.render_pass(cfg, progress, layout, control);
        let mut m = Measured {
            outcome: if pass.interrupted {
                RenderOutcome::Cancelled
            } else {
                RenderOutcome::Completed
            },
            buffer,
            film,
            rays: pass.rays,
            var_map: if instruments.variance {
                pass.var_map
            } else {
                Vec::new()
            },
            tiles: pass.tiles,
            clamp: pass.clamp,
            render_s: start.elapsed().as_secs_f64(),
        };
        // The camera's exposure multiplies the resolved image: the samples,
        // the clamp and the stopping rule all worked in scene radiance.
        // Scale 1, every camera without an exposure, is skipped, so its
        // image is the bits it was.
        let scale = self.settings.exposure_scale;
        if scale != 1.0 {
            m.apply_exposure(scale);
        }
        m
    }

    /// Whether camera rays sample the shutter: `ray.time` is read by exactly
    /// one thing — a moving instance interpolating its transform — so on a
    /// scene where nothing moves every value of it gives the same image, and
    /// `disableMotionBlur` asks for the same shutter-open image on a scene
    /// that does move (the motion stays in the scene for the `motionvector`
    /// AOV).
    fn shutter(&self) -> bool {
        self.world.has_motion() && self.settings.motion_blur
    }

    /// Config of a final (image-quality) pass: full budget, adaptive
    /// sampling.
    fn final_pass_config(&self, tiled: bool, instruments: Instruments) -> PassConfig {
        PassConfig {
            spp: self.settings.samples_per_pixel,
            seed: self.settings.frame as u32,
            tiled,
            adaptive: true,
            shutter: self.shutter(),
            twins: !self.lights.twinned_lights().is_empty(),
            timed: instruments.tile_times,
            // Only while the render's own clamp is off: with it on there is
            // nothing left for the measurement to remove.
            measure_clamp: instruments
                .clamp
                .filter(|_| self.settings.indirect_clamp.is_none()),
            staged: !instruments.unstaged,
        }
    }

    /// One pass over the render's region at `spp` samples per pixel. Returns
    /// the image, its AOVs (when `layout` asks for any) and the pass's
    /// [`PassStats`] — every one of them sized to the region. Rays, cones and sampling keys stay those of the full
    /// frame: a region decides only which pixels are traced.
    ///
    /// The work unit is a 16×16 tile or a region-wide row; the two differ in
    /// nothing but the tile list, and a render mode is scheduling only. A
    /// non-adaptive pass sweeps every unit to `spp`; an adaptive pass first
    /// sweeps every pixel to the first check point, then runs **rounds**
    /// over a region-sized convergence-index buffer: each round freezes
    /// every pixel's index and whether it is still sampling, decides against
    /// that frozen buffer which pixels stop (their own test, and the
    /// cross-neighbour rule of [`held_by_neighbour`]), then traces the next
    /// batch of samples ([`batch_schedule`]) for the pixels still active.
    /// Nothing writes the buffer while a decision reads it, so no decision
    /// depends on the order the units run in, and tiles and scanlines stay
    /// bit-identical with the comparison on. A final pass sweeps in stages
    /// ([`PassConfig::staged`]): every unit to 1 spp, then 2, 4, …, then the
    /// sweep's end, with the convergence test run only after the last — so
    /// staging is scheduling too. Each pixel draws the same sample indices,
    /// and checks at the same `taken` values, as it would if it ran alone.
    ///
    /// With a `control`, each unit publishes its pixels' estimates as it
    /// finishes a stage or a round, and every pixel's advance first checks
    /// whether the control was cancelled: a cancelled pass stops scheduling,
    /// and gathers what its units hold.
    fn render_pass(
        &self,
        cfg: PassConfig,
        progress: Option<ProgressCallback>,
        layout: Option<&AovLayout>,
        control: Option<&RenderControl>,
    ) -> (Buffer, Option<AovFilm>, PassStats) {
        let (w, h) = (self.settings.width, self.settings.height);
        // The pixels this pass traces, in raster space (rows bottom-up, as
        // `(i, j)` count them); every per-pixel plane is this size and is
        // indexed through `rect.index`.
        let rect = self.settings.raster_region();
        let mut buffer = Buffer::for_raster_rect(w, h, rect);
        let mut rays = RayStats::default();
        let mut var_map = vec![0.0f64; rect.area()];
        // One tabulation per pass, shared read-only by every worker.
        let filter = FilterSampler::new(self.settings.pixel_filter);
        // Read once per pass and dispatched to one of two monomorphisations
        // of the integrator (see `profile::scope_if`), so an unprofiled
        // render carries no trace of the profiler. The clamp counter lives
        // in the same instrumented instantiation — a branch per camera
        // sample there cost the ordinary render 0.3% of its instructions
        // (callgrind, cornellbox at 2 spp) — so a pass that measures it
        // takes that instantiation too; its sections record nothing unless
        // profiling is on.
        let profiling = profile::enabled() || cfg.measure_clamp.is_some();
        // The same for the film: the AOV instantiation runs only when a
        // product asks for something beyond the beauty.
        let cam = self.camera.frame(w, h);
        let aov = layout.is_some();

        let threshold = self.settings.variance_threshold as f64;
        let tolerance = self.settings.adaptive_neighbour_tolerance;
        let adaptive = cfg.adaptive && threshold > 0.0;
        let (min_spp, first_check) =
            adaptive_check_points(cfg.spp, self.settings.min_samples_per_pixel);
        let sweep_to = if adaptive {
            cfg.spp.min(first_check)
        } else {
            cfg.spp
        };
        // The first sweep's stages: every unit is taken to each in turn.
        let stages = if cfg.staged {
            sweep_stages(sweep_to)
        } else {
            vec![sweep_to]
        };
        // Rounds: one decision plus one batch each, until the budget is
        // spent. None when adaptive sampling is off or never gets to check.
        let schedule = if adaptive {
            batch_schedule(cfg.spp, first_check)
        } else {
            Vec::new()
        };
        let rounds = schedule.len();

        // Per *pass*, never per pixel or per ray: a render runs exactly one,
        // so the whole block costs nothing an integrator would notice.
        let pass_start = std::time::Instant::now();
        debug!(
            "pass: {} spp, seed {}, {}, {} sweep stage(s), adaptive {} (min {} spp, variance \
             threshold {}, neighbour tolerance {}, {} rounds), filter {} radius {}, strategy {:?}",
            cfg.spp,
            cfg.seed,
            if cfg.tiled {
                "16x16 tiles"
            } else {
                "scanlines"
            },
            stages.len(),
            cfg.adaptive,
            min_spp,
            self.settings.variance_threshold,
            tolerance,
            rounds,
            self.settings.pixel_filter.name(),
            self.settings.pixel_filter.radius(),
            self.settings.sampling_strategy,
        );

        let tiles = if cfg.tiled {
            generate_tiles(rect, TILE)
        } else {
            generate_rows(rect)
        };
        let mut units: Vec<Unit> = tiles
            .into_iter()
            .map(|tile| Unit::new(tile, layout, cam, cfg.measure_clamp.is_some()))
            .collect();
        // Each unit's steps follow its samples: the stages add 1, 1, 2, 4, …
        // samples and the rounds a quarter more each, so counting units alone
        // would let the first stages race through the bar. A unit reaching
        // `taken` samples has done `steps_at(taken)` of its `per_unit` steps
        // — one a sample up to `PROGRESS_STEPS`, the same share of the budget
        // past it, so no budget makes the reporting cost scale with it. The
        // last stage or round always targets `spp`, so the steps telescope
        // to `per_unit` a unit.
        let spp = cfg.spp.max(1) as u64;
        let per_unit = spp.min(PROGRESS_STEPS);
        let steps_at = |taken: u32| taken as u64 * per_unit / spp;
        let total = units.len() as u64 * per_unit;
        // Incremented and reported under one lock, so the callback sees
        // completions in increasing order, one at a time, even though units
        // finish on many threads at once (see `ProgressCallback`). Taken once
        // per unit per stage or round, for at most `PROGRESS_STEPS` reports a
        // unit over the pass, which no render will notice.
        let done = std::sync::Mutex::new(0u64);
        let report = |n: &mut u64, steps: u64| match progress {
            Some(cb) => {
                for _ in 0..steps {
                    *n += 1;
                    cb(*n, total);
                }
            }
            None => *n += steps,
        };
        let route_ctx = layout.and_then(|l| l.route.clone());
        let motion_aov = layout.is_some_and(|l| l.motion);
        let scratch = || {
            let mut s = PathScratch::new(self.settings.max_depth as usize);
            s.route_ctx = route_ctx.clone();
            s.motion = motion_aov;
            s
        };
        // Read before every pixel's advance (one relaxed load; nothing at
        // all without a control), so a cancelled render waits only for the
        // advances already in flight, not for a unit — a row, under
        // `--scanline` — or a round.
        let cancelled = || control.is_some_and(RenderControl::is_cancelled);
        // Set when a unit skipped a pixel, or the schedule a stage or a
        // round, because of a cancel: the pass is then incomplete.
        let interrupted = AtomicBool::new(false);
        // A unit's estimates into the control's display, once per unit per
        // stage or round in which it traced anything — unless the control
        // takes no snapshots.
        let display = control.filter(|c| c.takes_snapshots());
        // A snapshot shows the image as it will be written: exposed.
        let exposure = self.settings.exposure_scale;
        let publish = |unit: &Unit| {
            if let Some(control) = display {
                control.publish(w, h, rect, |display| {
                    unit.for_each_pixel_ref(|i, j, st| {
                        display.set_pixel(i, j, st.estimate().0 * exposure)
                    });
                });
            }
        };

        // First sweep: every pixel to the first check point (or to the
        // budget), stage by stage. The path scratch is held per rayon worker
        // rather than per unit; one buffer serves every sample of every
        // pixel it sees.
        // A pass starts with no sample taken.
        if let Some(control) = control {
            control.reach(0);
        }
        let last_stage = stages.len() - 1;
        for (s, &target) in stages.iter().enumerate() {
            if cancelled() {
                interrupted.store(true, Ordering::Relaxed);
                break;
            }
            let added = steps_at(target) - steps_at(if s == 0 { 0 } else { stages[s - 1] });
            // The convergence test runs once, after the last stage: at the
            // `taken` an unstaged sweep tests at.
            let finish = s == last_stage;
            units
                .par_iter_mut()
                .for_each_init(scratch, |scratch, unit| {
                    // Per unit, and only when the pass is timed.
                    let started = cfg.timed.then(std::time::Instant::now);
                    let (mut traced, mut skipped) = (false, false);
                    // Stamped once per `AOV` and chosen per unit, not per pixel:
                    // a per-pixel branch on the film cost the beauty-only render
                    // 0.02% of its instructions (callgrind, cornellbox at 2 spp).
                    macro_rules! sweep {
                        ($aov:literal) => {
                            unit.for_each_pixel(|i, j, p, work, st| {
                                if cancelled() {
                                    skipped = true;
                                    return;
                                }
                                self.advance::<$aov>(
                                    profiling, i, j, p, &cfg, &filter, work, scratch, st, target,
                                );
                                if finish {
                                    st.finish_round(cfg.spp, threshold);
                                }
                                traced = true;
                            })
                        };
                    }
                    if aov {
                        sweep!(true)
                    } else {
                        sweep!(false)
                    }
                    if let Some(t) = started {
                        unit.work.secs += t.elapsed().as_secs_f64();
                    }
                    // Once per unit, and a no-op unless `--profile` is on.
                    profile::flush();
                    if traced {
                        publish(unit);
                    }
                    // Reported only while the render runs: a cancelled
                    // render's progress stays where it stopped.
                    if skipped {
                        interrupted.store(true, Ordering::Relaxed);
                    } else if !cancelled() {
                        report(&mut done.lock().unwrap_or_else(|e| e.into_inner()), added);
                    }
                });
            // Every pixel has `target` samples now, unless a cancel cut the
            // stage short.
            if let Some(control) = control
                && !interrupted.load(Ordering::Relaxed)
            {
                control.reach(target);
            }
        }

        // The frozen buffers the decisions read: every pixel's index and
        // whether it is still sampling, in image order.
        let mut index = vec![f32::INFINITY; rect.area()];
        let mut active = vec![false; rect.area()];
        let mut previous = sweep_to;
        for &target in &schedule {
            if interrupted.load(Ordering::Relaxed) {
                break;
            }
            let added = steps_at(target) - steps_at(previous);
            previous = target;
            let mut any_active = false;
            for unit in &units {
                unit.for_each_pixel_ref(|i, j, st| {
                    index[rect.index(i, j)] = st.index;
                    active[rect.index(i, j)] = !st.stopped;
                    any_active |= !st.stopped;
                });
            }
            if !any_active {
                break;
            }
            if cancelled() {
                interrupted.store(true, Ordering::Relaxed);
                break;
            }
            let (index, active) = (&index, &active);
            units
                .par_iter_mut()
                .for_each_init(scratch, |scratch, unit| {
                    let started = cfg.timed.then(std::time::Instant::now);
                    let (mut traced, mut skipped) = (false, false);
                    // Per unit, as in the first sweep.
                    macro_rules! round {
                        ($aov:literal) => {
                            unit.for_each_pixel(|i, j, p, work, st| {
                                if st.stopped {
                                    return;
                                }
                                if cancelled() {
                                    skipped = true;
                                    return;
                                }
                                // The stop rule: past the minimum (always, by
                                // now), its own test, and no still-sampling
                                // cross neighbour much less converged than it is.
                                if st.converged {
                                    if held_by_neighbour(index, active, rect, i, j, tolerance) {
                                        st.held = true;
                                    } else {
                                        st.stopped = true;
                                        return;
                                    }
                                }
                                self.advance::<$aov>(
                                    profiling, i, j, p, &cfg, &filter, work, scratch, st, target,
                                );
                                st.finish_round(cfg.spp, threshold);
                                traced = true;
                            })
                        };
                    }
                    if aov {
                        round!(true)
                    } else {
                        round!(false)
                    }
                    if let Some(t) = started {
                        unit.work.secs += t.elapsed().as_secs_f64();
                    }
                    profile::flush();
                    if traced {
                        publish(unit);
                    }
                    // The round's steps whether or not the unit had a pixel
                    // still sampling — they are what was scheduled — but only
                    // while the render runs, as in the first sweep.
                    if skipped {
                        interrupted.store(true, Ordering::Relaxed);
                    } else if !cancelled() {
                        report(&mut done.lock().unwrap_or_else(|e| e.into_inner()), added);
                    }
                });
        }
        // An early finish still walks the callback to the total, one step at
        // a time, as the contract says; a cancelled pass stops where it was.
        let interrupted = interrupted.into_inner();
        if !interrupted {
            let mut n = done.lock().unwrap_or_else(|e| e.into_inner());
            let left = total - *n;
            report(&mut n, left);
        }

        // The unit results are replayed in scanline order (rows top-down,
        // pixels left to right) straight from the tile grid
        // (`generate_tiles` and `generate_rows` emit tile rows by increasing
        // `y`, each left to right, so walking it backwards by row gives rows
        // in scanline order), whichever the unit shape. Nothing full-frame
        // is copied to do it.
        // The AOVs need no ordering: each pixel's planes were accumulated in
        // its own sample order, so copying them in any order is exact.
        let film = layout.map(|layout| {
            let mut film = AovFilm::new(layout, rect);
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
        let tiles_x = units
            .iter()
            .take_while(|u| u.tile.y == rect.y0)
            .count()
            .max(1);
        let mut clamp = ClampMeasure::default();
        let mut tile_times = Vec::new();
        for unit in &units {
            rays.merge(&unit.work.rays);
            if cfg.timed {
                let t = &unit.tile;
                tile_times.push((
                    PixelRect::new(t.x, t.y, t.x + t.width, t.y + t.height),
                    unit.work.secs,
                ));
            }
            // Each pixel's removals through the beauty's own estimator. A
            // sum over pixels, so the order does not matter beyond f64
            // rounding; done apart from the gather below so that a render
            // that measures nothing never looks.
            for (st, &removed) in unit.pixels.iter().zip(&unit.work.clamp) {
                let removed = if st.weight_sum > 0.0 {
                    removed / st.weight_sum
                } else if st.taken > 0 {
                    removed / st.taken as f32
                } else {
                    // Not sampled: a cancelled pass's.
                    0.0
                };
                clamp.pixels += 1;
                if removed != 0.0 {
                    clamp.pixels_touched += 1;
                    clamp.removed_luminance += removed as f64;
                }
            }
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
                        var_map[rect.index(i, j)] = var;
                        if cfg.adaptive {
                            rays.adaptive_pixels += 1;
                            rays.adaptive_samples += st.taken as u64;
                            // Stopped by the adaptive rule short of the
                            // budget. Every pixel of a completed pass ends
                            // stopped; a cancelled pass's unfinished pixels
                            // did not stop, they were left.
                            if st.stopped && st.taken < cfg.spp {
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
                    }
                }
            }
        }

        let elapsed = pass_start.elapsed();
        debug!(
            "pass done in {:?}: {} camera rays, {} closest-hit, {} shadow, {} vertices, \
             {:.2} rays/camera ray, roulette killed {}/{}, ended {} escaped / {} at depth",
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
        );
        (
            buffer,
            film,
            PassStats {
                var_map,
                rays,
                tiles: tile_times,
                clamp,
                interrupted,
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
        work: &mut UnitWork,
        scratch: &mut PathScratch,
        st: &mut PixelState,
        target: u32,
    ) {
        if AOV && let Some(planes) = work.aov.as_mut() {
            planes.pixel = p;
        }
        // Monomorphised on medium boundaries too (`trace_path`'s `MEDIA`): a
        // world without one runs an integrator with none of their branches.
        // And on shadow-linked lights with a bounce-side twin (`TWINS`).
        macro_rules! go {
            ($profile:literal, $media:literal, $twins:literal) => {
                self.advance_pixel::<$profile, AOV, $media, $twins>(
                    i, j, p, cfg, filter, work, scratch, st, target,
                )
            };
        }
        match (profiling, self.world.has_medium_boundaries(), cfg.twins) {
            (true, false, false) => go!(true, false, false),
            (false, false, false) => go!(false, false, false),
            (true, true, false) => go!(true, true, false),
            (false, true, false) => go!(false, true, false),
            (true, false, true) => go!(true, false, true),
            (false, false, true) => go!(false, false, true),
            (true, true, true) => go!(true, true, true),
            (false, true, true) => go!(false, true, true),
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
    fn advance_pixel<const PROFILE: bool, const AOV: bool, const MEDIA: bool, const TWINS: bool>(
        &self,
        i: usize,
        j: usize,
        p: usize,
        cfg: &PassConfig,
        filter: &FilterSampler,
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

        // Is the shutter coordinate worth sampling at all (`Renderer::shutter`)?
        // On a scene where nothing moves, or with motion blur off, every
        // value of it produces the same image and drawing one is pure waste.
        // It is not cheap waste: `draw_sample_f32::<N>` computes a whole
        // 4-dimensional Owen-scrambled Sobol block whatever `N` is, which
        // measured 4.2% of the render on cornellbox, one block per camera ray
        // for one float.
        //
        // Skipping the draw cannot perturb the other dimensions: `new_domain`
        // is a pure function of the parent state and takes `&self`, so a
        // domain that is never derived leaves `root` untouched.
        let motion = cfg.shutter;

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
            light_samples: self.settings.light_samples,
            light_samples_indirect: self.settings.light_samples_indirect,
            // A measured clamp rides in the clamp's own slot, flagged as
            // measured: the integrator reads the flag only where it would
            // clamp (see `PathContext::measure_clamp`).
            indirect_clamp: cfg.measure_clamp.or(self.settings.indirect_clamp),
            measure_clamp: PROFILE && cfg.measure_clamp.is_some(),
        };
        let measuring = PROFILE && cfg.measure_clamp.is_some();
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
            let color = trace_path::<PROFILE, AOV, MEDIA, TWINS>(
                &r,
                &path_cx,
                root,
                scratch,
                &mut unit.rays,
            ) * (wx * wy);
            if AOV && let Some(planes) = unit.aov.as_mut() {
                let extras = SampleExtras {
                    lpe: &scratch.route.out,
                    albedo: scratch.route.albedo,
                    diffuse_filter: scratch.route.diffuse_filter,
                    time,
                };
                planes.add(&scratch.first, &extras, fx, fy, wx * wy);
            }
            if measuring {
                // What `trace_path` found the clamp would remove from this
                // sample, weighted as the sample is. Summed per sample, in
                // sample order, so the total is the same however the pixel's
                // samples were split into stages and rounds.
                let r = std::mem::take(&mut scratch.clamp_removed);
                if let Some(c) = unit.clamp.get_mut(p) {
                    *c += self.lights.luma().of(r) * (wx * wy);
                }
            }
            state.sum += color;
            state.weight_sum += wx * wy;
            let lum = self.lights.luma().of(color) as f64;
            state.lum_sum += lum;
            state.lum_sq += lum * lum;
        }
        state.taken = target;
    }
}

/// The unbiased variance of a mean over `n` samples whose sum is `sum` and
/// sum of squares `sq`: `(Σx² − (Σx)²/n) / (n−1) / n`, clamped at 0, and
/// `+∞` below two samples, where it cannot be estimated.
///
/// The one estimator behind adaptive sampling's stop rule, the `variance`
/// AOV and the light path expressions' variance modifier, so `C.*[LO]`'s
/// variance is the `variance` AOV bit for bit.
pub(crate) fn var_of_mean(sum: f64, sq: f64, n: u32) -> f64 {
    if n < 2 {
        return f64::INFINITY;
    }
    let n = n as f64;
    ((sq - sum * sum / n) / (n - 1.0) / n).max(0.0)
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
            index: f32::INFINITY,
            converged: false,
            stopped: false,
            held: false,
        }
    }

    /// Unbiased variance of the pixel-mean luminance over `taken` samples.
    fn var_of_mean(&self) -> f64 {
        var_of_mean(self.lum_sum, self.lum_sq, self.taken)
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

    /// The pixel's colour and the variance of its mean luminance. A pixel
    /// that took no sample — only a cancelled render leaves one — is black,
    /// with no variance, rather than the NaN of `0 / 0`.
    fn estimate(&self) -> (Vec3A, f64) {
        if self.taken == 0 {
            return (Vec3A::ZERO, 0.0);
        }
        let variance = self.var_of_mean();
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

/// The adaptive minimum and the first check point of a `spp` budget whose
/// authored minimum is `authored_min`.
///
/// The minimum grows with the budget: an authored minimum of 8 at 1024 spp
/// takes at least 32. A floor, not a default, so that the unauthored 32 still
/// keeps a 16 spp render (the goldens) from ever stopping early. Two at
/// least, or there is no variance to test. Checks happen every 4th sample
/// from the minimum on, so the first one is at the smallest multiple of 4
/// that is at least the minimum — if the budget reaches that far at all.
/// Checked: a minimum within 3 of `u32::MAX` must saturate, not wrap to a
/// check point of 0 that would let every pixel stop after 4 samples.
pub(crate) fn adaptive_check_points(spp: u32, authored_min: u32) -> (u32, u32) {
    let min_spp = authored_min.max((spp as f64).sqrt().ceil() as u32).max(2);
    let first_check = min_spp.checked_next_multiple_of(4).unwrap_or(u32::MAX);
    (min_spp, first_check)
}

/// Whether a render of `spp` samples, authored minimum `authored_min` and
/// variance threshold `threshold` can stop a pixel early: the threshold is
/// on and the budget passes the first check point, so [`batch_schedule`]
/// has a round. What `crust diff`'s comparability reads off two stamps.
pub(crate) fn samples_adaptively(spp: u32, authored_min: u32, threshold: f32) -> bool {
    threshold > 0.0 && adaptive_check_points(spp, authored_min).1 < spp
}

/// The stages of a final pass's first sweep (design D1): 1, 2, 4, … spp
/// below `sweep_to`, then `sweep_to` itself. Every unit is taken to each
/// stage before any goes on to the next, so a watcher sees the whole region
/// at 1 spp first and the image sharpen from there. Never empty: a sweep to
/// 1 (or 0) is the one stage.
fn sweep_stages(sweep_to: u32) -> Vec<u32> {
    let mut stages = Vec::new();
    let mut stage = 1u32;
    while stage < sweep_to {
        stages.push(stage);
        stage = stage.saturating_mul(2);
    }
    stages.push(sweep_to);
    stages
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
/// count is known before the pass starts; it ends at `spp`, which the
/// progress total relies on.
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
/// neighbour outside `rect` — the pixels the pass traces, which `index` and
/// `active` cover — does not exist: outside the frame, or outside a render
/// region, where nothing samples it. A negative tolerance skips
/// the comparison altogether, so "off" is the per-pixel stop with no
/// dependence on neighbour values. `+∞ − finite = +∞` holds; two `+∞`
/// pixels never get here, since each fails its own test.
fn held_by_neighbour(
    index: &[f32],
    active: &[bool],
    rect: PixelRect,
    x: usize,
    y: usize,
    tolerance: f32,
) -> bool {
    if tolerance < 0.0 {
        return false;
    }
    let own = index[rect.index(x, y)];
    let holds = |q: usize| active[q] && index[q] - own > tolerance;
    (x > rect.x0 && holds(rect.index(x - 1, y)))
        || (x + 1 < rect.x1 && holds(rect.index(x + 1, y)))
        || (y > rect.y0 && holds(rect.index(x, y - 1)))
        || (y + 1 < rect.y1 && holds(rect.index(x, y + 1)))
}

/// What a work unit's samples write besides the pixel accumulators: its
/// counters. Private to the unit, so no two
/// threads share a counter and there is nothing to synchronise.
#[derive(Default)]
struct UnitWork {
    rays: RayStats,
    /// The unit's AOV planes, beside (not inside) its `PixelState`s, so a
    /// render without AOVs keeps the pixel state it always had.
    aov: Option<UnitAov>,
    /// Wall-clock seconds the unit took, when the pass is timed.
    secs: f64,
    /// Per pixel, Σ over samples of filter weight × the luminance the
    /// measured clamp would remove; empty unless the pass measures one.
    clamp: Vec<f32>,
}

/// One work unit of a pass — a tile or a row — and its pixels' state.
struct Unit {
    tile: Tile,
    /// Row-major within the tile.
    pixels: Vec<PixelState>,
    work: UnitWork,
}

impl Unit {
    fn new(tile: Tile, layout: Option<&AovLayout>, cam: CameraFrame, clamp: bool) -> Self {
        let pixels = tile.width * tile.height;
        Unit {
            pixels: vec![PixelState::new(); pixels],
            tile,
            work: UnitWork {
                aov: layout.map(|l| UnitAov::new(l, cam, pixels)),
                clamp: if clamp { vec![0.0; pixels] } else { Vec::new() },
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

/// Mean relative squared error of a pass (Rousselle et al. 2011): per-pixel
/// variance over the squared luminance of a reference image, floored at
/// 1e-4 so directly visible light sources don't dominate. The reference
/// must be *shared* by every pass being compared — normalizing a low-spp
/// pass by its own noisy mean correlates numerator and denominator and
/// breaks the 1/spp scaling the comparison relies on.
pub(crate) fn mean_relative_error(var_map: &[f64], ref_lum: &[f64]) -> f64 {
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

/// Progress steps a work unit has at most over a pass ([`ProgressCallback`]):
/// one a sample up to this budget, the same share of the budget past it.
/// Bounds the reports — each a callback under the progress lock — whatever
/// the budget.
const PROGRESS_STEPS: u64 = 64;

/// The scanline work units over `rect` (raster space): one region-wide ×1
/// tile per row, in the same order `generate_tiles` emits (rows by
/// increasing `y`), so both unit shapes replay in scanline order through
/// the one gather.
fn generate_rows(rect: PixelRect) -> Vec<Tile> {
    (rect.y0..rect.y1)
        .map(|y| Tile {
            x: rect.x0,
            y,
            width: rect.width(),
            height: 1,
        })
        .collect()
}

/// The tile grid over `rect` (raster space), in tile rows from its lowest
/// `y` up, each row left to right — the order `render_pass` relies on to
/// replay tiles in scanline order. The grid is the frame's: tile edges sit
/// on multiples of `tile_size` from the frame's origin, clipped to `rect`,
/// so a pixel of a region falls in the same tile, at the same place, as in
/// a full-frame render. Over the full frame, this is the full frame's grid.
fn generate_tiles(rect: PixelRect, tile_size: usize) -> Vec<Tile> {
    let grid = |lo: usize, hi: usize| {
        (lo - lo % tile_size..hi)
            .step_by(tile_size)
            .map(move |a| (a.max(lo), (a + tile_size).min(hi)))
    };
    let mut tiles = Vec::new();
    for (y0, y1) in grid(rect.y0, rect.y1) {
        for (x0, x1) in grid(rect.x0, rect.x1) {
            tiles.push(Tile {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            });
        }
    }
    tiles
}

#[cfg(test)]
mod tests;
