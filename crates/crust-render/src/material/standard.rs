use crate::hittable::HitRecord;
use crate::material::Material;
use crate::material::brdf::*;
use crate::ray::Ray;
use utils::{Color, Vec3, dot, random2, reflect, refract_standard, unit_vector};

pub struct StandardSurface {
    pub base_color: Color,
    pub metallic: f32,
    pub roughness: f32,
    pub specular: f32,
    pub specular_color: Color,
    pub transmission: f32,
    pub ior: f32,
    pub clearcoat: f32,
    pub clearcoat_gloss: f32,
    pub emission: Color,
    pub film_thickness: f32,
    pub film_ior: f32,
    pub sheen: f32,
    pub sheen_color: Color,
    pub subsurface: f32,
    pub subsurface_color: Color,
    pub anisotropy: f32,
}

impl StandardSurface {
    pub fn new(
        base_color: Color,
        metallic: f32,
        roughness: f32,
        specular: f32,
        specular_color: Color,
        transmission: f32,
        ior: f32,
        clearcoat: f32,
        clearcoat_gloss: f32,
        emission: Color,
        film_thickness: f32,
        film_ior: f32,
        sheen: f32,
        sheen_color: Color,
        subsurface: f32,
        subsurface_color: Color,
        anisotropy: f32,
    ) -> Self {
        Self {
            base_color,
            metallic,
            roughness,
            specular,
            specular_color,
            transmission,
            ior,
            clearcoat,
            clearcoat_gloss,
            emission,
            film_thickness,
            film_ior,
            sheen,
            sheen_color,
            subsurface,
            subsurface_color,
            anisotropy,
        }
    }
}

impl Material for StandardSurface {
    fn scatter(
        &self,
        ray_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        if let Some((ray, color, _pdf)) = self.scatter_importance(ray_in, rec) {
            *scattered = ray;
            *attenuation = color;
            true
        } else {
            false
        }
    }

    fn scatter_importance(&self, ray_in: &Ray, rec: &HitRecord) -> Option<(Ray, Color, f32)> {
        let n = rec.normal;
        let v = -unit_vector(ray_in.direction());

        let base_weight = (1.0 - self.metallic) * (1.0 - self.transmission);
        let specular_weight = self.specular;
        let transmission_weight = self.transmission;
        let clearcoat_weight = self.clearcoat * (1.0 - self.metallic);
        let sheen_weight = self.sheen * (1.0 - self.metallic);
        let subsurface_weight = self.subsurface * (1.0 - self.metallic);
        let total = base_weight
            + specular_weight
            + transmission_weight
            + clearcoat_weight
            + sheen_weight
            + subsurface_weight;

        let (r, _) = random2();
        let choice = r * total;

        let (out_dir, brdf_color, pdf) = if choice < base_weight {
            let (l, c) = sample_diffuse(self.base_color, n, v, self.roughness);
            (l, c, 1.0 / (2.0 * std::f32::consts::PI))
        } else if choice < base_weight + specular_weight {
            let (l, c) = sample_specular(
                self.specular_color,
                n,
                v,
                self.roughness,
                self.ior,
                self.film_thickness,
                self.film_ior,
                self.anisotropy,
            );
            let h = unit_vector(v + l);
            let d = pdf_vndf_ggx(v, h, n, self.roughness);
            let pdf = d / (4.0 * dot(l, h).max(1e-4));
            (l, c, pdf)
        } else if choice < base_weight + specular_weight + transmission_weight {
            let (l, c) = sample_transmission(n, v, self.roughness, self.ior);
            (l, c, 1.0) // Placeholder, needs proper transmission PDF
        } else if choice < base_weight + specular_weight + transmission_weight + clearcoat_weight {
            let (l, c) = sample_clearcoat(n, v, self.clearcoat_gloss);
            (l, c, 1.0) // Clearcoat PDF can be added
        } else if choice
            < base_weight + specular_weight + transmission_weight + clearcoat_weight + sheen_weight
        {
            let (l, c) = sample_sheen(v, self.sheen_color);
            (l, c, 1.0)
        } else {
            let (l, c) =
                sample_subsurface(self.base_color, n, v, self.subsurface_color, self.roughness);
            (l, c, 1.0)
        };

        Some((Ray::new(rec.p, out_dir), brdf_color, pdf))
    }

