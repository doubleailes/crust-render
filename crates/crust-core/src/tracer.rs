use crate::buffer::Buffer;
use crate::filter::{FilterSampler, PixelFilter};
use crate::guiding::{GuidingConfig, GuidingField, SampleData, luminance};
use crate::hittable::HitRecord;
use crate::material::{Material, ScatterSample};
use crate::medium::sample_henyey_greenstein;
use crate::ray::Ray;
use crate::rt_world::{World, WorldHit};
use crate::stats::RayStats;
use crate::volume::{PhaseMix, VolumeEvent, Volumes};
use crate::{LightList, PathSampler, camera::Camera};
use glam::Vec3A;
use rayon::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing::{info, warn};

// OpenQMC domain-tree keys. The camera and the path subtree hang off the root
// (per-pixel, per-sample) sampler; each per-event sub-domain hangs off the
// current vertex domain. Distinct keys give independent 4D sub-patterns.
const K_CAMERA: i32 = 0; // off root: jitter (0,1) + lens uv (2,3)
const K_PATH: i32 = 1; // off root: the bounce subtree
const K_TIME: i32 = 2; // off root: shutter time for motion blur
const K_NEE: i32 = 0; // off vertex: light pick (0) + area uv (1,2)
const K_NEE_SHADOW: i32 = 1; // off vertex: shadow-ray volume transmittance
const K_BSDF: i32 = 2; // off vertex: material scatter block
const K_GUIDE: i32 = 3; // off vertex: guide coin (0) + guide seed (1,2)
const K_PHASE: i32 = 4; // off vertex: phase lobe (0) + HG uv (1,2)
const K_RR: i32 = 5; // off vertex: Russian-roulette survival
const K_MEDIUM: i32 = 6; // off vertex: carried-medium free flight
const K_VOLUME: i32 = 7; // off vertex: volume-region delta tracking

/// Render-progress callback: invoked with `(completed, total)` work units
/// (scanline rows, or tiles under bucket rendering) as a pass advances.
/// Called from worker threads, hence `Sync`. Presentation (progress bars,
/// logging) is the caller's concern — the engine has no UI dependencies.
pub type ProgressCallback<'a> = &'a (dyn Fn(u64, u64) + Sync);

/// Training-only clamp on recorded radiance so a single firefly cannot
/// dominate a directional distribution. Affects the guiding field, never the
/// image estimator.
const TRAIN_RADIANCE_CLAMP: f32 = 1e3;

/// Russian roulette: paths may terminate stochastically once they carry at
/// least this many vertices; the survival probability tracks the path
/// throughput but never drops below the floor, so weights stay bounded.
const RR_START_BOUNCE: usize = 3;
const RR_MIN_PROB: f32 = 0.05;

/// How the integrator combines its two direct-lighting strategies — light
/// sampling (NEE) and BSDF/phase sampling — into one estimate. The two MIS
/// variants weight each strategy's samples with a Veach heuristic; the
/// single-strategy variants disable one side entirely and exist to
/// visualize what each strategy contributes and where it fails (the classic
/// Veach comparison — `samples/veach_mis.usda` is the matching scene).
///
/// Every variant keeps `light_weight + bounce_weight = 1` for a light both
/// strategies can reach, so emission is counted exactly once and all four
/// estimators are unbiased — they differ only in variance. Lights only BSDF
/// sampling can reach (delta lobes, emissive geometry outside the light
/// list) keep full bounce weight under every strategy, `LightOnly`
/// included, because zeroing them would lose their energy entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SamplingStrategy {
    /// β=2 power-heuristic MIS — the renderer's historical default.
    #[default]
    PowerMis,
    /// Balance-heuristic MIS.
    BalanceMis,
    /// Light sampling only: NEE at full weight, bounce-hit emission dropped
    /// (for lights NEE could have sampled).
    LightOnly,
    /// BSDF sampling only: no shadow rays, bounce-hit emission at full
    /// weight.
    BsdfOnly,
}

impl SamplingStrategy {
    /// Does this strategy trace NEE shadow rays at all?
    pub fn samples_lights(self) -> bool {
        !matches!(self, SamplingStrategy::BsdfOnly)
    }

    /// Weight of a light-sampled (NEE) contribution, given the competing
    /// bounce strategy's density toward the same direction.
    pub fn light_weight(self, light_pdf: f32, bounce_pdf: f32) -> f32 {
        match self {
            SamplingStrategy::PowerMis => utils::power_heuristic(light_pdf, bounce_pdf),
            SamplingStrategy::BalanceMis => utils::balance_heuristic(light_pdf, bounce_pdf),
            SamplingStrategy::LightOnly => 1.0,
            SamplingStrategy::BsdfOnly => 0.0,
        }
    }

    /// Weight of bounce-hit emission on a light that NEE could also have
    /// sampled with density `light_pdf`. Mirror of [`Self::light_weight`]:
    /// for every strategy the two weights sum to one.
    pub fn bounce_weight(self, bounce_pdf: f32, light_pdf: f32) -> f32 {
        match self {
            SamplingStrategy::PowerMis => utils::power_heuristic(bounce_pdf, light_pdf),
            SamplingStrategy::BalanceMis => utils::balance_heuristic(bounce_pdf, light_pdf),
            SamplingStrategy::LightOnly => 0.0,
            SamplingStrategy::BsdfOnly => 1.0,
        }
    }
}

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

/// A cancellation handle shared between a render and its host. Clone it,
/// hand one side to [`Renderer::render_with_control`] (or a
/// [`ProgressiveRender`] step) and call [`StopToken::stop`] on the other —
/// from a signal handler, a UI thread, a Hydra `Stop()` — and the render
/// winds down at the next work-unit (row/tile) boundary, returning a
/// coherent partially-sampled image. Checked once per work unit, never per
/// sample, so it costs the hot loop nothing.
#[derive(Clone, Debug, Default)]
pub struct StopToken(Arc<AtomicBool>);

impl StopToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent; there is no un-stop.
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Everything one pixel has accumulated so far. Persisting exactly these
/// five estimator values (plus the adaptive-stop latch) between sampling
/// chunks is what makes progressive rendering bit-identical to the batch
/// path: the sampler is a pure function of the sample index, so resuming at
/// `taken` replays the identical IEEE addition sequence the one-shot loop
/// would have run.
#[derive(Clone, Copy)]
struct PixelAccum {
    /// Filter-weighted radiance sum Σwᵢ·Lᵢ (see `filter.rs`).
    sum: Vec3A,
    /// Filter weight sum Σwᵢ.
    weight_sum: f32,
    /// Luminance moments for the adaptive stop and the variance estimate.
    lum_sum: f64,
    lum_sq: f64,
    /// Samples taken so far — the next chunk resumes at this index.
    taken: u32,
    /// Latched by the adaptive early stop; later chunks skip the pixel.
    done: bool,
}

impl Default for PixelAccum {
    fn default() -> Self {
        PixelAccum {
            sum: Vec3A::ZERO,
            weight_sum: 0.0,
            lum_sum: 0.0,
            lum_sq: 0.0,
            taken: 0,
            done: false,
        }
    }
}

/// The persistent accumulation plane of an in-flight render: one
/// [`PixelAccum`] per pixel, row-major with row 0 at the bottom (the
/// `Buffer` convention). A batch render builds one, advances it to the full
/// budget and resolves it; a progressive render keeps it alive between
/// steps.
struct Film {
    width: usize,
    height: usize,
    pixels: Vec<PixelAccum>,
}

