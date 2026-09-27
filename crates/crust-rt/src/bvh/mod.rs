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
//! rather than read back one at a time. Each leaf's payload lands in the
//! separate `leaves` table — which keeps [`WideNode`] at two cache lines —
//! and its triangles are packed into 4-wide [`Tri4`] packets so a leaf
//! intersects four triangles per vector round; everything else (spheres,
//! curves, instances) keeps a scalar index in `indices`.
//!
//! Large subtrees build in parallel via `rayon::join`; every split decision
//! depends only on the input, so the tree is deterministic — threads only
//! change *when* subtrees are built, never *what*.

use build::{build_subtree, surface_area, union_all};
use collapse::{LeafData, collapse};
use glam::{Vec3A, Vec4};

use crate::aabb::AABB;
use crate::prim::{PrimHit, PrimNode, TrianglePrim};
use crate::ray::Ray;
use crate::scene::PrimitiveBreakdown;
use crate::triangle::{RayShear, Tri4};

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
struct Node {
    bbox: AABB,
    /// Leaf (`count > 0`): offset of the first entry in `indices`.
    /// Internal (`count == 0`): index of the right child — the left child
    /// immediately follows the node itself in depth-first order.
    first_or_right: u32,
    count: u32,
}

/// Marks an unused lane of a [`WideNode`].
const EMPTY_LANE: u32 = u32::MAX;

/// Lane-validity bits of [`WideNode::flags`] (bit `k` = lane `k` holds a
/// real child) and leaf bits (bit `4 + k` = that child is a leaf).
const VALID_MASK: u32 = 0b1111;
const LEAF_SHIFT: u32 = 4;

