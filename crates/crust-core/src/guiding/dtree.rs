//! Directional quadtree over the unit sphere, after Müller et al. 2017,
//! "Practical Path Guiding for Efficient Light-Transport Simulation".
//!
//! Directions are mapped to the canonical unit square with a cylindrical
//! equal-area parameterization, so the map has a constant Jacobian and a
//! canonical-space density converts to a solid-angle density by a plain
//! `1 / (4π)` factor. The tree stores radiance flux per quadrant at every
//! level; sampling proportional to flux and pdf evaluation both descend the
//! same path, which keeps them exactly consistent.

use glam::Vec3A;
use sampler::Sampler;
use std::f32::consts::{PI, TAU};

const NO_CHILD: u32 = u32::MAX;
const ONE_MINUS_EPS: f32 = 1.0 - f32::EPSILON;

/// PCG32 step producing a uniform f32 in [0, 1).
#[inline]
fn pcg_f32(state: &mut u64) -> f32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let xorshifted = (((*state >> 18) ^ *state) >> 27) as u32;
    let rot = (*state >> 59) as u32;
    let bits = xorshifted.rotate_right(rot);
    (bits >> 8) as f32 * (1.0 / (1u32 << 24) as f32)
}

/// Cylindrical equal-area map from a unit direction to the canonical square.
pub fn dir_to_canonical(d: Vec3A) -> [f32; 2] {
    let d = d.normalize();
    let u = d.y.atan2(d.x) / TAU + 0.5;
    let v = (d.z + 1.0) * 0.5;
    [u.clamp(0.0, ONE_MINUS_EPS), v.clamp(0.0, ONE_MINUS_EPS)]
}

/// Inverse of [`dir_to_canonical`].
pub fn canonical_to_dir(p: [f32; 2]) -> Vec3A {
    let phi = TAU * (p[0] - 0.5);
    let z = 2.0 * p[1] - 1.0;
    let r = (1.0 - z * z).max(0.0).sqrt();
    Vec3A::new(r * phi.cos(), r * phi.sin(), z)
}

/// Quadrant of a canonical point and the point rescaled into that quadrant's
/// own canonical square. Quadrant index is `qx + 2*qy`.
#[inline]
fn quadrant(p: [f32; 2]) -> (usize, [f32; 2]) {
    let qx = (p[0] >= 0.5) as usize;
    let qy = (p[1] >= 0.5) as usize;
    let child = [
        (p[0] * 2.0 - qx as f32).clamp(0.0, ONE_MINUS_EPS),
        (p[1] * 2.0 - qy as f32).clamp(0.0, ONE_MINUS_EPS),
    ];
    (qx + 2 * qy, child)
}

#[derive(Clone, Debug)]
struct DNode {
    /// Flux accumulated per quadrant. Plain `f32` — splatting happens
    /// single-threaded between render passes.
    sums: [f32; 4],
    /// Child node index per quadrant; `NO_CHILD` marks a leaf quadrant.
    children: [u32; 4],
}

impl DNode {
    fn leaf() -> Self {
        DNode {
            sums: [0.0; 4],
            children: [NO_CHILD; 4],
        }
    }

    #[inline]
    fn total(&self) -> f32 {
        self.sums[0] + self.sums[1] + self.sums[2] + self.sums[3]
    }
}

/// Adaptive quadtree distribution over the canonical square.
#[derive(Clone, Debug)]
pub struct DTree {
    nodes: Vec<DNode>,
}

impl Default for DTree {
    fn default() -> Self {
        Self::new()
    }
}

impl DTree {
    pub fn new() -> Self {
        DTree {
            nodes: vec![DNode::leaf()],
        }
    }

    pub fn total_flux(&self) -> f32 {
        self.nodes[0].total()
    }

    /// Splat `flux` at the canonical position, accumulating into the crossed
    /// quadrant at every level down the tree.
    pub fn record(&mut self, mut p: [f32; 2], flux: f32) {
        if !flux.is_finite() || flux <= 0.0 {
            return;
        }
        let mut node = 0usize;
        loop {
            let (q, child_p) = quadrant(p);
            self.nodes[node].sums[q] += flux;
            let c = self.nodes[node].children[q];
            if c == NO_CHILD {
                return;
            }
            node = c as usize;
            p = child_p;
        }
    }

