use utils::Color;
use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;

pub struct Metal {
    albedo: Color,
    fuzz: f32,
}

impl Metal {
    pub fn new(a: Color, f: f32) -> Metal {
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
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let reflected = utils::reflect(utils::unit_vector(r_in.direction()), rec.normal);

        *attenuation = self.albedo;
        *scattered = Ray::new(
            rec.p,
            reflected + self.fuzz * utils::random_in_unit_sphere(),
        );
        utils::dot(scattered.direction(), rec.normal) > 0.0
    }
}
