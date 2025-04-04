use crate::hittable::{HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use std::sync::Arc;
use utils::Point3;
use serde::{Serialize, Deserialize};

/// Geometric primitives that can be serialized.
#[derive(Debug, Serialize, Deserialize)]
pub enum Primitive {
    Sphere { center: Point3, radius: f32 },
    // Add other primitives here, e.g. Plane, Triangle...
}
impl Primitive{
    pub fn new_sphere(center: Point3, radius: f32) -> Self {
        Primitive::Sphere { center, radius }
    }
}

pub struct Object {
    pub primitive: Primitive,
    pub material: Arc<dyn Material>,
}

impl Object {
    pub fn new_sphere(center: Point3, radius: f32, material: Arc<dyn Material>) -> Self {
        Object {
            primitive: Primitive::new_sphere(center, radius),
            material,
        }
    }
}

impl Hittable for Object {
    fn hit(&self, r: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool {
        match &self.primitive {
            Primitive::Sphere { center, radius } => {
                let oc = r.origin() - *center;
                let a = r.direction().length_squared();
                let half_b = utils::dot(oc, r.direction());
                let c = oc.length_squared() - radius * radius;
                let discriminant = half_b * half_b - a * c;

                if discriminant < 0.0 {
                    return false;
                }

                let sqrt_d = f32::sqrt(discriminant);

                let mut root = (-half_b - sqrt_d) / a;
                if root <= t_min || t_max <= root {
                    root = (-half_b + sqrt_d) / a;
                    if root <= t_min || t_max <= root {
                        return false;
                    }
                }

                rec.t = root;
                rec.p = r.at(rec.t);
                let outward_normal = (rec.p - *center) / *radius;
                rec.set_face_normal(r, outward_normal);
                rec.mat = Some(self.material.clone());
                true
            }
        }
    }
}
