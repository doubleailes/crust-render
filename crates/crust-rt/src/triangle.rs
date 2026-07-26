//! Watertight ray/triangle intersection and exact triangle clipping.
//!
//! Two intersectors share one set of formulas: a scalar one for single
//! triangles ([`triangle_intersect`]) and a 4-wide SIMD one for the BVH's
//! leaf packets ([`Tri4::intersect`]). The Woop transform splits cleanly
//! along that seam — the axis permutation and shear depend only on the
//! *ray* ([`RayShear`]), so a packet computes them once and amortizes them
//! over four triangles, and the per-triangle work is pure arithmetic that
//! maps directly onto `Vec4` lanes.
//!
//! The two paths are written to produce *bit-identical* results (same
//! operations, same order, no FMA contraction), which
//! `simd_matches_scalar_bitwise` pins.

use crate::aabb::AABB;
use crate::ray::Ray;
use glam::{Vec3A, Vec4};

/// The per-ray constants of the Woop transform: which axis permutation
/// sends the ray direction to +Z, and the shear that straightens it.
/// Shared by every triangle a ray is tested against, so the BVH builds one
/// per traversal and hands it to each leaf packet.
pub(crate) struct RayShear {
    kx: usize,
    ky: usize,
    kz: usize,
    sx: f32,
    sy: f32,
    sz: f32,
    /// The ray origin's permuted components, pre-splatted for the packet
    /// path.
    okx: Vec4,
    oky: Vec4,
    okz: Vec4,
    sx4: Vec4,
    sy4: Vec4,
    sz4: Vec4,
}

impl RayShear {
    pub(crate) fn new(ray: &Ray) -> Self {
        let d = ray.dir;

        // Permute so the dominant direction axis is Z; swap X/Y when
        // d.z < 0 so the winding (and edge-function signs) are preserved.
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

        let o = ray.origin;
        RayShear {
            kx,
            ky,
            kz,
            sx,
            sy,
            sz,
            okx: Vec4::splat(o[kx]),
            oky: Vec4::splat(o[ky]),
            okz: Vec4::splat(o[kz]),
            sx4: Vec4::splat(sx),
            sy4: Vec4::splat(sy),
            sz4: Vec4::splat(sz),
        }
    }
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
    triangle_intersect_sheared(&RayShear::new(ray), ray.origin, v0, v1, v2, t_min, t_max)
}

