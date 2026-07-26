use crate::aabb::{AABB, triangle_aabb};
use crate::hittable::{Hit, HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3A;
use std::sync::Arc;

pub struct SmoothTriangle {
    pub v0: Vec3A,
    pub v1: Vec3A,
    pub v2: Vec3A,
    pub n0: Vec3A,
    pub n1: Vec3A,
    pub n2: Vec3A,
    pub material: Arc<dyn Material>,
}

impl SmoothTriangle {
    pub fn new(
        v0: Vec3A,
        v1: Vec3A,
        v2: Vec3A,
        n0: Vec3A,
        n1: Vec3A,
        n2: Vec3A,
        material: Arc<dyn Material>,
    ) -> Self {
        Self {
            v0,
            v1,
            v2,
            n0,
            n1,
            n2,
            material,
        }
    }
}

impl Hittable for SmoothTriangle {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<Hit<'_>> {
        let (t, u, v) =
            crate::primitives::triangle::triangle_intersect(ray, self.v0, self.v1, self.v2, t_min, t_max)?;

        let mut rec = HitRecord::new();
        rec.t = t;
        rec.p = ray.at(t);

        // Interpolate the shading normal from the watertight barycentrics
        // (u weights v1, v weights v2).
        let w = 1.0 - u - v;
        let interpolated_normal = (self.n0 * w + self.n1 * u + self.n2 * v).normalize();
        rec.set_face_normal(ray, interpolated_normal);

        Some(Hit {
            rec,
            mat: self.material.as_ref(),
        })
    }

    fn bounding_box(&self) -> Option<AABB> {
        Some(triangle_aabb(self.v0, self.v1, self.v2))
    }
}
