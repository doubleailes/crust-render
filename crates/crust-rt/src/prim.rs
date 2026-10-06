//! Internal primitives the BVH is built over. Each carries the IDs and
//! the visibility mask of the geometry it came from; a hit is plain
//! `Copy` data (no lifetimes, no shading state).

use crate::aabb::{AABB, triangle_aabb};
use crate::curve::rounded_cone_intersect;
use crate::ray::{Ray, RayMask};
use crate::scene::Scene;
use glam::{Affine3A, Mat3A, Vec3A};
use std::sync::Arc;

/// An intersection as the primitives report it: `outward` is the
/// *geometric outward* normal (not yet oriented against the ray) — the
/// public API flips it and derives `front_face` at the query edge, so
/// instance transforms can map it without bookkeeping.
///
/// `dpdu` is a curve's (unnormalised) direction at the hit, which instances
/// map by their local-to-world linear part; zero for every other primitive.
/// A `Vec3A`, though it grows the record from 48 to 64 bytes: stored as
/// `[f32; 3]` to keep 48, the unaligned stores and compares cost traversal
/// three times what the copies they save do.
#[derive(Clone, Copy)]
pub(crate) struct PrimHit {
    pub t: f32,
    pub outward: Vec3A,
    pub dpdu: Vec3A,
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

    /// Conservative bounds of the part inside the axis slab, for the
    /// BVH's spatial splits. Default: bbox clipped to the slab.
    fn clipped_aabb(&self, axis: usize, min: f32, max: f32) -> Option<AABB> {
        clip_box(self.bbox(), axis, min, max)
    }
}

/// `b` clipped to the slab `min..=max` on `axis`, or `None` outside it: the
/// default spatial-split bound of a primitive with no exact clip. Shared by
/// [`Prim::clipped_aabb`] and the instances, whose bounds live beside them
/// rather than in them.
pub(crate) fn clip_box(b: AABB, axis: usize, min: f32, max: f32) -> Option<AABB> {
    if b.minimum[axis] > max || b.maximum[axis] < min {
        return None;
    }
    let mut c = b;
    c.minimum[axis] = c.minimum[axis].max(min);
    c.maximum[axis] = c.maximum[axis].min(max);
    Some(c)
}

#[inline]
fn masked_out(ray: &Ray, mask: RayMask) -> bool {
    !ray.mask.sees(mask)
}

// ---------------------------------------------------------------------
// Triangle record
// ---------------------------------------------------------------------

/// One triangle of a committed scene: *which* vertices, not the vertices
/// themselves. The positions live once in the owning
/// [`Bvh`](crate::bvh::Bvh)'s shared vertex table, which the SIMD packets
/// and this record both index, and a per-vertex shading normal, when the
/// geometry has them, sits at the same offset in the normal table.
///
/// 24 bytes (pinned by `a_triangle_record_is_24_bytes`). The previous
/// layout copied the three vertices into an 80-byte primitive node that
/// traversal never read — packets carried their own copy — and held a
/// third copy of each vertex's normal per triangle corner; a subdivided
/// mesh paid 128 bytes per triangle for what this plus half a vertex now
/// holds in 36.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TriangleRecord {
    /// Global indices into the scene's vertex table, or
    /// [`DEGENERATE_VERTEX`] in every slot for a triangle whose attached
    /// indices were out of range — kept so `prim_id`s stay dense, never
    /// built over or hit.
    pub v: [u32; 3],
    pub geom_id: u32,
    pub prim_id: u32,
    pub mask: RayMask,
}

/// The vertex index of a [`TriangleRecord`] that refers to no vertex.
pub(crate) const DEGENERATE_VERTEX: u32 = u32::MAX;

impl TriangleRecord {
    #[inline]
    pub(crate) fn is_degenerate(&self) -> bool {
        self.v[0] == DEGENERATE_VERTEX
    }
}

