//! What a node format has to offer: the collapse fills nodes through
//! [`BuildNode`], the traversal walks them through [`NodeFormat`].
//!
//! Both are generic over the node's width `W` as a const parameter, so the
//! traversal's per-node scratch (`[_; W]`) is sized at compile time and each
//! format gets its own monomorphised copy of the loop: choosing a format per
//! tree costs one `match` per tree descent, never a branch per node.

use crate::aabb::AABB;
use crate::ray::Ray;

/// Marks an unused lane of a node.
pub(super) const EMPTY_LANE: u32 = u32::MAX;

/// A node the collapse can fill: one child at a time, in lane order.
pub(super) trait BuildNode<const W: usize>: Sized {
    /// A node with no children.
    fn empty() -> Self;
    /// Lane `lane` now holds a child with these bounds.
    fn set_lane_bounds(&mut self, lane: usize, b: &AABB);
    /// Lane `lane`'s child index: a node for an internal child, a leaf entry
    /// for a leaf (after [`BuildNode::mark_leaf`]).
    fn set_child(&mut self, lane: usize, child: u32);
    /// Lane `lane`'s child is a leaf.
    fn mark_leaf(&mut self, lane: usize);
}

/// A node the traversal can walk: a ray test against all `W` children at
/// once, and the child each lane stands for.
pub(super) trait NodeFormat<const W: usize>: Sized {
    /// The ray, prepared once per traversal in whatever form the slab test
    /// wants.
    type Ray;
    fn prepare(ray: &Ray, t_min: f32) -> Self::Ray;
    /// Bit `l` of the mask is set iff lane `l` holds a real child whose box
    /// the ray crosses in `(t_min, t_max)`; the array holds each lane's
    /// entry distance.
    fn slab(ray: &Self::Ray, node: &Self, t_max: f32) -> (u32, [f32; W]);
    fn is_leaf(&self, lane: usize) -> bool;
    /// An internal lane's node index, or a leaf lane's index into `leaves`.
    fn child_at(&self, lane: usize) -> u32;
}

/// Declares a plain SoA node of `f32` child bounds, one child index per lane
/// and the flag bits, with the [`BuildNode`] impl. The lane vector type
/// differs per format (`Vec4`, `f32x8`, `[f32; 8]`); the layout and the
/// flag encoding do not:
/// bits `0..W` of `flags` say lane `k` holds a real child, bits `W..2W` say
/// that child is a leaf.
///
/// The validity bits exist because unused lanes carry +INF/+INF bounds,
/// which *usually* fail the slab test but not always: for `t_max == INF` and
/// an all-positive ray direction the test reduces to `INF <= INF`. Traversal
/// ANDs them into the hit mask, one integer `and` in place of per-lane
/// branches.
macro_rules! f32_node {
    ($(#[$meta:meta])* $name:ident, $lanes:ty, $w:expr, $splat:expr) => {
        $(#[$meta])*
        pub(super) struct $name {
            pub(super) bmin_x: $lanes,
            pub(super) bmin_y: $lanes,
            pub(super) bmin_z: $lanes,
            pub(super) bmax_x: $lanes,
            pub(super) bmax_y: $lanes,
            pub(super) bmax_z: $lanes,
            /// Leaf lane: index into the BVH's `leaves`. Internal lane:
            /// index into the node array. Unused lane: `EMPTY_LANE`.
            pub(super) child: [u32; $w],
            pub(super) flags: u32,
        }

        impl $name {
            pub(super) const VALID_MASK: u32 = (1 << $w) - 1;
            pub(super) const LEAF_SHIFT: u32 = $w;

            #[inline]
            pub(super) fn leaf_bit(&self, lane: usize) -> bool {
                self.flags & (1 << (Self::LEAF_SHIFT + lane as u32)) != 0
            }
        }

        impl $crate::bvh::node::BuildNode<$w> for $name {
            fn empty() -> Self {
                let inf = $splat(f32::INFINITY);
                $name {
                    bmin_x: inf,
                    bmin_y: inf,
                    bmin_z: inf,
                    bmax_x: inf,
                    bmax_y: inf,
                    bmax_z: inf,
                    child: [$crate::bvh::node::EMPTY_LANE; $w],
                    flags: 0,
                }
            }

            fn set_lane_bounds(&mut self, lane: usize, b: &$crate::aabb::AABB) {
                self.bmin_x[lane] = b.minimum.x;
                self.bmin_y[lane] = b.minimum.y;
                self.bmin_z[lane] = b.minimum.z;
                self.bmax_x[lane] = b.maximum.x;
                self.bmax_y[lane] = b.maximum.y;
                self.bmax_z[lane] = b.maximum.z;
                self.flags |= 1 << lane;
            }

            fn set_child(&mut self, lane: usize, child: u32) {
                self.child[lane] = child;
            }

            fn mark_leaf(&mut self, lane: usize) {
                self.flags |= 1 << (Self::LEAF_SHIFT + lane as u32);
            }
        }
    };
}
pub(super) use f32_node;
