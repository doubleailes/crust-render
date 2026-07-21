use crate::ray::Ray;
use glam::Vec3A;
use utils::concentric_disk;

/// The `Camera` struct represents a virtual camera in the ray tracing system.
/// It is responsible for generating rays that simulate the perspective view of a scene.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// The origin of the camera (position in 3D space).
    origin: Vec3A,
    /// The lower-left corner of the viewport.
    lower_left_corner: Vec3A,
    /// The horizontal vector of the viewport.
    horizontal: Vec3A,
    /// The vertical vector of the viewport.
    vertical: Vec3A,
    /// The camera's local horizontal axis.
    u: Vec3A,
    /// The camera's local vertical axis.
    v: Vec3A,
    /// The radius of the camera's lens (used for depth of field).
    lens_radius: f32,
}

impl Camera {
    /// Creates a new `Camera` with the specified parameters.
    pub fn new(
        lookfrom: Vec3A,
        lookat: Vec3A,
        vup: Vec3A,
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
    /// - `s`, `t`: Normalized viewport coordinates in `[0, 1]`.
    /// - `lens_uv`: A 2D uniform sample used to sample the lens for depth of
    ///   field. Ignored when the camera has zero aperture. Callers pass the
    ///   sample from their `Sampler` so this dimension is decorrelated from
    ///   the pixel jitter.
    pub fn get_ray(&self, s: f32, t: f32, lens_uv: [f32; 2]) -> Ray {
        let offset = if self.lens_radius > 0.0 {
            let rd = self.lens_radius * concentric_disk(lens_uv);
            self.u * rd.x + self.v * rd.y
        } else {
            Vec3A::ZERO
        };
        Ray::new(
            self.origin + offset,
            self.lower_left_corner + s * self.horizontal + t * self.vertical - self.origin - offset,
        )
    }
}
