use crate::hittable::HitRecord;
use crate::material::Material;
use crate::material::brdf::*;
use crate::ray::Ray;
use glam::Vec3A;
use std::f32::consts::PI;
use utils::{Lerp, random_cosine_direction};

use serde::{Deserialize, Serialize};
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Disney {
    pub base_color: Vec3A,
    pub metallic: f32,
    pub roughness: f32,
    pub specular: f32,
    pub specular_tint: f32,
    pub sheen: f32,
    pub sheen_tint: f32,
    pub clearcoat: f32,
    pub clearcoat_gloss: f32,
}

impl Disney {
    pub fn new(
        base_color: Vec3A,
        metallic: f32,
        roughness: f32,
        specular: f32,
        specular_tint: f32,
        sheen: f32,
        sheen_tint: f32,
        clearcoat: f32,
        clearcoat_gloss: f32,
    ) -> Self {
        Disney {
            base_color,
            metallic,
            roughness,
            specular,
            specular_tint,
            sheen,
            sheen_tint,
            clearcoat,
            clearcoat_gloss,
        }
    }
}

impl Material for Disney {
    fn scatter_importance(&self, r_in: &Ray, rec: &HitRecord) -> Option<(Ray, Vec3A, f32)> {
        let n = rec.normal;
        let v = -r_in.direction().normalize();
        let l_local = random_cosine_direction();
        let l = utils::align_to_normal(l_local, n);

        let h = (v + l).normalize();
        let n_dot_l = n.dot(l).max(0.0);
        let n_dot_v = n.dot(v).max(0.0);
        let n_dot_h = n.dot(h).max(0.0);
        let l_dot_h = l.dot(h).max(0.0);
        let v_dot_h = v.dot(h).max(0.0);

        // Fresnel
        let tint = if self.base_color.max_element() > 0.0 {
            self.base_color / self.base_color.max_element()
        } else {
            Vec3A::new(1.0, 1.0, 1.0)
        };
        let f0 = Vec3A::new(0.04, 0.04, 0.04).lerp(tint, self.specular_tint) * self.specular;
        #[allow(non_snake_case)]
        let F = fresnel_schlick(v_dot_h, f0.lerp(self.base_color, self.metallic));

        // Diffuse lobe
        let kd = (Vec3A::new(1.0, 1.0, 1.0) - F) * (1.0 - self.metallic);
        let diffuse = disney_diffuse(self.base_color, self.roughness, n, v, l, h);

        // Sheen
        let sheen_color = Vec3A::new(1.0, 1.0, 1.0).lerp(tint, self.sheen_tint);
        let sheen = sheen_color * schlick_weight(l_dot_h) * self.sheen;

        // Specular lobe
        let a = self.roughness * self.roughness;
        let a2 = a * a;
        let denom = (n_dot_h * n_dot_h * (a2 - 1.0) + 1.0).powi(2);
        #[allow(non_snake_case)]
        let D = a2 / (PI * denom.max(1e-4));
        #[allow(non_snake_case)]
        let G = (2.0 * n_dot_l / (n_dot_l + f32::sqrt(a2 + (1.0 - a2) * n_dot_l * n_dot_l)))
            .min(1.0)
            * (2.0 * n_dot_v / (n_dot_v + f32::sqrt(a2 + (1.0 - a2) * n_dot_v * n_dot_v))).min(1.0);
        let specular = F * D * G / (4.0 * n_dot_v * n_dot_l + 1e-4);

        // Clearcoat lobe
        let h_clear = (v + l).normalize();
        let clear_alpha = (1.0 - self.clearcoat_gloss).lerp(0.1, 0.001);
        #[allow(non_snake_case)]
        let Dc = gtr1(n.dot(h_clear).max(0.0), clear_alpha);
        #[allow(non_snake_case)]
        let Fc = fresnel_schlick_scalar(v.dot(h_clear).max(0.0), 0.04);
        #[allow(non_snake_case)]
        let Gc = 1.0; // simplified

        let clearcoat = self.clearcoat * Dc * Fc * Gc / (4.0 * n_dot_v * n_dot_l + 1e-4);

        let total = kd * diffuse + specular + sheen + Vec3A::new(clearcoat, clearcoat, clearcoat);

        let scattered = Ray::new(rec.p, l);
        let pdf = n_dot_l / PI;

        Some((scattered, total * n_dot_l, pdf.max(1e-4)))
    }

    fn scatter(&self, _: &Ray, _: &HitRecord, _: &mut Vec3A, _: &mut Ray) -> bool {
        false // Only importance sampling supported
    }

    fn emitted(&self) -> Vec3A {
        Vec3A::new(0.0, 0.0, 0.0)
    }
}
