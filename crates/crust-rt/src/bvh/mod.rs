//! Bounding-volume hierarchy over the internal primitives.
//!
//! The build is reference-based, in the SBVH mold (Stich et al. 2009):
//! every subtree owns a list of `PrimRef`s (bbox + primitive index). Each
//! node first evaluates a binned surface-area-heuristic *object* split on
//! the longest centroid axis; when the object split's children overlap by
//! more than `SBVH_ALPHA` of the root surface area it also evaluates a
//! binned *spatial* split, which chops straddling references in two at the
//! split plane (bounds via `Prim::clipped_aabb` — exact polygon clipping
//! for triangles) and duplicates them into both children. The cheaper
//! split wins. A primitive can therefore appear in several leaves, while
//! the primitives themselves are stored exactly once.
//!
//! The binary tree is then collapsed into 4-wide SoA nodes whose slab
//! tests run on `Vec4` lanes, with the lane verdicts extracted as a bitmask
//! rather than read back one at a time. (With the nightly-only `bvh8`
//! feature the nodes are 8 wide on `std::simd::f32x8` instead — see
//! `lanes8.rs`; everything below is written against [`LANES`].) Each leaf's
//! payload lands in the separate `leaves` table — which keeps [`WideNode`] at two cache lines —
//! and its triangles are packed into 4-wide [`Tri4`] packets so a leaf
//! intersects four triangles per vector round; everything else (spheres,
//! curves, instances) keeps a scalar index in `indices`.
//!
//! Large subtrees build in parallel via `rayon::join`; every split decision
//! depends only on the input, so the tree is deterministic — threads only
//! change *when* subtrees are built, never *what*.

use build::{build_subtree, surface_area, union_all};
use collapse::{LeafData, collapse};

use crate::aabb::{AABB, triangle_aabb};
use crate::prim::{
    CubicCurvePrim, GeomTable, InstancePrim, NO_NORMALS, Prim, PrimHit, PrimNode, TriangleRecord,
    clip_box, triangle_hit_from_barycentric,
};
use crate::ray::Ray;
use crate::scene::PrimitiveBreakdown;
use crate::triangle::{
    Hit4, RayShear, Tri4, Tri4i, clip_triangle_aabb, triangle_intersect_sheared,
};
use glam::Vec3A;

/// Diagnostic traversal counters, compiled out unless the
/// `traversal-stats` feature is on — they sit in the innermost loop.
///
/// Deliberately global relaxed atomics rather than per-thread state:
/// this exists to answer "how many nodes does a ray touch", and the
/// contention that makes the timings meaningless does not affect the
/// counts. Read the counts from a feature-on build and the timings from a
/// feature-off one.
#[cfg(feature = "traversal-stats")]
pub mod stats;

/// Increments a traversal counter when the diagnostic feature is on, and
/// compiles to nothing when it is off.
macro_rules! tstat {
    ($name:ident, $n:expr) => {{
        #[cfg(feature = "traversal-stats")]
        stats::bump(&stats::$name, $n);
    }};
}

mod build;
mod collapse;
#[cfg(not(feature = "bvh8"))]
mod lanes4;
#[cfg(feature = "bvh8")]
mod lanes8;
#[cfg(not(feature = "bvh8"))]
use lanes4::{LANES, Lanes, RaySlab, splat};
#[cfg(feature = "bvh8")]
use lanes8::{LANES, Lanes, RaySlab, splat};

/// Leaves are forced at this depth so the traversal stack can never
/// overflow; SAH partitions are otherwise free to be arbitrarily uneven.
const MAX_DEPTH: usize = 60;
/// Ranges at or below this size always become a leaf.
const MIN_LEAF: usize = 2;
/// The leaf floor for ranges made *entirely* of triangles. Those are
/// intersected four at a time by one SIMD packet, so a 4-primitive leaf
/// costs the same vector round as a 1-primitive one — stopping at 2 would
/// leave half the lanes idle and pay for a node test that buys nothing.
/// Non-packable primitives (spheres, curves, instances) keep [`MIN_LEAF`]:
/// for them a bigger leaf really is more work. Measured on the
/// `ray_throughput` example, raising the floor for everything cost
/// instanced scenes ~12% while raising it for triangles alone gains ~12%.
const MIN_LEAF_PACKED: usize = 4;
/// A range no larger than this may stay a leaf when splitting is not
/// worth it by SAH cost; larger ranges are always split.
const MAX_LEAF: usize = 8;
/// Number of candidate split planes tested per axis.
const BINS: usize = 12;
/// Subtrees larger than this build their children on parallel rayon tasks.
const PARALLEL_THRESHOLD: usize = 4096;
/// Spatial splits are only *considered* when the object split's children
/// overlap by more than this fraction of the root surface area (the SBVH
/// α of Stich et al. 2009) — the natural brake on reference duplication.
const SBVH_ALPHA: f32 = 1e-5;
/// And never below this depth: chopping tiny deep subtrees duplicates
/// references for negligible traversal gain.
const SBVH_MAX_DEPTH: usize = 32;

/// Binary build node — an intermediate: the finished tree is the 4-wide
/// [`WideNode`] array produced by collapsing these.
///
/// 32 bytes: the bounds are stored unpadded. As two `Vec3A`s they made
/// this 48, and the build holds roughly one node per two references, so
/// on a scene of 100 M references the padding alone was 800 MiB of the
/// commit transient (`a_build_reference_is_28_bytes` pins both).
struct Node {
    min: [f32; 3],
    max: [f32; 3],
    /// Leaf (`count > 0`): offset of the first entry in `indices`.
    /// Internal (`count == 0`): index of the right child — the left child
    /// immediately follows the node itself in depth-first order.
    first_or_right: u32,
    count: u32,
}

