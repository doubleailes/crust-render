use crate::aabb::{AABB, triangle_aabb};
use crate::hittable::{Hit, HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3A;
use std::sync::Arc;

pub struct Triangle {
    v0: Vec3A,
    v1: Vec3A,
    v2: Vec3A,
    material: Arc<dyn Material>,
}
impl Triangle {
    pub fn new(v0: Vec3A, v1: Vec3A, v2: Vec3A, material: Arc<dyn Material>) -> Self {
        Self {
            v0,
            v1,
            v2,
            material,
        }
    }
}

impl Hittable for Triangle {
    fn bounding_box(&self) -> Option<AABB> {
        Some(triangle_aabb(self.v0, self.v1, self.v2))
    }
    fn hit(&self, r: &Ray, t_min: f32, t_max: f32) -> Option<Hit<'_>> {
        triangle_hit(r, self.v0, self.v1, self.v2, t_min, t_max).map(|rec| Hit {
            rec,
            mat: self.material.as_ref(),
        })
    }
    fn clipped_aabb(&self, axis: usize, min: f32, max: f32) -> Option<AABB> {
        clip_triangle_aabb(self.v0, self.v1, self.v2, axis, min, max)
    }
}

/// Exact bounds of a triangle clipped to the axis slab `[min, max]`:
/// Sutherland-Hodgman against the slab's two planes, then the bounds of
/// the surviving polygon (padded on degenerate axes like `triangle_aabb`).
/// This is what makes spatial splits effective — a long diagonal
/// triangle's *clipped* bounds shrink on every axis, where the default
/// bbox∩slab only shrinks along the split axis.
pub(crate) fn clip_triangle_aabb(
    v0: Vec3A,
    v1: Vec3A,
    v2: Vec3A,
    axis: usize,
    min: f32,
    max: f32,
) -> Option<AABB> {
    // Clip the polygon against x[axis] >= min, then x[axis] <= max.
    let mut poly = [Vec3A::ZERO; 8];
    let mut n = 3;
    poly[0] = v0;
    poly[1] = v1;
    poly[2] = v2;

    for &(bound, keep_ge) in &[(min, true), (max, false)] {
        let mut out = [Vec3A::ZERO; 8];
        let mut m = 0;
        for i in 0..n {
            let a = poly[i];
            let b = poly[(i + 1) % n];
            let (da, db) = if keep_ge {
                (a[axis] - bound, b[axis] - bound)
            } else {
                (bound - a[axis], bound - b[axis])
            };
            if da >= 0.0 {
                out[m] = a;
                m += 1;
            }
            if (da > 0.0) != (db > 0.0) && da != db {
                // Edge crosses the plane.
                let t = da / (da - db);
                out[m] = a + (b - a) * t;
                m += 1;
            }
        }
        poly = out;
        n = m;
        if n == 0 {
            return None;
        }
    }

    let mut lo = poly[0];
    let mut hi = poly[0];
    for &p in &poly[1..n] {
        lo = lo.min(p);
        hi = hi.max(p);
    }
    // Snap the split axis exactly to the slab (interpolation rounding can
    // stick out) and pad flat axes so the slab test keeps working.
    lo[axis] = lo[axis].max(min);
    hi[axis] = hi[axis].min(max);
    const PAD: f32 = 1e-4;
    for a in 0..3 {
        if hi[a] - lo[a] < PAD {
            lo[a] -= PAD;
            hi[a] += PAD;
        }
    }
    Some(AABB::new(lo, hi))
}

