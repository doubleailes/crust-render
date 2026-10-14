//! Training data and the deterministic trainer.
//!
//! Segments come from three families, each drawn from its own keyed
//! `openqmc` stream so sample `i` is the same whatever thread draws it:
//!
//! 1. surface-origin rays (50 %): an area-weighted point on a triangle (through
//!    instances), a uniform direction, offset by the renderer's `TRACE_T_MIN`,
//!    clipped to the nearest cut box it crosses: the contact query;
//! 2. through rays (35 %): two uniform points on the faces of a random cut box;
//! 3. short segments (15 %): family 2 truncated at a uniform fraction.
//!
//! Every segment is labelled by the scene's exact `hit_any` with the trained
//! mask. Each step splits its batch into [`CHUNKS`] fixed chunks; rayon
//! computes each chunk's gradients, and they are summed in chunk order on one
//! thread, which is the whole of the determinism contract (float addition is
//! not associative, so the order is fixed, not the thread count).

use std::collections::HashMap;

use glam::Vec3A;
use openqmc::pcg::{self, Rng};
use rayon::prelude::*;

use super::adam::Adam;
use super::cut::CutSet;
use super::grid::{self, HashGrid};
use super::mlp::{self, IN};
use super::{Model, POINTS};
use crate::aabb::AABB;
use crate::ray::{Ray, RayMask};
use crate::scene::{Piece, Scene};

/// Segments per step.
pub(crate) const BATCH: usize = 4096;
/// Fixed chunks per batch: the unit of parallel work and of the ordered sum.
pub(crate) const CHUNKS: usize = 64;
/// Training segments drawn per cut (64 batches; steps cycle through them).
pub(crate) const POOL: usize = 1 << 18;
/// Held-out validation segments per cut.
pub(crate) const VALIDATION: usize = 1 << 14;
/// Most triangles the surface-origin family samples from.
const MAX_SURFACES: usize = 1 << 22;
/// Instant NGP's learning-rate split.
const LR_GRID: f32 = 1e-2;
const LR_MLP: f32 = 1e-3;
/// The renderer's `crust_core::TRACE_T_MIN`: where a shadow ray from a
/// shading point starts.
pub(crate) const SURFACE_T_MIN: f32 = 1e-3;
/// Refinement rounds of the error-driven cut, and the share of lanes each
/// round splits.
const REFINE_ROUNDS: u32 = 3;
const REFINE_SHARE: f32 = 0.25;

const K_INIT: u32 = 0x6e62_7631; // "nbv1"
const K_TRAIN: u32 = 0x7472_6169;
const K_VAL: u32 = 0x7661_6c69;

/// One labelled segment: a ray, its interval inside one cut box, and that
/// box's diagonal.
#[derive(Clone, Copy)]
pub(crate) struct Segment {
    pub(crate) o: Vec3A,
    pub(crate) d: Vec3A,
    pub(crate) t0: f32,
    pub(crate) t1: f32,
    pub(crate) diag: f32,
    pub(crate) cut: u32,
    pub(crate) label: bool,
    /// Family 1: starts on a surface.
    pub(crate) contact: bool,
}

/// What the generator samples from: the scene's triangles in its own space,
/// with an area CDF.
pub(crate) struct Surfaces {
    tris: Vec<[[f32; 3]; 3]>,
    cdf: Vec<f64>,
}

