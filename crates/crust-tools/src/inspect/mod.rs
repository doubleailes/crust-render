//! The light path inspector (`openspec/specs/light-path-inspection`): which
//! individual light paths make a few pixels bright — each path's expression,
//! light, contribution and sample.
//!
//! [`run`] re-renders a region of at most [`MAX_PIXELS`] pixels with the
//! render's own settings, through the AOV instantiation of the integrator
//! with the inspection's routing ([`crate::tracer::RouteCtx::inspection`]).
//! Every camera sample's path is folded into **records**, one per term of
//! the beauty's recurrence that ends on emission (`Route::fold`), so a
//! sample's records sum to its radiance and nothing is attributed twice.
//! Each work unit keeps the brightest records and one aggregate per group
//! key (`tracer/inspection.rs`); the units merge in unit order after the
//! render, and the report is built once from the merge.
//!
//! The engine writes nothing: [`run`] returns a [`Report`], which the host
//! prints ([`Report::to_markdown`]) and saves ([`Report::to_json`]).

mod report;

use std::collections::HashMap;
use std::sync::Arc;

use tracing::debug;

use crate::region::PixelRect;
use crate::tracer::NO_LIGHT;
use crate::tracer::inspection::{
    self as store, Collected, Config, MIXED_LIGHTS, group_key, is_light,
};
use crate::{Error, RenderSettings, Renderer};

// The limits and the refusal live in `error.rs`, beside the `Error` that
// carries the refusal; the grouping with the per-unit store the integrator
// fills (`tracer/inspection.rs`).
pub use crate::error::{MAX_PIXELS, MAX_TOP, PRIMS_NOTE, Refusal};
pub use crate::tracer::inspection::GroupBy;
pub use report::{
    AMBIGUOUS_LUMA_SHARE, CLAMP_HIDES_RATIO, DOMINANT_LUMA_SHARE, HINTS, HintRule, RARE_HIT_RATE,
    RARE_SECOND_MOMENT_SHARE,
};
pub use report::{
    Consistency, FORMAT, Group, Hint, LobeShare, Pixel, Record, Report, Settings, Truncated,
};

/// Records reported when no `--top` is given.
pub const DEFAULT_TOP: usize = 32;

/// A record whose dominant event carries less than this share of its
/// vertex's luminance, at any vertex, is `label_ambiguous` (design D2).
pub const AMBIGUOUS_SHARE: f32 = 0.6;

/// What the host knows that the renderer does not.
#[derive(Debug, Clone)]
pub struct Options {
    /// The records kept, brightest first.
    pub top: usize,
    pub group_by: GroupBy,
    /// The command that reproduces the inspection, in CLI syntax, as the
    /// host received it.
    pub command: Option<String>,
    /// The render camera's prim path.
    pub camera: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            top: DEFAULT_TOP,
            group_by: GroupBy::default(),
            command: None,
            camera: None,
        }
    }
}

/// Refuses what no inspection can run with, before anything is loaded: an
/// empty region, one over [`MAX_PIXELS`] pixels, more than [`MAX_TOP`]
/// records, and the grouping by prims.
pub fn check_request(region: PixelRect, group_by: GroupBy, top: usize) -> Result<(), Error> {
    let refuse = |r| Err(Error::Inspection(r));
    if region.is_empty() {
        return refuse(Refusal::EmptyRegion(region));
    }
    // Checked: a rectangle out to `usize::MAX` wraps `area()` to something
    // small, which the frame would then clip to the whole frame.
    let pixels = region.width().checked_mul(region.height());
    if pixels.is_none_or(|n| n > MAX_PIXELS) {
        return refuse(Refusal::RegionTooLarge { region, pixels });
    }
    if top > MAX_TOP {
        return refuse(Refusal::TooManyRecords(top));
    }
    if group_by == GroupBy::ExprPrimsLight {
        return refuse(Refusal::PrimsUnavailable);
    }
    Ok(())
}

/// Whether a region render with `settings` takes the same samples as the
/// full frame, and why not.
fn reproduces(settings: &RenderSettings) -> Result<(), String> {
    if settings.guiding() {
        return Err(
            "path guiding trains on the region's paths only, so the region's samples \
                    are not the frame's (they are still the scene's paths)"
                .into(),
        );
    }
    let adaptive = crate::tracer::samples_adaptively(
        settings.samples_per_pixel(),
        settings.min_samples_per_pixel(),
        settings.variance_threshold(),
    );
    if adaptive && settings.adaptive_neighbour_tolerance() >= 0.0 && !settings.is_full_frame() {
        return Err(
            "adaptive sampling holds a pixel back for its neighbours, and a pixel \
                    outside the region is never sampled, so a pixel near its edge can stop \
                    earlier than in the frame"
                .into(),
        );
    }
    Ok(())
}

