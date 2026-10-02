//! `UsdRenderSettings` and the `crust:*` render attributes → [`RenderSettings`], and
//! which camera the render was told to use.

use openusd::sdf;
use openusd::usd::Stage;
use openusd_schemas::render::{RenderSettings as UsdRenderSettings, RenderSettingsBase};
use tracing::{debug, warn};

use crate::filter::PixelFilter;
use crate::light::LightSelection;
use crate::tracer::{RenderSettings, SamplingStrategy};

use super::attrs::{custom_bool, custom_f32, custom_i32, custom_token};
use super::prim_at;
use super::time::eval_time;

const DEFAULT_SPP: u32 = 128;
const DEFAULT_MAX_DEPTH: u32 = 32;
const DEFAULT_WIDTH: usize = 640;
const DEFAULT_HEIGHT: usize = 360;
const DEFAULT_MIN_SPP: u32 = 32;
const DEFAULT_VARIANCE: f32 = 0.05;
const DEFAULT_FRAME: isize = 0;
const DEFAULT_GUIDING_TRAIN_ITERATIONS: u32 = 4;
const DEFAULT_GUIDING_PROB: f32 = 0.5;
/// Hydra's default for `domeLightCameraVisibility`: the camera sees domes.
const DEFAULT_DOME_LIGHT_CAMERA_VISIBILITY: bool = true;

/// A camera the render was told to use, and who said so — which decides what
/// happens if it is not on the stage.
pub(super) enum CameraChoice {
    /// The host's `UsdImportOptions::camera` (the CLI's `--camera`). Missing
    /// is an error.
    Requested(sdf::Path),
    /// The stage's `RenderSettings.camera` relationship. Missing warns and
    /// falls back to the first camera met.
    Settings(sdf::Path),
}

impl CameraChoice {
    pub(super) fn path(&self) -> &sdf::Path {
        match self {
            CameraChoice::Requested(p) | CameraChoice::Settings(p) => p,
        }
    }
}

impl std::fmt::Display for CameraChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CameraChoice::Requested(p) => write!(f, "camera {p} (requested)"),
            CameraChoice::Settings(p) => write!(f, "camera {p} (RenderSettings.camera)"),
        }
    }
}

/// The prim holding the stage's render settings: the one the stage's
/// `renderSettingsPrimPath` metadatum names, else the conventional
/// `/Render/settings`.
fn render_settings_path(stage: &Stage) -> Option<sdf::Path> {
    UsdRenderSettings::stage_settings_path(stage)
        .ok()
        .flatten()
        .or_else(|| sdf::path("/Render/settings").ok())
}

/// The first target of the render settings' `camera` relationship — the USD
/// way for a stage to say which of its cameras is the shot camera.
pub(super) fn render_settings_camera(stage: &Stage) -> Option<sdf::Path> {
    let s = UsdRenderSettings::get(stage, render_settings_path(stage)?)
        .ok()
        .flatten()?;
    s.camera_rel().targets().ok()?.into_iter().next()
}

/// Whether the camera sees the lights at infinity at all: Hydra's
/// `domeLightCameraVisibility` render setting (the name hdEmbree and usdview
/// use), read off the `RenderSettings` prim, with `crust:domeLightCameraVisibility`
/// winning when both are authored. Default `true`. `false` hides every dome,
/// distant light and backdrop from camera rays whatever the lights themselves
/// author; how they illuminate is unchanged.
pub(super) fn dome_light_camera_visibility(stage: &Stage) -> bool {
    let Some(path) = render_settings_path(stage) else {
        return DEFAULT_DOME_LIGHT_CAMERA_VISIBILITY;
    };
    let prim = prim_at(stage, path);
    custom_bool(&prim, "crust:domeLightCameraVisibility")
        .or_else(|| custom_bool(&prim, "domeLightCameraVisibility"))
        .unwrap_or(DEFAULT_DOME_LIGHT_CAMERA_VISIBILITY)
}

