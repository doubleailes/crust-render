use crate::hittable::{Hittable, HitRecord};
use crate::material::Material;
use crate::ray::Ray;
use crate::aabb::{AABB, triangle_aabb};
use std::sync::Arc;
use utils::{Vec3, Point3};

pub struct SmoothTriangle {
    pub v0: Point3,
    pub v1: Point3,
    pub v2: Point3,
    pub n0: Vec3,
    pub n1: Vec3,
    pub n2: Vec3,
    pub material: Arc<dyn Material>,
}

impl SmoothTriangle {
    pub fn new(
        v0: Point3,
        v1: Point3,
        v2: Point3,
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
        let h = utils::cross(ray.direction(), edge2);
        let a = utils::dot(edge1, h);

        if a.abs() < 1e-6 {
            return false;
        }

        let f = 1.0 / a;
        let s = ray.origin() - self.v0;
        let u = f * utils::dot(s, h);
        if !(0.0..=1.0).contains(&u) {
            return false;
        }

        let q = utils::cross(s, edge1);
        let v = f * utils::dot(ray.direction(), q);
        if v < 0.0 || u + v > 1.0 {
            return false;
        }

        let t = f * utils::dot(edge2, q);
        if t < t_min || t > t_max {
            return false;
        }

        rec.t = t;
        rec.p = ray.at(t);

        // Interpolate normal using barycentric weights
        let w = 1.0 - u - v;
        let interpolated_normal = (self.n0 * w + self.n1 * u + self.n2 * v).unit_vector();
        rec.set_face_normal(ray, interpolated_normal);
        rec.mat = Some(self.material.clone());

        true
    }

    fn bounding_box(&self) -> Option<AABB> {
        Some(triangle_aabb(self.v0, self.v1, self.v2))
    }
}