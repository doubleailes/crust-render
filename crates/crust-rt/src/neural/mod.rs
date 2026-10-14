//! Learned shadow-ray occlusion proxies, in the N-BVH style (Weier et al.
//! 2024): an experiment, compiled only with the `neural-bvh` feature.
//!
//! A proxy is built at commit for a scene that asked for one
//! ([`ProxyRequest`]) and qualifies (triangles only, directly or through
//! static instances, no motion, enough triangles). It is a *cut* of the
//! scene's own BVH ([`cut`]) plus one model shared across the cut: a
//! multi-resolution hash grid ([`grid`]) over the scene's box, read at
//! [`POINTS`] points along the segment, and a small MLP ([`mlp`]) that turns
//! those features, the direction and the segment's length into the odds that
//! a surface lies on the segment. `Scene::occluded` with the trained mask
//! traverses exactly down to the cut and asks the model there; every other
//! query, and `intersect` always, stays exact.
//!
//! Training ([`train`]) is deterministic: `openqmc` draws keyed by the
//! geometry, and gradients summed in a fixed chunk order.

pub(crate) mod adam;
pub(crate) mod cut;
pub(crate) mod grid;
pub(crate) mod mlp;
pub(crate) mod train;

use std::collections::HashMap;
use std::time::Instant;

use glam::Vec3A;

use crate::aabb::AABB;
use crate::ray::{MASK_SHADOW, Ray, RayMask};
use crate::scene::Scene;
use cut::CutSet;
use grid::HashGrid;
use mlp::IN;

/// Points read along a segment, at the midpoints of `POINTS` equal strata.
pub(crate) const POINTS: usize = 4;

/// Training steps a request asks for unless it says otherwise.
pub const DEFAULT_STEPS: u32 = 2000;

/// How the cut is chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProxyCut {
    /// The highest lanes whose subtree holds at most 4 096 triangle
    /// references.
    #[default]
    Size,
    /// N-BVH's refinement: start four times coarser, then split the lanes
    /// with the most validation error over three rounds of training.
    Refined,
}

/// A commit's request for an occlusion proxy — see
/// [`crate::CommitOptions::occlusion_proxy`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxyRequest {
    /// Build nothing below this many triangles, counting instanced ones once
    /// per placement.
    pub min_triangles: usize,
    /// The ray mask the proxy is trained for and answers; a query with any
    /// other mask is exact.
    pub mask: RayMask,
    /// Training steps (of 4 096 segments each).
    pub steps: u32,
    pub cut: ProxyCut,
}

impl ProxyRequest {
    /// A shadow-ray proxy with the default training, for scenes of at least
    /// `min_triangles`.
    pub fn shadow(min_triangles: usize) -> Self {
        ProxyRequest {
            min_triangles,
            mask: MASK_SHADOW,
            steps: DEFAULT_STEPS,
            cut: ProxyCut::Size,
        }
    }
}

/// What a built proxy reports about itself.
#[derive(Clone, Debug, Default)]
pub struct ProxyReport {
    /// Triangles the scene holds, counting instanced ones per placement.
    pub triangles: usize,
    /// Lanes in the cut.
    pub cut_lanes: usize,
    /// Training steps taken, refinement rounds included.
    pub steps: u32,
    pub train_seconds: f64,
    /// Held-out validation segments, and the share of them misclassified
    /// each way: a false positive is a spurious shadow, a false negative a
    /// light leak. `contact_false_negative` is the leak rate among segments
    /// that start on a surface.
    pub validation: usize,
    pub false_positive: f32,
    pub false_negative: f32,
    pub contact_false_negative: f32,
    /// Resident bytes: the grid, the MLP and the cut.
    pub bytes: usize,
}

/// The model: the encoding, the MLP's weights, and the frame that maps the
/// scene's box onto the grid's unit cube.
pub(crate) struct Model {
    pub(crate) grid: HashGrid,
    pub(crate) mlp: Vec<f32>,
    origin: Vec3A,
    inv_extent: Vec3A,
}

impl Model {
    pub(crate) fn new(grid: HashGrid, mlp: Vec<f32>, root: AABB) -> Self {
        let e = root.maximum - root.minimum;
        // A flat scene has no extent along one axis: every point maps to 0.
        let inv = Vec3A::select(e.cmpgt(Vec3A::ZERO), e.recip(), Vec3A::ZERO);
        Model {
            grid,
            mlp,
            origin: root.minimum,
            inv_extent: inv,
        }
    }

