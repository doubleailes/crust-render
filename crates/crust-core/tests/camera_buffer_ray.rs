//! The small value types the renderer is built from: `Buffer`, `Camera`,
//! the renderer-side `Ray`, and `HitRecord`.

use crust_core::{
    Buffer, Camera, HitRecord, MASK_ALL, MASK_CAMERA, MASK_SHADOW, Medium, Ray, Vec3A,
};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Buffer
// ---------------------------------------------------------------------------

#[test]
fn buffer_starts_black() {
    let b = Buffer::new(4, 3);
    for y in 0..3 {
        for x in 0..4 {
            assert_eq!(b.get_pixel(x, y), Vec3A::ZERO);
        }
    }
}

#[test]
fn buffer_set_and_get_round_trip() {
    let mut b = Buffer::new(4, 3);
    b.set_pixel(2, 1, Vec3A::new(0.1, 0.2, 0.3));
    assert_eq!(b.get_pixel(2, 1), Vec3A::new(0.1, 0.2, 0.3));
    // Neighbours untouched.
    assert_eq!(b.get_pixel(1, 1), Vec3A::ZERO);
    assert_eq!(b.get_pixel(2, 0), Vec3A::ZERO);
    assert_eq!(b.get_pixel(3, 2), Vec3A::ZERO);
}

#[test]
fn buffer_ignores_out_of_bounds_writes_and_reads_black() {
    let mut b = Buffer::new(2, 2);
    b.set_pixel(2, 0, Vec3A::ONE);
    b.set_pixel(0, 2, Vec3A::ONE);
    b.set_pixel(99, 99, Vec3A::ONE);
    for y in 0..2 {
        for x in 0..2 {
            assert_eq!(b.get_pixel(x, y), Vec3A::ZERO);
        }
    }
    assert_eq!(b.get_pixel(2, 0), Vec3A::ZERO);
    assert_eq!(b.get_pixel(0, 5), Vec3A::ZERO);
}

#[test]
fn buffer_get_rgb_flips_the_y_axis() {
    let mut b = Buffer::new(2, 3);
    b.set_pixel(0, 0, Vec3A::new(1.0, 0.0, 0.0)); // bottom row in scene space
    b.set_pixel(1, 2, Vec3A::new(0.0, 0.0, 1.0)); // top row
    // Image row 0 is the top: it reads the pixel stored at y = height-1.
    assert_eq!(b.get_rgb(1, 0), (0.0, 0.0, 1.0));
    assert_eq!(b.get_rgb(0, 2), (1.0, 0.0, 0.0));
    assert_eq!(b.get_rgb(0, 1), (0.0, 0.0, 0.0));
}

#[test]
fn buffer_of_one_pixel_works() {
    let mut b = Buffer::new(1, 1);
    b.set_pixel(0, 0, Vec3A::splat(0.5));
    assert_eq!(b.get_rgb(0, 0), (0.5, 0.5, 0.5));
}

// ---------------------------------------------------------------------------
// Ray
// ---------------------------------------------------------------------------

#[test]
fn ray_defaults_to_vacuum_time_zero_and_all_masks() {
    let r = Ray::new(Vec3A::new(1.0, 2.0, 3.0), Vec3A::Z);
    assert!(r.medium().is_none());
    assert_eq!(r.time(), 0.0);
    assert_eq!(r.mask(), MASK_ALL);
    assert_eq!(r.origin(), Vec3A::new(1.0, 2.0, 3.0));
    assert_eq!(r.direction(), Vec3A::Z);
    let d = Ray::default();
    assert_eq!(d.origin(), Vec3A::ZERO);
    assert!(d.medium().is_none());
}

#[test]
fn ray_in_medium_carries_it() {
    let m = Arc::new(Medium::from_transmission(
        Vec3A::splat(0.5),
        1.0,
        Vec3A::ZERO,
        0.0,
    ));
    let r = Ray::new_in_medium(Vec3A::ZERO, Vec3A::X, Arc::clone(&m));
    let carried = r.medium().expect("medium kept");
    assert!(Arc::ptr_eq(carried, &m));
    // Cloning a ray shares the medium.
    let c = r.clone();
    assert!(Arc::ptr_eq(c.medium().unwrap(), &m));
}