impl Node {
    #[inline]
    fn new(bbox: AABB, first_or_right: u32, count: u32) -> Self {
        Node {
            min: bbox.minimum.to_array(),
            max: bbox.maximum.to_array(),
            first_or_right,
            count,
        }
    }

    #[inline]
    fn bbox(&self) -> AABB {
        AABB::new(Vec3A::from_array(self.min), Vec3A::from_array(self.max))
    }
}

/// Marks an unused lane of a [`WideNode`].
const EMPTY_LANE: u32 = u32::MAX;

/// Lane-validity bits of [`WideNode::flags`] (bit `k` = lane `k` holds a
/// real child) and leaf bits (bit `LANES + k` = that child is a leaf).
const VALID_MASK: u32 = (1 << LANES) - 1;
const LEAF_SHIFT: u32 = LANES as u32;

/// A [`LANES`]-wide BVH node in SoA layout: lane `k` of each vector holds
/// child `k`'s slab bounds, so one round of vector min/max tests every child
/// box against the ray at once (Embree's BVH4 / BVH8 idea).
///
/// At the default width exactly 128 bytes (two cache lines): six `Vec4`s,
/// one child index per lane, and the flag nibbles; 256 bytes under `bvh8`.
/// The leaf payload — how many primitives, and where their SIMD packets
/// live — sits in the separate [`Leaf`] table
/// rather than in the node, which is what keeps the node this small.
struct WideNode {
    bmin_x: Lanes,
    bmin_y: Lanes,
    bmin_z: Lanes,
    bmax_x: Lanes,
    bmax_y: Lanes,
    bmax_z: Lanes,
    /// Leaf lane: index into the BVH's `leaves`. Internal lane: index into
    /// `wide`. Unused lane: [`EMPTY_LANE`].
    child: [u32; LANES],
    /// Bits `0..LANES`: lane `k` holds a real child. Bits `LANES..2·LANES`:
    /// that child is a leaf.
    ///
    /// The validity bits exist because unused lanes carry +INF/+INF
    /// bounds, which *usually* fail the slab test but not always: for
    /// `t_max == INF` and an all-positive ray direction the test reduces to
    /// `INF <= INF`. Traversal ANDs the nibble into the SIMD hit mask — one
    /// integer `and` in place of four per-lane branches.
    flags: u32,
}

/// A leaf's payload: the 4-wide triangle packets to run, plus the leftover
/// primitives (spheres, curves, instances, and the triangles that did not
/// fill a packet's worth) that still go one at a time.
struct Leaf {
    /// Range in the BVH's `packets`.
    pkt_first: u32,
    pkt_count: u32,
    /// Range in the BVH's `indices`.
    idx_first: u32,
    idx_count: u32,
}

impl WideNode {
    fn empty() -> Self {
        let inf = splat(f32::INFINITY);
        WideNode {
            bmin_x: inf,
            bmin_y: inf,
            bmin_z: inf,
            bmax_x: inf,
            bmax_y: inf,
            bmax_z: inf,
            child: [EMPTY_LANE; LANES],
            flags: 0,
        }
    }

    fn set_lane_bounds(&mut self, lane: usize, b: &AABB) {
        self.bmin_x[lane] = b.minimum.x;
        self.bmin_y[lane] = b.minimum.y;
        self.bmin_z[lane] = b.minimum.z;
        self.bmax_x[lane] = b.maximum.x;
        self.bmax_y[lane] = b.maximum.y;
        self.bmax_z[lane] = b.maximum.z;
        self.flags |= 1 << lane;
    }

    #[inline]
    fn is_leaf(&self, lane: usize) -> bool {
        self.flags & (1 << (LEAF_SHIFT + lane as u32)) != 0
    }

    fn mark_leaf(&mut self, lane: usize) {
        self.flags |= 1 << (LEAF_SHIFT + lane as u32);
    }
}

/// Which packet table a tree uses — the resolved form of
/// [`crate::PacketLayout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    Gathered,
    Indexed,
}

/// Everything a scene expands into before its tree is built: the triangle
/// records with the vertex and normal tables they index, the per-geometry
/// table that says where each geometry's entries start, and the primitives
/// that are not triangles. [`Bvh::new`] takes it by value and boxes it.
///
/// The build sees one index space over all of it: `0..tris.len()` are
/// records, `tris.len() + k` is the `k`-th non-triangle *in attach order* —
/// see [`Primitives::is_triangle`]. The non-triangles live in one array per
/// kind, so `order` says where each one went. Keeping the attach order is
/// what keeps the build's references, its ties and its leaf order — and so
/// every query result — exactly what they were when all of them shared one
/// array.
#[derive(Default)]
pub(crate) struct Primitives {
    pub(crate) tris: Vec<TriangleRecord>,
    /// Every triangle mesh's vertices, concatenated in attach order.
    pub(crate) vertices: Vec<[f32; 3]>,
    /// Per-vertex shading normals of the meshes that have them, each mesh's
    /// run parallel to its vertex run (`GeomTable::normal_base`).
    pub(crate) normals: Vec<[f32; 3]>,
    pub(crate) geoms: Vec<GeomTable>,
    /// Spheres, disks, cylinders and linear curve segments.
    pub(crate) prims: Vec<PrimNode>,
    /// Instances, inline (no box, no `PrimNode` slot).
    pub(crate) instances: Vec<InstancePrim>,
    /// World bounds of `instances[i]`. Build-only: the finished tree's
    /// parent lane holds the same box, so the BVH drops this.
    pub(crate) instance_bounds: Vec<AABB>,
    /// Cubic curve spans, inline.
    pub(crate) cubics: Vec<CubicCurvePrim>,
    /// Build-only: the resident id ([`kind_tagged`]) of the `k`-th
    /// non-triangle in attach order.
    pub(crate) order: Vec<u32>,
}

