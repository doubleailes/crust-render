use crate::ray::Ray;
use std::sync::Arc;
use utils::{Point3, Vec3};

use crate::material::Material;
use crate::aabb::AABB;

/// The `HitRecord` struct stores information about a ray-object intersection.
/// It contains details such as the intersection point, surface normal, material, and more.
#[derive(Clone, Default)]
pub struct HitRecord {
    /// The point of intersection.
    pub p: Point3,
    /// The surface normal at the intersection point.
    pub normal: Vec3,
    /// The material of the object at the intersection point.
    pub mat: Option<Arc<dyn Material>>,
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
    pub fn set_face_normal(&mut self, r: &Ray, outward_normal: Vec3) {
        self.front_face = utils::dot(r.direction(), outward_normal) < 0.0;
        self.normal = if self.front_face {
            outward_normal
        } else {
            -outward_normal
        };
    }
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
    /// - `rec`: A mutable reference to a `HitRecord` to store intersection details.
    ///
    /// # Returns
    /// - `true` if the ray intersects the object, `false` otherwise.
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool;
    fn bounding_box(&self) -> Option<AABB>;
}
