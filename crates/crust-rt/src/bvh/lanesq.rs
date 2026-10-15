//! The `bvh8q` node: 8 children with bounds quantised to bytes, the node
//! format of Ylitie et al. (HPG 2017, "Efficient incoherent ray traversal
//! on GPUs through compressed wide BVHs") that obvhs ships as `CwBvh`.
//! Stable Rust, safe code: the byte→float conversion is plain `as f32`,
//! tested four lanes at a time on glam's `Vec4`.
//!
//! The tree is built and collapsed exactly as an `f32` 8-wide tree is (so
//! topology, leaves and packets are identical to `bvh8`), then
//! [`quantise_tree`] converts each [`BuildNode8`] into a [`QNode`] and
//! renumbers the nodes breadth-first
//! so a node's internal children, and its leaf children's `leaves` entries,
//! are contiguous. That is what lets a lane's child index be derived
//! (`base + popcount`) instead of stored, which is where most of the bytes
//! go. Only the node's *size and arithmetic* differ from `bvh8`, so the
//! two are an A/B of quantisation alone (design D1–D3).
//!
//! A child box is `p + q·2^(e-127)` per axis, rounded outward (floor for
//! min, ceil for max), so a quantised box contains the exact one: the test
//! can say "maybe" where the exact one says "no", never the reverse. Hits
//! stay exact because the triangles are tested exactly.

use glam::{Vec3A, Vec4};

use crate::ray::Ray;

use super::node::{NodeFormat, f32_node};
use super::{Leaf, safe_inv3};

fn splat8(v: f32) -> [f32; 8] {
    [v; 8]
}

f32_node! {
    /// The collapse's output for a quantised tree: plain `f32` arrays,
    /// converted to [`QNode`]s by [`quantise_tree`] and dropped. Never
    /// touched during traversal.
    BuildNode8, [f32; 8], 8, splat8
}

/// Grid resolution: 8-bit cells, so a node spans at most 255 of them.
const NQ_SCALE: f32 = 255.0;

/// A quantised 8-wide node, 76 bytes (obvhs's is 80: it also carries the
/// primitive base and per-child metadata this prototype keeps in `leaves`).
///
/// Lane `l`'s internal child is node `child_base + popcount(imask & (2^l -
/// 1))`; its leaf is `leaves[leaf_base + popcount(leaf_mask & (2^l - 1))]`.
#[repr(C)]
#[cfg_attr(feature = "bvh8q-pad", repr(align(128)))]
pub(super) struct QNode {
    /// Min corner of the node's box (the union of its children).
    p: [f32; 3],
    /// Biased exponent of the cell size per axis: cell = `2^(e - 127)`.
    e: [u8; 3],
    /// Bit `l` set: lane `l` holds an internal child.
    imask: u8,
    child_base: u32,
    leaf_base: u32,
    /// Bit `l` set: lane `l` holds a real child.
    valid: u8,
    qmin: [[u8; 8]; 3],
    qmax: [[u8; 8]; 3],
}

impl QNode {
    /// Dequantised box of lane `lane` (exact arithmetic, for tests).
    #[cfg(test)]
    pub(super) fn child_box(&self, lane: usize) -> ([f32; 3], [f32; 3]) {
        let mut lo = [0.0; 3];
        let mut hi = [0.0; 3];
        for a in 0..3 {
            let cell = f32::from_bits((self.e[a] as u32) << 23);
            lo[a] = self.p[a] + self.qmin[a][lane] as f32 * cell;
            hi[a] = self.p[a] + self.qmax[a][lane] as f32 * cell;
        }
        (lo, hi)
    }

    #[cfg(test)]
    pub(super) fn valid_mask(&self) -> u8 {
        self.valid
    }
}

/// Smallest biased exponent whose cell size makes 255 cells span `extent`.
fn cell_exponent(extent: f32) -> u8 {
    // 1e-30 keeps the quotient a normal float, so the bit tricks hold.
    let x = (extent / NQ_SCALE).max(1e-30);
    let bits = x.to_bits();
    let mut e = bits >> 23;
    if bits & 0x007f_ffff != 0 {
        e += 1; // round up to the next power of two
    }
    e.clamp(1, 254) as u8
}

fn quantise(w: &BuildNode8, imask: u8, child_base: u32, leaf_base: u32) -> QNode {
    let valid = (w.flags & BuildNode8::VALID_MASK) as u8;
    let lo = [&w.bmin_x, &w.bmin_y, &w.bmin_z];
    let hi = [&w.bmax_x, &w.bmax_y, &w.bmax_z];
    let lanes = || (0..8).filter(|&l| valid & (1 << l) != 0);

    let mut p = [0.0f32; 3];
    let mut e = [1u8; 3];
    let mut cell = [1.0f32; 3];
    for a in 0..3 {
        let mn = lanes().map(|l| lo[a][l]).fold(f32::INFINITY, f32::min);
        let mx = lanes().map(|l| hi[a][l]).fold(f32::NEG_INFINITY, f32::max);
        p[a] = mn;
        e[a] = cell_exponent(mx - mn);
        cell[a] = f32::from_bits((e[a] as u32) << 23);
    }

    // Unused lanes get an inverted box; `valid` masks them regardless.
    let mut qmin = [[255u8; 8]; 3];
    let mut qmax = [[0u8; 8]; 3];
    for l in lanes() {
        for a in 0..3 {
            let mut q0 = ((lo[a][l] - p[a]) / cell[a]).floor().clamp(0.0, 255.0) as u8;
            // Rounding in the subtraction can leave the grid line just
            // inside the child; step outward so the box stays conservative.
            while q0 > 0 && p[a] + q0 as f32 * cell[a] > lo[a][l] {
                q0 -= 1;
            }
            let mut q1 = ((hi[a][l] - p[a]) / cell[a]).ceil().clamp(0.0, 255.0) as u8;
            while q1 < 255 && p[a] + (q1 as f32) * cell[a] < hi[a][l] {
                q1 += 1;
            }
            qmin[a][l] = q0;
            qmax[a][l] = q1;
        }
    }
    QNode {
        p,
        e,
        imask,
        child_base,
        leaf_base,
        valid,
        qmin,
        qmax,
    }
}

