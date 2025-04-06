use crate::aabb::AABB;
use crate::hittable::HitRecord;
use crate::hittable::Hittable;
use crate::material::Material;
use crate::primitives::triangle::triangle_hit;
use crate::ray::Ray;
use std::sync::Arc;
use utils::Point3;

pub struct Mesh {
    vertices: Vec<Point3>,
    indices: Vec<u32>,
    pub material: Arc<dyn Material>,
}
impl Mesh {
    #[allow(dead_code)]
    pub fn new(vertices: Vec<Point3>, indices: Vec<u32>, material: Arc<dyn Material>) -> Self {
        Self {
            vertices,
            indices,
            material,
        }
    }

    pub fn get_vertices(&self) -> &Vec<Point3> {
        &self.vertices
    }

    pub fn get_indices(&self) -> &Vec<u32> {
        &self.indices
    }
}

impl Hittable for Mesh {
    fn bounding_box(&self) -> Option<AABB> {
        // These will be handled via BVH built at load time,
        // so we don't compute a bounding box here.
        None
    }
    fn hit(&self, r: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool {
        indexed_mesh_hit(
            r,
            self.get_vertices(),
            self.get_indices(),
            t_min,
            t_max,
            rec,
            &self.material,
        )
    }
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
