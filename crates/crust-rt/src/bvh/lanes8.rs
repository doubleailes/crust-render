//! The 8-wide node, for trees too large for the cache: there traversal is
//! bound by the latency of fetching each node, and a node that tests eight
//! children instead of four is fetched 30-40% less often.
//!
//! On stable the eight lanes are two `glam` `Vec4` halves — the 4-wide slab
//! test run twice over one 256-byte node, with the two verdict nibbles
//! joined into one mask. That is the wide *tree* without wide
//! *instructions*, and it is what pays out of cache: the saving is node
//! fetches, not arithmetic. The nightly-only `bvh8` feature swaps the halves
//! for one `std::simd::f32x8` (one AVX register at `-C
//! target-cpu=x86-64-v3`); both are safe code and give bit-identical
//! verdicts, since `safe_inv3` keeps every lane NaN-free and minNum/maxNum
//! agree with `minps`/`maxps` there.

#[cfg(feature = "bvh8")]
use std::simd::{cmp::SimdPartialOrd, f32x8, num::SimdFloat};

#[cfg(not(feature = "bvh8"))]
use glam::Vec4;

use crate::ray::Ray;

use super::{WideNode, Width, safe_inv3};

/// The 8-wide [`Width`].
pub(super) struct W8;

/// Eight slab-bound coordinates as two 4-lane halves: lanes `0..4` in the
/// first, `4..8` in the second. Aligned like an `f32x8`, so the node is 256
/// bytes either way rather than the 240 its fields need.
#[cfg(not(feature = "bvh8"))]
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C, align(32))]
pub(super) struct Lanes8([Vec4; 2]);

#[cfg(not(feature = "bvh8"))]
impl std::ops::Index<usize> for Lanes8 {
    type Output = f32;
    #[inline]
    fn index(&self, lane: usize) -> &f32 {
        &self.0[lane >> 2][lane & 3]
    }
}

#[cfg(not(feature = "bvh8"))]
impl std::ops::IndexMut<usize> for Lanes8 {
    #[inline]
    fn index_mut(&mut self, lane: usize) -> &mut f32 {
        &mut self.0[lane >> 2][lane & 3]
    }
}

#[cfg(feature = "bvh8")]
pub(super) type Lanes8 = f32x8;

/// The ray, pre-broadcast once per traversal. Four lanes are enough even
/// for the 8-wide test on stable: each half reuses the same splats.
#[cfg(not(feature = "bvh8"))]
pub(super) struct RaySlab8 {
    ox: Vec4,
    oy: Vec4,
    oz: Vec4,
    ix: Vec4,
    iy: Vec4,
    iz: Vec4,
    t_min: Vec4,
}

#[cfg(feature = "bvh8")]
pub(super) struct RaySlab8 {
    ox: f32x8,
    oy: f32x8,
    oz: f32x8,
    ix: f32x8,
    iy: f32x8,
    iz: f32x8,
    t_min: f32x8,
}

#[cfg(not(feature = "bvh8"))]
impl RaySlab8 {
    /// One half of the 8-lane test: the 4-wide slab test, operation for
    /// operation, on lanes `4h..4h + 4`.
    #[inline]
    fn half(&self, node: &WideNode<W8>, h: usize, t_max: Vec4) -> (u32, Vec4) {
        let t0x = (node.bmin_x.0[h] - self.ox) * self.ix;
        let t1x = (node.bmax_x.0[h] - self.ox) * self.ix;
        let t0y = (node.bmin_y.0[h] - self.oy) * self.iy;
        let t1y = (node.bmax_y.0[h] - self.oy) * self.iy;
        let t0z = (node.bmin_z.0[h] - self.oz) * self.iz;
        let t1z = (node.bmax_z.0[h] - self.oz) * self.iz;
        let tnear = t0x
            .min(t1x)
            .max(t0y.min(t1y))
            .max(t0z.min(t1z))
            .max(self.t_min);
        let tfar = t0x.max(t1x).min(t0y.max(t1y)).min(t0z.max(t1z)).min(t_max);
        (tnear.cmple(tfar).bitmask(), tnear)
    }
}

impl Width for W8 {
    const LANES: usize = 8;
    type Lanes = Lanes8;
    type Children = [u32; 8];
    type Dist = [f32; 8];
    type Order = [(f32, usize); 8];
    type RaySlab = RaySlab8;

    #[inline]
    fn splat(v: f32) -> Lanes8 {
        #[cfg(not(feature = "bvh8"))]
        return Lanes8([Vec4::splat(v); 2]);
        #[cfg(feature = "bvh8")]
        return f32x8::splat(v);
    }

    #[inline]
    fn children(fill: u32) -> [u32; 8] {
        [fill; 8]
    }

    #[inline]
    fn order() -> [(f32, usize); 8] {
        [(0.0, 0); 8]
    }

    #[inline]
    fn ray_slab(ray: &Ray, t_min: f32) -> RaySlab8 {
        #[cfg(not(feature = "bvh8"))]
        use Vec4 as V;
        #[cfg(feature = "bvh8")]
        use f32x8 as V;
        let o = ray.origin;
        let inv = safe_inv3(ray.dir);
        RaySlab8 {
            ox: V::splat(o.x),
            oy: V::splat(o.y),
            oz: V::splat(o.z),
            ix: V::splat(inv.x),
            iy: V::splat(inv.y),
            iz: V::splat(inv.z),
            t_min: V::splat(t_min),
        }
    }

    /// The 8-lane slab test; same contract as the 4-lane one.
    #[cfg(not(feature = "bvh8"))]
    #[inline]
    fn slab(rs: &RaySlab8, node: &WideNode<W8>, t_max: f32) -> (u32, [f32; 8]) {
        let t_max = Vec4::splat(t_max);
        let (lo, near_lo) = rs.half(node, 0, t_max);
        let (hi, near_hi) = rs.half(node, 1, t_max);
        let mask = (lo | hi << 4) & node.flags & Self::VALID_MASK;
        let mut tn = [0.0; 8];
        near_lo.write_to_slice(&mut tn[..4]);
        near_hi.write_to_slice(&mut tn[4..]);
        (mask, tn)
    }

    /// The 8-lane slab test on one `f32x8`; same contract as the 4-lane one.
    #[cfg(feature = "bvh8")]
    #[inline]
    fn slab(rs: &RaySlab8, node: &WideNode<W8>, t_max: f32) -> (u32, [f32; 8]) {
        let t0x = (node.bmin_x - rs.ox) * rs.ix;
        let t1x = (node.bmax_x - rs.ox) * rs.ix;
        let t0y = (node.bmin_y - rs.oy) * rs.iy;
        let t1y = (node.bmax_y - rs.oy) * rs.iy;
        let t0z = (node.bmin_z - rs.oz) * rs.iz;
        let t1z = (node.bmax_z - rs.oz) * rs.iz;
        let tnear = t0x
            .simd_min(t1x)
            .simd_max(t0y.simd_min(t1y))
            .simd_max(t0z.simd_min(t1z))
            .simd_max(rs.t_min);
        let tfar = t0x
            .simd_max(t1x)
            .simd_min(t0y.simd_max(t1y))
            .simd_min(t0z.simd_max(t1z))
            .simd_min(f32x8::splat(t_max));
        let mask = tnear.simd_le(tfar).to_bitmask() as u32 & node.flags & Self::VALID_MASK;
        (mask, tnear.to_array())
    }
}
