//! Public facade over the SD-tree, mirroring OpenPGL's `Field` /
//! `SampleStorage` split: the renderer records [`SampleData`] during a
//! training pass, calls [`GuidingField::update`] between passes, and queries
//! [`GuidingField::sample`] / [`GuidingField::pdf`] while tracing.

use super::dtree::{canonical_to_dir, dir_to_canonical};
use super::sdtree::SDTree;
use crate::aabb::AABB;
use glam::Vec3A;
use sampler::Sampler;

/// Tuning knobs for the guiding field. Defaults follow the PPG paper, with a
/// smaller spatial split constant suited to small scenes.
#[derive(Debug, Clone, Copy)]
pub struct GuidingConfig {
    /// Number of training passes before the final render pass.
    pub train_iterations: u32,
    /// Probability α of drawing the bounce direction from the guiding
    /// distribution instead of the BSDF (one-sample MIS mixture weight).
    pub guide_prob: f32,
    /// Spatial split constant `c` in the PPG leaf budget `c·√(2^k)`.
    pub spatial_c: f32,
    /// Directional subdivision threshold as a fraction of total flux.
    pub dtree_rho: f32,
    /// Maximum directional quadtree depth.
    pub dtree_max_depth: u32,
    /// Maximum spatial binary-tree depth.
    pub spatial_max_depth: u32,
}

impl Default for GuidingConfig {
    fn default() -> Self {
        GuidingConfig {
            train_iterations: 4,
            guide_prob: 0.5,
            spatial_c: 4000.0,
            dtree_rho: 0.01,
            dtree_max_depth: 20,
            spatial_max_depth: 24,
        }
    }
}

/// One radiance sample recorded at a path vertex: from `pos`, looking along
/// `dir`, the path estimated `radiance` incident luminance.
#[derive(Debug, Clone, Copy)]
pub struct SampleData {
    pub pos: Vec3A,
    pub dir: Vec3A,
    pub radiance: f32,
}

/// A learned spatio-directional distribution of incident radiance.
///
/// Plain owned data: shared immutably (`&self`) across render threads during
/// a pass, updated exclusively (`&mut self`) between passes.
#[derive(Debug, Clone)]
pub struct GuidingField {
    tree: SDTree,
    cfg: GuidingConfig,
}

impl GuidingField {
    pub fn new(bounds: AABB, cfg: GuidingConfig) -> Self {
        // Pad the bounds slightly so hit points that land exactly on the
        // scene hull stay strictly inside.
        let pad = ((bounds.maximum - bounds.minimum).max_element() * 1e-3).max(1e-3);
        let bounds = AABB::new(bounds.minimum - Vec3A::splat(pad), bounds.maximum + Vec3A::splat(pad));
        GuidingField {
            tree: SDTree::new(bounds),
            cfg,
        }
    }

    pub fn config(&self) -> &GuidingConfig {
        &self.cfg
    }

    /// Whether the distribution at `pos` has been trained. The integrator
    /// must use the mixture pdf iff this holds, regardless of which strategy
    /// the α-coin picks.
    pub fn trained_at(&self, pos: Vec3A) -> bool {
        self.tree.dtree_at(pos).total_flux() > 0.0
    }

    /// Draw a world-space direction from the local guiding distribution with
    /// its solid-angle pdf. `None` while the local distribution is untrained.
    pub fn sample(&self, pos: Vec3A, sampler: &mut dyn Sampler) -> Option<(Vec3A, f32)> {
        let (canonical, pdf) = self.tree.dtree_at(pos).sample(sampler)?;
        Some((canonical_to_dir(canonical), pdf))
    }

    /// Solid-angle pdf the guiding distribution at `pos` assigns to `dir`.
    /// Exact density — 0 where the distribution holds no flux.
    pub fn pdf(&self, pos: Vec3A, dir: Vec3A) -> f32 {
        self.tree.dtree_at(pos).pdf(dir_to_canonical(dir))
    }

    /// Splat a training pass's samples and adapt the tree resolution.
    /// `next_iteration` is the 1-based index of the pass about to start.
    pub fn update(&mut self, samples: &[SampleData], next_iteration: u32) {
        for s in samples {
            self.tree.record(s.pos, dir_to_canonical(s.dir), s.radiance);
        }
        self.tree.refine(
            next_iteration,
            self.cfg.spatial_c,
            self.cfg.spatial_max_depth,
            self.cfg.dtree_rho,
            self.cfg.dtree_max_depth,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_trains_and_samples() {
        let bounds = AABB::new(Vec3A::ZERO, Vec3A::ONE);
        let mut field = GuidingField::new(bounds, GuidingConfig::default());
        let mut s = sampler::RngSampler::default();
        assert!(!field.trained_at(Vec3A::splat(0.5)));
        assert!(field.sample(Vec3A::splat(0.5), &mut s).is_none());

        let samples: Vec<SampleData> = (0..1000)
            .map(|i| SampleData {
                pos: Vec3A::splat((i % 10) as f32 / 10.0),
                dir: Vec3A::Z,
                radiance: 1.0,
            })
            .collect();
        field.update(&samples, 1);

        assert!(field.trained_at(Vec3A::splat(0.5)));
        let (dir, pdf) = field.sample(Vec3A::splat(0.5), &mut s).unwrap();
        assert!(pdf > 0.0);
        assert!(dir.z > 0.0, "trained on +z but sampled {dir}");
        assert!(field.pdf(Vec3A::splat(0.5), dir) > 0.0);
    }
}
