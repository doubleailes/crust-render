//! `crust diagnostic`: measure how to make a scene's render faster or
//! cleaner, within a time budget, and report the evidence in a form a
//! machine can act on (`openspec/specs/diagnostics`).
//!
//! One import, then phases in priority order until the budget is spent:
//!
//! - **P1, the baseline**: a 1 spp calibration render sizes a full-frame
//!   render at a low fixed spp, with the clamp and adaptive sampling off,
//!   which measures the noise by light path, time per tile, the clamp's
//!   effect, path and cache statistics. The static checks read it.
//! - **Crops**: up to three windows chosen from the baseline's maps.
//! - **Tier 1**: every unbiased setting swapped one at a time, then the
//!   winners combined, each measured as interleaved baseline/trial pairs
//!   on every crop and judged by efficiency `1/(time · MRSE)`.
//! - **Tier 2**: the sample count a target error needs, and adaptive
//!   sampling measured.
//! - **Tier 3**: the settings that change the picture, measured only.
//!
//! The engine writes nothing: [`run`] returns a [`Report`], which the host
//! prints ([`Report::to_markdown`]) and saves ([`Report::to_json`]).

mod checks;
mod compare;
mod crops;
mod markdown;
mod noise;
pub mod report;
mod schedule;
mod trials;

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::tracer::{Instruments, Measured};
use crate::{
    LightSelection, PixelRect, PtexCacheStats, RenderSettings, Renderer, SamplingStrategy, Scene,
    TextureCacheStats, profile,
};
use report::*;
use trials::CropImage;

pub use report::{FORMAT, Report};

/// The cache counters the host's asset loader keeps — read before and
/// after the baseline, whose difference is the baseline's hit rate.
pub type CacheStats<'a> = &'a (dyn Fn() -> (TextureCacheStats, PtexCacheStats) + Sync);

/// What the host knows that the scene does not: how it was imported and
/// what the diagnosis is asked for.
pub struct Options<'a> {
    /// The stage, as given on the command line.
    pub scene_path: String,
    pub frame: Option<f64>,
    pub camera: Option<String>,
    /// `--region`, image space: the only crop.
    pub region: Option<PixelRect>,
    pub budget: Duration,
    /// Interleaved baseline/trial pairs per crop (`--repeats`).
    pub repeats: u32,
    /// `--target-mrse`; the square of the scene's adaptive threshold
    /// otherwise.
    pub target_mrse: Option<f64>,
    /// The JSON text of `--baseline PREV.json`.
    pub previous: Option<String>,
    /// How long the import took — reported, not counted.
    pub import: Duration,
    pub auto_tx: bool,
    /// `--subdiv-level` / `--subdiv-edge-length`, when given.
    pub subdivision_level: Option<u32>,
    pub subdivision_edge_length: Option<f32>,
    pub cache_stats: Option<CacheStats<'a>>,
}

impl Options<'_> {
    /// Defaults for a scene at `path`: a 120 s budget, three repeats.
    pub fn new(scene_path: impl Into<String>) -> Self {
        Options {
            scene_path: scene_path.into(),
            frame: None,
            camera: None,
            region: None,
            budget: Duration::from_secs(120),
            repeats: 3,
            target_mrse: None,
            previous: None,
            import: Duration::ZERO,
            auto_tx: false,
            subdivision_level: None,
            subdivision_edge_length: None,
            cache_stats: None,
        }
    }
}

/// Above this many lights, the light groups are not one per light.
const MAX_GROUP_LIGHTS: usize = 8;

/// `settings` as every efficiency comparison renders them (D3): the clamp
/// off, adaptive sampling off, `spp` samples in every pixel.
fn probe(settings: RenderSettings, spp: u32) -> RenderSettings {
    settings
        .with_indirect_clamp(0.0)
        .with_samples_per_pixel(spp)
        .with_adaptive_sampling(spp, 0.0)
}

/// A tier-1 change: one setting, by its CLI and USD names.
#[derive(Clone)]
struct Change {
    factor: &'static str,
    value: String,
    flag: Option<&'static str>,
    attribute: &'static str,
    apply: Apply,
}

impl Change {
    fn id(&self) -> String {
        format!("{}={}", self.factor, self.value)
    }
}

fn set_strategy(s: RenderSettings, v: &str) -> RenderSettings {
    s.with_sampling_strategy(v.parse().expect("a strategy name"))
}

fn set_selection(s: RenderSettings, v: &str) -> RenderSettings {
    s.with_light_selection(v.parse().expect("a selection name"))
}

fn set_light_samples(s: RenderSettings, v: &str) -> RenderSettings {
    s.with_light_samples(v.parse().expect("a count"), s.light_samples_indirect())
}

fn set_light_samples_indirect(s: RenderSettings, v: &str) -> RenderSettings {
    s.with_light_samples(s.light_samples(), v.parse().expect("a count"))
}

fn set_guiding(s: RenderSettings, v: &str) -> RenderSettings {
    s.with_guiding(v == "true", s.guiding_train_iterations(), s.guiding_prob())
}

type Apply = fn(RenderSettings, &str) -> RenderSettings;

fn change(
    factor: &'static str,
    value: String,
    flag: Option<&'static str>,
    attribute: &'static str,
    apply: Apply,
) -> Change {
    Change {
        factor,
        value,
        flag,
        attribute,
        apply,
    }
}

