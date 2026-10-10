//! `UsdRenderSettings` and the `crust:*` render attributes → [`RenderSettings`], and
//! which camera the render was told to use.

use crate::warning;
use openusd::sdf;
use openusd::usd::{Prim, Stage};
use openusd_schemas::render::{Settings as UsdRenderSettings, SettingsBaseSchema};
use tracing::debug;

use crate::color::Space;
use crate::filter::PixelFilter;
use crate::light::LightSelection;
use crate::tracer::{RenderSettings, SamplingStrategy};

use super::attrs::{custom_bool, custom_f32, custom_i32, custom_token, value_at};
use super::prim_at;

const DEFAULT_GUIDING_TRAIN_ITERATIONS: u32 = 4;
const DEFAULT_GUIDING_PROB: f32 = 0.5;
/// Hydra's default for `domeLightCameraVisibility`: the camera sees domes.
const DEFAULT_DOME_LIGHT_CAMERA_VISIBILITY: bool = true;
/// Hydra's default for `enableExposureCompensation`: the camera's exposure
/// applies.
const DEFAULT_ENABLE_EXPOSURE_COMPENSATION: bool = true;

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

/// The camera a render is told to use: the host's `requested` path (the
/// CLI's `--camera`), else the first RenderProduct's camera (`product_camera`,
/// which is the settings' own when the product names none), else the stage's
/// `RenderSettings.camera`. Read off the payload-free `index`, before any
/// traversal meets a camera. The one precedence the import and `crust ls`
/// both follow.
pub(super) fn wanted_camera(
    requested: Option<sdf::Path>,
    product_camera: Option<sdf::Path>,
    index: &Stage,
) -> Option<CameraChoice> {
    match requested {
        Some(p) => Some(CameraChoice::Requested(p)),
        None => product_camera
            .or_else(|| render_settings_camera(index))
            .map(CameraChoice::Settings),
    }
}

/// Which camera a render goes through, once the walk is done.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CameraPick<'a> {
    /// The camera it was told to use.
    Wanted,
    /// The first camera the import's walk met: nothing was named, or the
    /// stage's `RenderSettings.camera` names a prim that is not a camera.
    First,
    /// The procedural fallback camera: the stage has none.
    Procedural,
    /// The host asked for a camera the stage does not have: an error.
    Missing(&'a sdf::Path),
}

