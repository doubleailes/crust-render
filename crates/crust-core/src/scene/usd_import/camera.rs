//! `UsdGeomCamera` → [`Camera`].

use glam::{Mat4 as GMat4, Vec3, Vec3A};
use openusd::sdf;
use openusd::usd::{Prim, Stage};
use openusd_schemas::geom::{Camera as UsdCamera, CameraSchema, StageMetadata, tokens};
use openusd_schemas::render::{AspectRatioConformPolicy, conform_aperture};
use tracing::debug;

use crate::camera::Camera;
use crate::tracer::RenderSettings;
use crate::warning;

use super::attrs::{attr_f32, attr_f64};
use super::prim_at;
use super::products::Framing;
use super::xform::compose_with_parent;

/// One `UsdGeomCamera` as all its readers see it: [`build_camera`], which
/// builds the render camera, [`screen_projection`], which adaptive
/// subdivision and on-screen implicit shapes read before the traversal, and
/// [`camera_lens`], which `crust ls camera --json` reports. All derive from
/// this one read — the conform included — so they cannot describe two
/// different cameras.
struct CameraFrame {
    /// The camera's position and its view direction and up, in world space.
    /// USD's camera looks down local −Z with +Y up.
    eye: Vec3,
    forward: Vec3,
    up: Vec3,
    /// The focal length and the apertures, in the same units.
    focal_length: f32,
    /// Horizontal, vertical, as authored; an unauthored vertical aperture is
    /// the horizontal one over the image's aspect ratio.
    aperture: [f32; 2],
    /// `aperture` conformed to the image's aspect by the render's
    /// `aspectRatioConformPolicy` ([`conform`]): the aperture the render
    /// frames.
    conformed: [f32; 2],
    /// The pixel aspect ratio `adjustPixelAspectRatio` asked for, when it is
    /// not 1: crust renders square pixels, so the frame was conformed as
    /// `expandAperture` instead.
    unhonoured_pixel_aspect: Option<f32>,
    /// The image size the aperture default and the field of view assume.
    width: f32,
    height: f32,
}

impl CameraFrame {
    fn read(
        cam: &UsdCamera,
        stage: &Stage,
        prim: &Prim,
        settings: &RenderSettings,
        framing: Framing,
    ) -> Self {
        let world = local_to_world(stage, prim);
        let focal_length = attr_f32(&cam.focal_length_attr()).unwrap_or(50.0);
        let horiz_aperture = attr_f32(&cam.horizontal_aperture_attr()).unwrap_or(20.955);
        let (w, h) = settings.get_dimensions();
        let (width, height) = (w as f32, h as f32);
        let authored_vertical = attr_f32(&cam.vertical_aperture_attr());
        let aperture = [
            horiz_aperture,
            authored_vertical.unwrap_or(horiz_aperture * height / width),
        ];
        // An unauthored vertical aperture already has the image's aspect.
        let (conformed, unhonoured_pixel_aspect) = if authored_vertical.is_some() {
            conform(framing.policy, (w, h), aperture)
        } else {
            (aperture, None)
        };
        CameraFrame {
            eye: world.transform_point3(Vec3::ZERO),
            forward: world.transform_vector3(Vec3::NEG_Z).normalize(),
            up: world.transform_vector3(Vec3::Y).normalize(),
            focal_length,
            aperture,
            conformed,
            unhonoured_pixel_aspect,
            width,
            height,
        }
    }

    /// The conformed vertical aperture, which the field of view is derived
    /// from.
    fn vert_aperture(&self) -> f32 {
        self.conformed[1]
    }

    /// Tangent of the half vertical field of view.
    fn tan_half_vfov(&self) -> f32 {
        self.vert_aperture() / (2.0 * self.focal_length)
    }

    fn aspect(&self) -> f32 {
        self.width / self.height
    }
}

/// How far two aspect ratios may differ, relatively, and still be the same
/// shape: an aperture this close to the image's is not conformed, so a
/// rounding difference cannot move the field of view by an ulp.
const SAME_ASPECT: f32 = 1e-6;

