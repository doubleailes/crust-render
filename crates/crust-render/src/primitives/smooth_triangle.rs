use crate::aabb::{AABB, triangle_aabb};
use crate::hittable::{HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3;
use std::sync::Arc;

pub struct SmoothTriangle {
    pub v0: Vec3,
    pub v1: Vec3,
    pub v2: Vec3,
    pub n0: Vec3,
    pub n1: Vec3,
    pub n2: Vec3,
    pub material: Arc<dyn Material>,
}

impl SmoothTriangle {
    pub fn new(
        v0: Vec3,
        v1: Vec3,
        v2: Vec3,
        n0: Vec3,
        n1: Vec3,
        n2: Vec3,
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
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool {
        let edge1 = self.v1 - self.v0;
        let edge2 = self.v2 - self.v0;
        let h = ray.direction().cross(edge2);
        let a = edge1.dot(h);

        if a.abs() < 1e-6 {
            return false;
        }

        let f = 1.0 / a;
        let s = ray.origin() - self.v0;
        let u = f * s.dot(h);
        if !(0.0..=1.0).contains(&u) {
            return false;
        }

        let q = s.cross(edge1);
        let v = f * ray.direction().dot(q);
        if v < 0.0 || u + v > 1.0 {
            return false;
        }

        let t = f * edge2.dot(q);
        if t < t_min || t > t_max {
            return false;
        }

        rec.t = t;
        rec.p = ray.at(t);

        // Interpolate normal using barycentric weights
        let w = 1.0 - u - v;
        let interpolated_normal = (self.n0 * w + self.n1 * u + self.n2 * v).normalize();
        rec.set_face_normal(ray, interpolated_normal);
        rec.mat = Some(self.material.clone());

        true
    }

    fn bounding_box(&self) -> Option<AABB> {
        Some(triangle_aabb(self.v0, self.v1, self.v2))
    }
}
