//! Flattened bounding-volume hierarchy.
//!
//! One structure serves both levels of the scene: a per-mesh BVH over
//! triangles (built at USD import, placed by `Instance`) and the top-level
//! BVH over all scene objects (built by `Renderer::new`). Nodes live in a
//! contiguous array in depth-first order and are traversed iteratively
//! with a fixed stack — no per-node allocation, no `Arc` pointer chasing.
//!
//! The build is reference-based, in the SBVH mold (Stich et al. 2009):
//! every subtree owns a list of `PrimRef`s (bbox + primitive index). Each
//! node first evaluates a binned surface-area-heuristic *object* split on
//! the longest centroid axis; when the object split's children overlap by
//! more than `SBVH_ALPHA` of the root surface area it also evaluates a
//! binned *spatial* split, which chops straddling references in two at the
//! split plane (bounds via `Hittable::clipped_aabb` — exact polygon
//! clipping for triangles) and duplicates them into both children. The
//! cheaper split wins. Leaves therefore index into a shared `indices`
//! array (a primitive can appear in several leaves) while the primitives
//! themselves are stored exactly once.
//!
//! Large subtrees build in parallel via `rayon::join`. Every split
//! decision depends only on the input, so the tree is deterministic: the
//! same input always builds the same tree, threads only change *when*
//! subtrees are built, never *what*.

use crate::aabb::AABB;
use crate::hittable::{Hit, Hittable};
use crate::ray::Ray;
use glam::Vec3A;

/// Leaves are forced at this depth so the traversal stack can never
/// overflow; SAH partitions are otherwise free to be arbitrarily uneven.
const MAX_DEPTH: usize = 60;
/// Ranges at or below this size always become a leaf.
const MIN_LEAF: usize = 2;
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

struct Node {
    bbox: AABB,
    /// Leaf (`count > 0`): offset of the first entry in `indices`.
    /// Internal (`count == 0`): index of the right child — the left child
    /// immediately follows the node itself in depth-first order.
    first_or_right: u32,
    count: u32,
    /// Split axis of an internal node, for front-to-back child ordering.
    axis: u8,
}

pub struct Bvh {
    nodes: Vec<Node>,
    /// Leaf ranges index into this; spatial splits may list a primitive in
    /// more than one leaf.
    indices: Vec<u32>,
    /// The primitives, stored once each, in input order.
    prims: Vec<Box<dyn Hittable>>,
    /// Objects without a bounding box cannot enter the tree and are tested
    /// linearly on every ray.
    unbounded: Vec<Box<dyn Hittable>>,
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
    pub fn new(objects: Vec<Box<dyn Hittable>>) -> Self {
        let mut prims = Vec::with_capacity(objects.len());
        let mut refs = Vec::with_capacity(objects.len());
        let mut unbounded = Vec::new();
        for obj in objects {
            match obj.bounding_box() {
                Some(b) => {
                    refs.push(PrimRef {
                        bbox: b,
                        idx: prims.len() as u32,
                    });
                    prims.push(obj);
                }
                None => unbounded.push(obj),
            }
        }

        let (nodes, indices) = if refs.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let root_bbox = refs
                .iter()
                .skip(1)
                .fold(refs[0].bbox, |acc, r| AABB::surrounding_box(acc, r.bbox));
            let subtree = build_subtree(&prims, refs, 0, surface_area(&root_bbox));
            (subtree.nodes, subtree.indices)
        };

        Bvh {
            nodes,
            indices,
            prims,
            unbounded,
        }
    }

    pub fn count(&self) -> usize {
        self.prims.len() + self.unbounded.len()
    }
}

impl Hittable for Bvh {
    fn hit(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<Hit<'_>> {
        let mut closest = t_max;
        let mut best: Option<Hit> = None;

        for obj in &self.unbounded {
            if let Some(hit) = obj.hit(ray, t_min, closest) {
                closest = hit.rec.t;
                best = Some(hit);
            }
        }

        if self.nodes.is_empty() {
            return best;
        }

        // Depth is bounded by MAX_DEPTH and each traversal level holds at
        // most one deferred sibling on the stack.
        let mut stack = [0u32; MAX_DEPTH + 4];
        stack[0] = 0;
        let mut sp = 1usize;

        while sp > 0 {
            sp -= 1;
            let idx = stack[sp];
            let node = &self.nodes[idx as usize];
            if !node.bbox.hit(ray, t_min, closest) {
                continue;
            }
            if node.count > 0 {
                let first = node.first_or_right as usize;
                for &pi in &self.indices[first..first + node.count as usize] {
                    if let Some(hit) = self.prims[pi as usize].hit(ray, t_min, closest) {
                        closest = hit.rec.t;
                        best = Some(hit);
                    }
                }
            } else {
                // Visit the child on the ray's near side first so its hits
                // shrink `closest` before the far child is tested.
                let left = idx + 1;
                let right = node.first_or_right;
                let (near, far) = if ray.direction()[node.axis as usize] < 0.0 {
                    (right, left)
                } else {
                    (left, right)
                };
                stack[sp] = far;
                stack[sp + 1] = near;
                sp += 2;
            }
        }

        best
    }

