//! Collapsing the binary build tree into 4-wide [`WideNode`]s, packing each
//! all-triangle leaf into `Tri4` SIMD packets.

use glam::Vec3A;

use crate::prim::PrimNode;
use crate::triangle::Tri4;

use super::build::surface_area;
use super::{Leaf, Node, WideNode};

/// Everything the collapse pass emits besides the nodes themselves: one
/// [`Leaf`] per leaf lane, the triangle packets those leaves run, and the
/// re-ordered one-at-a-time primitive indices.
#[derive(Default)]
pub(super) struct LeafData {
    pub(super) leaves: Vec<Leaf>,
    pub(super) packets: Vec<Tri4>,
    pub(super) indices: Vec<u32>,
}

impl LeafData {
    /// Turns one binary leaf's primitive range into a [`Leaf`]: triangles
    /// are packed four to a SIMD packet, everything else keeps its scalar
    /// index. Returns the new leaf's index.
    ///
    /// Packets are emitted contiguously per leaf, so a leaf's packets are
    /// one linear sweep of memory at traversal time.
    pub(super) fn push_leaf(&mut self, range: &[u32], prims: &[PrimNode]) -> u32 {
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
pub(super) fn collapse(
    binary: &[Node],
    indices: &[u32],
    prims: &[PrimNode],
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
    prims: &[PrimNode],
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

    for (lane, &kid) in kids.iter().enumerate().take(n_kids) {
        let k = kid as usize;
        let bounds = binary[k].bbox;
        out[slot].set_lane_bounds(lane, &bounds);
        if binary[k].count > 0 {
            out[slot].child[lane] = data.push_leaf(leaf_range(&binary[k], indices), prims);
            out[slot].mark_leaf(lane);
        } else {
            let ci = collapse_node(binary, indices, prims, kid, out, data);
            out[slot].child[lane] = ci;
        }
    }
    slot as u32
}