/// `aperture` (horizontal, vertical) conformed to a `width` × `height` image
/// of square pixels by `policy`, as OpenUSD's `CameraUtilConformWindow`
/// conforms it, and the pixel aspect ratio `adjustPixelAspectRatio` would
/// have needed when it is not 1. That policy keeps the aperture and changes
/// the pixels instead; crust renders square pixels, so it is conformed as
/// `expandAperture`, the closest square-pixel frame that keeps the whole
/// aperture in view.
///
/// An aperture whose aspect already is the image's is returned unchanged, by
/// every policy. With square pixels the conformed aperture has the image's
/// aspect, which the ray generator assumes (`Camera::new` takes `W / H`).
fn conform(
    policy: AspectRatioConformPolicy,
    (width, height): (usize, usize),
    aperture: [f32; 2],
) -> ([f32; 2], Option<f32>) {
    let image_aspect = width as f32 / height as f32;
    let aperture_aspect = aperture[0] / aperture[1];
    if ((aperture_aspect / image_aspect) - 1.0).abs() <= SAME_ASPECT {
        return (aperture, None);
    }
    let (policy, unhonoured) = match policy {
        AspectRatioConformPolicy::AdjustPixelAspectRatio => (
            AspectRatioConformPolicy::ExpandAperture,
            Some(aperture_aspect / image_aspect),
        ),
        p => (p, None),
    };
    let resolution = [
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    ];
    let conformed = conform_aperture(policy, resolution, 1.0, aperture).aperture_size;
    // `conform_aperture` leaves a degenerate aperture alone; any other comes
    // out at the image's aspect, or the rays would stretch it.
    debug_assert!(
        !(aperture.iter().all(|a| a.is_finite() && *a > 0.0) && image_aspect.is_finite())
            || ((conformed[0] / conformed[1]) / image_aspect - 1.0).abs() <= 1e-5,
        "conformed aperture {conformed:?} is not at the image's aspect {image_aspect}"
    );
    (conformed, unhonoured)
}

/// A camera's lens as the render reads it, unauthored values at the render's
/// fallbacks — what [`build_camera`] builds from, and what `crust ls camera
/// --json` reports.
pub(super) struct CameraLens {
    pub(super) focal_length: f32,
    /// Horizontal, vertical, as authored; the vertical defaults to the
    /// horizontal over the image's aspect ratio.
    pub(super) aperture: [f32; 2],
    /// `aperture` conformed to the image by the render's
    /// `aspectRatioConformPolicy`: the aperture the render frames.
    pub(super) conformed_aperture: [f32; 2],
    /// `0` is a pinhole.
    pub(super) f_stop: f32,
    pub(super) focus_distance: f32,
}

/// The lens of the camera at `prim`, read as [`build_camera`] reads it.
pub(super) fn camera_lens(
    stage: &Stage,
    prim: &Prim,
    settings: &RenderSettings,
    framing: Framing,
) -> Option<CameraLens> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    Some(lens(
        &cam,
        &CameraFrame::read(&cam, stage, prim, settings, framing),
    ))
}

fn lens(cam: &UsdCamera, frame: &CameraFrame) -> CameraLens {
    CameraLens {
        focal_length: frame.focal_length,
        aperture: frame.aperture,
        conformed_aperture: frame.conformed,
        f_stop: attr_f32(&cam.f_stop_attr()).unwrap_or(0.0),
        focus_distance: attr_f32(&cam.focus_distance_attr()).unwrap_or(10.0),
    }
}

/// A camera as the import builds it: the rays it generates, and its linear
/// exposure scale as authored. The scale is checked only once this camera is
/// the one rendered through and its exposure applies, so a fallback candidate
/// or a disabled exposure raises no warning; so is the stage unit its lens
/// refused ([`RenderCamera::warn_lens_units`]).
pub(super) struct RenderCamera {
    pub(super) camera: Camera,
    pub(super) exposure_scale: f32,
    /// The stage's `metersPerUnit` when the lens read it and refused it (not
    /// finite, not positive, or overflowing the diameter); the lens was built
    /// with the fallback.
    pub(super) refused_meters_per_unit: Option<f64>,
    /// Its shutter, for the check that the one motion was measured over is
    /// this camera's ([`Shutter`]).
    pub(super) shutter: ShutterOffsets,
    /// The pixel aspect ratio `adjustPixelAspectRatio` asked for and the
    /// frame did not honour ([`CameraFrame::unhonoured_pixel_aspect`]).
    pub(super) unhonoured_pixel_aspect: Option<f32>,
}

