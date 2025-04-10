use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3A;
use serde::{Deserialize, Serialize};
use utils::random3;
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Lambertian {
    albedo: Vec3A,
}

impl Lambertian {
    pub fn new(a: Vec3A) -> Lambertian {
        Lambertian { albedo: a }
    }
}

impl Material for Lambertian {
    fn scatter(
        &self,
        _r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Vec3A,
        scattered: &mut Ray,
    ) -> bool {
        let mut scatter_direction = rec.normal + random3();

        // Catch degenerate scatter direction
        if is_near_zero(scatter_direction) {
            scatter_direction = rec.normal;
        }

        *attenuation = self.albedo;
        *scattered = Ray::new(rec.p, scatter_direction);
        true
    }
}

fn is_near_zero(v: Vec3A) -> bool {
    const EPS: f32 = 1.0e-8;
    v.abs_diff_eq(Vec3A::ZERO, EPS)
}