/// Where one geometry's triangles, vertices and normals start in its
/// scene's shared tables. Indexed by `geom_id`; a geometry that is not a
/// triangle mesh holds the defaults.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GeomTable {
    /// First entry of this geometry in the vertex table.
    pub vertex_base: u32,
    /// First entry of this geometry in the normal table, or
    /// [`NO_NORMALS`] when the geometry has no shading normals. A vertex at
    /// `vertex_base + k` has its normal at `normal_base + k`.
    pub normal_base: u32,
    /// First [`TriangleRecord`] of this geometry; `prim_id` offsets it.
    pub tri_base: u32,
}

/// [`GeomTable::normal_base`] for a geometry without shading normals.
pub(crate) const NO_NORMALS: u32 = u32::MAX;

impl Default for GeomTable {
    fn default() -> Self {
        GeomTable {
            vertex_base: 0,
            normal_base: NO_NORMALS,
            tri_base: 0,
        }
    }
}

/// Completes a triangle hit whose `(t, u, v)` are already known — the
/// shared tail of the scalar and the 4-wide SIMD intersectors, so both
/// derive the reported normal the same way. `normals` is the triangle's
/// three per-vertex shading normals when its geometry has them; without
/// them the geometric normal of `verts` is reported, and a sliver whose
/// cross product is exactly zero reports no hit at all.
#[inline]
pub(crate) fn triangle_hit_from_barycentric(
    rec: &TriangleRecord,
    verts: &[Vec3A; 3],
    normals: Option<[Vec3A; 3]>,
    t: f32,
    u: f32,
    v: f32,
) -> Option<PrimHit> {
    let outward = match normals {
        Some([n0, n1, n2]) => (n0 * (1.0 - u - v) + n1 * u + n2 * v).normalize(),
        None => {
            let n = (verts[1] - verts[0]).cross(verts[2] - verts[0]);
            if n == Vec3A::ZERO {
                return None; // degenerate sliver
            }
            n.normalize()
        }
    };
    Some(PrimHit {
        t,
        outward,
        dpdu: Vec3A::ZERO,
        u,
        v,
        geom_id: rec.geom_id,
        prim_id: rec.prim_id,
    })
}

// ---------------------------------------------------------------------
// Sphere
// ---------------------------------------------------------------------

