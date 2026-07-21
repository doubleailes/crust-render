//! Binary spatial tree over the scene bounds whose leaves each own a
//! directional quadtree — the "SD-tree" of Practical Path Guiding. Spatial
//! leaves split at their midpoint along their largest extent once they have
//! collected enough training samples, so the spatial resolution adapts to
//! where paths actually travel.

use super::dtree::DTree;
use crate::aabb::AABB;
use glam::Vec3A;

#[derive(Clone, Debug)]
enum SNode {
    Inner {
        axis: usize,
        split: f32,
        children: [u32; 2],
    },
    Leaf {
        dtree: u32,
        sample_count: u64,
    },
}

#[derive(Clone, Debug)]
pub struct SDTree {
    nodes: Vec<SNode>,
    dtrees: Vec<DTree>,
    bounds: AABB,
}

impl SDTree {
    pub fn new(bounds: AABB) -> Self {
        SDTree {
            nodes: vec![SNode::Leaf {
                dtree: 0,
                sample_count: 0,
            }],
            dtrees: vec![DTree::new()],
            bounds,
        }
    }

    fn leaf_index(&self, p: Vec3A) -> usize {
        let p = p.clamp(self.bounds.minimum, self.bounds.maximum);
        let mut node = 0usize;
        loop {
            match &self.nodes[node] {
                SNode::Inner {
                    axis,
                    split,
                    children,
                } => {
                    node = children[(p[*axis] >= *split) as usize] as usize;
                }
                SNode::Leaf { .. } => return node,
            }
        }
    }

    /// The directional distribution governing position `p`.
    pub fn dtree_at(&self, p: Vec3A) -> &DTree {
        match &self.nodes[self.leaf_index(p)] {
            SNode::Leaf { dtree, .. } => &self.dtrees[*dtree as usize],
            SNode::Inner { .. } => unreachable!("leaf_index returns a leaf"),
        }
    }

    /// Splat one training sample. Every sample counts toward the spatial
    /// split statistic, even if its flux is zero.
    pub fn record(&mut self, p: Vec3A, canonical: [f32; 2], flux: f32) {
        let leaf = self.leaf_index(p);
        if let SNode::Leaf {
            dtree,
            sample_count,
        } = &mut self.nodes[leaf]
        {
            *sample_count += 1;
            self.dtrees[*dtree as usize].record(canonical, flux);
        }
    }

    /// Refine spatial and directional resolution after training iteration
    /// `next_iteration` (1-based): split leaves that exceeded the PPG sample
    /// budget `c·√(2^k)`, then rebuild every directional tree and reset the
    /// per-leaf counters for the next pass.
    pub fn refine(
        &mut self,
        next_iteration: u32,
        spatial_c: f32,
        spatial_max_depth: u32,
        dtree_rho: f32,
        dtree_max_depth: u32,
    ) {
        let threshold =
            (spatial_c as f64 * 2.0f64.powi(next_iteration as i32).sqrt()).max(1.0) as u64;
        self.split_rec(0, self.bounds, 0, threshold, spatial_max_depth);
        for dtree in &mut self.dtrees {
            *dtree = dtree.refine(dtree_rho, dtree_max_depth);
        }
        for node in &mut self.nodes {
            if let SNode::Leaf { sample_count, .. } = node {
                *sample_count = 0;
            }
        }
    }

