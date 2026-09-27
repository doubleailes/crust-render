//! The default node width: 4 lanes on `glam`'s `Vec4` (SSE2 / NEON), stable
//! Rust. The `bvh8` feature swaps this module for [`super::lanes8`]; both
//! expose the same four items, so the rest of the BVH is width-agnostic.

use glam::Vec4;

use crate::ray::Ray;

use super::{WideNode, safe_inv3};

/// Children per wide node.
pub(super) const LANES: usize = 4;

/// One slab-bound coordinate for every lane of a node.
pub(super) type Lanes = Vec4;

#[inline]
pub(super) fn splat(v: f32) -> Lanes {
    Vec4::splat(v)
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

impl RaySlab {
    #[inline]
    pub(super) fn new(ray: &Ray, t_min: f32) -> Self {
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
    pub(super) fn slab(&self, node: &WideNode, t_max: f32) -> (u32, [f32; LANES]) {
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
        let mask = tnear.cmple(tfar).bitmask() & node.flags & super::VALID_MASK;
        (mask, tnear.to_array())
    }
}