impl RenderCamera {
    /// Raises `camera.invalid_meters_per_unit` for the camera rendered
    /// through, when its lens refused the stage's unit.
    pub(super) fn warn_lens_units(&self, camera_path: Option<&str>) {
        if let Some(m) = self.refused_meters_per_unit {
            let camera = camera_path.unwrap_or("the render camera");
            warning!(
                CameraInvalidMetersPerUnit,
                "{camera}: the stage's metersPerUnit = {m} is not a length the lens can use; it \
                 reads it as {FALLBACK_METERS_PER_UNIT} (a centimetre)"
            );
        }
    }

    /// Raises `product.unhonoured_attribute` for the camera rendered
    /// through, when `adjustPixelAspectRatio` asked for non-square pixels
    /// and its frame was conformed as `expandAperture` instead.
    pub(super) fn warn_framing(&self, camera_path: Option<&str>) {
        if let Some(pixel_aspect) = self.unhonoured_pixel_aspect {
            let camera = camera_path.unwrap_or("the render camera");
            warning!(
                ProductUnhonouredAttribute,
                at = camera,
                "{camera}: aspectRatioConformPolicy = adjustPixelAspectRatio needs \
                 pixelAspectRatio = {pixel_aspect}; rendering square pixels, conformed as \
                 expandAperture"
            );
        }
    }
}

/// The camera at `prim` and its exposure scale, `None` when `prim` is not a
/// camera on `stage`.
pub(super) fn build_camera(
    stage: &Stage,
    prim: &Prim,
    settings: &RenderSettings,
    framing: Framing,
) -> Option<RenderCamera> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    let frame = CameraFrame::read(&cam, stage, prim, settings, framing);
    let (camera, refused_meters_per_unit) = lens_camera(&cam, stage, &frame, settings);
    Some(RenderCamera {
        camera,
        exposure_scale: exposure_scale(&cam),
        refused_meters_per_unit,
        shutter: offsets(&cam),
        unhonoured_pixel_aspect: frame.unhonoured_pixel_aspect,
    })
}

/// A camera's `shutter:open` / `shutter:close` as authored: offsets in time
/// codes from the frame, each at USD's fallback 0 when unauthored.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ShutterOffsets {
    pub(super) open: f64,
    pub(super) close: f64,
}

impl ShutterOffsets {
    /// Whether the offsets describe an interval at all: `close > open`. A
    /// non-finite offset describes none.
    pub(super) fn opens(&self) -> bool {
        self.close > self.open
    }
}

fn offsets(cam: &UsdCamera) -> ShutterOffsets {
    ShutterOffsets {
        open: attr_f64(&cam.shutter_open_attr()).unwrap_or(0.0),
        close: attr_f64(&cam.shutter_close_attr()).unwrap_or(0.0),
    }
}

/// The shutter of the camera at `prim`, `None` when `prim` is not a camera
/// on `stage`.
pub(super) fn shutter_offsets(stage: &Stage, prim: &Prim) -> Option<ShutterOffsets> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    Some(offsets(&cam))
}

/// The interval motion is measured over, read before the traversal from the
/// camera the render is expected to go through: the frame `f` plus the
/// camera's offsets, `f + open .. f + close`. One interval for the beauty's
/// blur and the `motionvector` AOV, so the two cannot disagree.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Shutter {
    /// The offsets as authored on `camera`.
    pub(super) offsets: ShutterOffsets,
    /// The camera they were read from; `None` when the stage has none the
    /// read could find, which reads as the fallback 0 / 0.
    pub(super) camera: Option<sdf::Path>,
    /// Shutter open and shutter close in time codes; `None` when the shutter
    /// is instantaneous: the offsets open no interval, or there is no frame
    /// to measure from.
    pub(super) interval: Option<(f64, f64)>,
}

