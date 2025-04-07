use crate::ray::Ray;
use glam::Vec3;
use serde::{Deserialize, Serialize};
use utils::random_in_unit_disk;

/// The `Camera` struct represents a virtual camera in the ray tracing system.
/// It is responsible for generating rays that simulate the perspective view of a scene.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Camera {
    /// The origin of the camera (position in 3D space).
    origin: Vec3,
    /// The lower-left corner of the viewport.
    lower_left_corner: Vec3,
    /// The horizontal vector of the viewport.
    horizontal: Vec3,
    /// The vertical vector of the viewport.
    vertical: Vec3,
    /// The camera's local horizontal axis.
    u: Vec3,
    /// The camera's local vertical axis.
    v: Vec3,
    /// The radius of the camera's lens (used for depth of field).
    lens_radius: f32,
}

impl Camera {
    /// Creates a new `Camera` with the specified parameters.
    ///
    /// # Parameters
    /// - `lookfrom`: The position of the camera in 3D space.
    /// - `lookat`: The point in 3D space the camera is looking at.
    /// - `vup`: The "up" direction vector for the camera.
    /// - `vfov`: The vertical field-of-view in degrees.
    /// - `aspect_ratio`: The aspect ratio of the viewport (width/height).
    /// - `aperture`: The aperture size of the camera (controls depth of field).
    /// - `focus_dist`: The distance to the focus plane.
    ///
    /// # Returns
    /// - A new instance of `Camera`.
    pub fn new(
        lookfrom: Vec3,
        lookat: Vec3,
        vup: Vec3,
        vfov: f32, // Vertical field-of-view in degrees
        aspect_ratio: f32,
        aperture: f32,
        focus_dist: f32,
    ) -> Camera {
        let theta = utils::degrees_to_radians(vfov);
        let h = f32::tan(theta / 2.0);
        let viewport_height = 2.0 * h;
        let viewport_width = aspect_ratio * viewport_height;
        let w = (lookfrom - lookat).normalize();
        let u = vup.cross(w).normalize();
        let v = w.cross(u);

        let origin = lookfrom;
        let horizontal = focus_dist * viewport_width * u;
        let vertical = focus_dist * viewport_height * v;
        let lower_left_corner = origin - horizontal / 2.0 - vertical / 2.0 - focus_dist * w;

        let lens_radius = aperture / 2.0;

        Camera {
            origin,
            lower_left_corner,
            horizontal,
            vertical,
            u,
            v,
            lens_radius,
        }
    }

    /// Generates a ray originating from the camera through the viewport.
    ///
    /// # Parameters
    /// - `s`: The horizontal coordinate on the viewport (normalized to [0, 1]).
    /// - `t`: The vertical coordinate on the viewport (normalized to [0, 1]).
    ///
    /// # Returns
    /// - A `Ray` that starts at the camera and passes through the specified point on the viewport.
    pub fn get_ray(&self, s: f32, t: f32) -> Ray {
        let rd = self.lens_radius * random_in_unit_disk();
        let offset = self.u * rd.x + self.v * rd.y;
        Ray::new(
            self.origin + offset,
            self.lower_left_corner + s * self.horizontal + t * self.vertical - self.origin - offset,
        )
    }
}