#[test]
fn ray_builders_and_kernel_view_agree() {
    let r = Ray::new(Vec3A::Y, Vec3A::new(0.0, 0.0, 2.0))
        .with_time(0.75)
        .with_mask(MASK_SHADOW);
    assert_eq!(r.time(), 0.75);
    assert_eq!(r.mask(), MASK_SHADOW);
    assert_eq!(r.rt().origin, Vec3A::Y);
    assert_eq!(r.rt().dir, Vec3A::new(0.0, 0.0, 2.0));
    assert_eq!(r.rt().time, 0.75);
    assert_eq!(r.rt().mask, MASK_SHADOW);
    assert_eq!(r.at(0.5), Vec3A::new(0.0, 1.0, 1.0));
}

// ---------------------------------------------------------------------------
// HitRecord
// ---------------------------------------------------------------------------

#[test]
fn hit_record_defaults_to_no_face_and_no_uv() {
    let h = HitRecord::new();
    assert_eq!(h.face_id, HitRecord::NO_FACE);
    assert_eq!(HitRecord::NO_FACE, u32::MAX);
    assert!(!h.has_uv);
    assert_eq!(h.uv, (0.0, 0.0));
    assert_eq!(h.face_uv, (0.0, 0.0));
    assert_eq!(h.tangent, Vec3A::ZERO);
    assert!(!h.front_face);
    assert_eq!(h.t, 0.0);
    let d = HitRecord::default();
    assert_eq!(d.face_id, HitRecord::NO_FACE);
}

#[test]
fn set_face_normal_orients_against_the_ray() {
    let mut h = HitRecord::new();
    let outward = Vec3A::Z;
    h.set_face_normal(&Ray::new(Vec3A::ZERO, -Vec3A::Z), outward);
    assert!(h.front_face);
    assert_eq!(h.normal, Vec3A::Z);
    h.set_face_normal(&Ray::new(Vec3A::ZERO, Vec3A::Z), outward);
    assert!(!h.front_face);
    assert_eq!(h.normal, -Vec3A::Z);
    // Grazing: exactly perpendicular counts as back-facing (dot == 0).
    h.set_face_normal(&Ray::new(Vec3A::ZERO, Vec3A::X), outward);
    assert!(!h.front_face);
}

// ---------------------------------------------------------------------------
// Camera
// ---------------------------------------------------------------------------

