use crate::color::Color;
use crate::hittable::HitRecord;
use crate::ray::Ray;
use crate::vec3;

pub trait Material: Send + Sync {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool;

    fn scatter_importance(&self, r_in: &Ray, rec: &HitRecord) -> Option<(Ray, Color, f32)> {
        // Default fallback for materials that don't support importance sampling
        let mut attenuation = Color::default();
        let mut scattered = Ray::default();
        if self.scatter(r_in, rec, &mut attenuation, &mut scattered) {
            let cosine = f32::max(
                vec3::dot(rec.normal, vec3::unit_vector(scattered.direction())),
                0.0,
            );
            let pdf = 1.0; // uniform sampling (fake)
            return Some((scattered, attenuation * cosine, pdf));
        }
        None
    }
    fn emitted(&self) -> Color {
        Color::new(0.0, 0.0, 0.0)
    }
}
