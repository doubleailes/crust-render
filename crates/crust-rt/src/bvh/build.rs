//! The binned SAH / SBVH build: object and spatial split search, reference
//! clipping and duplication, and the parallel recursive subtree builder.
//!
//! Every decision reads only the input, never the schedule, so the result is
//! the same however `rayon::join` interleaves the subtrees.

use crate::aabb::AABB;

use super::{
    BINS, MAX_DEPTH, MAX_LEAF, MIN_LEAF, MIN_LEAF_PACKED, Node, PARALLEL_THRESHOLD, PrimRef,
    Primitives, SBVH_ALPHA, SBVH_MAX_DEPTH, Subtree,
};

pub(super) fn surface_area(b: &AABB) -> f32 {
    let d = b.maximum - b.minimum;
    2.0 * (d.x * d.y + d.y * d.z + d.z * d.x)
}

pub(super) fn union_all(refs: &[PrimRef]) -> AABB {
    refs.iter().skip(1).fold(refs[0].bbox(), |acc, r| {
        AABB::surrounding_box(acc, r.bbox())
    })
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

pub(super) fn leaf(bbox: AABB, refs: &[PrimRef]) -> Subtree {
    Subtree {
        nodes: vec![Node::new(bbox, 0, refs.len() as u32)],
        indices: refs.iter().map(|r| r.idx).collect(),
    }
}

/// Splices `left`/`right` under a fresh internal node, rewriting the
/// children's local node indices and leaf offsets into the merged frame.
///
/// Built *in* `left`'s vectors rather than into fresh ones: a third
/// allocation of `1 + left + right` while both children were still alive
/// put the whole tree in memory twice at the root merge, which on a
/// subdivided scene was the commit's peak. Shifting `left`'s nodes up by
/// one to make room for the parent is a memmove per level, O(n log n) in
/// all, and nothing against the SAH work that produced them.
pub(super) fn merge(bbox: AABB, left: Subtree, right: Subtree) -> Subtree {
    let Subtree {
        mut nodes,
        mut indices,
    } = left;
    let n_left = nodes.len() as u32;
    let right_node_offset = 1 + n_left;
    nodes.reserve_exact(1 + right.nodes.len());
    // The left child's internal nodes point one further once the parent
    // sits in front of them.
    for n in nodes.iter_mut() {
        if n.count == 0 {
            n.first_or_right += 1;
        }
    }
    nodes.insert(0, Node::new(bbox, right_node_offset, 0));
    let leaf_offset = indices.len() as u32;
    nodes.extend(right.nodes.into_iter().map(|mut n| {
        if n.count == 0 {
            n.first_or_right += right_node_offset;
        } else {
            n.first_or_right += leaf_offset;
        }
        n
    }));
    // Exact: `extend` alone would grow by doubling, and the slack of every
    // merge up the recursion would be alive at once.
    indices.reserve_exact(right.indices.len());
    indices.extend(right.indices);
    Subtree { nodes, indices }
}

/// The winning object split: partition predicate parameters plus the SAH
/// cost and the (unclipped) child bounds the overlap test needs.
struct ObjSplit {
    pub(super) axis: usize,
    /// References on each side, for the packet-aware leaf decision.
    pub(super) left_count: usize,
    pub(super) right_count: usize,
    /// Centroid-to-bin mapping: `bin = ((c - cmin) * scale) as usize`.
    pub(super) cmin: f32,
    pub(super) scale: f32,
    pub(super) split_bin: usize,
    pub(super) cost: f32,
    pub(super) left_bbox: AABB,
    pub(super) right_bbox: AABB,
}

/// The winning spatial split: chop plane on `axis` at `pos`.
pub(super) struct SpatSplit {
    pub(super) axis: usize,
    pub(super) pos: f32,
    pub(super) cost: f32,
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
            Some(existing) => AABB::surrounding_box(existing, r.bbox()),
            None => r.bbox(),
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
                left_count: lc,
                right_count: rc,
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
pub(super) fn best_spatial_split(
    prims: &Primitives,
    refs: &[PrimRef],
    bbox: &AABB,
) -> Option<SpatSplit> {
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
        let b0 = bin_of(r.min[axis]);
        let b1 = bin_of(r.max[axis]);
        if b0 == b1 {
            entry[b0] += 1;
            exit[b1] += 1;
            add(b0, r.bbox());
            continue;
        }
        // Entry and exit are counted at the first and last bin the reference
        // actually *lands* in, not at the ends of its bounding box, and the
        // difference is load-bearing. A reference's bbox is conservative — for
        // a reference produced by an earlier spatial split it is the clipped
        // box, which can overlap a bin the triangle itself misses entirely, so
        // `clipped_aabb` comes back `None` there. Counting the ends of the
        // bbox regardless left bins holding a count with no bounds, and the
        // cost loop below then took `lc > 0` as a promise that `lb` was
        // `Some`. On the DPEL lion's high-resolution mesh that promise broke
        // and the builder panicked outright. Deriving both from the same pass
        // is what makes the invariant true rather than merely usual.
        let mut first: Option<usize> = None;
        let mut last = b0;
        for b in b0..=b1 {
            let (bin_lo, bin_hi) = (lo + b as f32 * width, lo + (b + 1) as f32 * width);
            if let Some(c) = prims
                .clipped_aabb(r.idx, axis, bin_lo, bin_hi)
                .and_then(|c| intersect_aabb(&c, &r.bbox()))
            {
                add(b, c);
                first.get_or_insert(b);
                last = b;
            }
        }
        // No bin took it: the reference contributes nothing to this split, so
        // it must not be counted into either child either.
        if let Some(f) = first {
            entry[f] += 1;
            exit[last] += 1;
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
        // Safe by construction: a bin is only counted in `entry`/`exit` when
        // the same pass gave it bounds, so a non-zero count implies `Some`.
        let cost = surface_area(&lb.expect("lc > 0 implies bounds")) * lc as f32
            + surface_area(&rb.expect("rc > 0 implies bounds")) * rc as f32;
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
pub(super) fn build_subtree(
    prims: &Primitives,
    mut refs: Vec<PrimRef>,
    depth: usize,
    root_area: f32,
    packet_sah: bool,
) -> Subtree {
    let bbox = union_all(&refs);
    let count = refs.len();
    let all_triangles = refs.iter().all(|r| prims.is_triangle(r.idx));
    let min_leaf = if all_triangles {
        MIN_LEAF_PACKED
    } else {
        MIN_LEAF
    };
    if count <= min_leaf || depth >= MAX_DEPTH {
        return leaf(bbox, &refs);
    }

    let object = best_object_split(&refs);

    // By packet cost a small all-triangle range whose object split does
    // not pay is a leaf before any spatial split is weighed: chopping it
    // would only duplicate references into packets that are half empty.
    // (The per-triangle rule never leafed on this path; keeping that is
    // what makes `packet_sah = false` the behaviour it replaces.)
    if packet_sah
        && all_triangles
        && count <= MAX_LEAF
        && let Some(o) = &object
        && !splitting_pays(o, &bbox, count, true)
    {
        return leaf(bbox, &refs);
    }

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
        // Size each side first (bbox compares only, no clipping): a
        // straddling reference may land in both, so this is an upper
        // bound, but a far tighter one than the input length twice over.
        let (mut n_left, mut n_right) = (0usize, 0usize);
        for r in &refs {
            if r.max[s.axis] <= s.pos {
                n_left += 1;
            } else if r.min[s.axis] >= s.pos {
                n_right += 1;
            } else {
                n_left += 1;
                n_right += 1;
            }
        }
        let mut left = Vec::with_capacity(n_left);
        let mut right = Vec::with_capacity(n_right);
        for r in refs {
            if r.max[s.axis] <= s.pos {
                left.push(r);
            } else if r.min[s.axis] >= s.pos {
                right.push(r);
            } else {
                let bbox = r.bbox();
                if let Some(c) = prims
                    .clipped_aabb(r.idx, s.axis, f32::NEG_INFINITY, s.pos)
                    .and_then(|c| intersect_aabb(&c, &bbox))
                {
                    left.push(PrimRef::new(c, r.idx));
                }
                if let Some(c) = prims
                    .clipped_aabb(r.idx, s.axis, s.pos, f32::INFINITY)
                    .and_then(|c| intersect_aabb(&c, &bbox))
                {
                    right.push(PrimRef::new(c, r.idx));
                }
            }
        }
        // Degenerate chop (numeric edge): fall back to a leaf-or-object
        // path rather than recursing on an empty side.
        if left.is_empty() || right.is_empty() {
            let mut refs: Vec<PrimRef> = left;
            refs.extend(right);
            return object_partition_or_leaf(
                prims, refs, bbox, object, depth, root_area, packet_sah,
            );
        }
        (left, right)
    } else {
        match object {
            Some(o) => {
                // Leaf when splitting costs more than intersecting through.
                if count <= MAX_LEAF
                    && !splitting_pays(&o, &bbox, count, all_triangles && packet_sah)
                {
                    return leaf(bbox, &refs);
                }
                // By value: the parent's buffer is freed here rather than
                // staying alive through both children's parallel builds.
                partition_by_bin(refs, &o)
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
            || build_subtree(prims, left_refs, depth + 1, root_area, packet_sah),
            || build_subtree(prims, right_refs, depth + 1, root_area, packet_sah),
        )
    } else {
        (
            build_subtree(prims, left_refs, depth + 1, root_area, packet_sah),
            build_subtree(prims, right_refs, depth + 1, root_area, packet_sah),
        )
    };
    merge(bbox, l, r)
}

/// Whether the object split beats keeping the range as one leaf.
///
/// Per triangle (`packets` false, the rule before `CommitOptions::packet_sah`):
/// the split's `Σ area · count` against the leaf's `area · count`, with no
/// node cost, so a range only stays a leaf when its children are nearly as
/// large as it is.
///
/// Per packet (`packets` true, all-triangle ranges only): a leaf of `n`
/// triangles costs `ceil(n / 4)` SIMD rounds, the split the same per side
/// plus one node test — a 4-wide slab test, about one packet's worth — so
/// five to eight overlapping triangles stay one leaf of two full packets
/// instead of two half-empty ones. On the production scenes in the design
/// records packet lanes were 48% full under the per-triangle rule, and
/// every half-empty packet is 192 resident bytes, a `Leaf` and a node lane.
fn splitting_pays(o: &ObjSplit, bbox: &AABB, count: usize, packets: bool) -> bool {
    let area = surface_area(bbox);
    if !packets {
        return o.cost < area * count as f32;
    }
    let rounds = |n: usize| n.div_ceil(4) as f32;
    let split = surface_area(&o.left_bbox) * rounds(o.left_count)
        + surface_area(&o.right_bbox) * rounds(o.right_count)
        + area;
    split < area * rounds(count)
}

/// The non-spatial tail of `build_subtree`, reused by the degenerate-chop
/// fallback: object-partition when possible, else leaf.
#[allow(clippy::too_many_arguments)]
fn object_partition_or_leaf(
    prims: &Primitives,
    refs: Vec<PrimRef>,
    bbox: AABB,
    object: Option<ObjSplit>,
    depth: usize,
    root_area: f32,
    packet_sah: bool,
) -> Subtree {
    let count = refs.len();
    match object {
        Some(o) if count > min_leaf_for(prims, &refs) => {
            let (l, r) = partition_by_bin(refs, &o);
            if l.is_empty() || r.is_empty() {
                let mut all = l;
                all.extend(r);
                return leaf(bbox, &all);
            }
            let left = build_subtree(prims, l, depth + 1, root_area, packet_sah);
            let right = build_subtree(prims, r, depth + 1, root_area, packet_sah);
            merge(bbox, left, right)
        }
        _ => leaf(bbox, &refs),
    }
}

/// The leaf-size floor for this range: [`MIN_LEAF_PACKED`] when every
/// reference is a triangle (so the leaf becomes exactly one SIMD packet),
/// [`MIN_LEAF`] otherwise.
fn min_leaf_for(prims: &Primitives, refs: &[PrimRef]) -> usize {
    if refs.iter().all(|r| prims.is_triangle(r.idx)) {
        MIN_LEAF_PACKED
    } else {
        MIN_LEAF
    }
}

/// Order-preserving partition of `refs` by the object split's centroid
/// bin — deterministic for a given input order.
///
/// Takes `refs` **by value** and counts each side before allocating, so
/// the two children together hold exactly one copy of the input rather
/// than two. Sizing both sides at the full input length instead — the
/// obvious one-pass version — wastes an allocation the size of the input
/// at every node of the recursion, and hands the parent's buffer down
/// alive into a parallel subtree build. On a scene with 100M references
/// that is the difference between a build that fits and one that does
/// not. The extra pass is a few flops per reference against an
/// allocation it avoids touching at all.
fn partition_by_bin(refs: Vec<PrimRef>, o: &ObjSplit) -> (Vec<PrimRef>, Vec<PrimRef>) {
    let bin_of = |r: &PrimRef| (((r.centroid()[o.axis] - o.cmin) * o.scale) as usize).min(BINS - 1);
    let n_left = refs.iter().filter(|r| bin_of(r) <= o.split_bin).count();
    let mut left = Vec::with_capacity(n_left);
    let mut right = Vec::with_capacity(refs.len() - n_left);
    for r in refs {
        if bin_of(&r) <= o.split_bin {
            left.push(r);
        } else {
            right.push(r);
        }
    }
    (left, right)
}
