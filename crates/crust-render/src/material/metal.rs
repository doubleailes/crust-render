use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3;

use rand::Rng;
use serde::{Deserialize, Serialize};
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Metal {
    albedo: Vec3,
    fuzz: f32,
}

impl Metal {
    pub fn new(a: Vec3, f: f32) -> Metal {
        Metal {
            albedo: a,
            fuzz: if f < 1.0 { f } else { 1.0 },
        }
    }
}

impl Material for Metal {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Vec3,
        scattered: &mut Ray,
    ) -> bool {
        let mut rnd = rand::rng();
        let reflected = r_in.direction().normalize().reflect(rec.normal);
        *attenuation = self.albedo;
        *scattered = Ray::new(
            rec.p,
            reflected + self.fuzz * utils::random_vec3_unit_sphere(&mut rnd),
        );
        scattered.direction().dot(rec.normal) > 0.0
    }
}
