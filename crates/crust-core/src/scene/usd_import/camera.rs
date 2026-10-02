//! `UsdGeomCamera` → [`Camera`].

use glam::{Mat4 as GMat4, Vec3, Vec3A};
use openusd::usd::{Prim, Stage};
use openusd_schemas::geom::Camera as UsdCamera;
use tracing::debug;

use crate::camera::Camera;
use crate::tracer::RenderSettings;

use super::attrs::attr_f32;
use super::prim_at;
use super::xform::{local_matrix_at, resets_xform_stack_at};

pub(super) fn build_camera(
    stage: &Stage,
    prim: &Prim,
    settings: &RenderSettings,
) -> Option<Camera> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    let world = local_to_world(stage, prim);

    // USD camera looks down -Z with +Y up in local space.
    let lookfrom_v = world.transform_point3(Vec3::ZERO);
    let forward_v = world.transform_vector3(Vec3::NEG_Z).normalize();
    let up_v = world.transform_vector3(Vec3::Y).normalize();

    let (focal_length, vert_aperture) = lens(&cam, settings);
    let f_stop = attr_f32(&cam.f_stop_attr()).unwrap_or(0.0);
    let focus_distance = attr_f32(&cam.focus_distance_attr()).unwrap_or(10.0);

    let (w, h) = settings.get_dimensions();
    let (w_f, h_f) = (w as f32, h as f32);

    let vfov_deg = 2.0 * (vert_aperture / (2.0 * focal_length)).atan().to_degrees();
    let aperture = if f_stop > 0.0 {
        focal_length / f_stop
    } else {
        0.0
    };

    let aspect = w_f / h_f;
    let lookfrom = Vec3A::new(lookfrom_v.x, lookfrom_v.y, lookfrom_v.z);
    let lookat_v = lookfrom_v + forward_v * focus_distance;
    let lookat = Vec3A::new(lookat_v.x, lookat_v.y, lookat_v.z);
    let vup = Vec3A::new(up_v.x, up_v.y, up_v.z);

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

/// The focal length and the vertical aperture, in the same units: the vertical
/// aperture defaults to the horizontal one over the image's aspect ratio.
fn lens(cam: &UsdCamera, settings: &RenderSettings) -> (f32, f32) {
    let focal_length = attr_f32(&cam.focal_length_attr()).unwrap_or(50.0);
    let horiz_aperture = attr_f32(&cam.horizontal_aperture_attr()).unwrap_or(20.955);
    let (w, h) = settings.get_dimensions();
    let vert_aperture =
        attr_f32(&cam.vertical_aperture_attr()).unwrap_or(horiz_aperture * h as f32 / w as f32);
    (focal_length, vert_aperture)
}

/// What adaptive subdivision needs of the render camera, read before the
/// traversal builds it: the position and the pixels per world unit at unit
/// distance, `image height / (2 tan(vfov / 2))` = `height · focal / aperture`.
/// From the same attributes and the same [`lens`] as [`build_camera`], so the
/// two describe one camera. `None` when `prim` is not a camera on `stage`.
pub(super) fn screen_projection(
    stage: &Stage,
    prim: &Prim,
    settings: &RenderSettings,
) -> Option<ScreenProjection> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    let world = local_to_world(stage, prim);
    let eye = world.transform_point3(Vec3::ZERO);
    let (focal_length, vert_aperture) = lens(&cam, settings);
    let (w, h) = settings.get_dimensions();
    let f_px = h as f32 * focal_length / vert_aperture;
    // The view pyramid, as `build_camera` builds it: the vertical field of
    // view from the lens, the horizontal one from the image's aspect ratio.
    let tan_v = vert_aperture / (2.0 * focal_length);
    let forward = world.transform_vector3(Vec3::NEG_Z).normalize();
    let up = world.transform_vector3(Vec3::Y).normalize();
    (f_px.is_finite() && f_px > 0.0).then_some(ScreenProjection {
        eye,
        f_px,
        forward,
        up,
        tan_v,
        tan_h: tan_v * w as f32 / h as f32,
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
    let mut acc = GMat4::IDENTITY;
    for p in &ancestors {
        let local = local_matrix_at(stage, p);
        let resets = resets_xform_stack_at(stage, p);
        acc = if resets { local } else { acc * local };
    }
    acc
}
