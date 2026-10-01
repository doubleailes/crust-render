//! Internal primitives the BVH is built over. Each carries the IDs and
//! the visibility mask of the geometry it came from; a hit is plain
//! `Copy` data (no lifetimes, no shading state).

use crate::aabb::{AABB, triangle_aabb};
use crate::curve::rounded_cone_intersect;
use crate::ray::{Ray, RayMask};
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
    /// `normals` is the owning [`Bvh`](crate::bvh::Bvh)'s shading-normal
    /// table, which only a triangle reads (see [`TrianglePrim::normals`]).
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, normals: &[VertexNormal]) -> Option<PrimHit>;

    /// Boolean occlusion variant; overridden where cheaper than `hit`.
    /// Occlusion reports no normal, so the table is not needed: the one
    /// primitive that reads it (a triangle) overrides this.
    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        self.hit(ray, t_min, t_max, &[]).is_some()
    }

    fn bbox(&self) -> AABB;

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
fn masked_out(ray: &Ray, mask: RayMask) -> bool {
    !ray.mask.sees(mask)
}

// ---------------------------------------------------------------------
// Triangle (optionally with per-vertex shading normals)
// ---------------------------------------------------------------------

pub(crate) struct TrianglePrim {
    pub v0: Vec3A,
    pub v1: Vec3A,
    pub v2: Vec3A,
    pub geom_id: u32,
    pub prim_id: u32,
    pub mask: RayMask,
    /// Indices of this triangle's three per-vertex shading normals in its
    /// [`Bvh`](crate::bvh::Bvh)'s normal table, or [`NO_NORMALS`]; the
    /// reported normal interpolates them by the hit barycentrics when
    /// present.
    pub normals: [u32; 3],
}

/// One per-vertex shading normal, an entry of a BVH's normal table. Stored
/// as `[f32; 3]` rather than `Vec3A`: it is read once per closest hit, so
/// the 4 bytes of alignment padding a `Vec3A` carries would be pure waste
/// on a table that holds every smooth mesh's normals.
pub(crate) type VertexNormal = [f32; 3];

/// [`TrianglePrim::normals`] for a triangle with no shading normals.
pub(crate) const NO_NORMALS: [u32; 3] = [u32::MAX; 3];

impl TrianglePrim {
    /// The resident part of this triangle: everything but its vertices,
    /// which live only in the BVH's SIMD packets once it is built.
    pub(crate) fn record(&self) -> TriRecord {
        TriRecord {
            geom_id: self.geom_id,
            prim_id: self.prim_id,
            mask: self.mask,
            normals: self.normals,
        }
    }

    fn vertices(&self) -> [Vec3A; 3] {
        [self.v0, self.v1, self.v2]
    }
}

impl Prim for TrianglePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, normals: &[VertexNormal]) -> Option<PrimHit> {
        self.record()
            .hit(self.vertices(), ray, t_min, t_max, normals)
    }

    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        self.record().hit_any(self.vertices(), ray, t_min, t_max)
    }

    fn bbox(&self) -> AABB {
        triangle_aabb(self.v0, self.v1, self.v2)
    }

    fn clipped_aabb(&self, axis: usize, min: f32, max: f32) -> Option<AABB> {
        clip_triangle_aabb(self.v0, self.v1, self.v2, axis, min, max)
    }
}

/// What a built BVH keeps of a triangle: its ids, its mask and its normal
/// indices — 24 bytes. The vertices are not here: every triangle sits in at
/// least one `Tri4` packet, which already holds them exactly, so a hit reads
/// them from the lane it came from (`Tri4::lane_vertices`) when it needs
/// them at all (the scalar tie-break, and the geometric normal of a
/// triangle without shading normals).
pub(crate) struct TriRecord {
    pub geom_id: u32,
    pub prim_id: u32,
    pub mask: RayMask,
    pub normals: [u32; 3],
}