impl Shutter {
    pub(super) fn new(
        camera: Option<sdf::Path>,
        offsets: ShutterOffsets,
        frame: Option<f64>,
    ) -> Self {
        let interval = frame
            .filter(|_| offsets.opens())
            .map(|f| (f + offsets.open, f + offsets.close))
            .filter(|(open, close)| close > open);
        Shutter {
            offsets,
            camera,
            interval,
        }
    }

    /// What the MCP session compares to decide whether a settings edit
    /// changed the shutter ([`crate::live::ShutterKey`]); `query` is whether
    /// the import asks prims about time ([`super::xform::TransformMotion`]),
    /// `frame_pose` whether moving placements are posed at the frame.
    pub(super) fn key(&self, query: bool, frame_pose: bool) -> crate::live::ShutterKey {
        crate::live::ShutterKey {
            camera: self.camera.as_ref().map(ToString::to_string),
            offsets: [self.offsets.open.to_bits(), self.offsets.close.to_bits()],
            interval: self.interval.map(|(o, c)| [o.to_bits(), c.to_bits()]),
            query,
            frame_pose,
        }
    }

    /// Offsets that open an interval with no frame to place it at: time
    /// samples are not evaluated over it (`time.motion_needs_frame`).
    pub(super) fn needs_frame(&self) -> bool {
        self.offsets.opens() && self.interval.is_none()
    }
}

/// The linear exposure scale of `cam` at the import's time (C++
/// `UsdGeomCamera::ComputeLinearExposureScale`), computed as C++ computes it,
/// in single precision and in its order:
/// `time × iso × 2^exposure × responsivity / (100 × fStop × fStop)`. Each
/// unauthored attribute reads its schema fallback, which together give
/// exactly 1. `exposure:fStop` is the photometric f-stop; the lens's
/// depth-of-field `fStop` does not enter. Unchecked: a zero `exposure:fStop`
/// gives infinity, which the import refuses once the camera is chosen.
fn exposure_scale(cam: &UsdCamera) -> f32 {
    let time = attr_f32(&cam.exposure_time_attr()).unwrap_or(1.0);
    let iso = attr_f32(&cam.exposure_iso_attr()).unwrap_or(100.0);
    let f_stop = attr_f32(&cam.exposure_f_stop_attr()).unwrap_or(1.0);
    let responsivity = attr_f32(&cam.exposure_responsivity_attr()).unwrap_or(1.0);
    let exponent = attr_f32(&cam.exposure_attr()).unwrap_or(0.0);
    (time * iso * 2.0_f32.powf(exponent) * responsivity) / (100.0 * f_stop * f_stop)
}

/// USD's `metersPerUnit` fallback: a centimetre.
const FALLBACK_METERS_PER_UNIT: f64 = 0.01;

/// The stage's authored `metersPerUnit` (C++ `UsdGeomGetStageMetersPerUnit`,
/// which reads only the root and session layers), `None` when no layer
/// authors it. Unchecked: [`lens_diameter`] refuses a value that is not a
/// length.
fn authored_meters_per_unit(stage: &Stage) -> Option<f64> {
    if !stage
        .has_authored_stage_metadata(tokens::METERS_PER_UNIT)
        .unwrap_or(false)
    {
        return None;
    }
    stage.meters_per_unit().ok().flatten()
}

/// The lens diameter in scene units: `focalLength / fStop` millimetres,
/// converted through the stage's `metersPerUnit` (unauthored reads as
/// [`FALLBACK_METERS_PER_UNIT`]), and the unit when it was refused. `0` is a
/// pinhole: an `fStop` of 0 or less, or `disableDepthOfField`. Only a lens
/// reads the unit, so a pinhole render refuses none it does not use; a unit
/// that is not finite, not positive, or so small that the diameter overflows
/// `f32` is refused and reads as the fallback.
fn lens_diameter(
    stage: &Stage,
    focal_length: f32,
    f_stop: f32,
    depth_of_field: bool,
) -> (f32, Option<f64>) {
    if !depth_of_field || f_stop <= 0.0 {
        return (0.0, None);
    }
    let diameter = |meters_per_unit: f64| {
        (f64::from(focal_length) / f64::from(f_stop) * 0.001 / meters_per_unit) as f32
    };
    match authored_meters_per_unit(stage) {
        // A unit so small that the diameter overflows `f32` is no length
        // either: the lens would offset every ray to infinity.
        Some(m) if m.is_finite() && m > 0.0 && diameter(m).is_finite() => (diameter(m), None),
        Some(m) => (diameter(FALLBACK_METERS_PER_UNIT), Some(m)),
        None => (diameter(FALLBACK_METERS_PER_UNIT), None),
    }
}

