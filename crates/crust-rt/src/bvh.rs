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

use crate::aabb::AABB;
use crate::prim::{Prim, PrimHit, TrianglePrim};
use crate::ray::Ray;
use crate::triangle::{RayShear, Tri4};
use glam::{Vec3A, Vec4};

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
    prims: Vec<Box<dyn Prim>>,
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
    pub(crate) fn new(prims: Vec<Box<dyn Prim>>) -> Self {
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
        let mut closest = t_max;
        let mut best: Option<PrimHit> = None;

        // Splat the ray into SoA lanes once for the whole traversal
        // instead of once per visited node, and likewise derive the Woop
        // shear once instead of once per triangle.
        let rs = RaySlab::new(ray, t_min);
        let shear = self.shear(ray);

        let mut stack = [0u32; WIDE_STACK];
        stack[0] = 0;
        let mut sp = 1usize;

        while sp > 0 {
            sp -= 1;
            let node = &self.wide[stack[sp] as usize];
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
                    stack[sp] = node.child[l];
                    sp += 1;
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

        let mut stack = [0u32; WIDE_STACK];
        stack[0] = 0;
        let mut sp = 1usize;

        while sp > 0 {
            sp -= 1;
            let node = &self.wide[stack[sp] as usize];
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
                    stack[sp] = node.child[l];
                    sp += 1;
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

/// Traversal stack: wide depth ≤ binary `MAX_DEPTH`, and each visited node
/// leaves at most 3 deferred lanes behind, so 3·MAX_DEPTH + 4 slots hold
/// the worst case.
const WIDE_STACK: usize = 3 * MAX_DEPTH + 4;

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

fn surface_area(b: &AABB) -> f32 {
    let d = b.maximum - b.minimum;
    2.0 * (d.x * d.y + d.y * d.z + d.z * d.x)
}

fn union_all(refs: &[PrimRef]) -> AABB {
    refs.iter()
        .skip(1)
        .fold(refs[0].bbox, |acc, r| AABB::surrounding_box(acc, r.bbox))
}

/// Component-wise intersection; `None` when the boxes do not overlap.
fn intersect_aabb(a: &AABB, b: &AABB) -> Option<AABB> {
    let lo = a.minimum.max(b.minimum);
    let hi = a.maximum.min(b.maximum);
    if lo.cmple(hi).all() {
        Some(AABB::new(lo, hi))
    } else {
        None
    }
}

fn leaf(bbox: AABB, refs: &[PrimRef]) -> Subtree {
    Subtree {
        nodes: vec![Node {
            bbox,
            first_or_right: 0,
            count: refs.len() as u32,
        }],
        indices: refs.iter().map(|r| r.idx).collect(),
    }
}

/// Splices `left`/`right` under a fresh internal node, rewriting the
/// children's local node indices and leaf offsets into the merged frame.
fn merge(bbox: AABB, left: Subtree, right: Subtree) -> Subtree {
    let mut nodes = Vec::with_capacity(1 + left.nodes.len() + right.nodes.len());
    let right_node_offset = 1 + left.nodes.len() as u32;
    nodes.push(Node {
        bbox,
        first_or_right: right_node_offset,
        count: 0,
    });
    for mut n in left.nodes {
        if n.count == 0 {
            n.first_or_right += 1;
        }
        nodes.push(n);
    }
    let leaf_offset = left.indices.len() as u32;
    for mut n in right.nodes {
        if n.count == 0 {
            n.first_or_right += right_node_offset;
        } else {
            n.first_or_right += leaf_offset;
        }
        nodes.push(n);
    }
    let mut indices = left.indices;
    indices.extend(right.indices);
    Subtree { nodes, indices }
}

/// The winning object split: partition predicate parameters plus the SAH
/// cost and the (unclipped) child bounds the overlap test needs.
struct ObjSplit {
    axis: usize,
    /// Centroid-to-bin mapping: `bin = ((c - cmin) * scale) as usize`.
    cmin: f32,
    scale: f32,
    split_bin: usize,
    cost: f32,
    left_bbox: AABB,
    right_bbox: AABB,
}

/// The winning spatial split: chop plane on `axis` at `pos`.
struct SpatSplit {
    axis: usize,
    pos: f32,
    cost: f32,
}

fn best_object_split(refs: &[PrimRef]) -> Option<ObjSplit> {
    // Split along the longest axis of the centroid bounds.
    let mut cmin = refs[0].centroid();
    let mut cmax = cmin;
    for r in &refs[1..] {
        let c = r.centroid();
        cmin = cmin.min(c);
        cmax = cmax.max(c);
    }
    let extent = cmax - cmin;
    let axis = if extent.x >= extent.y && extent.x >= extent.z {
        0
    } else if extent.y >= extent.z {
        1
    } else {
        2
    };
    if extent[axis] <= 1e-6 {
        // All centroids coincide — nothing to partition on.
        return None;
    }
    let scale = BINS as f32 / extent[axis];
    let bin_of = |r: &PrimRef| (((r.centroid()[axis] - cmin[axis]) * scale) as usize).min(BINS - 1);

    // Binned SAH: histogram the centroids, then score every split plane
    // between adjacent bins by `area · count` on each side.
    let mut bin_counts = [0usize; BINS];
    let mut bin_bounds: [Option<AABB>; BINS] = [None; BINS];
    for r in refs {
        let b = bin_of(r);
        bin_counts[b] += 1;
        bin_bounds[b] = Some(match bin_bounds[b] {
            Some(existing) => AABB::surrounding_box(existing, r.bbox),
            None => r.bbox,
        });
    }

    let mut best: Option<ObjSplit> = None;
    for split in 0..BINS - 1 {
        let mut lb: Option<AABB> = None;
        let mut lc = 0usize;
        for b in 0..=split {
            lc += bin_counts[b];
            lb = match (lb, bin_bounds[b]) {
                (Some(x), Some(y)) => Some(AABB::surrounding_box(x, y)),
                (x, y) => x.or(y),
            };
        }
        let mut rb: Option<AABB> = None;
        let mut rc = 0usize;
        for b in split + 1..BINS {
            rc += bin_counts[b];
            rb = match (rb, bin_bounds[b]) {
                (Some(x), Some(y)) => Some(AABB::surrounding_box(x, y)),
                (x, y) => x.or(y),
            };
        }
        if lc == 0 || rc == 0 {
            continue;
        }
        let (lb, rb) = (lb.expect("lc > 0"), rb.expect("rc > 0"));
        let cost = surface_area(&lb) * lc as f32 + surface_area(&rb) * rc as f32;
        if best.as_ref().is_none_or(|b| cost < b.cost) {
            best = Some(ObjSplit {
                axis,
                cmin: cmin[axis],
                scale,
                split_bin: split,
                cost,
                left_bbox: lb,
                right_bbox: rb,
            });
        }
    }
    best
}

/// Binned spatial split on the node's longest axis: references straddling
/// a bin contribute their *clipped* bounds to it (entry/exit counting), so
/// the candidate children reflect what duplication would actually produce.
fn best_spatial_split(prims: &[Box<dyn Prim>], refs: &[PrimRef], bbox: &AABB) -> Option<SpatSplit> {
    let extent = bbox.maximum - bbox.minimum;
    let axis = if extent.x >= extent.y && extent.x >= extent.z {
        0
    } else if extent.y >= extent.z {
        1
    } else {
        2
    };
    if extent[axis] <= 1e-6 {
        return None;
    }
    let lo = bbox.minimum[axis];
    let width = extent[axis] / BINS as f32;
    let bin_of = |x: f32| (((x - lo) / width) as usize).clamp(0, BINS - 1);

    let mut entry = [0usize; BINS];
    let mut exit = [0usize; BINS];
    let mut bounds: [Option<AABB>; BINS] = [None; BINS];
    let mut add = |b: usize, aabb: AABB| {
        bounds[b] = Some(match bounds[b] {
            Some(existing) => AABB::surrounding_box(existing, aabb),
            None => aabb,
        });
    };

    for r in refs {
        let b0 = bin_of(r.bbox.minimum[axis]);
        let b1 = bin_of(r.bbox.maximum[axis]);
        entry[b0] += 1;
        exit[b1] += 1;
        if b0 == b1 {
            add(b0, r.bbox);
            continue;
        }
        for b in b0..=b1 {
            let (bin_lo, bin_hi) = (lo + b as f32 * width, lo + (b + 1) as f32 * width);
            if let Some(c) = prims[r.idx as usize]
                .clipped_aabb(axis, bin_lo, bin_hi)
                .and_then(|c| intersect_aabb(&c, &r.bbox))
            {
                add(b, c);
            }
        }
    }

    let mut best: Option<SpatSplit> = None;
    for split in 0..BINS - 1 {
        let mut lb: Option<AABB> = None;
        let mut lc = 0usize;
        for b in 0..=split {
            lc += entry[b];
            lb = match (lb, bounds[b]) {
                (Some(x), Some(y)) => Some(AABB::surrounding_box(x, y)),
                (x, y) => x.or(y),
            };
        }
        let mut rb: Option<AABB> = None;
        let mut rc = 0usize;
        for b in split + 1..BINS {
            rc += exit[b];
            rb = match (rb, bounds[b]) {
                (Some(x), Some(y)) => Some(AABB::surrounding_box(x, y)),
                (x, y) => x.or(y),
            };
        }
        if lc == 0 || rc == 0 {
            continue;
        }
        let cost = surface_area(&lb.expect("lc > 0")) * lc as f32
            + surface_area(&rb.expect("rc > 0")) * rc as f32;
        if best.as_ref().is_none_or(|b| cost < b.cost) {
            best = Some(SpatSplit {
                axis,
                pos: lo + (split + 1) as f32 * width,
                cost,
            });
        }
    }
    best
}

/// Builds the subtree for `refs`, appending nodes in depth-first order
/// (locally indexed — `merge` rebases children). `root_area` normalizes
/// the SBVH overlap test.
fn build_subtree(
    prims: &[Box<dyn Prim>],
    mut refs: Vec<PrimRef>,
    depth: usize,
    root_area: f32,
) -> Subtree {
    let bbox = union_all(&refs);
    let count = refs.len();
    let min_leaf = min_leaf_for(prims, &refs);
    if count <= min_leaf || depth >= MAX_DEPTH {
        return leaf(bbox, &refs);
    }

    let object = best_object_split(&refs);

    // Consider a spatial split only when the object split's children
    // overlap enough for chopping to pay for the duplicated references.
    let spatial = match &object {
        Some(o) if depth < SBVH_MAX_DEPTH => {
            let overlap =
                intersect_aabb(&o.left_bbox, &o.right_bbox).map_or(0.0, |b| surface_area(&b));
            if overlap / root_area > SBVH_ALPHA {
                best_spatial_split(prims, &refs, &bbox).filter(|s| s.cost < o.cost)
            } else {
                None
            }
        }
        _ => None,
    };

    let (left_refs, right_refs) = if let Some(s) = spatial {
        // Chop: straddling references are clipped into both children.
        let mut left = Vec::with_capacity(count);
        let mut right = Vec::with_capacity(count);
        for r in refs {
            if r.bbox.maximum[s.axis] <= s.pos {
                left.push(r);
            } else if r.bbox.minimum[s.axis] >= s.pos {
                right.push(r);
            } else {
                let prim = &prims[r.idx as usize];
                if let Some(c) = prim
                    .clipped_aabb(s.axis, f32::NEG_INFINITY, s.pos)
                    .and_then(|c| intersect_aabb(&c, &r.bbox))
                {
                    left.push(PrimRef { bbox: c, idx: r.idx });
                }
                if let Some(c) = prim
                    .clipped_aabb(s.axis, s.pos, f32::INFINITY)
                    .and_then(|c| intersect_aabb(&c, &r.bbox))
                {
                    right.push(PrimRef { bbox: c, idx: r.idx });
                }
            }
        }
        // Degenerate chop (numeric edge): fall back to a leaf-or-object
        // path rather than recursing on an empty side.
        if left.is_empty() || right.is_empty() {
            let mut refs: Vec<PrimRef> = left;
            refs.extend(right);
            return object_partition_or_leaf(prims, refs, bbox, object, depth, root_area);
        }
        (left, right)
    } else {
        match object {
            Some(o) => {
                // Leaf when splitting costs more than intersecting through.
                if count <= MAX_LEAF && o.cost >= surface_area(&bbox) * count as f32 {
                    return leaf(bbox, &refs);
                }
                partition_by_bin(&mut refs, &o)
            }
            // Every centroid coincides: median split by input order.
            None => {
                let mid = count / 2;
                let right = refs.split_off(mid);
                (refs, right)
            }
        }
    };

    let parallel = left_refs.len().max(right_refs.len()) > PARALLEL_THRESHOLD;
    let (l, r) = if parallel {
        rayon::join(
            || build_subtree(prims, left_refs, depth + 1, root_area),
            || build_subtree(prims, right_refs, depth + 1, root_area),
        )
    } else {
        (
            build_subtree(prims, left_refs, depth + 1, root_area),
            build_subtree(prims, right_refs, depth + 1, root_area),
        )
    };
    merge(bbox, l, r)
}

/// The non-spatial tail of `build_subtree`, reused by the degenerate-chop
/// fallback: object-partition when possible, else leaf.
fn object_partition_or_leaf(
    prims: &[Box<dyn Prim>],
    mut refs: Vec<PrimRef>,
    bbox: AABB,
    object: Option<ObjSplit>,
    depth: usize,
    root_area: f32,
) -> Subtree {
    let count = refs.len();
    match object {
        Some(o) if count > min_leaf_for(prims, &refs) => {
            let (l, r) = partition_by_bin(&mut refs, &o);
            if l.is_empty() || r.is_empty() {
                let mut all = l;
                all.extend(r);
                return leaf(bbox, &all);
            }
            let left = build_subtree(prims, l, depth + 1, root_area);
            let right = build_subtree(prims, r, depth + 1, root_area);
            merge(bbox, left, right)
        }
        _ => leaf(bbox, &refs),
    }
}

/// The leaf-size floor for this range: [`MIN_LEAF_PACKED`] when every
/// reference is a triangle (so the leaf becomes exactly one SIMD packet),
/// [`MIN_LEAF`] otherwise.
fn min_leaf_for(prims: &[Box<dyn Prim>], refs: &[PrimRef]) -> usize {
    if refs
        .iter()
        .all(|r| prims[r.idx as usize].as_triangle().is_some())
    {
        MIN_LEAF_PACKED
    } else {
        MIN_LEAF
    }
}

/// Order-preserving partition of `refs` by the object split's centroid
/// bin — deterministic for a given input order.
fn partition_by_bin(refs: &mut Vec<PrimRef>, o: &ObjSplit) -> (Vec<PrimRef>, Vec<PrimRef>) {
    let mut left = Vec::with_capacity(refs.len());
    let mut right = Vec::with_capacity(refs.len());
    for r in refs.drain(..) {
        let bin = (((r.centroid()[o.axis] - o.cmin) * o.scale) as usize).min(BINS - 1);
        if bin <= o.split_bin {
            left.push(r);
        } else {
            right.push(r);
        }
    }
    (left, right)
}

/// Everything the collapse pass emits besides the nodes themselves: one
/// [`Leaf`] per leaf lane, the triangle packets those leaves run, and the
/// re-ordered one-at-a-time primitive indices.
#[derive(Default)]
struct LeafData {
    leaves: Vec<Leaf>,
    packets: Vec<Tri4>,
    indices: Vec<u32>,
}

impl LeafData {
    /// Turns one binary leaf's primitive range into a [`Leaf`]: triangles
    /// are packed four to a SIMD packet, everything else keeps its scalar
    /// index. Returns the new leaf's index.
    ///
    /// Packets are emitted contiguously per leaf, so a leaf's packets are
    /// one linear sweep of memory at traversal time.
    fn push_leaf(&mut self, range: &[u32], prims: &[Box<dyn Prim>]) -> u32 {
        let pkt_first = self.packets.len() as u32;
        let mut batch: Vec<(Vec3A, Vec3A, Vec3A, u32, u32)> = Vec::with_capacity(4);
        let idx_first = self.indices.len() as u32;
        let mut idx_count = 0u32;

        for &pi in range {
            match prims[pi as usize].as_triangle() {
                Some(t) => {
                    batch.push((t.v0, t.v1, t.v2, pi, t.mask));
                    if batch.len() == 4 {
                        self.packets.push(Tri4::new(&batch));
                        batch.clear();
                    }
                }
                None => {
                    self.indices.push(pi);
                    idx_count += 1;
                }
            }
        }
        if !batch.is_empty() {
            // A partial tail packet still beats scalar calls: the unused
            // lanes ride along for free.
            self.packets.push(Tri4::new(&batch));
        }

        self.leaves.push(Leaf {
            pkt_first,
            pkt_count: self.packets.len() as u32 - pkt_first,
            idx_first,
            idx_count,
        });
        self.leaves.len() as u32 - 1
    }
}

/// Collapses the binary tree into 4-wide nodes: each wide node adopts its
/// binary node's two children, then repeatedly replaces the largest-area
/// internal child with that child's own two children until four lanes are
/// filled (or only leaves remain). Leaf lanes are converted to [`Leaf`]
/// entries with their triangles packed into SIMD packets as they are
/// reached. Purely input-driven, so determinism is preserved.
fn collapse(
    binary: &[Node],
    indices: &[u32],
    prims: &[Box<dyn Prim>],
) -> (Vec<WideNode>, LeafData) {
    let mut out = Vec::with_capacity(binary.len() / 2 + 1);
    let mut data = LeafData::default();
    if binary.is_empty() {
        return (out, data);
    }
    if binary[0].count > 0 {
        // Single-leaf tree.
        let mut w = WideNode::empty();
        w.set_lane_bounds(0, &binary[0].bbox);
        w.child[0] = data.push_leaf(leaf_range(&binary[0], indices), prims);
        w.mark_leaf(0);
        out.push(w);
        return (out, data);
    }
    collapse_node(binary, indices, prims, 0, &mut out, &mut data);
    (out, data)
}

/// The slice of `indices` a binary leaf owns.
fn leaf_range<'a>(node: &Node, indices: &'a [u32]) -> &'a [u32] {
    let first = node.first_or_right as usize;
    &indices[first..first + node.count as usize]
}

fn collapse_node(
    binary: &[Node],
    indices: &[u32],
    prims: &[Box<dyn Prim>],
    b_idx: u32,
    out: &mut Vec<WideNode>,
    data: &mut LeafData,
) -> u32 {
    let slot = out.len();
    out.push(WideNode::empty());

    let mut kids = [0u32; 4];
    kids[0] = b_idx + 1;
    kids[1] = binary[b_idx as usize].first_or_right;
    let mut n_kids = 2;
    while n_kids < 4 {
        // Expand the internal kid with the largest surface area; ties
        // resolve to the first (deterministic).
        let mut best: Option<(usize, f32)> = None;
        for (i, &k) in kids.iter().enumerate().take(n_kids) {
            if binary[k as usize].count == 0 {
                let a = surface_area(&binary[k as usize].bbox);
                if best.is_none_or(|(_, ba)| a > ba) {
                    best = Some((i, a));
                }
            }
        }
        let Some((i, _)) = best else { break };
        let k = kids[i];
        kids[i] = k + 1;
        kids[n_kids] = binary[k as usize].first_or_right;
        n_kids += 1;
    }

    for lane in 0..n_kids {
        let k = kids[lane] as usize;
        let bounds = binary[k].bbox;
        out[slot].set_lane_bounds(lane, &bounds);
        if binary[k].count > 0 {
            out[slot].child[lane] = data.push_leaf(leaf_range(&binary[k], indices), prims);
            out[slot].mark_leaf(lane);
        } else {
            let ci = collapse_node(binary, indices, prims, kids[lane], out, data);
            out[slot].child[lane] = ci;
        }
    }
    slot as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prim::{SpherePrim, TrianglePrim};
    use crate::ray::MASK_ALL;

    fn sphere_grid(n: i32) -> Vec<Box<dyn Prim>> {
        let mut out: Vec<Box<dyn Prim>> = Vec::new();
        for x in 0..n {
            for y in 0..n {
                for z in 0..n {
                    out.push(Box::new(SpherePrim {
                        center: Vec3A::new(x as f32, y as f32, z as f32) * 3.0,
                        radius: 0.5,
                        geom_id: (x * n * n + y * n + z) as u32,
                        mask: MASK_ALL,
                    }));
                }
            }
        }
        out
    }

    /// Long thin diagonal triangles — the geometry spatial splits exist
    /// for. Built so object splits alone leave heavily overlapping
    /// children.
    fn diagonal_shards(n: i32) -> Vec<Box<dyn Prim>> {
        let mut out: Vec<Box<dyn Prim>> = Vec::new();
        for i in 0..n {
            let o = i as f32 * 0.35;
            out.push(Box::new(TrianglePrim {
                v0: Vec3A::new(o, o, o),
                v1: Vec3A::new(o + 10.0, o + 10.0, o + 10.2),
                v2: Vec3A::new(o + 10.0, o + 10.3, o + 10.0),
                normals: None,
                geom_id: 0,
                prim_id: i as u32,
                mask: MASK_ALL,
            }));
        }
        out
    }

    fn linear_scan(prims: &[Box<dyn Prim>], ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        let mut closest = t_max;
        let mut best = None;
        for p in prims {
            if let Some(h) = p.hit(ray, t_min, closest) {
                closest = h.t;
                best = Some(h);
            }
        }
        best
    }

    fn assert_matches_linear(objects: impl Fn() -> Vec<Box<dyn Prim>>) {
        let bvh = Bvh::new(objects());
        let reference = objects();

        let origins = [
            Vec3A::new(-5.0, 4.5, 4.5),
            Vec3A::new(20.0, 3.0, 3.0),
            Vec3A::new(4.5, -5.0, 4.5),
            Vec3A::new(0.0, 0.0, -10.0),
            Vec3A::new(5.0, 5.2, -3.0),
        ];
        let dirs = [
            Vec3A::new(1.0, 0.0, 0.0),
            Vec3A::new(-1.0, 0.05, 0.02).normalize(),
            Vec3A::new(0.0, 1.0, 0.0),
            Vec3A::new(0.3, 0.3, 1.0).normalize(),
            Vec3A::new(0.0, 0.0, -1.0),
            Vec3A::new(0.577, 0.577, 0.577),
        ];
        for o in origins {
            for d in dirs {
                let ray = Ray::new(o, d);
                let a = bvh.hit(&ray, 0.001, f32::INFINITY);
                let b = linear_scan(&reference, &ray, 0.001, f32::INFINITY);
                match (a, b) {
                    (Some(x), Some(y)) => {
                        assert!((x.t - y.t).abs() < 1e-4, "t mismatch for {o:?} {d:?}");
                        assert_eq!(x.geom_id, y.geom_id, "id mismatch for {o:?} {d:?}");
                    }
                    (None, None) => {}
                    (x, y) => panic!(
                        "hit disagreement for {o:?} {d:?}: bvh={} linear={}",
                        x.is_some(),
                        y.is_some()
                    ),
                }
                assert_eq!(
                    bvh.hit_any(&ray, 0.001, f32::INFINITY),
                    b.is_some(),
                    "occlusion disagreement for {o:?} {d:?}"
                );
            }
        }
    }

    /// The BVH must find exactly the hits a linear scan finds.
    #[test]
    fn matches_linear_scan() {
        assert_matches_linear(|| sphere_grid(4));
    }

    /// Same, on geometry that triggers spatial splits (verified below).
    #[test]
    fn spatial_splits_match_linear_scan() {
        assert_matches_linear(|| diagonal_shards(64));
    }

    /// Diagonal shards must actually produce duplicated references —
    /// otherwise the spatial-split path is dead code. References live in
    /// two places now: packed SIMD lanes and the scalar `indices` list.
    #[test]
    fn spatial_splits_duplicate_references() {
        let bvh = Bvh::new(diagonal_shards(64));
        let refs = bvh.leaf_ref_count();
        assert!(
            refs > bvh.prims.len(),
            "no reference duplication: {refs} leaf references for {} prims",
            bvh.prims.len()
        );
    }

    /// Every leaf reference must land in exactly one place, and every
    /// triangle must be packed rather than left on the scalar path.
    #[test]
    fn triangles_are_packed_into_simd_lanes() {
        let bvh = Bvh::new(diagonal_shards(64));
        assert!(!bvh.packets.is_empty(), "no packets built for a triangle scene");
        assert!(
            bvh.indices.is_empty(),
            "{} triangles fell back to the scalar list",
            bvh.indices.len()
        );

        // Spheres are not packable and must stay on the scalar path.
        let bvh = Bvh::new(sphere_grid(4));
        assert!(bvh.packets.is_empty(), "spheres must not be packed");
        assert_eq!(bvh.indices.len(), bvh.leaf_ref_count());

        // Mixed leaves must place each primitive on exactly one path.
        let mut mixed = diagonal_shards(16);
        mixed.extend(sphere_grid(2));
        let bvh = Bvh::new(mixed);
        assert!(!bvh.packets.is_empty() && !bvh.indices.is_empty());
        assert!(bvh.leaf_ref_count() >= bvh.prims.len());
    }

    /// Packet lanes must average close to 4 on a dense mesh — a packing
    /// that mostly emitted 1-lane packets would be SIMD in name only.
    #[test]
    fn packets_are_well_filled() {
        let bvh = Bvh::new(diagonal_shards(256));
        let lanes: u32 = bvh.packets.iter().map(|p| p.active.count_ones()).sum();
        let avg = lanes as f32 / bvh.packets.len() as f32;
        // `MIN_LEAF_PACKED` is what keeps this high — ~2.9 of 4 on this
        // scene, against ~1.7 when the leaf floor was 2. Below 2.5 means
        // the floor has drifted away from the SIMD width again.
        assert!(avg >= 2.5, "average packet occupancy {avg} of 4 lanes");
    }

    /// `hit_any` must agree with `hit(..).is_some()` for every ray and range.
    #[test]
    fn hit_any_matches_hit() {
        let bvh = Bvh::new(sphere_grid(4));
        let origins = [
            Vec3A::new(-5.0, 4.5, 4.5),
            Vec3A::new(20.0, 3.0, 3.0),
            Vec3A::new(4.5, 4.5, 4.5),
        ];
        let dirs = [
            Vec3A::new(1.0, 0.0, 0.0),
            Vec3A::new(-1.0, 0.05, 0.02).normalize(),
            Vec3A::new(0.3, 0.3, 1.0).normalize(),
        ];
        for o in origins {
            for d in dirs {
                let ray = Ray::new(o, d);
                for t_max in [0.5, 3.0, f32::INFINITY] {
                    assert_eq!(
                        bvh.hit_any(&ray, 0.001, t_max),
                        bvh.hit(&ray, 0.001, t_max).is_some(),
                        "occlusion disagreement for {o:?} {d:?} t_max={t_max}"
                    );
                }
            }
        }
    }

    /// Node size is a load-bearing claim, not a comment: the leaf payload
    /// was moved into a side table precisely so the node still fits two
    /// cache lines. Growing it would silently cost traversal bandwidth.
    #[test]
    fn wide_node_is_two_cache_lines() {
        assert_eq!(std::mem::size_of::<WideNode>(), 128);
        assert_eq!(std::mem::align_of::<WideNode>(), 16);
    }

    /// Parallel subtree builds must not change the tree: the same input
    /// always produces byte-identical topology.
    #[test]
    fn build_is_deterministic() {
        let a = Bvh::new(sphere_grid(6));
        let b = Bvh::new(sphere_grid(6));
        assert_eq!(a.wide.len(), b.wide.len());
        assert_eq!(a.indices, b.indices);
        assert_eq!(a.packets.len(), b.packets.len());
        assert_eq!(a.leaves.len(), b.leaves.len());
        for (x, y) in a.leaves.iter().zip(&b.leaves) {
            assert_eq!((x.pkt_first, x.pkt_count), (y.pkt_first, y.pkt_count));
            assert_eq!((x.idx_first, x.idx_count), (y.idx_first, y.idx_count));
        }
        for (x, y) in a.wide.iter().zip(&b.wide) {
            assert_eq!(x.child, y.child);
            assert_eq!(x.flags, y.flags);
            assert_eq!(x.bmin_x, y.bmin_x);
            assert_eq!(x.bmax_z, y.bmax_z);
        }
    }

    /// The collapse must actually widen: a big tree ends up with far
    /// fewer wide nodes than a binary tree would need.
    #[test]
    fn collapse_widens_the_tree() {
        let bvh = Bvh::new(sphere_grid(6)); // 216 prims
        let n_leaf_slots: usize = bvh
            .wide
            .iter()
            .map(|w| (0..4).filter(|&l| w.is_leaf(l)).count())
            .sum();
        assert!(n_leaf_slots > 0);
        assert_eq!(n_leaf_slots, bvh.leaves.len());
        // A binary tree over L leaves has L-1 internal nodes; BVH4 should
        // need roughly a third of that.
        assert!(
            bvh.wide.len() * 2 < n_leaf_slots.max(2) * 2 - 1,
            "{} wide nodes for {} leaves",
            bvh.wide.len(),
            n_leaf_slots
        );
    }

    #[test]
    fn empty_bvh_misses() {
        let bvh = Bvh::new(Vec::new());
        let ray = Ray::new(Vec3A::ZERO, Vec3A::X);
        assert!(bvh.hit(&ray, 0.001, f32::INFINITY).is_none());
        assert!(bvh.bounds().is_none());
    }

    #[test]
    fn bounds_cover_all_prims() {
        let bvh = Bvh::new(sphere_grid(3));
        let bbox = bvh.bounds().expect("grid is fully bounded");
        assert!(bbox.minimum.cmple(Vec3A::splat(-0.5)).all());
        assert!(bbox.maximum.cmpge(Vec3A::splat(6.5)).all());
    }
}

#[cfg(test)]
mod lane_width {
    use super::*;
    use crate::prim::TrianglePrim;
    use crate::ray::MASK_ALL;
    use glam::Vec3A;

    fn uv_sphere_prims(segs: usize, rings: usize) -> Vec<Box<dyn Prim>> {
        let mut v = Vec::new();
        for r in 0..=rings {
            let phi = (r as f32 / rings as f32) * std::f32::consts::PI;
            for s in 0..=segs {
                let th = (s as f32 / segs as f32) * std::f32::consts::TAU;
                v.push(Vec3A::new(
                    phi.sin() * th.cos(),
                    phi.cos(),
                    phi.sin() * th.sin(),
                ));
            }
        }
        let row = segs + 1;
        let mut out: Vec<Box<dyn Prim>> = Vec::new();
        let mut id = 0u32;
        for r in 0..rings {
            for s in 0..segs {
                let (a, b, c, d) = (
                    r * row + s,
                    r * row + s + 1,
                    (r + 1) * row + s + 1,
                    (r + 1) * row + s,
                );
                for (i, j, k) in [(a, b, c), (a, c, d)] {
                    out.push(Box::new(TrianglePrim {
                        v0: v[i],
                        v1: v[j],
                        v2: v[k],
                        normals: None,
                        geom_id: 0,
                        prim_id: id,
                        mask: MASK_ALL,
                    }));
                    id += 1;
                }
            }
        }
        out
    }

    /// Records *why* the packets are 4 wide and not 8.
    ///
    /// The obvious next step from a 4-wide leaf intersector is an 8-wide one
    /// (AVX2). It would buy nothing here: with `MIN_LEAF_PACKED` at 4 and
    /// the SAH free to split above it, no leaf on a dense mesh holds more
    /// than four triangles, so a leaf already costs exactly one 4-lane
    /// round — and one 8-lane round, with half the lanes idle. This test
    /// asserts that equality, so if a future retune makes leaves bigger
    /// (raising `MIN_LEAF_PACKED`, or `MAX_LEAF` with a cheaper leaf cost)
    /// it fails and says the trade-off has changed.
    #[test]
    fn eight_wide_packets_would_not_reduce_vector_rounds() {
        let bvh = Bvh::new(uv_sphere_prims(80, 40));
        let per_leaf: Vec<usize> = bvh
            .leaves
            .iter()
            .map(|leaf| {
                bvh.packets[leaf.pkt_first as usize..(leaf.pkt_first + leaf.pkt_count) as usize]
                    .iter()
                    .map(|p| p.active.count_ones() as usize)
                    .sum()
            })
            .collect();

        let max = per_leaf.iter().copied().max().unwrap_or(0);
        assert!(max > 0, "no triangles ended up in leaves");
        assert!(
            max <= 4,
            "a leaf holds {max} triangles — 8-wide packets are now worth evaluating"
        );

        let rounds4: usize = per_leaf.iter().map(|n| n.div_ceil(4)).sum();
        let rounds8: usize = per_leaf.iter().map(|n| n.div_ceil(8)).sum();
        assert_eq!(
            rounds4, rounds8,
            "8-wide packets would cut vector rounds {rounds4} -> {rounds8}; \
             widening the leaf intersector is worth revisiting"
        );
    }
}
