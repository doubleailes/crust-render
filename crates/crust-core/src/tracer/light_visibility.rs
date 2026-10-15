//! Neural visibility-aware light selection: `crust:lightSelection = "neural"`.
//!
//! `learned` (`light_cache.rs`) stores what its pre-pass found in a uniform
//! grid of per-cell tables, and a grid has two measured weaknesses: a cell
//! straddling a shadow boundary averages visibility across it, and its
//! resolution is set by the receiver count, not by where visibility changes.
//! Bokšanský & Meister (*Neural Visibility Cache for Real-Time Light
//! Sampling*, JCGT 14(2), 2025) store the same knowledge as a continuous
//! function instead: a multi-resolution hash grid feeding a small MLP. This is
//! a CPU, deterministic version of it, an opt-in experiment against
//! `learned` at equal time (`docs/light_sampling.md`, "Neural visibility
//! cache").
//!
//! - **Labels.** The `learned` pre-pass walk ([`light_cache::walk`]) reports,
//!   per receiver per light, the fraction of its samples that delivered light
//!   — backfacing, below the horizon and occluded all count as not.
//! - **The network** ([`shape`], design D3) maps a position in the receivers'
//!   robust bounds to one logit per light; its sigmoid is the light's
//!   predicted visibility `V̂ℓ(x)`. Trained by [`train`] (D5) and frozen before
//!   the first pass.
//! - **The pick** (D2): `pℓ(x) = (1 − D) · πℓ V̂ℓ(x) / Σ πV̂ + D / n_live` over
//!   the power table `π`, with `learned`'s defensive share `D`, and the power
//!   table itself where every prediction is hidden or `x` is outside the
//!   bounds.
//!
//! Three properties are load-bearing, as for `learned`:
//!
//! - **Unbiased.** Every light that emits keeps at least `D / n_live`.
//! - **One strategy on both MIS sides** (D6). [`VisibilityCache::with_table`]
//!   is a pure function of the position's bits, so the bounce side,
//!   recomputing it at `prev.pos`, gets the bits NEE picked with. A
//!   one-entry thread-local memo makes that second lookup free; a miss
//!   recomputes the same bits.
//! - **Deterministic.** The walk collects in receiver order, initialisation
//!   and shuffling draw from `openqmc` under [`K_NVC`], and a batch's
//!   gradient is summed in chunk order whatever the thread count
//!   (`crust_nn::Network::batch_gradient`).

use std::sync::atomic::Ordering;

use crust_nn::{Adam, GridConfig, Network, Shape};
use rayon::prelude::*;

use super::light_cache;
use crate::PathSampler;
use crate::camera::Camera;
use crate::light::LightList;
use crate::light::neural::{MAX_LIGHTS, NEXT_ID, VisibilityCache};
use crate::rt_world::World;

/// Domain key for initialisation and shuffling, apart from every key the
/// render and `learned`'s walk draw with. "NV".
const K_NVC: i32 = 0x4e56;
/// Passes over the receivers, then the weights freeze. A count, not a
/// timer, so the network is a function of the scene alone.
const EPOCHS: usize = 8;
/// Receivers per Adam step.
const BATCH: usize = 256;
/// Adam's learning rates: the hash table's, and the dense layers'
/// (tiny-cuda-nn's defaults for a hash encoding).
const GRID_LR: f32 = 1e-2;
const MLP_LR: f32 = 1e-3;
/// The network (design D3): 8 hash levels of 2 features, from 4 to 107 cells
/// per axis — the finest matching `learned`'s 96-cell grid — in tables of
/// 2^14 vertices (at most 1 MiB of `f32`), then 16 → 32 → 32 → `lights`, ReLU
/// between.
pub(crate) fn shape(lights: usize) -> Shape {
    Shape {
        grid: GridConfig {
            levels: 8,
            features: 2,
            log2_entries: 14,
            base: 4,
            growth: 1.6,
        },
        hidden: vec![32, 32],
        outputs: lights,
    }
}