    fn bounding_box(&self) -> Option<AABB> {
        if !self.unbounded.is_empty() {
            return None;
        }
        self.nodes.first().map(|n| n.bbox)
    }

    /// Early-exit occlusion traversal: no front-to-back ordering, returns on
    /// the first confirmed hit anywhere in `(t_min, t_max)`.
    fn hit_any(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        for obj in &self.unbounded {
            if obj.hit_any(ray, t_min, t_max) {
                return true;
            }
        }
        if self.nodes.is_empty() {
            return false;
        }

        let mut stack = [0u32; MAX_DEPTH + 4];
        stack[0] = 0;
        let mut sp = 1usize;

        while sp > 0 {
            sp -= 1;
            let idx = stack[sp];
            let node = &self.nodes[idx as usize];
            if !node.bbox.hit(ray, t_min, t_max) {
                continue;
            }
            if node.count > 0 {
                let first = node.first_or_right as usize;
                for &pi in &self.indices[first..first + node.count as usize] {
                    if self.prims[pi as usize].hit_any(ray, t_min, t_max) {
                        return true;
                    }
                }
            } else {
                stack[sp] = idx + 1;
                stack[sp + 1] = node.first_or_right;
                sp += 2;
            }
        }
        false
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
            axis: 0,
        }],
        indices: refs.iter().map(|r| r.idx).collect(),
    }
}