/// The fallback rule of [`CameraChoice`]: `wanted_met` says whether the
/// wanted camera was met, `any_met` whether any camera was. Shared by the
/// import (`resolve_camera`) and the listing (`is_render_camera`), so the
/// camera `ls` marks is the one a render uses.
pub(super) fn pick_camera(
    wanted: Option<&CameraChoice>,
    wanted_met: bool,
    any_met: bool,
) -> CameraPick<'_> {
    match wanted {
        Some(_) if wanted_met => CameraPick::Wanted,
        Some(CameraChoice::Requested(p)) => CameraPick::Missing(p),
        _ if any_met => CameraPick::First,
        _ => CameraPick::Procedural,
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
pub(super) fn render_settings_path(stage: &Stage) -> Option<sdf::Path> {
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

/// Whether the render camera's exposure scales the image: Hydra's
/// `enableExposureCompensation` render setting (the name hdEmbree reads), read
/// off the `RenderSettings` prim, with `crust:enableExposureCompensation`
/// winning when both are authored. Default `true`. `false` renders at a scale
/// of 1 whatever the camera authors.
pub(super) fn enable_exposure_compensation(stage: &Stage) -> bool {
    let Some(path) = render_settings_path(stage) else {
        return DEFAULT_ENABLE_EXPOSURE_COMPENSATION;
    };
    let prim = prim_at(stage, path);
    custom_bool(&prim, "crust:enableExposureCompensation")
        .or_else(|| custom_bool(&prim, "enableExposureCompensation"))
        .unwrap_or(DEFAULT_ENABLE_EXPOSURE_COMPENSATION)
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

/// The working colour space the stage asks for: `renderingColorSpace` on the
/// `RenderSettings` prim, resolved through the OCIO config ([`crate::color`]).
/// Unauthored or empty is `lin_rec709`; a name the config does not know, or a
/// space that is not scene-linear, is refused with a warning and is
/// `lin_rec709` too.
pub(super) fn render_settings_color_space(stage: &Stage) -> Space {
    let Some(path) = render_settings_path(stage) else {
        return Space::LIN_REC709;
    };
    match custom_token(&prim_at(stage, path.clone()), "renderingColorSpace") {
        Some(name) if !name.is_empty() => {
            crate::color::working_space(&name).unwrap_or_else(|why| {
                warning!(
                    ColorWorkingSpaceRefused,
                    at = path,
                    "{path}: renderingColorSpace refused ({why}); rendering in lin_rec709"
                );
                Space::LIN_REC709
            })
        }
        _ => Space::LIN_REC709,
    }
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

    let d = RenderSettings::default();
    let (mut w, mut h) = d.get_dimensions();
    if let Some(v2) = value_at(&s.resolution_attr()).and_then(|v| v.try_as_vec_2i()) {
        w = v2.x as usize;
        h = v2.y as usize;
    }

    // Custom `crust:*` attrs. We look them up on the RenderSettings prim.
    let prim = prim_at(stage, path);
    let spp =
        custom_i32(&prim, "crust:samplesPerPixel").map_or(d.samples_per_pixel(), |n| n as u32);
    let max_depth = custom_i32(&prim, "crust:maxDepth").map_or(d.max_depth(), |n| n as u32);
    // A negative minimum is refused rather than cast: `-1 as u32` is
    // `u32::MAX`, which would overflow the first check point.
    let min_spp = match custom_i32(&prim, "crust:minSamplesPerPixel") {
        Some(n) if n < 0 => {
            let fallback = d.min_samples_per_pixel();
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:minSamplesPerPixel = {n} is negative — using {fallback}"
            );
            fallback
        }
        Some(n) => n as u32,
        None => d.min_samples_per_pixel(),
    };
    let variance = custom_f32(&prim, "crust:varianceThreshold").unwrap_or(d.variance_threshold());
    let frame = custom_i32(&prim, "crust:frame").map_or(d.frame(), |n| n as isize);

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
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:samplingStrategy: {e} — using power MIS"
            );
            SamplingStrategy::PowerMis
        }),
    };

    // Light selection: `power` (default) | `uniform` | `learned`.
    let light_selection = match custom_token(&prim, "crust:lightSelection") {
        None => LightSelection::Power,
        Some(name) => name.parse().unwrap_or_else(|e| {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:lightSelection: {e} — picking lights by power"
            );
            LightSelection::Power
        }),
    };

    // Pixel reconstruction filter: `box` | `triangle` (default) | `gaussian`
    // | `blackman` | `mitchell`, each at its conventional radius unless
    // `crust:pixelFilterRadius` overrides it (in pixels, from the center).
    let mut filter = match custom_token(&prim, "crust:pixelFilter") {
        None => PixelFilter::default(),
        Some(name) => name.parse().unwrap_or_else(|e| {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:pixelFilter: {e} — using the triangle filter"
            );
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
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "crust:adaptiveNeighbourTolerance = {t} is not finite — using {}",
                crate::tracer::DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE
            );
            crate::tracer::DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE
        }
        None => crate::tracer::DEFAULT_ADAPTIVE_NEIGHBOUR_TOLERANCE,
    };

    // Light samples per vertex: at the camera vertex and at every later one.
    // Fewer than one is no estimator at all, so it is refused rather than
    // clamped without a word.
    let light_samples = light_sample_count(&prim, "crust:lightSamples");
    let light_samples_indirect = light_sample_count(&prim, "crust:lightSamplesIndirect");

    // What the stage asked for, before the CLI's own overrides. Every field
    // here silently falls back to a default when unauthored, so this is the
    // line that separates "the scene set it" from "nobody did".
    debug!(
        "RenderSettings at {}: {w}x{h}, {spp} spp (min {min_spp}, variance threshold \
         {variance}, neighbour tolerance {neighbour_tolerance}), max depth {max_depth}, \
         frame {frame}, strategy {strategy:?}, \
         light selection {light_selection:?}, light samples {light_samples} camera / \
         {light_samples_indirect} indirect, filter {} radius {}, indirect clamp {}, guiding {}",
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
    d.with_resolution(w, h)
        .with_max_depth(max_depth)
        .with_adaptive_sampling(min_spp, variance)
        .with_frame(frame)
        .with_samples_per_pixel(spp)
        .with_guiding(guiding, guiding_iters, guiding_prob)
        .with_sampling_strategy(strategy)
        .with_light_selection(light_selection)
        .with_pixel_filter(filter)
        .with_indirect_clamp(indirect_clamp)
        .with_adaptive_neighbour_tolerance(neighbour_tolerance)
        .with_light_samples(light_samples, light_samples_indirect)
}

/// A per-vertex light sample count off the `RenderSettings` prim: the
/// authored value when it is at least 1, clamped to `MAX_LIGHT_SAMPLES` with
/// a warning above it (a count multiplies every vertex's shadow rays, so a
/// typo there is a render that never ends), else the default with a warning.
/// Unauthored is the default.
fn light_sample_count(prim: &Prim, name: &str) -> u32 {
    use crate::tracer::{DEFAULT_LIGHT_SAMPLES, MAX_LIGHT_SAMPLES};
    match custom_i32(prim, name) {
        Some(n) if n >= 1 && n as u32 > MAX_LIGHT_SAMPLES => {
            warning!(
                SettingsLightSamplesClamped,
                at = prim.path(),
                "{name} = {n} is above {MAX_LIGHT_SAMPLES} — taking {MAX_LIGHT_SAMPLES}"
            );
            MAX_LIGHT_SAMPLES
        }
        Some(n) if n >= 1 => n as u32,
        Some(n) => {
            warning!(
                SettingsInvalidValue,
                at = prim.path(),
                "{name} = {n} is below 1 — taking {DEFAULT_LIGHT_SAMPLES}"
            );
            DEFAULT_LIGHT_SAMPLES
        }
        None => DEFAULT_LIGHT_SAMPLES,
    }
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
        warning!(
            TimeOutsideRange,
            "Frame {time} is outside the stage's time range [{start}, {end}]; \
             animated attributes hold their nearest time sample"
        );
    } else {
        debug!("Rendering at time code {time} of [{start}, {end}]");
    }
}

fn default_settings() -> RenderSettings {
    RenderSettings::default()
}
