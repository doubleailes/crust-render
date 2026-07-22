use crate::hittable::HitRecord;
use crate::material::{Material, ScatterSample};
use crate::ray::Ray;
use glam::Vec3A;
use sampler::Sampler;

/// A purely emissive surface material. Emission is all it knows — the shape
/// of the light it belongs to lives in a `LightShape` on the light side
/// (`light.rs`), and the two are tied together by binding the same
/// `Arc<Emissive>` to both the scene geometry and the `AreaLight`.
#[derive(Debug, Clone)]
pub struct Emissive {
    color: Vec3A,
}

impl Emissive {
    pub fn new(color: Vec3A) -> Self {
        Emissive { color }
    }
    pub fn color(&self) -> Vec3A {
        self.color
    }
}

impl Material for Emissive {
    fn emitted(&self) -> Vec3A {
        self.color
    }

    // Emissive surfaces do not scatter.
    fn scatter_importance(
        &self,
        _r_in: &Ray,
        _rec: &HitRecord,
        _sampler: &mut dyn Sampler,
    ) -> Option<ScatterSample> {
        None
    }
}