    fn emitted(&self) -> Color {
        self.emission
    }
}

fn sample_diffuse(base_color: Color, n: Vec3, v: Vec3, roughness: f32) -> (Vec3, Color) {
    let l = utils::random_cosine_direction();
    let h = unit_vector(v + l);
    let color = disney_diffuse(base_color, roughness, n, v, l, h);
    (l, color)
}

fn thin_film_interference(
    cos_theta: f32,
    base_f0: Color,
    ior1: f32,
    ior2: f32,
    thickness_nm: f32,
) -> Color {
    let lambda = [440.0, 550.0, 680.0];
    let two_pi = std::f32::consts::PI * 2.0;

    let components = [base_f0.x(), base_f0.y(), base_f0.z()];
    let mut result = [0.0; 3];

    for (i, &wl) in lambda.iter().enumerate() {
        let delta = two_pi * ior2 * thickness_nm * cos_theta / wl;
        let interference = 0.5 + 0.5 * f32::cos(delta);
        let base = components[i];
        result[i] = base * interference + (1.0 - interference) * base;
    }

    Color::new(result[0], result[1], result[2])
}

fn sample_specular(
    spec_color: Color,
    _n: Vec3,
    v: Vec3,
    roughness: f32,
    ior: f32,
    film_thickness: f32,
    film_ior: f32,
    anisotropy: f32,
) -> (Vec3, Color) {
    let h = sample_vndf_ggx(v, roughness); // Replace with anisotropic if needed
    let l = reflect(-v, h);
    let f_raw = fresnel_schlick(dot(l, h).max(0.0), spec_color);
    let f_tfi = thin_film_interference(dot(v, h), f_raw, 1.0, film_ior, film_thickness);
    (l, f_tfi)
}

fn sample_transmission(n: Vec3, v: Vec3, roughness: f32, ior: f32) -> (Vec3, Color) {
    let eta = if dot(v, n) > 0.0 { 1.0 / ior } else { ior };
    let h = sample_vndf_ggx(v, roughness);
    if let Some(refracted) = refract_standard(v, h, eta) {
        let f0 = ((1.0 - ior) / (1.0 + ior)).powi(2);
        let f = fresnel_schlick_scalar(dot(v, h).max(0.0), f0);
        (refracted, Color::new(1.0 - f, 1.0 - f, 1.0 - f))
    } else {
        let reflected = reflect(v, h);
        (reflected, Color::new(1.0, 1.0, 1.0))
    }
}

fn sample_clearcoat(_n: Vec3, v: Vec3, gloss: f32) -> (Vec3, Color) {
    let alpha = 1.0 - gloss;
    let h = sample_vndf_ggx(v, alpha);
    let l = reflect(-v, h);
    let f = fresnel_schlick_scalar(dot(l, h).max(0.0), 0.04);
    (l, Color::new(f, f, f))
}

fn sample_sheen(v: Vec3, sheen_color: Color) -> (Vec3, Color) {
    let l = utils::random_cosine_direction();
    let h = unit_vector(v + l);
    let sheen_fresnel = schlick_weight(dot(l, h).max(0.0));
    (l, sheen_color * sheen_fresnel)
}

fn sample_subsurface(
    base_color: Color,
    n: Vec3,
    v: Vec3,
    subsurface_color: Color,
    roughness: f32,
) -> (Vec3, Color) {
    let l = utils::random_cosine_direction();
    let h = unit_vector(v + l);
    let wrap = 0.5;
    let nl = dot(n, l).max(0.0) * (1.0 - wrap) + wrap;
    let color = disney_diffuse(base_color * subsurface_color, roughness, n, v, l, h) * nl;
    (l, color)
}
