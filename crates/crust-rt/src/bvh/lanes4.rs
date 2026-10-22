//! The 4-wide node: 4 lanes on `glam`'s `Vec4` (SSE2 / NEON), the width of
//! every tree before [`super::lanes8`] existed and still the width of every
//! tree that fits the cache.

use glam::Vec4;

use crate::ray::Ray;

use super::{WideNode, Width, safe_inv3};

/// The 4-wide [`Width`].
pub(super) struct W4;

/// The ray, pre-broadcast into the SoA layout the 4-wide slab test wants.
/// Built once per traversal: the six splats and the reciprocal used to be
/// recomputed for every visited node, which is pure overhead in a loop
/// that visits tens of nodes per ray.
pub(super) struct RaySlab4 {
    ox: Vec4,
    oy: Vec4,
    oz: Vec4,
    ix: Vec4,
    iy: Vec4,
    iz: Vec4,
    t_min: Vec4,
}

impl Width for W4 {
    const LANES: usize = 4;
    type Lanes = Vec4;
    type Children = [u32; 4];
    type Dist = [f32; 4];
    type Order = [(f32, usize); 4];
    type RaySlab = RaySlab4;

    #[inline]
    fn splat(v: f32) -> Vec4 {
        Vec4::splat(v)
    }

    #[inline]
    fn children(fill: u32) -> [u32; 4] {
        [fill; 4]
    }

    #[inline]
    fn order() -> [(f32, usize); 4] {
        [(0.0, 0); 4]
    }

    #[inline]
    fn ray_slab(ray: &Ray, t_min: f32) -> RaySlab4 {
        let o = ray.origin;
        let inv = safe_inv3(ray.dir);
        RaySlab4 {
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
    fn slab(rs: &RaySlab4, node: &WideNode<W4>, t_max: f32) -> (u32, [f32; 4]) {
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
        let mask = tnear.cmple(tfar).bitmask() & node.flags & Self::VALID_MASK;
        (mask, tnear.to_array())
    }
}