pub(crate) struct SpherePrim {
    pub center: Vec3A,
    pub radius: f32,
    pub geom_id: u32,
    pub mask: RayMask,
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
        // The discriminant from the ray's closest approach to the centre,
        // `l = oc − (oc·d/a) d`: `a·(r² − |l|²)` is `half_b² − a·c` in exact
        // arithmetic, but the textbook form subtracts two terms of order
        // |oc|² to leave one of order r², and from 8 units away a sphere of
        // radius 0.05 had 1e-4 of rounding on its distance — enough for a
        // segment restarted just short of the hit to meet it again. Both
        // terms here are of order r², so the error scales with the sphere,
        // not its distance (Haines et al., Ray Tracing Gems ch. 7).
        let l = oc - ray.dir * (half_b / a);
        let discriminant = a * (self.radius * self.radius - l.length_squared());
        if discriminant < 0.0 {
            return None;
        }
        // The stable root pair: `q` adds the square root to `half_b` rather
        // than cancelling against it, and the other root follows from the
        // product `c/a`. `q == 0` is a tangent through the origin, both
        // roots at `t = 0`; comparing against `t_min` from below also
        // rejects the NaN a zero direction produces.
        let q = -half_b - half_b.signum() * discriminant.sqrt();
        if q == 0.0 {
            return None;
        }
        let (t0, t1) = (c / q, q / a);
        let (near, far) = if t0 < t1 { (t0, t1) } else { (t1, t0) };
        let root = if near > t_min && near < t_max {
            near
        } else if far > t_min && far < t_max {
            far
        } else {
            return None;
        };
        Some(PrimHit {
            t: root,
            outward: (ray.at(root) - self.center) / self.radius,
            dpdu: Vec3A::ZERO,
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
// Disk (one flat circle)
// ---------------------------------------------------------------------

/// A flat circular disk. `normal` is unit length and names the disk's
/// *front*: it is reported as the outward normal, so a hit's `front_face`
/// says which side the ray arrived from — which is how a one-sided emitter
/// (UsdLux `DiskLight`) tells its emitting side from its back.
pub(crate) struct DiskPrim {
    pub center: Vec3A,
    pub normal: Vec3A,
    pub radius: f32,
    pub geom_id: u32,
    pub mask: RayMask,
}

impl Prim for DiskPrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let denom = self.normal.dot(ray.dir);
        if denom == 0.0 {
            return None; // parallel to the plane: a disk has no thickness
        }
        let t = self.normal.dot(self.center - ray.origin) / denom;
        if t <= t_min || t >= t_max {
            return None;
        }
        if (ray.at(t) - self.center).length_squared() > self.radius * self.radius {
            return None;
        }
        Some(PrimHit {
            t,
            outward: self.normal,
            dpdu: Vec3A::ZERO,
            u: 0.0,
            v: 0.0,
            geom_id: self.geom_id,
            prim_id: 0,
        })
    }

    /// Exact: a circle of radius `r` with unit normal `n` extends
    /// `r·√(1 − nᵢ²)` along axis `i`. Padded like a triangle on the axis it
    /// has no thickness along, which the slab test would otherwise reject.
    fn bbox(&self) -> AABB {
        let n2 = self.normal * self.normal;
        let half = self.radius * (Vec3A::ONE - n2).max(Vec3A::ZERO).sqrt();
        let (lo, hi) = (self.center - half, self.center + half);
        triangle_aabb(lo, hi, lo)
    }
}

// ---------------------------------------------------------------------
// Cylinder (open tube)
// ---------------------------------------------------------------------

/// The side wall of a circular cylinder from `p0` along the unit `axis` for
/// `length` — **no caps**, which is UsdLux `CylinderLight`'s shape ("does not
/// emit light from the flat end-caps"). The outward normal is radial, so a
/// ray reaching the wall from inside the tube reports a back face.
pub(crate) struct CylinderPrim {
    pub p0: Vec3A,
    pub axis: Vec3A,
    pub length: f32,
    pub radius: f32,
    pub geom_id: u32,
    pub mask: RayMask,
}

impl Prim for CylinderPrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        // Project the ray onto the plane perpendicular to the axis, where
        // the wall is a circle; the axial coordinate then bounds the tube.
        let oc = ray.origin - self.p0;
        let d_perp = ray.dir - ray.dir.dot(self.axis) * self.axis;
        let oc_perp = oc - oc.dot(self.axis) * self.axis;
        let a = d_perp.length_squared();
        if a == 0.0 {
            return None; // parallel to the axis: never meets the wall
        }
        let half_b = oc_perp.dot(d_perp);
        let c = oc_perp.length_squared() - self.radius * self.radius;
        // The sphere's closest-approach discriminant and stable root pair,
        // in the projected plane (see `SpherePrim::hit`): a far, thin tube
        // had the same 1e-4 of rounding on its distance.
        let l = oc_perp - d_perp * (half_b / a);
        let discriminant = a * (self.radius * self.radius - l.length_squared());
        if discriminant < 0.0 {
            return None;
        }
        let q = -half_b - half_b.signum() * discriminant.sqrt();
        if q == 0.0 {
            return None;
        }
        let (t0, t1) = (c / q, q / a);
        let (near, far) = if t0 < t1 { (t0, t1) } else { (t1, t0) };
        for t in [near, far] {
            // From below, so a NaN root is rejected too.
            let in_range = t > t_min && t < t_max;
            if !in_range {
                continue;
            }
            let rel = oc + t * ray.dir;
            let s = rel.dot(self.axis);
            if !(0.0..=self.length).contains(&s) {
                continue;
            }
            return Some(PrimHit {
                t,
                outward: (rel - s * self.axis) / self.radius,
                dpdu: Vec3A::ZERO,
                u: 0.0,
                v: 0.0,
                geom_id: self.geom_id,
                prim_id: 0,
            });
        }
        None
    }

    /// Exact: the two end circles' extents, `r·√(1 − aᵢ²)` about each end.
    fn bbox(&self) -> AABB {
        let p1 = self.p0 + self.length * self.axis;
        let a2 = self.axis * self.axis;
        let half = self.radius * (Vec3A::ONE - a2).max(Vec3A::ZERO).sqrt();
        let (lo, hi) = (self.p0.min(p1) - half, self.p0.max(p1) + half);
        triangle_aabb(lo, hi, lo)
    }
}

