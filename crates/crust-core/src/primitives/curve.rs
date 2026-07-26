//! Round curve segments — the hair/fur primitive, Embree's "round linear
//! curve": the convex hull of two spheres, i.e. a tangent cone frustum
//! with spherical caps. A `UsdGeomBasisCurves` prim imports as a chain of
//! these (cubic bases are flattened to a polyline first).

use crate::aabb::AABB;
use crate::hittable::{Hit, HitRecord, Hittable};
use crate::material::Material;
use crate::ray::Ray;
use glam::Vec3A;
use std::sync::Arc;

pub struct RoundCurveSegment {
    p0: Vec3A,
    p1: Vec3A,
    r0: f32,
    r1: f32,
    material: Arc<dyn Material>,
}

impl RoundCurveSegment {
    pub fn new(p0: Vec3A, p1: Vec3A, r0: f32, r1: f32, material: Arc<dyn Material>) -> Self {
        Self {
            p0,
            p1,
            r0: r0.max(1e-6),
            r1: r1.max(1e-6),
            material,
        }
    }
}

/// Nearest boundary hit of the rounded cone (sphere-swept segment) in
/// `[t_min, t_max]`, as `(t, outward_normal)`. The solid is the union of
/// the two cap spheres and the tangent cone body between them; taking the
/// minimum valid `t` over all three surfaces yields the hull entry point
/// for rays starting outside (rays starting *inside* may report an
/// interior sphere surface — irrelevant for opaque hair).
///
/// Following Quilez's rounded-cone intersector, evaluated with a
/// normalized direction and rescaled back to the caller's parameter.
fn rounded_cone_intersect(
    ray: &Ray,
    p0: Vec3A,
    p1: Vec3A,
    r0: f32,
    r1: f32,
    t_min: f32,
    t_max: f32,
) -> Option<(f32, Vec3A)> {
    let len = ray.direction().length();
    if len < 1e-20 {
        return None;
    }
    let rd = ray.direction() / len;
    let o = ray.origin();
    // Candidate ts below are in normalized units; the range test happens
    // there too, then the winner converts back.
    let (n_min, n_max) = (t_min * len, t_max * len);

    let ba = p1 - p0;
    let oa = o - p0;
    let ob = o - p1;
    let rr = r0 - r1;
    let m0 = ba.dot(ba);
    let m1 = ba.dot(oa);
    let m2 = ba.dot(rd);
    let m3 = rd.dot(oa);
    let m5 = oa.dot(oa);
    let m6 = ob.dot(rd);
    let m7 = ob.dot(ob);

    let mut best: Option<(f32, Vec3A)> = None;
    let mut consider = |t: f32, n: Vec3A| {
        if t >= n_min && t <= n_max && best.map_or(true, |(bt, _)| t < bt) {
            best = Some((t, n));
        }
    };

    // Cone body between the tangency circles (0 < y < d2), when neither
    // sphere swallows the other.
    let d2 = m0 - rr * rr;
    if d2 > 0.0 {
        let k2 = d2 - m2 * m2;
        let k1 = d2 * m3 - m1 * m2 + m2 * rr * r0;
        let k0 = d2 * m5 - m1 * m1 + m1 * rr * r0 * 2.0 - m0 * r0 * r0;
        let h = k1 * k1 - k0 * k2;
        if h >= 0.0 && k2.abs() > 1e-12 {
            let sq = h.sqrt();
            for t in [(-k1 - sq) / k2, (-k1 + sq) / k2] {
                let y = m1 - r0 * rr + t * m2;
                if y > 0.0 && y < d2 {
                    consider(t, (d2 * (oa + t * rd) - ba * y).normalize());
                }
            }
        }
    }

    // Cap spheres.
    let h0 = m3 * m3 - m5 + r0 * r0;
    if h0 >= 0.0 {
        let sq = h0.sqrt();
        for t in [-m3 - sq, -m3 + sq] {
            consider(t, (oa + t * rd) / r0);
        }
    }
    let h1 = m6 * m6 - m7 + r1 * r1;
    if h1 >= 0.0 {
        let sq = h1.sqrt();
        for t in [-m6 - sq, -m6 + sq] {
            consider(t, (ob + t * rd) / r1);
        }
    }

    best.map(|(t, n)| (t / len, n))
}