impl Film {
    fn new(width: usize, height: usize) -> Self {
        Film {
            width,
            height,
            pixels: vec![PixelAccum::default(); width * height],
        }
    }
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
        lights: LightList,
        settings: RenderSettings,
    ) -> Self {
        info!("world holds {} geometries", world.count());
        Renderer {
            camera,
            world,
            lights,
            settings,
            volumes: Volumes::default(),
        }
    }

    pub fn with_volumes(mut self, regions: Vec<crate::volume::VolumeRegion>) -> Self {
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
    pub fn render_with_stats(
        &self,
        tiled: bool,
        progress: ProgressCallback,
    ) -> (Buffer, RayStats) {
        self.render_impl(tiled, Some(progress))
    }

    fn render_impl(
        &self,
        tiled: bool,
        progress: Option<ProgressCallback>,
    ) -> (Buffer, RayStats) {
        if self.settings.guiding {
            let (buf, rays, _) = self.render_guided(tiled, progress, None);
            return (buf, rays);
        }
        let (buf, _, pass, _) =
            self.render_pass(self.final_pass_config(tiled), None, progress, None);
        (buf, pass.rays)
    }

    /// As the plain entry points, plus a [`StopToken`]: when it fires the
    /// render winds down at the next row/tile boundary and returns the
    /// coherent partial image (every pixel a valid mean of the samples it
    /// took; unreached pixels black). The `bool` reports whether the render
    /// ran to completion. With guiding enabled, a stop during a training
    /// pass discards that pass — its partial sample set never reaches the
    /// field — and blends the passes that completed.
    pub fn render_with_control(
        &self,
        tiled: bool,
        progress: Option<ProgressCallback>,
        stop: Option<&StopToken>,
    ) -> (Buffer, RayStats, bool) {
        if self.settings.guiding {
            return self.render_guided(tiled, progress, stop);
        }
        let cfg = self.final_pass_config(tiled);
        let mut film = Film::new(self.settings.width, self.settings.height);
        let (_, rays, completed) = self.advance_film(&mut film, cfg.spp, &cfg, None, progress, stop);
        let (buffer, _, _) = self.finish_pass(&film, cfg.tiled);
        (buffer, rays, completed)
    }

    /// Begins a progressive render session: the same render the batch entry
    /// points produce, advanced in caller-sized sample chunks with a
    /// readable partial image between chunks. Run to completion, the result
    /// is **bit-identical** to [`Renderer::render`]/`render_with_tiles` —
    /// the sampler is a pure function of the sample index and the film
    /// persists every accumulator, so chunking cannot move a single bit.
    ///
    /// With guiding enabled, training passes run whole (one per `step` call,
    /// however small the requested chunk — their sample stream feeds the
    /// order-sensitive field update and must not be split); only the final
    /// pass is chunked. Run to completion that too is bit-identical to the
    /// batch guided render.
    pub fn begin_progressive(&self, tiled: bool) -> ProgressiveRender<'_> {
        let state = if self.settings.guiding {
            match self.world.bounds() {
                Some(bounds) => {
                    let gcfg = GuidingConfig {
                        train_iterations: self.settings.guiding_train_iterations,
                        guide_prob: self.settings.guiding_prob,
                        ..GuidingConfig::default()
                    };
                    ProgState::Training {
                        field: GuidingField::new(bounds, gcfg),
                        gcfg,
                        passes: Vec::new(),
                        k: 0,
                        eff_unguided: None,
                        eff_guided: None,
                        train_spp_done: 0,
                    }
                }
                None => {
                    warn!(
                        "path guiding enabled but the scene has no bounding box; rendering unguided"
                    );
                    ProgState::Simple {
                        film: Film::new(self.settings.width, self.settings.height),
                        spp_done: 0,
                    }
                }
            }
        } else {
            ProgState::Simple {
                film: Film::new(self.settings.width, self.settings.height),
                spp_done: 0,
            }
        };
        ProgressiveRender {
            renderer: self,
            tiled,
            rays: RayStats::default(),
            state,
        }
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
        stop: Option<&StopToken>,
    ) -> (Buffer, RayStats, bool) {
        // Every pass costs time, training included, so the counters cover
        // all of them rather than the final pass alone.
        let mut rays = RayStats::default();
        let bounds = match self.world.bounds() {
            Some(b) => b,
            None => {
                warn!("path guiding enabled but the scene has no bounding box; rendering unguided");
                let (buf, _, pass, completed) =
                    self.render_pass(self.final_pass_config(tiled), None, progress, stop);
                return (buf, pass.rays, completed);
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
            let (buffer, samples, stats, completed) =
                self.render_pass(train_cfg, Some(&gctx), None, stop);
            rays.merge(&stats.rays);
            let secs = start.elapsed().as_secs_f64();
            drop(gctx);
            if !completed {
                // The pass's sample set is partial (and dependent on where
                // the stop landed), so the field never sees it and its image
                // is not blended. Blend what completed; if nothing did, the
                // interrupted pass's coherent partial image is still the
                // best available.
                info!("path guiding: stopped during training pass {} — discarding it", k + 1);
                if passes.is_empty() {
                    return (buffer, rays, false);
                }
                return (self.blend_passes(&passes), rays, false);
            }
            info!(
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
            passes.push((buffer, stats.variance));
        }

        let guide_final = self.decide_guide_final(&passes, &eff_unguided, &eff_guided);

        info!(
            "path guiding: final pass at {} spp ({})",
            self.settings.samples_per_pixel,
            if guide_final { "guided" } else { "unguided" }
        );
        let gctx = GuidingContext {
            field: &field,
            training: false,
        };
        let final_gctx = if guide_final { Some(&gctx) } else { None };
        let (final_buffer, _, final_stats, completed) =
            self.render_pass(self.final_pass_config(tiled), final_gctx, progress, stop);
        rays.merge(&final_stats.rays);
        // An interrupted final pass carries infinite variance (some pixels
        // hold <2 samples), so `blend_passes` gives it zero weight and the
        // completed training passes carry the image — pushing it is still
        // right, as the fallback when *nothing* completed.
        passes.push((final_buffer, final_stats.variance));

        (self.blend_passes(&passes), rays, completed)
    }

    /// The guiding efficiency verdict (see [`Renderer::render_guided`]):
    /// should the final pass draw from the trained field at all? `true`
    /// unless the measured efficiency ratio says training made things worse.
    fn decide_guide_final(
        &self,
        passes: &[(Buffer, f64)],
        eff_unguided: &Option<(Vec<f64>, f64)>,
        eff_guided: &Option<(Vec<f64>, f64)>,
    ) -> bool {
        match (eff_unguided, eff_guided) {
            (Some((var_pt, cost_pt)), Some((var_pg, cost_pg))) if *cost_pg > 0.0 => {
                // Reference image for relative error: the blend of all
                // training passes — our stand-in for the paper's denoised
                // accumulated image, and crucially the *same* image for both
                // sides of the ratio.
                let ref_lum = blend_luminance(passes, self.settings.width, self.settings.height);
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
        }
    }

    /// Inverse-variance blend of independent unbiased passes. Passes whose
    /// variance could not be estimated (spp < 2) get zero weight; if nothing
    /// is weightable, the last (final) pass is returned as-is.
    fn blend_passes(&self, passes: &[(Buffer, f64)]) -> Buffer {
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
            return passes
                .last()
                .expect("at least the final pass exists")
                .0
                .clone();
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

    /// One full-frame pass at `cfg.spp` samples per pixel. Returns the
    /// image, whatever training samples the pass recorded (empty unless a
    /// training `GuidingContext` is supplied), the pass's [`PassStats`], and
    /// whether the pass ran to completion (`false` only when `stop` fired).
    fn render_pass(
        &self,
        cfg: PassConfig,
        gctx: Option<&GuidingContext>,
        progress: Option<ProgressCallback>,
        stop: Option<&StopToken>,
    ) -> (Buffer, Vec<SampleData>, PassStats, bool) {
        let mut film = Film::new(self.settings.width, self.settings.height);
        let (all_samples, rays, completed) =
            self.advance_film(&mut film, cfg.spp, &cfg, gctx, progress, stop);
        let (buffer, variance, var_map) = self.finish_pass(&film, cfg.tiled);
        (
            buffer,
            all_samples,
            PassStats {
                variance,
                var_map,
                rays,
            },
        completed,
        )
    }

    /// Advances every pixel of `film` to `target_spp` samples (skipping
    /// pixels the adaptive stop already finished). This is the one scheduling
    /// loop in the renderer — batch passes advance a fresh film to the full
    /// budget in a single call, progressive rendering calls it repeatedly
    /// with a rising target. Sample indices are consumed in ascending order
    /// with no gaps, so how the budget is chunked cannot change the result.
    ///
    /// Returns the training samples recorded, the ray counters, and whether
    /// the advance completed (`false` when `stop` cut it short at a row/tile
    /// boundary — the film stays coherent either way).
    fn advance_film(
        &self,
        film: &mut Film,
        target_spp: u32,
        cfg: &PassConfig,
        gctx: Option<&GuidingContext>,
        progress: Option<ProgressCallback>,
        stop: Option<&StopToken>,
    ) -> (Vec<SampleData>, RayStats, bool) {
        let mut all_samples = Vec::new();
        let mut rays = RayStats::default();
        // One tabulation per advance, shared read-only by every worker.
        let filter = FilterSampler::new(self.settings.pixel_filter);

        if cfg.tiled {
            let tiles = generate_tiles(self.settings.width, self.settings.height, 16); // tile size: 16x16
            let total = tiles.len() as u64;
            let done = AtomicU64::new(0);
            type TileOut = (Vec<(usize, usize, PixelAccum)>, Vec<SampleData>, RayStats);
            // Shared read view for the workers; each tile copies its accums
            // out, advances them privately, and the sequential merge below
            // writes them back — keeping the merge order identical to the
            // scheduling order however rayon interleaves the tiles.
            let film_ref: &Film = film;
            let results: Vec<Option<TileOut>> = tiles
                .into_par_iter()
                .map(|tile| {
                    // A fired stop skips tiles that have not started; whole
                    // tiles either run or don't, so the film stays coherent.
                    if stop.is_some_and(|s| s.is_stopped()) {
                        return None;
                    }
                    let mut pixels = Vec::with_capacity(tile.width * tile.height);
                    let mut samples = Vec::new();
                    // Private to this tile, so no two threads share a
                    // counter and there is nothing to synchronise. The path
                    // scratch has the same ownership story: one buffer serves
                    // every sample of every pixel in the tile.
                    let mut tile_rays = RayStats::default();
                    let mut scratch = PathScratch::new(self.settings.max_depth as usize);
                    for j in tile.y..tile.y + tile.height {
                        for i in tile.x..tile.x + tile.width {
                            let mut acc = film_ref.pixels[j * film_ref.width + i];
                            let mut s = self.sample_pixel(
                                i,
                                j,
                                &mut acc,
                                target_spp,
                                cfg,
                                &filter,
                                gctx,
                                &mut scratch,
                                &mut tile_rays,
                            );
                            pixels.push((i, j, acc));
                            samples.append(&mut s);
                        }
                    }
                    if let Some(cb) = progress {
                        cb(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                    }
                    Some((pixels, samples, tile_rays))
                })
                .collect();
            let mut completed = true;
            for result in results {
                let Some((pixels, samples, tile_rays)) = result else {
                    completed = false;
                    continue;
                };
                rays.merge(&tile_rays);
                for (i, j, acc) in pixels {
                    film.pixels[j * film.width + i] = acc;
                }
                all_samples.extend(samples);
            }
            (all_samples, rays, completed)
        } else {
            let total = self.settings.height as u64;
            let mut done = 0u64;
            let mut completed = true;
            let width = film.width;
            for j in (0..self.settings.height).rev() {
                // A fired stop ends the advance at the next row boundary.
                if stop.is_some_and(|s| s.is_stopped()) {
                    completed = false;
                    break;
                }
                // `map_init` rather than `map`: the path scratch is reused
                // across every pixel rayon hands one worker, instead of being
                // rebuilt per pixel. (This path parallelises over pixels, so
                // unlike the tiled path there is no per-work-unit closure to
                // hang the buffer on.)
                let row_accums = &mut film.pixels[j * width..(j + 1) * width];
                let row: Vec<(Vec<SampleData>, RayStats)> = row_accums
                    .par_iter_mut()
                    .enumerate()
                    .map_init(
                        || PathScratch::new(self.settings.max_depth as usize),
                        |scratch, (i, acc)| {
                            let mut px = RayStats::default();
                            let s = self.sample_pixel(
                                i, j, acc, target_spp, cfg, &filter, gctx, scratch, &mut px,
                            );
                            (s, px)
                        },
                    )
                    .collect();
                for (samples, px_rays) in row {
                    rays.merge(&px_rays);
                    all_samples.extend(samples);
                }
                done += 1;
                if let Some(cb) = progress {
                    cb(done, total);
                }
            }
            (all_samples, rays, completed)
        }
    }

    /// Resolves a film into the pass image and its variance statistics.
    ///
    /// Pixels are visited in the *scheduling order* of the pass mode
    /// (scanline: rows top-down, i ascending; tiled: tile order) purely so
    /// the f64 `variance_sum` accumulates in the same order the pre-film
    /// code produced — that mean feeds the guided blend weights, and a
    /// reordered float sum would move the final image by ulps.
    fn finish_pass(&self, film: &Film, tiled: bool) -> (Buffer, f64, Vec<f64>) {
        let mut buffer = Buffer::new(film.width, film.height);
        let mut var_map = vec![0.0f64; film.width * film.height];
        let mut variance_sum = 0.0f64;
        let pixel_count = (film.width * film.height) as f64;
        let mut resolve = |i: usize, j: usize| {
            let (color, var) = resolve_pixel(&film.pixels[j * film.width + i]);
            buffer.set_pixel(i, j, color);
            var_map[j * film.width + i] = var;
            variance_sum += var;
        };
        if tiled {
            for tile in generate_tiles(film.width, film.height, 16) {
                for j in tile.y..tile.y + tile.height {
                    for i in tile.x..tile.x + tile.width {
                        resolve(i, j);
                    }
                }
            }
        } else {
            for j in (0..film.height).rev() {
                for i in 0..film.width {
                    resolve(i, j);
                }
            }
        }
        drop(resolve);
        (buffer, variance_sum / pixel_count, var_map)
    }

    /// Advances one pixel's accumulator from `acc.taken` to `target_spp`
    /// samples (or to its adaptive stop), returning the training samples the
    /// new samples recorded. The loop body is the renderer's hot path; the
    /// accumulator state lives in locals for the duration of the loop and is
    /// stored back once per call.
    #[allow(clippy::too_many_arguments)]
    fn sample_pixel(
        &self,
        i: usize,
        j: usize,
        acc: &mut PixelAccum,
        target_spp: u32,
        cfg: &PassConfig,
        filter: &FilterSampler,
        gctx: Option<&GuidingContext>,
        scratch: &mut PathScratch,
        stats: &mut RayStats,
    ) -> Vec<SampleData> {
        let mut samples = Vec::new();
        if acc.done || acc.taken >= target_spp {
            return samples;
        }
        // FIS weight sum (see `filter.rs`): the pixel estimate is the
        // weighted average Σwᵢ·Lᵢ / Σwᵢ. For box and triangle every wᵢ is
        // exactly 1.0, so the sum is exactly `taken as f32` and the estimate
        // is the plain mean — box at radius 0.5 stays bit-identical to the
        // historical unweighted, unfiltered estimator.
        let mut sum = acc.sum;
        let mut weight_sum = acc.weight_sum;
        let mut lum_sum = acc.lum_sum;
        let mut lum_sq = acc.lum_sq;

        let threshold = self.settings.variance_threshold as f64;
        let min_spp = self.settings.min_samples_per_pixel.max(2);
        let mut taken = acc.taken;
        let mut done = false;

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

        for sample in acc.taken..target_spp {
            let root =
                PathSampler::new(i as i32, j as i32, cfg.seed as i32, sample as i32).new_domain(tile);
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
            let r = self.camera.get_ray(u, v, [cam[2], cam[3]], time);
            stats.camera_rays += 1;
            let color = trace_path(
                &r,
                &self.world,
                &self.lights,
                &self.volumes,
                self.settings.max_depth as i32,
                self.settings.sampling_strategy,
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
            if cfg.adaptive && threshold > 0.0 && taken >= min_spp && taken % 4 == 0 {
                let n = taken as f64;
                let var_of_mean =
                    ((lum_sq - lum_sum * lum_sum / n) / (n - 1.0) / n).max(0.0);
                let mean = (lum_sum / n).max(1e-4);
                if var_of_mean.sqrt() / mean < threshold {
                    done = true;
                    break;
                }
            }
        }

        acc.sum = sum;
        acc.weight_sum = weight_sum;
        acc.lum_sum = lum_sum;
        acc.lum_sq = lum_sq;
        acc.taken = taken;
        acc.done = done;
        samples
    }
}

/// Resolves an accumulator into its pixel estimate and the unbiased
/// variance of the pixel-mean luminance. A pure function of the persisted
/// state, so *when* a pixel is resolved (per pass, per progressive
/// snapshot) cannot change what it resolves to.
fn resolve_pixel(acc: &PixelAccum) -> (Vec3A, f64) {
    // A pixel a cancelled render never reached: black, with variance
    // "unknown" — unreachable in a completed pass.
    if acc.taken == 0 {
        return (Vec3A::ZERO, f64::INFINITY);
    }
    let n = acc.taken as f64;
    let variance = if acc.taken >= 2 {
        ((acc.lum_sq - acc.lum_sum * acc.lum_sum / n) / (n - 1.0) / n).max(0.0)
    } else {
        f64::INFINITY
    };
    // Weighted-average film estimator. A Mitchell pixel whose few
    // samples all landed on negative lobes could zero the denominator;
    // the plain mean is the sane fallback there.
    let mean = if acc.weight_sum > 0.0 {
        acc.sum / acc.weight_sum
    } else {
        acc.sum / acc.taken as f32
    };
    (mean, variance)
}

/// Outcome of one [`ProgressiveRender::step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// The step reached its target; more of the budget remains.
    InProgress { spp_done: u32 },
    /// The stop token fired mid-step. The image stays coherent, and a later
    /// `step` resumes exactly where this one was cut off.
    Stopped { spp_done: u32 },
    /// The full sample budget is rendered; further `step`s are no-ops.
    Complete { spp_done: u32 },
}

impl StepStatus {
    /// Samples per pixel fully banked so far (budgeted samples of completed
    /// chunks — pixels the adaptive stop finished early hold fewer by
    /// design).
    pub fn spp_done(self) -> u32 {
        match self {
            StepStatus::InProgress { spp_done }
            | StepStatus::Stopped { spp_done }
            | StepStatus::Complete { spp_done } => spp_done,
        }
    }
}

/// State of an in-flight progressive render — see
/// [`Renderer::begin_progressive`].
enum ProgState {
    /// Unguided: one film advanced toward the full budget.
    Simple { film: Film, spp_done: u32 },
    /// Guided, in the training phase: passes run whole, one per step.
    Training {
        field: GuidingField,
        gcfg: GuidingConfig,
        passes: Vec<(Buffer, f64)>,
        k: u32,
        eff_unguided: Option<(Vec<f64>, f64)>,
        eff_guided: Option<(Vec<f64>, f64)>,
        train_spp_done: u32,
    },
    /// Guided, in the (chunkable) final pass. `field` is `None` when the
    /// efficiency estimate turned guiding off for the final pass.
    Final {
        field: Option<GuidingField>,
        passes: Vec<(Buffer, f64)>,
        film: Film,
        train_spp_done: u32,
        spp_done: u32,
    },
    Done { buffer: Buffer, spp_done: u32 },
    /// Placeholder while `step` owns the state; never observable.
    Transitioning,
}

/// A progressive render session (see [`Renderer::begin_progressive`]): the
/// host thread owns it, calls [`ProgressiveRender::step`] to advance the
/// image by a sample budget, and may read a coherent intermediate image
/// with [`ProgressiveRender::snapshot`] between steps — the reason there is
/// no locking anywhere in this type.
pub struct ProgressiveRender<'a> {
    renderer: &'a Renderer,
    tiled: bool,
    rays: RayStats,
    state: ProgState,
}

impl ProgressiveRender<'_> {
    /// Advances the render by (up to) `spp` more samples per pixel. In the
    /// guided training phase the chunk size is ignored and the next whole
    /// training pass runs instead (see [`Renderer::begin_progressive`]).
    pub fn step(
        &mut self,
        spp: u32,
        progress: Option<ProgressCallback>,
        stop: Option<&StopToken>,
    ) -> StepStatus {
        let renderer = self.renderer;
        let total_spp = renderer.settings.samples_per_pixel;
        let state = std::mem::replace(&mut self.state, ProgState::Transitioning);
        let (state, status) = match state {
            ProgState::Done { buffer, spp_done } => (
                ProgState::Done { buffer, spp_done },
                StepStatus::Complete { spp_done },
            ),

            ProgState::Simple { mut film, spp_done } => {
                let target = spp_done.saturating_add(spp.max(1)).min(total_spp);
                let cfg = renderer.final_pass_config(self.tiled);
                let (_, rays, chunk_done) =
                    renderer.advance_film(&mut film, target, &cfg, None, progress, stop);
                self.rays.merge(&rays);
                if !chunk_done {
                    (
                        ProgState::Simple { film, spp_done },
                        StepStatus::Stopped { spp_done },
                    )
                } else if target >= total_spp {
                    let (buffer, _, _) = renderer.finish_pass(&film, self.tiled);
                    (
                        ProgState::Done {
                            buffer,
                            spp_done: target,
                        },
                        StepStatus::Complete { spp_done: target },
                    )
                } else {
                    (
                        ProgState::Simple {
                            film,
                            spp_done: target,
                        },
                        StepStatus::InProgress { spp_done: target },
                    )
                }
            }

            ProgState::Training {
                mut field,
                gcfg,
                mut passes,
                k,
                mut eff_unguided,
                mut eff_guided,
                train_spp_done,
            } => {
                // Mirror of the batch schedule in `render_guided` — same
                // budgets, same seeds, same field updates, so a progressive
                // guided render trains the identical field.
                let spp_k = (1u32 << k.min(16)).max(2);
                let seed = (renderer.settings.frame as u32)
                    .wrapping_add((k + 1).wrapping_mul(0x9E37_79B9));
                let train_cfg = PassConfig {
                    spp: spp_k,
                    seed,
                    tiled: self.tiled,
                    adaptive: false,
                };
                let gctx = GuidingContext {
                    field: &field,
                    training: true,
                };
                let start = std::time::Instant::now();
                let (buffer, samples, stats, completed) =
                    renderer.render_pass(train_cfg, Some(&gctx), progress, stop);
                let secs = start.elapsed().as_secs_f64();
                drop(gctx);
                self.rays.merge(&stats.rays);
                if !completed {
                    // Discard the interrupted pass wholesale (partial sample
                    // sets must not train the field); the next step re-runs
                    // it from scratch.
                    info!(
                        "path guiding: stopped during training pass {} — discarding it",
                        k + 1
                    );
                    (
                        ProgState::Training {
                            field,
                            gcfg,
                            passes,
                            k,
                            eff_unguided,
                            eff_guided,
                            train_spp_done,
                        },
                        StepStatus::Stopped {
                            spp_done: train_spp_done,
                        },
                    )
                } else {
                    info!(
                        "path guiding: training pass {}/{} at {} spp — {} samples, variance {:.3e}, {:.2}s",
                        k + 1,
                        gcfg.train_iterations,
                        spp_k,
                        samples.len(),
                        stats.variance,
                        secs
                    );
                    if k == 0 {
                        eff_unguided = Some((stats.var_map, secs));
                    } else if k == gcfg.train_iterations - 1 {
                        eff_guided = Some((stats.var_map, secs));
                    }
                    field.update(&samples, k + 1);
                    passes.push((buffer, stats.variance));
                    let train_spp_done = train_spp_done + spp_k;
                    let k = k + 1;
                    if k < gcfg.train_iterations {
                        (
                            ProgState::Training {
                                field,
                                gcfg,
                                passes,
                                k,
                                eff_unguided,
                                eff_guided,
                                train_spp_done,
                            },
                            StepStatus::InProgress {
                                spp_done: train_spp_done,
                            },
                        )
                    } else {
                        let guide_final =
                            renderer.decide_guide_final(&passes, &eff_unguided, &eff_guided);
                        info!(
                            "path guiding: final pass at {} spp ({})",
                            total_spp,
                            if guide_final { "guided" } else { "unguided" }
                        );
                        (
                            ProgState::Final {
                                field: guide_final.then_some(field),
                                passes,
                                film: Film::new(
                                    renderer.settings.width,
                                    renderer.settings.height,
                                ),
                                train_spp_done,
                                spp_done: 0,
                            },
                            StepStatus::InProgress {
                                spp_done: train_spp_done,
                            },
                        )
                    }
                }
            }

            ProgState::Final {
                field,
                mut passes,
                mut film,
                train_spp_done,
                spp_done,
            } => {
                let target = spp_done.saturating_add(spp.max(1)).min(total_spp);
                let cfg = renderer.final_pass_config(self.tiled);
                let gctx_store;
                let gctx = match &field {
                    Some(f) => {
                        gctx_store = GuidingContext {
                            field: f,
                            training: false,
                        };
                        Some(&gctx_store)
                    }
                    None => None,
                };
                let (_, rays, chunk_done) =
                    renderer.advance_film(&mut film, target, &cfg, gctx, progress, stop);
                self.rays.merge(&rays);
                if !chunk_done {
                    (
                        ProgState::Final {
                            field,
                            passes,
                            film,
                            train_spp_done,
                            spp_done,
                        },
                        StepStatus::Stopped {
                            spp_done: train_spp_done + spp_done,
                        },
                    )
                } else if target >= total_spp {
                    let (buffer, variance, _) = renderer.finish_pass(&film, self.tiled);
                    passes.push((buffer, variance));
                    let blended = renderer.blend_passes(&passes);
                    (
                        ProgState::Done {
                            buffer: blended,
                            spp_done: train_spp_done + target,
                        },
                        StepStatus::Complete {
                            spp_done: train_spp_done + target,
                        },
                    )
                } else {
                    (
                        ProgState::Final {
                            field,
                            passes,
                            film,
                            train_spp_done,
                            spp_done: target,
                        },
                        StepStatus::InProgress {
                            spp_done: train_spp_done + target,
                        },
                    )
                }
            }

            ProgState::Transitioning => unreachable!("ProgState::Transitioning escaped step()"),
        };
        self.state = state;
        status
    }

    /// A coherent copy of the image as rendered so far. Call between steps
    /// (the session is single-owner, so there is nothing to race). In the
    /// guided final phase the partial final pass joins the blend weighted by
    /// its variance so far — an approximation that only ever affects
    /// intermediate snapshots, never the completed render.
    pub fn snapshot(&self) -> Buffer {
        let renderer = self.renderer;
        match &self.state {
            ProgState::Done { buffer, .. } => buffer.clone(),
            ProgState::Simple { film, .. } => renderer.finish_pass(film, self.tiled).0,
            ProgState::Training { passes, .. } => {
                if passes.is_empty() {
                    Buffer::new(renderer.settings.width, renderer.settings.height)
                } else {
                    blend_pass_refs(
                        &passes.iter().map(|(b, v)| (b, *v)).collect::<Vec<_>>(),
                        renderer.settings.width,
                        renderer.settings.height,
                    )
                }
            }
            ProgState::Final { passes, film, .. } => {
                let (partial, variance, _) = renderer.finish_pass(film, self.tiled);
                let mut entries: Vec<(&Buffer, f64)> =
                    passes.iter().map(|(b, v)| (b, *v)).collect();
                entries.push((&partial, variance));
                blend_pass_refs(
                    &entries,
                    renderer.settings.width,
                    renderer.settings.height,
                )
            }
            ProgState::Transitioning => unreachable!("ProgState::Transitioning escaped step()"),
        }
    }

    /// As [`ProgressiveRender::snapshot`], writing into a caller-owned
    /// buffer (resized to the image if it does not match).
    pub fn snapshot_into(&self, buf: &mut Buffer) {
        match &self.state {
            ProgState::Simple { film, .. } => {
                if buf.width() != film.width || buf.height() != film.height {
                    *buf = Buffer::new(film.width, film.height);
                }
                for j in 0..film.height {
                    for i in 0..film.width {
                        buf.set_pixel(i, j, resolve_pixel(&film.pixels[j * film.width + i]).0);
                    }
                }
            }
            _ => *buf = self.snapshot(),
        }
    }

    /// Budgeted samples per pixel banked so far (training + final for
    /// guided sessions).
    pub fn spp_done(&self) -> u32 {
        match &self.state {
            ProgState::Simple { spp_done, .. } => *spp_done,
            ProgState::Training { train_spp_done, .. } => *train_spp_done,
            ProgState::Final {
                train_spp_done,
                spp_done,
                ..
            } => train_spp_done + spp_done,
            ProgState::Done { spp_done, .. } => *spp_done,
            ProgState::Transitioning => unreachable!("ProgState::Transitioning escaped step()"),
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self.state, ProgState::Done { .. })
    }

    /// Consumes the session, returning the image as rendered so far (the
    /// final image when complete) and the ray counters across every step.
    pub fn finish(self) -> (Buffer, RayStats) {
        if matches!(self.state, ProgState::Done { .. }) {
            let rays = self.rays;
            match self.state {
                ProgState::Done { buffer, .. } => (buffer, rays),
                _ => unreachable!(),
            }
        } else {
            let buffer = self.snapshot();
            (buffer, self.rays)
        }
    }
}

/// Inverse-variance blend over borrowed passes — `blend_passes` for the
/// snapshot path, minus the ownership and the logging (snapshots may run
/// every frame). Non-finite/zero variances get zero weight; if nothing is
/// weightable the last pass is returned as-is.
fn blend_pass_refs(passes: &[(&Buffer, f64)], width: usize, height: usize) -> Buffer {
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
        return passes
            .last()
            .expect("at least one pass exists")
            .0
            .clone();
    }
    let mut out = Buffer::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let mut c = Vec3A::ZERO;
            for ((pass, _), w) in passes.iter().zip(&weights) {
                c += pass.get_pixel(x, y) * (*w / total) as f32;
            }
            out.set_pixel(x, y, c);
        }
    }
    out
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
    // MIS strategy (see `SamplingStrategy`; `crust:samplingStrategy` /
    // `--strategy`).
    sampling_strategy: SamplingStrategy,
    // Pixel reconstruction filter (see `PixelFilter`; `crust:pixelFilter` /
    // `--filter`). Applied by filter importance sampling in `render_pixel`.
    pixel_filter: PixelFilter,
}
impl RenderSettings {
    /// Image width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Image height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

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
            sampling_strategy: SamplingStrategy::default(),
            pixel_filter: PixelFilter::default(),
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