/// Converts the collapse output into quantised nodes, renumbered
/// breadth-first with the root at 0. Children of a node are consecutive, as
/// are the `leaves` entries of its leaf lanes; the leaves' packet ranges are
/// untouched.
pub(super) fn quantise_tree(wide: Vec<BuildNode8>, leaves: Vec<Leaf>) -> (Vec<QNode>, Vec<Leaf>) {
    if wide.is_empty() {
        return (Vec::new(), leaves);
    }
    let mut out = Vec::with_capacity(wide.len());
    let mut new_leaves = Vec::with_capacity(leaves.len());
    let mut order: Vec<u32> = Vec::with_capacity(wide.len());
    order.push(0);
    let mut i = 0;
    while i < order.len() {
        let w = &wide[order[i] as usize];
        let child_base = order.len() as u32;
        let leaf_base = new_leaves.len() as u32;
        let mut imask = 0u8;
        for l in 0..8 {
            if w.flags & (1 << l) == 0 {
                continue;
            }
            if w.leaf_bit(l) {
                new_leaves.push(leaves[w.child[l] as usize]);
            } else {
                imask |= 1 << l;
                order.push(w.child[l]);
            }
        }
        out.push(quantise(w, imask, child_base, leaf_base));
        i += 1;
    }
    debug_assert_eq!(out.len(), wide.len());
    debug_assert_eq!(new_leaves.len(), leaves.len());
    (out, new_leaves)
}

/// The ray, kept as scalars: each node folds its own origin and cell size
/// into two per-axis constants, so a child's slab distance is one
/// multiply-add on the byte converted to `f32`.
pub(super) struct RaySlabQ {
    o: Vec3A,
    inv: Vec3A,
    t_min: f32,
}

impl NodeFormat<8> for QNode {
    type Ray = RaySlabQ;

    #[inline]
    fn prepare(ray: &Ray, t_min: f32) -> RaySlabQ {
        RaySlabQ {
            o: ray.origin,
            inv: safe_inv3(ray.dir),
            t_min,
        }
    }

    #[inline]
    fn is_leaf(&self, lane: usize) -> bool {
        (self.valid & !self.imask) & (1 << lane) != 0
    }

    /// Index of the child in lane `lane`: into the node array for an
    /// internal child, into `leaves` for a leaf.
    #[inline]
    fn child_at(&self, lane: usize) -> u32 {
        let below = (1u32 << lane) - 1;
        if self.is_leaf(lane) {
            let leaf_mask = (self.valid & !self.imask) as u32;
            self.leaf_base + (leaf_mask & below).count_ones()
        } else {
            self.child_base + (self.imask as u32 & below).count_ones()
        }
    }

    /// Slab test of all eight quantised boxes: as the `f32` test, with
    /// `t = q * (cell * inv) + (p - o) * inv`. `safe_inv3` bounds `inv` by
    /// 1e20, so for any scene this side of 1e18 units nothing overflows to
    /// the `0 * inf = NaN` that would poison `min`/`max`.
    #[inline]
    fn slab(rs: &RaySlabQ, node: &QNode, t_max: f32) -> (u32, [f32; 8]) {
        let cell = Vec3A::new(
            f32::from_bits((node.e[0] as u32) << 23),
            f32::from_bits((node.e[1] as u32) << 23),
            f32::from_bits((node.e[2] as u32) << 23),
        );
        let ae = cell * rs.inv;
        let ao = (Vec3A::from_array(node.p) - rs.o) * rs.inv;
        let tmin = Vec4::splat(rs.t_min);
        let tmax = Vec4::splat(t_max);

        let mut mask = 0u32;
        let mut tn = [0.0f32; 8];
        for h in 0..2 {
            let t = |q: &[u8; 8], a: usize| -> Vec4 {
                let b = h * 4;
                let qf = Vec4::new(
                    q[b] as f32,
                    q[b + 1] as f32,
                    q[b + 2] as f32,
                    q[b + 3] as f32,
                );
                qf * ae[a] + ao[a]
            };
            let t0x = t(&node.qmin[0], 0);
            let t1x = t(&node.qmax[0], 0);
            let t0y = t(&node.qmin[1], 1);
            let t1y = t(&node.qmax[1], 1);
            let t0z = t(&node.qmin[2], 2);
            let t1z = t(&node.qmax[2], 2);
            let tnear = t0x.min(t1x).max(t0y.min(t1y)).max(t0z.min(t1z)).max(tmin);
            let tfar = t0x.max(t1x).min(t0y.max(t1y)).min(t0z.max(t1z)).min(tmax);
            mask |= tnear.cmple(tfar).bitmask() << (h * 4);
            tn[h * 4..h * 4 + 4].copy_from_slice(&tnear.to_array());
        }
        (mask & node.valid as u32, tn)
    }
}
