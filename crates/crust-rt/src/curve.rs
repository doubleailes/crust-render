//! Rounded-cone intersection — the round (sphere-swept) curve segment —
//! and the cubic curve intersector built on top of it.

use crate::aabb::AABB;
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

/// Recursion depth ceiling — matches pbrt's `Curve::IntersectRay`; a
/// backstop against pathological control points, not something real
/// curves approach (see `flatness_depth`'s doc comment).
const MAX_RECURSION_DEPTH: u32 = 10;

/// De Casteljau midpoint split of a cubic Bézier into two cubic Béziers
/// covering `[0, 0.5]` and `[0.5, 1]` of the original parameter range.
/// Shares its middle control point (index 3) between both halves, so the
/// two curves it returns are `out[0..4]` and `out[3..7]`.
fn subdivide_bezier(cp: &[Vec3A; 4]) -> [Vec3A; 7] {
    let p01 = (cp[0] + cp[1]) * 0.5;
    let p12 = (cp[1] + cp[2]) * 0.5;
    let p23 = (cp[2] + cp[3]) * 0.5;
    let p012 = (p01 + p12) * 0.5;
    let p123 = (p12 + p23) * 0.5;
    let p0123 = (p012 + p123) * 0.5;
    [cp[0], p01, p012, p0123, p123, p23, cp[3]]
}

/// How many levels of subdivision a cubic Bézier span needs before each
/// piece is flat enough that treating its two endpoints as a straight
/// capsule stays within a fraction of the curve's own width of the true
/// surface. Rotation- and translation-invariant (a second difference of
/// positions), so it can be computed once from the *unprojected* control
/// points — no ray-facing frame needed, unlike a flatness test bounding
/// perpendicular deviation alone. Ported from pbrt's `Curve::IntersectRay`
/// flatness heuristic (Nakamaru & Ohno 2002 curve rendering).
fn flatness_depth(cp: &[Vec3A; 4], max_width: f32) -> u32 {
    let d0 = cp[0] - cp[1] * 2.0 + cp[2];
    let d1 = cp[1] - cp[2] * 2.0 + cp[3];
    let l0 = d0
        .abs()
        .max(d1.abs())
        .max_element();
    if l0 <= 0.0 || max_width <= 0.0 {
        return 0;
    }
    // eps is 5% of the curve's width; depth is chosen so linear
    // interpolation error (~L0/4 per halving, quartering each level) drops
    // below it — log base 4 of L0/eps, i.e. log2 halved.
    let eps = max_width * 0.05;
    let r0 = (std::f32::consts::SQRT_2 * 6.0 * l0 / (8.0 * eps)).log2() * 0.5;
    if !r0.is_finite() {
        return 0;
    }
    (r0.floor().max(0.0) as u32).min(MAX_RECURSION_DEPTH)
}

/// Axis-aligned bounds of a Bézier span (the convex hull of its control
/// points — a cubic Bézier never leaves it), padded by the wider of the
/// two endpoint radii.
fn bezier_bounds(cp: &[Vec3A; 4], radius: f32) -> AABB {
    let min = cp[0].min(cp[1]).min(cp[2]).min(cp[3]) - Vec3A::splat(radius);
    let max = cp[0].max(cp[1]).max(cp[2]).max(cp[3]) + Vec3A::splat(radius);
    AABB::new(min, max)
}

