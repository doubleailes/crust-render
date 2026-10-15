//! The default node: 4 lanes of `f32` bounds on `glam`'s `Vec4` (SSE2 /
//! NEON), stable Rust.

use glam::Vec4;

use crate::ray::Ray;

use super::node::{NodeFormat, f32_node};
use super::safe_inv3;

f32_node! {
    /// A 4-wide BVH node in SoA layout: lane `k` of each vector holds child
    /// `k`'s slab bounds, so one round of vector min/max tests every child
    /// box against the ray at once (Embree's BVH4 idea).
    ///
    /// Exactly 128 bytes (two cache lines): six `Vec4`s, one child index per
    /// lane, and the flag nibbles. The leaf payload (how many primitives, and
    /// where their SIMD packets live) sits in the separate `Leaf` table rather
    /// than in the node, which is what keeps the node this small.
    #[repr(C)]
    Node4, Vec4, 4, Vec4::splat
}

/// The ray, pre-broadcast into the SoA layout the 4-wide slab test wants.
/// Built once per traversal: the six splats and the reciprocal used to be
/// recomputed for every visited node, which is pure overhead in a loop
/// that visits tens of nodes per ray.
pub(super) struct RaySlab {
    ox: Vec4,
    oy: Vec4,
    oz: Vec4,
    ix: Vec4,
    iy: Vec4,
    iz: Vec4,
    t_min: Vec4,
}

impl NodeFormat<4> for Node4 {
    type Ray = RaySlab;

    #[inline]
    fn prepare(ray: &Ray, t_min: f32) -> RaySlab {
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

    /// The 4-lane slab test against all four child boxes of `node` at
    /// once: bit `l` of the returned mask is set iff lane `l` holds a real
    /// child whose box the ray crosses in `(t_min, t_max)`, and the array
    /// holds each lane's entry distance. One vector compare plus one
    /// movmskps gives all four verdicts; the validity bits drop unused
    /// lanes.
    #[inline]
    fn slab(rs: &RaySlab, node: &Node4, t_max: f32) -> (u32, [f32; 4]) {
        let t0x = (node.bmin_x - rs.ox) * rs.ix;
        let t1x = (node.bmax_x - rs.ox) * rs.ix;
        let t0y = (node.bmin_y - rs.oy) * rs.iy;
        let t1y = (node.bmax_y - rs.oy) * rs.iy;
        let t0z = (node.bmin_z - rs.oz) * rs.iz;
        let t1z = (node.bmax_z - rs.oz) * rs.iz;
        let tnear = t0x
            .min(t1x)
            .max(t0y.min(t1y))
            .max(t0z.min(t1z))
            .max(rs.t_min);
        let tfar = t0x
            .max(t1x)
            .min(t0y.max(t1y))
            .min(t0z.max(t1z))
            .min(Vec4::splat(t_max));
        let mask = tnear.cmple(tfar).bitmask() & node.flags & Node4::VALID_MASK;
        (mask, tnear.to_array())
    }

    #[inline]
    fn is_leaf(&self, lane: usize) -> bool {
        self.leaf_bit(lane)
    }

    #[inline]
    fn child_at(&self, lane: usize) -> u32 {
        self.child[lane]
    }
}
