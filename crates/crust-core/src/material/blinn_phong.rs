use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3A;

use serde::{Deserialize, Serialize};
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct BlinnPhong {
    pub diffuse: Vec3A,
    pub specular: Vec3A,
    pub shininess: f32,
    pub light_dir: Vec3A, // Assume one directional light for now
}

impl BlinnPhong {
    pub fn new(diffuse: Vec3A, specular: Vec3A, shininess: f32, light_dir: Vec3A) -> Self {
        BlinnPhong {
            diffuse,
            specular,
            shininess,
            light_dir: light_dir.normalize(),
        }
    }
}

impl Material for BlinnPhong {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Vec3A,
        scattered: &mut Ray,
    ) -> bool {
        let normal = rec.normal;
        let view_dir = -r_in.direction().normalize(); // Ensure view_dir is normalized
        let light_dir = self.light_dir;

        let halfway = (view_dir + light_dir).normalize();

        let diff = f32::max(normal.dot(light_dir), 0.0);
        let spec = f32::powf(f32::max(normal.dot(halfway), 0.0), self.shininess);

        let color = self.diffuse * diff + self.specular * spec;

        *attenuation = color;
        *scattered = Ray::new(rec.p, light_dir); // Optional: or bounce randomly for realism

        true
    }
}