/// A leaf's scalar (non-packet) primitive reference: the kind in the top two
/// bits, the index into that kind's array below.
const KIND_SHIFT: u32 = 30;
const INDEX_MASK: u32 = (1 << KIND_SHIFT) - 1;
const KIND_NODE: u32 = 0;
const KIND_INSTANCE: u32 = 1;
const KIND_CUBIC: u32 = 2;

/// The resident id of the `index`-th primitive of `kind`.
fn kind_tagged(kind: u32, index: usize) -> u32 {
    assert!(
        index <= INDEX_MASK as usize,
        "one BVH holds more than {INDEX_MASK} primitives of one kind"
    );
    (kind << KIND_SHIFT) | index as u32
}

impl Primitives {
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.tris.len() + self.order.len()
    }

    /// Appends an analytic primitive, in attach order.
    pub(crate) fn push_node(&mut self, p: PrimNode) {
        self.order.push(kind_tagged(KIND_NODE, self.prims.len()));
        self.prims.push(p);
    }

    /// Appends an instance and its world bounds, in attach order.
    pub(crate) fn push_instance(&mut self, p: InstancePrim, bounds: AABB) {
        self.order
            .push(kind_tagged(KIND_INSTANCE, self.instances.len()));
        self.instances.push(p);
        self.instance_bounds.push(bounds);
    }

    /// Appends a cubic curve span, in attach order.
    pub(crate) fn push_cubic(&mut self, p: CubicCurvePrim) {
        self.order.push(kind_tagged(KIND_CUBIC, self.cubics.len()));
        self.cubics.push(p);
    }

    /// The resident id a leaf stores for non-triangle build index `idx`.
    #[inline]
    pub(crate) fn resident_id(&self, idx: u32) -> u32 {
        self.order[idx as usize - self.tris.len()]
    }

    #[inline]
    pub(crate) fn is_triangle(&self, idx: u32) -> bool {
        (idx as usize) < self.tris.len()
    }

    /// The three vertices of record `rec`, gathered from the table.
    #[inline]
    pub(crate) fn tri_verts(&self, rec: &TriangleRecord) -> [Vec3A; 3] {
        [
            Vec3A::from_array(self.vertices[rec.v[0] as usize]),
            Vec3A::from_array(self.vertices[rec.v[1] as usize]),
            Vec3A::from_array(self.vertices[rec.v[2] as usize]),
        ]
    }

    /// Bounds of the primitive at build index `idx`; `None` for a record
    /// the build must not reference: one whose attached indices were out of
    /// range, or — on a geometry without shading normals — a sliver whose
    /// geometric normal is exactly zero. Closest-hit traversal rejected
    /// such a sliver on every candidate hit (it has no normal to report), so
    /// leaving it out of the tree changes no reported hit and spares every
    /// candidate the vertex gather that check cost. A geometry with normals
    /// keeps its slivers: they shade by interpolation and were never
    /// rejected.
    pub(crate) fn bbox(&self, idx: u32) -> Option<AABB> {
        if self.is_triangle(idx) {
            let r = &self.tris[idx as usize];
            if r.is_degenerate() {
                return None;
            }
            let [a, b, c] = self.tri_verts(r);
            if self.geoms[r.geom_id as usize].normal_base == NO_NORMALS
                && (b - a).cross(c - a) == Vec3A::ZERO
            {
                return None;
            }
            Some(triangle_aabb(a, b, c))
        } else {
            let id = self.resident_id(idx);
            let i = (id & INDEX_MASK) as usize;
            Some(match id >> KIND_SHIFT {
                KIND_INSTANCE => self.instance_bounds[i],
                KIND_CUBIC => self.cubics[i].bbox(),
                _ => self.prims[i].bbox(),
            })
        }
    }

    /// `Prim::clipped_aabb` over the unified index space.
    pub(crate) fn clipped_aabb(&self, idx: u32, axis: usize, min: f32, max: f32) -> Option<AABB> {
        if self.is_triangle(idx) {
            let [a, b, c] = self.tri_verts(&self.tris[idx as usize]);
            clip_triangle_aabb(a, b, c, axis, min, max)
        } else {
            let id = self.resident_id(idx);
            let i = (id & INDEX_MASK) as usize;
            match id >> KIND_SHIFT {
                KIND_INSTANCE => clip_box(self.instance_bounds[i], axis, min, max),
                KIND_CUBIC => self.cubics[i].clipped_aabb(axis, min, max),
                _ => self.prims[i].clipped_aabb(axis, min, max),
            }
        }
    }
}

/// A finished tree. Its tables are boxed slices, not `Vec`s: it is immutable, and
/// a box can neither grow nor hold capacity slack the memory footprint would
/// have to count.
pub(crate) struct Bvh {
    wide: Box<[WideNode]>,
    /// Leaf payloads, indexed by a leaf lane's `child`.
    leaves: Box<[Leaf]>,
    /// 4-wide triangle packets, grouped per leaf; one of the two tables
    /// is empty, per `layout`.
    packets: Box<[Tri4]>,
    packets_i: Box<[Tri4i]>,
    layout: Layout,
    /// The one-at-a-time primitives of each leaf, as kind-tagged ids into
    /// `prims` / `instances` / `cubics`; spatial splits may list a primitive
    /// in more than one leaf.
    indices: Box<[u32]>,
    /// Triangle records, stored once each, in input order; packet lanes
    /// index them.
    tris: Box<[TriangleRecord]>,
    /// The shared vertex table every record and (indexed) packet reads.
    vertices: Box<[[f32; 3]]>,
    /// Per-vertex shading normals, see [`Primitives::normals`].
    normals: Box<[[f32; 3]]>,
    geoms: Box<[GeomTable]>,
    /// The analytic primitives, stored once each, in input order.
    prims: Box<[PrimNode]>,
    /// Instances, 96 bytes each, inline.
    instances: Box<[InstancePrim]>,
    /// Cubic curve spans, inline.
    cubics: Box<[CubicCurvePrim]>,
    /// Records that refer to no vertex (out-of-range input), kept only so
    /// `prim_id`s stay dense; not primitives for counting purposes.
    n_degenerate: usize,
    /// Bounds of the whole tree (the binary root's, kept through collapse).
    root_bbox: Option<AABB>,
}