// ---------------------------------------------------------------------
// Round curve segment (sphere-swept cone)
// ---------------------------------------------------------------------

pub(crate) struct CurvePrim {
    /// Unpadded on purpose: with `Vec3A` endpoints this is 64 bytes and,
    /// as the largest inline variant, sizes every `PrimNode` at 80; at 48
    /// the enum is 64 (`a_triangle_record_is_24_bytes` pins it). The two
    /// conversions on a hit are nothing beside the cone intersection.
    pub p0: [f32; 3],
    pub p1: [f32; 3],
    pub r0: f32,
    pub r1: f32,
    pub geom_id: u32,
    pub prim_id: u32,
    pub mask: RayMask,
    /// Which ends continue into the neighbouring segment of the same strand
    /// (`curve::JOINED_START` / `JOINED_END`): their caps are not the
    /// strand's boundary to a ray passing out of tubes. In the padding.
    pub joints: u8,
}

impl Prim for CurvePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let (p0, p1) = (Vec3A::from_array(self.p0), Vec3A::from_array(self.p1));
        let h = rounded_cone_intersect(ray, p0, p1, self.r0, self.r1, t_min, t_max, self.joints)?;
        Some(PrimHit {
            t: h.t,
            outward: h.normal,
            dpdu: p1 - p0,
            u: h.u,
            v: 0.0,
            geom_id: self.geom_id,
            prim_id: self.prim_id,
        })
    }

    fn bbox(&self) -> AABB {
        let a = AABB::new(
            Vec3A::from_array(self.p0) - Vec3A::splat(self.r0),
            Vec3A::from_array(self.p0) + Vec3A::splat(self.r0),
        );
        let b = AABB::new(
            Vec3A::from_array(self.p1) - Vec3A::splat(self.r1),
            Vec3A::from_array(self.p1) + Vec3A::splat(self.r1),
        );
        AABB::surrounding_box(a, b)
    }
}

// ---------------------------------------------------------------------
// Cubic curve span (round, analytically subdivided — see curve.rs)
// ---------------------------------------------------------------------

/// One authored cubic curve span, stored as its own Bézier control
/// points rather than pre-flattened into several `CurvePrim`s: a dense
/// xgen-style curve archive (grass, needles) attaches tens of millions of
/// these, so cutting each span's primitive count by the flattening factor
/// (`CURVE_FLATTEN_SEGS` in the USD importer, default 8) is the single
/// biggest lever on that memory. The full round-tube fidelity survives —
/// see `crate::curve::cubic_curve_intersect`.
pub(crate) struct CubicCurvePrim {
    pub cp: [Vec3A; 4],
    pub r0: f32,
    pub r1: f32,
    pub geom_id: u32,
    pub prim_id: u32,
    pub mask: RayMask,
    /// Which ends continue into the neighbouring span, as for
    /// [`CurvePrim::joints`]. In the padding.
    pub joints: u8,
}

impl Prim for CubicCurvePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let h = crate::curve::cubic_curve_intersect(
            ray,
            &self.cp,
            self.r0,
            self.r1,
            t_min,
            t_max,
            self.joints,
        )?;
        Some(PrimHit {
            t: h.t,
            outward: h.normal,
            dpdu: crate::curve::bezier_tangent(&self.cp, h.u),
            u: h.u,
            v: 0.0,
            geom_id: self.geom_id,
            prim_id: self.prim_id,
        })
    }

    fn bbox(&self) -> AABB {
        let radius = self.r0.max(self.r1);
        let min = self.cp[0].min(self.cp[1]).min(self.cp[2]).min(self.cp[3]) - Vec3A::splat(radius);
        let max = self.cp[0].max(self.cp[1]).max(self.cp[2]).max(self.cp[3]) + Vec3A::splat(radius);
        AABB::new(min, max)
    }
}

