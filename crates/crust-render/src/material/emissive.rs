use crate::hittable::HitRecord;
use crate::light::Light;
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3A;
use serde::{Deserialize, Serialize};
use utils::random3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Emissive {
    color: Vec3A,
    position: Vec3A,
    radius: f32,
}

impl Emissive {
    pub fn new(color: Vec3A, position: Vec3A, radius: f32) -> Self {
        Emissive {
            color,
            position,
            radius,
        }
    }
    pub fn color(&self) -> Vec3A {
        self.color
    }
    pub fn position(&self) -> Vec3A {
        self.position
    }
    pub fn radius(&self) -> f32 {
        self.radius
    }
}

impl Material for Emissive {
    fn scatter(
        &self,
        _r_in: &Ray,
        _rec: &HitRecord,
        _attenuation: &mut Vec3A,
        _scattered: &mut Ray,
    ) -> bool {
        false // Emissive materials do not scatter
    }

    fn emitted(&self) -> Vec3A {
        self.color
    }

    fn scatter_importance(&self, _r_in: &Ray, _rec: &HitRecord) -> Option<(Ray, Vec3A, f32)> {
        None
    }
}

impl Light for Emissive {
    fn sample(&self) -> Vec3A {
        let uv = (utils::random(), utils::random());
        let theta = 2.0 * std::f32::consts::PI * uv.0;
        let phi = (1.0 - 2.0 * uv.1).acos();
        let x = phi.sin() * theta.cos();
        let y = phi.sin() * theta.sin();
        let z = phi.cos();
        self.position + self.radius * Vec3A::new(x, y, z)
    }

    fn sample_cmj(&self, u: f32, v: f32) -> Vec3A {
        let theta = 2.0 * std::f32::consts::PI * u;
        let phi = (1.0 - 2.0 * v).acos();
        let x = phi.sin() * theta.cos();
        let y = phi.sin() * theta.sin();
        let z = phi.cos();

        self.position + self.radius * Vec3A::new(x, y, z)
    }

    fn pdf(&self, hit_point: Vec3A, light_point: Vec3A) -> f32 {
        let direction = light_point - hit_point;
        let distance_squared = direction.length_squared();
        let normal = direction.normalize();
        let cosine = f32::max(normal.dot((light_point - hit_point).normalize()), 0.0);
        let area = 4.0 * std::f32::consts::PI * self.radius * self.radius;
        distance_squared / (cosine * area + 1e-4)
    }

    fn color(&self) -> Vec3A {
        self.color
    }
}