/// Splices `left`/`right` under a fresh internal node, rewriting the
/// children's local node indices and leaf offsets into the merged frame.
fn merge(bbox: AABB, axis: u8, left: Subtree, right: Subtree) -> Subtree {
    let mut nodes = Vec::with_capacity(1 + left.nodes.len() + right.nodes.len());
    let right_node_offset = 1 + left.nodes.len() as u32;
    nodes.push(Node {
        bbox,
        first_or_right: right_node_offset,
        count: 0,
        axis,
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
    let bin_of =
        |r: &PrimRef| (((r.centroid()[axis] - cmin[axis]) * scale) as usize).min(BINS - 1);

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
fn best_spatial_split(
    prims: &[Box<dyn Hittable>],
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
    prims: &[Box<dyn Hittable>],
    mut refs: Vec<PrimRef>,
    depth: usize,
    root_area: f32,
) -> Subtree {
    let bbox = union_all(&refs);
    let count = refs.len();
    if count <= MIN_LEAF || depth >= MAX_DEPTH {
        return leaf(bbox, &refs);
    }

    let object = best_object_split(&refs);

    // Consider a spatial split only when the object split's children
    // overlap enough for chopping to pay for the duplicated references.
    let spatial = match &object {
        Some(o) if depth < SBVH_MAX_DEPTH => {
            let overlap = intersect_aabb(&o.left_bbox, &o.right_bbox)
                .map_or(0.0, |b| surface_area(&b));
            if overlap / root_area > SBVH_ALPHA {
                best_spatial_split(prims, &refs, &bbox).filter(|s| s.cost < o.cost)
            } else {
                None
            }
        }
        _ => None,
    };

    let (axis, left_refs, right_refs) = if let Some(s) = spatial {
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
        (s.axis, left, right)
    } else {
        match object {
            Some(o) => {
                // Leaf when splitting costs more than intersecting through.
                if count <= MAX_LEAF && o.cost >= surface_area(&bbox) * count as f32 {
                    return leaf(bbox, &refs);
                }
                let (l, r) = partition_by_bin(&mut refs, &o);
                (o.axis, l, r)
            }
            // Every centroid coincides: median split by input order.
            None => {
                let mid = count / 2;
                let right = refs.split_off(mid);
                (0, refs, right)
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
    merge(bbox, axis as u8, l, r)
}

/// The non-spatial tail of `build_subtree`, reused by the degenerate-chop
/// fallback: object-partition when possible, else leaf.
fn object_partition_or_leaf(
    prims: &[Box<dyn Hittable>],
    mut refs: Vec<PrimRef>,
    bbox: AABB,
    object: Option<ObjSplit>,
    depth: usize,
    root_area: f32,
) -> Subtree {
    let count = refs.len();
    match object {
        Some(o) if count > MIN_LEAF => {
            let (l, r) = partition_by_bin(&mut refs, &o);
            if l.is_empty() || r.is_empty() {
                let mut all = l;
                all.extend(r);
                return leaf(bbox, &all);
            }
            let axis = o.axis as u8;
            let left = build_subtree(prims, l, depth + 1, root_area);
            let right = build_subtree(prims, r, depth + 1, root_area);
            merge(bbox, axis, left, right)
        }
        _ => leaf(bbox, &refs),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::OpenPBR;
    use crate::primitives::{Sphere, Triangle};
    use std::sync::Arc;

    fn sphere_grid(n: i32) -> Vec<Box<dyn Hittable>> {
        let mat = Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5)));
        let mut out: Vec<Box<dyn Hittable>> = Vec::new();
        for x in 0..n {
            for y in 0..n {
                for z in 0..n {
                    out.push(Box::new(Sphere::new(
                        Vec3A::new(x as f32, y as f32, z as f32) * 3.0,
                        0.5,
                        mat.clone(),
                    )));
                }
            }
        }
        out
    }

    /// Long thin diagonal triangles — the geometry spatial splits exist
    /// for. Built so object splits alone leave heavily overlapping
    /// children.
    fn diagonal_shards(n: i32) -> Vec<Box<dyn Hittable>> {
        let mat = Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5)));
        let mut out: Vec<Box<dyn Hittable>> = Vec::new();
        for i in 0..n {
            let o = i as f32 * 0.35;
            out.push(Box::new(Triangle::new(
                Vec3A::new(o, o, o),
                Vec3A::new(o + 10.0, o + 10.0, o + 10.2),
                Vec3A::new(o + 10.0, o + 10.3, o + 10.0),
                mat.clone(),
            )));
        }
        out
    }

    fn assert_matches_linear(objects: impl Fn() -> Vec<Box<dyn Hittable>>) {
        let bvh = Bvh::new(objects());
        let mut list = crate::hittable_list::HittableList::new();
        for obj in objects() {
            list.add(obj);
        }

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
                let b = list.hit(&ray, 0.001, f32::INFINITY);
                match (a, b) {
                    (Some(x), Some(y)) => {
                        assert!((x.rec.t - y.rec.t).abs() < 1e-4, "t mismatch for {o:?} {d:?}");
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
    /// otherwise the spatial-split path is dead code.
    #[test]
    fn spatial_splits_duplicate_references() {
        let bvh = Bvh::new(diagonal_shards(64));
        assert!(
            bvh.indices.len() > bvh.prims.len(),
            "no reference duplication: {} indices for {} prims",
            bvh.indices.len(),
            bvh.prims.len()
        );
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

    /// Parallel subtree builds must not change the tree: the same input
    /// always produces byte-identical topology.
    #[test]
    fn build_is_deterministic() {
        let a = Bvh::new(sphere_grid(6));
        let b = Bvh::new(sphere_grid(6));
        assert_eq!(a.nodes.len(), b.nodes.len());
        assert_eq!(a.indices, b.indices);
        for (x, y) in a.nodes.iter().zip(&b.nodes) {
            assert_eq!(x.first_or_right, y.first_or_right);
            assert_eq!(x.count, y.count);
            assert_eq!(x.axis, y.axis);
            assert_eq!(x.bbox.minimum, y.bbox.minimum);
            assert_eq!(x.bbox.maximum, y.bbox.maximum);
        }
    }

    #[test]
    fn empty_bvh_misses() {
        let bvh = Bvh::new(Vec::new());
        let ray = Ray::new(Vec3A::ZERO, Vec3A::X);
        assert!(bvh.hit(&ray, 0.001, f32::INFINITY).is_none());
        assert!(bvh.bounding_box().is_none());
    }

    #[test]
    fn bounding_box_covers_all_prims() {
        let bvh = Bvh::new(sphere_grid(3));
        let bbox = bvh.bounding_box().expect("grid is fully bounded");
        assert!(bbox.minimum.cmple(Vec3A::splat(-0.5)).all());
        assert!(bbox.maximum.cmpge(Vec3A::splat(6.5)).all());
    }
}
