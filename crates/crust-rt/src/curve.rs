//! Rounded-cone intersection — the round (sphere-swept) curve segment.

use crate::ray::Ray;
use glam::Vec3A;

/// Nearest boundary hit of the rounded cone (sphere-swept segment) in
/// `[t_min, t_max]`, as `(t, outward_normal)`. The solid is the union of
/// the two cap spheres and the tangent cone body between them; taking the
/// minimum valid `t` over all three surfaces yields the hull entry point
/// for rays starting outside (rays starting *inside* may report an
/// interior sphere surface — irrelevant for opaque hair).
///
/// Following Quilez's rounded-cone intersector, evaluated with a
/// normalized direction and rescaled back to the caller's parameter.
pub(crate) fn rounded_cone_intersect(
    ray: &Ray,
    p0: Vec3A,
    p1: Vec3A,
    r0: f32,
    r1: f32,
    t_min: f32,
    t_max: f32,
) -> Option<(f32, Vec3A)> {
    let r0 = r0.max(1e-6);
    let r1 = r1.max(1e-6);
    let len = ray.dir.length();
    if len < 1e-20 {
        return None;
    }
    let rd = ray.dir / len;
    let o = ray.origin;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(o: Vec3A, d: Vec3A, p0: Vec3A, p1: Vec3A, r0: f32, r1: f32) -> Option<(f32, Vec3A)> {
        rounded_cone_intersect(&Ray::new(o, d), p0, p1, r0, r1, 0.001, f32::INFINITY)
    }

    #[test]
    fn capsule_axial_hit_at_cap() {
        // Capsule from (0,0,0) to (4,0,0), radius 0.5; axial ray from -X
        // hits the p0 cap sphere at x = -0.5.
        let (t, n) = hit(
            Vec3A::new(-3.0, 0.0, 0.0),
            Vec3A::X,
            Vec3A::ZERO,
            Vec3A::new(4.0, 0.0, 0.0),
            0.5,
            0.5,
        )
        .expect("axial hit");
        assert!((t - 2.5).abs() < 1e-4, "t = {t}");
        assert!(n.abs_diff_eq(-Vec3A::X, 1e-4));
    }

    #[test]
    fn capsule_perpendicular_hit_at_radius() {
        let (t, n) = hit(
            Vec3A::new(2.0, 3.0, 0.0),
            -Vec3A::Y,
            Vec3A::ZERO,
            Vec3A::new(4.0, 0.0, 0.0),
            0.5,
            0.5,
        )
        .expect("side hit");
        assert!((t - 2.5).abs() < 1e-3, "t = {t}");
        assert!(n.abs_diff_eq(Vec3A::Y, 1e-3));
    }

    #[test]
    fn cone_radius_shrinks_along_axis() {
        // r goes 0.5 -> 0.1 over x in [0,4]; a perpendicular ray near the
        // thin end must graze closer to the axis than one near the thick end.
        let (p0, p1) = (Vec3A::ZERO, Vec3A::new(4.0, 0.0, 0.0));
        let t_thick = hit(Vec3A::new(0.5, 3.0, 0.0), -Vec3A::Y, p0, p1, 0.5, 0.1)
            .expect("thick hit")
            .0;
        let t_thin = hit(Vec3A::new(3.5, 3.0, 0.0), -Vec3A::Y, p0, p1, 0.5, 0.1)
            .expect("thin hit")
            .0;
        let surf_thick = 3.0 - t_thick; // height of surface above axis
        let surf_thin = 3.0 - t_thin;
        assert!(
            surf_thick > surf_thin + 0.2,
            "cone does not taper: {surf_thick} vs {surf_thin}"
        );
        assert!(surf_thick <= 0.5 + 1e-3 && surf_thin >= 0.1 - 1e-3);
    }

    #[test]
    fn respects_t_range_and_misses() {
        let (p0, p1) = (Vec3A::ZERO, Vec3A::new(4.0, 0.0, 0.0));
        let o = Vec3A::new(2.0, 3.0, 0.0);
        let r = Ray::new(o, -Vec3A::Y);
        assert!(rounded_cone_intersect(&r, p0, p1, 0.5, 0.5, 0.001, 2.0).is_none());
        // Ray passing wide of the capsule.
        assert!(hit(Vec3A::new(2.0, 3.0, 2.0), -Vec3A::Y, p0, p1, 0.5, 0.5).is_none());
        // Unnormalized direction: same surface point, halved t.
        let (t, _) = hit(o, -Vec3A::Y * 2.0, p0, p1, 0.5, 0.5).expect("hit");
        assert!((t - 1.25).abs() < 1e-3);
    }

    #[test]
    fn degenerate_swallowed_sphere_still_hits() {
        // Segment shorter than the radius difference: the big sphere
        // swallows the small one; hits must come from the big sphere.
        let (t, _) = hit(
            Vec3A::new(0.0, 0.0, -5.0),
            Vec3A::Z,
            Vec3A::ZERO,
            Vec3A::new(0.1, 0.0, 0.0),
            1.0,
            0.05,
        )
        .expect("hit");
        assert!((t - 4.0).abs() < 1e-3);
    }
}
