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
    Triangle { v0: Point3, v1: Point3, v2: Point3 },
}
impl Primitive {
    pub fn new_sphere(center: Point3, radius: f32) -> Self {
        Primitive::Sphere { center, radius }
    }
    pub fn new_triangle(v0: Point3, v1: Point3, v2: Point3) -> Self {
        Primitive::Triangle { v0, v1, v2 }
    }
    pub fn rotate(&self, r_x: f32, r_y: f32, r_z: f32) -> Self {
        match self {
            Primitive::Sphere { center, radius } => {
                Primitive::Sphere {
                    center: center.rotate(r_x, r_y, r_z),
                    radius: *radius,
                }
            }
            Primitive::Triangle { v0, v1, v2 } => {
                Primitive::Triangle {
                    v0: v0.rotate(r_x, r_y, r_z),
                    v1: v1.rotate(r_x, r_y, r_z),
                    v2: v2.rotate(r_x, r_y, r_z),
                }
            }
        }
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
    pub fn new_triangle(v0: Point3, v1: Point3, v2: Point3,material:Arc<dyn Material>) -> Self {
        Object {
            primitive: Primitive::new_triangle(v0, v1, v2),
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
            },
            Primitive::Triangle { v0, v1, v2 } => {
                let edge1 = *v1 - *v0;
                let edge2 = *v2 - *v0;
                let h = utils::cross(r.direction(), edge2);
                let a = utils::dot(edge1, h);
                if a.abs() < 1e-6 {
                    return false;
                }
                let f = 1.0 / a;
                let s = r.origin() - *v0;
                let u = f * utils::dot(s, h);
                if u < 0.0 || u > 1.0 {
                    return false;
                }
                let q = utils::cross(s, edge1);
                let v = f * utils::dot(r.direction(), q);
                if v < 0.0 || u + v > 1.0 {
                    return false;
                }
            
                let t = f * utils::dot(edge2, q);
                if t < t_min || t > t_max {
                    return false;
                }
            
                rec.t = t;
                rec.p = r.at(t);
                let normal = utils::cross(edge1, edge2).unit_vector();
                rec.set_face_normal(r, normal);
                rec.mat = Some(self.material.clone());
                true
            }
        }
    }
}