impl TriRecord {
    /// Completes a hit whose `(t, u, v)` are already known — the shared tail
    /// of the scalar and the 4-wide SIMD intersectors, so both derive the
    /// reported normal the same way. `vertices` is only called for a
    /// triangle without shading normals.
    #[inline]
    pub(crate) fn hit_from_barycentric(
        &self,
        normals: &[VertexNormal],
        vertices: impl FnOnce() -> [Vec3A; 3],
        t: f32,
        u: f32,
        v: f32,
    ) -> Option<PrimHit> {
        let outward = if self.normals == NO_NORMALS {
            let [v0, v1, v2] = vertices();
            let n = (v1 - v0).cross(v2 - v0);
            if n == Vec3A::ZERO {
                return None; // degenerate sliver
            }
            n.normalize()
        } else {
            let [n0, n1, n2] = self.normals.map(|i| Vec3A::from_array(normals[i as usize]));
            (n0 * (1.0 - u - v) + n1 * u + n2 * v).normalize()
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

    /// The scalar intersection of this triangle, whose vertices the caller
    /// supplies (from its packet lane, or from the build-time triangle).
    #[inline]
    pub(crate) fn hit(
        &self,
        [v0, v1, v2]: [Vec3A; 3],
        ray: &Ray,
        t_min: f32,
        t_max: f32,
        normals: &[VertexNormal],
    ) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let (t, u, v) = triangle_intersect(ray, v0, v1, v2, t_min, t_max)?;
        self.hit_from_barycentric(normals, || [v0, v1, v2], t, u, v)
    }

    #[inline]
    pub(crate) fn hit_any(
        &self,
        [v0, v1, v2]: [Vec3A; 3],
        ray: &Ray,
        t_min: f32,
        t_max: f32,
    ) -> bool {
        !masked_out(ray, self.mask) && triangle_intersect(ray, v0, v1, v2, t_min, t_max).is_some()
    }
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
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, _normals: &[VertexNormal]) -> Option<PrimHit> {
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
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, _normals: &[VertexNormal]) -> Option<PrimHit> {
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
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, _normals: &[VertexNormal]) -> Option<PrimHit> {
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
        let discriminant = half_b * half_b - a * c;
        if discriminant < 0.0 {
            return None;
        }
        let sqrt_d = discriminant.sqrt();
        for t in [(-half_b - sqrt_d) / a, (-half_b + sqrt_d) / a] {
            if t <= t_min || t >= t_max {
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
    pub p0: Vec3A,
    pub p1: Vec3A,
    pub r0: f32,
    pub r1: f32,
    pub geom_id: u32,
    pub prim_id: u32,
    pub mask: RayMask,
}

impl Prim for CurvePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, _normals: &[VertexNormal]) -> Option<PrimHit> {
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
        let a = AABB::new(
            self.p0 - Vec3A::splat(self.r0),
            self.p0 + Vec3A::splat(self.r0),
        );
        let b = AABB::new(
            self.p1 - Vec3A::splat(self.r1),
            self.p1 + Vec3A::splat(self.r1),
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
}

impl Prim for CubicCurvePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, _normals: &[VertexNormal]) -> Option<PrimHit> {
        if masked_out(ray, self.mask) {
            return None;
        }
        let (t, outward) =
            crate::curve::cubic_curve_intersect(ray, &self.cp, self.r0, self.r1, t_min, t_max)?;
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
    /// World-to-local at shutter time 0. The only transform a static
    /// instance keeps: rays go into local space through it, and the normal
    /// matrix is its linear part transposed — exact, so it is recomputed per
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
    /// [`NO_ID_OFFSET`] when the instance reports `geom_id` instead.
    pub id_offset: u32,
    pub mask: RayMask,
}

/// Local-to-world at shutter times 0 and 1 of a moving instance.
pub(crate) struct InstanceMotion {
    pub l2w: Affine3A,
    pub l2w_end: Affine3A,
}

/// An instance as the BVH build sees it: the resident [`InstancePrim`] plus
/// its world bounds, which only the build reads — once the tree exists, the
/// parent node's lane holds the same box.
pub(crate) struct BuildInstance {
    pub prim: InstancePrim,
    pub bounds: AABB,
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
            _ => (self.w2l, self.w2l.matrix3.transpose()),
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

impl Prim for BuildInstance {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32, _normals: &[VertexNormal]) -> Option<PrimHit> {
        self.prim.hit(ray, t_min, t_max)
    }

    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        self.prim.hit_any(ray, t_min, t_max)
    }

    fn bbox(&self) -> AABB {
        self.bounds
    }
}

// ---------------------------------------------------------------------
// BuildPrim: every primitive kind, as the BVH build consumes it.
// ---------------------------------------------------------------------

/// A primitive as the BVH *build* sees it: a closed, unboxed sum, so the
/// SBVH can bound and clip any kind (triangles keep their vertices here,
/// for exact spatial clipping). It is transient — [`Prims::from_build`]
/// moves every one into the resident per-kind arrays once the tree exists,
/// and the triangles leave their vertices behind in the packets.
///
/// `Instance` and `CubicCurve` stay boxed *here* for the reason they always
/// were: an enum is sized by its largest variant, and a build over millions
/// of triangles should not pay an instance's size per triangle. Resident,
/// each kind has its own array and none is boxed.
pub(crate) enum BuildPrim {
    Triangle(TrianglePrim),
    Sphere(SpherePrim),
    Disk(DiskPrim),
    Cylinder(CylinderPrim),
    Curve(CurvePrim),
    CubicCurve(Box<CubicCurvePrim>),
    Instance(Box<BuildInstance>),
}

impl BuildPrim {
    /// The brute-force reference the BVH tests compare traversal against.
    #[cfg(test)]
    pub(crate) fn hit(
        &self,
        ray: &Ray,
        t_min: f32,
        t_max: f32,
        normals: &[VertexNormal],
    ) -> Option<PrimHit> {
        match self {
            BuildPrim::Triangle(p) => p.hit(ray, t_min, t_max, normals),
            BuildPrim::Sphere(p) => p.hit(ray, t_min, t_max, normals),
            BuildPrim::Disk(p) => p.hit(ray, t_min, t_max, normals),
            BuildPrim::Cylinder(p) => p.hit(ray, t_min, t_max, normals),
            BuildPrim::Curve(p) => p.hit(ray, t_min, t_max, normals),
            BuildPrim::CubicCurve(p) => p.hit(ray, t_min, t_max, normals),
            BuildPrim::Instance(p) => p.hit(ray, t_min, t_max, normals),
        }
    }

