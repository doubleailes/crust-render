use crate::hittable::{HitRecord, Hittable};
use std::sync::Arc;
use crate::aabb::AABB;
use crate::ray::Ray;
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
