use crate::hittable::HitRecord;
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3A;
use sampler::Sampler;
use std::f32::consts::PI;
use utils::uniform_sphere;
#[derive(Debug, Clone)]
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
        sampler: &mut dyn Sampler,
        attenuation: &mut Vec3A,
        scattered: &mut Ray,
    ) -> bool {
        // True Lambertian scattering: perturb the normal by a uniformly random
        // unit vector.
        let mut scatter_direction = rec.normal + uniform_sphere(sampler.next_2d());

        // Catch degenerate scatter direction
        if is_near_zero(scatter_direction) {
            scatter_direction = rec.normal;
        }

        *attenuation = self.albedo;
        *scattered = Ray::new(rec.p, scatter_direction);
        true
    }

    fn scatter_importance(
        &self,
        _r_in: &Ray,
        rec: &HitRecord,
        sampler: &mut dyn Sampler,
    ) -> Option<(Ray, Vec3A, f32)> {
        // `normal + uniform_sphere` is cosine-weighted sampling: pdf = cos/pi.
        let mut scatter_direction = rec.normal + uniform_sphere(sampler.next_2d());
        if is_near_zero(scatter_direction) {
            scatter_direction = rec.normal;
        }
        let dir = scatter_direction.normalize();
        let cosine = rec.normal.dot(dir).max(0.0);
        let pdf = (cosine / PI).max(1e-4);
        Some((Ray::new(rec.p, dir), self.albedo * cosine / PI, pdf))
    }

    fn eval(&self, _r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        let cosine = rec.normal.dot(wi.normalize()).max(0.0);
        Some((self.albedo * cosine / PI, (cosine / PI).max(1e-4)))
    }
}

fn is_near_zero(v: Vec3A) -> bool {
    const EPS: f32 = 1.0e-8;
    v.abs_diff_eq(Vec3A::ZERO, EPS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sampler::RngSampler;

    #[test]
    fn eval_matches_scatter_importance() {
        let m = Lambertian::new(Vec3A::new(0.6, 0.4, 0.2));
        let mut sampler = RngSampler::default();
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let r_in = Ray::new(Vec3A::new(0.3, -0.2, 1.0), Vec3A::new(-0.3, 0.2, -1.0).normalize());

        for _ in 0..64 {
            let (scattered, value, pdf) = m
                .scatter_importance(&r_in, &rec, &mut sampler)
                .expect("lambertian always scatters");
            let wi = scattered.direction().normalize();
            let (ev, epdf) = m.eval(&r_in, &rec, wi).expect("lambertian is evaluable");
            assert!((ev - value).abs().max_element() < 1e-3, "{ev} vs {value}");
            assert!((epdf - pdf).abs() < 1e-3, "{epdf} vs {pdf}");
        }
    }
}