    #[inline]
    pub(crate) fn bbox(&self) -> AABB {
        match self {
            BuildPrim::Triangle(p) => p.bbox(),
            BuildPrim::Sphere(p) => p.bbox(),
            BuildPrim::Disk(p) => p.bbox(),
            BuildPrim::Cylinder(p) => p.bbox(),
            BuildPrim::Curve(p) => p.bbox(),
            BuildPrim::CubicCurve(p) => p.bbox(),
            BuildPrim::Instance(p) => p.bbox(),
        }
    }

    /// `Some` for triangles only.
    #[inline]
    pub(crate) fn as_triangle(&self) -> Option<&TrianglePrim> {
        match self {
            BuildPrim::Triangle(p) => Some(p),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn clipped_aabb(&self, axis: usize, min: f32, max: f32) -> Option<AABB> {
        match self {
            BuildPrim::Triangle(p) => p.clipped_aabb(axis, min, max),
            BuildPrim::Sphere(p) => p.clipped_aabb(axis, min, max),
            BuildPrim::Disk(p) => p.clipped_aabb(axis, min, max),
            BuildPrim::Cylinder(p) => p.clipped_aabb(axis, min, max),
            BuildPrim::Curve(p) => p.clipped_aabb(axis, min, max),
            BuildPrim::CubicCurve(p) => p.clipped_aabb(axis, min, max),
            BuildPrim::Instance(p) => p.clipped_aabb(axis, min, max),
        }
    }
}

// ---------------------------------------------------------------------
// Prims: the resident primitives, one array per kind.
// ---------------------------------------------------------------------

/// The analytic primitives that have no array of their own: few in any real
/// scene (UsdLux shapes, linear curve segments), so one enum serves them.
pub(crate) enum OtherPrim {
    Sphere(SpherePrim),
    Disk(DiskPrim),
    Cylinder(CylinderPrim),
    Curve(CurvePrim),
}

impl OtherPrim {
    #[inline]
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        match self {
            OtherPrim::Sphere(p) => p.hit(ray, t_min, t_max, &[]),
            OtherPrim::Disk(p) => p.hit(ray, t_min, t_max, &[]),
            OtherPrim::Cylinder(p) => p.hit(ray, t_min, t_max, &[]),
            OtherPrim::Curve(p) => p.hit(ray, t_min, t_max, &[]),
        }
    }

    #[inline]
    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        match self {
            OtherPrim::Sphere(p) => p.hit_any(ray, t_min, t_max),
            OtherPrim::Disk(p) => p.hit_any(ray, t_min, t_max),
            OtherPrim::Cylinder(p) => p.hit_any(ray, t_min, t_max),
            OtherPrim::Curve(p) => p.hit_any(ray, t_min, t_max),
        }
    }
}

/// A leaf's scalar (non-packet) primitive reference: the kind in the top
/// two bits, the index into that kind's array below. Triangles never appear
/// here — every triangle is in a packet.
pub(crate) type PrimRefId = u32;
const KIND_SHIFT: u32 = 30;
const INDEX_MASK: u32 = (1 << KIND_SHIFT) - 1;
const KIND_OTHER: u32 = 0;
const KIND_INSTANCE: u32 = 1;
const KIND_CUBIC: u32 = 2;

/// The resident primitives of one BVH, stored once each, in input order
/// within each kind. Every array is exactly as long as its kind's count.
#[derive(Default)]
pub(crate) struct Prims {
    /// Indexed by `Tri4::prim[lane]`.
    pub tris: Box<[TriRecord]>,
    pub instances: Box<[InstancePrim]>,
    pub cubics: Box<[CubicCurvePrim]>,
    pub others: Box<[OtherPrim]>,
    /// Per-vertex shading normals, indexed by [`TriRecord::normals`].
    pub normals: Box<[VertexNormal]>,
}

impl Prims {
    /// For each build primitive, the id the finished tree refers to it by:
    /// a triangle's index into `tris` (what its packet lane stores), or a
    /// kind-tagged [`PrimRefId`] for everything else. Pure in the input
    /// order, so the build stays deterministic.
    pub(crate) fn resident_ids(build: &[BuildPrim]) -> Vec<u32> {
        let mut next = [0u32; 4]; // tris, other, instance, cubic
        let tagged = |kind: u32, n: &mut u32| {
            assert!(
                *n <= INDEX_MASK,
                "one BVH holds more than {INDEX_MASK} primitives of one kind"
            );
            let id = (kind << KIND_SHIFT) | *n;
            *n += 1;
            id
        };
        build
            .iter()
            .map(|p| match p {
                BuildPrim::Triangle(_) => {
                    let id = next[0];
                    next[0] = id.checked_add(1).expect("too many triangles in one BVH");
                    id
                }
                BuildPrim::Instance(_) => tagged(KIND_INSTANCE, &mut next[2]),
                BuildPrim::CubicCurve(_) => tagged(KIND_CUBIC, &mut next[3]),
                _ => tagged(KIND_OTHER, &mut next[1]),
            })
            .collect()
    }

