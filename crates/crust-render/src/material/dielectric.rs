use crate::hittable::HitRecord;
use crate::material::Material;
use crate::material::brdf;
use crate::ray::Ray;
use utils::Color;

use serde::{Deserialize, Serialize};
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Dielectric {
    ir: f32, // Index of refraction
}

impl Dielectric {
    pub fn new(index_of_refraction: f32) -> Dielectric {
        Dielectric {
            ir: index_of_refraction,
        }
    }

    fn reflectance(cosine: f32, ref_idx: f32) -> f32 {
        // Use Schlick's approximation for reflectance
        let mut r0 = (1.0 - ref_idx) / (1.0 + ref_idx);
        r0 = r0 * r0;
        r0 + (1.0 - r0) * f32::powf(1.0 - cosine, 5.0)
    }
}

impl Material for Dielectric {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let refraction_ratio = if rec.front_face {
            1.0 / self.ir
        } else {
            self.ir
        };

        let unit_direction = utils::unit_vector(r_in.direction());
        let cos_theta = f32::min(utils::dot(-unit_direction, rec.normal), 1.0);
        let sin_theta = f32::sqrt(1.0 - cos_theta * cos_theta);

        let cannot_refract = refraction_ratio * sin_theta > 1.0;
        let direction =
            if cannot_refract || Self::reflectance(cos_theta, refraction_ratio) > utils::random() {
                utils::reflect(unit_direction, rec.normal)
            } else {
                utils::refract(unit_direction, rec.normal, refraction_ratio)
            };

        *attenuation = Color::new(1.0, 1.0, 1.0);
        *scattered = Ray::new(rec.p, direction);
        true
    }
}

pub struct ComplexDielectric {
    pub ior: f32,
    pub roughness: f32,
    pub absorption: Option<Color>,
    pub thin: bool,
}

impl ComplexDielectric {
    pub fn new(ior: f32, roughness: f32, absorption: Option<Color>, thin: bool) -> Self {
        Self {
            ior,
            roughness,
            absorption,
            thin,
        }
    }
}

impl Material for ComplexDielectric {
    fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        attenuation: &mut Color,
        scattered: &mut Ray,
    ) -> bool {
        let normal = rec.normal;
        let view = -utils::unit_vector(r_in.direction());
        let n = if rec.front_face { normal } else { -normal };

        // Sample half vector from GGX VNDF
        let h = brdf::sample_vndf_ggx(view, self.roughness);
        let h = if utils::dot(h, n) < 0.0 { -h } else { h };

        let cos_theta = utils::dot(view, h).max(0.0);
        let f0 = Color::new(1.0, 1.0, 1.0) * (((1.0 - self.ior) / (1.0 + self.ior)).powi(2));
        let fresnel = brdf::fresnel_schlick(cos_theta, f0);

        // Decide between reflection and refraction
        let reflect = utils::random() < fresnel.x();

        let direction = if reflect {
            utils::reflect(view, h)
        } else {
            let eta = if rec.front_face || self.thin {
                1.0 / self.ior
            } else {
                self.ior
            };
            utils::refract(view, h, eta)
        };

        *scattered = Ray::new(rec.p, direction);

        // Attenuation for transmission (Beer’s Law)
        if reflect || self.thin {
            *attenuation = Color::new(1.0, 1.0, 1.0);
        } else if let Some(abs) = self.absorption {
            let distance = 1.0; // Or distance inside medium, if available
            *attenuation = Color::new(
                (-abs.x() * distance).exp(),
                (-abs.y() * distance).exp(),
                (-abs.z() * distance).exp(),
            );
        } else {
            *attenuation = Color::new(1.0, 1.0, 1.0);
        }

        true
    }
}