/// Watertight ray/triangle intersection (Woop, Benthin & Wald 2013,
/// "Watertight Ray/Triangle Intersection"). Returns `(t, u, v)` where `u`
/// is the barycentric weight of `v1` and `v` of `v2` (`w = 1 - u - v` of
/// `v0`), or `None` on a miss.
///
/// The ray is transformed so its dominant axis becomes +Z (a winding-
/// preserving permutation plus a shear), the triangle is projected onto
/// z = 0, and the hit test becomes three 2D signed edge functions. Because
/// adjacent triangles share exact vertex coordinates, the shared edge's
/// function evaluates to the *same* value (with opposite sign convention)
/// for both triangles, so a ray crossing the edge can never miss both —
/// the guarantee epsilon-based Möller-Trumbore lacks. Edge functions that
/// come out exactly 0.0 in f32 are recomputed in f64, which resolves the
/// on-edge ties consistently.
pub(crate) fn triangle_intersect(
    ray: &Ray,
    v0: Vec3A,
    v1: Vec3A,
    v2: Vec3A,
    t_min: f32,
    t_max: f32,
) -> Option<(f32, f32, f32)> {
    let d = ray.direction();

    // Permute so the dominant direction axis is Z; swap X/Y when d.z < 0 so
    // the winding (and edge-function signs) are preserved.
    let ad = d.abs();
    let kz = if ad.x > ad.y {
        if ad.x > ad.z { 0 } else { 2 }
    } else if ad.y > ad.z {
        1
    } else {
        2
    };
    let mut kx = (kz + 1) % 3;
    let mut ky = (kz + 2) % 3;
    if d[kz] < 0.0 {
        std::mem::swap(&mut kx, &mut ky);
    }

    // Shear constants mapping the ray direction onto +Z.
    let sx = d[kx] / d[kz];
    let sy = d[ky] / d[kz];
    let sz = 1.0 / d[kz];

    // Vertices relative to the ray origin, sheared into ray space.
    let a = v0 - ray.origin();
    let b = v1 - ray.origin();
    let c = v2 - ray.origin();
    let ax = a[kx] - sx * a[kz];
    let ay = a[ky] - sy * a[kz];
    let bx = b[kx] - sx * b[kz];
    let by = b[ky] - sy * b[kz];
    let cx = c[kx] - sx * c[kz];
    let cy = c[ky] - sy * c[kz];

    // Signed 2D edge functions; e0 is opposite v0, etc.
    let mut e0 = bx * cy - by * cx;
    let mut e1 = cx * ay - cy * ax;
    let mut e2 = ax * by - ay * bx;

    // An exact 0.0 means the ray passes through an edge or vertex in f32;
    // re-evaluate in f64 so both triangles sharing the edge agree on which
    // side the ray falls.
    if e0 == 0.0 || e1 == 0.0 || e2 == 0.0 {
        e0 = (bx as f64 * cy as f64 - by as f64 * cx as f64) as f32;
        e1 = (cx as f64 * ay as f64 - cy as f64 * ax as f64) as f32;
        e2 = (ax as f64 * by as f64 - ay as f64 * bx as f64) as f32;
    }

    // Inside iff all edge functions share a sign (0.0 counts as both,
    // keeping shared edges hittable from either triangle).
    if (e0 < 0.0 || e1 < 0.0 || e2 < 0.0) && (e0 > 0.0 || e1 > 0.0 || e2 > 0.0) {
        return None;
    }
    let det = e0 + e1 + e2;
    if det == 0.0 {
        return None;
    }

    // Scaled hit distance; range-tested against t_min/t_max without a
    // division, minding det's sign.
    let az = sz * a[kz];
    let bz = sz * b[kz];
    let cz = sz * c[kz];
    let t_scaled = e0 * az + e1 * bz + e2 * cz;
    if det < 0.0 && (t_scaled > t_min * det || t_scaled < t_max * det) {
        return None;
    }
    if det > 0.0 && (t_scaled < t_min * det || t_scaled > t_max * det) {
        return None;
    }

    let inv_det = 1.0 / det;
    Some((t_scaled * inv_det, e1 * inv_det, e2 * inv_det))
}