impl Surfaces {
    /// The triangles of `scene` a ray of `mask` sees, through every
    /// placement — every `stride`-th of them past [`MAX_SURFACES`], so a
    /// field of millions of placed prototypes samples a fixed budget rather
    /// than copying out billions of triangles.
    pub(crate) fn of(scene: &Scene, mask: RayMask, triangles: usize) -> Self {
        let stride = triangles.div_ceil(MAX_SURFACES).max(1);
        let mut tris = Vec::with_capacity(triangles.min(MAX_SURFACES));
        let mut k = 0usize;
        scene.visit(0..u32::MAX, mask, |_, piece| {
            if let Piece::Triangle(t) = piece {
                if k.is_multiple_of(stride) {
                    tris.push([t[0].to_array(), t[1].to_array(), t[2].to_array()]);
                }
                k += 1;
            }
        });
        let mut acc = 0f64;
        let cdf = tris
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(Vec3A::from_array);
                acc += 0.5 * (b - a).cross(c - a).length() as f64;
                acc
            })
            .collect();
        Surfaces { tris, cdf }
    }

    /// An area-uniform point, or `None` with no area at all.
    fn point(&self, rng: &mut Rng) -> Option<Vec3A> {
        let total = *self.cdf.last()?;
        if total <= 0.0 {
            return None;
        }
        let u = rng.next_f32() as f64 * total;
        let i = self
            .cdf
            .partition_point(|&c| c <= u)
            .min(self.tris.len() - 1);
        let [a, b, c] = self.tris[i].map(Vec3A::from_array);
        let (mut s, mut t) = (rng.next_f32(), rng.next_f32());
        if s + t > 1.0 {
            (s, t) = (1.0 - s, 1.0 - t);
        }
        Some(a + s * (b - a) + t * (c - a))
    }

    pub(crate) fn len(&self) -> usize {
        self.tris.len()
    }
}

fn keyed(seed: u32, key: u32, i: u32) -> Rng {
    Rng::new(pcg::hash(seed ^ pcg::hash(key ^ pcg::hash(i))))
}

fn sphere(rng: &mut Rng) -> Vec3A {
    let z = 1.0 - 2.0 * rng.next_f32();
    let r = (1.0 - z * z).max(0.0).sqrt();
    let phi = std::f32::consts::TAU * rng.next_f32();
    Vec3A::new(r * phi.cos(), r * phi.sin(), z)
}

/// A uniform point on the surface of `b`.
fn on_box(b: &AABB, rng: &mut Rng) -> Vec3A {
    let e = b.maximum - b.minimum;
    let areas = [e.y * e.z, e.x * e.z, e.x * e.y];
    let total = areas[0] + areas[1] + areas[2];
    let mut u = rng.next_f32() * total;
    let mut axis = 2;
    for (a, &area) in areas.iter().enumerate() {
        if u < area {
            axis = a;
            break;
        }
        u -= area;
    }
    let mut p = b.minimum + e * Vec3A::new(rng.next_f32(), rng.next_f32(), rng.next_f32());
    p[axis] = if rng.next_f32() < 0.5 {
        b.minimum[axis]
    } else {
        b.maximum[axis]
    };
    p
}

/// The context one cut's segments are drawn in.
pub(crate) struct Draw<'a> {
    pub(crate) scene: &'a Scene,
    pub(crate) cut: &'a CutSet,
    pub(crate) boxes: Vec<AABB>,
    index: HashMap<(u32, u8), u32>,
    pub(crate) surfaces: &'a Surfaces,
    pub(crate) mask: RayMask,
    root_diag: f32,
}

impl<'a> Draw<'a> {
    pub(crate) fn new(
        scene: &'a Scene,
        cut: &'a CutSet,
        surfaces: &'a Surfaces,
        mask: RayMask,
    ) -> Self {
        let bvh = scene.bvh();
        let boxes = cut
            .lanes()
            .iter()
            .map(|&(n, l)| bvh.lane(n, l as usize).expect("cut lanes are valid").0)
            .collect();
        let index = cut
            .lanes()
            .iter()
            .enumerate()
            .map(|(i, &k)| (k, i as u32))
            .collect();
        let root = scene.bounds().expect("an eligible scene has bounds");
        Draw {
            scene,
            cut,
            boxes,
            index,
            surfaces,
            mask,
            root_diag: (root.maximum - root.minimum).length(),
        }
    }

    /// Segment `i` of the stream `key`.
    pub(crate) fn segment(&self, seed: u32, key: u32, i: u32) -> Segment {
        let mut rng = keyed(seed, key, i);
        let family = rng.next_f32();
        for _ in 0..64 {
            let s = if family < 0.5 {
                self.surface_origin(&mut rng)
            } else {
                self.through(&mut rng, family >= 0.85)
            };
            if let Some(mut s) = s {
                let ray = Ray::new(s.o, s.d).with_mask(self.mask);
                // Exact at every depth: an instance that holds a proxy of
                // its own must not teach this one its approximation.
                s.label =
                    crate::scene::exact_occlusion(|| self.scene.bvh().hit_any(&ray, s.t0, s.t1));
                return s;
            }
        }
        // Unreachable on an eligible scene: every family has positive
        // probability of a valid segment. A miss is an empty segment.
        Segment {
            o: Vec3A::ZERO,
            d: Vec3A::X,
            t0: 0.0,
            t1: 0.0,
            diag: 1.0,
            cut: 0,
            label: false,
            contact: false,
        }
    }

