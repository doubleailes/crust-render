use crate::hittable::{HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use tracing::error;
use utils::Point3;

use obj::{Obj, load_obj};

/// Geometric primitives that can be serialized.
#[derive(Debug, Serialize, Deserialize)]
pub enum Primitive {
    Sphere {
        center: Point3,
        radius: f32,
    },
    Triangle {
        v0: Point3,
        v1: Point3,
        v2: Point3,
    },
    Mesh {
        vertices: Vec<Point3>,
        indices: Vec<u32>,
    },
    Obj {
        path: String,
    },
}

impl Primitive {
    pub fn new_sphere(center: Point3, radius: f32) -> Self {
        Self::Sphere { center, radius }
    }

    pub fn new_triangle(v0: Point3, v1: Point3, v2: Point3) -> Self {
        Self::Triangle { v0, v1, v2 }
    }
    pub fn new_mesh(vertices: Vec<Point3>, indices: Vec<u32>) -> Self {
        Self::Mesh { vertices, indices }
    }
    pub fn new_obj(path: String) -> Self {
        Self::Obj { path }
    }
}

pub struct Object {
    pub primitive: Primitive,
    pub material: Arc<dyn Material>,
    pub obj_cache: RwLock<Option<(Vec<Point3>, Vec<u32>)>>,
}

impl Object {
    pub fn new_sphere(center: Point3, radius: f32, material: Arc<dyn Material>) -> Self {
        Self {
            primitive: Primitive::new_sphere(center, radius),
            material,
            obj_cache: RwLock::new(None),
        }
    }

    pub fn new_triangle(v0: Point3, v1: Point3, v2: Point3, material: Arc<dyn Material>) -> Self {
        Self {
            primitive: Primitive::new_triangle(v0, v1, v2),
            material,
            obj_cache: RwLock::new(None),
        }
    }

    pub fn new_mesh(vertices: Vec<Point3>, indices: Vec<u32>, material: Arc<dyn Material>) -> Self {
        Self {
            primitive: Primitive::Mesh { vertices, indices },
            material,
            obj_cache: RwLock::new(None),
        }
    }

    pub fn new_obj(path: String, material: Arc<dyn Material>) -> Self {
        Self {
            primitive: Primitive::Obj { path },
            material,
            obj_cache: RwLock::new(None),
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
                let outward_normal = (rec.p - *center) / *radius;
                rec.set_face_normal(r, outward_normal);
                rec.mat = Some(self.material.clone());
                true
            }

            Primitive::Triangle { v0, v1, v2 } => {
                triangle_hit(r, *v0, *v1, *v2, t_min, t_max, rec, &self.material)
            }

            Primitive::Mesh { vertices, indices } => {
                indexed_mesh_hit(r, vertices, indices, t_min, t_max, rec, &self.material)
            }

            Primitive::Obj { path } => {
                // Try reading cache first
                {
                    let cache = self.obj_cache.read().unwrap();
                    if let Some((vertices, indices)) = &*cache {
                        return indexed_mesh_hit(
                            r,
                            vertices,
                            indices,
                            t_min,
                            t_max,
                            rec,
                            &self.material,
                        );
                    }
                }

                if !Path::new(path).exists() {
                    error!("OBJ file {} does not exist", path);
                    return false;
                }

                // Upgrade to write lock and populate cache
                let mut cache = self.obj_cache.write().unwrap();
                if cache.is_none() {
                    let file = File::open(path).unwrap_or_else(|e| {
                        error!("Failed to open OBJ file {}: {}", path, e);
                        panic!("Cannot open OBJ file");
                    });

                    let input = BufReader::new(file);
                    let obj: Obj = match load_obj(input) {
                        Ok(o) => o,
                        Err(e) => {
                            error!("Failed to parse OBJ file {}: {}", path, e);
                            return false;
                        }
                    };

                    let vertices: Vec<Point3> =
                        obj.vertices.iter().map(|v| v.position.into()).collect();
                    let indices: Vec<u32> = obj.indices.iter().map(|&i| i as u32).collect();
                    *cache = Some((vertices, indices));
                }

                // Use cached data
                if let Some((vertices, indices)) = &*cache {
                    indexed_mesh_hit(r, vertices, indices, t_min, t_max, rec, &self.material)
                } else {
                    false
                }
            }
        }
    }
}

fn triangle_hit(
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

fn indexed_mesh_hit(
    ray: &Ray,
    vertices: &[Point3],
    indices: &[u32],
    t_min: f32,
    t_max: f32,
    rec: &mut HitRecord,
    material: &Arc<dyn Material>,
) -> bool {
    let mut hit_anything = false;
    let mut closest_so_far = t_max;

    for i in (0..indices.len()).step_by(3) {
        if i + 2 >= indices.len() {
            break;
        }

        let v0 = vertices[indices[i] as usize];
        let v1 = vertices[indices[i + 1] as usize];
        let v2 = vertices[indices[i + 2] as usize];

        let mut temp_rec = HitRecord::default();

        if triangle_hit(
            ray,
            v0,
            v1,
            v2,
            t_min,
            closest_so_far,
            &mut temp_rec,
            material,
        ) {
            closest_so_far = temp_rec.t;
            *rec = temp_rec;
            hit_anything = true;
        }
    }

    hit_anything
}