    /// Solid-angle pdf of the direction mapping to canonical `p`. Returns the
    /// exact density — 0 for an untrained tree or a zero-flux region.
    pub fn pdf(&self, mut p: [f32; 2]) -> f32 {
        if self.total_flux() <= 0.0 {
            return 0.0;
        }
        let mut node = 0usize;
        let mut pdf = 1.0 / (4.0 * PI);
        loop {
            let n = &self.nodes[node];
            let total = n.total();
            if total <= 0.0 {
                // No information below this node: uniform over its domain.
                return pdf;
            }
            let (q, child_p) = quadrant(p);
            let frac = n.sums[q] / total;
            if frac <= 0.0 {
                return 0.0;
            }
            pdf *= 4.0 * frac;
            let c = n.children[q];
            if c == NO_CHILD {
                return pdf;
            }
            node = c as usize;
            p = child_p;
        }
    }

    /// Draw a canonical position proportional to the stored flux, returning
    /// it with its solid-angle pdf. `None` if the tree holds no flux yet.
    ///
    /// Exactly one 2D draw is consumed from `sampler`; it seeds a PCG stream
    /// that supplies fresh uniforms per tree level. Rescaling a single 2D
    /// sample down the tree (the textbook trick) loses entropy on sharp
    /// distributions until deep cells are no longer sampled uniformly, and
    /// drawing per-level from the QMC sampler burns through its dimension
    /// window; the hashed stream avoids both while keeping the sampler's
    /// dimension usage fixed.
    pub fn sample(&self, sampler: &mut dyn Sampler) -> Option<([f32; 2], f32)> {
        if self.total_flux() <= 0.0 {
            return None;
        }
        let seed = sampler.next_2d();
        let mut rng_state: u64 = ((seed[0].to_bits() as u64) << 32 | seed[1].to_bits() as u64)
            ^ 0x9E37_79B9_7F4A_7C15;
        let mut node = 0usize;
        let mut base = [0.0f32; 2];
        let mut scale = 1.0f32;
        let mut pdf = 1.0 / (4.0 * PI);
        loop {
            let n = &self.nodes[node];
            let total = n.total();
            if total <= 0.0 {
                // No information below this node: uniform within its domain.
                break;
            }
            let u = [pcg_f32(&mut rng_state), pcg_f32(&mut rng_state)];

            // Pick the column proportional to column flux.
            let p_left = (n.sums[0] + n.sums[2]) / total;
            let qx = (u[0] >= p_left) as usize;

            // Pick the row within the column.
            let col_total = n.sums[qx] + n.sums[qx + 2];
            let p_bottom = if col_total > 0.0 {
                n.sums[qx] / col_total
            } else {
                0.5
            };
            let qy = (u[1] >= p_bottom) as usize;

            let q = qx + 2 * qy;
            let frac = n.sums[q] / total;
            if frac <= 0.0 {
                // Numerically unreachable quadrant; treat as uninformative.
                break;
            }
            pdf *= 4.0 * frac;
            base[0] += 0.5 * scale * qx as f32;
            base[1] += 0.5 * scale * qy as f32;
            scale *= 0.5;

            let c = n.children[q];
            if c == NO_CHILD {
                break;
            }
            node = c as usize;
        }
        // Uniform position within the reached cell.
        let u = [pcg_f32(&mut rng_state), pcg_f32(&mut rng_state)];
        let p = [
            (base[0] + u[0] * scale).clamp(0.0, ONE_MINUS_EPS),
            (base[1] + u[1] * scale).clamp(0.0, ONE_MINUS_EPS),
        ];
        Some((p, pdf))
    }