    fn surface_origin(&self, rng: &mut Rng) -> Option<Segment> {
        let o = self.surfaces.point(rng)?;
        let d = sphere(rng);
        let t_max = if rng.next_f32() < 0.5 {
            f32::INFINITY
        } else {
            rng.next_f32() * self.root_diag
        };
        let ray = Ray::new(o, d).with_mask(self.mask);
        let mut nearest: Option<(u32, usize, f32, f32, AABB)> = None;
        self.scene.bvh().cut_crossings(
            &ray,
            SURFACE_T_MIN,
            t_max,
            self.cut,
            &mut |n, l, t0, t1, b| {
                if nearest.is_none_or(|(.., t, _, _)| t0 < t) {
                    nearest = Some((n, l, t0, t1, *b));
                }
            },
        );
        let (n, l, t0, t1, b) = nearest?;
        Some(Segment {
            o,
            d,
            t0,
            t1,
            diag: (b.maximum - b.minimum).length(),
            cut: self.index[&(n, l as u8)],
            label: false,
            contact: true,
        })
    }

    fn through(&self, rng: &mut Rng, short: bool) -> Option<Segment> {
        let c = ((rng.next_f32() * self.boxes.len() as f32) as usize).min(self.boxes.len() - 1);
        let b = &self.boxes[c];
        let (p0, p1) = (on_box(b, rng), on_box(b, rng));
        let d = p1 - p0;
        if d == Vec3A::ZERO {
            return None;
        }
        let t1 = if short { rng.next_f32() } else { 1.0 };
        (t1 > 0.0).then_some(Segment {
            o: p0,
            d,
            t0: 0.0,
            t1,
            diag: (b.maximum - b.minimum).length(),
            cut: c as u32,
            label: false,
            contact: false,
        })
    }

    /// The training pool for `steps` steps: one batch per step, up to
    /// [`POOL`], cycled through beyond it.
    pub(crate) fn pool(&self, seed: u32, key: u32, steps: u32) -> Vec<Segment> {
        let batches = (steps as usize).clamp(1, POOL / BATCH);
        self.many(seed, key, batches * BATCH)
    }

    pub(crate) fn many(&self, seed: u32, key: u32, n: usize) -> Vec<Segment> {
        (0..n as u32)
            .into_par_iter()
            .map(|i| self.segment(seed, key, i))
            .collect()
    }
}

impl Model {
    /// Freshly initialised from the keyed stream: grid entries uniform in
    /// ±1e-4 (Instant NGP), He-uniform MLP weights.
    pub(crate) fn init(seed: u32, root: AABB) -> Self {
        let mut rng = keyed(seed, K_INIT, 0);
        let grid = HashGrid::new(|| (2.0 * rng.next_f32() - 1.0) * 1e-4);
        let mlp = mlp::init(|| rng.next_f32());
        Model::new(grid, mlp, root)
    }
}

/// The trainer's state: the model and one Adam per block.
pub(crate) struct Trainer {
    pub(crate) model: Model,
    adam_grid: Adam,
    adam_mlp: Adam,
}

/// One chunk's gradients.
struct ChunkGrad {
    mlp: Vec<f32>,
    grid: Vec<(u32, f32)>,
}

impl Trainer {
    pub(crate) fn new(model: Model) -> Self {
        Trainer {
            adam_grid: Adam::new(model.grid.params.len(), LR_GRID),
            adam_mlp: Adam::new(model.mlp.len(), LR_MLP),
            model,
        }
    }

