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
    pub mask: u32,
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
    pub mask: u32,
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
    pub mask: u32,
}

impl Prim for CubicCurvePrim {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
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
    /// Local-to-world at shutter time 0.
    pub l2w: Affine3A,
    /// Cached world-to-local and inverse-transpose at time 0.
    pub w2l: Affine3A,
    pub normal_mat: Mat3A,
    /// Local-to-world at shutter time 1, when the instance moves.
    ///
    /// Boxed for the same reason as [`crate::Geometry::Instance`]'s
    /// `transform_end`, and it matters far more here: `Geometry` values
    /// are transient build inputs, whereas an `InstancePrim` is resident
    /// for the whole render. Inline, the `Option` costs 80 bytes on every
    /// instance — a scene with tens of millions of static placements pays
    /// gigabytes for a field none of them use.
    pub l2w_end: Option<Box<Affine3A>>,
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
        match &self.l2w_end {
            Some(end) if time > 0.0 => {
                let w2l = lerp_affine(&self.l2w, end.as_ref(), time).inverse();
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
        // Attribute the nested traversal to the instance level, so
        // top-level and instanced work can be told apart.
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::enter_instance();
        let inner = self.scene.intersect_outward(&local, t_min, t_max);
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::leave_instance();
        let mut hit = inner?;
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
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::enter_instance();
        let occluded = self.scene.occluded(&self.to_local(ray, &w2l), t_min, t_max);
        #[cfg(feature = "traversal-stats")]
        crate::bvh::stats::leave_instance();
        occluded
    }

    fn bbox(&self) -> AABB {
        self.bounds
    }
}

// ---------------------------------------------------------------------
// PrimNode: a closed, unboxed sum of the prim kinds.
// ---------------------------------------------------------------------

/// The BVH's actual primitive storage. A trait object (`Box<dyn Prim>`)
/// puts every primitive in its own heap allocation — fine for a handful
/// of meshes, but a dense `PointInstancer`-free curve archive (xgen
/// "grass"/"groundcover") attaches tens of millions of individual curve
/// segments, and tens of millions of separate small allocations cost real
/// memory in allocator bookkeeping alone, on top of losing locality
/// during traversal. `PrimNode` stores the same four kinds inline in one
/// contiguous `Vec`, dispatching by match instead of vtable.
///
/// `Instance` is boxed because `InstancePrim` (two cached transforms plus
/// an optional motion-blur end transform) is far larger than the other
/// three variants — an enum's size is its largest variant's, so leaving
/// it inline would make every triangle and curve pay Instance's size for
/// nothing. The other three stay inline: none of them are worth an
/// allocation on their own.
pub(crate) enum PrimNode {
    Triangle(TrianglePrim),
    Sphere(SpherePrim),
    Disk(DiskPrim),
    Cylinder(CylinderPrim),
    Curve(CurvePrim),
    /// Boxed for the same reason as `Instance`: at 4 `Vec3A` control
    /// points, this is bigger than every other variant, and an enum's
    /// size is its largest variant's — inlining it would tax every
    /// triangle, sphere and linear-curve segment everywhere in the
    /// kernel for a variant most of them will never be.
    CubicCurve(Box<CubicCurvePrim>),
    Instance(Box<InstancePrim>),
}

impl PrimNode {
    #[inline]
    pub(crate) fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        match self {
            PrimNode::Triangle(p) => p.hit(ray, t_min, t_max),
            PrimNode::Sphere(p) => p.hit(ray, t_min, t_max),
            PrimNode::Disk(p) => p.hit(ray, t_min, t_max),
            PrimNode::Cylinder(p) => p.hit(ray, t_min, t_max),
            PrimNode::Curve(p) => p.hit(ray, t_min, t_max),
            PrimNode::CubicCurve(p) => p.hit(ray, t_min, t_max),
            PrimNode::Instance(p) => p.hit(ray, t_min, t_max),
        }
    }

    #[inline]
    pub(crate) fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        match self {
            PrimNode::Triangle(p) => p.hit_any(ray, t_min, t_max),
            PrimNode::Sphere(p) => p.hit_any(ray, t_min, t_max),
            PrimNode::Disk(p) => p.hit_any(ray, t_min, t_max),
            PrimNode::Cylinder(p) => p.hit_any(ray, t_min, t_max),
            PrimNode::Curve(p) => p.hit_any(ray, t_min, t_max),
            PrimNode::CubicCurve(p) => p.hit_any(ray, t_min, t_max),
            PrimNode::Instance(p) => p.hit_any(ray, t_min, t_max),
        }
    }

    #[inline]
    pub(crate) fn bbox(&self) -> AABB {
        match self {
            PrimNode::Triangle(p) => p.bbox(),
            PrimNode::Sphere(p) => p.bbox(),
            PrimNode::Disk(p) => p.bbox(),
            PrimNode::Cylinder(p) => p.bbox(),
            PrimNode::Curve(p) => p.bbox(),
            PrimNode::CubicCurve(p) => p.bbox(),
            PrimNode::Instance(p) => p.bbox(),
        }
    }

    /// `Some` for triangles only — see `Prim::as_triangle`.
    #[inline]
    pub(crate) fn as_triangle(&self) -> Option<&TrianglePrim> {
        match self {
            PrimNode::Triangle(p) => Some(p),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn clipped_aabb(&self, axis: usize, min: f32, max: f32) -> Option<AABB> {
        match self {
            PrimNode::Triangle(p) => p.clipped_aabb(axis, min, max),
            PrimNode::Sphere(p) => p.clipped_aabb(axis, min, max),
            PrimNode::Disk(p) => p.clipped_aabb(axis, min, max),
            PrimNode::Cylinder(p) => p.clipped_aabb(axis, min, max),
            PrimNode::Curve(p) => p.clipped_aabb(axis, min, max),
            PrimNode::CubicCurve(p) => p.clipped_aabb(axis, min, max),
            PrimNode::Instance(p) => p.clipped_aabb(axis, min, max),
        }
    }
}