/// One build reference: conservative bounds of (a fragment of) primitive
/// `idx`. Spatial splits shrink the bounds and duplicate the reference.
///
/// 28 bytes, unpadded, for the same reason as [`Node`]: the build's peak
/// is set by the references alive across the recursion, and an `AABB` of
/// two `Vec3A`s made this 48.
#[derive(Clone, Copy)]
struct PrimRef {
    min: [f32; 3],
    max: [f32; 3],
    idx: u32,
}

impl PrimRef {
    #[inline]
    fn new(bbox: AABB, idx: u32) -> Self {
        PrimRef {
            min: bbox.minimum.to_array(),
            max: bbox.maximum.to_array(),
            idx,
        }
    }

    #[inline]
    fn bbox(&self) -> AABB {
        AABB::new(Vec3A::from_array(self.min), Vec3A::from_array(self.max))
    }

    #[inline]
    fn centroid(&self) -> Vec3A {
        0.5 * (Vec3A::from_array(self.min) + Vec3A::from_array(self.max))
    }
}

/// A built subtree with node indices and leaf offsets local to itself;
/// `merge` splices children under a parent, offsetting as it goes.
struct Subtree {
    nodes: Vec<Node>,
    indices: Vec<u32>,
}

/// What closest-hit traversal carries while it searches: a scalar
/// primitive's finished hit, or a packet lane whose normal is still to be
/// derived (see `Bvh::resolve`).
#[derive(Clone, Copy)]
enum Candidate {
    Prim(PrimHit),
    Lane { rec: u32, t: f32, u: f32, v: f32 },
}

impl Candidate {
    #[inline]
    fn t(&self) -> f32 {
        match self {
            Candidate::Prim(h) => h.t,
            Candidate::Lane { t, .. } => *t,
        }
    }
}

impl Bvh {
    pub(crate) fn new(input: Primitives, layout: Layout, packet_sah: bool) -> Self {
        // One reference per primitive, in input order (the order decides
        // ties, so it is part of the build's determinism); degenerate
        // records get none.
        let refs: Vec<PrimRef> = (0..input.len() as u32)
            .filter_map(|i| input.bbox(i).map(|bbox| PrimRef::new(bbox, i)))
            .collect();

        let (wide, collected, root_bbox) = if refs.is_empty() {
            (Vec::new(), LeafData::default(), None)
        } else {
            let root_bbox = union_all(&refs);
            let subtree = build_subtree(&input, refs, 0, surface_area(&root_bbox), packet_sah);
            let (wide, collected) = collapse(&subtree.nodes, &subtree.indices, &input, layout);
            (wide, collected, Some(root_bbox))
        };

        let n_degenerate = input.tris.iter().filter(|r| r.is_degenerate()).count();
        Bvh {
            wide: wide.into_boxed_slice(),
            leaves: collected.leaves.into_boxed_slice(),
            packets: collected.packets.into_boxed_slice(),
            packets_i: collected.packets_i.into_boxed_slice(),
            layout,
            indices: collected.indices.into_boxed_slice(),
            tris: input.tris.into_boxed_slice(),
            n_degenerate,
            vertices: input.vertices.into_boxed_slice(),
            normals: input.normals.into_boxed_slice(),
            geoms: input.geoms.into_boxed_slice(),
            prims: input.prims.into_boxed_slice(),
            instances: input.instances.into_boxed_slice(),
            cubics: input.cubics.into_boxed_slice(),
            root_bbox,
        }
        // `input.order` and `input.instance_bounds` drop here: build-only.
    }

    /// The three vertices of a record, gathered from the shared table.
    #[inline]
    fn tri_verts(&self, rec: &TriangleRecord) -> [Vec3A; 3] {
        [
            Vec3A::from_array(self.vertices[rec.v[0] as usize]),
            Vec3A::from_array(self.vertices[rec.v[1] as usize]),
            Vec3A::from_array(self.vertices[rec.v[2] as usize]),
        ]
    }

    /// The record's three per-vertex shading normals, when its geometry has
    /// them.
    #[inline]
    fn tri_normals(&self, rec: &TriangleRecord) -> Option<[Vec3A; 3]> {
        let g = &self.geoms[rec.geom_id as usize];
        if g.normal_base == NO_NORMALS {
            return None;
        }
        let at = |vi: u32| {
            let k = (g.normal_base + (vi - g.vertex_base)) as usize;
            Vec3A::from_array(self.normals[k])
        };
        Some([at(rec.v[0]), at(rec.v[1]), at(rec.v[2])])
    }

    /// The hit a candidate stands for, its normal derived now — once, for
    /// the lane that won — rather than for every lane that briefly held the
    /// record: the interpolated shading normal through the record's vertex
    /// indices when the geometry has normals, else the geometric normal.
    /// Exactly what `triangle_hit_from_barycentric` computes from the same
    /// inputs, and it cannot meet the zero cross product that function
    /// rejects, because `Primitives::bbox` keeps such slivers out of the
    /// tree.
    ///
    #[inline]
    fn resolve(&self, c: Candidate) -> PrimHit {
        match c {
            Candidate::Prim(h) => h,
            Candidate::Lane { rec, t, u, v } => {
                let rec = &self.tris[rec as usize];
                let outward = match self.tri_normals(rec) {
                    Some([n0, n1, n2]) => (n0 * (1.0 - u - v) + n1 * u + n2 * v).normalize(),
                    None => {
                        let [a, b, c] = self.tri_verts(rec);
                        let n = (b - a).cross(c - a);
                        debug_assert!(n != Vec3A::ZERO, "slivers are excluded at commit");
                        n.normalize()
                    }
                };
                PrimHit {
                    t,
                    outward,
                    u,
                    v,
                    geom_id: rec.geom_id,
                    prim_id: rec.prim_id,
                }
            }
        }
    }