    /// `steps` steps over `pool`, cycling through it batch by batch.
    pub(crate) fn train(&mut self, pool: &[Segment], steps: u32) {
        let mut grid_grad = vec![0f32; self.model.grid.params.len()];
        let mut mlp_grad = vec![0f32; self.model.mlp.len()];
        let batches = pool.len() / BATCH;
        for step in 0..steps as usize {
            let batch = &pool[(step % batches) * BATCH..][..BATCH];
            // Class-balanced: each class carries half the batch's weight,
            // or all of it when the other is absent.
            let pos = batch.iter().filter(|s| s.label).count();
            let neg = BATCH - pos;
            let (w_pos, w_neg) = match (pos, neg) {
                (0, _) => (0.0, 1.0 / neg as f32),
                (_, 0) => (1.0 / pos as f32, 0.0),
                _ => (0.5 / pos as f32, 0.5 / neg as f32),
            };
            let model = &self.model;
            let grads: Vec<ChunkGrad> = batch
                .par_chunks(BATCH / CHUNKS)
                .map(|chunk| {
                    let mut g = ChunkGrad {
                        mlp: vec![0f32; model.mlp.len()],
                        grid: Vec::with_capacity(chunk.len() * POINTS * grid::LEVELS * 16),
                    };
                    for s in chunk {
                        let (x, pts) = model.features(s.o, s.d, s.t0, s.t1, s.diag);
                        let (z, acts) = mlp::forward(&model.mlp, &x);
                        let (y, w) = if s.label { (1.0, w_pos) } else { (0.0, w_neg) };
                        let dz = w * (mlp::sigmoid(z) - y);
                        let mut dx = [0f32; IN];
                        mlp::backward(&model.mlp, &x, &acts, dz, &mut g.mlp, &mut dx);
                        for (k, p) in pts.iter().enumerate() {
                            HashGrid::backward(
                                *p,
                                &dx[k * grid::OUT..(k + 1) * grid::OUT],
                                &mut g.grid,
                            );
                        }
                    }
                    g
                })
                .collect();
            grid_grad.fill(0.0);
            mlp_grad.fill(0.0);
            for g in &grads {
                for (a, b) in mlp_grad.iter_mut().zip(&g.mlp) {
                    *a += b;
                }
                for &(i, v) in &g.grid {
                    grid_grad[i as usize] += v;
                }
            }
            self.adam_grid.step(&mut self.model.grid.params, &grid_grad);
            self.adam_mlp.step(&mut self.model.mlp, &mlp_grad);
        }
    }
}

/// Validation counts over a held-out set.
#[derive(Clone, Debug, Default)]
pub(crate) struct Validation {
    pub(crate) n: usize,
    pub(crate) false_pos: usize,
    pub(crate) false_neg: usize,
    pub(crate) contact: usize,
    pub(crate) contact_false_neg: usize,
    /// Errors per cut lane.
    pub(crate) per_cut: Vec<u32>,
}

pub(crate) fn validate(model: &Model, set: &[Segment], n_cut: usize) -> Validation {
    let mut v = Validation {
        n: set.len(),
        per_cut: vec![0; n_cut],
        ..Validation::default()
    };
    let verdicts: Vec<bool> = set
        .par_iter()
        .map(|s| model.logit(s.o, s.d, s.t0, s.t1, s.diag) > 0.0)
        .collect();
    for (s, &occluded) in set.iter().zip(&verdicts) {
        v.contact += s.contact as usize;
        if occluded != s.label {
            v.per_cut[s.cut as usize] += 1;
            if occluded {
                v.false_pos += 1;
            } else {
                v.false_neg += 1;
                v.contact_false_neg += s.contact as usize;
            }
        }
    }
    v
}

/// The training stream's key for refinement round `round` (the final
/// training is the last round).
pub(crate) fn train_key(round: u32) -> u32 {
    K_TRAIN ^ round.wrapping_mul(0x9E37_79B9)
}

pub(crate) fn val_key(round: u32) -> u32 {
    K_VAL ^ round.wrapping_mul(0x9E37_79B9)
}

/// The cut lanes the refinement splits next: the [`REFINE_SHARE`] of lanes
/// with the most validation errors (ties to the lower index), errorless ones
/// never.
pub(crate) fn worst_lanes(v: &Validation) -> Vec<usize> {
    let mut order: Vec<usize> = (0..v.per_cut.len()).filter(|&i| v.per_cut[i] > 0).collect();
    order.sort_by(|&a, &b| v.per_cut[b].cmp(&v.per_cut[a]).then(a.cmp(&b)));
    let take = ((v.per_cut.len() as f32 * REFINE_SHARE).ceil() as usize).min(order.len());
    order.truncate(take);
    order
}

pub(crate) const ROUNDS: u32 = REFINE_ROUNDS;
