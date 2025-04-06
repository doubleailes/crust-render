use crate::aabb::{AABB, triangle_aabb};
use crate::hittable::{HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use std::sync::Arc;
use utils::Point3;

pub struct Triangle {
    v0: Point3,
    v1: Point3,
    v2: Point3,
    material: Arc<dyn Material>,
}
impl Triangle {
    pub fn new(v0: Point3, v1: Point3, v2: Point3, material: Arc<dyn Material>) -> Self {
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
    v0: Point3,
    v1: Point3,
    v2: Point3,
    t_min: f32,
    t_max: f32,
    rec: &mut HitRecord,
    material: &Arc<dyn Material>,
) -> bool {
    let edge1 = v1 - v0;
    let edge2 = v2 - v0;
    let h = utils::cross(ray.direction(), edge2);
    let a = utils::dot(edge1, h);

    if a.abs() < 1e-6 {
        return false;
    }

    let f = 1.0 / a;
    let s = ray.origin() - v0;
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
    let normal = utils::cross(edge1, edge2).unit_vector();
    rec.set_face_normal(ray, normal);
    rec.mat = Some(material.clone());
    true
}