// ---------------------------------------------------------------------
// Instance: a committed scene placed by a transform, with optional
// transform motion blur (linear matrix interpolation at the ray's time).
// ---------------------------------------------------------------------

pub(crate) struct InstancePrim {
    pub scene: Arc<Scene>,
    /// World-to-local at shutter time 0 — the only transform a static
    /// instance keeps. Rays go into local space through it, and the normal
    /// matrix is its linear part transposed: exact, so it is recomputed per
    /// hit rather than cached.
    pub w2l: Affine3A,
    /// The endpoint placements of a moving instance; `None` for the
    /// overwhelming majority, which are static.
    ///
    /// Boxed because an `InstancePrim` is resident for the whole render:
    /// inline, the two transforms would cost 128 bytes on every instance —
    /// on a scene with tens of millions of static placements, gigabytes
    /// for fields none of them use.
    pub motion: Option<Box<InstanceMotion>>,
    /// What a hit inside reports when `id_offset` is [`NO_ID_OFFSET`]: the
    /// instance's own id, or the id [`crate::InstanceHitId::As`] asked for.
    pub geom_id: u32,
    /// [`crate::InstanceHitId::Offset`]'s base, added to the inner hit's id;
    /// [`NO_ID_OFFSET`] when the instance reports `geom_id` instead. Fits in
    /// the padding the 16-byte-aligned transform already leaves, so
    /// forwarding costs no resident memory (pinned by a test).
    pub id_offset: u32,
    pub mask: RayMask,
}

/// Local-to-world at shutter times 0 and 1 of a moving instance.
pub(crate) struct InstanceMotion {
    pub l2w: Affine3A,
    pub l2w_end: Affine3A,
}

/// [`InstancePrim::id_offset`] for an instance that does not forward.
pub(crate) const NO_ID_OFFSET: u32 = u32::MAX;

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
            if i & 1 == 0 {
                local.minimum.x
            } else {
                local.maximum.x
            },
            if i & 2 == 0 {
                local.minimum.y
            } else {
                local.maximum.y
            },
            if i & 4 == 0 {
                local.minimum.z
            } else {
                local.maximum.z
            },
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
        match &self.motion {
            Some(m) if time > 0.0 => {
                let w2l = lerp_affine(&m.l2w, &m.l2w_end, time).inverse();
                (w2l, w2l.matrix3.transpose())
            }
            // The expression `normal_mat` used to be cached from; a
            // transpose is exact, so recomputing it changes nothing.
            _ => (self.w2l, self.w2l.matrix3.transpose()),
        }
    }

    /// World bounds of this instance, recomputed — an instance keeps none
    /// once its tree is built. Exact for a moving instance (the union of its
    /// endpoint placements, as at commit); through `w2l.inverse()` for a
    /// static one, so possibly a few ulps off the build's box. For
    /// diagnostics only. `None` for an empty inner scene.
    pub(crate) fn approx_world_bounds(&self) -> Option<AABB> {
        let inner = self.scene.bounds()?;
        Some(match &self.motion {
            Some(m) => AABB::surrounding_box(
                transformed_aabb(&inner, &m.l2w),
                transformed_aabb(&inner, &m.l2w_end),
            ),
            None => transformed_aabb(&inner, &self.w2l.inverse()),
        })
    }

    fn to_local(&self, ray: &Ray, w2l: &Affine3A) -> Ray {
        Ray {
            origin: w2l.transform_point3a(ray.origin),
            // Unnormalized on purpose: local t == world t.
            dir: w2l.transform_vector3a(ray.dir),
            time: ray.time,
            mask: ray.mask,
            ignore_curve_exits: ray.ignore_curve_exits,
        }
    }
}

