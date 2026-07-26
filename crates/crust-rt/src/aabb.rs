use crate::ray::Ray;
use glam::Vec3A;

#[derive(Debug, Clone, Copy)]
pub struct AABB {
    pub minimum: Vec3A,
    pub maximum: Vec3A,
}

impl AABB {
    pub fn new(minimum: Vec3A, maximum: Vec3A) -> Self {
        AABB { minimum, maximum }
    }

    pub fn surrounding_box(box0: AABB, box1: AABB) -> AABB {
        AABB {
            minimum: box0.minimum.min(box1.minimum),
            maximum: box0.maximum.max(box1.maximum),
        }
    }

    /// Scalar slab test. The BVH traversal uses its own 4-wide variant;
    /// this is for callers working with a single box.
    pub fn hit(&self, ray: &Ray, mut t_min: f32, mut t_max: f32) -> bool {
        for a in 0..3 {
            let inv_d = 1.0 / ray.dir[a];
            let mut t0 = (self.minimum[a] - ray.origin[a]) * inv_d;
            let mut t1 = (self.maximum[a] - ray.origin[a]) * inv_d;

            if inv_d < 0.0 {
                std::mem::swap(&mut t0, &mut t1);
            }

            t_min = t_min.max(t0);
            t_max = t_max.min(t1);

            if t_max <= t_min {
                return false;
            }
        }
        true
    }
}

/// Bounds of a triangle, padded on degenerate axes so axis-aligned
/// geometry survives the slab test (which rejects zero-thickness boxes).
pub fn triangle_aabb(v0: Vec3A, v1: Vec3A, v2: Vec3A) -> AABB {
    let mut min = v0.min(v1).min(v2);
    let mut max = v0.max(v1).max(v2);
    const PAD: f32 = 1e-4;
    for a in 0..3 {
        if max[a] - min[a] < PAD {
            min[a] -= PAD;
            max[a] += PAD;
        }
    }
    AABB::new(min, max)
}