    /// Select how light sampling and BSDF sampling combine — see
    /// [`SamplingStrategy`].
    pub fn with_sampling_strategy(mut self, strategy: SamplingStrategy) -> Self {
        self.sampling_strategy = strategy;
        self
    }

    pub fn sampling_strategy(&self) -> SamplingStrategy {
        self.sampling_strategy
    }

    /// Select the pixel reconstruction filter — see [`PixelFilter`].
    pub fn with_pixel_filter(mut self, filter: PixelFilter) -> Self {
        self.pixel_filter = filter;
        self
    }

    pub fn pixel_filter(&self) -> PixelFilter {
        self.pixel_filter
    }

    pub fn get_dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn samples_per_pixel(&self) -> u32 {
        self.samples_per_pixel
    }

    pub fn max_depth(&self) -> u32 {
        self.max_depth
    }
}

pub fn ray_color(
    r: &Ray,
    world: &World,
    lights: &LightList,
    volumes: &Volumes,
    depth: i32,
    strategy: SamplingStrategy,
    sampler: PathSampler,
) -> Vec3A {
    let mut no_training = Vec::new();
    let mut stats = RayStats::default();
    // The one-shot entry point (benches and tests), so a scratch per call is
    // the right trade — the renderer's own paths reuse one per work unit.
    let mut scratch = PathScratch::new(depth.max(0) as usize);
    trace_path(
        r,
        world,
        lights,
        volumes,
        depth,
        strategy,
        sampler,
        None,
        &mut no_training,
        &mut scratch,
        &mut stats,
    )
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
    sampler: PathSampler,
) -> Option<ScatterSample> {
    // Distinct sub-domains: the BSDF scatter block, and the guide block whose
    // first dimension is the α-coin and next two are the guide-sampling seed.
    let bsdf_dom = sampler.new_domain(K_BSDF);
    let g = match guiding {
        Some(g) if g.field.trained_at(rec.p) => g,
        _ => return mat.scatter_importance(r, rec, bsdf_dom),
    };
    let alpha = g.field.config().guide_prob;
    let gs = sampler.new_domain(K_GUIDE).draw_sample_f32::<4>();

    if gs[0] < alpha {
        // Guide branch: draw from the field; the material's continuous
        // component supplies the value and the BSDF side of the mixture pdf.
        if let Some((wi, p_guide)) = g.field.sample(rec.p, [gs[1], gs[2]]) {
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
        mat.scatter_importance(r, rec, bsdf_dom)
    } else {
        // BSDF branch.
        let mut sample = mat.scatter_importance(r, rec, bsdf_dom)?;
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
/// the old recursion did: `R = segment_emit + atten · (emit_here + nee +
/// factor · (next_emit·next_emit_weight + R_incoming))`.
struct VertexRec {
    /// Transmittance over the segment that arrived at this vertex —
    /// Beer-Lambert for a carried medium times the volume-region tracking
    /// weight. For volume-scatter vertices this is the full walk weight of
    /// the event (transmittance × albedo compensation), which multiplies
    /// everything at and beyond the vertex, NEE included.
    atten: Vec3A,
    /// Volume emission collected along the arriving segment, already
    /// weighted by the tracking-walk weight up to each emission point.
    /// Added OUTSIDE `atten` in the gather — folding it into `emit_here`
    /// would attenuate it a second time.
    segment_emit: Vec3A,
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

/// Buffers a path walk needs, owned by the worker rather than the walk.
///
/// The forward pass records one [`VertexRec`] per vertex and the backward
/// gather reads them, so the walk needs somewhere to put them — but it does
/// not need *fresh* storage. Allocating per camera sample cost one
/// `malloc`/`free` pair per sample, measured at 4.4% of the render on
/// cornellbox (460 800 pairs for 460 800 samples). A work unit renders
/// thousands of samples through the same buffer instead.
///
/// Held per tile (or, in the scanline path, per rayon worker) — the same
/// granularity as `RayStats`, so it is private to one thread by construction
/// and there is nothing to synchronise.
pub(crate) struct PathScratch {
    records: Vec<VertexRec>,
}

impl PathScratch {
    /// `max_depth` is a capacity hint only; the walk may push fewer.
    pub(crate) fn new(max_depth: usize) -> Self {
        Self {
            records: Vec::with_capacity(max_depth),
        }
    }
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

/// The previous path vertex, as far as emission MIS is concerned: either a
/// surface bounce or a volume-region phase scatter (which runs NEE, so its
/// bounce-hit emission must be MIS-weighted against the same light
/// strategy or it is double-counted). Carried-medium (subsurface) scatters
/// run no NEE and keep `prev = None` instead.
enum PrevVertex<'a> {
    Surface(PrevBounce<'a>),
    Phase {
        /// The scatter point (the light strategy's pdf is evaluated from it).
        pos: Vec3A,
        /// Solid-angle pdf of the sampled phase direction.
        pdf: f32,
    },
}

/// MIS weight for emission reached by the previous vertex's bounce ray.
/// Delta samples are invisible to light sampling (their lobe is excluded
/// from eval), so the bounce carries the emission whole — likewise at
/// vertices where NEE is inactive, and for emissive geometry with no
/// light-list entry, which NEE can never sample. Otherwise the competing
/// density is the same strategy the NEE side uses: uniform 1-of-N pick
/// times the hit light's area-sampling pdf.
fn bounce_emission_weight(
    prev: &PrevVertex,
    lights: &LightList,
    hit: &WorldHit,
    strategy: SamplingStrategy,
) -> f32 {
    let (from, bounce_pdf) = match prev {
        PrevVertex::Surface(p) => {
            if p.delta || p.mat.eval(&p.ray, &p.rec, p.dir).is_none() {
                return 1.0;
            }
            (p.rec.p, p.pdf)
        }
        PrevVertex::Phase { pos, pdf } => (*pos, *pdf),
    };
    match lights.find_by_geom(hit.geom_id) {
        Some(light) => {
            let light_pdf =
                (light.pdf_at_point(from, hit.rec.p) / lights.count() as f32).max(1e-6);
            strategy.bounce_weight(bounce_pdf, light_pdf)
        }
        None => 1.0,
    }
}

/// The infinite-light half of bounce-side MIS: what a ray that left the
/// scene along `direction` picks up.
///
/// This is [`bounce_emission_weight`]'s mirror. A light with geometry is
/// found by a bounce ray *hitting* it and weighted by `pdf_at_point`; a
/// light at infinity is found by a bounce ray *escaping* along a direction
/// it covers, and weighted by the pdf reported from `Light::escaped`. Both
/// must use the same density NEE used, or emission is double-counted.
///
/// Returns the MIS-weighted radiance and whether any light covered the
/// direction — the caller falls back to the sky gradient when nothing did.
fn escaped_emission(
    prev: &Option<PrevVertex>,
    lights: &LightList,
    direction: Vec3A,
    strategy: SamplingStrategy,
) -> (Vec3A, bool) {
    if lights.count() == 0 {
        return (Vec3A::ZERO, false);
    }
    // As on the bounce-hit path, a delta or non-evaluable previous vertex
    // means NEE could not have found this light, so there is no competing
    // strategy and the emission is taken whole.
    let competing = match prev {
        Some(PrevVertex::Surface(p)) => {
            (!p.delta && p.mat.eval(&p.ray, &p.rec, p.dir).is_some()).then_some((p.rec.p, p.pdf))
        }
        Some(PrevVertex::Phase { pos, pdf }) => Some((*pos, *pdf)),
        // Primary rays, and rays leaving a carried-medium scatter, run no
        // NEE — full weight, exactly as `prev = None` means elsewhere.
        None => None,
    };
    let n_lights = lights.count() as f32;

    let mut radiance = Vec3A::ZERO;
    let mut covered = false;
    for light in &lights.lights {
        let from = competing.map_or(Vec3A::ZERO, |(p, _)| p);
        let Some((emitted, pdf)) = light.escaped(from, direction) else {
            continue;
        };
        covered = true;
        let weight = match competing {
            Some((_, bounce_pdf)) if strategy.samples_lights() => {
                let light_pdf = (pdf / n_lights).max(1e-6);
                strategy.bounce_weight(bounce_pdf, light_pdf)
            }
            // No NEE ran for this vertex (or the strategy does not sample
            // lights at all), so nothing competes.
            _ => 1.0,
        };
        radiance += emitted * weight;
    }
    (radiance, covered)
}

/// NEE shadow test used at surface and volume vertices alike: ZERO when a
/// surface occludes the segment, otherwise the volumetric transmittance
/// through every region it crosses (stochastic for heterogeneous regions,
/// exact for homogeneous ones). MIS weights are unaffected — transmittance
/// is part of the integrand on both strategies, not of either pdf.
fn shadow_transmittance(
    world: &World,
    volumes: &Volumes,
    shadow_ray: &Ray,
    distance: f32,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    // Dedicated occlusion query: any hit in range means full shadow, so the
    // early-exit traversal beats searching for the closest hit.
    stats.shadow_rays += 1;
    if world.occluded(shadow_ray, 0.001, distance - 0.001) {
        return Vec3A::ZERO;
    }
    if volumes.is_empty() {
        return Vec3A::ONE;
    }
    let mut rng = vertex.new_domain(K_NEE_SHADOW).rng();
    volumes.transmittance(shadow_ray, 0.001, distance - 0.001, &mut rng)
}

/// Direct lighting at a volume-region scatter point. The exact mirror of
/// the surface NEE block: same uniform 1-of-N light strategy, with the
/// phase function (value == pdf for the HG mixture) in place of
/// `brdf·cos`, and the same phase pdf as the competing bounce density that
/// `bounce_emission_weight`'s `Phase` arm uses.
fn volume_nee(
    p: Vec3A,
    wi: Vec3A,
    phase: &PhaseMix,
    world: &World,
    volumes: &Volumes,
    lights: &LightList,
    strategy: SamplingStrategy,
    vertex: PathSampler,
    time: f32,
    stats: &mut RayStats,
) -> Vec3A {
    if !strategy.samples_lights() {
        return Vec3A::ZERO;
    }
    let nee = vertex.new_domain(K_NEE).draw_sample_f32::<4>();
    let Some(light) = lights.pick(nee[0]) else {
        return Vec3A::ZERO;
    };
    let n_lights = lights.count() as f32;
    let Some(s) = light.sample_li(p, nee[1], nee[2]) else {
        return Vec3A::ZERO;
    };
    let shadow_ray = Ray::new(p, s.direction)
        .with_time(time)
        .with_mask(crate::ray::MASK_SHADOW);
    let tr = shadow_transmittance(world, volumes, &shadow_ray, s.distance, vertex, stats);
    if tr == Vec3A::ZERO {
        return Vec3A::ZERO;
    }
    let light_pdf = (s.pdf / n_lights).max(1e-6);
    let phase_val = phase.pdf(wi.dot(s.direction));
    let weight = strategy.light_weight(light_pdf, phase_val);
    s.radiance * phase_val * tr * weight / light_pdf
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
    world: &World,
    lights: &LightList,
    volumes: &Volumes,
    depth: i32,
    strategy: SamplingStrategy,
    sampler: PathSampler,
    guiding: Option<&GuidingContext>,
    train_out: &mut Vec<SampleData>,
    scratch: &mut PathScratch,
    stats: &mut RayStats,
) -> Vec3A {
    let training = guiding.is_some_and(|g| g.training);
    // The bounce subtree; each vertex derives its own domain off this by depth.
    let path = sampler.new_domain(K_PATH);
    // Borrowed, not allocated — see `PathScratch`. Capacity carries over from
    // the previous sample, so after the first walk this is free.
    let records = &mut scratch.records;
    records.clear();
    let mut ray = r.clone();
    let mut remaining = depth;
    // Set after surface bounces and volume-region phase scatters; `None`
    // at the primary vertex and after carried-medium (subsurface)
    // scatters, where the next vertex's emission counts fully.
    let mut prev: Option<PrevVertex> = None;
    // Running throughput. Only drives the roulette survival probability —
    // the estimate itself is rebuilt by the backward gather.
    let mut beta = Vec3A::ONE;
    // Radiance entering the path from beyond the last vertex.
    let mut terminal = Vec3A::ZERO;

    loop {
        // This vertex's domain: `records.len()` is the vertex index (nothing
        // has been pushed for it yet). Every per-event draw hangs off `v`.
        let v = path.new_domain(records.len() as i32);

        if remaining <= 0 {
            stats.ended_depth += 1;
            // Depth exhausted. The old recursion still counted bounce-hit
            // emission at the last vertex (its `add_emission` term traced
            // the ray itself) but never the background — reproduce both,
            // attenuating through any media the final segment crosses.
            if let Some(p) = &prev {
                stats.closest_hit += 1;
                if let Some(hit) = world.intersect(&ray, 0.001, f32::INFINITY) {
                    let cos_o = ray.direction().normalize().dot(hit.rec.normal).abs();
                    let mut emitted = hit.mat.emitted_directional(cos_o);
                    if emitted.length_squared() > 0.0 {
                        if let Some(m) = ray.medium() {
                            emitted *= m.transmittance(hit.rec.t);
                        }
                        if !volumes.is_empty() {
                            let mut rng = v.new_domain(K_VOLUME).rng();
                            emitted *= volumes.transmittance(&ray, 0.001, hit.rec.t, &mut rng);
                        }
                        let last = records.last_mut().expect("prev implies a record");
                        last.next_emit = emitted;
                        last.next_emit_weight = bounce_emission_weight(p, lights, &hit, strategy);
                    }
                }
            }
            break;
        }

        stats.closest_hit += 1;
        let hit_opt = world.intersect(&ray, 0.001, f32::INFINITY);
        let t_surf = hit_opt.as_ref().map_or(f32::INFINITY, |h| h.rec.t);

        // Free-flight candidate in the carried homogeneous medium
        // (subsurface / participating glass interiors) — analog sampling
        // at the extinction majorant. Incidental (unbounded across bounces),
        // so it uses the PRNG side rather than a stratified dimension.
        let t_med = match ray.medium() {
            Some(m) if m.is_scattering() => {
                let sigma_t_max = m.sigma_t_max().max(1e-4);
                -(v.new_domain(K_MEDIUM).draw_rnd_f32::<1>()[0].ln()) / sigma_t_max
            }
            _ => f32::INFINITY,
        };

        // Volume-region interaction, clipped to whatever event would
        // otherwise end the segment. Because the walk is bounded by
        // `t_lim`, a real collision is the nearest event by construction —
        // the competition between the carried medium, the surface and the
        // regions is exact (superposed processes), and the `Passthrough`
        // weight is precisely the region transmittance up to the winner.
        let t_lim = t_surf.min(t_med);
        let event = if volumes.is_empty() {
            VolumeEvent::Passthrough {
                transmittance: Vec3A::ONE,
                emitted: Vec3A::ZERO,
            }
        } else {
            let mut rng = v.new_domain(K_VOLUME).rng();
            volumes.sample_interaction(&ray, 0.001, t_lim, &mut rng)
        };

        let (vol_tr, vol_emit) = match event {
            VolumeEvent::Scatter {
                p,
                weight,
                phase,
                emitted,
                ..
            } => {
                // === Volume-region scatter vertex ===
                let wi = ray.direction().normalize();
                let ps = v.new_domain(K_PHASE).draw_sample_f32::<4>();
                let dir = phase.sample(wi, ps[0], [ps[1], ps[2]]);
                let phase_pdf = phase.pdf(wi.dot(dir)).max(1e-6);
                let nee =
                    volume_nee(p, wi, &phase, world, volumes, lights, strategy, v, ray.time(), stats);

                // The walk weight goes into `atten` (it multiplies NEE and
                // everything beyond); the continuation factor is ONE
                // because the HG value and pdf cancel exactly. Volume
                // vertices are not trained on — the field guides surface
                // bounces only.
                let mut vrec = VertexRec {
                    atten: weight,
                    segment_emit: emitted,
                    emit_here: Vec3A::ZERO,
                    nee,
                    factor: Vec3A::ONE,
                    next_emit: Vec3A::ZERO,
                    next_emit_weight: 1.0,
                    train: None,
                };
                beta *= weight;
                let mut survived = true;
                if records.len() >= RR_START_BOUNCE {
                    stats.rr_tested += 1;
                    let p_survive = beta.max_element().clamp(RR_MIN_PROB, 1.0);
                    if p_survive < 1.0 {
                        if v.new_domain(K_RR).draw_rnd_f32::<1>()[0] >= p_survive {
                            survived = false;
                            stats.rr_killed += 1;
                            vrec.factor = Vec3A::ZERO;
                        } else {
                            vrec.factor /= p_survive;
                            beta /= p_survive;
                        }
                    }
                }
                if !survived {
                    stats.vertices += 1;
                    records.push(vrec);
                    break;
                }
                prev = Some(PrevVertex::Phase { pos: p, pdf: phase_pdf });
                stats.vertices += 1;
                records.push(vrec);
                // Preserve the carried medium: scattering in fog inside a
                // glass interior must keep attenuating in the glass.
                ray = match ray.medium() {
                    Some(m) => Ray::new_in_medium(p, dir, m.clone()),
                    None => Ray::new(p, dir),
                }
                .with_time(ray.time())
                .with_mask(crate::ray::MASK_INDIRECT);
                remaining -= 1;
                continue;
            }
            VolumeEvent::Passthrough {
                transmittance,
                emitted,
            } => (transmittance, emitted),
        };

        if t_med < t_surf {
            // === Carried-medium scatter vertex (subsurface interiors) ===
            let medium = ray.medium().expect("t_med implies a medium").clone();
            let sigma_bar = medium.sigma_t_max().max(1e-4);
            let pos = ray.at(t_med);
            let phase_uv = v.new_domain(K_PHASE).draw_sample_f32::<2>();
            let dir = sample_henyey_greenstein(
                ray.direction().normalize(),
                medium.g,
                phase_uv[0],
                phase_uv[1],
            );
            // Weighted analog estimator: sampling was at rate σ̄ (the
            // max-channel extinction), so the event pays σₛ/σ̄ with the
            // chromatic correction e^{(σ̄−σₜ)·t} per channel. For a gray
            // medium this is exactly the single-scattering albedo — the
            // old code's `factor = albedo` with an extra Beer-Lambert on
            // top double-counted extinction.
            let e = (Vec3A::splat(sigma_bar) - (medium.sigma_a + medium.sigma_s)) * t_med;
            let factor = medium.sigma_s / sigma_bar * Vec3A::new(e.x.exp(), e.y.exp(), e.z.exp());
            // Subsurface vertices run no NEE (their shadow rays are
            // blocked by the enclosing surface), so `prev = None` keeps
            // the next hit's emission at full weight — the pairing that
            // avoids double counting.
            let mut vrec = VertexRec {
                atten: vol_tr,
                segment_emit: vol_emit,
                emit_here: Vec3A::ZERO,
                nee: Vec3A::ZERO,
                factor,
                next_emit: Vec3A::ZERO,
                next_emit_weight: 1.0,
                train: None,
            };
            beta *= vol_tr * factor;
            let mut survived = true;
            if records.len() >= RR_START_BOUNCE {
                stats.rr_tested += 1;
                let p_survive = beta.max_element().clamp(RR_MIN_PROB, 1.0);
                if p_survive < 1.0 {
                    if v.new_domain(K_RR).draw_rnd_f32::<1>()[0] >= p_survive {
                        survived = false;
                        stats.rr_killed += 1;
                        vrec.factor = Vec3A::ZERO;
                    } else {
                        vrec.factor /= p_survive;
                        beta /= p_survive;
                    }
                }
            }
            if !survived {
                stats.vertices += 1;
                records.push(vrec);
                break;
            }
            stats.vertices += 1;
            records.push(vrec);
            ray = Ray::new_in_medium(pos, dir, medium)
                .with_time(ray.time())
                .with_mask(crate::ray::MASK_INDIRECT);
            remaining -= 1;
            prev = None;
            continue;
        }

        let Some(hit) = hit_opt else {
            stats.ended_escaped += 1;
            // === Background ===
            // A ray leaving the scene is how lights at infinity are found
            // by chance, so it is a bounce-side MIS event just like hitting
            // an emissive surface.
            let unit_direction = Vec3A::normalize(ray.direction());
            let (mut background, covered) =
                escaped_emission(&prev, lights, unit_direction, strategy);
            if !covered {
                // Nothing at infinity covers this direction — keep the
                // built-in sky gradient so scenes without an environment
                // light look as they always have.
                let t = 0.5 * (unit_direction.y + 1.0);
                background +=
                    (1.0 - t) * Vec3A::new(1.0, 1.0, 1.0) + t * Vec3A::new(0.5, 0.7, 1.0);
            }
            // Segment emission is already weighted; the background pays the
            // volume transmittance of the final segment.
            terminal = vol_emit + vol_tr * background;
            break;
        };
        let rec: HitRecord = hit.rec;
        let mat = hit.mat;

        // Attenuation across the arriving segment: volume-region
        // transmittance times the carried medium's. For a *scattering*
        // medium the surface arrival already paid e^{−σ̄·t} through the
        // free-flight competition (t_med ≥ t_surf), so only the chromatic
        // correction e^{(σ̄−σₜ)·t} remains — exactly ONE for gray media.
        // Non-scattering media (glass tint) keep pure Beer-Lambert.
        let med_arrival = match ray.medium() {
            Some(m) if m.is_scattering() => {
                let sigma_bar = m.sigma_t_max().max(1e-4);
                let e = (Vec3A::splat(sigma_bar) - (m.sigma_a + m.sigma_s)) * rec.t;
                Vec3A::new(e.x.exp(), e.y.exp(), e.z.exp())
            }
            Some(m) => m.transmittance(rec.t),
            None => Vec3A::ONE,
        };
        let atten = vol_tr * med_arrival;

        // Emission accounting: a vertex reached by a bounce hands its
        // emission to the previous vertex's record, MIS-weighted —
        // counting it here too would double it. At the primary vertex and
        // after carried-medium scatters it counts here, in full. Either
        // way the emission pays the arriving segment's attenuation (an
        // emitter seen through tinted glass or smoke must dim).
        let cos_o = ray.direction().normalize().dot(rec.normal).abs();
        let emitted = mat.emitted_directional(cos_o);
        let mut emit_here = Vec3A::ZERO;
        match &prev {
            Some(p) => {
                if emitted.length_squared() > 0.0 {
                    let last = records.last_mut().expect("prev implies a record");
                    last.next_emit = atten * emitted;
                    last.next_emit_weight = bounce_emission_weight(p, lights, &hit, strategy);
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
        let nee_s = v.new_domain(K_NEE).draw_sample_f32::<4>();
        // `sample_li` returns `None` when the light cannot be reached from
        // this point at all — below a dome's horizon, or a degenerate
        // coincident point.
        if let Some(light) = strategy
            .samples_lights()
            .then(|| lights.pick(nee_s[0]))
            .flatten()
            && let Some(ls) = light.sample_li(rec.p, nee_s[1], nee_s[2])
        {
            let n_lights = lights.count() as f32;
            let light_dir_unit = ls.direction;

            let shadow_ray = Ray::new(rec.p, light_dir_unit)
                .with_time(ray.time())
                .with_mask(crate::ray::MASK_SHADOW);

            let shadow_tr =
                shadow_transmittance(world, volumes, &shadow_ray, ls.distance, v, stats);
            if shadow_tr != Vec3A::ZERO {
                // Unsigned: lights behind the ray-facing normal are reachable
                // through a continuous transmission lobe (opaque materials
                // evaluate to zero there anyway).
                let cosine = rec.normal.dot(light_dir_unit).abs();
                let light_pdf = (ls.pdf / n_lights).max(1e-6);

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
                    let weight = strategy.light_weight(light_pdf, bounce_pdf);
                    nee += ls.radiance * brdf_value * cosine * shadow_tr * weight
                        / light_pdf;
                }
            }
        }

        let mut vrec = VertexRec {
            atten,
            segment_emit: vol_emit,
            emit_here,
            nee,
            factor: Vec3A::ZERO,
            next_emit: Vec3A::ZERO,
            next_emit_weight: 1.0,
            train: None,
        };

        // === 2. Indirect Lighting via BSDF (or guided) Sampling ===
        if let Some(sample) = sample_bounce_direction(&ray, &rec, mat, guiding_here, v) {
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
                stats.rr_tested += 1;
                let p_survive = beta.max_element().clamp(RR_MIN_PROB, 1.0);
                if p_survive < 1.0 {
                    if v.new_domain(K_RR).draw_rnd_f32::<1>()[0] >= p_survive {
                        survived = false;
                        stats.rr_killed += 1;
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
                prev = Some(PrevVertex::Surface(PrevBounce {
                    ray: ray.clone(),
                    rec,
                    mat,
                    dir,
                    pdf: sample.pdf,
                    delta: sample.delta,
                }));
                stats.vertices += 1;
                records.push(vrec);
                // Materials build the scattered ray without path context;
                // stamp the path's shutter time and the indirect category.
                ray = sample
                    .ray
                    .with_time(ray.time())
                    .with_mask(crate::ray::MASK_INDIRECT);
                remaining -= 1;
                continue;
            }
        }

        // Absorbed or roulette-killed: this vertex's own gathers stand
        // (factor stays zero), the path ends here.
        stats.vertices += 1;
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
        radiance = vrec.segment_emit
            + vrec.atten
                * (vrec.emit_here
                    + vrec.nee
                    + vrec.factor * (vrec.next_emit * vrec.next_emit_weight + radiance));
    }
    radiance
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

#[cfg(test)]
mod tests {
    use super::SamplingStrategy;

    /// The invariant every strategy must keep: for a light both strategies
    /// can reach, the NEE weight and the bounce-emission weight are a
    /// partition of unity — anything else double-counts or loses emission.
    #[test]
    fn strategy_weights_partition_unity() {
        let strategies = [
            SamplingStrategy::PowerMis,
            SamplingStrategy::BalanceMis,
            SamplingStrategy::LightOnly,
            SamplingStrategy::BsdfOnly,
        ];
        // (light_pdf, bounce_pdf) pairs spanning near-delta glossy spikes,
        // balanced cases, and tiny-light spikes.
        let pdf_pairs = [
            (0.5, 0.5),
            (1e-4, 1e4),
            (1e4, 1e-4),
            (3.0, 0.2),
            (0.05, 40.0),
        ];
        for s in strategies {
            for (light_pdf, bounce_pdf) in pdf_pairs {
                let sum = s.light_weight(light_pdf, bounce_pdf)
                    + s.bounce_weight(bounce_pdf, light_pdf);
                assert!(
                    (sum - 1.0).abs() < 1e-3,
                    "{s:?}: weights sum to {sum} at pdfs ({light_pdf}, {bounce_pdf})"
                );
            }
        }
    }

    #[test]
    fn single_strategy_modes_disable_the_other_side() {
        assert!(!SamplingStrategy::BsdfOnly.samples_lights());
        assert!(SamplingStrategy::LightOnly.samples_lights());
        assert_eq!(SamplingStrategy::LightOnly.light_weight(1.0, 100.0), 1.0);
        assert_eq!(SamplingStrategy::LightOnly.bounce_weight(100.0, 1.0), 0.0);
        assert_eq!(SamplingStrategy::BsdfOnly.light_weight(100.0, 1.0), 0.0);
        assert_eq!(SamplingStrategy::BsdfOnly.bounce_weight(1.0, 100.0), 1.0);
    }

    /// The power heuristic commits harder to the denser strategy than the
    /// balance heuristic — the property that makes it the better default on
    /// glossy surfaces.
    #[test]
    fn power_sharpens_balance() {
        let (a, b) = (10.0, 1.0);
        let balance = SamplingStrategy::BalanceMis.light_weight(a, b);
        let power = SamplingStrategy::PowerMis.light_weight(a, b);
        assert!(power > balance, "power {power} <= balance {balance}");
    }

    use super::{Renderer, StepStatus, StopToken};
    use crate::tracer::RenderSettings;
    use crate::world::simple_scene;
    use crate::{Buffer, Camera};
    use glam::Vec3A;

    /// A tiny renderer over the procedural scene — small enough that the
    /// bitwise progressive-vs-batch comparisons below run in milliseconds.
    fn tiny_renderer(spp: u32, adaptive: bool) -> Renderer {
        let (world, lights) = simple_scene();
        let camera = Camera::new(
            Vec3A::new(15.0, 3.0, 3.0),
            Vec3A::new(0.0, 1.0, 0.0),
            Vec3A::new(0.0, 1.0, 0.0),
            20.0,
            64.0 / 36.0,
            0.1,
            10.0,
        );
        // variance_threshold > 0 with a low min_spp arms the adaptive early
        // stop; 0 disables it.
        let (min_spp, threshold) = if adaptive { (4, 0.5) } else { (0, 0.0) };
        let settings = RenderSettings::new(spp, 8, 64, 36, min_spp, threshold, 0);
        Renderer::new(camera, world, lights, settings)
    }

    fn assert_buffers_bit_identical(a: &Buffer, b: &Buffer, what: &str) {
        assert_eq!((a.width(), a.height()), (b.width(), b.height()));
        for (idx, (pa, pb)) in a.as_slice().iter().zip(b.as_slice()).enumerate() {
            assert_eq!(
                [pa.x.to_bits(), pa.y.to_bits(), pa.z.to_bits()],
                [pb.x.to_bits(), pb.y.to_bits(), pb.z.to_bits()],
                "{what}: pixel {idx} differs ({pa:?} vs {pb:?})"
            );
        }
    }

    /// The load-bearing Phase 0 guarantee: a progressive render run to
    /// completion is bit-identical to the batch render — for any chunk
    /// size, with adaptive sampling off and on, scanline and tiled.
    #[test]
    fn progressive_equals_batch_bitwise() {
        for adaptive in [false, true] {
            for tiled in [false, true] {
                let renderer = tiny_renderer(16, adaptive);
                let batch = if tiled {
                    renderer.render_with_tiles()
                } else {
                    renderer.render()
                };
                for chunk in [1u32, 3, 5, 16] {
                    let mut session = renderer.begin_progressive(tiled);
                    while !session.is_complete() {
                        session.step(chunk, None, None);
                    }
                    let (progressive, _) = session.finish();
                    assert_buffers_bit_identical(
                        &batch,
                        &progressive,
                        &format!("adaptive={adaptive} tiled={tiled} chunk={chunk}"),
                    );
                }
            }
        }
    }

    /// Guided progressive (whole training passes, chunked final pass) must
    /// reproduce the guided batch render bit for bit.
    ///
    /// One training iteration on purpose: with two or more, the guiding
    /// efficiency estimate (`decide_guide_final`) compares *wall-clock*
    /// pass costs, so even two batch renders of the same scene can
    /// legitimately pick different final passes when ΔEff sits near 1 —
    /// a pre-existing property of the guided pipeline, not of the
    /// progressive refactor. A single iteration never produces the
    /// estimate, making the guided/unguided decision (and therefore the
    /// image) deterministic.
    #[test]
    fn guided_progressive_equals_guided_batch() {
        let mut renderer = tiny_renderer(8, false);
        renderer.settings = renderer.settings.with_guiding(true, 1, 0.5);
        let batch = renderer.render();
        let mut session = renderer.begin_progressive(false);
        while !session.is_complete() {
            session.step(3, None, None);
        }
        let (progressive, _) = session.finish();
        assert_buffers_bit_identical(&batch, &progressive, "guided");
    }

    /// A fired stop leaves a coherent partial image (no NaNs, unreached
    /// pixels black), and resuming afterwards still converges to the exact
    /// batch result.
    #[test]
    fn stopped_render_is_coherent_and_resumes() {
        let renderer = tiny_renderer(16, false);
        let batch = renderer.render();

        let stop = StopToken::new();
        stop.stop(); // fire before the first row: the whole step is skipped
        let mut session = renderer.begin_progressive(false);
        let status = session.step(16, None, Some(&stop));
        assert_eq!(status, StepStatus::Stopped { spp_done: 0 });
        let snapshot = session.snapshot();
        for p in snapshot.as_slice() {
            assert!(p.is_finite(), "stopped snapshot contains non-finite pixels");
        }

        // Resume without the token: the interrupted work is picked back up
        // and the completed render matches batch bitwise.
        while !session.is_complete() {
            session.step(4, None, None);
        }
        let (resumed, _) = session.finish();
        assert_buffers_bit_identical(&batch, &resumed, "stop/resume");

        // The controlled one-shot entry point reports the interruption too.
        let stop2 = StopToken::new();
        stop2.stop();
        let (partial, _, completed) = renderer.render_with_control(false, None, Some(&stop2));
        assert!(!completed);
        for p in partial.as_slice() {
            assert!(p.is_finite());
        }
    }

    /// `render_with_control` without a token is exactly the plain render.
    #[test]
    fn controlled_render_without_token_matches_batch() {
        let renderer = tiny_renderer(8, true);
        let batch = renderer.render();
        let (controlled, _, completed) = renderer.render_with_control(false, None, None);
        assert!(completed);
        assert_buffers_bit_identical(&batch, &controlled, "render_with_control");
    }

    /// Mid-flight snapshots are coherent and `snapshot_into` agrees with
    /// `snapshot`.
    #[test]
    fn snapshots_are_coherent_mid_render() {
        let renderer = tiny_renderer(16, false);
        let mut session = renderer.begin_progressive(true);
        session.step(4, None, None);
        assert_eq!(session.spp_done(), 4);
        let snap = session.snapshot();
        let mut into = Buffer::new(1, 1); // wrong size on purpose: must resize
        session.snapshot_into(&mut into);
        assert_buffers_bit_identical(&snap, &into, "snapshot_into");
        for p in snap.as_slice() {
            assert!(p.is_finite());
        }
    }
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
