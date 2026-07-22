use crate::aabb::AABB;
use crate::hittable::{Hit, HitRecord, Hittable};
use crate::material::Material;
use crate::primitives::triangle::triangle_hit;
use crate::ray::Ray;
use glam::Vec3A;
use std::sync::Arc;

pub struct Mesh {
    vertices: Vec<Vec3A>,
    indices: Vec<u32>,
    pub material: Arc<dyn Material>,
}
// `Mesh` is not re-exported at the crate root yet, so its constructors and
// accessors count as dead code from inside the crate.
#[allow(dead_code)]
impl Mesh {
    pub fn new(vertices: Vec<Vec3A>, indices: Vec<u32>, material: Arc<dyn Material>) -> Self {
        Self {
            vertices,
            indices,
            material,
        }
    }

    pub fn get_vertices(&self) -> &Vec<Vec3A> {
        &self.vertices
    }

    pub fn get_indices(&self) -> &Vec<u32> {
        &self.indices
    }
}

impl Hittable for Mesh {
    fn bounding_box(&self) -> Option<AABB> {
        let mut verts = self.vertices.iter();
        let first = *verts.next()?;
        let (mut min, mut max) = (first, first);
        for &v in verts {
            min = min.min(v);
            max = max.max(v);
        }
        // Pad degenerate axes so flat meshes survive the slab test, matching
        // `triangle_aabb`.
        const PAD: f32 = 1e-4;
        for a in 0..3 {
            if max[a] - min[a] < PAD {
                min[a] -= PAD;
                max[a] += PAD;
            }
        }
        Some(AABB::new(min, max))
    }
    fn hit(&self, r: &Ray, t_min: f32, t_max: f32) -> Option<Hit<'_>> {
        indexed_mesh_hit(r, &self.vertices, &self.indices, t_min, t_max).map(|rec| Hit {
            rec,
            mat: self.material.as_ref(),
        })
    }
}

fn indexed_mesh_hit(
    ray: &Ray,
    vertices: &[Vec3A],
    indices: &[u32],
    t_min: f32,
    t_max: f32,
) -> Option<HitRecord> {
    let mut best: Option<HitRecord> = None;
    let mut closest_so_far = t_max;

    for tri in indices.chunks_exact(3) {
        let v0 = vertices[tri[0] as usize];
        let v1 = vertices[tri[1] as usize];
        let v2 = vertices[tri[2] as usize];

        if let Some(rec) = triangle_hit(ray, v0, v1, v2, t_min, closest_so_far) {
            closest_so_far = rec.t;
            best = Some(rec);
        }
    }

    best
}
