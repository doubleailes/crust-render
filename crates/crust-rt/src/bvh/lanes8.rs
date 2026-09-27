//! The `bvh8` node width: 8 lanes on `std::simd::f32x8`. Nightly-only,
//! because `portable_simd` is still unstable (rust-lang/rust#86656); see
//! `docs/simd.md` for why this is the one place 256-bit vectors can pay.
//!
//! `f32x8` is only *one* AVX register when the build enables AVX
//! (`-C target-cpu=x86-64-v3`); on the x86-64 baseline LLVM lowers every
//! operation to two SSE2 halves. That is still correct and still safe
//! code, but it is the wide tree without the wide instructions, so measure
//! with the target level set.

use std::simd::cmp::SimdPartialOrd;
use std::simd::f32x8;
use std::simd::num::SimdFloat;

use crate::ray::Ray;

use super::{WideNode, safe_inv3};

/// Children per wide node.
pub(super) const LANES: usize = 8;

/// One slab-bound coordinate for every lane of a node.
pub(super) type Lanes = f32x8;

#[inline]
pub(super) fn splat(v: f32) -> Lanes {
    f32x8::splat(v)
}

/// The ray, pre-broadcast into eight SoA lanes once per traversal.
pub(super) struct RaySlab {
    ox: f32x8,
    oy: f32x8,
    oz: f32x8,
    ix: f32x8,
    iy: f32x8,
    iz: f32x8,
    t_min: f32x8,
}

impl RaySlab {
    #[inline]
    pub(super) fn new(ray: &Ray, t_min: f32) -> Self {
        let o = ray.origin;
        let inv = safe_inv3(ray.dir);
        RaySlab {
            ox: f32x8::splat(o.x),
            oy: f32x8::splat(o.y),
            oz: f32x8::splat(o.z),
            ix: f32x8::splat(inv.x),
            iy: f32x8::splat(inv.y),
            iz: f32x8::splat(inv.z),
            t_min: f32x8::splat(t_min),
        }
    }

    /// The 8-lane slab test; same contract as the 4-lane one. The lane
    /// arithmetic is the 4-wide version's operation for operation, and
    /// `safe_inv3` keeps it NaN-free, so `simd_min`/`simd_max` (IEEE
    /// minNum/maxNum) and SSE's `minps`/`maxps` agree on every input the
    /// traversal can produce.
    #[inline]
    pub(super) fn slab(&self, node: &WideNode, t_max: f32) -> (u32, [f32; LANES]) {
        let t0x = (node.bmin_x - self.ox) * self.ix;
        let t1x = (node.bmax_x - self.ox) * self.ix;
        let t0y = (node.bmin_y - self.oy) * self.iy;
        let t1y = (node.bmax_y - self.oy) * self.iy;
        let t0z = (node.bmin_z - self.oz) * self.iz;
        let t1z = (node.bmax_z - self.oz) * self.iz;
        let tnear = t0x
            .simd_min(t1x)
            .simd_max(t0y.simd_min(t1y))
            .simd_max(t0z.simd_min(t1z))
            .simd_max(self.t_min);
        let tfar = t0x
            .simd_max(t1x)
            .simd_min(t0y.simd_max(t1y))
            .simd_min(t0z.simd_max(t1z))
            .simd_min(f32x8::splat(t_max));
        let mask = tnear.simd_le(tfar).to_bitmask() as u32 & node.flags & super::VALID_MASK;
        (mask, tnear.to_array())
    }
}