    fn split_rec(
        &mut self,
        node: usize,
        bounds: AABB,
        depth: u32,
        threshold: u64,
        max_depth: u32,
    ) {
        match self.nodes[node].clone() {
            SNode::Inner {
                axis,
                split,
                children,
            } => {
                let (b0, b1) = split_bounds(bounds, axis, split);
                self.split_rec(children[0] as usize, b0, depth + 1, threshold, max_depth);
                self.split_rec(children[1] as usize, b1, depth + 1, threshold, max_depth);
            }
            SNode::Leaf {
                dtree,
                sample_count,
            } => {
                if sample_count <= threshold || depth >= max_depth {
                    return;
                }
                let extent = bounds.maximum - bounds.minimum;
                let axis = if extent.x >= extent.y && extent.x >= extent.z {
                    0
                } else if extent.y >= extent.z {
                    1
                } else {
                    2
                };
                let split = 0.5 * (bounds.minimum[axis] + bounds.maximum[axis]);

                // Both children start from the parent's distribution and half
                // its sample statistic.
                let cloned = self.dtrees[dtree as usize].clone();
                let dtree1 = self.dtrees.len() as u32;
                self.dtrees.push(cloned);
                let c0 = self.nodes.len() as u32;
                self.nodes.push(SNode::Leaf {
                    dtree,
                    sample_count: sample_count / 2,
                });
                let c1 = self.nodes.len() as u32;
                self.nodes.push(SNode::Leaf {
                    dtree: dtree1,
                    sample_count: sample_count / 2,
                });
                self.nodes[node] = SNode::Inner {
                    axis,
                    split,
                    children: [c0, c1],
                };

                let (b0, b1) = split_bounds(bounds, axis, split);
                self.split_rec(c0 as usize, b0, depth + 1, threshold, max_depth);
                self.split_rec(c1 as usize, b1, depth + 1, threshold, max_depth);
            }
        }
    }

    /// Number of spatial leaves (test/diagnostics helper).
    #[allow(dead_code)]
    pub fn leaf_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n, SNode::Leaf { .. }))
            .count()
    }
}

fn split_bounds(bounds: AABB, axis: usize, split: f32) -> (AABB, AABB) {
    let mut max0 = bounds.maximum;
    max0[axis] = split;
    let mut min1 = bounds.minimum;
    min1[axis] = split;
    (
        AABB::new(bounds.minimum, max0),
        AABB::new(min1, bounds.maximum),
    )
}

#[cfg(test)]
mod tests {
    use super::super::dtree::dir_to_canonical;
    use super::*;

    fn unit_bounds() -> AABB {
        AABB::new(Vec3A::ZERO, Vec3A::ONE)
    }

    #[test]
    fn splits_after_enough_samples() {
        let mut tree = SDTree::new(unit_bounds());
        let canonical = dir_to_canonical(Vec3A::Z);
        for i in 0..5000 {
            let t = (i % 100) as f32 / 100.0;
            tree.record(Vec3A::new(t, 0.5, 0.5), canonical, 1.0);
        }
        assert_eq!(tree.leaf_count(), 1);
        // threshold = 100·√2 ≈ 141 « 5000 ⇒ must split (possibly repeatedly).
        tree.refine(1, 100.0, 24, 0.01, 20);
        assert!(tree.leaf_count() > 1, "leaf did not split");
    }

    #[test]
    fn children_inherit_parent_distribution() {
        let mut tree = SDTree::new(unit_bounds());
        let canonical = dir_to_canonical(Vec3A::Z);
        for i in 0..2000 {
            let t = (i % 100) as f32 / 100.0;
            tree.record(Vec3A::new(t, 0.5, 0.5), canonical, 1.0);
        }
        tree.refine(1, 100.0, 24, 0.01, 20);
        // Every region should still sample toward +z after the split.
        for p in [Vec3A::new(0.1, 0.5, 0.5), Vec3A::new(0.9, 0.5, 0.5)] {
            let dtree = tree.dtree_at(p);
            assert!(dtree.total_flux() > 0.0, "child at {p} lost its flux");
            let (c, _) = dtree.sample([0.4, 0.6]).unwrap();
            let d = super::super::dtree::canonical_to_dir(c);
            assert!(d.z > 0.0, "child at {p} samples away from the light: {d}");
        }
    }

    #[test]
    fn no_split_below_threshold() {
        let mut tree = SDTree::new(unit_bounds());
        let canonical = dir_to_canonical(Vec3A::Z);
        for _ in 0..50 {
            tree.record(Vec3A::splat(0.5), canonical, 1.0);
        }
        tree.refine(1, 4000.0, 24, 0.01, 20);
        assert_eq!(tree.leaf_count(), 1);
    }
}
