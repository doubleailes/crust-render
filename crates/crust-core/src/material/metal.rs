use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3A;
use sampler::Sampler;
use utils::uniform_ball;

#[derive(Debug, Clone)]
pub struct Metal {
    albedo: Vec3A,
    fuzz: f32,
}

impl Metal {
    pub fn new(a: Vec3A, f: f32) -> Metal {
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
        sampler: &mut dyn Sampler,
        attenuation: &mut Vec3A,
        scattered: &mut Ray,
    ) -> bool {
        let reflected = r_in.direction().normalize().reflect(rec.normal);
        // Fuzz jitter: sample a point in the unit ball from three sampler
        // dimensions and offset the reflected direction.
        let ball_uvw = {
            let a = sampler.next_2d();
            [a[0], a[1], sampler.next_1d()]
        };
        *attenuation = self.albedo;
        *scattered = Ray::new(rec.p, reflected + self.fuzz * uniform_ball(ball_uvw));
        scattered.direction().dot(rec.normal) > 0.0
    }
}