    /// The scalar test of one record — the packet lanes' `f64` tie-break.
    /// Rare (a ray exactly through an edge), so kept out of the hot loop.
    #[inline(never)]
    fn tri_hit(
        &self,
        rec_idx: u32,
        ray: &Ray,
        shear: &RayShear,
        t_min: f32,
        t_max: f32,
    ) -> Option<PrimHit> {
        let rec = &self.tris[rec_idx as usize];
        if !ray.mask.sees(rec.mask) {
            return None;
        }
        let verts = self.tri_verts(rec);
        let (t, u, v) = triangle_intersect_sheared(
            shear, ray.origin, verts[0], verts[1], verts[2], t_min, t_max,
        )?;
        triangle_hit_from_barycentric(rec, &verts, self.tri_normals(rec), t, u, v)
    }

    #[inline(never)]
    fn tri_hit_any(
        &self,
        rec_idx: u32,
        ray: &Ray,
        shear: &RayShear,
        t_min: f32,
        t_max: f32,
    ) -> bool {
        let rec = &self.tris[rec_idx as usize];
        if !ray.mask.sees(rec.mask) {
            return false;
        }
        let [a, b, c] = self.tri_verts(rec);
        triangle_intersect_sheared(shear, ray.origin, a, b, c, t_min, t_max).is_some()
    }

    /// The vertices of triangle `prim_id` of geometry `geom_id`, or `None`
    /// when there is no such triangle (a non-mesh geometry, an id past the
    /// end, or a triangle whose attached indices were out of range).
    pub(crate) fn triangle_vertices(&self, geom_id: u32, prim_id: u32) -> Option<[Vec3A; 3]> {
        let g = self.geoms.get(geom_id as usize)?;
        let rec = self.tris.get(g.tri_base.checked_add(prim_id)? as usize)?;
        if rec.geom_id != geom_id || rec.is_degenerate() {
            return None;
        }
        Some(self.tri_verts(rec))
    }

    pub(crate) fn prim_count(&self) -> usize {
        self.triangle_count() + self.prims.len() + self.instances.len() + self.cubics.len()
    }

    /// Triangles that refer to real vertices.
    #[inline]
    fn triangle_count(&self) -> usize {
        self.tris.len() - self.n_degenerate
    }

    #[cfg(feature = "traversal-stats")]
    pub(crate) fn instances(&self) -> &[InstancePrim] {
        &self.instances
    }

    /// `(count, sum of bbox diagonals, max diagonal)` over top-level
    /// primitives — feeds [`crate::Scene::primitive_extents`], a diagnostic.
    /// Computed on request, never at commit: every prototype scene is
    /// committed too, and none of them is ever asked.
    ///
    /// Instances keep no bounds once built, so theirs are recomputed
    /// ([`InstancePrim::approx_world_bounds`]), and the kinds are summed one
    /// after another rather than in attach order: the figure can differ from
    /// the build's own boxes by a few ulps, which a diagnostic printed to a
    /// tenth can afford.
    pub(crate) fn primitive_extent_sum(&self) -> (usize, f32, f32) {
        let mut sum = 0.0f32;
        let mut max = 0.0f32;
        let mut n = 0usize;
        let mut add = |b: AABB| {
            let d = (b.maximum - b.minimum).length();
            sum += d;
            max = max.max(d);
            n += 1;
        };
        for r in self.tris.iter().filter(|r| !r.is_degenerate()) {
            let [a, b, c] = self.tri_verts(r);
            add(triangle_aabb(a, b, c));
        }
        for p in &self.prims {
            add(p.bbox());
        }
        for p in &self.cubics {
            add(p.bbox());
        }
        for i in &self.instances {
            if let Some(b) = i.approx_world_bounds() {
                add(b);
            }
        }
        (n, sum, max)
    }

    pub(crate) fn primitive_breakdown(&self) -> PrimitiveBreakdown {
        let mut b = PrimitiveBreakdown {
            triangles: self.triangle_count(),
            ..PrimitiveBreakdown::default()
        };
        self.count_non_triangles(&mut b);
        b
    }

    /// Adds this BVH's own non-triangle primitives to `b`, by kind.
    fn count_non_triangles(&self, b: &mut PrimitiveBreakdown) {
        b.instances += self.instances.len();
        b.cubic_curve_spans += self.cubics.len();
        for p in &self.prims {
            match p {
                PrimNode::Sphere(_) => b.spheres += 1,
                PrimNode::Disk(_) => b.disks += 1,
                PrimNode::Cylinder(_) => b.cylinders += 1,
                PrimNode::Curve(_) => b.curve_segments += 1,
            }
        }
    }