/// Inspects `region` (image space) of a render of `renderer` with its own
/// settings: renders the region, keeps every camera sample's light paths,
/// and reports the brightest and their groups. The renderer's settings are
/// left as they were.
///
/// Refuses what [`check_request`] refuses, and a region with no pixel in
/// the frame ([`Error::EmptyRegion`]); a region partly outside is clipped
/// to it, as `--region` is.
pub fn run(renderer: &mut Renderer, region: PixelRect, options: &Options) -> Result<Report, Error> {
    check_request(region, options.group_by, options.top)?;
    let original = renderer.settings;
    let (w, h) = original.get_dimensions();
    let settings = original.with_region(region)?;
    // Interned tags: lights sharing one share an `expr` group, as they share
    // its `<L.'tag'>`. A tag a label cannot hold is no tag.
    let lights = &renderer.lights;
    let mut tag_ids: HashMap<&str, u32> = HashMap::new();
    let tags: Arc<[u32]> = (0..lights.count())
        .map(|i| match lights.lpe_tag(i).filter(|t| !t.contains('\'')) {
            Some(t) => {
                let next = tag_ids.len() as u32 + 1;
                *tag_ids.entry(t).or_insert(next)
            }
            None => 0,
        })
        .collect();
    let cfg = Arc::new(Config {
        top: options.top,
        group_by: options.group_by,
        rect: settings.raster_region(),
        height: h,
        tags,
        luma: lights.luma(),
        ambiguous_below: AMBIGUOUS_SHARE,
    });
    let started = std::time::Instant::now();
    renderer.retune(settings);
    let measured = renderer.render_inspection(cfg.clone());
    renderer.retune(original);
    let collected = measured.inspection.unwrap_or_default();
    debug!(
        "light paths: {} records from {} samples over {} in {:.2?}",
        collected.records,
        collected.samples,
        settings.region(),
        started.elapsed()
    );
    let reproduction = reproduces(&settings);
    Ok(build(
        renderer,
        &settings,
        (w, h),
        &cfg,
        &measured.buffer,
        collected,
        reproduction,
        options,
    ))
}

/// What a light index names in a report: the light's prim path, `None` for
/// none (or one the importer gave no name).
fn light_name(renderer: &Renderer, light: u32) -> Option<String> {
    is_light(light)
        .then(|| renderer.lights.name(light as usize))
        .flatten()
        .map(str::to_owned)
}

/// The writable LPE tag of light `light`, if any.
fn light_tag(renderer: &Renderer, light: u32) -> Option<&str> {
    if light == NO_LIGHT || light == MIXED_LIGHTS {
        return None;
    }
    renderer
        .lights
        .lpe_tag(light as usize)
        .filter(|t| !t.contains('\''))
}

