use crate::aabb::{AABB, triangle_aabb};
use crate::hittable::{HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use rand::Rng;
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
    pub obj_cache: RwLock<Option<Arc<dyn Hittable>>>,
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
    fn bounding_box(&self) -> Option<AABB> {
        match &self.primitive {
            Primitive::Sphere { center, radius } => {
                let r_vec = Point3::new(*radius, *radius, *radius);
                Some(AABB::new(*center - r_vec, *center + r_vec))
            }

            Primitive::Triangle { v0, v1, v2 } => Some(triangle_aabb(*v0, *v1, *v2)),

            Primitive::Mesh { .. } | Primitive::Obj { .. } => {
                // These will be handled via BVH built at load time,
                // so we don't compute a bounding box here.
                None
            }
        }
    }
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
                // Step 1: Check cache
                {
                    let cache = self.obj_cache.read().unwrap();
                    if let Some(bvh) = &*cache {
                        return bvh.hit(r, t_min, t_max, rec);
                    }
                }

                // Step 2: Load the OBJ
                use std::path::Path;
                if !Path::new(path).exists() {
                    error!("OBJ file {} does not exist", path);
                    return false;
                }

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

                // Step 3: Convert to triangle Objects
                let vertices: Vec<Point3> =
                    obj.vertices.iter().map(|v| v.position.into()).collect();
                let indices: Vec<u32> = obj.indices.iter().map(|&i| i as u32).collect();

                let mut triangle_objs: Vec<Arc<dyn Hittable>> =
                    Vec::with_capacity(indices.len() / 3);

                for i in (0..indices.len()).step_by(3) {
                    let v0 = vertices[indices[i] as usize];
                    let v1 = vertices[indices[i + 1] as usize];
                    let v2 = vertices[indices[i + 2] as usize];

                    let tri = Arc::new(Object::new_triangle(v0, v1, v2, self.material.clone()));
                    triangle_objs.push(tri);
                }

                // Step 4: Build BVH
                let bvh = BVHNode::build(triangle_objs);

                // Step 5: Cache it
                {
                    let mut cache = self.obj_cache.write().unwrap();
                    *cache = Some(bvh.clone());
                }

                // Step 6: Intersect using the cached BVH
                bvh.hit(r, t_min, t_max, rec)
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

pub struct BVHNode {
    pub left: Arc<dyn Hittable>,
    pub right: Arc<dyn Hittable>,
    pub bbox: AABB,
}

impl BVHNode {
    pub fn build(mut objects: Vec<Arc<dyn Hittable>>) -> Arc<dyn Hittable> {
        let axis: i32 = rand::random_range(0..3);
        let comparator = match axis {
            0 => AABB::compare_x,
            1 => AABB::compare_y,
            _ => AABB::compare_z,
        };

        objects.sort_by(|a, b| comparator(a.bounding_box().unwrap(), b.bounding_box().unwrap()));

        let node: Arc<dyn Hittable> = match objects.len() {
            1 => objects[0].clone(),
            2 => {
                let left = objects[0].clone();
                let right = objects[1].clone();
                let bbox = AABB::surrounding_box(
                    left.bounding_box().unwrap(),
                    right.bounding_box().unwrap(),
                );
                Arc::new(BVHNode { left, right, bbox })
            }
            _ => {
                let mid = objects.len() / 2;
                let left = BVHNode::build(objects[..mid].to_vec());
                let right = BVHNode::build(objects[mid..].to_vec());
                let bbox = AABB::surrounding_box(
                    left.bounding_box().unwrap(),
                    right.bounding_box().unwrap(),
                );
                Arc::new(BVHNode { left, right, bbox })
            }
        };

        node
    }
}

impl Hittable for BVHNode {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool {
        if !self.bbox.hit(ray, t_min, t_max) {
            return false;
        }

        let mut temp_rec = HitRecord::default();
        let mut hit_anything = false;
        let mut closest_so_far = t_max;

        if self.left.hit(ray, t_min, closest_so_far, &mut temp_rec) {
            closest_so_far = temp_rec.t;
            *rec = temp_rec.clone();
            hit_anything = true;
        }

        if self.right.hit(ray, t_min, closest_so_far, &mut temp_rec) {
            *rec = temp_rec.clone();
            hit_anything = true;
        }

        hit_anything
    }

    fn bounding_box(&self) -> Option<AABB> {
        Some(self.bbox)
    }
}
