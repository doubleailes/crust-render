use crate::aabb::AABB;
use crate::hittable::{HitRecord, Hittable};
use crate::ray::Ray;
use std::sync::Arc;
use utils::{Mat4,Point3};

pub struct Instance {
    pub object: Arc<dyn Hittable>,
    pub transform: Mat4,
    pub inverse_transform: Mat4,
}

impl Hittable for Instance {
    fn hit(&self, r: &Ray, t_min: f32, t_max: f32, rec: &mut HitRecord) -> bool {
        let transformed_ray = Ray::new(
            self.inverse_transform.transform_point(r.origin()),
            self.inverse_transform.transform_direction(r.direction()),
        );

        let mut temp_rec = HitRecord::default();

        if !self
            .object
            .hit(&transformed_ray, t_min, t_max, &mut temp_rec)
        {
            return false;
        }

        temp_rec.p = self.transform.transform_point(temp_rec.p);
        temp_rec.set_face_normal(
            r,
            self.transform
                .transform_direction(temp_rec.normal)
                .unit_vector(),
        );

        *rec = temp_rec;
        true
    }

    fn bounding_box(&self) -> Option<AABB> {
        if let Some(bbox) = self.object.bounding_box() {
            // Transform the bounding box corners
            let min = bbox.minimum;
            let max = bbox.maximum;
        
            // Transform all 8 corners of the box and find the new bounds
            let corners = [
                Point3::new(min.x(), min.y(), min.z()),
                Point3::new(max.x(), min.y(), min.z()),
                Point3::new(min.x(), max.y(), min.z()),
                Point3::new(min.x(), min.y(), max.z()),
                Point3::new(max.x(), max.y(), min.z()),
                Point3::new(max.x(), min.y(), max.z()),
                Point3::new(min.x(), max.y(), max.z()),
                Point3::new(max.x(), max.y(), max.z()),
            ];
        
            let mut new_min = self.transform.transform_point(corners[0]);
            let mut new_max = new_min;
        
            for i in 1..8 {
                let p = self.transform.transform_point(corners[i]);
                new_min = Point3::new(
                    new_min.x().min(p.x()),
                    new_min.y().min(p.y()),
                    new_min.z().min(p.z()),
                );
                new_max = Point3::new(
                    new_max.x().max(p.x()),
                    new_max.y().max(p.y()),
                    new_max.z().max(p.z()),
                );
            }
        
            Some(AABB::new(new_min, new_max))
        } else {
            None
        }
    }
}
