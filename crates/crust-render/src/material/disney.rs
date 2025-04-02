use crate::hittable::HitRecord;
use crate::material::Material;
use crate::material::brdf::*;
use crate::ray::Ray;
use std::f32::consts::PI;
use utils::Color;
use utils::{Lerp, dot, unit_vector};

pub struct Disney {
    pub base_color: Color,
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
        base_color: Color,
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
    fn scatter_importance(&self, r_in: &Ray, rec: &HitRecord) -> Option<(Ray, Color, f32)> {
        let n = rec.normal;
        let v = -unit_vector(r_in.direction());
        let l_local = utils::random_cosine_direction();
        let l = utils::align_to_normal(l_local, n);

        let h = unit_vector(v + l);
        let n_dot_l = dot(n, l).max(0.0);
        let n_dot_v = dot(n, v).max(0.0);
        let n_dot_h = dot(n, h).max(0.0);
        let l_dot_h = dot(l, h).max(0.0);
        let v_dot_h = dot(v, h).max(0.0);

        // Fresnel
        let tint = if self.base_color.max_component() > 0.0 {
            self.base_color / self.base_color.max_component()
        } else {
            Color::new(1.0, 1.0, 1.0)
        };
        let f0 = Color::new(0.04, 0.04, 0.04).lerp(tint, self.specular_tint) * self.specular;
        #[allow(non_snake_case)]
        let F = fresnel_schlick(v_dot_h, f0.lerp(self.base_color, self.metallic));

        // Diffuse lobe
        let kd = (Color::new(1.0, 1.0, 1.0) - F) * (1.0 - self.metallic);
        let diffuse = disney_diffuse(self.base_color, self.roughness, n, v, l, h);

        // Sheen
        let sheen_color = Color::new(1.0, 1.0, 1.0).lerp(tint, self.sheen_tint);
        let sheen = sheen_color * schlick_weight(l_dot_h) * self.sheen;

        // Specular lobe
        let a = self.roughness * self.roughness;
        let a2 = a * a;
        let denom = (n_dot_h * n_dot_h * (a2 - 1.0) + 1.0).powi(2);
        #[allow(non_snake_case)]
        let D = a2 / (PI * denom.max(1e-4));
        #[allow(non_snake_case)]
        let G = (2.0 * n_dot_h * n_dot_v / v_dot_h).min(1.0)
            * (2.0 * n_dot_h * n_dot_l / v_dot_h).min(1.0);
        let specular = F * D * G / (4.0 * n_dot_v * n_dot_l + 1e-4);

        // Clearcoat lobe
        let h_clear = unit_vector(v + l);
        let clear_alpha = (1.0 - self.clearcoat_gloss).lerp(0.1, 0.001);
        #[allow(non_snake_case)]
        let Dc = gtr1(dot(n, h_clear).max(0.0), clear_alpha);
        #[allow(non_snake_case)]
        let Fc = fresnel_schlick_scalar(dot(v, h_clear).max(0.0), 0.04);
        #[allow(non_snake_case)]
        let Gc = 1.0; // simplified

        let clearcoat = self.clearcoat * Dc * Fc * Gc / (4.0 * n_dot_v * n_dot_l + 1e-4);

        let total = kd * diffuse + specular + sheen + Color::new(clearcoat, clearcoat, clearcoat);

        let scattered = Ray::new(rec.p, l);
        let pdf = n_dot_l / PI;

        Some((scattered, total * n_dot_l, pdf.max(1e-4)))
    }

    fn scatter(&self, _: &Ray, _: &HitRecord, _: &mut Color, _: &mut Ray) -> bool {
        false // Only importance sampling supported
    }

    fn emitted(&self) -> Color {
        Color::zero()
    }
}
