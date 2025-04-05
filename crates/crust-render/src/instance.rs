use crate::aabb::AABB;
use crate::hittable::{HitRecord, Hittable};
use crate::ray::Ray;
use std::sync::Arc;
use utils::Mat4; // Adjust based on your math module

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
        self.object.bounding_box() // not transformed — OK for now
    }
}