/// [`triangle_intersect`] with the per-ray shear already computed — the
/// form the packet path's f64 fallback lanes take.
pub(crate) fn triangle_intersect_sheared(
    sh: &RayShear,
    origin: Vec3A,
    v0: Vec3A,
    v1: Vec3A,
    v2: Vec3A,
    t_min: f32,
    t_max: f32,
) -> Option<(f32, f32, f32)> {
    let (kx, ky, kz) = (sh.kx, sh.ky, sh.kz);
    let (sx, sy, sz) = (sh.sx, sh.sy, sh.sz);

    // Vertices relative to the ray origin, sheared into ray space.
    let a = v0 - origin;
    let b = v1 - origin;
    let c = v2 - origin;
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

/// Four triangles in structure-of-arrays layout: `v[i][axis]` holds vertex
/// `i`'s `axis` component across all four lanes, so the whole packet is
/// nine `Vec4`s and the intersector never shuffles.
///
/// Packets are built per BVH leaf at commit time. A leaf with a triangle
/// count that is not a multiple of four leaves the tail lanes inactive
/// (their vertices are set to the last real triangle's, so the arithmetic
/// stays finite and the lanes are dropped by `active`).
pub(crate) struct Tri4 {
    v: [[Vec4; 3]; 3],
    /// Index into the BVH's primitive array per lane — how a hit gets back
    /// to its shading normals and IDs.
    pub prim: [u32; 4],
    /// Bit `k` set iff lane `k` holds a real triangle.
    pub active: u32,
    /// AND / OR of the active lanes' visibility masks. Together they give a
    /// two-compare fast path: `ray.mask & mask_and != 0` means every lane
    /// is visible (the overwhelmingly common case), `ray.mask & mask_or == 0`
    /// means none is, and only a genuinely mixed packet pays per-lane tests.
    mask_and: u32,
    mask_or: u32,
    masks: [u32; 4],
}

/// Per-lane outcome of [`Tri4::intersect`].
pub(crate) struct Hit4 {
    /// Lanes that hit; their `t`/`u`/`v` entries are valid.
    pub hits: u32,
    /// Lanes where an edge function came out exactly 0.0. The f64 tie-break
    /// that keeps adjacent triangles watertight is inherently scalar, so
    /// these lanes carry no verdict here — the caller must re-test them
    /// with [`triangle_intersect_sheared`]. Rare in practice (it takes a
    /// ray passing exactly through an edge or vertex), so the branch costs
    /// nothing on real geometry.
    pub fallback: u32,
    pub t: [f32; 4],
    pub u: [f32; 4],
    pub v: [f32; 4],
}

impl Tri4 {
    /// Packs up to four triangles. `tris` shorter than 4 leaves the tail
    /// lanes inactive.
    pub(crate) fn new(tris: &[(Vec3A, Vec3A, Vec3A, u32, u32)]) -> Self {
        debug_assert!(!tris.is_empty() && tris.len() <= 4);
        let mut v = [[Vec4::ZERO; 3]; 3];
        let mut prim = [u32::MAX; 4];
        let mut masks = [0u32; 4];
        let mut active = 0u32;
        let mut mask_and = u32::MAX;
        let mut mask_or = 0u32;
        for lane in 0..4 {
            // Inactive tail lanes duplicate the last real triangle: the
            // lane is masked off anyway, and duplicating keeps the values
            // finite (a zeroed lane would be a degenerate triangle whose
            // edge functions are all exactly 0.0, needlessly tripping the
            // f64 fallback on every query).
            let (v0, v1, v2, pi, mask) = tris[lane.min(tris.len() - 1)];
            for axis in 0..3 {
                v[0][axis][lane] = v0[axis];
                v[1][axis][lane] = v1[axis];
                v[2][axis][lane] = v2[axis];
            }
            if lane < tris.len() {
                prim[lane] = pi;
                masks[lane] = mask;
                active |= 1 << lane;
                mask_and &= mask;
                mask_or |= mask;
            }
        }
        Tri4 {
            v,
            prim,
            active,
            mask_and,
            mask_or,
            masks,
        }
    }

    /// The lanes this ray's category is allowed to see.
    #[inline]
    fn visible_lanes(&self, ray_mask: u32) -> u32 {
        if ray_mask & self.mask_and != 0 {
            return self.active;
        }
        if ray_mask & self.mask_or == 0 {
            return 0;
        }
        let mut m = 0;
        for lane in 0..4 {
            if self.active & (1 << lane) != 0 && self.masks[lane] & ray_mask != 0 {
                m |= 1 << lane;
            }
        }
        m
    }

    /// Intersects all four triangles at once. Every step is the `Vec4`
    /// transcription of the scalar path above, in the same order, so a
    /// lane's `t`/`u`/`v` are bit-identical to the scalar result.
    pub(crate) fn intersect(&self, sh: &RayShear, ray_mask: u32, t_min: f32, t_max: f32) -> Hit4 {
        let mut m = self.visible_lanes(ray_mask);
        if m == 0 {
            return Hit4::MISS;
        }

        let (kx, ky, kz) = (sh.kx, sh.ky, sh.kz);

        // Vertices relative to the ray origin, sheared into ray space.
        let akz = self.v[0][kz] - sh.okz;
        let bkz = self.v[1][kz] - sh.okz;
        let ckz = self.v[2][kz] - sh.okz;
        let ax = (self.v[0][kx] - sh.okx) - sh.sx4 * akz;
        let ay = (self.v[0][ky] - sh.oky) - sh.sy4 * akz;
        let bx = (self.v[1][kx] - sh.okx) - sh.sx4 * bkz;
        let by = (self.v[1][ky] - sh.oky) - sh.sy4 * bkz;
        let cx = (self.v[2][kx] - sh.okx) - sh.sx4 * ckz;
        let cy = (self.v[2][ky] - sh.oky) - sh.sy4 * ckz;

        // Signed 2D edge functions; e0 is opposite v0, etc.
        let e0 = bx * cy - by * cx;
        let e1 = cx * ay - cy * ax;
        let e2 = ax * by - ay * bx;

        let zero = Vec4::ZERO;
        // Lanes sitting exactly on an edge go to the scalar f64 path, and
        // leave the SIMD verdict entirely — a lane the sign test below
        // rejects may well be a hit once the ties are resolved in f64.
        let fallback =
            m & (e0.cmpeq(zero).bitmask() | e1.cmpeq(zero).bitmask() | e2.cmpeq(zero).bitmask());
        m &= !fallback;

        // Inside iff the three edge functions share a sign.
        let neg = e0.cmplt(zero).bitmask() | e1.cmplt(zero).bitmask() | e2.cmplt(zero).bitmask();
        let pos = e0.cmpgt(zero).bitmask() | e1.cmpgt(zero).bitmask() | e2.cmpgt(zero).bitmask();
        m &= !(neg & pos);

        let det = e0 + e1 + e2;
        m &= !det.cmpeq(zero).bitmask();
        if m == 0 {
            return Hit4 {
                hits: 0,
                fallback,
                ..Hit4::MISS
            };
        }

        // Scaled hit distance, range-tested without a division. The scalar
        // path branches on det's sign; here the same test is a single
        // sign-flip: `t_scaled * sign(det)` compared against
        // `t_min * |det|` and `t_max * |det|`.
        let t_scaled = e0 * (sh.sz4 * akz) + e1 * (sh.sz4 * bkz) + e2 * (sh.sz4 * ckz);
        let abs_det = det.abs();
        let ts = Vec4::select(det.cmplt(zero), -t_scaled, t_scaled);
        m &= ts.cmpge(Vec4::splat(t_min) * abs_det).bitmask();
        m &= ts.cmple(Vec4::splat(t_max) * abs_det).bitmask();
        if m == 0 {
            return Hit4 {
                hits: 0,
                fallback,
                ..Hit4::MISS
            };
        }

        let inv_det = Vec4::ONE / det;
        Hit4 {
            hits: m,
            fallback,
            t: (t_scaled * inv_det).to_array(),
            u: (e1 * inv_det).to_array(),
            v: (e2 * inv_det).to_array(),
        }
    }
}

impl Hit4 {
    const MISS: Hit4 = Hit4 {
        hits: 0,
        fallback: 0,
        t: [0.0; 4],
        u: [0.0; 4],
        v: [0.0; 4],
    };
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

    /// The 4-wide intersector is a lane-for-lane transcription of the
    /// scalar one, so it must agree *bit-for-bit* — not within an epsilon.
    /// Anything looser would mean the two paths can disagree about a hit
    /// near an edge, which is exactly what watertightness forbids.
    #[test]
    fn simd_matches_scalar_bitwise() {
        // A cheap LCG, so the case list is fixed but not hand-picked.
        let mut state = 0x1234_5678u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };

        let mut compared = 0;
        let mut hits = 0;
        for _ in 0..2000 {
            let tris: Vec<(Vec3A, Vec3A, Vec3A, u32, u32)> = (0..4)
                .map(|i| {
                    let base = Vec3A::new(next(), next(), next()) * 4.0;
                    (
                        base + Vec3A::new(next(), next(), next()),
                        base + Vec3A::new(next(), next(), next()),
                        base + Vec3A::new(next(), next(), next()),
                        i as u32,
                        crate::ray::MASK_ALL,
                    )
                })
                .collect();
            let packet = Tri4::new(&tris);

            let origin = Vec3A::new(next(), next(), next()) * 6.0;
            // Half the rays are aimed at a barycentric point of one of the
            // four triangles (so hits are common), half are arbitrary (so
            // near-misses and the range tests get exercised too).
            let aimed = next() > 0.0;
            let dir = if aimed {
                let pick = ((next() + 0.5) * 4.0) as usize % 4;
                let (v0, v1, v2, _, _) = tris[pick];
                let (a, b) = (next() + 0.5, next() + 0.5);
                let (a, b) = if a + b > 1.0 { (1.0 - a, 1.0 - b) } else { (a, b) };
                (v0 + (v1 - v0) * a + (v2 - v0) * b) - origin
            } else {
                Vec3A::new(next(), next(), next())
            };
            if dir.length_squared() < 1e-8 {
                continue;
            }
            let r = ray(origin, dir.normalize());
            let sh = RayShear::new(&r);
            let out = packet.intersect(&sh, crate::ray::MASK_ALL, 0.001, f32::INFINITY);

            for (lane, &(v0, v1, v2, _, _)) in tris.iter().enumerate() {
                let scalar = triangle_intersect(&r, v0, v1, v2, 0.001, f32::INFINITY);
                if out.fallback & (1 << lane) != 0 {
                    continue; // routed to the scalar path by design
                }
                compared += 1;
                match (out.hits & (1 << lane) != 0, scalar) {
                    (true, Some((t, u, v))) => {
                        hits += 1;
                        assert_eq!(out.t[lane].to_bits(), t.to_bits(), "t differs, lane {lane}");
                        assert_eq!(out.u[lane].to_bits(), u.to_bits(), "u differs, lane {lane}");
                        assert_eq!(out.v[lane].to_bits(), v.to_bits(), "v differs, lane {lane}");
                    }
                    (false, None) => {}
                    (simd, sc) => panic!(
                        "hit disagreement on lane {lane}: simd={simd} scalar={}",
                        sc.is_some()
                    ),
                }
            }
        }
        assert!(compared > 7000, "only {compared} lanes compared");
        assert!(hits > 50, "only {hits} hits — the test is not exercising hits");
    }

    /// Range tests must agree too: the packet folds the scalar path's
    /// sign-of-det branch into one sign flip.
    #[test]
    fn simd_respects_t_range_like_scalar() {
        let tri = (
            Vec3A::new(-1.0, -1.0, 5.0),
            Vec3A::new(1.0, -1.0, 5.0),
            Vec3A::new(0.0, 1.0, 5.0),
        );
        // Both windings, so both signs of det are exercised.
        let cases = [
            (tri.0, tri.1, tri.2, 0u32, crate::ray::MASK_ALL),
            (tri.0, tri.2, tri.1, 1u32, crate::ray::MASK_ALL),
        ];
        let packet = Tri4::new(&cases);
        for (t_min, t_max) in [(0.001, 4.9), (5.1, 100.0), (0.001, f32::INFINITY), (4.9, 5.1)] {
            for dir in [Vec3A::Z, -Vec3A::Z] {
                let o = Vec3A::new(0.0, 0.0, if dir.z > 0.0 { 0.0 } else { 10.0 });
                let r = ray(o, dir);
                let out = packet.intersect(&RayShear::new(&r), crate::ray::MASK_ALL, t_min, t_max);
                for (lane, &(v0, v1, v2, _, _)) in cases.iter().enumerate() {
                    let scalar = triangle_intersect(&r, v0, v1, v2, t_min, t_max);
                    assert_eq!(
                        out.hits & (1 << lane) != 0,
                        scalar.is_some(),
                        "lane {lane} dir {dir:?} range ({t_min}, {t_max})"
                    );
                }
            }
        }
    }

    /// A partially-filled packet must never report its padding lanes.
    #[test]
    fn simd_inactive_lanes_never_hit() {
        let one = [(
            Vec3A::new(-1.0, -1.0, 5.0),
            Vec3A::new(1.0, -1.0, 5.0),
            Vec3A::new(0.0, 1.0, 5.0),
            0u32,
            crate::ray::MASK_ALL,
        )];
        let packet = Tri4::new(&one);
        assert_eq!(packet.active, 0b0001);
        let r = ray(Vec3A::ZERO, Vec3A::Z);
        let out = packet.intersect(&RayShear::new(&r), crate::ray::MASK_ALL, 0.001, f32::INFINITY);
        assert_eq!(out.hits, 0b0001, "only the real lane may hit");
        assert_eq!(out.fallback & !0b0001, 0, "padding lanes must not fall back");
    }

    /// Per-lane visibility masks must gate lanes independently.
    #[test]
    fn simd_masks_gate_lanes() {
        use crate::ray::{MASK_ALL, MASK_CAMERA, MASK_SHADOW};
        let tri = |z: f32, pi: u32, mask: u32| {
            (
                Vec3A::new(-1.0, -1.0, z),
                Vec3A::new(1.0, -1.0, z),
                Vec3A::new(0.0, 1.0, z),
                pi,
                mask,
            )
        };
        let cases = [
            tri(5.0, 0, MASK_CAMERA),
            tri(6.0, 1, MASK_SHADOW),
            tri(7.0, 2, MASK_ALL),
        ];
        let packet = Tri4::new(&cases);
        let r = ray(Vec3A::ZERO, Vec3A::Z);
        let sh = RayShear::new(&r);
        assert_eq!(
            packet
                .intersect(&sh, MASK_CAMERA, 0.001, f32::INFINITY)
                .hits,
            0b101
        );
        assert_eq!(
            packet
                .intersect(&sh, MASK_SHADOW, 0.001, f32::INFINITY)
                .hits,
            0b110
        );
        assert_eq!(packet.intersect(&sh, MASK_ALL, 0.001, f32::INFINITY).hits, 0b111);
        assert_eq!(packet.intersect(&sh, 1 << 20, 0.001, f32::INFINITY).hits, 0b100);
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
