//! Instanced geometry: one shared object (typically a mesh's triangle BVH
//! in its local space) placed in the world by a transform, Embree-style.
//! N placements of the same mesh share one `Arc` — one copy of the
//! triangles and one acceleration structure — instead of N world-baked
//! copies. Rays are transformed into local space (direction left
//! unnormalized so `t` values carry over unchanged), hits are mapped back
//! with the inverse-transpose for normals.
//!
//! An instance may also *move*: with a second end-of-shutter transform the
//! placement is interpolated linearly (per matrix element) at the ray's
//! shutter time — transform motion blur. Linear matrix interpolation keeps
//! every interpolated point inside the convex hull of its endpoint
//! positions, so the union of the two endpoint bounding boxes is a
//! conservative bound over the whole shutter.

use crate::aabb::AABB;
use crate::hittable::{Hit, Hittable};
use crate::ray::Ray;
use glam::{Affine3A, Mat3A, Vec3A};
use std::sync::Arc;

pub struct Instance {
    object: Arc<dyn Hittable>,
    /// World-to-local at shutter time 0 (cached inverse).
    w2l: Affine3A,
    /// Normal transform at time 0: inverse-transpose of the linear part.
    normal_mat: Mat3A,
    /// Local-to-world at shutter time 0.
    l2w: Affine3A,
    /// Local-to-world at shutter time 1, when the instance moves.
    l2w_end: Option<Affine3A>,
    /// World bounds over the whole shutter interval.
    bbox: Option<AABB>,
}

/// Element-wise linear interpolation of two affine transforms.
fn lerp_affine(a: &Affine3A, b: &Affine3A, t: f32) -> Affine3A {
    Affine3A {
        matrix3: Mat3A::from_cols(
            a.matrix3.x_axis.lerp(b.matrix3.x_axis, t),
            a.matrix3.y_axis.lerp(b.matrix3.y_axis, t),
            a.matrix3.z_axis.lerp(b.matrix3.z_axis, t),
        ),
        translation: a.translation.lerp(b.translation, t),
    }
}

/// World-space box of `local` under `m`: transform the 8 corners and take
/// their bounds, padding degenerate axes like `triangle_aabb` does so flat
/// geometry survives the slab test.
fn transformed_aabb(local: &AABB, m: &Affine3A) -> AABB {
    let mut min = Vec3A::splat(f32::INFINITY);
    let mut max = Vec3A::splat(f32::NEG_INFINITY);
    for i in 0..8 {
        let corner = Vec3A::new(
            if i & 1 == 0 { local.minimum.x } else { local.maximum.x },
            if i & 2 == 0 { local.minimum.y } else { local.maximum.y },
            if i & 4 == 0 { local.minimum.z } else { local.maximum.z },
        );
        let p = m.transform_point3a(corner);
        min = min.min(p);
        max = max.max(p);
    }
    const PAD: f32 = 1e-4;
    for a in 0..3 {
        if max[a] - min[a] < PAD {
            min[a] -= PAD;
            max[a] += PAD;
        }
    }
    AABB::new(min, max)
}

impl Instance {
    /// Places `object` in the world by `l2w`. The transform must be
    /// invertible — the caller is expected to have checked its determinant.
    pub fn new(object: Arc<dyn Hittable>, l2w: Affine3A) -> Self {
        let w2l = l2w.inverse();
        // Normals transform by the inverse-transpose of the linear part.
        let normal_mat = w2l.matrix3.transpose();
        let bbox = object.bounding_box().map(|b| transformed_aabb(&b, &l2w));
        Instance {
            object,
            w2l,
            normal_mat,
            l2w,
            l2w_end: None,
            bbox,
        }
    }

    /// Adds transform motion blur: the placement interpolates linearly from
    /// `l2w` (time 0) to `l2w_end` (time 1) at each ray's shutter time.
    pub fn with_motion(mut self, l2w_end: Affine3A) -> Self {
        self.bbox = self
            .object
            .bounding_box()
            .map(|b| AABB::surrounding_box(
                transformed_aabb(&b, &self.l2w),
                transformed_aabb(&b, &l2w_end),
            ));
        self.l2w_end = Some(l2w_end);
        self
    }

    /// World-to-local and normal transform at the ray's shutter time.
    fn transforms_at(&self, time: f32) -> (Affine3A, Mat3A) {
        match &self.l2w_end {
            Some(end) if time > 0.0 => {
                let w2l = lerp_affine(&self.l2w, end, time).inverse();
                (w2l, w2l.matrix3.transpose())
            }
            _ => (self.w2l, self.normal_mat),
        }
    }

    fn to_local(&self, ray: &Ray, w2l: &Affine3A) -> Ray {
        ray.transformed(
            w2l.transform_point3a(ray.origin()),
            // Unnormalized on purpose: local t == world t.
            w2l.transform_vector3a(ray.direction()),
        )
    }
}

impl Hittable for Instance {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<Hit<'_>> {
        let (w2l, normal_mat) = self.transforms_at(ray.time());
        let local_ray = self.to_local(ray, &w2l);
        let mut hit = self.object.hit(&local_ray, t_min, t_max)?;

