use crate::ray::Ray;
use glam::Vec3A;

/// The `HitRecord` struct stores the geometry of a ray-surface
/// intersection as the *materials* consume it: the intersection point,
/// the ray-facing surface normal, the ray parameter, and the facing flag.
/// Intersection itself happens in the `crust-rt` kernel (which reports
/// ID-based [`crust_rt::RayHit`]s); [`crate::World`] converts kernel hits
/// into `HitRecord`s and looks up the material by `geom_id`.
#[derive(Clone, Copy, Default)]
pub struct HitRecord {
    /// The point of intersection.
    pub p: Vec3A,
    /// The surface normal at the intersection point.
    pub normal: Vec3A,
    /// The parameter `t` along the ray where the intersection occurs.
    pub t: f32,
    /// Indicates whether the ray hit the front face of the surface.
    pub front_face: bool,
}

impl HitRecord {
    /// Creates a new, default `HitRecord`.
    pub fn new() -> HitRecord {
        Default::default()
    }

    /// Sets the surface normal and determines whether the ray hit the front face.
    ///
    /// # Parameters
    /// - `r`: The ray that intersects the object.
    /// - `outward_normal`: The outward-facing normal of the surface.
    ///
    /// This method adjusts the normal to always point against the ray's direction
    /// and sets the `front_face` flag accordingly.
    pub fn set_face_normal(&mut self, r: &Ray, outward_normal: Vec3A) {
        self.front_face = r.direction().dot(outward_normal) < 0.0;
        self.normal = if self.front_face {
            outward_normal
        } else {
            -outward_normal
        };
    }
}
