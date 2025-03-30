use crate::color::Color;
use crate::hittable::HitRecord;
use crate::light::Light;
use crate::material::Material;
use crate::ray::Ray;
use crate::vec3;
use crate::vec3::Point3;
use crate::vec3::*;

pub struct Emissive {
    color: Color,
    position: Point3,
    radius: f32,
}

impl Emissive {
    pub fn new(color: Color, position: Point3, radius: f32) -> Self {
        Emissive {
            color,
            position,
            radius,
        }
    }
    pub fn color(&self) -> Color {
        self.color
    }
    pub fn position(&self) -> Point3 {
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
        _attenuation: &mut Color,
        _scattered: &mut Ray,
    ) -> bool {
        false // Emissive materials do not scatter
    }

    fn emitted(&self) -> Color {
        self.color
    }

    fn scatter_importance(&self, _r_in: &Ray, _rec: &HitRecord) -> Option<(Ray, Color, f32)> {
        None
    }
}

impl Light for Emissive {
    fn sample(&self) -> Point3 {
        self.position + self.radius * crate::vec3::random_unit_vector()
    }
    fn sample_cmj(&self, u: f32, v: f32) -> Point3 {
        // Map (u, v) on a sphere (uniform sphere sampling)
        let theta = 2.0 * std::f32::consts::PI * u;
        let phi = (1.0 - 2.0 * v).acos();
        let x = phi.sin() * theta.cos();
        let y = phi.sin() * theta.sin();
        let z = phi.cos();

        self.position + self.radius * Vec3::new(x, y, z)
    }

    fn pdf(&self, hit_point: Point3, light_point: Point3) -> f32 {
        let direction = light_point - hit_point;
        let distance_squared = direction.length_squared();
        let normal = vec3::unit_vector(direction);
        let cosine = f32::max(
            vec3::dot(normal, vec3::unit_vector(light_point - hit_point)),
            0.0,
        );
        let area = 4.0 * std::f32::consts::PI * self.radius * self.radius;
        distance_squared / (cosine * area + 1e-4)
    }

    fn color(&self) -> Color {
        self.color
    }
}