pub(crate) fn triangle_hit(
    ray: &Ray,
    v0: Vec3A,
    v1: Vec3A,
    v2: Vec3A,
    t_min: f32,
    t_max: f32,
) -> Option<HitRecord> {
    let (t, _, _) = triangle_intersect(ray, v0, v1, v2, t_min, t_max)?;

    let n = (v1 - v0).cross(v2 - v0);
    if n == Vec3A::ZERO {
        // Degenerate sliver: no meaningful surface normal.
        return None;
    }

    let mut rec = HitRecord::new();
    rec.t = t;
    rec.p = ray.at(t);
    rec.set_face_normal(ray, n.normalize());
    Some(rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ray(o: Vec3A, d: Vec3A) -> Ray {
        Ray::new(o, d)
    }

    #[test]
    fn interior_hit_has_expected_t_and_barycentrics() {
        let (v0, v1, v2) = (
            Vec3A::new(0.0, 0.0, 5.0),
            Vec3A::new(4.0, 0.0, 5.0),
            Vec3A::new(0.0, 4.0, 5.0),
        );
        // Aim at the barycentric point w=0.25, u=0.5, v=0.25 -> (2.0, 1.0, 5).
        let r = ray(Vec3A::new(2.0, 1.0, 0.0), Vec3A::Z);
        let (t, u, v) =
            triangle_intersect(&r, v0, v1, v2, 0.001, f32::INFINITY).expect("interior hit");
        assert!((t - 5.0).abs() < 1e-5);
        assert!((u - 0.5).abs() < 1e-5, "u={u}");
        assert!((v - 0.25).abs() < 1e-5, "v={v}");
    }

    #[test]
    fn respects_t_range() {
        let (v0, v1, v2) = (
            Vec3A::new(-1.0, -1.0, 5.0),
            Vec3A::new(1.0, -1.0, 5.0),
            Vec3A::new(0.0, 1.0, 5.0),
        );
        let r = ray(Vec3A::ZERO, Vec3A::Z);
        assert!(triangle_intersect(&r, v0, v1, v2, 0.001, 4.9).is_none());
        assert!(triangle_intersect(&r, v0, v1, v2, 5.1, 100.0).is_none());
        assert!(triangle_intersect(&r, v0, v1, v2, 0.001, f32::INFINITY).is_some());
        // Negative-t (behind the origin) must never report.
        let back = ray(Vec3A::ZERO, -Vec3A::Z);
        assert!(triangle_intersect(&back, v0, v1, v2, 0.001, f32::INFINITY).is_none());
    }

    /// The watertightness guarantee: rays crossing the shared edge of two
    /// triangles forming a quad must hit at least one of them — no pinholes.
    /// The diagonal is deliberately irrational-ish so the edge points do not
    /// land on exact float grid values.
    #[test]
    fn shared_edge_is_watertight() {
        let p00 = Vec3A::new(-1.3371, -0.7713, 3.7);
        let p10 = Vec3A::new(1.9241, -1.1157, 4.3);
        let p11 = Vec3A::new(1.6083, 1.4127, 3.9);
        let p01 = Vec3A::new(-0.9743, 0.8291, 4.1);
        // Shared edge p00 -> p11.
        let tris = [(p00, p10, p11), (p00, p11, p01)];

        for i in 0..=10_000 {
            let s = i as f32 / 10_000.0;
            let target = p00 + s * (p11 - p00);
            let origin = Vec3A::new(0.1731, -0.0913, 0.0);
            let r = ray(origin, target - origin);
            let hits = tris
                .iter()
                .filter(|(a, b, c)| {
                    triangle_intersect(&r, *a, *b, *c, 0.001, f32::INFINITY).is_some()
                })
                .count();
            assert!(hits >= 1, "pinhole at s={s} (target {target:?})");
        }
    }

    /// Vertices shared by several triangles must also be covered.
    #[test]
    fn shared_vertex_is_covered() {
        // Fan of 4 triangles around a hub vertex.
        let hub = Vec3A::new(0.2137, 0.5391, 5.1);
        let rim = [
            Vec3A::new(1.3, 0.4, 5.0),
            Vec3A::new(0.3, 1.7, 5.3),
            Vec3A::new(-1.1, 0.6, 4.9),
            Vec3A::new(-0.2, -1.2, 5.2),
        ];
        let r = ray(Vec3A::new(0.0, 0.0, 0.0), hub);
        let hits = (0..4)
            .filter(|&k| {
                triangle_intersect(&r, hub, rim[k], rim[(k + 1) % 4], 0.001, f32::INFINITY)
                    .is_some()
            })
            .count();
        assert!(hits >= 1, "ray through shared vertex missed the whole fan");
    }

    #[test]
    fn degenerate_triangle_misses() {
        let v = Vec3A::new(1.0, 1.0, 5.0);
        let r = ray(Vec3A::ZERO, Vec3A::new(1.0, 1.0, 5.0));
        // Collinear vertices — zero area.
        assert!(
            triangle_hit(
                &r,
                Vec3A::new(0.0, 0.0, 5.0),
                Vec3A::new(1.0, 0.0, 5.0),
                Vec3A::new(2.0, 0.0, 5.0),
                0.001,
                f32::INFINITY
            )
            .is_none()
        );
        // Repeated vertex.
        assert!(triangle_hit(&r, v, v, Vec3A::new(0.0, 2.0, 5.0), 0.001, f32::INFINITY).is_none());
    }

    /// Axis-aligned rays (a zero direction component on the dominant-axis
    /// permutation path) must still intersect correctly.
    #[test]
    fn axis_aligned_rays_hit() {
        let (v0, v1, v2) = (
            Vec3A::new(-1.0, -1.0, 2.0),
            Vec3A::new(1.0, -1.0, 2.0),
            Vec3A::new(0.0, 1.0, 2.0),
        );
        for d in [Vec3A::Z, -Vec3A::Z] {
            let o = Vec3A::new(0.0, 0.0, if d.z > 0.0 { 0.0 } else { 4.0 });
            let hit = triangle_intersect(&ray(o, d), v0, v1, v2, 0.001, f32::INFINITY);
            assert!(hit.is_some(), "axis-aligned ray {d:?} missed");
            assert!((hit.unwrap().0 - 2.0).abs() < 1e-5);
        }
    }
}
