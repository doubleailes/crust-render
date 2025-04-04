mod material;
pub use material::Material;
mod lambert;
pub use lambert::Lambertian;
mod metal;
pub use metal::Metal;
mod dielectric;
pub use dielectric::{ComplexDielectric, Dielectric};
mod blinn_phong;
pub use blinn_phong::BlinnPhong;
mod cook_torrance;
pub use cook_torrance::CookTorrance;
mod emissive;
pub use emissive::Emissive;
mod brdf;
pub use brdf::{fresnel_schlick, geometry_schlick_ggx, pdf_vndf_ggx, sample_vndf_ggx};
mod disney;
pub use disney::Disney;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub enum MaterialType {
    Lambertian(Lambertian),
    Metal(Metal),
    Dielectric(Dielectric),
    BlinnPhong(BlinnPhong),
    CookTorrance(CookTorrance),
    Emissive(Emissive),
    Disney(Disney),
}
use std::sync::Arc;

impl MaterialType {
    pub fn get_material(&self) -> Arc<dyn Material> {
        match self {
            MaterialType::Lambertian(m) => Arc::new((*m).clone()),
            MaterialType::Metal(m) => Arc::new((*m).clone()),
            MaterialType::Dielectric(m) => Arc::new((*m).clone()),
            MaterialType::BlinnPhong(m) => Arc::new((*m).clone()),
            MaterialType::CookTorrance(m) => Arc::new((*m).clone()),
            MaterialType::Emissive(m) => Arc::new((*m).clone()),
            MaterialType::Disney(m) => Arc::new((*m).clone()),
        }
    }
    pub fn is_emissive(&self) -> bool {
        match self {
            MaterialType::Emissive(_) => true,
            _ => false,
        }
    }
    pub fn get_emissive(&self) -> Option<Emissive> {
        match self {
            MaterialType::Emissive(m) => Some(m.clone()),
            _ => None,
        }
    }
}
