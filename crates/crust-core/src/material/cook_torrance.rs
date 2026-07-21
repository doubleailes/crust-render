use crate::hittable::HitRecord;
use crate::material::Material;
use crate::material::fresnel_schlick;
use crate::material::geometry_schlick_ggx;
use crate::material::pdf_vndf_ggx;
use crate::material::sample_vndf_ggx;
use crate::ray::Ray;
use glam::Vec3A;
use sampler::Sampler;
use std::f32::consts::PI;

#[derive(Debug, Clone)]
pub struct CookTorrance {
    pub albedo: Vec3A,
    pub roughness: f32,
    pub metallic: f32,
}

impl CookTorrance {
    pub fn new(albedo: Vec3A, roughness: f32, metallic: f32) -> Self {
        Self {
            albedo,
            roughness: roughness.clamp(0.05, 1.0),
            metallic: metallic.clamp(0.0, 1.0),
        }
    }

    /// Value (`brdf * cos * mis_weight`, matching this material's estimator)
    /// and mixture pdf (`0.5·pdf_ggx + 0.5·pdf_cosine`) for an arbitrary
    /// outgoing direction `l`. Shared by sampling and evaluation so the two
    /// stay consistent.
    fn value_pdf(&self, v: Vec3A, l: Vec3A, n: Vec3A) -> (Vec3A, f32) {
        let h = (v + l).normalize();

        let pdf_specular = pdf_vndf_ggx(v, h, n, self.roughness) * 0.5;
        let pdf_diffuse = n.dot(l).max(1e-4) / PI * 0.5;

        let f0 = Vec3A::new(0.04, 0.04, 0.04).lerp(self.albedo, self.metallic);
        let f = fresnel_schlick(v.dot(h), f0);

        let a = self.roughness * self.roughness;
        let a2 = a * a;
        let n_dot_h = n.dot(h).max(1e-4);
        let denom = (n_dot_h * n_dot_h * (a2 - 1.0) + 1.0).powi(2);
        let d = a2 / (PI * denom);

        let g = geometry_schlick_ggx(n.dot(v), self.roughness)
            * geometry_schlick_ggx(n.dot(l), self.roughness);

        let spec = (f * d * g) / (4.0 * n.dot(v) * n.dot(l) + 1e-4);
        let kd = (Vec3A::new(1.0, 1.0, 1.0) - f) * (1.0 - self.metallic);
        let diffuse = self.albedo / PI;
        let brdf = kd * diffuse + spec;

        let weight = utils::balance_heuristic(pdf_specular, pdf_diffuse);
        let n_dot_l = n.dot(l).max(1e-4);

        (brdf * n_dot_l * weight, (pdf_specular + pdf_diffuse).max(1e-4))
    }
}

impl Material for CookTorrance {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: &mut dyn Sampler,
        attenuation: &mut Vec3A,
        scattered: &mut Ray,
    ) -> bool {
        let n = rec.normal;
        let v = -r_in.direction().normalize();

        // Sample a halfway vector using VNDF
        let h = sample_vndf_ggx(v, self.roughness, sampler.next_2d());
        let l = -v.reflect(h);
        if l.dot(n) <= 0.0 {
            return false;
        }

        let n_dot_v = n.dot(v).max(1e-4);
        let n_dot_l = n.dot(l).max(1e-4);
        let n_dot_h = n.dot(h).max(1e-4);
        let v_dot_h = v.dot(h).max(1e-4);
        let f0 = Vec3A::new(0.04, 0.04, 0.04).lerp(self.albedo, self.metallic);
        let f = fresnel_schlick(v_dot_h, f0);

        let a = self.roughness * self.roughness;
        let a2 = a * a;
        let denom = (n_dot_h * n_dot_h * (a2 - 1.0) + 1.0).powi(2);
        let d = a2 / (PI * denom);

        let g = geometry_schlick_ggx(n_dot_v, self.roughness)
            * geometry_schlick_ggx(n_dot_l, self.roughness);
        let specular = (f * d * g) / (4.0 * n_dot_v * n_dot_l + 1e-4);
        let kd = (Vec3A::new(1.0, 1.0, 1.0) - f) * (1.0 - self.metallic);
        let diffuse = self.albedo / PI;

        *attenuation = kd * diffuse + specular;
        *scattered = Ray::new(rec.p, l);

        true
    }

    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: &mut dyn Sampler,
    ) -> Option<(Ray, Vec3A, f32)> {
        let n = rec.normal;
        let v = -r_in.direction().normalize();

        let sample_specular = sampler.next_1d() < 0.5;

        let l = if sample_specular {
            // === Sample GGX specular ===
            let h = sample_vndf_ggx(v, self.roughness, sampler.next_2d());
            let l = -v.reflect(h);
            if l.dot(n) <= 0.0 {
                return None;
            }
            l
        } else {
            // === Sample cosine-weighted hemisphere (diffuse) ===
            let l_local = utils::cosine_hemisphere(sampler.next_2d());
            let l = utils::align_to_normal(l_local, n);
            if l.dot(n) <= 0.0 {
                return None;
            }
            l
        };

        let (value, pdf) = self.value_pdf(v, l, n);
        Some((Ray::new(rec.p, l), value, pdf))
    }

    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        let n = rec.normal;
        let v = -r_in.direction().normalize();
        let l = wi.normalize();
        if l.dot(n) <= 0.0 || v.dot(n) <= 0.0 {
            return Some((Vec3A::ZERO, 1e-4));
        }
        Some(self.value_pdf(v, l, n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sampler::RngSampler;

    #[test]
    fn eval_matches_scatter_importance() {
        let m = CookTorrance::new(Vec3A::new(0.7, 0.5, 0.3), 0.4, 0.2);
        let mut sampler = RngSampler::default();
        let mut rec = HitRecord::new();
        rec.p = Vec3A::ZERO;
        rec.normal = Vec3A::Z;
        rec.front_face = true;
        let r_in = Ray::new(Vec3A::new(0.3, -0.2, 1.0), Vec3A::new(-0.3, 0.2, -1.0).normalize());

        let mut checked = 0;
        for _ in 0..128 {
            if let Some((scattered, value, pdf)) = m.scatter_importance(&r_in, &rec, &mut sampler)
            {
                let wi = scattered.direction().normalize();
                let (ev, epdf) = m.eval(&r_in, &rec, wi).expect("cook-torrance is evaluable");
                let tol = 1e-3 * (1.0 + value.max_element().abs());
                assert!((ev - value).abs().max_element() < tol, "{ev} vs {value}");
                assert!((epdf - pdf).abs() < 1e-3 * (1.0 + pdf), "{epdf} vs {pdf}");
                checked += 1;
            }
        }
        assert!(checked > 32, "too few valid samples: {checked}");
    }
}
