use crate::hittable::HitRecord;
use crate::ray::Ray;
use glam::Vec3A;
use sampler::Sampler;

/// The `Material` trait defines the behavior of materials in the ray tracing system.
/// Materials determine how rays interact with surfaces, including scattering and emission.
pub trait Material: Send + Sync {
    /// Determines how a ray interacts with the material.
    ///
    /// # Parameters
    /// - `r_in`: The incoming ray.
    /// - `rec`: The hit record containing information about the intersection.
    /// - `sampler`: The active QMC sampler, from which any random samples must be drawn.
    /// - `attenuation`: A mutable reference to the color attenuation (output).
    /// - `scattered`: A mutable reference to the scattered ray (output).
    ///
    /// # Returns
    /// - `true` if the ray is scattered, `false` otherwise.
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: &mut dyn Sampler,
        attenuation: &mut Vec3A,
        scattered: &mut Ray,
    ) -> bool;

    /// Computes the importance sampling for the material, if supported.
    ///
    /// # Parameters
    /// - `r_in`: The incoming ray.
    /// - `rec`: The hit record containing information about the intersection.
    /// - `sampler`: The active QMC sampler.
    ///
    /// # Returns
    /// - `Some((scattered_ray, attenuation, pdf))` if importance sampling is supported.
    /// - `None` if the material does not scatter the ray.
    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: &mut dyn Sampler,
    ) -> Option<(Ray, Vec3A, f32)> {
        // Default fallback for materials that don't support importance sampling
        let mut attenuation = Vec3A::default();
        let mut scattered = Ray::default();
        if self.scatter(r_in, rec, sampler, &mut attenuation, &mut scattered) {
            let cosine = f32::max(rec.normal.dot(scattered.direction().normalize()), 0.0);
            let pdf = 1.0; // uniform sampling (fake)
            return Some((scattered, attenuation * cosine, pdf));
        }
        None
    }

    /// Evaluates the BSDF toward a given world-space unit direction `wi`,
    /// without sampling. This is what MIS against an external sampling
    /// strategy (light sampling, path guiding) needs and `scatter_importance`
    /// cannot provide, since the latter picks its own direction.
    ///
    /// # Returns
    /// - `Some((value, pdf))` where `value` follows the codebase convention of
    ///   `brdf * cos(theta_i)` (the tracer multiplies by the cosine again) and
    ///   `pdf` is the solid-angle density `scatter_importance` would have
    ///   assigned to `wi`.
    /// - `None` if the material cannot be evaluated for arbitrary directions
    ///   (delta/specular or transmissive lobes).
    ///
    /// # Contract
    /// Whether this returns `None` must depend only on the material and hit
    /// state, never on `wi` — the integrator uses "eval is available" to pick
    /// a single estimator per vertex before choosing a direction. Rejecting a
    /// particular direction (e.g. below the hemisphere) must instead return
    /// `Some((Vec3A::ZERO, pdf))` with a small positive pdf.
    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        let _ = (r_in, rec, wi);
        None
    }

    /// Returns the emitted color of the material.
    ///
    /// This method is used for materials that emit light, such as light sources.
    /// By default, it returns black (no emission).
    fn emitted(&self) -> Vec3A {
        Vec3A::new(0.0, 0.0, 0.0)
    }
}
