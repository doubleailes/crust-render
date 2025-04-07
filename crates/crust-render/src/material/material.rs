use crate::hittable::HitRecord;
use crate::ray::Ray;
use glam::Vec3;

/// The `Material` trait defines the behavior of materials in the ray tracing system.
/// Materials determine how rays interact with surfaces, including scattering and emission.
pub trait Material: Send + Sync {
    /// Determines how a ray interacts with the material.
    ///
    /// # Parameters
    /// - `r_in`: The incoming ray.
    /// - `rec`: The hit record containing information about the intersection.
    /// - `attenuation`: A mutable reference to the color attenuation (output).
    /// - `scattered`: A mutable reference to the scattered ray (output).
    ///
    /// # Returns
    /// - `true` if the ray is scattered, `false` otherwise.
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Vec3,
        scattered: &mut Ray,
    ) -> bool;

    /// Computes the importance sampling for the material, if supported.
    ///
    /// This method provides a default implementation for materials that do not
    /// explicitly support importance sampling. It uses the `scatter` method to
    /// determine the scattered ray and attenuation.
    ///
    /// # Parameters
    /// - `r_in`: The incoming ray.
    /// - `rec`: The hit record containing information about the intersection.
    ///
    /// # Returns
    /// - `Some((scattered_ray, attenuation, pdf))` if importance sampling is supported.
    /// - `None` if the material does not scatter the ray.
    fn scatter_importance(&self, r_in: &Ray, rec: &HitRecord) -> Option<(Ray, Vec3, f32)> {
        // Default fallback for materials that don't support importance sampling
        let mut attenuation = Vec3::default();
        let mut scattered = Ray::default();
        if self.scatter(r_in, rec, &mut attenuation, &mut scattered) {
            let cosine = f32::max(rec.normal.dot(scattered.direction().normalize()), 0.0);
            let pdf = 1.0; // uniform sampling (fake)
            return Some((scattered, attenuation * cosine, pdf));
        }
        None
    }

    /// Returns the emitted color of the material.
    ///
    /// This method is used for materials that emit light, such as light sources.
    /// By default, it returns black (no emission).
    ///
    /// # Returns
    /// - A `Vec3` representing the emitted light.
    fn emitted(&self) -> Vec3 {
        Vec3::new(0.0, 0.0, 0.0)
    }
}
