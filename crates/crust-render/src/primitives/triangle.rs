use crate::aabb::{AABB, triangle_aabb};
use crate::hittable::{HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3;
use std::sync::Arc;

pub struct Triangle {
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    material: Arc<dyn Material>,
}
impl Triangle {
    pub fn new(v0: Vec3, v1: Vec3, v2: Vec3, material: Arc<dyn Material>) -> Self {
        Self {
            v0,
            v1,
            v2,
            material,
        }
    }
}

impl Hittable for Triangle {
    fn bounding_box(&self) -> Option<AABB> {
        Some(triangle_aabb(self.v0, self.v1, self.v2))
    }
    fn hit(&self, r: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool {
        triangle_hit(
            r,
            self.v0,
            self.v1,
            self.v2,
            t_min,
            t_max,
            rec,
            &self.material,
        )
    }
}

pub(crate) fn triangle_hit(
    ray: &Ray,
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    t_min: f32,
    t_max: f32,
    rec: &mut HitRecord,
    material: &Arc<dyn Material>,
) -> bool {
    let edge1 = v1 - v0;
    let edge2 = v2 - v0;
    let h = ray.direction().cross(edge2);
    let a = edge1.dot(h);

    if a.abs() < 1e-6 {
        return false;
    }

    let f = 1.0 / a;
    let s = ray.origin() - v0;
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
    let normal = edge1.cross(edge2).normalize();
    rec.set_face_normal(ray, normal);
    rec.mat = Some(material.clone());
    true
}