fn looking_down_minus_z(aperture: f32) -> Camera {
    Camera::new(
        Vec3A::new(0.0, 0.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        90.0,
        1.0,
        aperture,
        1.0,
    )
}

#[test]
fn centre_ray_points_at_the_look_at() {
    let cam = looking_down_minus_z(0.0);
    let r = cam.get_ray(0.5, 0.5, [0.5, 0.5], 0.0);
    assert_eq!(r.origin(), Vec3A::new(0.0, 0.0, 5.0));
    assert!(r.direction().normalize().abs_diff_eq(-Vec3A::Z, 1e-5));
}

#[test]
fn camera_rays_carry_the_camera_mask_and_shutter_time() {
    let cam = looking_down_minus_z(0.0);
    let r = cam.get_ray(0.3, 0.7, [0.1, 0.9], 0.42);
    assert_eq!(r.mask(), MASK_CAMERA);
    assert!((r.time() - 0.42).abs() < 1e-7);
    assert!(r.medium().is_none());
}

#[test]
fn viewport_corners_follow_right_and_up() {
    let cam = looking_down_minus_z(0.0);
    // vfov 90° at aspect 1 gives a viewport spanning ±1 at focus distance 1.
    let corner = cam
        .get_ray(1.0, 1.0, [0.5, 0.5], 0.0)
        .direction()
        .normalize();
    assert!(
        corner.abs_diff_eq(Vec3A::new(1.0, 1.0, -1.0).normalize(), 1e-4),
        "{corner}"
    );
    let right = cam.get_ray(1.0, 0.5, [0.5, 0.5], 0.0).direction();
    assert!(right.x > 0.0 && right.y.abs() < 1e-5);
    let up = cam.get_ray(0.5, 1.0, [0.5, 0.5], 0.0).direction();
    assert!(up.y > 0.0 && up.x.abs() < 1e-5);
    let left_down = cam.get_ray(0.0, 0.0, [0.5, 0.5], 0.0).direction();
    assert!(left_down.x < 0.0 && left_down.y < 0.0);
}

#[test]
fn field_of_view_scales_the_viewport() {
    let narrow = Camera::new(
        Vec3A::new(0.0, 0.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        30.0,
        1.0,
        0.0,
        1.0,
    );
    let wide = looking_down_minus_z(0.0);
    let n = narrow
        .get_ray(1.0, 0.5, [0.5, 0.5], 0.0)
        .direction()
        .normalize();
    let w = wide
        .get_ray(1.0, 0.5, [0.5, 0.5], 0.0)
        .direction()
        .normalize();
    // Angle from the axis is half the fov (aspect 1): 15° vs 45°.
    let ang = |d: Vec3A| d.dot(-Vec3A::Z).clamp(-1.0, 1.0).acos().to_degrees();
    assert!((ang(n) - 15.0).abs() < 0.1, "{}", ang(n));
    assert!((ang(w) - 45.0).abs() < 0.1, "{}", ang(w));
}

#[test]
fn aspect_ratio_widens_the_horizontal_extent() {
    let cam = Camera::new(
        Vec3A::new(0.0, 0.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        90.0,
        2.0,
        0.0,
        1.0,
    );
    let right = cam.get_ray(1.0, 0.5, [0.5, 0.5], 0.0).direction();
    let up = cam.get_ray(0.5, 1.0, [0.5, 0.5], 0.0).direction();
    // At focus distance 1 the half-height is 1 and the half-width is 2.
    assert!((right.x - 2.0).abs() < 1e-4, "{right}");
    assert!((up.y - 1.0).abs() < 1e-4, "{up}");
}

#[test]
fn zero_aperture_ignores_the_lens_sample() {
    let cam = looking_down_minus_z(0.0);
    let a = cam.get_ray(0.5, 0.5, [0.0, 0.0], 0.0);
    let b = cam.get_ray(0.5, 0.5, [0.9, 0.2], 0.0);
    assert_eq!(a.origin(), b.origin());
    assert_eq!(a.direction(), b.direction());
}

#[test]
fn a_finite_aperture_offsets_the_origin_within_the_lens() {
    let aperture = 0.4;
    let cam = looking_down_minus_z(aperture);
    let centre = cam.get_ray(0.5, 0.5, [0.5, 0.5], 0.0);
    assert!(
        centre.origin().abs_diff_eq(Vec3A::new(0.0, 0.0, 5.0), 1e-6),
        "disk centre is no offset"
    );
    let edge = cam.get_ray(0.5, 0.5, [1.0, 0.5], 0.0);
    let offset = edge.origin() - Vec3A::new(0.0, 0.0, 5.0);
    assert!((offset.length() - aperture / 2.0).abs() < 1e-5, "{offset}");
    assert!(offset.z.abs() < 1e-6, "the lens lies in the camera plane");
}

#[test]
fn every_lens_sample_converges_on_the_focal_point() {
    let cam = Camera::new(
        Vec3A::new(0.0, 0.0, 5.0),
        Vec3A::ZERO,
        Vec3A::Y,
        60.0,
        1.5,
        0.5,
        3.0,
    );
    let focus = Vec3A::new(0.0, 0.0, 2.0); // 3 units along -Z
    for lens in [[0.0, 0.0], [1.0, 1.0], [0.2, 0.8], [0.5, 0.5], [0.9, 0.1]] {
        let r = cam.get_ray(0.5, 0.5, lens, 0.0);
        // The direction is scaled so t = 1 lands on the focus plane.
        assert!(
            r.at(1.0).abs_diff_eq(focus, 1e-4),
            "lens {lens:?}: {}",
            r.at(1.0)
        );
    }
}

#[test]
fn camera_respects_an_arbitrary_look_direction() {
    let from = Vec3A::new(3.0, 4.0, -2.0);
    let at = Vec3A::new(-1.0, 0.5, 6.0);
    let cam = Camera::new(from, at, Vec3A::Y, 45.0, 1.0, 0.0, 1.0);
    let r = cam.get_ray(0.5, 0.5, [0.5, 0.5], 0.0);
    assert_eq!(r.origin(), from);
    assert!(
        r.direction()
            .normalize()
            .abs_diff_eq((at - from).normalize(), 1e-4)
    );
    // "Up" on the image is not below the horizon.
    let up = cam
        .get_ray(0.5, 1.0, [0.5, 0.5], 0.0)
        .direction()
        .normalize();
    assert!(up.y > r.direction().normalize().y);
}