    /// Adds this BVH's resident bytes to `acc`, descending into each
    /// distinct instanced scene once. Uses `capacity`, not `len`: unused
    /// capacity is resident too, and over-allocation is exactly the kind
    /// of waste a memory report should not hide.
    pub(crate) fn accumulate_footprint(
        &self,
        visited: &mut std::collections::HashSet<usize>,
        acc: &mut crate::scene::MemoryFootprint,
    ) {
        use std::mem::size_of_val;
        acc.prim_nodes += size_of_val(&*self.prims);
        acc.bvh_nodes += size_of_val(&*self.wide);
        acc.leaves += size_of_val(&*self.leaves);
        acc.packets += size_of_val(&*self.packets);
        acc.packets_indexed += size_of_val(&*self.packets_i);
        acc.indices += size_of_val(&*self.indices);
        acc.triangle_records += size_of_val(&*self.tris);
        acc.vertices += size_of_val(&*self.vertices);
        acc.vertex_normals += size_of_val(&*self.normals);
        acc.geometry_tables += size_of_val(&*self.geoms);
        acc.lanes += 4 * (self.packets.len() + self.packets_i.len());
        acc.lanes_filled += self
            .packets
            .iter()
            .map(|p| p.lanes.active.count_ones() as usize)
            .chain(
                self.packets_i
                    .iter()
                    .map(|p| p.lanes.active.count_ones() as usize),
            )
            .sum::<usize>();
        acc.instances += size_of_val(&*self.instances);
        acc.cubic_spans += size_of_val(&*self.cubics);
        for i in &self.instances {
            if i.motion.is_some() {
                acc.instances += size_of::<crate::prim::InstanceMotion>();
            }
            if visited.insert(std::sync::Arc::as_ptr(&i.scene) as usize) {
                i.scene.accumulate_footprint_into(visited, acc);
            }
        }
    }

    /// Adds this BVH's primitives to `acc`, descending into each *distinct*
    /// instanced scene exactly once — `visited` holds the `Arc` addresses
    /// already counted. The result is the geometry actually resident in
    /// memory: a prototype shared by a thousand placements is counted once,
    /// which is precisely what distinguishes instanced from baked geometry.
    pub(crate) fn accumulate_unique(
        &self,
        visited: &mut std::collections::HashSet<usize>,
        acc: &mut PrimitiveBreakdown,
    ) {
        acc.triangles += self.triangle_count();
        self.count_non_triangles(acc);
        for i in &self.instances {
            if visited.insert(std::sync::Arc::as_ptr(&i.scene) as usize) {
                i.scene.accumulate_unique_into(visited, acc);
            }
        }
    }

    /// Closest hit on a leaf's scalar (non-packet) primitive, by its
    /// kind-tagged id.
    ///
    /// Out of line on purpose (the trap `compact-triangle-storage` measured):
    /// inlined, curve subdivision, cone intersection and the instance
    /// descent spill `Bvh::hit`'s traversal loop — +6% instructions on
    /// cornellbox, +17% on materialx_basic. The packet path, which is the
    /// hot one, never comes here.
    #[inline(never)]
    fn scalar_hit(&self, id: u32, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        let i = (id & INDEX_MASK) as usize;
        match id >> KIND_SHIFT {
            KIND_INSTANCE => self.instances[i].hit(ray, t_min, t_max),
            KIND_CUBIC => self.cubics[i].hit(ray, t_min, t_max),
            _ => self.prims[i].hit(ray, t_min, t_max),
        }
    }

    /// Out of line for the same reason as [`Bvh::scalar_hit`].
    #[inline(never)]
    fn scalar_hit_any(&self, id: u32, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        let i = (id & INDEX_MASK) as usize;
        match id >> KIND_SHIFT {
            KIND_INSTANCE => self.instances[i].hit_any(ray, t_min, t_max),
            KIND_CUBIC => self.cubics[i].hit_any(ray, t_min, t_max),
            _ => self.prims[i].hit_any(ray, t_min, t_max),
        }
    }

    /// The per-ray Woop shear, derived once per traversal — but only for
    /// scenes that actually hold triangle packets. It costs two divides,
    /// which is real money on a scene of spheres or instances that would
    /// never look at it.
    #[inline]
    fn shear(&self, ray: &Ray) -> Option<RayShear> {
        (!self.packets.is_empty() || !self.packets_i.is_empty()).then(|| RayShear::new(ray))
    }

    /// Total primitive references held by leaves — packed SIMD lanes plus
    /// scalar indices. Larger than `prim_count` exactly when spatial splits
    /// duplicated references.
    #[cfg(test)]
    fn leaf_ref_count(&self) -> usize {
        let packed: u32 = self
            .packets
            .iter()
            .map(|p| p.lanes.active.count_ones())
            .sum::<u32>()
            + self
                .packets_i
                .iter()
                .map(|p| p.lanes.active.count_ones())
                .sum::<u32>();
        packed as usize + self.indices.len()
    }

    pub(crate) fn bounds(&self) -> Option<AABB> {
        self.root_bbox
    }

    /// Closest hit in `(t_min, t_max)`.
    pub(crate) fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        if self.wide.is_empty() {
            return None;
        }
        tstat!(QUERIES, 1);
        let mut closest = t_max;
        let mut best: Option<Candidate> = None;

        // Splat the ray into SoA lanes once for the whole traversal
        // instead of once per visited node, and likewise derive the Woop
        // shear once instead of once per triangle.
        let rs = RaySlab::new(ray, t_min);
        let shear = self.shear(ray);

        let mut stack = TraversalStack::new(0);

        while let Some(node_idx) = stack.pop() {
            tstat!(NODES_VISITED, 1);
            let node = &self.wide[node_idx as usize];
            let (mut mask, tn) = rs.slab(node, closest);
            if mask == 0 {
                continue;
            }

            // Hit lanes, insertion-sorted near-to-far (≤ LANES entries).
            // The distances are read from one spilled copy of the vector
            // rather than re-extracting a lane at a time.
            let mut order = [(0f32, 0usize); LANES];
            let mut n_hit = 0;
            while mask != 0 {
                let l = mask.trailing_zeros() as usize;
                mask &= mask - 1;
                let t = tn[l];
                let mut i = n_hit;
                while i > 0 && order[i - 1].0 > t {
                    order[i] = order[i - 1];
                    i -= 1;
                }
                order[i] = (t, l);
                n_hit += 1;
            }

            // Leaf lanes intersect immediately (near first, shrinking
            // `closest`); internal lanes are pushed far-to-near so the
            // nearest pops first.
            for &(_, l) in &order[..n_hit] {
                if node.is_leaf(l)
                    && let Some(hit) =
                        self.intersect_leaf(node.child[l], ray, shear.as_ref(), t_min, closest)
                {
                    closest = hit.t();
                    best = Some(hit);
                }
            }
            for i in (0..n_hit).rev() {
                let l = order[i].1;
                if !node.is_leaf(l) {
                    stack.push(node.child[l]);
                }
            }
        }

