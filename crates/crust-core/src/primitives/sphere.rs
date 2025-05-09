use crate::aabb::AABB;
use crate::hittable::HitRecord;
use crate::hittable::Hittable;
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3A;
use std::sync::Arc;

pub struct Sphere {
    center: Vec3A,
    radius: f32,
    material: Arc<dyn Material>,
}
impl Sphere {
    pub fn new(center: Vec3A, radius: f32, material: Arc<dyn Material>) -> Self {
        Self {
            center,
            radius,
            material,
        }
    }
}
impl Hittable for Sphere {
    fn bounding_box(&self) -> Option<AABB> {
        Some(AABB::new(
            self.center - Vec3A::new(self.radius, self.radius, self.radius),
            self.center + Vec3A::new(self.radius, self.radius, self.radius),
        ))
    }
    fn hit(&self, r: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool {
        let oc = r.origin() - self.center;
        let a = r.direction().length_squared();
        let half_b = oc.dot(r.direction());
        let c = oc.length_squared() - self.radius * self.radius;
        let discriminant = half_b * half_b - a * c;

        if discriminant < 0.0 {
            return false;
        }

        let sqrt_d = discriminant.sqrt();
        let mut root = (-half_b - sqrt_d) / a;

        if root <= t_min || root >= t_max {
            root = (-half_b + sqrt_d) / a;
            if root <= t_min || root >= t_max {
                return false;
            }
        }

        rec.t = root;
        rec.p = r.at(root);
        let outward_normal = (rec.p - self.center) / self.radius;
        rec.set_face_normal(r, outward_normal);
        rec.mat = Some(self.material.clone());
        true
    }
}