        // Same t on the world ray (direction was not renormalized), so the
        // world hit point comes from the world ray — no round trip through
        // the transform.
        hit.rec.p = ray.at(hit.rec.t);
        // Recover the geometric outward normal (set_face_normal may have
        // flipped it against the local ray), map it with the
        // inverse-transpose, and re-orient against the world ray.
        let outward = if hit.rec.front_face {
            hit.rec.normal
        } else {
            -hit.rec.normal
        };
        let world_normal = (normal_mat * outward).normalize();
        hit.rec.set_face_normal(ray, world_normal);
        Some(hit)
    }

    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        let (w2l, _) = self.transforms_at(ray.time());
        self.object.hit_any(&self.to_local(ray, &w2l), t_min, t_max)
    }

    fn bounding_box(&self) -> Option<AABB> {
        self.bbox
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::OpenPBR;
    use crate::primitives::{Sphere, Triangle};
    use glam::Mat4;

    fn unit_sphere() -> Arc<dyn Hittable> {
        let mat = Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5)));
        Arc::new(Sphere::new(Vec3A::ZERO, 1.0, mat))
    }

    #[test]
    fn translated_instance_matches_baked() {
        let inst = Instance::new(
            unit_sphere(),
            Affine3A::from_translation(glam::Vec3::new(3.0, 0.0, 0.0)),
        );
        let ray = Ray::new(Vec3A::new(3.0, 0.0, -5.0), Vec3A::Z);
        let hit = inst.hit(&ray, 0.001, f32::INFINITY).expect("hit");
        assert!((hit.rec.t - 4.0).abs() < 1e-4);
        assert!(hit.rec.normal.abs_diff_eq(-Vec3A::Z, 1e-4));
        assert!(inst.hit_any(&ray, 0.001, f32::INFINITY));
        assert!(!inst.hit_any(&ray, 0.001, 3.9));
    }

    #[test]
    fn nonuniform_scale_transforms_normals_correctly() {
        // Sphere scaled 2x in X: at the +Y pole the surface is still
        // perpendicular to Y, and the naive (non inverse-transpose) mapping
        // would agree — so probe an oblique point instead.
        let inst = Instance::new(
            unit_sphere(),
            Affine3A::from_scale(glam::Vec3::new(2.0, 1.0, 1.0)),
        );
        // Hit the ellipsoid straight down above x=1 (local x=0.5).
        let ray = Ray::new(Vec3A::new(1.0, 5.0, 0.0), -Vec3A::Y);
        let hit = inst.hit(&ray, 0.001, f32::INFINITY).expect("hit");
        // Implicit ellipsoid (x/2)^2 + y^2 + z^2 = 1: gradient at
        // (1, sqrt(3)/2, 0) is (x/2, 2y, 2z) ∝ (0.5, sqrt(3), 0).
        let expected = Vec3A::new(0.5, 3.0f32.sqrt(), 0.0).normalize();
        assert!(
            hit.rec.normal.abs_diff_eq(expected, 1e-3),
            "normal {:?} != expected {:?}",
            hit.rec.normal,
            expected
        );
        // And the hit point sits on the ellipsoid.
        let p = hit.rec.p;
        let f = (p.x / 2.0).powi(2) + p.y * p.y + p.z * p.z;
        assert!((f - 1.0).abs() < 1e-3, "hit point off surface: {f}");
    }

    #[test]
    fn rotated_instance_hits_where_baked_triangle_would() {
        let mat = Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5)));
        let local: Arc<dyn Hittable> = Arc::new(Triangle::new(
            Vec3A::new(-1.0, -1.0, 0.0),
            Vec3A::new(1.0, -1.0, 0.0),
            Vec3A::new(0.0, 1.0, 0.0),
            mat.clone(),
        ));
        let xf = Affine3A::from_mat4(
            Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2)
                * Mat4::from_translation(glam::Vec3::new(0.0, 0.0, 2.0)),
        );
        let inst = Instance::new(local, xf);
        // Local (0,0,2) maps to world (2,0,0); triangle now faces +X.
        let ray = Ray::new(Vec3A::new(5.0, 0.0, 0.0), -Vec3A::X);
        let hit = inst.hit(&ray, 0.001, f32::INFINITY).expect("hit");
        assert!((hit.rec.t - 3.0).abs() < 1e-4);
        assert!(hit.rec.normal.abs_diff_eq(Vec3A::X, 1e-4));
    }

    #[test]
    fn motion_blur_interpolates_position() {
        let inst = Instance::new(unit_sphere(), Affine3A::IDENTITY)
            .with_motion(Affine3A::from_translation(glam::Vec3::new(4.0, 0.0, 0.0)));
        // At time 0 the sphere is at the origin...
        let r0 = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z).with_time(0.0);
        assert!(inst.hit(&r0, 0.001, f32::INFINITY).is_some());
        // ...at time 1 it has moved to x=4...
        let r1 = Ray::new(Vec3A::new(4.0, 0.0, -5.0), Vec3A::Z).with_time(1.0);
        assert!(inst.hit(&r1, 0.001, f32::INFINITY).is_some());
        let r1_origin = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z).with_time(1.0);
        assert!(inst.hit(&r1_origin, 0.001, f32::INFINITY).is_none());
        // ...and at time 0.5 it is halfway.
        let rh = Ray::new(Vec3A::new(2.0, 0.0, -5.0), Vec3A::Z).with_time(0.5);
        let hit = inst.hit(&rh, 0.001, f32::INFINITY).expect("halfway hit");
        assert!((hit.rec.t - 4.0).abs() < 1e-4);
        // The shutter-union bounding box covers both endpoints.
        let bb = inst.bounding_box().unwrap();
        assert!(bb.minimum.x <= -1.0 && bb.maximum.x >= 5.0);
    }
}