/// The ray-generating camera of `frame`, from its transform and lens, and
/// the stage unit its lens refused.
fn lens_camera(
    cam: &UsdCamera,
    stage: &Stage,
    frame: &CameraFrame,
    settings: &RenderSettings,
) -> (Camera, Option<f64>) {
    let CameraLens {
        f_stop,
        focus_distance,
        ..
    } = lens(cam, frame);

    let vfov_deg = 2.0 * frame.tan_half_vfov().atan().to_degrees();
    let (aperture, refused_meters_per_unit) =
        lens_diameter(stage, frame.focal_length, f_stop, settings.depth_of_field());
    let aspect = frame.aspect();
    let lookfrom = Vec3A::from(frame.eye);
    let lookat = Vec3A::from(frame.eye + frame.forward * focus_distance);
    let vup = Vec3A::from(frame.up);

    debug!(
        "USD camera: lookfrom={:?} lookat={:?} vup={:?} vfov={} aspect={} lens diameter={} \
         (scene units) focus={}",
        lookfrom, lookat, vup, vfov_deg, aspect, aperture, focus_distance
    );

    let camera = Camera::new(
        lookfrom,
        lookat,
        vup,
        vfov_deg,
        aspect,
        aperture,
        focus_distance,
    );
    (camera, refused_meters_per_unit)
}

/// What adaptive subdivision needs of the render camera, read before the
/// traversal builds it: the position and the pixels per world unit at unit
/// distance, `image height / (2 tan(vfov / 2))` = `height · focal / aperture`,
/// the aperture conformed. The same [`CameraFrame`] as [`build_camera`]'s.
/// `None` when `prim` is not a camera on `stage`.
pub(super) fn screen_projection(
    stage: &Stage,
    prim: &Prim,
    settings: &RenderSettings,
    framing: Framing,
) -> Option<ScreenProjection> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    let frame = CameraFrame::read(&cam, stage, prim, settings, framing);
    let f_px = frame.height * frame.focal_length / frame.vert_aperture();
    // The view pyramid, as `build_camera` builds it: the vertical field of
    // view from the conformed lens, the horizontal one from the image's
    // aspect ratio — which the conformed aperture has.
    let tan_v = frame.tan_half_vfov();
    (f_px.is_finite() && f_px > 0.0).then_some(ScreenProjection {
        eye: frame.eye,
        f_px,
        forward: frame.forward,
        up: frame.up,
        tan_v,
        tan_h: tan_v * frame.width / frame.height,
    })
}

/// The render camera as adaptive subdivision needs it, read before traversal.
pub(super) struct ScreenProjection {
    pub(super) eye: Vec3,
    /// Pixels per world unit at unit distance.
    pub(super) f_px: f32,
    /// The view direction and the image's up, in world space.
    pub(super) forward: Vec3,
    pub(super) up: Vec3,
    /// Tangents of the half fields of view, vertical and horizontal.
    pub(super) tan_v: f32,
    pub(super) tan_h: f32,
}

