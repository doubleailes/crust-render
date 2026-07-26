//! Internal primitives the BVH is built over. Each carries the IDs and
//! the visibility mask of the geometry it came from; a hit is plain
//! `Copy` data (no lifetimes, no shading state).

use crate::aabb::{AABB, triangle_aabb};
use crate::curve::rounded_cone_intersect;
use crate::ray::Ray;
use crate::scene::Scene;
use crate::triangle::{clip_triangle_aabb, triangle_intersect};
use glam::{Affine3A, Mat3A, Vec3A};
use std::sync::Arc;

/// An intersection as the primitives report it: `outward` is the
/// *geometric outward* normal (not yet oriented against the ray) — the
/// public API flips it and derives `front_face` at the query edge, so
/// instance transforms can map it without bookkeeping.
#[derive(Clone, Copy)]
pub(crate) struct PrimHit {
    pub t: f32,
    pub outward: Vec3A,
    pub u: f32,
    pub v: f32,
    pub geom_id: u32,
    pub prim_id: u32,
}

pub(crate) trait Prim: Send + Sync {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit>;

    /// Boolean occlusion variant; overridden where cheaper than `hit`.
    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        self.hit(ray, t_min, t_max).is_some()
    }

    fn bbox(&self) -> AABB;

    /// `Some` for triangles, which the BVH groups into 4-wide SIMD packets
    /// when it builds its leaves. Avoids downcasting: a primitive simply
    /// declares whether it can join a packet.
    fn as_triangle(&self) -> Option<&TrianglePrim> {
        None
    }

    /// Conservative bounds of the part inside the axis slab, for the
    /// BVH's spatial splits. Default: bbox clipped to the slab.
    fn clipped_aabb(&self, axis: usize, min: f32, max: f32) -> Option<AABB> {
        let b = self.bbox();
        if b.minimum[axis] > max || b.maximum[axis] < min {
            return None;
        }
        let mut c = b;
        c.minimum[axis] = c.minimum[axis].max(min);
        c.maximum[axis] = c.maximum[axis].min(max);
        Some(c)
    }
}

#[inline]
fn masked_out(ray: &Ray, mask: u32) -> bool {
    ray.mask & mask == 0
}

// ---------------------------------------------------------------------
// Triangle (optionally with per-vertex shading normals)
// ---------------------------------------------------------------------

pub(crate) struct TrianglePrim {
    pub v0: Vec3A,
    pub v1: Vec3A,
    pub v2: Vec3A,
    /// Per-vertex shading normals; the reported normal interpolates them
    /// by the hit barycentrics when present.
    pub normals: Option<[Vec3A; 3]>,
    pub geom_id: u32,
    pub prim_id: u32,
    pub mask: u32,
}

impl TrianglePrim {
    /// Completes a hit whose `(t, u, v)` are already known — the shared tail
    /// of the scalar and the 4-wide SIMD intersectors, so both derive the
    /// reported normal the same way.
    pub(crate) fn hit_from_barycentric(&self, t: f32, u: f32, v: f32) -> Option<PrimHit> {
        let outward = match &self.normals {
            Some([n0, n1, n2]) => (*n0 * (1.0 - u - v) + *n1 * u + *n2 * v).normalize(),
            None => {
                let n = (self.v1 - self.v0).cross(self.v2 - self.v0);
                if n == Vec3A::ZERO {
                    return None; // degenerate sliver
                }
                n.normalize()
            }
        };
        Some(PrimHit {
            t,
            outward,
            u,
            v,
            geom_id: self.geom_id,
            prim_id: self.prim_id,
        })
    }
}

impl Prim for TrianglePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let (t, u, v) = triangle_intersect(ray, self.v0, self.v1, self.v2, t_min, t_max)?;
        self.hit_from_barycentric(t, u, v)
    }

    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        !masked_out(ray, self.mask)
            && triangle_intersect(ray, self.v0, self.v1, self.v2, t_min, t_max).is_some()
    }

    fn bbox(&self) -> AABB {
        triangle_aabb(self.v0, self.v1, self.v2)
    }

    fn as_triangle(&self) -> Option<&TrianglePrim> {
        Some(self)
    }

    fn clipped_aabb(&self, axis: usize, min: f32, max: f32) -> Option<AABB> {
        clip_triangle_aabb(self.v0, self.v1, self.v2, axis, min, max)
    }
}

// ---------------------------------------------------------------------
// Sphere
// ---------------------------------------------------------------------

pub(crate) struct SpherePrim {
    pub center: Vec3A,
    pub radius: f32,
    pub geom_id: u32,
    pub mask: u32,
}