/// `C` and `events` as an expression, the final `L` labelled with
/// `light`'s tag.
fn expression(renderer: &Renderer, events: &[u16], light: u32) -> String {
    let tag = light_tag(renderer, light);
    let mut out = String::from("C");
    for (k, &e) in events.iter().enumerate() {
        let last = k + 1 == events.len();
        out.push_str(&crate::lpe::inspection_text(
            e,
            if last { tag } else { None },
        ));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn build(
    renderer: &Renderer,
    settings: &RenderSettings,
    (w, h): (usize, usize),
    cfg: &Config,
    buffer: &crate::Buffer,
    collected: Collected,
    reproduction: Result<(), String>,
    options: &Options,
) -> Report {
    let exposure = settings.exposure_scale() as f64;
    let rect = cfg.rect;
    let luma = cfg.luma;
    // The region's totals through the beauty's own estimator, per pixel.
    let per_pixel = |f: &dyn Fn(&store::PixelAcc) -> f64| -> f64 {
        collected
            .pixels
            .iter()
            .filter(|p| p.taken > 0)
            .map(|p| f(p) / p.divisor())
            .sum()
    };
    let total = per_pixel(&|p| p.luma);
    let total_clamped = per_pixel(&|p| p.luma_clamped);
    let ambiguous = per_pixel(&|p| p.luma_ambiguous);
    let share = |x: f64| if total > 0.0 { x / total } else { f64::NAN };
    let second_total: f64 = collected.groups.iter().map(|g| g.second_moment).sum();
    let samples = collected.samples;

    // Groups, best first: the second moment, then the luminance, then the
    // key, so ties order the same every run.
    let mut groups: Vec<(usize, Group)> = collected
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let mut sum = (0.0f64, 0.0f64);
            for &(p, l, c) in &g.pixels {
                let d = collected.pixels[p as usize].divisor();
                sum.0 += l / d;
                sum.1 += c / d;
            }
            let expression = (cfg.group_by != GroupBy::Light)
                .then(|| expression(renderer, &g.events, g.first_light));
            let light = light_name(renderer, g.light);
            let key = match (cfg.group_by, &expression) {
                (GroupBy::Light, _) => match g.light {
                    l if is_light(l) => light.clone().unwrap_or_else(|| format!("light #{l}")),
                    _ => {
                        if crate::lpe::inspection_event(g.events.last().copied().unwrap_or(0))
                            .is_some_and(|e| e.0 == crate::lpe::EventType::Light)
                        {
                            "backdrop".into()
                        } else {
                            "O".into()
                        }
                    }
                },
                (GroupBy::Expr, Some(e)) => e.clone(),
                (_, Some(e)) => match (&light, is_light(g.light)) {
                    (Some(l), _) => format!("{e} via {l}"),
                    (None, true) => format!("{e} via light #{}", g.light),
                    (None, false) => e.clone(),
                },
                (_, None) => String::new(),
            };
            (
                i,
                Group {
                    key,
                    expression,
                    light,
                    luma_share: share(sum.0),
                    luma_share_clamped: share(sum.1),
                    second_moment_share: if second_total > 0.0 {
                        g.second_moment / second_total
                    } else {
                        f64::NAN
                    },
                    hit_rate: if samples > 0 {
                        g.hits as f64 / samples as f64
                    } else {
                        f64::NAN
                    },
                    max_luma: (g.max_luma as f64 * exposure) as f32,
                    records: g.records,
                    top_records: Vec::new(),
                },
            )
        })
        .collect();
    groups.sort_by(|(_, a), (_, b)| {
        b.second_moment_share
            .total_cmp(&a.second_moment_share)
            .then(b.luma_share.total_cmp(&a.luma_share))
            .then_with(|| a.key.cmp(&b.key))
    });
    // Where each merged group landed in that order.
    let mut position = vec![0usize; groups.len()];
    for (at, (i, _)) in groups.iter().enumerate() {
        position[*i] = at;
    }
    let mut groups: Vec<Group> = groups.into_iter().map(|(_, g)| g).collect();

    let camera = options.camera.clone();
    let mut key = Vec::new();
    let records: Vec<Record> = collected
        .kept
        .iter()
        .enumerate()
        .map(|(id, k)| {
            let group = (cfg.group_by != GroupBy::None).then(|| {
                group_key(cfg, &k.events, k.light, &mut key);
                collected.keys.get(&key[..]).map(|&g| position[g as usize])
            });
            let group = group.flatten();
            if let Some(g) = group {
                groups[g].top_records.push(id);
            }
            let scale = |v: glam::Vec3A| (v * exposure as f32).to_array();
            Record {
                id,
                luma: (k.rank.luma as f64 * exposure) as f32,
                contribution: scale(k.value),
                contribution_clamped: scale(k.clamped),
                expression: expression(renderer, &k.events, k.light),
                lobe_split: k
                    .splits
                    .iter()
                    .map(|split| {
                        split
                            .iter()
                            .map(|&(e, share)| LobeShare {
                                event: crate::lpe::inspection_text(e, None),
                                share,
                            })
                            .collect()
                    })
                    .collect(),
                label_ambiguous: k.min_share < cfg.ambiguous_below,
                light: light_name(renderer, k.light),
                prims: None,
                pixel: [k.rank.x as usize, k.rank.y as usize],
                coordinates: k.coordinates,
                sample: k.rank.sample,
                depth: k.splits.len(),
                camera: camera.clone(),
                group,
            }
        })
        .collect();

    // Each pixel, top-down as the image reads.
    let mut pixels = Vec::with_capacity(rect.area());
    let mut max_pixel_error = 0.0f32;
    let image = settings.region();
    for y in image.y0..image.y1 {
        for x in image.x0..image.x1 {
            let j = h - 1 - y;
            let acc = collected.pixels[rect.index(x, j)];
            let beauty = buffer.get_pixel(x, j);
            if acc.taken > 0 {
                let error = (acc.beauty - acc.records).abs().max_element();
                let relative = if error == 0.0 {
                    0.0
                } else if acc.scale > 0.0 && !error.is_nan() {
                    error / acc.scale
                } else {
                    f32::INFINITY
                };
                max_pixel_error = max_pixel_error.max(relative);
            }
            pixels.push(Pixel {
                pixel: [x, y],
                beauty: beauty.to_array(),
                luma: luma.of(beauty),
                samples: acc.taken,
            });
        }
    }

    let (reproduces_frame, reproduces_frame_reason) = match reproduction {
        Ok(()) => (true, None),
        Err(reason) => (false, Some(reason)),
    };
    let ambiguous_luma_share = share(ambiguous);
    let hints = report::hints(&groups, ambiguous_luma_share);
    let truncated = Truncated {
        kept: records.len() as u64,
        of: collected.records,
    };
    let region = settings.region();
    let summary = report::summary(
        &groups,
        &records,
        samples,
        collected.records,
        reproduces_frame_reason.as_deref(),
    );
    Report {
        format: FORMAT.into(),
        crust_version: crate::report::CRUST_VERSION.into(),
        summary,
        command: options.command.clone(),
        region: [region.x0, region.y0, region.x1, region.y1],
        resolution: [w, h],
        camera,
        settings: Settings::of(settings),
        reproduces_frame,
        reproduces_frame_reason,
        samples,
        consistency: Consistency {
            max_relative_error: max_pixel_error,
            max_sample_relative_error: collected.max_sample_error,
        },
        clamp_removed_share: if total > 0.0 {
            1.0 - total_clamped / total
        } else {
            f64::NAN
        },
        ambiguous_luma_share,
        group_by: cfg.group_by.to_string(),
        groups,
        records,
        truncated,
        pixels,
        prims_note: Some(PRIMS_NOTE.into()),
        hints,
    }
}