/// The tier-1 changes of `factor` from `base`, or why there are none.
fn changes(factor: &str, base: RenderSettings, lights: usize) -> Result<Vec<Change>, String> {
    let needs_lights = factor != "guiding";
    if needs_lights && lights == 0 {
        return Err("the scene has no light-list entry".into());
    }
    Ok(match factor {
        "strategy" => SamplingStrategy::CHOICES
            .iter()
            .filter(|(v, _, _)| *v != base.sampling_strategy())
            .map(|(_, name, _)| {
                change(
                    "strategy",
                    name.to_string(),
                    Some("--strategy"),
                    "crust:samplingStrategy",
                    set_strategy,
                )
            })
            .collect(),
        "light_selection" => {
            if lights < 2 {
                return Err("one light: every selection picks it".into());
            }
            LightSelection::CHOICES
                .iter()
                .filter(|(v, _, _)| *v != base.light_selection())
                .map(|(_, name, _)| {
                    change(
                        "light_selection",
                        name.to_string(),
                        Some("--light-selection"),
                        "crust:lightSelection",
                        set_selection,
                    )
                })
                .collect()
        }
        "light_samples" => {
            if base.light_samples() != 1 {
                return Err(format!(
                    "already {} per camera vertex",
                    base.light_samples()
                ));
            }
            ["2", "4"]
                .into_iter()
                .map(|n| {
                    change(
                        "light_samples",
                        n.into(),
                        Some("--light-samples"),
                        "crust:lightSamples",
                        set_light_samples,
                    )
                })
                .collect()
        }
        "light_samples_indirect" => {
            if base.light_samples_indirect() == 2 {
                return Err("already 2 per indirect vertex".into());
            }
            vec![change(
                "light_samples_indirect",
                "2".into(),
                Some("--light-samples-indirect"),
                "crust:lightSamplesIndirect",
                set_light_samples_indirect,
            )]
        }
        "guiding" => vec![change(
            "guiding",
            (!base.guiding()).to_string(),
            // crust render has no guiding flag: the stage authors it.
            None,
            "crust:pathGuiding",
            set_guiding,
        )],
        other => unreachable!("unknown factor {other}"),
    })
}

/// One measured render of a crop: its image, and its time split.
struct Shot {
    image: CropImage,
    setup_s: f64,
    render_s: f64,
}

impl Shot {
    fn time_s(&self) -> f64 {
        self.setup_s + self.render_s
    }
}

/// Each pixel's luminance in a measured render, indexed as its variance
/// map (raster space, over the render's region).
fn luminance(m: &Measured, rect: PixelRect, luma: utils::Luma) -> Vec<f64> {
    let mut out = vec![0.0; rect.area()];
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            out[rect.index(x, y)] = luma.of(m.buffer.get_pixel(x, y)) as f64;
        }
    }
    out
}

/// Renders `settings` (switching the renderer to them) and measures it: the
/// light selection's setup and guiding's training count as setup.
fn shoot(renderer: &mut Renderer, settings: RenderSettings) -> (Shot, Measured) {
    let selection = renderer.reconfigure(settings);
    let mut m = renderer.render_measured(
        None,
        Instruments {
            variance: true,
            quiet: true,
            ..Instruments::default()
        },
    );
    let rect = settings.region().flip_y(settings.get_dimensions().1);
    let lum = luminance(&m, rect, renderer.lights.luma());
    let shot = Shot {
        image: CropImage {
            lum,
            var: std::mem::take(&mut m.var_map),
        },
        setup_s: selection.as_secs_f64() + m.setup_s,
        render_s: m.render_s,
    };
    (shot, m)
}

/// A tier-1 trial's measurements on one crop.
struct CropRun {
    /// `(baseline, trial)` per pair, interleaved B T B T ….
    pairs: Vec<(Shot, Shot)>,
}

/// A tier-1 trial while it is being measured.
struct Running {
    changes: Vec<Change>,
    id: String,
    factor: String,
    value: String,
    per_crop: Vec<CropRun>,
}

impl Running {
    /// The single change's flag and attribute; `None` for the combined trial.
    fn names(&self) -> (Option<String>, Option<String>) {
        match &self.changes[..] {
            [c] => (c.flag.map(Into::into), Some(c.attribute.into())),
            _ => (None, None),
        }
    }
}

/// Judges `run` on every crop against the crops' references.
fn judge(run: &Running, crops: &[Crop], refs: &[CropImage], spp: u32) -> Trial {
    let mut per_crop = Vec::new();
    for ((crop, cr), reference) in crops.iter().zip(&run.per_crop).zip(refs) {
        let mrse = |s: &Shot| trials::mrse(&s.image.var, &reference.lum);
        let pairs: Vec<f64> = cr
            .pairs
            .iter()
            .map(|(b, t)| trials::delta_eff(b.time_s(), mrse(b), t.time_s(), mrse(t)))
            .collect();
        let (median, min, max) = trials::summary(&pairs);
        let med = |f: &dyn Fn(&(Shot, Shot)) -> f64| {
            trials::summary(&cr.pairs.iter().map(f).collect::<Vec<_>>()).0
        };
        per_crop.push(CropTrial {
            crop: crop.id.clone(),
            spp,
            delta_eff: pairs.iter().map(|&p| Num(p)).collect(),
            median: median.into(),
            min: min.into(),
            max: max.into(),
            mrse_baseline: med(&|(b, _)| mrse(b)).into(),
            mrse_trial: med(&|(_, t)| mrse(t)).into(),
            time_baseline_s: med(&|(b, _)| b.time_s()).into(),
            time_trial_s: med(&|(_, t)| t.time_s()).into(),
            setup_trial_s: med(&|(_, t)| t.setup_s).into(),
            verdict: trials::crop_verdict(&pairs),
        });
    }
    let (geo, verdict) = trials::overall(
        &per_crop
            .iter()
            .map(|c| (c.median.0, c.verdict))
            .collect::<Vec<_>>(),
    );
    let (flag, usd_attribute) = run.names();
    Trial {
        id: run.id.clone(),
        tier: 1,
        factor: run.factor.clone(),
        value: run.value.clone(),
        flag,
        usd_attribute,
        per_crop,
        overall_delta_eff: geo.is_finite().then_some(Num(geo)),
        verdict,
    }
}