/// Nearest hit of a cubic curve span in `[t_min, t_max]`, as
/// `(t, outward_normal)`. `cp` are the span's Bézier control points and
/// `r0`/`r1` the radii at its two ends (linearly interpolated in between,
/// matching USD's curve width semantics).
///
/// Adaptively subdivides the curve (depth chosen once from its own
/// flatness, see `flatness_depth`) until each piece is flat enough to
/// treat as a straight rounded cone — deferring to the already-exact
/// [`rounded_cone_intersect`] there rather than a cheaper flat-ribbon
/// test, so this stays a true round tube like the pre-flattened segments
/// it replaces. The subdivision happens per ray query, to only the depth
/// that ray's bounding-box tests actually demand — most rays reject at
/// depth 0 or 1 — rather than being baked into a fixed number of stored
/// primitives regardless of need.
pub(crate) fn cubic_curve_intersect(
    ray: &Ray,
    cp: &[Vec3A; 4],
    r0: f32,
    r1: f32,
    t_min: f32,
    t_max: f32,
) -> Option<(f32, Vec3A)> {
    let depth = flatness_depth(cp, 2.0 * r0.max(r1));
    subdivide_and_intersect(ray, cp, 0.0, 1.0, r0, r1, depth, t_min, t_max)
}

fn subdivide_and_intersect(
    ray: &Ray,
    cp: &[Vec3A; 4],
    u0: f32,
    u1: f32,
    r0_full: f32,
    r1_full: f32,
    depth: u32,
    t_min: f32,
    t_max: f32,
) -> Option<(f32, Vec3A)> {
    let radius_at = |u: f32| r0_full + (r1_full - r0_full) * u;

    if depth == 0 {
        return rounded_cone_intersect(ray, cp[0], cp[3], radius_at(u0), radius_at(u1), t_min, t_max);
    }

    let split = subdivide_bezier(cp);
    let u_mid = (u0 + u1) * 0.5;
    let halves = [
        ([split[0], split[1], split[2], split[3]], u0, u_mid),
        ([split[3], split[4], split[5], split[6]], u_mid, u1),
    ];

    let mut best: Option<(f32, Vec3A)> = None;
    for (sub_cp, su0, su1) in halves {
        let cur_t_max = best.map_or(t_max, |(t, _)| t);
        let radius = radius_at(su0).max(radius_at(su1));
        let bounds = bezier_bounds(&sub_cp, radius);
        if !bounds.hit(ray, t_min, cur_t_max) {
            continue;
        }
        if let Some(hit) = subdivide_and_intersect(
            ray, &sub_cp, su0, su1, r0_full, r1_full, depth - 1, t_min, cur_t_max,
        ) {
            best = Some(hit);
        }
    }
    best
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

#[cfg(test)]
mod cubic_curve_tests {
    use super::*;

    fn cubic_hit(
        o: Vec3A,
        d: Vec3A,
        cp: &[Vec3A; 4],
        r0: f32,
        r1: f32,
    ) -> Option<(f32, Vec3A)> {
        cubic_curve_intersect(&Ray::new(o, d), cp, r0, r1, 0.001, f32::INFINITY)
    }

    #[test]
    fn flat_span_needs_no_subdivision() {
        // Bézier control points evenly spaced on a line: the curve *is*
        // that line, so the flatness heuristic must not subdivide at all.
        let p0 = Vec3A::new(0.0, 0.0, 0.0);
        let p3 = Vec3A::new(4.0, 0.0, 0.0);
        let cp = [p0, p0.lerp(p3, 1.0 / 3.0), p0.lerp(p3, 2.0 / 3.0), p3];
        assert_eq!(flatness_depth(&cp, 1.0), 0);
    }

    #[test]
    fn curved_span_requires_subdivision() {
        // The classic 4-point cubic Bézier approximation of a unit-radius
        // quarter circle: clearly not flat, must recurse.
        const K: f32 = 0.552_284_75;
        let cp = [
            Vec3A::new(1.0, 0.0, 0.0),
            Vec3A::new(1.0, K, 0.0),
            Vec3A::new(K, 1.0, 0.0),
            Vec3A::new(0.0, 1.0, 0.0),
        ];
        assert!(flatness_depth(&cp, 0.1) > 0);
    }

    #[test]
    fn straight_span_matches_rounded_cone() {
        // A degenerate (perfectly straight) cubic span must behave exactly
        // like `rounded_cone_intersect` on its two endpoints — depth 0
        // reduces to precisely that call.
        let p0 = Vec3A::new(0.0, 0.0, 0.0);
        let p3 = Vec3A::new(4.0, 0.0, 0.0);
        let cp = [p0, p0.lerp(p3, 1.0 / 3.0), p0.lerp(p3, 2.0 / 3.0), p3];
        let (o, d) = (Vec3A::new(2.0, 3.0, 0.0), -Vec3A::Y);

        let cubic = cubic_hit(o, d, &cp, 0.5, 0.5).expect("cubic hit");
        let cone = rounded_cone_intersect(&Ray::new(o, d), p0, p3, 0.5, 0.5, 0.001, f32::INFINITY)
            .expect("cone hit");
        assert!((cubic.0 - cone.0).abs() < 1e-4);
        assert!(cubic.1.abs_diff_eq(cone.1, 1e-4));
    }

    #[test]
    fn quarter_circle_arc_is_followed_not_its_chord() {
        // Same quarter-circle Bézier as above, radius 0.05 (thin tube), in
        // the z = 0 plane. At the curve's own t = 0.5 this specific
        // 4-point approximation reproduces the true unit circle to high
        // precision: (cos 45°, sin 45°) = (√2/2, √2/2), *not* the chord
        // P0–P3's midpoint (0.5, 0.5) — the arc bulges ~0.29 units further
        // from the origin than the chord does at that point. An
        // implementation that only tested the chord (or stopped
        // subdividing too early) would get this backwards.
        const K: f32 = 0.552_284_75;
        let cp = [
            Vec3A::new(1.0, 0.0, 0.0),
            Vec3A::new(1.0, K, 0.0),
            Vec3A::new(K, 1.0, 0.0),
            Vec3A::new(0.0, 1.0, 0.0),
        ];
        let r = 0.05;

        // A Z-parallel ray through the true arc point: must hit.
        let true_arc = Vec3A::new(std::f32::consts::SQRT_2 / 2.0, std::f32::consts::SQRT_2 / 2.0, 0.0);
        let ray_o = true_arc + Vec3A::new(0.0, 0.0, 10.0);
        cubic_hit(ray_o, -Vec3A::Z, &cp, r, r).expect("ray through the true arc point must hit");

        // A Z-parallel ray through the chord's midpoint: must miss — it's
        // ~0.29 units from the nearest point on the actual curve, far
        // outside the 0.05 tube radius.
        let chord_mid = Vec3A::new(0.5, 0.5, 0.0);
        let ray_o = chord_mid + Vec3A::new(0.0, 0.0, 10.0);
        assert!(
            cubic_hit(ray_o, -Vec3A::Z, &cp, r, r).is_none(),
            "ray through the chord's midpoint (not on the curve) must miss"
        );
    }

    #[test]
    fn ray_passing_wide_misses() {
        let p0 = Vec3A::new(0.0, 0.0, 0.0);
        let p3 = Vec3A::new(4.0, 0.0, 0.0);
        let cp = [p0, p0.lerp(p3, 1.0 / 3.0), p0.lerp(p3, 2.0 / 3.0), p3];
        assert!(cubic_hit(Vec3A::new(2.0, 3.0, 2.0), -Vec3A::Y, &cp, 0.5, 0.5).is_none());
    }

    #[test]
    fn respects_t_range() {
        let p0 = Vec3A::new(0.0, 0.0, 0.0);
        let p3 = Vec3A::new(4.0, 0.0, 0.0);
        let cp = [p0, p0.lerp(p3, 1.0 / 3.0), p0.lerp(p3, 2.0 / 3.0), p3];
        let ray = Ray::new(Vec3A::new(2.0, 3.0, 0.0), -Vec3A::Y);
        assert!(cubic_curve_intersect(&ray, &cp, 0.5, 0.5, 0.001, 2.0).is_none());
    }
}
