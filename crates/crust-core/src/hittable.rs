use crate::ray::Ray;
use glam::Vec3A;

use crate::aabb::AABB;
use crate::material::Material;

/// The `HitRecord` struct stores the geometry of a ray-object intersection:
/// the intersection point, surface normal, ray parameter, and facing.
/// It is plain `Copy` data — the material of the hit surface travels
/// alongside it in [`Hit`], borrowed from the primitive that was hit.
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
    ///
    /// # Returns
    /// - A new instance of `HitRecord` with default values.
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

/// A successful ray-object intersection: the geometric [`HitRecord`] plus the
/// material at the hit point, borrowed from the scene for the duration of the
/// trace. Borrowing (instead of the former `Option<Arc<dyn Material>>` field)
/// keeps intersection records `Copy` and removes an atomic refcount bump per
/// candidate hit in the traversal hot loop.
#[derive(Clone, Copy)]
pub struct Hit<'a> {
    pub rec: HitRecord,
    pub mat: &'a dyn Material,
}

/// The `Hittable` trait defines objects that can be intersected by rays.
/// Implementing this trait allows objects to participate in ray tracing.
pub trait Hittable: Send + Sync {
    /// Determines if a ray intersects the object.
    ///
    /// # Parameters
    /// - `ray`: The ray to test for intersection.
    /// - `t_min`: The minimum value of the parameter `t` to consider.
    /// - `t_max`: The maximum value of the parameter `t` to consider.
    ///
    /// # Returns
    /// - `Some(Hit)` describing the closest intersection in `(t_min, t_max)`,
    ///   or `None` if the ray misses the object.
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<Hit<'_>>;
    fn bounding_box(&self) -> Option<AABB>;
}