/// A 4-wide BVH node in SoA layout: lane `k` of each `Vec4` holds child
/// `k`'s slab bounds, so one round of vector min/max tests all four child
/// boxes against the ray at once (Embree's BVH4 idea).
///
/// Exactly 128 bytes (two cache lines): six `Vec4`s, one child index per
/// lane, and the flag nibbles. The leaf payload — how many primitives, and
/// where their SIMD packets live — sits in the separate [`Leaf`] table
/// rather than in the node, which is what keeps the node this small.
struct WideNode {
    bmin_x: Vec4,
    bmin_y: Vec4,
    bmin_z: Vec4,
    bmax_x: Vec4,
    bmax_y: Vec4,
    bmax_z: Vec4,
    /// Leaf lane: index into the BVH's `leaves`. Internal lane: index into
    /// `wide`. Unused lane: [`EMPTY_LANE`].
    child: [u32; 4],
    /// Bits 0..4: lane `k` holds a real child. Bits 4..8: that child is a
    /// leaf.
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
        WideNode {
            bmin_x: Vec4::INFINITY,
            bmin_y: Vec4::INFINITY,
            bmin_z: Vec4::INFINITY,
            bmax_x: Vec4::INFINITY,
            bmax_y: Vec4::INFINITY,
            bmax_z: Vec4::INFINITY,
            child: [EMPTY_LANE; 4],
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

pub(crate) struct Bvh {
    wide: Vec<WideNode>,
    /// Leaf payloads, indexed by a leaf lane's `child`.
    leaves: Vec<Leaf>,
    /// 4-wide triangle packets, grouped per leaf.
    packets: Vec<Tri4>,
    /// The one-at-a-time primitives of each leaf; spatial splits may list a
    /// primitive in more than one leaf.
    indices: Vec<u32>,
    /// The primitives, stored once each, in input order.
    prims: Vec<PrimNode>,
    /// Bounds of the whole tree (the binary root's, kept through collapse).
    root_bbox: Option<AABB>,
}

/// One build reference: conservative bounds of (a fragment of) primitive
/// `idx`. Spatial splits shrink the bounds and duplicate the reference.
#[derive(Clone, Copy)]
struct PrimRef {
    bbox: AABB,
    idx: u32,
}

impl PrimRef {
    fn centroid(&self) -> Vec3A {
        0.5 * (self.bbox.minimum + self.bbox.maximum)
    }
}

/// A built subtree with node indices and leaf offsets local to itself;
/// `merge` splices children under a parent, offsetting as it goes.
struct Subtree {
    nodes: Vec<Node>,
    indices: Vec<u32>,
}

impl Bvh {
    pub(crate) fn new(prims: Vec<PrimNode>) -> Self {
        let refs: Vec<PrimRef> = prims
            .iter()
            .enumerate()
            .map(|(i, p)| PrimRef {
                bbox: p.bbox(),
                idx: i as u32,
            })
            .collect();

        let (wide, collected, root_bbox) = if refs.is_empty() {
            (Vec::new(), LeafData::default(), None)
        } else {
            let root_bbox = union_all(&refs);
            let subtree = build_subtree(&prims, refs, 0, surface_area(&root_bbox));
            let (wide, collected) = collapse(&subtree.nodes, &subtree.indices, &prims);
            (wide, collected, Some(root_bbox))
        };

        Bvh {
            wide,
            leaves: collected.leaves,
            packets: collected.packets,
            indices: collected.indices,
            prims,
            root_bbox,
        }
    }

    pub(crate) fn prim_count(&self) -> usize {
        self.prims.len()
    }

    /// `(count, sum of bbox diagonals, max diagonal)` over top-level
    /// primitives — feeds [`crate::Scene::primitive_extents`].
    #[cfg(feature = "traversal-stats")]
    pub(crate) fn prims(&self) -> &[PrimNode] {
        &self.prims
    }

    pub(crate) fn primitive_extent_sum(&self) -> (usize, f32, f32) {
        let mut sum = 0.0f32;
        let mut max = 0.0f32;
        for p in &self.prims {
            let b = p.bbox();
            let d = (b.maximum - b.minimum).length();
            sum += d;
            max = max.max(d);
        }
        (self.prims.len(), sum, max)
    }

    pub(crate) fn primitive_breakdown(&self) -> PrimitiveBreakdown {
        let mut b = PrimitiveBreakdown::default();
        for p in &self.prims {
            match p {
                PrimNode::Triangle(_) => b.triangles += 1,
                PrimNode::Sphere(_) => b.spheres += 1,
                PrimNode::Disk(_) => b.disks += 1,
                PrimNode::Cylinder(_) => b.cylinders += 1,
                PrimNode::Curve(_) => b.curve_segments += 1,
                PrimNode::CubicCurve(_) => b.cubic_curve_spans += 1,
                PrimNode::Instance(_) => b.instances += 1,
            }
        }
        b
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
        use std::mem::size_of;
        acc.prim_nodes += self.prims.capacity() * size_of::<PrimNode>();
        acc.bvh_nodes += self.wide.capacity() * size_of::<WideNode>();
        acc.leaves += self.leaves.capacity() * size_of::<Leaf>();
        acc.packets += self.packets.capacity() * size_of::<Tri4>();
        acc.indices += self.indices.capacity() * size_of::<u32>();
        for p in &self.prims {
            match p {
                PrimNode::Instance(i) => {
                    acc.boxed_prims += size_of::<crate::prim::InstancePrim>();
                    if visited.insert(std::sync::Arc::as_ptr(&i.scene) as usize) {
                        i.scene.accumulate_footprint_into(visited, acc);
                    }
                }
                PrimNode::CubicCurve(_) => {
                    acc.boxed_prims += size_of::<crate::prim::CubicCurvePrim>();
                }
                _ => {}
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
        for p in &self.prims {
            match p {
                PrimNode::Triangle(_) => acc.triangles += 1,
                PrimNode::Sphere(_) => acc.spheres += 1,
                PrimNode::Disk(_) => acc.disks += 1,
                PrimNode::Cylinder(_) => acc.cylinders += 1,
                PrimNode::Curve(_) => acc.curve_segments += 1,
                PrimNode::CubicCurve(_) => acc.cubic_curve_spans += 1,
                PrimNode::Instance(i) => {
                    acc.instances += 1;
                    if visited.insert(std::sync::Arc::as_ptr(&i.scene) as usize) {
                        i.scene.accumulate_unique_into(visited, acc);
                    }
                }
            }
        }
    }

    /// The per-ray Woop shear, derived once per traversal — but only for
    /// scenes that actually hold triangle packets. It costs two divides,
    /// which is real money on a scene of spheres or instances that would
    /// never look at it.
    #[inline]
    fn shear(&self, ray: &Ray) -> Option<RayShear> {
        (!self.packets.is_empty()).then(|| RayShear::new(ray))
    }

    /// Total primitive references held by leaves — packed SIMD lanes plus
    /// scalar indices. Larger than `prim_count` exactly when spatial splits
    /// duplicated references.
    #[cfg(test)]
    fn leaf_ref_count(&self) -> usize {
        let packed: u32 = self.packets.iter().map(|p| p.active.count_ones()).sum();
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
        let mut best: Option<PrimHit> = None;

        // Splat the ray into SoA lanes once for the whole traversal
        // instead of once per visited node, and likewise derive the Woop
        // shear once instead of once per triangle.
        let rs = RaySlab::new(ray, t_min);
        let shear = self.shear(ray);

        let mut stack = TraversalStack::new(0);

        while let Some(node_idx) = stack.pop() {
            tstat!(NODES_VISITED, 1);
            let node = &self.wide[node_idx as usize];
            let (tnear, tfar) = rs.slab4(node, closest);

            // One vector compare + one movmskps gives all four lane
            // verdicts as a nibble; the validity bits drop unused lanes.
            let mut mask = tnear.cmple(tfar).bitmask() & node.flags & VALID_MASK;
            if mask == 0 {
                continue;
            }

            // Hit lanes, insertion-sorted near-to-far (≤ 4 entries). The
            // distances are read from one spilled copy of the vector
            // rather than re-extracting a lane at a time.
            let tn = tnear.to_array();
            let mut order = [(0f32, 0usize); 4];
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
                    closest = hit.t;
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

        best
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
    ) -> Option<PrimHit> {
        let leaf = &self.leaves[leaf_idx as usize];
        tstat!(LEAVES_VISITED, 1);
        tstat!(PACKET_TESTS, leaf.pkt_count as u64);
        tstat!(PRIM_TESTS, leaf.idx_count as u64);
        let mut closest = t_max;
        let mut best: Option<PrimHit> = None;

        let first = leaf.pkt_first as usize;
        for packet in &self.packets[first..first + leaf.pkt_count as usize] {
            let shear = shear.expect("a leaf with packets implies the scene has triangles");
            let out = packet.intersect(shear, ray.mask, t_min, closest);
            let mut hits = out.hits;
            while hits != 0 {
                let lane = hits.trailing_zeros() as usize;
                hits &= hits - 1;
                // Lanes were tested against the `closest` on entry, which
                // earlier lanes may since have shrunk. The comparison is
                // strict-greater, not greater-or-equal, so an exact tie
                // resolves to the later primitive exactly as a run of
                // scalar `hit` calls would.
                if out.t[lane] > closest {
                    continue;
                }
                let tri = self.triangle(packet.prim[lane]);
                if let Some(hit) = tri.hit_from_barycentric(out.t[lane], out.u[lane], out.v[lane]) {
                    closest = hit.t;
                    best = Some(hit);
                }
            }
            // Lanes sitting exactly on an edge: the f64 tie-break is scalar.
            let mut fb = out.fallback;
            while fb != 0 {
                let lane = fb.trailing_zeros() as usize;
                fb &= fb - 1;
                let pi = packet.prim[lane] as usize;
                if let Some(hit) = self.prims[pi].hit(ray, t_min, closest) {
                    closest = hit.t;
                    best = Some(hit);
                }
            }
        }

        let first = leaf.idx_first as usize;
        for &pi in &self.indices[first..first + leaf.idx_count as usize] {
            if let Some(hit) = self.prims[pi as usize].hit(ray, t_min, closest) {
                closest = hit.t;
                best = Some(hit);
            }
        }
        best
    }

    /// The triangle a packet lane came from. Packets are only built from
    /// primitives that answered `as_triangle`, so this always resolves.
    #[inline]
    fn triangle(&self, prim_idx: u32) -> &TrianglePrim {
        self.prims[prim_idx as usize]
            .as_triangle()
            .expect("packet lanes are built from triangles only")
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
            let (tnear, tfar) = rs.slab4(node, t_max);
            let mut mask = tnear.cmple(tfar).bitmask() & node.flags & VALID_MASK;
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
        for packet in &self.packets[first..first + leaf.pkt_count as usize] {
            // Matching `TrianglePrim::hit_any`, occlusion needs no normal:
            // any lane in range occludes.
            let shear = shear.expect("a leaf with packets implies the scene has triangles");
            let out = packet.intersect(shear, ray.mask, t_min, t_max);
            if out.hits != 0 {
                return true;
            }
            let mut fb = out.fallback;
            while fb != 0 {
                let lane = fb.trailing_zeros() as usize;
                fb &= fb - 1;
                if self.prims[packet.prim[lane] as usize].hit_any(ray, t_min, t_max) {
                    return true;
                }
            }
        }

        let first = leaf.idx_first as usize;
        for &pi in &self.indices[first..first + leaf.idx_count as usize] {
            if self.prims[pi as usize].hit_any(ray, t_min, t_max) {
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

/// The ray, pre-broadcast into the SoA layout the 4-wide slab test wants.
/// Built once per traversal: the six splats and the reciprocal used to be
/// recomputed for every visited node, which is pure overhead in a loop
/// that visits tens of nodes per ray.
struct RaySlab {
    ox: Vec4,
    oy: Vec4,
    oz: Vec4,
    ix: Vec4,
    iy: Vec4,
    iz: Vec4,
    t_min: Vec4,
}

impl RaySlab {
    #[inline]
    fn new(ray: &Ray, t_min: f32) -> Self {
        let o = ray.origin;
        let inv = safe_inv3(ray.dir);
        RaySlab {
            ox: Vec4::splat(o.x),
            oy: Vec4::splat(o.y),
            oz: Vec4::splat(o.z),
            ix: Vec4::splat(inv.x),
            iy: Vec4::splat(inv.y),
            iz: Vec4::splat(inv.z),
            t_min: Vec4::splat(t_min),
        }
    }

    /// The 4-lane slab test: entry/exit distances for all four child boxes
    /// of `node` at once. A lane hits iff `tnear[l] <= tfar[l]`.
    #[inline]
    fn slab4(&self, node: &WideNode, t_max: f32) -> (Vec4, Vec4) {
        let t0x = (node.bmin_x - self.ox) * self.ix;
        let t1x = (node.bmax_x - self.ox) * self.ix;
        let t0y = (node.bmin_y - self.oy) * self.iy;
        let t1y = (node.bmax_y - self.oy) * self.iy;
        let t0z = (node.bmin_z - self.oz) * self.iz;
        let t1z = (node.bmax_z - self.oz) * self.iz;
        let tnear = t0x
            .min(t1x)
            .max(t0y.min(t1y))
            .max(t0z.min(t1z))
            .max(self.t_min);
        let tfar = t0x
            .max(t1x)
            .min(t0y.max(t1y))
            .min(t0z.max(t1z))
            .min(Vec4::splat(t_max));
        (tnear, tfar)
    }
}

#[cfg(test)]
mod lane_width;
#[cfg(test)]
mod tests;