/// Not a [`Prim`]: an instance keeps no bounds once its tree is built (the
/// build reads them from `Primitives::instance_bounds`), so these are
/// inherent.
impl InstancePrim {
    pub(crate) fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let (w2l, normal_mat) = self.transforms_at(ray.time);
        let local = self.to_local(ray, &w2l);
        // Attribute the nested traversal to the instance level, so
        // top-level and instanced work can be told apart.
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::note_descent(self.geom_id);
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::enter_instance();
        let inner = self.scene.intersect_outward(&local, t_min, t_max);
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::leave_instance();
        let mut hit = inner?;
        hit.outward = (normal_mat * hit.outward).normalize();
        // Only curves carry a direction; a triangle or sphere hit skips the
        // inverse. The normal matrix is the inverse's transpose, so the
        // tangent's map is recovered from it rather than stored.
        if hit.dpdu != Vec3A::ZERO {
            hit.dpdu = normal_mat.transpose().inverse() * hit.dpdu;
        }
        // The hit is attributed to the *instance's* geometry id — the
        // application maps materials per top-level geometry — unless the
        // instance forwards the inner id under an offset (a prototype of
        // many parts placed as one instance). The inner primitive index is
        // kept either way.
        hit.geom_id = if self.id_offset == NO_ID_OFFSET {
            self.geom_id
        } else {
            // Cannot overflow: commit refuses an offset that could.
            self.id_offset + hit.geom_id
        };
        Some(hit)
    }

    pub(crate) fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        if masked_out(ray, self.mask) {
            return false;
        }
        let (w2l, _) = self.transforms_at(ray.time);
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::note_descent(self.geom_id);
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::enter_instance();
        let occluded = self.scene.occluded(&self.to_local(ray, &w2l), t_min, t_max);
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::leave_instance();
        occluded
    }
}

// ---------------------------------------------------------------------
// PrimNode: the analytic primitives, a closed unboxed sum.
// ---------------------------------------------------------------------

/// Storage for the analytic primitives: spheres, disks, cylinders and
/// linear curve segments, inline in one contiguous array and dispatched by
/// match instead of vtable. (A dense curve archive attaches tens of millions
/// of segments; one allocation each would cost real memory in allocator
/// bookkeeping alone.)
///
/// Instances and cubic curve spans used to be boxed variants here: their
/// payloads are larger than every other kind, so an enum would make every
/// slot pay their size. They now have arrays of their own (`Primitives`),
/// which costs neither a box nor a slot.
pub(crate) enum PrimNode {
    Sphere(SpherePrim),
    Disk(DiskPrim),
    Cylinder(CylinderPrim),
    Curve(CurvePrim),
}

impl PrimNode {
    /// Inlined into the BVH's out-of-line `scalar_hit`, which is what the
    /// traversal calls: that function, not this one, keeps the analytic and
    /// instance code out of `Bvh::hit`.
    #[inline]
    pub(crate) fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        match self {
            PrimNode::Sphere(p) => p.hit(ray, t_min, t_max),
            PrimNode::Disk(p) => p.hit(ray, t_min, t_max),
            PrimNode::Cylinder(p) => p.hit(ray, t_min, t_max),
            PrimNode::Curve(p) => p.hit(ray, t_min, t_max),
        }
    }

    #[inline]
    pub(crate) fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        match self {
            PrimNode::Sphere(p) => p.hit_any(ray, t_min, t_max),
            PrimNode::Disk(p) => p.hit_any(ray, t_min, t_max),
            PrimNode::Cylinder(p) => p.hit_any(ray, t_min, t_max),
            PrimNode::Curve(p) => p.hit_any(ray, t_min, t_max),
        }
    }

    #[inline]
    pub(crate) fn bbox(&self) -> AABB {
        match self {
            PrimNode::Sphere(p) => p.bbox(),
            PrimNode::Disk(p) => p.bbox(),
            PrimNode::Cylinder(p) => p.bbox(),
            PrimNode::Curve(p) => p.bbox(),
        }
    }

    #[inline]
    pub(crate) fn clipped_aabb(&self, axis: usize, min: f32, max: f32) -> Option<AABB> {
        match self {
            PrimNode::Sphere(p) => p.clipped_aabb(axis, min, max),
            PrimNode::Disk(p) => p.clipped_aabb(axis, min, max),
            PrimNode::Cylinder(p) => p.clipped_aabb(axis, min, max),
            PrimNode::Curve(p) => p.clipped_aabb(axis, min, max),
        }
    }
}