/// Runs the `learned` walk, trains the network on what it delivered, and
/// builds the cache — or `None` when there is nothing to learn: fewer than
/// two lights, more than [`MAX_LIGHTS`] (with a `WARN`), or no receiver.
///
/// `lights` must already carry the power selection: the predictions weigh
/// it, and every position the network does not cover falls back to it.
pub(crate) fn train(
    world: &World,
    camera: &Camera,
    lights: &LightList,
    width: usize,
    height: usize,
    frame: isize,
) -> Option<VisibilityCache> {
    if !light_cache::trainable(lights, MAX_LIGHTS, "neural") {
        return None;
    }
    let started = std::time::Instant::now();
    let receivers = light_cache::walk(world, camera, lights, width, height, frame, true);
    let (lo, hi) = light_cache::robust_bounds(&receivers)?;
    let n = lights.count();
    let extent = (hi - lo).max_element().max(1e-6);
    let inv_extent = 1.0 / extent;

    // The training set: the receivers inside the bounds, in the unit cube,
    // and their labels, light-major within each receiver.
    let mut xs: Vec<[f32; 3]> = Vec::with_capacity(receivers.len());
    let mut ys: Vec<f32> = Vec::with_capacity(receivers.len() * n);
    for r in &receivers {
        if !(r.p.cmpge(lo).all() && r.p.cmple(hi).all()) {
            continue;
        }
        xs.push(((r.p - lo) * inv_extent).to_array());
        ys.extend((0..n).map(|k| r.delivered(k)));
    }
    if xs.is_empty() {
        return None;
    }
    let (net, loss) = fit(&shape(n), &xs, &ys, frame);
    tracing::debug!(
        "neural light selection: {} receivers ({} trained on), {n} lights, {EPOCHS} epochs, \
         final loss {loss:.4}, in {:?}",
        receivers.len(),
        xs.len(),
        started.elapsed()
    );
    let (live, n_live) = crate::light::learned::live_lights(lights);
    Some(VisibilityCache {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        lo,
        hi,
        inv_extent,
        net,
        power: (0..n).map(|k| lights.pmf(k)).collect(),
        live,
        n_live,
        receivers: receivers.len(),
        trained: xs.len(),
        loss,
    })
}

/// Trains a network of `shape` on `(xs, ys)` (design D5), returning it with
/// the last epoch's mean loss. Deterministic: the initial weights and every
/// epoch's order come from `openqmc` under [`K_NVC`], and a batch's gradient
/// is the same at any thread count.
pub(crate) fn fit(shape: &Shape, xs: &[[f32; 3]], ys: &[f32], frame: isize) -> (Network<f32>, f64) {
    let n = shape.outputs;
    let root = PathSampler::new(0, 0, frame as i32, 0).new_domain(K_NVC);
    let mut init = root.new_domain(0).rng();
    let mut net = Network::<f32>::new(shape, || init.next_f32());
    let mut adam = Adam::new(net.params().len(), net.groups(GRID_LR, MLP_LR));
    let mut grad = vec![0.0f32; net.params().len()];
    let mut order: Vec<u32> = (0..xs.len() as u32).collect();
    let mut bx: Vec<[f32; 3]> = Vec::with_capacity(BATCH);
    let mut by: Vec<f32> = Vec::with_capacity(BATCH * n);
    let mut loss = 0.0f64;
    for epoch in 0..EPOCHS {
        // Fisher–Yates, from the epoch's own stream.
        let mut rng = root.new_domain(1).new_domain(epoch as i32).rng();
        for i in (1..order.len()).rev() {
            let j = ((rng.next_u32() as u64 * (i as u64 + 1)) >> 32) as usize;
            order.swap(i, j);
        }
        let (mut sum, mut batches) = (0.0f64, 0usize);
        for batch in order.chunks(BATCH) {
            bx.clear();
            by.clear();
            for &i in batch {
                let i = i as usize;
                bx.push(xs[i]);
                by.extend_from_slice(&ys[i * n..(i + 1) * n]);
            }
            sum += net.batch_gradient(&bx, &by, &mut grad, |chunks, chunk| {
                (0..chunks).into_par_iter().map(chunk).collect()
            });
            batches += 1;
            adam.step(net.params_mut(), &grad);
        }
        loss = sum / batches as f64;
    }
    (net, loss)
}

#[cfg(test)]
mod tests;
