use crate::error::Error;
use crate::ray::Ray;
use glam::{Mat4, Vec3A, Vec4};
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

    /// Builds a camera from a world-to-camera (view) matrix and a projection
    /// matrix — the pair a Hydra host hands its render delegate — instead of
    /// lookat/vfov parameters.
    ///
    /// Works by unprojecting three NDC corners onto the camera-space focus
    /// plane `z = -focus_dist` and expressing the existing viewport fields
    /// from them, so `get_ray` is untouched and both constructors share one
    /// ray-generation path. Unprojecting a near→far point pair per corner
    /// (rather than assuming a depth convention) makes this indifferent to
    /// GL/DX depth ranges and reverse-Z, and correct for off-axis/asymmetric
    /// frustums: each corner lands wherever the projection actually looks.
    ///
    /// NDC x = -1 is the left edge and y = -1 the **bottom** edge (matching
    /// the render loop's `v = (j + fy) / h` with row 0 at the bottom).
    ///
    /// # Errors
    /// [`Error::InvalidCamera`] when either matrix is non-invertible, when
    /// the projection is affine (orthographic — its rays would need
    /// per-pixel origins, which this camera model cannot express), or when
    /// the unprojected frustum degenerates (non-finite or zero-area).
    pub fn from_view_projection(
        view: Mat4,
        proj: Mat4,
        aperture: f32,
        focus_dist: f32,
    ) -> Result<Camera, Error> {
        if !view.determinant().is_finite() || view.determinant().abs() < 1e-12 {
            return Err(Error::InvalidCamera("view matrix is not invertible".into()));
        }
        if !proj.determinant().is_finite() || proj.determinant().abs() < 1e-12 {
            return Err(Error::InvalidCamera(
                "projection matrix is not invertible".into(),
            ));
        }
        // A perspective projection feeds -z (or +z) into the output w; an
        // affine one leaves w constant. `z_axis.w` is that coupling term
        // (column-major: row 3 of the z column).
        if proj.z_axis.w.abs() < 1e-8 {
            return Err(Error::InvalidCamera(
                "projection is affine (orthographic projections are not supported)".into(),
            ));
        }
        if !(focus_dist.is_finite() && focus_dist > 0.0) {
            return Err(Error::InvalidCamera(format!(
                "focus distance must be finite and positive, got {focus_dist}"
            )));
        }

        let inv_view = view.inverse();
        let inv_proj = proj.inverse();

        // Unproject one NDC (x, y) to the camera-space plane z = -focus_dist.
        let corner_on_focus_plane = |x: f32, y: f32| -> Option<Vec3A> {
            let near = inv_proj * Vec4::new(x, y, -1.0, 1.0);
            let far = inv_proj * Vec4::new(x, y, 1.0, 1.0);
            if near.w.abs() < 1e-12 || far.w.abs() < 1e-12 {
                return None;
            }
            let a = near.truncate() / near.w;
            let b = far.truncate() / far.w;
            let dz = b.z - a.z;
            if !dz.is_finite() || dz.abs() < 1e-12 {
                return None;
            }
            let t = (-focus_dist - a.z) / dz;
            let p = a + (b - a) * t;
            let world = inv_view * p.extend(1.0);
            let world = Vec3A::from_vec4(world);
            world.is_finite().then_some(world)
        };

        let degenerate = || Error::InvalidCamera("frustum unprojection degenerates".into());
        let c00 = corner_on_focus_plane(-1.0, -1.0).ok_or_else(degenerate)?; // bottom-left
        let c10 = corner_on_focus_plane(1.0, -1.0).ok_or_else(degenerate)?; // bottom-right
        let c01 = corner_on_focus_plane(-1.0, 1.0).ok_or_else(degenerate)?; // top-left

        let origin = Vec3A::from_vec4(inv_view * Vec4::new(0.0, 0.0, 0.0, 1.0));
        let horizontal = c10 - c00;
        let vertical = c01 - c00;
        if !origin.is_finite()
            || horizontal.length_squared() < 1e-24
            || vertical.length_squared() < 1e-24
        {
            return Err(degenerate());
        }

        // The lens offset axes: the camera's world-space right and up. Taken
        // from the view matrix (not from horizontal/vertical) so depth of
        // field keeps a circular lens even under an asymmetric frustum.
        let u = Vec3A::from_vec4(inv_view.x_axis).normalize();
        let v = Vec3A::from_vec4(inv_view.y_axis).normalize();

        Ok(Camera {
            origin,
            lower_left_corner: c00,
            horizontal,
            vertical,
            u,
            v,
            lens_radius: aperture / 2.0,
        })
    }

    /// The camera's viewing direction: from the origin through the center of
    /// the viewport. Works for both constructors (for `Camera::new` it is
    /// `-w`, the lookfrom→lookat direction).
    pub fn forward(&self) -> Vec3A {
        (self.lower_left_corner + 0.5 * self.horizontal + 0.5 * self.vertical - self.origin)
            .normalize()
    }

    /// Generates a ray originating from the camera through the viewport.
    ///
    /// # Parameters
    /// - `s`, `t`: Normalized viewport coordinates in `[0, 1]`.
    /// - `lens_uv`: A 2D uniform sample used to sample the lens for depth of
    ///   field. Ignored when the camera has zero aperture. Callers pass a QMC
    ///   sample so this dimension is decorrelated from the pixel jitter.
    /// - `time`: Shutter time in `[0, 1)`, carried on the ray for motion
    ///   blur (moving instances interpolate their transform at this time).
    pub fn get_ray(&self, s: f32, t: f32, lens_uv: [f32; 2], time: f32) -> Ray {
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
        .with_time(time)
        .with_mask(crate::ray::MASK_CAMERA)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use glam::camera::rh::{proj, view};

    fn assert_close(a: Vec3A, b: Vec3A, eps: f32, what: &str) {
        assert!(
            (a - b).length() <= eps,
            "{what}: {a:?} vs {b:?} (Δ = {})",
            (a - b).length()
        );
    }

    /// The matrix constructor must reproduce the lookat constructor when
    /// fed the equivalent view + projection pair — with and without
    /// aperture, across the whole viewport.
    #[test]
    fn matrix_camera_matches_lookat_camera() {
        let lookfrom = Vec3A::new(15.0, 3.0, 3.0);
        let lookat = Vec3A::new(0.0, 1.0, 0.0);
        let vup = Vec3A::new(0.0, 1.0, 0.0);
        let (vfov, aspect, focus) = (20.0f32, 16.0 / 9.0, 10.0);
        let view = view::look_at_mat4(lookfrom.into(), lookat.into(), vup.into());
        let proj = proj::opengl::perspective(vfov.to_radians(), aspect, 0.1, 100.0);

        for aperture in [0.0f32, 0.4] {
            let reference = Camera::new(lookfrom, lookat, vup, vfov, aspect, aperture, focus);
            let from_matrices =
                Camera::from_view_projection(view, proj, aperture, focus).expect("valid camera");
            for (s, t) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (0.5, 0.5), (0.3, 0.8)] {
                let lens = [0.7, 0.3];
                let a = reference.get_ray(s, t, lens, 0.0);
                let b = from_matrices.get_ray(s, t, lens, 0.0);
                assert_close(a.origin(), b.origin(), 1e-3, "origin");
                assert_close(
                    a.direction().normalize(),
                    b.direction().normalize(),
                    1e-4,
                    "direction",
                );
            }
            assert_close(
                from_matrices.forward(),
                (lookat - lookfrom).normalize(),
                1e-4,
                "forward",
            );
        }
    }

    /// An asymmetric (off-axis) frustum: every generated ray must pass
    /// through the point glam's own matrix inversion unprojects for the
    /// same NDC coordinate — an independent check of the corner-plane
    /// construction.
    #[test]
    fn matrix_camera_handles_off_axis_frustums() {
        // Right-handed GL-style frustum, deliberately lopsided.
        let proj = proj::opengl::frustum(-0.02, 0.08, -0.01, 0.05, 0.1, 100.0);
        let view = view::look_at_mat4(
            Vec3::new(2.0, 1.0, 5.0),
            Vec3::new(0.0, 0.5, 0.0),
            Vec3::Y,
        );
        let camera = Camera::from_view_projection(view, proj, 0.0, 4.0).expect("valid camera");

        let inv_vp = (proj * view).inverse();
        for (s, t) in [(0.0f32, 0.0f32), (1.0, 1.0), (0.5, 0.5), (0.2, 0.9)] {
            let ray = camera.get_ray(s, t, [0.5, 0.5], 0.0);
            // The same NDC coordinate at an arbitrary depth, unprojected by
            // glam: it must lie on the ray's line.
            let ndc = Vec3::new(s * 2.0 - 1.0, t * 2.0 - 1.0, 0.5);
            let p = Vec3A::from(inv_vp.project_point3(ndc));
            let d = ray.direction().normalize();
            let along = (p - ray.origin()).dot(d);
            let off_axis_distance = ((p - ray.origin()) - along * d).length();
            let scale = (p - ray.origin()).length().max(1.0);
            assert!(
                off_axis_distance / scale < 1e-4,
                "NDC ({s}, {t}): unprojected point {p:?} misses the ray by {off_axis_distance}"
            );
        }
    }

    #[test]
    fn orthographic_and_singular_matrices_are_rejected() {
        let view = view::look_at_mat4(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let ortho = proj::directx::orthographic(-1.0, 1.0, -1.0, 1.0, 0.1, 100.0);
        assert!(Camera::from_view_projection(view, ortho, 0.0, 1.0).is_err());

        let proj = proj::opengl::perspective(0.5, 1.0, 0.1, 100.0);
        assert!(Camera::from_view_projection(Mat4::ZERO, proj, 0.0, 1.0).is_err());
        assert!(Camera::from_view_projection(view, proj, 0.0, -1.0).is_err());
        assert!(Camera::from_view_projection(view, proj, 0.0, 1.0).is_ok());
    }

    /// The unprojection must be indifferent to the projection's depth
    /// convention: DirectX [0,1] z and reverse-Z infinite-far projections
    /// describe the same frustum as their GL twin, so the camera they
    /// build must generate the same rays.
    #[test]
    fn depth_convention_does_not_matter() {
        let view = view::look_at_mat4(Vec3::new(3.0, 2.0, 4.0), Vec3::ZERO, Vec3::Y);
        let gl = proj::opengl::perspective(0.8, 1.5, 0.1, 100.0);
        let dx = proj::directx::perspective(0.8, 1.5, 0.1, 100.0);
        let rev = proj::directx::perspective_infinite_reverse(0.8, 1.5, 0.1);
        let a = Camera::from_view_projection(view, gl, 0.0, 5.0).unwrap();
        let b = Camera::from_view_projection(view, dx, 0.0, 5.0).unwrap();
        let c = Camera::from_view_projection(view, rev, 0.0, 5.0).unwrap();
        for (s, t) in [(0.0, 0.0), (1.0, 1.0), (0.5, 0.5), (0.9, 0.1)] {
            let ra = a.get_ray(s, t, [0.5, 0.5], 0.0);
            for other in [&b, &c] {
                let rb = other.get_ray(s, t, [0.5, 0.5], 0.0);
                assert_close(ra.origin(), rb.origin(), 1e-4, "origin");
                assert_close(
                    ra.direction().normalize(),
                    rb.direction().normalize(),
                    1e-4,
                    "direction",
                );
            }
        }
    }
}
