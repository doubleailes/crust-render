use utils::Vec3;
use crate::ray::Ray;

#[derive(Debug, Clone, Copy)]
pub struct AABB {
    pub minimum: Vec3,
    pub maximum: Vec3,
}

impl AABB {
    pub fn new(minimum: Vec3, maximum: Vec3) -> Self {
        AABB { minimum, maximum }
    }

    pub fn surrounding_box(box0: AABB, box1: AABB) -> AABB {
        let small = Vec3::new(
            box0.minimum.x().min(box1.minimum.x()),
            box0.minimum.y().min(box1.minimum.y()),
            box0.minimum.z().min(box1.minimum.z()),
        );
        let big = Vec3::new(
            box0.maximum.x().max(box1.maximum.x()),
            box0.maximum.y().max(box1.maximum.y()),
            box0.maximum.z().max(box1.maximum.z()),
        );
        AABB { minimum: small, maximum: big }
    }

    pub fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        for a in 0..3 {
            let inv_d = 1.0 / ray.direction()[a];
            let mut t0 = (self.minimum[a] - ray.origin()[a]) * inv_d;
            let mut t1 = (self.maximum[a] - ray.origin()[a]) * inv_d;

            if inv_d < 0.0 {
                std::mem::swap(&mut t0, &mut t1);
            }

            let t_min = t_min.max(t0);
            let t_max = t_max.min(t1);

            if t_max <= t_min {
                return false;
            }
        }
        true
    }

    pub fn compare_x(a: AABB, b: AABB) -> std::cmp::Ordering {
        a.minimum.x().partial_cmp(&b.minimum.x()).unwrap()
    }

    pub fn compare_y(a: AABB, b: AABB) -> std::cmp::Ordering {
        a.minimum.y().partial_cmp(&b.minimum.y()).unwrap()
    }

    pub fn compare_z(a: AABB, b: AABB) -> std::cmp::Ordering {
        a.minimum.z().partial_cmp(&b.minimum.z()).unwrap()
    }
}

pub fn triangle_aabb(v0: Vec3, v1: Vec3, v2: Vec3) -> AABB {
    let min = Vec3::new(
        v0[0].min(v1[0]).min(v2[0]),
        v0[1].min(v1[1]).min(v2[1]),
        v0[2].min(v1[2]).min(v2[2]),
    );
    let max = Vec3::new(
        v0[0].max(v1[0]).max(v2[0]),
        v0[1].max(v1[1]).max(v2[1]),
        v0[2].max(v1[2]).max(v2[2]),
    );
    AABB::new(min, max)
}