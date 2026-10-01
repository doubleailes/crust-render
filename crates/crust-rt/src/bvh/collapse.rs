//! Collapsing the binary build tree into [`LANES`]-wide [`WideNode`]s, packing each
//! all-triangle leaf into `Tri4` SIMD packets.

use crate::triangle::{PacketTri, Tri4, Tri4i};

use super::build::surface_area;
use super::{LANES, Layout, Leaf, Node, Primitives, WideNode};

/// Everything the collapse pass emits besides the nodes themselves: one
/// [`Leaf`] per leaf lane, the triangle packets those leaves run, and the
/// re-ordered one-at-a-time primitive indices.
#[derive(Default)]
pub(super) struct LeafData {
    pub(super) leaves: Vec<Leaf>,
    /// Exactly one of the two packet tables is used, per `Layout`.
    pub(super) packets: Vec<Tri4>,
    pub(super) packets_i: Vec<Tri4i>,
    pub(super) indices: Vec<u32>,
}

impl LeafData {
    /// Turns one binary leaf's primitive range into a [`Leaf`]: triangles
    /// are packed four to a SIMD packet, everything else keeps its scalar
    /// index. Returns the new leaf's index.
    ///
    /// Packets are emitted contiguously per leaf, so a leaf's packets are
    /// one linear sweep of memory at traversal time.
    pub(super) fn push_leaf(&mut self, range: &[u32], prims: &Primitives, layout: Layout) -> u32 {
        let pkt_first = self.packet_count() as u32;
        let mut batch: Vec<PacketTri> = Vec::with_capacity(4);
        let idx_first = self.indices.len() as u32;
        let mut idx_count = 0u32;

        for &pi in range {
            if prims.is_triangle(pi) {
                let rec = &prims.tris[pi as usize];
                batch.push(PacketTri {
                    verts: prims.tri_verts(rec),
                    idx: rec.v,
                    rec: pi,
                    mask: rec.mask,
                });
                if batch.len() == 4 {
                    self.push_packet(&batch, layout);
                    batch.clear();
                }
            } else {
                // Scalar primitives are indexed within their own table.
                self.indices.push(prims.resident_id(pi));
                idx_count += 1;
            }
        }
        if !batch.is_empty() {
            // A partial tail packet still beats scalar calls: the unused
            // lanes ride along for free.
            self.push_packet(&batch, layout);
        }

        self.leaves.push(Leaf {
            pkt_first,
            pkt_count: self.packet_count() as u32 - pkt_first,
            idx_first,
            idx_count,
        });
        self.leaves.len() as u32 - 1
    }

    fn packet_count(&self) -> usize {
        self.packets.len() + self.packets_i.len()
    }

    fn push_packet(&mut self, batch: &[PacketTri], layout: Layout) {
        match layout {
            Layout::Gathered => self.packets.push(Tri4::new(batch)),
            Layout::Indexed => self.packets_i.push(Tri4i::new(batch)),
        }
    }
}

/// Collapses the binary tree into [`LANES`]-wide nodes: each wide node adopts its
/// binary node's two children, then repeatedly replaces the largest-area
/// internal child with that child's own two children until every lane is
/// filled (or only leaves remain). Leaf lanes are converted to [`Leaf`]
/// entries with their triangles packed into SIMD packets as they are
/// reached. Purely input-driven, so determinism is preserved.
pub(super) fn collapse(
    binary: &[Node],
    indices: &[u32],
    prims: &Primitives,
    layout: Layout,
) -> (Vec<WideNode>, LeafData) {
    // A binary tree of `n` nodes has `(n + 1) / 2` leaves, and a full
    // `LANES`-wide tree over `L` leaves needs `(L - 1) / (LANES - 1)`
    // internal nodes. Reserving one node per leaf instead (as this once did)
    // held three times the 4-wide tree's nodes and seven times the 8-wide
    // one's; an uneven tree that needs more just grows the vector.
    let leaves = binary.len().div_ceil(2);
    let mut out = Vec::with_capacity(leaves.saturating_sub(1) / (LANES - 1) + 1);
    // The leaf tables are sized exactly from the binary leaves before any
    // is emitted: a packet is 192 bytes, and letting the vector double
    // meant up to twice the packets' final size was alive at the build's
    // peak, on top of the binary tree still being read.
    let mut n_packets = 0usize;
    let mut n_indices = 0usize;
    for leaf in binary.iter().filter(|n| n.count > 0) {
        let tris = leaf_range(leaf, indices)
            .iter()
            .filter(|&&i| prims.is_triangle(i))
            .count();
        n_packets += tris.div_ceil(4);
        n_indices += leaf.count as usize - tris;
    }
    let (gathered, indexed) = match layout {
        Layout::Gathered => (n_packets, 0),
        Layout::Indexed => (0, n_packets),
    };
    let mut data = LeafData {
        leaves: Vec::with_capacity(leaves),
        packets: Vec::with_capacity(gathered),
        packets_i: Vec::with_capacity(indexed),
        indices: Vec::with_capacity(n_indices),
    };
    if binary.is_empty() {
        return (out, data);
    }
    if binary[0].count > 0 {
        // Single-leaf tree.
        let mut w = WideNode::empty();
        w.set_lane_bounds(0, &binary[0].bbox());
        w.child[0] = data.push_leaf(leaf_range(&binary[0], indices), prims, layout);
        w.mark_leaf(0);
        out.push(w);
        return (out, data);
    }
    collapse_node(binary, indices, prims, layout, 0, &mut out, &mut data);
    // These live as long as the scene, and `Bvh::accumulate_footprint`
    // counts capacity, so a growth doubling's slack would be both resident
    // and reported. Trimming costs one copy per vector at build time.
    out.shrink_to_fit();
    data.leaves.shrink_to_fit();
    data.packets.shrink_to_fit();
    data.packets_i.shrink_to_fit();
    data.indices.shrink_to_fit();
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
    prims: &Primitives,
    layout: Layout,
    b_idx: u32,
    out: &mut Vec<WideNode>,
    data: &mut LeafData,
) -> u32 {
    let slot = out.len();
    out.push(WideNode::empty());

    let mut kids = [0u32; LANES];
    kids[0] = b_idx + 1;
    kids[1] = binary[b_idx as usize].first_or_right;
    let mut n_kids = 2;
    while n_kids < LANES {
        // Expand the internal kid with the largest surface area; ties
        // resolve to the first (deterministic).
        let mut best: Option<(usize, f32)> = None;
        for (i, &k) in kids.iter().enumerate().take(n_kids) {
            if binary[k as usize].count == 0 {
                let a = surface_area(&binary[k as usize].bbox());
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
        let bounds = binary[k].bbox();
        out[slot].set_lane_bounds(lane, &bounds);
        if binary[k].count > 0 {
            out[slot].child[lane] = data.push_leaf(leaf_range(&binary[k], indices), prims, layout);
            out[slot].mark_leaf(lane);
        } else {
            let ci = collapse_node(binary, indices, prims, layout, kid, out, data);
            out[slot].child[lane] = ci;
        }
    }
    slot as u32
}
