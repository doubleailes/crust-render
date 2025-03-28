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
}

pub struct Lambertian {
    albedo: Color,
}

impl Lambertian {
    pub fn new(a: Color) -> Lambertian {
        Lambertian { albedo: a }
    }
}

impl Material for Lambertian {
    fn scatter(
        &self,
        _r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let mut scatter_direction = rec.normal + vec3::random_unit_vector();

        // Catch degenerate scatter direction
        if scatter_direction.near_zero() {
            scatter_direction = rec.normal;
        }
        *attenuation = self.albedo;
        *scattered = Ray::new(rec.p, scatter_direction);
        true
    }
}

pub struct Metal {
    albedo: Color,
    roughness: f32,
}
 
impl Metal {
    pub fn new(a: Color, r:f32) -> Metal {
        Metal { albedo: a, roughness:if r < 1.0 { r } else { 1.0 }, }
    }
}
 
impl Material for Metal {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let reflected = vec3::reflect(vec3::unit_vector(r_in.direction()), rec.normal);
 
        *attenuation = self.albedo;
        *scattered = Ray::new(rec.p, reflected + self.roughness * vec3::random_in_unit_sphere());
        vec3::dot(scattered.direction(), rec.normal) > 0.0
    }
}