/// `crust:subdivisionLevel` as authored on the `RenderSettings` prim — the
/// stage's refinement level for every mesh whose `subdivisionScheme` is
/// not `none`. `None` when unauthored; the caller
/// ([`resolve_subdiv_level`](super::attrs::resolve_subdiv_level)) applies the
/// host override, the default and the clamp. A geometry setting, not a
/// tracer one, so it stays out of [`RenderSettings`].
pub(super) fn render_settings_subdiv_level(stage: &Stage) -> Option<i32> {
    let prim = prim_at(stage, render_settings_path(stage)?);
    custom_i32(&prim, "crust:subdivisionLevel")
}

/// `crust:subdivisionEdgeLength` as authored on the `RenderSettings` prim: the
/// stage's adaptive-subdivision target, in pixels. `None` when unauthored; the
/// caller ([`resolve_subdiv_edge_length`](super::attrs::resolve_subdiv_edge_length))
/// applies the host override and refuses a bad value. A geometry setting, like
/// [`render_settings_subdiv_level`].
pub(super) fn render_settings_subdiv_edge_length(stage: &Stage) -> Option<f32> {
    let prim = prim_at(stage, render_settings_path(stage)?);
    custom_f32(&prim, "crust:subdivisionEdgeLength")
}

pub(super) fn import_render_settings(stage: &Stage) -> RenderSettings {
    let Some(path) = render_settings_path(stage) else {
        return default_settings();
    };

    let s = match UsdRenderSettings::get(stage, path.clone()).ok().flatten() {
        Some(s) => s,
        None => {
            debug!(
                "No UsdRenderSettings at {} — using defaults for render settings",
                path
            );
            return default_settings();
        }
    };

    let (mut w, mut h) = (DEFAULT_WIDTH, DEFAULT_HEIGHT);
    if let Ok(Some(v)) = s.resolution_attr().get_at::<sdf::Value>(eval_time())
        && let Some(v2) = v.try_as_vec_2i()
    {
        w = v2.x as usize;
        h = v2.y as usize;
    }

    // Custom `crust:*` attrs. We look them up on the RenderSettings prim.
    let prim = prim_at(stage, path);
    let spp = custom_i32(&prim, "crust:samplesPerPixel").unwrap_or(DEFAULT_SPP as i32) as u32;
    let max_depth = custom_i32(&prim, "crust:maxDepth").unwrap_or(DEFAULT_MAX_DEPTH as i32) as u32;
    // A negative minimum is refused rather than cast: `-1 as u32` is
    // `u32::MAX`, which would overflow the first check point.
    let min_spp = match custom_i32(&prim, "crust:minSamplesPerPixel") {
        Some(n) if n < 0 => {
            warn!("crust:minSamplesPerPixel = {n} is negative — using {DEFAULT_MIN_SPP}");
            DEFAULT_MIN_SPP
        }
        Some(n) => n as u32,
        None => DEFAULT_MIN_SPP,
    };
    let variance = custom_f32(&prim, "crust:varianceThreshold").unwrap_or(DEFAULT_VARIANCE);
    let frame = custom_i32(&prim, "crust:frame").unwrap_or(DEFAULT_FRAME as i32) as isize;

    // Path guiding (opt-in).
    let guiding = custom_bool(&prim, "crust:pathGuiding").unwrap_or(false);
    let guiding_iters = custom_i32(&prim, "crust:guidingTrainIterations")
        .unwrap_or(DEFAULT_GUIDING_TRAIN_ITERATIONS as i32)
        .max(1) as u32;
    let guiding_prob = custom_f32(&prim, "crust:guidingProb").unwrap_or(DEFAULT_GUIDING_PROB);

    // MIS strategy: `power` (default) | `balance` | `light` | `bsdf`.
    let strategy = match custom_token(&prim, "crust:samplingStrategy") {
        None => SamplingStrategy::PowerMis,
        Some(name) => name.parse().unwrap_or_else(|e| {
            warn!("crust:samplingStrategy: {e} — using power MIS");
            SamplingStrategy::PowerMis
        }),
    };

    // Light selection: `power` (default) | `uniform` | `learned`.
    let light_selection = match custom_token(&prim, "crust:lightSelection") {
        None => LightSelection::Power,
        Some(name) => name.parse().unwrap_or_else(|e| {
            warn!("crust:lightSelection: {e} — picking lights by power");
            LightSelection::Power
        }),
    };

    // Pixel reconstruction filter: `box` | `triangle` (default) | `gaussian`
    // | `blackman` | `mitchell`, each at its conventional radius unless
    // `crust:pixelFilterRadius` overrides it (in pixels, from the center).
    let mut filter = match custom_token(&prim, "crust:pixelFilter") {
        None => PixelFilter::default(),
        Some(name) => name.parse().unwrap_or_else(|e| {
            warn!("crust:pixelFilter: {e} — using the triangle filter");
            PixelFilter::default()
        }),
    };
    if let Some(radius) = custom_f32(&prim, "crust:pixelFilterRadius") {
        filter = filter.with_radius(radius);
    }
    // Firefly clamp on indirect light, in the film's linear units (the
    // largest channel of one sample's indirect radiance). Unauthored takes
    // the engine default; an authored 0 leaves the estimator unbiased.
    let indirect_clamp =
        custom_f32(&prim, "crust:indirectClamp").unwrap_or(crate::tracer::DEFAULT_INDIRECT_CLAMP);
    // Adaptive sampling's cross-neighbour tolerance, in convergence-index
    // units; negative turns the comparison off. A non-finite value cannot
    // be compared against, so it is refused rather than silently disabling
    // the guard.
    let neighbour_tolerance = match custom_f32(&prim, "crust:adaptiveNeighbourTolerance") {
        Some(t) if t.is_finite() => t,
        Some(t) => {
            warn!(
                "crust:adaptiveNeighbourTolerance = {t} is not finite — using {}",
                crate::tracer::DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE
            );
            crate::tracer::DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE
        }
        None => crate::tracer::DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE,
    };

    // What the stage asked for, before the CLI's own overrides. Every field
    // here silently falls back to a default when unauthored, so this is the
    // line that separates "the scene set it" from "nobody did".
    debug!(
        "RenderSettings at {}: {w}x{h}, {spp} spp (min {min_spp}, variance threshold \
         {variance}, neighbour tolerance {neighbour_tolerance}), max depth {max_depth}, \
         frame {frame}, strategy {strategy:?}, \
         light selection {light_selection:?}, filter {} radius {}, indirect clamp {}, guiding {}",
        prim.path(),
        filter.name(),
        filter.radius(),
        if indirect_clamp > 0.0 {
            indirect_clamp.to_string()
        } else {
            "off".to_string()
        },
        if guiding {
            format!("on ({guiding_iters} training iterations, guide probability {guiding_prob})")
        } else {
            "off".to_string()
        }
    );
    RenderSettings::new(spp, max_depth, w, h, min_spp, variance, frame)
        .with_guiding(guiding, guiding_iters, guiding_prob)
        .with_sampling_strategy(strategy)
        .with_light_selection(light_selection)
        .with_pixel_filter(filter)
        .with_indirect_clamp(indirect_clamp)
        .with_adaptive_neighbour_tolerance(neighbour_tolerance)
}

/// Warns when `time` lies outside the stage's authored
/// `startTimeCode`..`endTimeCode`. Not an error — USD holds the first or last
/// sample past either end, so the render is well defined — but a frame off
/// the end of the shot is far more often a typo than a request, and it would
/// otherwise render a plausible, frozen image without a word.
pub(super) fn check_time_range(stage: &Stage, time: f64) {
    if !stage.has_authored_time_code_range() {
        debug!("Rendering at time code {time}; the stage authors no startTimeCode/endTimeCode");
        return;
    }
    let (start, end) = (stage.start_time_code(), stage.end_time_code());
    if time < start || time > end {
        warn!(
            "Frame {time} is outside the stage's time range [{start}, {end}]; \
             animated attributes hold their nearest time sample"
        );
    } else {
        debug!("Rendering at time code {time} of [{start}, {end}]");
    }
}

fn default_settings() -> RenderSettings {
    RenderSettings::new(
        DEFAULT_SPP,
        DEFAULT_MAX_DEPTH,
        DEFAULT_WIDTH,
        DEFAULT_HEIGHT,
        DEFAULT_MIN_SPP,
        DEFAULT_VARIANCE,
        DEFAULT_FRAME,
    )
}