        best.map(|c| self.resolve(c))
    }

    /// Closest hit within one leaf: the 4-wide triangle packets first (four
    /// triangles per vector round), then whatever did not fit a packet.
    #[inline]
    fn intersect_leaf(
        &self,
        leaf_idx: u32,
        ray: &Ray,
        shear: Option<&RayShear>,
        t_min: f32,
        t_max: f32,
    ) -> Option<Candidate> {
        let leaf = &self.leaves[leaf_idx as usize];
        tstat!(LEAVES_VISITED, 1);
        tstat!(PACKET_TESTS, leaf.pkt_count as u64);
        tstat!(PRIM_TESTS, leaf.idx_count as u64);
        let mut closest = t_max;
        let mut best: Option<Candidate> = None;

        let first = leaf.pkt_first as usize;
        let range = first..first + leaf.pkt_count as usize;
        match self.layout {
            Layout::Gathered => {
                for packet in &self.packets[range] {
                    let shear = shear.expect("a leaf with packets implies the scene has triangles");
                    let out = packet.intersect(shear, ray.mask, t_min, closest);
                    self.lanes_hit(
                        &packet.lanes,
                        out,
                        ray,
                        shear,
                        t_min,
                        &mut closest,
                        &mut best,
                    );
                }
            }
            Layout::Indexed => {
                for packet in &self.packets_i[range] {
                    let shear = shear.expect("a leaf with packets implies the scene has triangles");
                    let out = packet.intersect(&self.vertices, shear, ray.mask, t_min, closest);
                    self.lanes_hit(
                        &packet.lanes,
                        out,
                        ray,
                        shear,
                        t_min,
                        &mut closest,
                        &mut best,
                    );
                }
            }
        }

        let first = leaf.idx_first as usize;
        for &pi in &self.indices[first..first + leaf.idx_count as usize] {
            if let Some(hit) = self.scalar_hit(pi, ray, t_min, closest) {
                closest = hit.t;
                best = Some(Candidate::Prim(hit));
            }
        }
        best
    }

    /// Folds one packet's lane verdicts into the leaf's running best: hit
    /// lanes in lane order, then the `f64` tie-break lanes.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn lanes_hit(
        &self,
        lanes: &crate::triangle::LaneMasks,
        out: Hit4,
        ray: &Ray,
        shear: &RayShear,
        t_min: f32,
        closest: &mut f32,
        best: &mut Option<Candidate>,
    ) {
        let mut hits = out.hits;
        while hits != 0 {
            let lane = hits.trailing_zeros() as usize;
            hits &= hits - 1;
            // Lanes were tested against the `closest` on entry, which
            // earlier lanes may since have shrunk. The comparison is
            // strict-greater, not greater-or-equal, so an exact tie
            // resolves to the later primitive exactly as a run of
            // scalar `hit` calls would.
            if out.t[lane] > *closest {
                continue;
            }
            *closest = out.t[lane];
            *best = Some(Candidate::Lane {
                rec: lanes.rec[lane],
                t: out.t[lane],
                u: out.u[lane],
                v: out.v[lane],
            });
        }
        // Lanes sitting exactly on an edge: the f64 tie-break is scalar.
        let mut fb = out.fallback;
        while fb != 0 {
            let lane = fb.trailing_zeros() as usize;
            fb &= fb - 1;
            if let Some(hit) = self.tri_hit(lanes.rec[lane], ray, shear, t_min, *closest) {
                *closest = hit.t;
                *best = Some(Candidate::Prim(hit));
            }
        }
    }

    /// Early-exit occlusion traversal: no ordering, returns on the first
    /// confirmed hit anywhere in `(t_min, t_max)`.
    pub(crate) fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        if self.wide.is_empty() {
            return false;
        }
        let rs = RaySlab::new(ray, t_min);
        let shear = self.shear(ray);

        let mut stack = TraversalStack::new(0);

        while let Some(node_idx) = stack.pop() {
            let node = &self.wide[node_idx as usize];
            let (mut mask, _) = rs.slab(node, t_max);
            while mask != 0 {
                let l = mask.trailing_zeros() as usize;
                mask &= mask - 1;
                if node.is_leaf(l) {
                    if self.occlude_leaf(node.child[l], ray, shear.as_ref(), t_min, t_max) {
                        return true;
                    }
                } else {
                    stack.push(node.child[l]);
                }
            }
        }
        false
    }

    /// Boolean variant of [`Bvh::intersect_leaf`]: any lane hitting anywhere
    /// in range ends the query, so there is no ordering and no need to
    /// resolve which lane won.
    #[inline]
    fn occlude_leaf(
        &self,
        leaf_idx: u32,
        ray: &Ray,
        shear: Option<&RayShear>,
        t_min: f32,
        t_max: f32,
    ) -> bool {
        let leaf = &self.leaves[leaf_idx as usize];

        let first = leaf.pkt_first as usize;
        let range = first..first + leaf.pkt_count as usize;
        // Matching `tri_hit_any`, occlusion needs no normal: any lane in
        // range occludes.
        match self.layout {
            Layout::Gathered => {
                for packet in &self.packets[range] {
                    let shear = shear.expect("a leaf with packets implies the scene has triangles");
                    let out = packet.intersect(shear, ray.mask, t_min, t_max);
                    if self.lanes_occlude(&packet.lanes, out, ray, shear, t_min, t_max) {
                        return true;
                    }
                }
            }
            Layout::Indexed => {
                for packet in &self.packets_i[range] {
                    let shear = shear.expect("a leaf with packets implies the scene has triangles");
                    let out = packet.intersect(&self.vertices, shear, ray.mask, t_min, t_max);
                    if self.lanes_occlude(&packet.lanes, out, ray, shear, t_min, t_max) {
                        return true;
                    }
                }
            }
        }

        let first = leaf.idx_first as usize;
        for &pi in &self.indices[first..first + leaf.idx_count as usize] {
            if self.scalar_hit_any(pi, ray, t_min, t_max) {
                return true;
            }
        }
        false
    }
}