    /// Moves the build primitives into their resident arrays, in the order
    /// [`Prims::resident_ids`] numbered them. Triangle vertices are dropped:
    /// the packets hold them.
    pub(crate) fn from_build(build: Vec<BuildPrim>, normals: Vec<VertexNormal>) -> Prims {
        let (mut n_tri, mut n_inst, mut n_cubic, mut n_other) = (0, 0, 0, 0);
        for p in &build {
            match p {
                BuildPrim::Triangle(_) => n_tri += 1,
                BuildPrim::Instance(_) => n_inst += 1,
                BuildPrim::CubicCurve(_) => n_cubic += 1,
                _ => n_other += 1,
            }
        }
        let mut tris = Vec::with_capacity(n_tri);
        let mut instances = Vec::with_capacity(n_inst);
        let mut cubics = Vec::with_capacity(n_cubic);
        let mut others = Vec::with_capacity(n_other);
        for p in build {
            match p {
                BuildPrim::Triangle(t) => tris.push(t.record()),
                BuildPrim::Instance(i) => instances.push(i.prim),
                BuildPrim::CubicCurve(c) => cubics.push(*c),
                BuildPrim::Sphere(p) => others.push(OtherPrim::Sphere(p)),
                BuildPrim::Disk(p) => others.push(OtherPrim::Disk(p)),
                BuildPrim::Cylinder(p) => others.push(OtherPrim::Cylinder(p)),
                BuildPrim::Curve(p) => others.push(OtherPrim::Curve(p)),
            }
        }
        Prims {
            tris: tris.into_boxed_slice(),
            instances: instances.into_boxed_slice(),
            cubics: cubics.into_boxed_slice(),
            others: others.into_boxed_slice(),
            normals: normals.into_boxed_slice(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.tris.len() + self.instances.len() + self.cubics.len() + self.others.len()
    }

    /// Closest hit on a leaf's scalar primitive.
    ///
    /// Kept out of line on purpose: inlined, it pulls the instance path —
    /// which re-enters `Bvh::hit` — into the traversal loop, and the bigger
    /// frame stopped LLVM from building the traversal stack in place. That
    /// cost ~7% of `Bvh::hit`'s instructions on cornellbox, more than the
    /// call does.
    #[inline(never)]
    pub(crate) fn hit(&self, id: PrimRefId, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        let i = (id & INDEX_MASK) as usize;
        match id >> KIND_SHIFT {
            KIND_INSTANCE => self.instances[i].hit(ray, t_min, t_max),
            KIND_CUBIC => self.cubics[i].hit(ray, t_min, t_max, &[]),
            _ => self.others[i].hit(ray, t_min, t_max),
        }
    }

    /// Out of line for the same reason as [`Prims::hit`].
    #[inline(never)]
    pub(crate) fn hit_any(&self, id: PrimRefId, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        let i = (id & INDEX_MASK) as usize;
        match id >> KIND_SHIFT {
            KIND_INSTANCE => self.instances[i].hit_any(ray, t_min, t_max),
            KIND_CUBIC => self.cubics[i].hit_any(ray, t_min, t_max),
            _ => self.others[i].hit_any(ray, t_min, t_max),
        }
    }
}