    /// The MLP input for the segment `o + t·d`, `t ∈ [t0, t1]`, inside a cut
    /// box of diagonal `diag`, and the unit-cube points it read.
    #[inline]
    pub(crate) fn features(
        &self,
        o: Vec3A,
        d: Vec3A,
        t0: f32,
        t1: f32,
        diag: f32,
    ) -> ([f32; IN], [[f32; 3]; POINTS]) {
        let mut x = [0f32; IN];
        let mut pts = [[0f32; 3]; POINTS];
        for (k, p) in pts.iter_mut().enumerate() {
            let t = t0 + (k as f32 + 0.5) / POINTS as f32 * (t1 - t0);
            *p = ((o + t * d - self.origin) * self.inv_extent).to_array();
            self.grid
                .encode(*p, &mut x[k * grid::OUT..(k + 1) * grid::OUT]);
        }
        let len = d.length();
        let base = POINTS * grid::OUT;
        if len > 0.0 {
            x[base..base + 3].copy_from_slice(&(d / len).to_array());
        }
        x[base + 3] = if diag > 0.0 {
            (t1 - t0) * len / diag
        } else {
            0.0
        };
        (x, pts)
    }

    #[inline]
    pub(crate) fn logit(&self, o: Vec3A, d: Vec3A, t0: f32, t1: f32, diag: f32) -> f32 {
        mlp::logit(&self.mlp, &self.features(o, d, t0, t1, diag).0)
    }
}

/// A trained proxy: held by its scene when the commit asked for one, or by a
/// caller beside the scene it was trained for
/// ([`Scene::train_occlusion_proxy`], [`Scene::occluded_by`]) — a probe of
/// prototypes already committed.
pub struct OcclusionProxy {
    pub(crate) model: Model,
    pub(crate) cut: CutSet,
    pub(crate) mask: RayMask,
    pub(crate) report: ProxyReport,
    /// The tree it was trained on, by size: what `occluded_by` checks the
    /// scene it is handed against.
    pub(crate) nodes: usize,
    /// Every distinct ray mask on the geometry the proxy stands for, at any
    /// depth: what [`OcclusionProxy::answers`] compares a query's mask
    /// against.
    pub(crate) inner_masks: Vec<RayMask>,
}

impl OcclusionProxy {
    /// Size, training time and validation error.
    pub fn report(&self) -> &ProxyReport {
        &self.report
    }

    /// The ray mask the proxy was trained for.
    pub fn mask(&self) -> RayMask {
        self.mask
    }

    /// Whether the proxy may answer a query of `mask`: one that carries
    /// every bit of the trained mask, and sees exactly the geometry the
    /// trained mask sees — for each mask inside the scene, both see it or
    /// neither does. A shadow ray of a shadow-linked light carries class bits
    /// on top of the shadow bit, but the classes are written on the
    /// instances that place a prototype, not inside it, so it usually sees
    /// the prototype's geometry exactly as a plain shadow ray does. A camera
    /// ray (no shadow bit), or any mask that would see other geometry here,
    /// stays exact.
    #[inline]
    pub fn answers(&self, mask: RayMask) -> bool {
        mask == self.mask
            || (mask & self.mask == self.mask
                && self
                    .inner_masks
                    .iter()
                    .all(|&m| mask.sees(m) == self.mask.sees(m)))
    }

    /// Whether the model calls the segment `[t0, t1]` of `ray` inside the
    /// cut box `b` occluded at logit threshold `z`.
    #[inline]
    pub(crate) fn occludes(&self, ray: &Ray, t0: f32, t1: f32, b: &AABB, z: f32) -> bool {
        let diag = (b.maximum - b.minimum).length();
        self.model.logit(ray.origin, ray.dir, t0, t1, diag) > z
    }

    pub(crate) fn bytes(&self) -> usize {
        self.model.grid.bytes()
            + std::mem::size_of_val(&*self.model.mlp)
            + self.cut.bytes()
            + std::mem::size_of_val(&*self.inner_masks)
    }
}

/// Triangles the scene holds through every placement, or `None` when it
/// holds anything else, at any depth, or moves.
pub(crate) fn instanced_triangles(scene: &Scene) -> Option<usize> {
    fn walk(s: &Scene, memo: &mut HashMap<usize, Option<usize>>) -> Option<usize> {
        let key = s as *const Scene as usize;
        if let Some(&v) = memo.get(&key) {
            return v;
        }
        let bvh = s.bvh();
        let v = (|| {
            if !bvh.only_triangles_and_instances() {
                return None;
            }
            let mut n = bvh.own_triangle_count();
            for inner in bvh.instance_scenes() {
                n = n.saturating_add(walk(inner, memo)?);
            }
            Some(n)
        })();
        memo.insert(key, v);
        v
    }
    if scene.has_motion() {
        return None;
    }
    walk(scene, &mut HashMap::new())
}

