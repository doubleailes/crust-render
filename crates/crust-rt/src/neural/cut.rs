//! The cut: the set of `(wide node, lane)` children of a scene's own BVH at
//! which a proxied occlusion query stops descending and asks the model.
//!
//! Every leaf lies under exactly one cut lane, so a ray that reaches a leaf's
//! box reaches a cut box first. The first rule is the subtree size (cut at
//! the highest lanes whose subtree holds at most [`SIZE_CUT_REFS`] triangle
//! references); N-BVH's refinement then splits the lanes whose validation
//! error is largest ([`CutSet::split`]).

use crate::bvh::{Bvh, LaneChild, WIDTH};
use crate::scene::Scene;

/// The size rule's K: triangle references per cut subtree.
pub(crate) const SIZE_CUT_REFS: usize = 4096;

/// A cut, as a bitset over `node · WIDTH + lane` beside the tree, plus the
/// same lanes listed in a fixed order so the trainer can index them.
pub(crate) struct CutSet {
    bits: Vec<u64>,
    lanes: Vec<(u32, u8)>,
}

impl CutSet {
    fn empty(nodes: usize) -> Self {
        CutSet {
            bits: vec![0; (nodes * WIDTH).div_ceil(64)],
            lanes: Vec::new(),
        }
    }

    #[inline]
    pub(crate) fn contains(&self, node: u32, lane: usize) -> bool {
        let k = node as usize * WIDTH + lane;
        self.bits[k >> 6] & (1 << (k & 63)) != 0
    }

    fn insert(&mut self, node: u32, lane: usize) {
        let k = node as usize * WIDTH + lane;
        self.bits[k >> 6] |= 1 << (k & 63);
        self.lanes.push((node, lane as u8));
    }

    /// The cut lanes, in the order the cut was formed.
    pub(crate) fn lanes(&self) -> &[(u32, u8)] {
        &self.lanes
    }

    pub(crate) fn len(&self) -> usize {
        self.lanes.len()
    }

    pub(crate) fn bytes(&self) -> usize {
        std::mem::size_of_val(&*self.bits) + std::mem::size_of_val(&*self.lanes)
    }

    /// The subtree-size cut: from the root down, a lane becomes a cut lane
    /// when its subtree holds at most `k` references, or when it is a leaf.
    pub(crate) fn by_size(bvh: &Bvh, k: usize, refs: &dyn Fn(&Scene) -> usize) -> Self {
        let weights = subtree_weights(bvh, refs);
        let mut cut = CutSet::empty(bvh.node_count());
        if bvh.node_count() > 0 {
            descend(bvh, 0, k, &weights, &mut cut);
        }
        cut
    }

    /// The same cut with each of `lanes` (indices into [`CutSet::lanes`])
    /// replaced by its children, when it has any. Leaf lanes stay. The new
    /// order keeps the unsplit lanes in place and appends the children in
    /// the order of `lanes`, so a split is deterministic.
    pub(crate) fn split(&self, bvh: &Bvh, lanes: &[usize]) -> Self {
        let mut out = CutSet::empty(bvh.node_count());
        let mut children = Vec::new();
        for (i, &(node, lane)) in self.lanes.iter().enumerate() {
            let child = match bvh.lane(node, lane as usize) {
                Some((_, LaneChild::Node(c))) if lanes.contains(&i) => Some(c),
                _ => None,
            };
            match child {
                Some(c) => children.push(c),
                None => out.insert(node, lane as usize),
            }
        }
        for c in children {
            for l in 0..WIDTH {
                if bvh.lane(c, l).is_some() {
                    out.insert(c, l);
                }
            }
        }
        out
    }
}

/// References under each lane: `[node · WIDTH + lane]`, zero for unused.
fn subtree_weights(bvh: &Bvh, refs: &dyn Fn(&Scene) -> usize) -> Vec<usize> {
    let mut w = vec![0usize; bvh.node_count() * WIDTH];
    if bvh.node_count() > 0 {
        weigh(bvh, 0, refs, &mut w);
    }
    w
}

fn weigh(bvh: &Bvh, node: u32, refs: &dyn Fn(&Scene) -> usize, w: &mut [usize]) -> usize {
    let mut total = 0;
    for l in 0..WIDTH {
        let v = match bvh.lane(node, l) {
            None => 0,
            Some((_, LaneChild::Leaf(leaf))) => bvh.leaf_weight(leaf, &mut |s| refs(s)),
            Some((_, LaneChild::Node(c))) => weigh(bvh, c, refs, w),
        };
        w[node as usize * WIDTH + l] = v;
        total += v;
    }
    total
}

fn descend(bvh: &Bvh, node: u32, k: usize, w: &[usize], cut: &mut CutSet) {
    for l in 0..WIDTH {
        match bvh.lane(node, l) {
            None => {}
            Some((_, LaneChild::Node(c))) if w[node as usize * WIDTH + l] > k => {
                descend(bvh, c, k, w, cut)
            }
            Some(_) => cut.insert(node, l),
        }
    }
}

/// Every leaf under `(node, lane)`, appended to `out`.
#[cfg(test)]
pub(crate) fn leaves_under(bvh: &Bvh, node: u32, lane: usize, out: &mut Vec<u32>) {
    match bvh.lane(node, lane) {
        None => {}
        Some((_, LaneChild::Leaf(leaf))) => out.push(leaf),
        Some((_, LaneChild::Node(c))) => {
            for l in 0..WIDTH {
                leaves_under(bvh, c, l, out);
            }
        }
    }
}
