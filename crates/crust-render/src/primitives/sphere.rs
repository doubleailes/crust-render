use crate::hittable::{HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use std::sync::Arc;
use utils::Point3;

/// The `Sphere` struct represents a 3D sphere primitive in the ray tracing system.
/// It implements the `Hittable` trait, allowing it to be intersected by rays.
pub struct Sphere {
    /// The center of the sphere in 3D space.
    center: Point3,
    /// The radius of the sphere.
    radius: f32,
    /// The material of the sphere.
    mat: Arc<dyn Material>,
}

impl Sphere {
    /// Creates a new `Sphere` with the specified center, radius, and material.
    ///
    /// # Parameters
    /// - `cen`: The center of the sphere in 3D space.
    /// - `r`: The radius of the sphere.
    /// - `m`: An `Arc<dyn Material>` representing the material of the sphere.
    ///
    /// # Returns
    /// - A new instance of `Sphere`.
    pub fn new(cen: Point3, r: f32, m: Arc<dyn Material>) -> Sphere {
        Sphere {
            center: cen,
            radius: r,
            mat: m,
        }
    }
}

impl Hittable for Sphere {
    /// Determines if a ray intersects the sphere.
    ///
    /// # Parameters
    /// - `r`: The ray to test for intersection.
    /// - `t_min`: The minimum value of the parameter `t` to consider.
    /// - `t_max`: The maximum value of the parameter `t` to consider.
    /// - `rec`: A mutable reference to a `HitRecord` to store intersection details.
    ///
    /// # Returns
    /// - `true` if the ray intersects the sphere, `false` otherwise.
    ///
    /// This method computes the intersection of the ray with the sphere using the quadratic formula.
    /// If an intersection is found within the valid range, it updates the `HitRecord` with details
    /// such as the intersection point, surface normal, and material.
    fn hit(&self, r: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool {
        let oc = r.origin() - self.center;
        let a = r.direction().length_squared();
        let half_b = utils::dot(oc, r.direction());
        let c = oc.length_squared() - self.radius * self.radius;
        let discriminant = half_b * half_b - a * c;
        if discriminant < 0.0 {
            return false;
        }

        let sqrt_d = f32::sqrt(discriminant);

        // Find the nearest root that lies in the acceptable range
        let mut root = (-half_b - sqrt_d) / a;
        if root <= t_min || t_max <= root {
            root = (-half_b + sqrt_d) / a;
            if root <= t_min || t_max <= root {
                return false;
            }
        }

        rec.t = root;
        rec.p = r.at(rec.t);
        let outward_normal = (rec.p - self.center) / self.radius;
        rec.set_face_normal(r, outward_normal);
        rec.mat = Some(self.mat.clone());
        true
    }
}
