//! Flattened bounding-volume hierarchy.
//!
//! One structure serves both levels of the scene: a per-mesh BVH over
//! triangles (built at USD import) and the top-level BVH over all scene
//! objects (built by `Renderer::new`). Nodes live in a contiguous array in
//! depth-first order and are traversed iteratively with a fixed stack —
//! no per-node allocation, no `Arc` pointer chasing. Splits use binned
//! surface-area-heuristic partitioning on the longest centroid axis, which
//! is deterministic: the same input always builds the same tree.

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

struct Node {
    bbox: AABB,
    /// Leaf (`count > 0`): index of the first primitive in `prims`.
    /// Internal (`count == 0`): index of the right child — the left child
    /// immediately follows the node itself in depth-first order.
    first_or_right: u32,
    count: u32,
    /// Split axis of an internal node, for front-to-back child ordering.
    axis: u8,
}

pub struct Bvh {
    nodes: Vec<Node>,
    /// Primitives permuted into leaf order, so each leaf owns a contiguous run.
    prims: Vec<Box<dyn Hittable>>,
    /// Objects without a bounding box cannot enter the tree and are tested
    /// linearly on every ray.
    unbounded: Vec<Box<dyn Hittable>>,
}

impl Bvh {
    pub fn new(objects: Vec<Box<dyn Hittable>>) -> Self {
        let mut bounded = Vec::with_capacity(objects.len());
        let mut bboxes = Vec::with_capacity(objects.len());
        let mut unbounded = Vec::new();
        for obj in objects {
            match obj.bounding_box() {
                Some(b) => {
                    bboxes.push(b);
                    bounded.push(obj);
                }
                None => unbounded.push(obj),
            }
        }

        let centroids: Vec<Vec3A> = bboxes
            .iter()
            .map(|b| 0.5 * (b.minimum + b.maximum))
            .collect();
        let mut order: Vec<u32> = (0..bounded.len() as u32).collect();
        let mut nodes = Vec::new();
        if !bounded.is_empty() {
            nodes.reserve(2 * bounded.len());
            build_range(&mut nodes, &mut order, 0, bounded.len(), &bboxes, &centroids, 0);
        }

        // Permute the primitives so leaf ranges are contiguous.
        let mut slots: Vec<Option<Box<dyn Hittable>>> = bounded.into_iter().map(Some).collect();
        let prims = order
            .iter()
            .map(|&i| slots[i as usize].take().expect("permutation visits each slot once"))
            .collect();

        Bvh {
            nodes,
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
                for prim in &self.prims[first..first + node.count as usize] {
                    if let Some(hit) = prim.hit(ray, t_min, closest) {
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
}

fn surface_area(b: &AABB) -> f32 {
    let d = b.maximum - b.minimum;
    2.0 * (d.x * d.y + d.y * d.z + d.z * d.x)
}

/// Builds the node for `order[start..end]` (indices into the bbox/centroid
/// arrays), appending it and its subtree to `nodes` in depth-first order.
/// Returns the node's index. Leaf `first` indices refer to positions in
/// `order`, which after the build become positions in the permuted `prims`.
fn build_range(
    nodes: &mut Vec<Node>,
    order: &mut [u32],
    start: usize,
    end: usize,
    bboxes: &[AABB],
    centroids: &[Vec3A],
    depth: usize,
) -> u32 {
    let count = end - start;
    let mut bbox = bboxes[order[start] as usize];
    for &i in &order[start + 1..end] {
        bbox = AABB::surrounding_box(bbox, bboxes[i as usize]);
    }

    let idx = nodes.len() as u32;
    let leaf = |nodes: &mut Vec<Node>| {
        nodes.push(Node {
            bbox,
            first_or_right: start as u32,
            count: count as u32,
            axis: 0,
        });
        idx
    };

    if count <= MIN_LEAF || depth >= MAX_DEPTH {
        return leaf(nodes);
    }

    // Split along the longest axis of the centroid bounds.
    let mut cmin = centroids[order[start] as usize];
    let mut cmax = cmin;
    for &i in &order[start + 1..end] {
        cmin = cmin.min(centroids[i as usize]);
        cmax = cmax.max(centroids[i as usize]);
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
        return leaf(nodes);
    }

    // Binned SAH: histogram the centroids, then score every split plane
    // between adjacent bins by `area · count` on each side.
    let bin_of = |i: u32| -> usize {
        let t = (centroids[i as usize][axis] - cmin[axis]) / extent[axis];
        ((t * BINS as f32) as usize).min(BINS - 1)
    };
    let mut bin_counts = [0usize; BINS];
    let mut bin_bounds: [Option<AABB>; BINS] = [None; BINS];
    for &i in &order[start..end] {
        let b = bin_of(i);
        bin_counts[b] += 1;
        bin_bounds[b] = Some(match bin_bounds[b] {
            Some(existing) => AABB::surrounding_box(existing, bboxes[i as usize]),
            None => bboxes[i as usize],
        });
    }

    let side_cost = |bounds: Option<AABB>, count: usize| -> f32 {
        match bounds {
            Some(b) if count > 0 => surface_area(&b) * count as f32,
            _ => 0.0,
        }
    };
    let mut best: Option<(usize, f32)> = None; // (last bin of the left side, cost)
    for split in 0..BINS - 1 {
        let mut lb = None;
        let mut lc = 0usize;
        for b in 0..=split {
            lc += bin_counts[b];
            lb = match (lb, bin_bounds[b]) {
                (Some(x), Some(y)) => Some(AABB::surrounding_box(x, y)),
                (x, y) => x.or(y),
            };
        }
        let mut rb = None;
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
        let cost = side_cost(lb, lc) + side_cost(rb, rc);
        if best.is_none_or(|(_, c)| cost < c) {
            best = Some((split, cost));
        }
    }

    let mid = match best {
        // Small ranges may stay a leaf when the best split costs more than
        // intersecting every primitive directly.
        Some((_, cost)) if count <= MAX_LEAF && cost >= surface_area(&bbox) * count as f32 => {
            return leaf(nodes);
        }
        Some((split, _)) => {
            // Unstable in-place partition by bin — deterministic for a given
            // input order.
            let mut mid = start;
            for i in start..end {
                if bin_of(order[i]) <= split {
                    order.swap(i, mid);
                    mid += 1;
                }
            }
            mid
        }
        // Every populated bin on one side (can happen with extreme float
        // distributions): fall back to a median split.
        None => {
            order[start..end]
                .sort_unstable_by(|&a, &b| {
                    centroids[a as usize][axis].total_cmp(&centroids[b as usize][axis])
                });
            start + count / 2
        }
    };

    nodes.push(Node {
        bbox,
        first_or_right: 0, // patched once the left subtree is laid out
        count: 0,
        axis: axis as u8,
    });
    build_range(nodes, order, start, mid, bboxes, centroids, depth + 1);
    let right = build_range(nodes, order, mid, end, bboxes, centroids, depth + 1);
    nodes[idx as usize].first_or_right = right;
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::OpenPBR;
    use crate::primitives::Sphere;
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

    /// The BVH must find exactly the hits a linear scan finds.
    #[test]
    fn matches_linear_scan() {
        let bvh = Bvh::new(sphere_grid(4));
        let mut list = crate::hittable_list::HittableList::new();
        for obj in sphere_grid(4) {
            list.add(obj);
        }

        let origins = [
            Vec3A::new(-5.0, 4.5, 4.5),
            Vec3A::new(20.0, 3.0, 3.0),
            Vec3A::new(4.5, -5.0, 4.5),
            Vec3A::new(0.0, 0.0, -10.0),
        ];
        let dirs = [
            Vec3A::new(1.0, 0.0, 0.0),
            Vec3A::new(-1.0, 0.05, 0.02).normalize(),
            Vec3A::new(0.0, 1.0, 0.0),
            Vec3A::new(0.3, 0.3, 1.0).normalize(),
            Vec3A::new(0.0, 0.0, -1.0),
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
            }
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
