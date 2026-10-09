//! `UsdGeomCamera` → [`Camera`].

use glam::{Mat4 as GMat4, Vec3, Vec3A};
use openusd::usd::{Prim, Stage};
use openusd_schemas::geom::Camera as UsdCamera;
use tracing::debug;

use crate::camera::Camera;
use crate::tracer::RenderSettings;

use super::attrs::attr_f32;
use super::prim_at;
use super::xform::compose_with_parent;

/// One `UsdGeomCamera` as both its readers see it: [`build_camera`], which
/// builds the render camera, and [`screen_projection`], which adaptive
/// subdivision reads before the traversal. Both derive from this one read, so
/// they cannot describe two different cameras.
struct CameraFrame {
    /// The camera's position and its view direction and up, in world space.
    /// USD's camera looks down local −Z with +Y up.
    eye: Vec3,
    forward: Vec3,
    up: Vec3,
    /// The focal length and the vertical aperture, in the same units: the
    /// vertical aperture defaults to the horizontal one over the image's
    /// aspect ratio.
    focal_length: f32,
    horiz_aperture: f32,
    vert_aperture: f32,
    /// The image size the aperture default and the field of view assume.
    width: f32,
    height: f32,
}

impl CameraFrame {
    fn read(cam: &UsdCamera, stage: &Stage, prim: &Prim, settings: &RenderSettings) -> Self {
        let world = local_to_world(stage, prim);
        let focal_length = attr_f32(&cam.focal_length_attr()).unwrap_or(50.0);
        let horiz_aperture = attr_f32(&cam.horizontal_aperture_attr()).unwrap_or(20.955);
        let (w, h) = settings.get_dimensions();
        let (width, height) = (w as f32, h as f32);
        let vert_aperture =
            attr_f32(&cam.vertical_aperture_attr()).unwrap_or(horiz_aperture * height / width);
        CameraFrame {
            eye: world.transform_point3(Vec3::ZERO),
            forward: world.transform_vector3(Vec3::NEG_Z).normalize(),
            up: world.transform_vector3(Vec3::Y).normalize(),
            focal_length,
            horiz_aperture,
            vert_aperture,
            width,
            height,
        }
    }

    /// Tangent of the half vertical field of view.
    fn tan_half_vfov(&self) -> f32 {
        self.vert_aperture / (2.0 * self.focal_length)
    }

    fn aspect(&self) -> f32 {
        self.width / self.height
    }
}

/// A camera's lens as the render reads it, unauthored values at the render's
/// fallbacks — what [`build_camera`] builds from, and what `crust ls camera
/// --json` reports.
pub(super) struct CameraLens {
    pub(super) focal_length: f32,
    /// Horizontal, vertical; the vertical defaults to the horizontal over
    /// the image's aspect ratio.
    pub(super) aperture: [f32; 2],
    /// `0` is a pinhole.
    pub(super) f_stop: f32,
    pub(super) focus_distance: f32,
}

/// The lens of the camera at `prim`, read as [`build_camera`] reads it.
pub(super) fn camera_lens(
    stage: &Stage,
    prim: &Prim,
    settings: &RenderSettings,
) -> Option<CameraLens> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    Some(lens(&cam, &CameraFrame::read(&cam, stage, prim, settings)))
}

fn lens(cam: &UsdCamera, frame: &CameraFrame) -> CameraLens {
    CameraLens {
        focal_length: frame.focal_length,
        aperture: [frame.horiz_aperture, frame.vert_aperture],
        f_stop: attr_f32(&cam.f_stop_attr()).unwrap_or(0.0),
        focus_distance: attr_f32(&cam.focus_distance_attr()).unwrap_or(10.0),
    }
}

pub(super) fn build_camera(
    stage: &Stage,
    prim: &Prim,
    settings: &RenderSettings,
) -> Option<Camera> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    let frame = CameraFrame::read(&cam, stage, prim, settings);
    let CameraLens {
        f_stop,
        focus_distance,
        ..
    } = lens(&cam, &frame);

    let vfov_deg = 2.0 * frame.tan_half_vfov().atan().to_degrees();
    let aperture = if f_stop > 0.0 {
        frame.focal_length / f_stop
    } else {
        0.0
    };
    let aspect = frame.aspect();
    let lookfrom = Vec3A::from(frame.eye);
    let lookat = Vec3A::from(frame.eye + frame.forward * focus_distance);
    let vup = Vec3A::from(frame.up);

    debug!(
        "USD camera: lookfrom={:?} lookat={:?} vup={:?} vfov={} aspect={} aperture={} focus={}",
        lookfrom, lookat, vup, vfov_deg, aspect, aperture, focus_distance
    );

    Some(Camera::new(
        lookfrom,
        lookat,
        vup,
        vfov_deg,
        aspect,
        aperture,
        focus_distance,
    ))
}

/// What adaptive subdivision needs of the render camera, read before the
/// traversal builds it: the position and the pixels per world unit at unit
/// distance, `image height / (2 tan(vfov / 2))` = `height · focal / aperture`.
/// The same [`CameraFrame`] as [`build_camera`]'s. `None` when `prim` is not a
/// camera on `stage`.
pub(super) fn screen_projection(
    stage: &Stage,
    prim: &Prim,
    settings: &RenderSettings,
) -> Option<ScreenProjection> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    let frame = CameraFrame::read(&cam, stage, prim, settings);
    let f_px = frame.height * frame.focal_length / frame.vert_aperture;
    // The view pyramid, as `build_camera` builds it: the vertical field of
    // view from the lens, the horizontal one from the image's aspect ratio.
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
fn local_to_world(stage: &Stage, prim: &Prim) -> GMat4 {
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