    /// Rebuild the tree structure from the current flux: subdivide any
    /// quadrant holding more than `rho` of the total flux (down to
    /// `max_depth` levels), collapse quadrants below the threshold. Newly
    /// created children are seeded with a quarter of their parent quadrant's
    /// flux so the refined tree remains a valid sampling distribution.
    pub fn refine(&self, rho: f32, max_depth: u32) -> DTree {
        let mut out = DTree::new();
        let total = self.total_flux();
        if total <= 0.0 {
            return out;
        }
        self.refine_rec(&mut out.nodes, 0, Some(0), self.nodes[0].sums, total, rho, 1, max_depth);
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn refine_rec(
        &self,
        out: &mut Vec<DNode>,
        out_idx: usize,
        old_idx: Option<usize>,
        seeded_sums: [f32; 4],
        total: f32,
        rho: f32,
        depth: u32,
        max_depth: u32,
    ) {
        let sums = match old_idx {
            Some(i) => self.nodes[i].sums,
            None => seeded_sums,
        };
        out[out_idx].sums = sums;
        for q in 0..4 {
            let flux = sums[q];
            if flux > rho * total && depth < max_depth {
                let child_idx = out.len();
                out.push(DNode::leaf());
                out[out_idx].children[q] = child_idx as u32;
                let old_child = old_idx.and_then(|i| {
                    let c = self.nodes[i].children[q];
                    (c != NO_CHILD).then_some(c as usize)
                });
                self.refine_rec(
                    out,
                    child_idx,
                    old_child,
                    [flux / 4.0; 4],
                    total,
                    rho,
                    depth + 1,
                    max_depth,
                );
            }
        }
    }

    /// Maximum depth of the tree (a single root node has depth 1).
    /// Diagnostics/tests helper.
    #[allow(dead_code)]
    pub fn depth(&self) -> u32 {
        fn rec(nodes: &[DNode], idx: usize) -> u32 {
            let mut d = 1;
            for &c in &nodes[idx].children {
                if c != NO_CHILD {
                    d = d.max(1 + rec(nodes, c as usize));
                }
            }
            d
        }
        rec(&self.nodes, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sampler::{RngSampler, Sampler};
    use utils::uniform_sphere;

    #[test]
    fn canonical_round_trip() {
        let mut s = RngSampler::default();
        for _ in 0..1000 {
            let d = uniform_sphere(s.next_2d()).normalize();
            let back = canonical_to_dir(dir_to_canonical(d));
            assert!((back - d).length() < 1e-3, "{d} -> {back}");
        }
    }

    #[test]
    fn canonical_map_is_equal_area() {
        // Uniform canonical points must map to uniform sphere directions:
        // E[z] ≈ 0 and E[z²] ≈ 1/3.
        let mut s = RngSampler::default();
        let n = 200_000;
        let (mut mean_z, mut mean_z2) = (0.0f64, 0.0f64);
        for _ in 0..n {
            let u = s.next_2d();
            let d = canonical_to_dir([u[0], u[1]]);
            mean_z += d.z as f64;
            mean_z2 += (d.z * d.z) as f64;
        }
        mean_z /= n as f64;
        mean_z2 /= n as f64;
        assert!(mean_z.abs() < 0.01, "mean z = {mean_z}");
        assert!((mean_z2 - 1.0 / 3.0).abs() < 0.01, "mean z² = {mean_z2}");
    }

    /// Build a tree trained on an anisotropic distribution: all flux inside
    /// one octant of the sphere.
    fn trained_tree() -> DTree {
        let mut s = RngSampler::default();
        let mut tree = DTree::new();
        for round in 0..4 {
            for _ in 0..20_000 {
                let d = uniform_sphere(s.next_2d()).abs().normalize();
                tree.record(dir_to_canonical(d), 1.0);
            }
            if round < 3 {
                tree = tree.refine(0.01, 20);
            }
        }
        tree
    }

    #[test]
    fn pdf_integrates_to_one() {
        let tree = trained_tree();
        let mut s = RngSampler::default();
        let n = 200_000;
        let mut acc = 0.0f64;
        for _ in 0..n {
            let d = uniform_sphere(s.next_2d());
            acc += tree.pdf(dir_to_canonical(d)) as f64;
        }
        // Uniform-sphere MC integral of the pdf: 4π · E[pdf] ≈ 1.
        let integral = 4.0 * std::f64::consts::PI * acc / n as f64;
        assert!((integral - 1.0).abs() < 0.02, "∫pdf = {integral}");
    }

    #[test]
    fn sample_pdf_matches_pdf_lookup() {
        let tree = trained_tree();
        let mut s = RngSampler::default();
        for _ in 0..1000 {
            let (p, pdf) = tree.sample(&mut s).expect("trained tree samples");
            let lookup = tree.pdf(p);
            assert!(
                (pdf - lookup).abs() < 1e-3 * (1.0 + pdf),
                "sample pdf {pdf} vs lookup {lookup}"
            );
        }
    }

    #[test]
    fn samples_follow_flux() {
        // All flux in the +z hemisphere ⇒ samples must land there.
        let mut tree = DTree::new();
        let mut s = RngSampler::default();
        for _ in 0..10_000 {
            let mut d = uniform_sphere(s.next_2d());
            d.z = d.z.abs().max(0.1);
            tree.record(dir_to_canonical(d.normalize()), 1.0);
        }
        let tree = tree.refine(0.01, 20);
        for _ in 0..1000 {
            let (p, _) = tree.sample(&mut s).unwrap();
            let d = canonical_to_dir(p);
            assert!(d.z > -1e-3, "sampled below the trained hemisphere: {d}");
        }
    }

    #[test]
    fn sampled_histogram_matches_pdf_on_sharp_tree() {
        // Train a sharply concentrated distribution over many rounds — the
        // regime where sample/pdf inconsistencies (e.g. degraded sample
        // entropy at deep levels) show up as estimator bias.
        let mut s = RngSampler::default();
        let mut tree = DTree::new();
        for round in 0..8 {
            for _ in 0..50_000 {
                // A tight cone around +z plus a dim uniform background.
                let u = s.next_2d();
                let d = if u[0] < 0.9 {
                    let v = s.next_2d();
                    Vec3A::new(0.05 * (v[0] - 0.5), 0.05 * (v[1] - 0.5), 1.0).normalize()
                } else {
                    uniform_sphere(s.next_2d())
                };
                tree.record(dir_to_canonical(d), 1.0);
            }
            if round < 7 {
                tree = tree.refine(0.01, 20);
            }
        }
        let tree = tree.refine(0.01, 20);

        // Empirical mass per octant must match the pdf-integrated mass.
        let n = 400_000;
        let mut counts = [0u64; 8];
        for _ in 0..n {
            let (p, _) = tree.sample(&mut s).unwrap();
            let d = canonical_to_dir(p);
            let oct = (d.x >= 0.0) as usize + 2 * ((d.y >= 0.0) as usize)
                + 4 * ((d.z >= 0.0) as usize);
            counts[oct] += 1;
        }
        let m = 400_000;
        let mut integrals = [0.0f64; 8];
        for _ in 0..m {
            let d = uniform_sphere(s.next_2d());
            let oct = (d.x >= 0.0) as usize + 2 * ((d.y >= 0.0) as usize)
                + 4 * ((d.z >= 0.0) as usize);
            integrals[oct] += tree.pdf(dir_to_canonical(d)) as f64;
        }
        for oct in 0..8 {
            let freq = counts[oct] as f64 / n as f64;
            let mass = 4.0 * std::f64::consts::PI * integrals[oct] / m as f64;
            assert!(
                (freq - mass).abs() < 0.02,
                "octant {oct}: sampled {freq:.4} vs pdf mass {mass:.4}"
            );
        }
    }

    #[test]
    fn refinement_grows_hot_regions() {
        let tree = trained_tree();
        assert!(tree.depth() > 2, "hot octant should subdivide, depth = {}", tree.depth());
    }

    #[test]
    fn untrained_tree_declines_to_sample() {
        let tree = DTree::new();
        let mut s = RngSampler::default();
        assert!(tree.sample(&mut s).is_none());
        assert_eq!(tree.pdf([0.3, 0.7]), 0.0);
    }
}