impl Hittable for RoundCurveSegment {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<Hit<'_>> {
        let (t, outward) =
            rounded_cone_intersect(ray, self.p0, self.p1, self.r0, self.r1, t_min, t_max)?;
        let mut rec = HitRecord::new();
        rec.t = t;
        rec.p = ray.at(t);
        rec.set_face_normal(ray, outward);
        Some(Hit {
            rec,
            mat: self.material.as_ref(),
        })
    }

    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        rounded_cone_intersect(ray, self.p0, self.p1, self.r0, self.r1, t_min, t_max).is_some()
    }

    fn bounding_box(&self) -> Option<AABB> {
        let a = AABB::new(self.p0 - Vec3A::splat(self.r0), self.p0 + Vec3A::splat(self.r0));
        let b = AABB::new(self.p1 - Vec3A::splat(self.r1), self.p1 + Vec3A::splat(self.r1));
        Some(AABB::surrounding_box(a, b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::OpenPBR;

    fn seg(p0: Vec3A, p1: Vec3A, r0: f32, r1: f32) -> RoundCurveSegment {
        let mat = Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5)));
        RoundCurveSegment::new(p0, p1, r0, r1, mat)
    }

    #[test]
    fn capsule_axial_hit_at_cap() {
        // Capsule from (0,0,0) to (4,0,0), radius 0.5; axial ray from -X
        // hits the p0 cap sphere at x = -0.5.
        let s = seg(Vec3A::ZERO, Vec3A::new(4.0, 0.0, 0.0), 0.5, 0.5);
        let r = Ray::new(Vec3A::new(-3.0, 0.0, 0.0), Vec3A::X);
        let hit = s.hit(&r, 0.001, f32::INFINITY).expect("axial hit");
        assert!((hit.rec.t - 2.5).abs() < 1e-4, "t = {}", hit.rec.t);
        assert!(hit.rec.normal.abs_diff_eq(-Vec3A::X, 1e-4));
    }

    #[test]
    fn capsule_perpendicular_hit_at_radius() {
        let s = seg(Vec3A::ZERO, Vec3A::new(4.0, 0.0, 0.0), 0.5, 0.5);
        let r = Ray::new(Vec3A::new(2.0, 3.0, 0.0), -Vec3A::Y);
        let hit = s.hit(&r, 0.001, f32::INFINITY).expect("side hit");
        assert!((hit.rec.t - 2.5).abs() < 1e-3, "t = {}", hit.rec.t);
        assert!(hit.rec.normal.abs_diff_eq(Vec3A::Y, 1e-3));
    }

    #[test]
    fn cone_radius_shrinks_along_axis() {
        // r goes 0.5 -> 0.1 over x in [0,4]; a perpendicular ray near the
        // thin end must graze closer to the axis than one near the thick end.
        let s = seg(Vec3A::ZERO, Vec3A::new(4.0, 0.0, 0.0), 0.5, 0.1);
        let thick = Ray::new(Vec3A::new(0.5, 3.0, 0.0), -Vec3A::Y);
        let thin = Ray::new(Vec3A::new(3.5, 3.0, 0.0), -Vec3A::Y);
        let t_thick = s.hit(&thick, 0.001, f32::INFINITY).expect("thick hit").rec.t;
        let t_thin = s.hit(&thin, 0.001, f32::INFINITY).expect("thin hit").rec.t;
        let surf_thick = 3.0 - t_thick; // height of surface above axis
        let surf_thin = 3.0 - t_thin;
        assert!(
            surf_thick > surf_thin + 0.2,
            "cone does not taper: {surf_thick} vs {surf_thin}"
        );
        // And both within the sphere radii bounds.
        assert!(surf_thick <= 0.5 + 1e-3 && surf_thin >= 0.1 - 1e-3);
    }

    #[test]
    fn respects_t_range_and_misses() {
        let s = seg(Vec3A::ZERO, Vec3A::new(4.0, 0.0, 0.0), 0.5, 0.5);
        let r = Ray::new(Vec3A::new(2.0, 3.0, 0.0), -Vec3A::Y);
        assert!(s.hit(&r, 0.001, 2.0).is_none());
        assert!(!s.hit_any(&r, 0.001, 2.0));
        // Ray passing wide of the capsule.
        let miss = Ray::new(Vec3A::new(2.0, 3.0, 2.0), -Vec3A::Y);
        assert!(s.hit(&miss, 0.001, f32::INFINITY).is_none());
        // Unnormalized direction: same surface point, halved t.
        let fast = Ray::new(Vec3A::new(2.0, 3.0, 0.0), -Vec3A::Y * 2.0);
        let hit = s.hit(&fast, 0.001, f32::INFINITY).expect("hit");
        assert!((hit.rec.t - 1.25).abs() < 1e-3);
    }

    #[test]
    fn degenerate_swallowed_sphere_still_hits() {
        // Segment shorter than the radius difference: the big sphere
        // swallows the small one; hits must come from the big sphere.
        let s = seg(Vec3A::ZERO, Vec3A::new(0.1, 0.0, 0.0), 1.0, 0.05);
        let r = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
        let hit = s.hit(&r, 0.001, f32::INFINITY).expect("hit");
        assert!((hit.rec.t - 4.0).abs() < 1e-3);
    }
}