impl Prim for SpherePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let oc = ray.origin - self.center;
        let a = ray.dir.length_squared();
        let half_b = oc.dot(ray.dir);
        let c = oc.length_squared() - self.radius * self.radius;
        let discriminant = half_b * half_b - a * c;
        if discriminant < 0.0 {
            return None;
        }
        let sqrt_d = discriminant.sqrt();
        let mut root = (-half_b - sqrt_d) / a;
        if root <= t_min || root >= t_max {
            root = (-half_b + sqrt_d) / a;
            if root <= t_min || root >= t_max {
                return None;
            }
        }
        Some(PrimHit {
            t: root,
            outward: (ray.at(root) - self.center) / self.radius,
            u: 0.0,
            v: 0.0,
            geom_id: self.geom_id,
            prim_id: 0,
        })
    }

    fn bbox(&self) -> AABB {
        AABB::new(
            self.center - Vec3A::splat(self.radius),
            self.center + Vec3A::splat(self.radius),
        )
    }
}

// ---------------------------------------------------------------------
// Round curve segment (sphere-swept cone)
// ---------------------------------------------------------------------

pub(crate) struct CurvePrim {
    pub p0: Vec3A,
    pub p1: Vec3A,
    pub r0: f32,
    pub r1: f32,
    pub geom_id: u32,
    pub prim_id: u32,
    pub mask: u32,
}

impl Prim for CurvePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let (t, outward) =
            rounded_cone_intersect(ray, self.p0, self.p1, self.r0, self.r1, t_min, t_max)?;
        Some(PrimHit {
            t,
            outward,
            u: 0.0,
            v: 0.0,
            geom_id: self.geom_id,
            prim_id: self.prim_id,
        })
    }

    fn bbox(&self) -> AABB {
        let a = AABB::new(self.p0 - Vec3A::splat(self.r0), self.p0 + Vec3A::splat(self.r0));
        let b = AABB::new(self.p1 - Vec3A::splat(self.r1), self.p1 + Vec3A::splat(self.r1));
        AABB::surrounding_box(a, b)
    }
}

// ---------------------------------------------------------------------
// Instance: a committed scene placed by a transform, with optional
// transform motion blur (linear matrix interpolation at the ray's time).
// ---------------------------------------------------------------------

pub(crate) struct InstancePrim {
    pub scene: Arc<Scene>,
    /// Local-to-world at shutter time 0.
    pub l2w: Affine3A,
    /// Cached world-to-local and inverse-transpose at time 0.
    pub w2l: Affine3A,
    pub normal_mat: Mat3A,
    /// Local-to-world at shutter time 1, when the instance moves.
    pub l2w_end: Option<Affine3A>,
    pub bounds: AABB,
    pub geom_id: u32,
    pub mask: u32,
}

/// Element-wise linear interpolation of two affine transforms. Every
/// interpolated point stays inside the convex hull of its endpoint
/// positions, so a union-of-endpoints bound is conservative.
pub(crate) fn lerp_affine(a: &Affine3A, b: &Affine3A, t: f32) -> Affine3A {
    Affine3A {
        matrix3: Mat3A::from_cols(
            a.matrix3.x_axis.lerp(b.matrix3.x_axis, t),
            a.matrix3.y_axis.lerp(b.matrix3.y_axis, t),
            a.matrix3.z_axis.lerp(b.matrix3.z_axis, t),
        ),
        translation: a.translation.lerp(b.translation, t),
    }
}

/// World-space box of `local` under `m`: transformed corner bounds,
/// padded on degenerate axes like `triangle_aabb`.
pub(crate) fn transformed_aabb(local: &AABB, m: &Affine3A) -> AABB {
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

impl InstancePrim {
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
        Ray {
            origin: w2l.transform_point3a(ray.origin),
            // Unnormalized on purpose: local t == world t.
            dir: w2l.transform_vector3a(ray.dir),
            time: ray.time,
            mask: ray.mask,
        }
    }
}

impl Prim for InstancePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let (w2l, normal_mat) = self.transforms_at(ray.time);
        let local = self.to_local(ray, &w2l);
        let mut hit = self.scene.intersect_outward(&local, t_min, t_max)?;
        hit.outward = (normal_mat * hit.outward).normalize();
        // The hit is attributed to the *instance's* geometry id: the
        // application maps materials per top-level geometry. The inner
        // primitive index is kept.
        hit.geom_id = self.geom_id;
        Some(hit)
    }

    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        if masked_out(ray, self.mask) {
            return false;
        }
        let (w2l, _) = self.transforms_at(ray.time);
        self.scene.occluded(&self.to_local(ray, &w2l), t_min, t_max)
    }

    fn bbox(&self) -> AABB {
        self.bounds
    }
}