/// The references of every crop: the blend of every unbiased image of it
/// rendered so far — the first baseline and each trial's first image.
fn references(runs: &[Running], crops: usize) -> Vec<CropImage> {
    (0..crops)
        .map(|k| {
            let mut images: Vec<&CropImage> = Vec::new();
            if let Some((b, _)) = runs.iter().find_map(|r| r.per_crop[k].pairs.first()) {
                images.push(&b.image);
            }
            for r in runs {
                if let Some((_, t)) = r.per_crop[k].pairs.first() {
                    images.push(&t.image);
                }
            }
            trials::reference(&images)
        })
        .collect()
}

/// Seconds as a [`Num`].
fn secs(d: Duration) -> Num {
    Num(d.as_secs_f64())
}

/// Diagnoses `scene`, imported once by the host, within `options.budget`.
/// Writes nothing; logs one `INFO` line per phase.
pub fn run(scene: Scene, options: &Options) -> Report {
    let clock = Instant::now();
    let elapsed = || clock.elapsed().as_secs_f64();
    let budget_s = options.budget.as_secs_f64();
    let mut sched = schedule::Schedule::new(budget_s);
    let repeats = options.repeats.max(1);
    let threads = rayon::current_num_threads();
    let authored = scene.settings;
    let (w, h) = authored.get_dimensions();
    // The crop a region asks for; a stage's own data window counts as one.
    let region = match options.region {
        Some(r) => r.clip_to(w, h),
        None => (!authored.is_full_frame()).then(|| authored.region()),
    };
    let full = authored.with_resolution(w, h);
    let cache = || options.cache_stats.map(|f| f()).unwrap_or_default();
    let mut phases: Vec<PhaseRun> = Vec::new();
    let mut not_tried: Vec<NotTried> = Vec::new();
    let mut exceeded: Option<String> = None;
    let import_stats = scene.stats;
    let textures_after_import = cache().0;

    // -- P1: calibration and the full-frame baseline -----------------------
    let mut lights = scene.lights;
    let groups = noise::label_groups(&mut lights, MAX_GROUP_LIGHTS);
    let light_count = lights.count();
    let luma = lights.luma();
    let started = Instant::now();
    let mut renderer = Renderer::new(scene.camera, scene.world, lights, probe(full, 1))
        .with_volumes(scene.volumes);
    let _ = renderer.render_measured(
        None,
        Instruments {
            quiet: true,
            ..Instruments::default()
        },
    );
    let calibration_s = started.elapsed().as_secs_f64();
    let spp_p1 = schedule::baseline_spp(budget_s, calibration_s);
    let rows = noise::rows(&groups);
    let request = noise::request(&rows);
    let before_p1 = cache();
    let p1_start = Instant::now();
    let selection = renderer.reconfigure(probe(full, spp_p1));
    profile::set_enabled(true);
    let p1 = renderer.render_measured(
        Some(&request),
        Instruments {
            tile_times: true,
            clamp: authored.indirect_clamp(),
            variance: true,
            quiet: true,
        },
    );
    let prof = profile::take();
    profile::set_enabled(false);
    let after_p1 = cache();
    let p1_s = p1_start.elapsed().as_secs_f64();
    sched.set_spent(elapsed());
    phases.push(PhaseRun {
        name: "P1".into(),
        time_s: Num(calibration_s + p1_s),
        completed: true,
    });
    if sched.exhausted() {
        exceeded = Some("P1".into());
    }
    let p1_setup = selection.as_secs_f64() + p1.setup_s;
    info!(
        "diagnostic P1: {w}x{h} at {spp_p1} spp in {:.2}s (calibration {:.2}s at 1 spp)",
        p1_s, calibration_s
    );

    let frame_rect = PixelRect::full(w, h);
    let p1_lum = luminance(&p1, frame_rect, luma);
    let p1_mrse = trials::mrse(&p1.var_map, &p1_lum);
    let pixels = (w * h) as f64;
    let rays = &p1.rays;
    let profile_top = prof
        .map(|p| {
            let total = p.thread_time().as_secs_f64().max(f64::MIN_POSITIVE);
            let mut v: Vec<(profile::Section, f64)> = profile::Section::ALL
                .iter()
                .map(|&s| (s, p.section(s).local.as_secs_f64() / total))
                .filter(|(_, share)| *share > 0.0)
                .collect();
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            v.into_iter()
                .take(5)
                .map(|(s, share)| ProfileShare {
                    section: s.name().into(),
                    share: share.into(),
                })
                .collect()
        })
        .unwrap_or_default();
    let tex_delta = (
        after_p1.0.lookups().saturating_sub(before_p1.0.lookups()),
        (after_p1.0.micro_hits + after_p1.0.hits)
            .saturating_sub(before_p1.0.micro_hits + before_p1.0.hits),
    );
    let ptex_hits = |p: &PtexCacheStats| p.micro_hits + p.cache_hits;
    let ptex_delta = (
        after_p1.1.lookups().saturating_sub(before_p1.1.lookups()),
        ptex_hits(&after_p1.1).saturating_sub(ptex_hits(&before_p1.1)),
    );
    let rate = |(n, hits): (u64, u64)| (n > 0).then(|| Num(hits as f64 / n as f64));
    let peak_mem = crate::peak_memory_bytes();
    let baseline = Baseline {
        calibration_time_s: calibration_s.into(),
        spp: spp_p1,
        time_s: (p1_setup + p1.render_s).into(),
        setup_s: p1_setup.into(),
        render_s: p1.render_s.into(),
        mrse: p1_mrse.into(),
        rays_per_s: (rays.total_rays() as f64 / p1.render_s.max(f64::MIN_POSITIVE)).into(),
        mean_path_length: rays.mean_path_length().into(),
        rr_kill_rate: rays.rr_kill_rate().into(),
        ended_by_depth_share: (rays.ended_depth as f64 / rays.camera_rays.max(1) as f64).into(),
        shadow_rays_per_vertex: rays.shadow_rays_per_vertex().into(),
        profile_top,
        texture_hit_rate: rate(tex_delta),
        ptex_hit_rate: rate(ptex_delta),
        peak_mem_bytes: peak_mem,
    };

    // -- Noise breakdown and static findings -------------------------------
    let film = p1.film.as_ref().expect("the noise request needs a film");
    let measured = noise::measure(&rows, film, &p1.buffer, luma);
    let (components, light_groups) = measured.split_at(noise::COMPONENTS.len());
    let dominant = noise::dominant(components);
    let noise_breakdown = NoiseBreakdown {
        components: components.to_vec(),
        light_groups: light_groups.to_vec(),
        light_groups_by: groups.by.into(),
        dominant: dominant.clone(),
    };
    let unlit = components
        .iter()
        .find(|r| r.key == "unlit_emitters")
        .map(|r| r.mean_luminance.0);
    let facts = checks::Facts {
        lights: light_count,
        light_selection: authored.light_selection(),
        auto_tx: options.auto_tx,
        textures_without_tx: textures_after_import.preloaded,
        texture_lookups: Some(tex_delta),
        ptex_lookups: Some(ptex_delta),
        unlit_emission: unlit,
        guiding: authored.guiding(),
        indirect_dominant: Some(
            dominant
                .as_deref()
                .is_some_and(|d| noise::INDIRECT_ROWS.contains(&d)),
        ),
        peak_mem_bytes: peak_mem,
        machine_mem_bytes: crate::machine_memory_bytes(),
    };
    let static_findings = checks::run(&facts);

    // -- Crops ---------------------------------------------------------------
    let mut maps = crops::CropMaps {
        width: w,
        height: h,
        rel_var: vec![0.0; w * h],
        time: vec![0.0; w * h],
        lum: vec![0.0; w * h],
    };
    for yi in 0..h {
        for x in 0..w {
            let q = frame_rect.index(x, h - 1 - yi);
            let lum = p1_lum[q];
            maps.lum[yi * w + x] = lum;
            maps.rel_var[yi * w + x] = p1.var_map[q] / (lum * lum).max(1e-4);
        }
    }
    for (tile, t) in &p1.tiles {
        let img = tile.flip_y(h);
        let per = t / img.area().max(1) as f64;
        for y in img.y0..img.y1 {
            for x in img.x0..img.x1 {
                maps.time[y * w + x] = per;
            }
        }
    }
    let side = crops::crop_side(threads);
    if let Some(r) = region
        && r.area() < side * side
    {
        warn!(
            "--region {r} is smaller than {side}x{side}: with {threads} threads it measures the \
             thread pool as much as the scene"
        );
    }
    let picked = crops::pick(&maps, side, region);
    let mut crops_out: Vec<Crop> = picked
        .iter()
        .enumerate()
        .map(|(k, p)| Crop {
            id: if p.reason == "region" {
                "region".into()
            } else {
                format!("crop_{}", (b'a' + k as u8) as char)
            },
            rect: [p.rect.x0, p.rect.y0, p.rect.x1, p.rect.y1],
            reason: p.reason.into(),
            relative_variance: p.rel_var.into(),
            baseline_thread_s: p.time_s.into(),
            reference_mrse: None,
        })
        .collect();
    let crop_settings = |base: RenderSettings, k: usize, spp: u32| {
        probe(base, spp)
            .with_region(picked[k].rect)
            .expect("a crop is inside the frame")
    };
    info!(
        "diagnostic crops: {}",
        crops_out
            .iter()
            .map(|c| format!("{} {:?} ({})", c.id, c.rect, c.reason))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // -- Tier 1 --------------------------------------------------------------
    let tier1_start = Instant::now();
    sched.open_tiers();
    let mut planned: Vec<Change> = Vec::new();
    debug!(
        "diagnostic: tier 1 ordered by {:?} (dominant row {:?}, {light_count} lights)",
        noise::holding(dominant.as_deref(), light_count),
        dominant
    );
    for factor in noise::factor_order(dominant.as_deref(), light_count) {
        match changes(factor, full, light_count) {
            Ok(c) => planned.extend(c),
            Err(why) => not_tried.push(NotTried {
                id: factor.into(),
                tier: 1,
                reason: "not_applicable".into(),
                detail: Some(why),
            }),
        }
    }
    let crop_pixels: usize = picked.iter().map(|p| p.rect.area()).sum();
    let per_px_spp = p1.render_s / (pixels * spp_p1 as f64);
    // The combined trial counts as one more.
    let spp_t = schedule::trial_spp(
        per_px_spp,
        crop_pixels,
        repeats,
        planned.len() + 1,
        sched.tier_left_s(1),
    );
    let mut setup_by_factor: Vec<(&'static str, f64)> = Vec::new();
    let mut runs: Vec<Running> = Vec::new();
    let mut tier1_complete = exceeded.is_none();
    let measure = |renderer: &mut Renderer,
                   sched: &mut schedule::Schedule,
                   setup_by_factor: &mut Vec<(&'static str, f64)>,
                   changes: Vec<Change>,
                   id: String,
                   factor: String,
                   value: String|
     -> Option<Running> {
        let setup = changes
            .iter()
            .filter_map(|c| {
                setup_by_factor
                    .iter()
                    .find(|(f, _)| *f == c.factor)
                    .map(|(_, s)| *s)
            })
            .sum::<f64>();
        let estimate = schedule::trial_cost_s(
            per_px_spp,
            crop_pixels,
            spp_t,
            repeats,
            setup * picked.len() as f64 * repeats as f64,
        );
        sched.set_spent(elapsed());
        if !sched.fits(1, estimate) {
            debug!(
                "diagnostic: {id} not started: needs ~{estimate:.2}s, tier 1 has {:.2}s",
                sched.tier_left_s(1)
            );
            return None;
        }
        let mut per_crop = Vec::new();
        let mut setup_seen = 0.0f64;
        for k in 0..picked.len() {
            let base = crop_settings(full, k, spp_t);
            let trial = changes.iter().fold(base, |s, c| (c.apply)(s, &c.value));
            let mut pairs = Vec::new();
            for _ in 0..repeats {
                let (b, _) = shoot(renderer, base);
                let (t, _) = shoot(renderer, trial);
                setup_seen = setup_seen.max(t.setup_s);
                pairs.push((b, t));
            }
            per_crop.push(CropRun { pairs });
        }
        for c in &changes {
            setup_by_factor.retain(|(f, _)| *f != c.factor);
            setup_by_factor.push((c.factor, setup_seen));
        }
        sched.set_spent(elapsed());
        debug!("diagnostic: {id} measured on {} crop(s)", per_crop.len());
        Some(Running {
            changes,
            id,
            factor,
            value,
            per_crop,
        })
    };
    if exceeded.is_none() {
        for change in &planned {
            let id = change.id();
            match measure(
                &mut renderer,
                &mut sched,
                &mut setup_by_factor,
                vec![change.clone()],
                id.clone(),
                change.factor.into(),
                change.value.clone(),
            ) {
                Some(r) => runs.push(r),
                None => {
                    tier1_complete = false;
                    not_tried.push(NotTried {
                        id,
                        tier: 1,
                        reason: "budget".into(),
                        detail: None,
                    });
                }
            }
        }
    } else {
        for change in &planned {
            not_tried.push(NotTried {
                id: change.id(),
                tier: 1,
                reason: "budget".into(),
                detail: Some("the baseline used the whole budget".into()),
            });
        }
    }
    // The winners: the best `better` value of each factor, judged against
    // the references of the single trials.
    let refs = references(&runs, picked.len());
    let judged: Vec<Trial> = runs
        .iter()
        .map(|r| judge(r, &crops_out, &refs, spp_t))
        .collect();
    let mut winners: Vec<(usize, f64)> = Vec::new();
    for (i, t) in judged.iter().enumerate() {
        if t.verdict != Verdict::Better {
            continue;
        }
        let eff = t.overall_delta_eff.map_or(0.0, |n| n.0);
        match winners
            .iter_mut()
            .find(|(j, _)| judged[*j].factor == t.factor)
        {
            Some(w) if w.1 >= eff => {}
            Some(w) => *w = (i, eff),
            None => winners.push((i, eff)),
        }
    }
    let combined_id = "combined".to_string();
    if winners.len() >= 2 {
        let changes: Vec<Change> = winners
            .iter()
            .flat_map(|(i, _)| runs[*i].changes.clone())
            .collect();
        let value = winners
            .iter()
            .map(|(i, _)| runs[*i].id.clone())
            .collect::<Vec<_>>()
            .join(",");
        match measure(
            &mut renderer,
            &mut sched,
            &mut setup_by_factor,
            changes,
            combined_id.clone(),
            "combined".into(),
            value,
        ) {
            Some(r) => runs.push(r),
            None => {
                tier1_complete = false;
                not_tried.push(NotTried {
                    id: combined_id.clone(),
                    tier: 1,
                    reason: "budget".into(),
                    detail: None,
                });
            }
        }
    } else if exceeded.is_none() {
        not_tried.push(NotTried {
            id: combined_id.clone(),
            tier: 1,
            reason: "not_applicable".into(),
            detail: Some(format!(
                "{} factor(s) better; a combination needs two",
                winners.len()
            )),
        });
    }
    // Final: every trial against the references of all of tier 1.
    let refs = references(&runs, picked.len());
    for (c, r) in crops_out.iter_mut().zip(&refs) {
        if !r.var.is_empty() {
            c.reference_mrse = Some(trials::mrse(&r.var, &r.lum).into());
        }
    }
    let trials_out: Vec<Trial> = runs
        .iter()
        .map(|r| judge(r, &crops_out, &refs, spp_t))
        .collect();
    phases.push(PhaseRun {
        name: "tier 1".into(),
        time_s: secs(tier1_start.elapsed()),
        completed: tier1_complete,
    });
    if !tier1_complete && exceeded.is_none() {
        exceeded = Some("tier 1".into());
    }
    info!(
        "diagnostic tier 1: {} trial(s) on {} crop(s) at {spp_t} spp, {} better, in {:.2}s",
        trials_out.len(),
        crops_out.len(),
        trials_out
            .iter()
            .filter(|t| t.verdict == Verdict::Better)
            .count(),
        tier1_start.elapsed().as_secs_f64()
    );

    // The suggestion: the combined trial if it beats the best single one,
    // else the best single — when it clears the suggestion bar.
    let eff = |t: &Trial| t.overall_delta_eff.map_or(0.0, |n| n.0);
    let best_single = trials_out
        .iter()
        .filter(|t| t.id != combined_id && t.verdict == Verdict::Better)
        .max_by(|a, b| eff(a).total_cmp(&eff(b)));
    let combined = trials_out
        .iter()
        .find(|t| t.id == combined_id && t.verdict == Verdict::Better);
    let best: Option<&Trial> = match (combined, best_single) {
        (Some(c), Some(s)) if eff(c) > eff(s) => Some(c),
        (_, Some(s)) => Some(s),
        (c, None) => c,
    }
    .filter(|t| eff(t) >= trials::SUGGEST_ABOVE);
    let best_changes: Vec<Change> = best
        .and_then(|t| runs.iter().find(|r| r.id == t.id))
        .map(|r| r.changes.clone())
        .unwrap_or_default();
    let suggestions: Vec<Suggestion> = best_changes
        .iter()
        .map(|c| Suggestion {
            id: c.id(),
            flag: c.flag.map(Into::into),
            usd_attribute: Some(c.attribute.into()),
            value: c.value.clone(),
            expected_delta_eff: Num(best.map_or(f64::NAN, eff)),
            evidence: {
                let mut e = vec![best.map(|t| t.id.clone()).unwrap_or_default()];
                if e[0] != c.id() {
                    e.push(c.id());
                }
                e
            },
        })
        .collect();
    let best_settings = best_changes
        .iter()
        .fold(full, |s, c| (c.apply)(s, &c.value));

    // -- Tier 2: the sample budget --------------------------------------------
    let tier2_start = Instant::now();
    let threshold = authored.variance_threshold() as f64;
    let (target, target_from) = match options.target_mrse {
        Some(t) => (Some(t), "--target-mrse"),
        None if threshold > 0.0 => (Some(threshold * threshold), "variance_threshold"),
        None => (None, "variance_threshold"),
    };
    // MRSE and time of the best settings relative to the baseline, from the
    // trial that measured them (geometric means over the crops).
    let geo = |f: &dyn Fn(&CropTrial) -> f64| -> f64 {
        best.map_or(1.0, |t| {
            let v: Vec<f64> = t
                .per_crop
                .iter()
                .map(f)
                .filter(|x| x.is_finite() && *x > 0.0)
                .collect();
            if v.is_empty() {
                1.0
            } else {
                (v.iter().map(|x| x.ln()).sum::<f64>() / v.len() as f64).exp()
            }
        })
    };
    let mrse_ratio = geo(&|c| c.mrse_trial.0 / c.mrse_baseline.0);
    let time_ratio = geo(&|c| c.time_trial_s.0 / c.time_baseline_s.0);
    let spp_to_target = target
        .map(|t| spp_p1 as f64 * p1_mrse * mrse_ratio / t)
        .filter(|s| s.is_finite());
    let projected = spp_to_target.map(|s| p1.render_s / spp_p1 as f64 * s * time_ratio);
    let mut adaptive = Vec::new();
    if threshold > 0.0 && exceeded.is_none() {
        // A budget the adaptive test can act on: well past its minimum.
        let min_spp = authored.min_samples_per_pixel().max(2);
        let spp_a = (4 * min_spp).clamp(64, 1024);
        for (k, p) in picked.iter().enumerate() {
            let fixed = crop_settings(best_settings, k, spp_a);
            let estimate = per_px_spp * p.rect.area() as f64 * spp_a as f64 * time_ratio;
            sched.set_spent(elapsed());
            if !sched.fits(2, estimate) {
                not_tried.push(NotTried {
                    id: format!("adaptive@{}", crops_out[k].id),
                    tier: 2,
                    reason: "budget".into(),
                    detail: None,
                });
                continue;
            }
            let on =
                fixed.with_adaptive_sampling(authored.min_samples_per_pixel(), threshold as f32);
            let (shot, m) = shoot(&mut renderer, on);
            // The fixed side from what the tier-1 renders of this crop cost
            // per sample, at the best settings.
            let per_spp = runs
                .iter()
                .find(|r| best.is_some_and(|b| b.id == r.id))
                .map(|r| {
                    trials::summary(
                        &r.per_crop[k]
                            .pairs
                            .iter()
                            .map(|(_, t)| t.render_s)
                            .collect::<Vec<_>>(),
                    )
                    .0
                })
                .or_else(|| {
                    runs.first().map(|r| {
                        trials::summary(
                            &r.per_crop[k]
                                .pairs
                                .iter()
                                .map(|(b, _)| b.render_s)
                                .collect::<Vec<_>>(),
                        )
                        .0
                    })
                })
                .map(|s| s / spp_t as f64)
                .unwrap_or(per_px_spp * p.rect.area() as f64);
            let fixed_s = per_spp * spp_a as f64;
            let px = m.rays.adaptive_pixels.max(1) as f64;
            adaptive.push(AdaptiveCrop {
                crop: crops_out[k].id.clone(),
                variance_threshold: threshold.into(),
                spp: spp_a,
                mean_spp: (m.rays.adaptive_samples as f64 / px).into(),
                early_stopped_share: (m.rays.early_stopped as f64 / px).into(),
                time_adaptive_s: shot.render_s.into(),
                time_fixed_estimate_s: fixed_s.into(),
                time_saved_share: (1.0 - shot.render_s / fixed_s).into(),
            });
        }
    } else if threshold <= 0.0 {
        not_tried.push(NotTried {
            id: "adaptive".into(),
            tier: 2,
            reason: "not_applicable".into(),
            detail: Some("the scene's variance threshold is 0: adaptive sampling is off".into()),
        });
    }
    let sample_budget = target.map(|t| SampleBudget {
        target_mrse: t.into(),
        target_from: target_from.into(),
        settings: best.map_or_else(|| "baseline".into(), |t| t.id.clone()),
        estimate: true,
        spp_to_target: spp_to_target.map(Num),
        projected_render_s: projected.map(Num),
        adaptive,
    });
    phases.push(PhaseRun {
        name: "tier 2".into(),
        time_s: secs(tier2_start.elapsed()),
        completed: !not_tried
            .iter()
            .any(|n| n.tier == 2 && n.reason == "budget"),
    });
    info!(
        "diagnostic tier 2: target MRSE {}, estimated {} spp, in {:.2}s",
        target.map_or_else(|| "none".into(), |t| format!("{t:.4}")),
        spp_to_target.map_or_else(|| "–".into(), |s| format!("{s:.0}")),
        tier2_start.elapsed().as_secs_f64()
    );

    // -- Tier 3: picture-changing settings -------------------------------------
    let tier3_start = Instant::now();
    let beauty_total: f64 = p1_lum.iter().sum();
    let clamp = authored.indirect_clamp().map(|limit| ClampResult {
        limit: (limit as f64).into(),
        removed_luminance_share: (p1.clamp.removed_luminance / beauty_total).into(),
        mean_removed_luminance: (p1.clamp.removed_luminance / pixels).into(),
        pixels_affected_share: (p1.clamp.pixels_touched as f64 / p1.clamp.pixels.max(1) as f64)
            .into(),
    });
    let max_depth = authored.max_depth();
    let mut half_depth = None;
    if max_depth >= 2 && !picked.is_empty() && exceeded.is_none() {
        let k = 0;
        let full_s = crop_settings(full, k, spp_t);
        let half_s = full_s.with_max_depth(max_depth / 2);
        let estimate =
            2.0 * repeats as f64 * per_px_spp * picked[k].rect.area() as f64 * spp_t as f64;
        sched.set_spent(elapsed());
        if sched.fits(3, estimate) {
            let (mut tf, mut th) = (Vec::new(), Vec::new());
            let (mut lf, mut lh) = (0.0, 0.0);
            for _ in 0..repeats {
                let (f, _) = shoot(&mut renderer, full_s);
                let (hh, _) = shoot(&mut renderer, half_s);
                tf.push(f.render_s);
                th.push(hh.render_s);
                lf = f.image.lum.iter().sum::<f64>();
                lh = hh.image.lum.iter().sum::<f64>();
            }
            let (mf, mh) = (trials::summary(&tf).0, trials::summary(&th).0);
            half_depth = Some(HalfDepth {
                crop: crops_out[k].id.clone(),
                depth: max_depth / 2,
                time_saved_share: (1.0 - mh / mf).into(),
                mean_luminance_change: (lh / lf - 1.0).into(),
            });
        } else {
            not_tried.push(NotTried {
                id: "max_depth_half".into(),
                tier: 3,
                reason: "budget".into(),
                detail: None,
            });
        }
    }
    let picture_changing = PictureChanging {
        clamp,
        max_depth: DepthResult {
            max_depth,
            ended_by_depth_share: baseline.ended_by_depth_share,
            half_depth,
        },
        subdivision: SubdivisionResult {
            levels: import_stats.subdivision.levels.clone(),
            triangles: import_stats.scene.unique.triangles as u64,
            mem_bytes: import_stats.scene.footprint.total() as u64,
            build_s: None,
        },
    };
    phases.push(PhaseRun {
        name: "tier 3".into(),
        time_s: secs(tier3_start.elapsed()),
        completed: !not_tried
            .iter()
            .any(|n| n.tier == 3 && n.reason == "budget"),
    });
    info!(
        "diagnostic tier 3: clamp {}, {:.1}% of paths end at max depth, in {:.2}s",
        picture_changing.clamp.as_ref().map_or_else(
            || "off".into(),
            |c| format!("removes {:.2}%", 100.0 * c.removed_luminance_share.0)
        ),
        100.0 * baseline.ended_by_depth_share.0,
        tier3_start.elapsed().as_secs_f64()
    );

    // -- Report ---------------------------------------------------------------
    let converged = converged(&trials_out, &static_findings);
    let effective_settings = effective(&authored, options);
    let suggested_command = command(options, &best_settings, &suggestions, region);
    let exit = if tier1_complete { 0 } else { 3 };
    let mut report = Report {
        format: FORMAT.into(),
        crust_version: env!("CARGO_PKG_VERSION").into(),
        scene: SceneInfo {
            path: options.scene_path.clone(),
            frame: options.frame,
            camera: options.camera.clone(),
            resolution: [w, h],
            region: region.map(|r| [r.x0, r.y0, r.x1, r.y1]),
        },
        effective_settings,
        run: RunInfo {
            budget_s: budget_s.into(),
            used_s: elapsed().into(),
            import_s: secs(options.import),
            threads,
            repeats,
            probe_conditions: ProbeConditions {
                indirect_clamp: "off".into(),
                adaptive_sampling: "off".into(),
                fixed_spp: true,
                resolution: [w, h],
            },
            phases,
            budget_exceeded_in: exceeded,
            exit,
        },
        static_findings,
        baseline,
        noise_breakdown,
        crops: crops_out,
        trials: trials_out,
        sample_budget,
        picture_changing,
        not_tried,
        suggestions,
        converged,
        suggested_command,
        deltas: None,
    };
    if let Some(prev) = &options.previous {
        report.deltas = Some(compare::deltas(prev, &report));
    }
    report
}

/// Converged (D11): no tier-1 trial, the combined one included, is
/// `better` by at least [`trials::SUGGEST_ABOVE`], and no `time` or
/// `noise` finding has an action left to take.
fn converged(trials: &[Trial], findings: &[Finding]) -> bool {
    !trials.iter().any(|t| {
        t.verdict == Verdict::Better
            && t.overall_delta_eff
                .is_some_and(|e| e.0 >= trials::SUGGEST_ABOVE)
    }) && !findings.iter().any(|f| {
        matches!(f.kind, FindingKind::Time | FindingKind::Noise) && f.action.is_actionable()
    })
}

/// Every setting the diagnosis ran with, by the names that set it.
fn effective(s: &RenderSettings, options: &Options) -> Vec<Setting> {
    let row = |name: &str, value: String, flag: Option<&str>, attr: Option<&str>| Setting {
        name: name.into(),
        value,
        flag: flag.map(Into::into),
        usd_attribute: attr.map(Into::into),
    };
    vec![
        row(
            "samples_per_pixel",
            s.samples_per_pixel().to_string(),
            Some("-s"),
            Some("crust:samplesPerPixel"),
        ),
        row(
            "max_depth",
            s.max_depth().to_string(),
            None,
            Some("crust:maxDepth"),
        ),
        row(
            "sampling_strategy",
            s.sampling_strategy().to_string(),
            Some("--strategy"),
            Some("crust:samplingStrategy"),
        ),
        row(
            "light_selection",
            s.light_selection().to_string(),
            Some("--light-selection"),
            Some("crust:lightSelection"),
        ),
        row(
            "light_samples",
            s.light_samples().to_string(),
            Some("--light-samples"),
            Some("crust:lightSamples"),
        ),
        row(
            "light_samples_indirect",
            s.light_samples_indirect().to_string(),
            Some("--light-samples-indirect"),
            Some("crust:lightSamplesIndirect"),
        ),
        row(
            "indirect_clamp",
            s.indirect_clamp()
                .map_or_else(|| "0".into(), |c| c.to_string()),
            Some("--indirect-clamp"),
            Some("crust:indirectClamp"),
        ),
        row(
            "pixel_filter",
            s.pixel_filter().name().into(),
            Some("--filter"),
            Some("crust:pixelFilter"),
        ),
        row(
            "pixel_filter_radius",
            s.pixel_filter().radius().to_string(),
            Some("--filter-radius"),
            Some("crust:pixelFilterRadius"),
        ),
        row(
            "path_guiding",
            s.guiding().to_string(),
            None,
            Some("crust:pathGuiding"),
        ),
        row(
            "variance_threshold",
            s.variance_threshold().to_string(),
            None,
            Some("crust:varianceThreshold"),
        ),
        row(
            "min_samples_per_pixel",
            s.min_samples_per_pixel().to_string(),
            None,
            Some("crust:minSamplesPerPixel"),
        ),
        row(
            "subdivision_level",
            options
                .subdivision_level
                .map_or_else(|| "stage".into(), |l| l.to_string()),
            Some("--subdiv-level"),
            Some("crust:subdivisionLevel"),
        ),
        row(
            "subdivision_edge_length",
            options
                .subdivision_edge_length
                .map_or_else(|| "stage".into(), |l| l.to_string()),
            Some("--subdiv-edge-length"),
            Some("crust:subdivisionEdgeLength"),
        ),
        row(
            "auto_tx",
            options.auto_tx.to_string(),
            Some("--auto-tx"),
            None,
        ),
    ]
}

/// A `crust render` command line for the same scene with every suggestion
/// applied. A suggestion with no flag can only be authored on the stage; it
/// is named in a trailing shell comment.
fn command(
    options: &Options,
    s: &RenderSettings,
    suggestions: &[Suggestion],
    region: Option<PixelRect>,
) -> String {
    let quote = |v: &str| {
        if v.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-,:".contains(c))
        {
            v.to_owned()
        } else {
            format!("'{}'", v.replace('\'', r"'\''"))
        }
    };
    let mut parts = vec![
        "crust".to_owned(),
        "render".into(),
        "-i".into(),
        quote(&options.scene_path),
    ];
    if let Some(f) = options.frame {
        parts.extend(["-f".into(), f.to_string()]);
    }
    if let Some(c) = &options.camera {
        parts.extend(["--camera".into(), quote(c)]);
    }
    if let Some(r) = options.region.and(region) {
        parts.extend([
            "--region".into(),
            format!("{},{},{},{}", r.x0, r.y0, r.x1, r.y1),
        ]);
    }
    parts.extend([
        "--strategy".into(),
        s.sampling_strategy().to_string(),
        "--light-selection".into(),
        s.light_selection().to_string(),
        "--light-samples".into(),
        s.light_samples().to_string(),
        "--light-samples-indirect".into(),
        s.light_samples_indirect().to_string(),
    ]);
    if let Some(l) = options.subdivision_level {
        parts.extend(["--subdiv-level".into(), l.to_string()]);
    }
    if let Some(l) = options.subdivision_edge_length {
        parts.extend(["--subdiv-edge-length".into(), l.to_string()]);
    }
    if options.auto_tx {
        parts.push("--auto-tx".into());
    }
    let stage_only: Vec<String> = suggestions
        .iter()
        .filter(|s| s.flag.is_none())
        .filter_map(|s| {
            s.usd_attribute
                .as_ref()
                .map(|a| format!("{a} = {}", s.value))
        })
        .collect();
    let mut line = parts.join(" ");
    if !stage_only.is_empty() {
        line.push_str(&format!(
            "  # and author on the stage: {}",
            stage_only.join(", ")
        ));
    }
    line
}