/// Composed local-to-world by walking the prim path upwards. Slower than
/// tracking it during DFS, but exact and only used at build_camera time.
pub(super) fn local_to_world(stage: &Stage, prim: &Prim) -> GMat4 {
    let mut ancestors: Vec<Prim> = Vec::new();
    let mut cur_path = prim.path().clone();
    ancestors.push(prim_at(stage, cur_path.clone()));
    while let Some(parent) = cur_path.parent() {
        cur_path = parent;
        ancestors.push(prim_at(stage, cur_path.clone()));
        if cur_path.as_str() == "/" {
            break;
        }
    }
    ancestors.reverse();
    ancestors
        .iter()
        .fold(GMat4::IDENTITY, |acc, p| compose_with_parent(p, acc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::usd_import::settings::CameraChoice;
    use crate::scene::usd_import::{read_camera_ahead, stage_builder};
    use openusd::usd::InitialLoadSet;

    /// A stage on a `.usda` holding `metadata` and nothing else.
    fn stage(name: &str, metadata: &str) -> Stage {
        let dir = std::env::temp_dir().join("crust_camera_units");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(&path, format!("#usda 1.0\n(\n{metadata}\n)\n")).expect("write stage");
        super::super::stage_builder()
            .open(path.to_str().unwrap())
            .expect("stage opens")
    }

    /// The authored unit, and `None` (not the schema's fallback) when no
    /// layer authors one.
    #[test]
    fn meters_per_unit_reads_what_the_stage_authors() {
        let metre = stage("metre", "    metersPerUnit = 1");
        assert_eq!(authored_meters_per_unit(&metre), Some(1.0));
        let unauthored = stage("unauthored", "    upAxis = \"Y\"");
        assert_eq!(authored_meters_per_unit(&unauthored), None);
    }

    fn write(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("crust_shutter_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(&path, text).expect("write stage");
        path
    }

    /// The shutter read ahead of the traversal off `stage`'s index (payloads
    /// unloaded), through the camera at `named` when one is.
    fn shutter(name: &str, stage: &str, named: Option<&str>, frame: Option<f64>) -> Shutter {
        let path = write(name, stage);
        let path_str = path.to_str().unwrap();
        let index = stage_builder()
            .load(InitialLoadSet::LoadNone)
            .open(path_str)
            .expect("stage opens");
        let named = named.map(|p| CameraChoice::Requested(sdf::path(p).unwrap()));
        read_camera_ahead(
            &index,
            &path,
            path_str,
            named.as_ref(),
            &RenderSettings::default(),
            Framing::default(),
            frame,
        )
        .expect("reads")
        .1
    }

    /// Every policy over a 36 × 24 aperture at 1:1, 2:1 and 1:2, against
    /// apertures worked out by hand from the USD documentation's table, not
    /// by calling `conform_aperture`.
    #[test]
    fn the_five_policies_conform_a_36_by_24_aperture() {
        use AspectRatioConformPolicy::*;
        let cases = [
            ((100, 100), ExpandAperture, [36.0, 36.0], None),
            ((100, 100), CropAperture, [24.0, 24.0], None),
            ((100, 100), AdjustApertureWidth, [24.0, 24.0], None),
            ((100, 100), AdjustApertureHeight, [36.0, 36.0], None),
            ((100, 100), AdjustPixelAspectRatio, [36.0, 36.0], Some(1.5)),
            ((200, 100), ExpandAperture, [48.0, 24.0], None),
            ((200, 100), CropAperture, [36.0, 18.0], None),
            ((200, 100), AdjustApertureWidth, [48.0, 24.0], None),
            ((200, 100), AdjustApertureHeight, [36.0, 18.0], None),
            ((200, 100), AdjustPixelAspectRatio, [48.0, 24.0], Some(0.75)),
            ((100, 200), ExpandAperture, [36.0, 72.0], None),
            ((100, 200), CropAperture, [12.0, 24.0], None),
            ((100, 200), AdjustApertureWidth, [12.0, 24.0], None),
            ((100, 200), AdjustApertureHeight, [36.0, 72.0], None),
            ((100, 200), AdjustPixelAspectRatio, [36.0, 72.0], Some(3.0)),
        ];
        for (resolution, policy, want, pixel_aspect) in cases {
            assert_eq!(
                conform(policy, resolution, [36.0, 24.0]),
                (want, pixel_aspect),
                "{policy:?} at {resolution:?}"
            );
        }
    }

    /// An aperture that already has the image's shape is left alone, bit
    /// for bit, by every policy, and asks for no other pixel shape.
    #[test]
    fn a_matching_aperture_is_not_conformed() {
        use AspectRatioConformPolicy::*;
        // 0.1 / (0.1 · 3 / 4) is not exactly 4 / 3 in f32.
        let near = [0.1_f32, 0.1 * 3.0 / 4.0];
        for policy in [
            ExpandAperture,
            CropAperture,
            AdjustApertureWidth,
            AdjustApertureHeight,
            AdjustPixelAspectRatio,
        ] {
            assert_eq!(
                conform(policy, (200, 100), [36.0, 18.0]),
                ([36.0, 18.0], None)
            );
            assert_eq!(conform(policy, (400, 300), near), (near, None));
        }
    }

    /// A stage with one camera `/Cam` authoring `aperture`.
    fn camera_stage(name: &str, aperture: &str) -> (Stage, Prim) {
        let path = write(
            name,
            &format!("#usda 1.0\ndef Camera \"Cam\"\n{{\n{aperture}\n}}\n"),
        );
        let stage = stage_builder()
            .open(path.to_str().unwrap())
            .expect("stage opens");
        let prim = prim_at(&stage, sdf::path("/Cam").unwrap());
        (stage, prim)
    }

    const APERTURE_36_24: &str =
        "    float horizontalAperture = 36\n    float verticalAperture = 24";

    fn framed(policy: AspectRatioConformPolicy) -> Framing {
        Framing { policy }
    }

    /// `adjustPixelAspectRatio` on a mismatched aperture renders the
    /// `expandAperture` camera and, once it is the camera rendered through,
    /// names the pixel aspect ratio it would have needed.
    #[test]
    fn adjust_pixel_aspect_ratio_renders_as_expand_aperture_and_warns() {
        use AspectRatioConformPolicy::*;
        let (stage, prim) = camera_stage("pixel_aspect", APERTURE_36_24);
        let settings = RenderSettings::default().with_resolution(200, 100);
        let par = build_camera(&stage, &prim, &settings, framed(AdjustPixelAspectRatio))
            .expect("a camera");
        let expand =
            build_camera(&stage, &prim, &settings, framed(ExpandAperture)).expect("a camera");
        assert_eq!(format!("{:?}", par.camera), format!("{:?}", expand.camera));
        assert_eq!(par.unhonoured_pixel_aspect, Some(0.75));
        assert_eq!(expand.unhonoured_pixel_aspect, None);
        let scope = crate::WarningScope::enter();
        par.warn_framing(Some("/Cam"));
        expand.warn_framing(Some("/Cam"));
        let warnings = scope.finish();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(
            warnings[0].code,
            crate::WarningCode::ProductUnhonouredAttribute
        );
        assert_eq!(warnings[0].count, 1);
        assert!(
            warnings[0].message.contains("pixelAspectRatio = 0.75"),
            "{}",
            warnings[0].message
        );

        // An aperture that already has the image's aspect needs no other
        // pixel shape.
        let (stage, prim) = camera_stage(
            "pixel_aspect_matching",
            "    float horizontalAperture = 36\n    float verticalAperture = 18",
        );
        let par = build_camera(&stage, &prim, &settings, framed(AdjustPixelAspectRatio))
            .expect("a camera");
        assert_eq!(par.unhonoured_pixel_aspect, None);
    }

    /// Every reader sees the conformed aperture: the lens `ls` reports, the
    /// projection adaptive subdivision reads and the rays agree.
    #[test]
    fn every_reader_reads_the_conformed_aperture() {
        use AspectRatioConformPolicy::*;
        let (stage, prim) = camera_stage("readers", APERTURE_36_24);
        let settings = RenderSettings::default().with_resolution(200, 100);
        let lens = camera_lens(&stage, &prim, &settings, framed(AdjustApertureHeight)).unwrap();
        assert_eq!(lens.aperture, [36.0, 24.0]);
        assert_eq!(lens.conformed_aperture, [36.0, 18.0]);
        let height =
            screen_projection(&stage, &prim, &settings, framed(AdjustApertureHeight)).unwrap();
        let expand = screen_projection(&stage, &prim, &settings, framed(ExpandAperture)).unwrap();
        // 18 mm of film over 100 pixels against 24 mm.
        assert_eq!(height.f_px, 100.0 * 50.0 / 18.0);
        assert_eq!(expand.f_px, 100.0 * 50.0 / 24.0);
        assert_eq!(height.tan_v, 18.0 / 100.0);
        assert_eq!(height.tan_h, height.tan_v * 2.0);

        // An unauthored vertical aperture follows the image: nothing to
        // conform, whatever the policy.
        let (stage, prim) = camera_stage("readers_unauthored", "    float horizontalAperture = 36");
        for policy in [CropAperture, AdjustApertureHeight, AdjustPixelAspectRatio] {
            let lens = camera_lens(&stage, &prim, &settings, framed(policy)).unwrap();
            assert_eq!(lens.aperture, [36.0, 18.0]);
            assert_eq!(lens.conformed_aperture, lens.aperture);
        }
    }

    fn one_camera(shutter: &str) -> String {
        format!(
            "#usda 1.0\ndef Xform \"World\"\n{{\n    def Camera \"Cam\"\n    {{\n{shutter}\n    }}\n}}\n"
        )
    }

    #[test]
    fn an_authored_shutter_is_measured_from_the_frame() {
        let s = shutter(
            "open_close",
            &one_camera("        double shutter:open = 0\n        double shutter:close = 0.5"),
            None,
            Some(5.0),
        );
        assert_eq!(s.interval, Some((5.0, 5.5)));
        assert_eq!(s.camera, Some(sdf::path("/World/Cam").unwrap()));
    }

    #[test]
    fn a_shutter_may_open_before_the_frame() {
        let s = shutter(
            "centred",
            &one_camera("        double shutter:open = -0.25\n        double shutter:close = 0.25"),
            None,
            Some(5.0),
        );
        assert_eq!(s.interval, Some((4.75, 5.25)));
    }

    #[test]
    fn an_unauthored_shutter_is_instantaneous() {
        let s = shutter("unauthored", &one_camera(""), None, Some(5.0));
        assert_eq!(s.offsets, ShutterOffsets::default());
        assert_eq!(s.interval, None);
        assert!(!s.needs_frame());
    }

    #[test]
    fn without_a_frame_the_shutter_is_instantaneous() {
        let s = shutter(
            "no_frame",
            &one_camera("        double shutter:close = 0.5"),
            None,
            None,
        );
        assert_eq!(s.interval, None);
        assert!(s.needs_frame());
    }

    #[test]
    fn a_named_camera_wins_over_an_earlier_one() {
        let stage = "#usda 1.0\ndef Xform \"World\"\n{\n    def Camera \"A\"\n    {\n        \
                     double shutter:close = 1\n    }\n    def Camera \"B\"\n    {\n        \
                     double shutter:close = 0.5\n    }\n}\n";
        let s = shutter("named", stage, Some("/World/A"), Some(2.0));
        assert_eq!(s.interval, Some((2.0, 3.0)));
        // Unnamed, the walk meets B first, as the traversal does (last child
        // first).
        let s = shutter("unnamed", stage, None, Some(2.0));
        assert_eq!(s.camera, Some(sdf::path("/World/B").unwrap()));
        assert_eq!(s.interval, Some((2.0, 2.5)));
    }

    #[test]
    fn a_named_camera_in_an_unloaded_payload_is_read_through_a_masked_stage() {
        write(
            "payload_camera_shot",
            "#usda 1.0\ndef Xform \"Shot\"\n{\n    def Camera \"Cam\"\n    {\n        \
             double shutter:open = -0.5\n        double shutter:close = 0.5\n    }\n}\n",
        );
        let stage = "#usda 1.0\ndef Xform \"World\"\n{\n    def Xform \"Shot\" (\n        \
                     prepend payload = @payload_camera_shot.usda@</Shot>\n    )\n    {\n    }\n}\n";
        let s = shutter("payload_camera", stage, Some("/World/Shot/Cam"), Some(10.0));
        assert_eq!(s.interval, Some((9.5, 10.5)));
        // Unnamed, the index walk cannot see it.
        let s = shutter("payload_camera_unnamed", stage, None, Some(10.0));
        assert_eq!((s.camera, s.interval), (None, None));
    }
}
