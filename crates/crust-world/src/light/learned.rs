//! `learned` light selection's frozen product: the per-cell pick tables
//! [`LightList`] reads at every pick and every MIS weight, and the defensive
//! table both learned selections build theirs with. The pre-pass that trains
//! them walks the world, so it lives with the integrator
//! (`tracer/light_cache.rs`, which also holds the design notes).

use crate::Vec3A;
use crate::light::LightList;

/// Share of every trained cell's picks spread evenly over the lights that
/// emit. What keeps the estimator unbiased, and what bounds the cost where a
/// cell's few receivers mis-estimated a light.
///
/// 0.3 rather than 0.2 for `domelight`. A cell straddling the sun's shadow
/// boundary averages over mixed visibility, and at 0.2 the sun kept only a
/// tenth of the picks there, which showed as fireflies: relMSE 0.113 -> 0.138
/// against power, although 0.021 -> 0.015 with the worst 0.1% of pixels
/// trimmed. At 0.3 it beats power on both (0.107, 0.017), for 5% of the gain
/// on `usdlux`.
///
/// Mixing with the *power* table instead of uniform was measured too, and
/// lost. On ALab power is the problem: it gives the two hidden lights half the
/// picks.
///
/// `neural` shares it (`light_visibility.rs`). Measured there at 0.2 and 0.1
/// as well (16 spp, `--indirect-clamp 0`, mean of four frames): relMSE moved by
/// at most 2.5%, inside the noise, on `domelight` (0.291 / 0.283 / 0.285 at
/// 0.3 / 0.2 / 0.1) and `usdlux` (0.0646 / 0.0635 / 0.0640). No firefly came
/// with the lower shares, as a continuous visibility does not straddle the
/// sun's shadow the way a cell does, but nothing was gained either. On
/// ALab's direct lighting a lower share does gain (trimmed relMSE, frames
/// 1004 and 1030: `neural` 0.189 / 0.141, `learned` 0.195 / 0.164 at 0.3 /
/// 0.1), but its full image does not (`neural` 3.66 / 3.73, `learned` 3.00
/// / 3.01), and `domelight` fireflies under `learned` at 0.2, so it stays
/// 0.3 for both (`docs/light_sampling.md` §3.14).
pub const DEFENSIVE: f32 = 0.3;
/// The learned per-cell pick distributions. Built by the pre-pass
/// (`crust-integrator`'s `tracer/light_cache.rs`, hence the public fields),
/// frozen, and read by [`LightList`] at every pick and every MIS weight.
#[derive(Debug)]
pub struct LightCache {
    pub origin: Vec3A,
    pub inv_cell: f32,
    pub dims: [usize; 3],
    /// Dense grid of `dims` cells: an index into the tables, or `u32::MAX`
    /// for an untrained cell.
    pub slot: Vec<u32>,
    pub lights: usize,
    /// `slots x lights` pick probabilities, and their inclusive running sums.
    pub pmf: Vec<f32>,
    pub cdf: Vec<f32>,
    /// For the debug line and the tests.
    pub receivers: usize,
    pub trained_cells: usize,
}

impl LightCache {
    /// The trained table for the cell holding `p`, as `(pmf, cdf)`.
    #[inline]
    pub fn lookup(&self, p: Vec3A) -> Option<(&[f32], &[f32])> {
        let c = (p - self.origin) * self.inv_cell;
        if !(c.x >= 0.0 && c.y >= 0.0 && c.z >= 0.0) {
            return None;
        }
        let (x, y, z) = (c.x as usize, c.y as usize, c.z as usize);
        if x >= self.dims[0] || y >= self.dims[1] || z >= self.dims[2] {
            return None;
        }
        let s = self.slot[(z * self.dims[1] + y) * self.dims[0] + x];
        if s == u32::MAX {
            return None;
        }
        let r = s as usize * self.lights..(s as usize + 1) * self.lights;
        Some((&self.pmf[r.clone()], &self.cdf[r]))
    }
}

/// The lights that can emit at all — the global selection gives them a
/// nonzero pick — and how many: only these share a defensive part.
pub fn live_lights(lights: &LightList) -> (Vec<bool>, usize) {
    let live: Vec<bool> = (0..lights.count()).map(|k| lights.pmf(k) > 0.0).collect();
    let n_live = live.iter().filter(|&&l| l).count().max(1);
    (live, n_live)
}

/// A learned pick distribution into `cdf` and `pmf`: each live light gets
/// `(1 - d) * w/total + d / n_live`, a dead one nothing, for a defensive
/// share `d` (`DEFENSIVE`, for both learned selections today).
/// `learned`'s cells weigh by contribution, `neural`'s vertices by power
/// times predicted visibility; both build their tables here.
///
/// The CDF is finalised first, in f32, and each pmf is then the width of its
/// own interval, `cdf[k] - cdf[k-1]`. So the probability NEE divides by, and
/// the bounce side weights with, is the one the pick actually lands with.
/// Rounding `p` and the running sum separately could let the two differ, and
/// the forced final 1.0 could make them differ by more.
#[inline]
pub fn defensive_table(
    w: &[f64],
    total: f64,
    live: &[bool],
    n_live: usize,
    d: f32,
    cdf: &mut [f32],
    pmf: &mut [f32],
) {
    let mut running = 0.0f64;
    let mut last_live = None;
    for (k, (&e, c)) in w.iter().zip(cdf.iter_mut()).enumerate() {
        let p = if live[k] {
            last_live = Some(k);
            (1.0 - d as f64) * e / total + d as f64 / n_live as f64
        } else {
            0.0
        };
        running += p;
        *c = running as f32;
    }
    // As in `LightList::select_by`: the last pickable light ends the CDF at
    // exactly one, so no `u` below one falls past it.
    if let Some(last) = last_live {
        for c in &mut cdf[last..] {
            *c = 1.0;
        }
    }
    let mut prev = 0.0f32;
    for (p, &c) in pmf.iter_mut().zip(cdf.iter()) {
        *p = c - prev;
        prev = c;
    }
}