/// Every distinct ray mask on `scene`'s geometry, through every instance.
fn inner_masks(scene: &Scene) -> Vec<RayMask> {
    fn walk(s: &Scene, seen: &mut std::collections::HashSet<usize>, out: &mut Vec<RayMask>) {
        s.bvh().own_masks(out);
        for inner in s.bvh().instance_scenes() {
            if seen.insert(std::sync::Arc::as_ptr(inner) as usize) {
                walk(inner, seen, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(scene, &mut std::collections::HashSet::new(), &mut out);
    out
}

/// The seed of everything random in a proxy: the geometry's own numbers, so
/// the same input trains the same proxy wherever it is committed.
fn seed_of(scene: &Scene, triangles: usize) -> u32 {
    use openqmc::pcg::hash;
    let b = scene.bounds().expect("an eligible scene has bounds");
    let mut h = hash(triangles as u32 ^ hash((triangles >> 32) as u32));
    for v in b.minimum.to_array().into_iter().chain(b.maximum.to_array()) {
        h = hash(h ^ v.to_bits());
    }
    h ^ hash(scene.bvh().node_count() as u32)
}

/// Trains a proxy for `scene`, or `None` when the scene does not qualify.
pub(crate) fn build(scene: &Scene, req: &ProxyRequest) -> Option<OcclusionProxy> {
    let triangles = instanced_triangles(scene)?;
    if triangles == 0 || triangles < req.min_triangles {
        return None;
    }
    let root = scene.bounds()?;
    let start = Instant::now();
    let seed = seed_of(scene, triangles);
    let surfaces = train::Surfaces::of(scene, req.mask, triangles);
    if surfaces.len() == 0 {
        return None;
    }
    let bvh = scene.bvh();
    let refs = |s: &Scene| instanced_triangles(s).unwrap_or(0);
    let mut trainer = train::Trainer::new(Model::init(seed, root));
    let mut steps = 0;
    let cut = match req.cut {
        ProxyCut::Size => CutSet::by_size(bvh, cut::SIZE_CUT_REFS, &refs),
        ProxyCut::Refined => {
            let mut cut = CutSet::by_size(bvh, 4 * cut::SIZE_CUT_REFS, &refs);
            for round in 0..train::ROUNDS {
                let draw = train::Draw::new(scene, &cut, &surfaces, req.mask);
                let pool = draw.pool(seed, train::train_key(round), req.steps / 4);
                let round_steps = req.steps / 4;
                trainer.train(&pool, round_steps);
                steps += round_steps;
                let val = draw.many(seed, train::val_key(round), train::VALIDATION);
                let v = train::validate(&trainer.model, &val, cut.len());
                cut = cut.split(bvh, &train::worst_lanes(&v));
            }
            cut
        }
    };
    let last = match req.cut {
        ProxyCut::Size => 0,
        ProxyCut::Refined => train::ROUNDS,
    };
    let draw = train::Draw::new(scene, &cut, &surfaces, req.mask);
    let pool = draw.pool(seed, train::train_key(last), req.steps);
    trainer.train(&pool, req.steps);
    steps += req.steps;
    let val = draw.many(seed, train::val_key(last), train::VALIDATION);
    let v = train::validate(&trainer.model, &val, cut.len());
    drop(draw);
    let rate = |k: usize, n: usize| if n == 0 { 0.0 } else { k as f32 / n as f32 };
    let cut_lanes = cut.len();
    let mut proxy = OcclusionProxy {
        model: trainer.model,
        cut,
        mask: req.mask,
        nodes: bvh.node_count(),
        inner_masks: inner_masks(scene),
        report: ProxyReport {
            triangles,
            cut_lanes,
            steps,
            train_seconds: 0.0,
            validation: v.n,
            false_positive: rate(v.false_pos, v.n),
            false_negative: rate(v.false_neg, v.n),
            contact_false_negative: rate(v.contact_false_neg, v.contact),
            bytes: 0,
        },
    };
    proxy.report.bytes = proxy.bytes();
    proxy.report.train_seconds = start.elapsed().as_secs_f64();
    Some(proxy)
}

#[cfg(test)]
mod tests;