impl Bvh {
    #[inline]
    fn lanes_occlude(
        &self,
        lanes: &crate::triangle::LaneMasks,
        out: Hit4,
        ray: &Ray,
        shear: &RayShear,
        t_min: f32,
        t_max: f32,
    ) -> bool {
        if out.hits != 0 {
            return true;
        }
        let mut fb = out.fallback;
        while fb != 0 {
            let lane = fb.trailing_zeros() as usize;
            fb &= fb - 1;
            if self.tri_hit_any(lanes.rec[lane], ray, shear, t_min, t_max) {
                return true;
            }
        }
        false
    }
}

/// Finite reciprocal of every direction component: zero (and denormal-tiny)
/// components become a huge same-signed value instead of ±∞, so the slab
/// arithmetic can never produce the 0·∞ = NaN that poisons vector min/max.
/// Branch-free and component-wise, so all three lanes go through one
/// divide and one select.
#[inline]
fn safe_inv3(d: Vec3A) -> Vec3A {
    const TINY: f32 = 1e-20;
    const HUGE: f32 = 1e20;
    // `copysign` via a sign-bit blend: `HUGE` with `d`'s sign bit.
    let huge = Vec3A::splat(HUGE).copysign(d);
    Vec3A::select(d.abs().cmplt(Vec3A::splat(TINY)), huge, d.recip())
}

/// Inline capacity of the traversal stack, in node indices.
///
/// Sized from measurement rather than from a worst-case bound. With the
/// `traversal-stats` feature on, `traversal_probe` reports the deepest stack
/// any query reached (see [`stats::stack_high_water`]); it grows
/// logarithmically, as a good tree should:
///
/// | primitives in one tree | deepest stack |
/// | --- | --- |
/// | 1 024 | 6 |
/// | 16 384 | 8 |
/// | 262 144 | 11 |
/// | 1 048 576 | 12 |
///
/// Instanced and nested-instanced layouts stay at 9-10, since each level is
/// a separate tree with its own stack.
///
/// 32 is therefore ~2.7x the deepest figure observed on a million-primitive
/// tree, and the trend says reaching it would take a tree orders of
/// magnitude larger. Do not tighten it to hug the measurement: the point of
/// the headroom is that overflowing costs a heap allocation *per traversal*,
/// which would be worse than the `memset` this design removed. Shrinking to
/// 16 was measured at ~0.3% — not worth moving that cliff closer.
const STACK_INLINE: usize = 32;

/// The node indices a traversal still has to visit.
///
/// The obvious spelling is a fixed `[u32; 3 * MAX_DEPTH + 4]` array — the
/// bound is genuinely worst-case, since a wide node can advance the binary
/// depth by as little as one level and leaves 3 deferred lanes behind. But
/// that array is 736 bytes and Rust zeroes it on entry to *every* traversal,
/// including every instance descent (which re-enters `hit` recursively).
/// Callgrind attributed 180M of cornellbox's 187M `memset` instructions to
/// exactly those two declarations — 4.8% of the render, and 5.4% on
/// veach_mis, all of it clearing memory that is written before it is read.
///
/// So: a small inline array plus a `Vec` that stays unallocated until a tree
/// is deep enough to overflow it. What matters is that the spill makes
/// correctness *unconditional* — [`STACK_INLINE`] becomes a tuning knob, and
/// the old "prove 3·MAX_DEPTH + 4 is always enough" argument goes away
/// rather than being replaced by a smaller and shakier one.
struct TraversalStack {
    inline: [u32; STACK_INLINE],
    /// Entries at logical depth ≥ [`STACK_INLINE`], in order. `Vec::new`
    /// does not allocate, so a query that never gets that deep pays nothing.
    spill: Vec<u32>,
    sp: usize,
}

impl TraversalStack {
    /// A stack holding just `root`.
    #[inline]
    fn new(root: u32) -> Self {
        let mut inline = [0u32; STACK_INLINE];
        inline[0] = root;
        Self {
            inline,
            spill: Vec::new(),
            sp: 1,
        }
    }

    #[inline]
    fn push(&mut self, node: u32) {
        if self.sp < STACK_INLINE {
            self.inline[self.sp] = node;
        } else {
            self.spill.push(node);
        }
        self.sp += 1;
        #[cfg(feature = "traversal-stats")]
        stats::note_stack_depth(self.sp);
    }

    #[inline]
    fn pop(&mut self) -> Option<u32> {
        self.sp = self.sp.checked_sub(1)?;
        if self.sp < STACK_INLINE {
            Some(self.inline[self.sp])
        } else {
            // Mirrors `push`: everything at depth ≥ STACK_INLINE lives in
            // the spill, so this side is never empty when we take it.
            self.spill.pop()
        }
    }
}

#[cfg(test)]
mod lane_width;
#[cfg(test)]
mod tests;
