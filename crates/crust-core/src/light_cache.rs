//! Visibility-aware light selection: `crust:lightSelection = "learned"`.
//!
//! Power selection is blind to where a light is and whether it can be seen.
//! On ALab (`docs/alab_profile.md`) two exterior rect lights took 49% of NEE's
//! picks for being the most powerful, and were visible from **none** of the
//! frame's receivers: only 3.2% of light samples delivered light. No selection
//! that ignores visibility can fix that (`docs/light_sampling.md` §6.5).
//!
//! So this learns it, in the spirit of Hyperion's cache points and roadmap
//! item (n). Before the first pass, a short **training pre-pass** traces coarse
//! camera paths (one per `TRAIN_STRIDE`² pixels, `TRAIN_BOUNCES` BSDF bounces),
//! and at every vertex estimates **each** light's NEE contribution with the
//! integrand itself, `L · f · V / p`, through the same `sample_li`, BSDF `eval`
//! and shadow-ray query NEE uses. The estimates are summed into a uniform grid
//! over the receivers' bounds. Each trained cell gets its own pick
//! distribution, proportional to its lights' contributions and mixed
//! defensively with a uniform share over every light that can emit, with a
//! floor under every light seen in the cell or one of its 26 neighbours. A
//! point reads the **trilinear blend** of the trained cells among the eight
//! whose centres surround it, so no probability jumps at a cell edge.
//!
//! The target is each light's **mean** contribution in the cell. The
//! variance-optimal one-sample pick is proportional to the square root of the
//! second moment (Vévoda et al. 2018), and that was measured too. It won on
//! `usdlux` (0.0395 against 0.0430) and lost on the textured scenes
//! (`usdpreview_textured` fell behind power), so the mean stays.
//!
//! Three properties are load-bearing:
//!
//! - **Unbiased.** The defensive share gives every light that emits a pick
//!   probability of at least `DEFENSIVE / n` in every cell, so no light that
//!   could contribute is ever unsampleable. A point with no trained cell
//!   around it uses the global power distribution.
//! - **One strategy on both MIS sides.** NEE picks with the blend at the
//!   vertex it samples from; the bounce side (`bounce_emission_weight`,
//!   `escaped_emission`) asks for the pmf at the *previous* vertex's position,
//!   which is that same vertex. Both evaluate the same blended CDF, in the
//!   same order, so they read the same `f32`.
//! - **Deterministic, and scheduling-free.** The pre-pass depends on the scene
//!   and the settings alone, reduces in receiver order, and is frozen before
//!   any pass starts. So tiled and scanline renders stay bit-identical.

use crate::camera::Camera;
use crate::light::{Light, LightList};
use crate::material::ShadingPoint;
use crate::ray::{MASK_INDIRECT, Ray};
use crate::rt_world::World;
use crate::{PathSampler, Vec3A};
use rayon::prelude::*;

/// One training path per `TRAIN_STRIDE` x `TRAIN_STRIDE` pixels, at least.
const TRAIN_STRIDE: usize = 4;
/// Receiver-light pairs the pre-pass may estimate, and so retain as `f32`s
/// until the reduction: 32 MiB of contributions, `LIGHT_SAMPLES` times as many
/// light evaluations. The stride grows past `TRAIN_STRIDE` to stay inside it.
/// Resolution times light count is what scales, and unbounded, 3840x2160 at
/// the `MAX_LIGHTS` limit would hold ~2 GiB and evaluate over a billion light
/// samples before the first pass. ALab (47 lights, 640x360) uses 2.0 M.
const MAX_PAIRS: usize = 8 << 20;
/// BSDF bounces after the primary hit, so cells that only indirect paths
/// reach are trained too.
const TRAIN_BOUNCES: usize = 2;
/// Samples per light per receiver.
const LIGHT_SAMPLES: usize = 2;
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
pub const DEFENSIVE: f32 = 0.3;
/// The floor under a light that delivered light to any receiver in a cell or
/// one of its 26 neighbours: `SEEN_FLOOR / n_seen`, `n_seen` being the lights
/// so seen there. A light seen once nearby used to sit at the uniform floor
/// `DEFENSIVE / n`, the same as one never seen; where it was in fact visible
/// but under-sampled, its contribution over that small pmf was a firefly.
/// Lights seen nowhere nearby keep `DEFENSIVE / n`, so hidden lights stay out
/// of the picks (mixing the power table back in put ALab's exterior lights
/// back). The value is chosen by measurement: `docs/light_sampling.md` §3.12.
pub const SEEN_FLOOR: f32 = 0.15;
/// Receivers a cell needs before its own distribution is trusted.
const MIN_RECEIVERS: u32 = 2;
/// Target receivers per occupied cell; sets the grid resolution.
const RECEIVERS_PER_CELL: f64 = 16.0;
/// Fraction of receivers per side, per axis, left outside the grid's bounds.
const OUTLIER: f64 = 0.02;
/// Cells along the grid's longest axis, at most.
const MAX_RESOLUTION: usize = 96;
/// Past this many lights the per-cell tables stop being small, and the
/// training cost (every light at every receiver) stops being cheap.
pub const MAX_LIGHTS: usize = 1024;
/// Domain key for the pre-pass, apart from every key the render draws with.
const K_TRAIN: i32 = 0x4c43; // "LC"

/// The learned per-cell pick distributions. Built by [`train`], frozen, and
/// read by [`LightList`] at every pick and every MIS weight.
#[derive(Debug)]
pub struct LightCache {
    origin: Vec3A,
    inv_cell: f32,
    dims: [usize; 3],
    /// Dense grid of `dims` cells: an index into the tables, or `u32::MAX`
    /// for an untrained cell.
    slot: Vec<u32>,
    lights: usize,
    /// `slots x lights` inclusive running sums of each cell's pick
    /// probabilities. Only the CDFs are stored: a probability is an interval
    /// of the blended CDF (see [`LightCache::pmf_at`]).
    cdf: Vec<f32>,
    /// The last light that can be picked at all: where a `u` past the end of
    /// a blended CDF that rounds below one lands.
    last_live: usize,
    /// For the debug line and the tests.
    pub receivers: usize,
    pub trained_cells: usize,
}

/// The trained cells around a point and their trilinear weights: what every
/// probability at that point is read from.
///
/// Of the eight cells whose centres surround the point, only the trained ones
/// take part, their weights renormalised to one; an untrained corner has
/// weight zero. Blending the power table in for untrained corners was the
/// alternative, and the wrong one: receivers lie on surfaces, so half of a
/// floor point's corners are empty cells, and mixing the power table back in
/// is what put ALab's hidden exterior lights back into the picks. A point
/// with no trained corner has no blend, and reads the power table as before.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Blend {
    slots: [u32; 8],
    weights: [f32; 8],
}

impl LightCache {
    /// The blend at `p`, or `None` where no trained cell is near: outside
    /// the grid, or with every surrounding cell untrained.
    #[inline]
    pub(crate) fn blend_at(&self, p: Vec3A) -> Option<Blend> {
        let c = (p - self.origin) * self.inv_cell;
        // Also false for a NaN, which has no cell.
        if !(c.x >= 0.0 && c.y >= 0.0 && c.z >= 0.0) {
            return None;
        }
        if c.x >= self.dims[0] as f32 || c.y >= self.dims[1] as f32 || c.z >= self.dims[2] as f32 {
            return None;
        }
        // Cell-centre coordinates: the point lies between the centres of
        // cells `base` and `base + 1` along each axis, `t` of the way.
        let g = c - Vec3A::splat(0.5);
        let base = g.floor();
        let t = g - base;
        let base = [base.x as i64, base.y as i64, base.z as i64];
        let mut slots = [0u32; 8];
        let mut weights = [0.0f32; 8];
        let mut total = 0.0f32;
        for (corner, (slot, weight)) in slots.iter_mut().zip(&mut weights).enumerate() {
            let d = [corner & 1, (corner >> 1) & 1, corner >> 2];
            let mut inside = true;
            let mut index = 0usize;
            for axis in (0..3).rev() {
                let i = base[axis] + d[axis] as i64;
                if i < 0 || i >= self.dims[axis] as i64 {
                    inside = false;
                    break;
                }
                index = index * self.dims[axis] + i as usize;
            }
            if !inside {
                continue;
            }
            let s = self.slot[index];
            if s == u32::MAX {
                continue;
            }
            let w = (if d[0] == 1 { t.x } else { 1.0 - t.x })
                * (if d[1] == 1 { t.y } else { 1.0 - t.y })
                * (if d[2] == 1 { t.z } else { 1.0 - t.z });
            *slot = s;
            *weight = w;
            total += w;
        }
        if total <= 0.0 || total.is_nan() {
            return None;
        }
        for w in &mut weights {
            *w /= total;
        }
        Some(Blend { slots, weights })
    }

    /// The blended CDF at light `j`: the weighted sum of the corners' CDFs,
    /// which is the CDF of the weighted sum of their pmfs. Monotone in `j`,
    /// since every corner's is and rounding keeps order. Exactly one from
    /// the last pickable light on: every corner's CDF is one there, so the
    /// weighted sum is one up to the rounding of the weights, and pinning it
    /// makes the last light's interval end where the pick's does
    /// ([`LightCache::pick`] sends every `u` below one somewhere, and
    /// [`LightCache::pmf_at`] must report the interval it lands in).
    #[inline]
    fn cdf_at(&self, b: &Blend, j: usize) -> f32 {
        if j >= self.last_live {
            return 1.0;
        }
        let n = self.lights;
        let mut sum = 0.0f32;
        for (&slot, &w) in b.slots.iter().zip(&b.weights) {
            sum += w * self.cdf[slot as usize * n + j];
        }
        sum
    }

    /// The probability [`LightCache::pick`] lands on light `j` from blend
    /// `b`: its interval of the blended CDF. Computed from the same CDF
    /// values, in the same order, as the pick that lands in it — so the
    /// pmf NEE divides by and the bounce side weights with is the one the
    /// pick actually has, to the bit. Zero for a light no corner can pick.
    #[inline]
    pub(crate) fn pmf_at(&self, b: &Blend, j: usize) -> f32 {
        let hi = self.cdf_at(b, j);
        if j == 0 {
            hi
        } else {
            hi - self.cdf_at(b, j - 1)
        }
    }

    /// Picks a light from `[0, 1)` sample `u` by inverting the blended CDF:
    /// the first light whose CDF exceeds `u`, with its probability. The CDF
    /// is exactly one from the last pickable light on, so every `u` below
    /// one lands on a light at or before it; the fallback only guards a `u`
    /// of one or more.
    #[inline]
    pub(crate) fn pick(&self, b: &Blend, u: f32) -> (usize, f32) {
        let n = self.lights;
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.cdf_at(b, mid) <= u {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let j = if lo < n { lo } else { self.last_live };
        (j, self.pmf_at(b, j))
    }
}

/// Every light's estimated NEE contribution at one receiver.
struct Receiver {
    p: Vec3A,
    contrib: Vec<f32>,
}

/// Runs the training pre-pass and builds the cache, or `None` when there is
/// nothing to learn: fewer than two lights, too many, or no receiver at all.
///
/// `lights` must already carry the global (power) selection: untrained cells
/// fall back to it.
pub(crate) fn train(
    world: &World,
    camera: &Camera,
    lights: &LightList,
    width: usize,
    height: usize,
    frame: isize,
) -> Option<LightCache> {
    let n = lights.count();
    // Only over a power table: with none, `LightList::density` divides by
    // `n` instead of multiplying by the pmf, and would not describe the
    // per-cell distribution NEE picks with.
    if n < 2 || lights.selection() != crate::LightSelection::Power {
        return None;
    }
    if n > MAX_LIGHTS {
        tracing::warn!(
            "crust:lightSelection learned: {n} lights is past the {MAX_LIGHTS} it supports — \
             picking by power instead"
        );
        return None;
    }
    // Vertices per path, times lights, times paths, within `MAX_PAIRS`.
    let per_path = (1 + TRAIN_BOUNCES) * n;
    let stride = TRAIN_STRIDE.max(
        ((width * height * per_path) as f64 / MAX_PAIRS as f64)
            .sqrt()
            .ceil() as usize,
    );
    let (gw, gh) = (width.div_ceil(stride), height.div_ceil(stride));
    let list = lights.lights();

    // Parallel over rows, collected in row order: the reduction below is then
    // sequential and the cache the same whatever the thread count.
    let rows: Vec<Vec<Receiver>> = (0..gh)
        .into_par_iter()
        .map(|j| {
            let mut out = Vec::new();
            for i in 0..gw {
                // OpenQMC decorrelates coordinates within a 256x256 tile only, so
                // grids past 256 take a tile domain, as `render_pixel` does. It is
                // skipped for tile 0, which leaves every grid up to 256 — every
                // render up to 1024 pixels across, the ones measured in
                // `docs/light_sampling.md` §3.12 — drawing exactly what it did.
                let base = PathSampler::new(i as i32, j as i32, frame as i32, 0);
                let tile = (i >> 8) as i32 + ((j >> 8) as i32) * 4096;
                let base = if tile == 0 {
                    base
                } else {
                    base.new_domain(tile)
                };
                let root = base.new_domain(K_TRAIN);
                let cam = root.draw_sample_f32::<4>();
                let u = (i as f32 + cam[0]) / gw as f32;
                let v = (j as f32 + cam[1]) / gh as f32;
                if u >= 1.0 || v >= 1.0 {
                    continue;
                }
                let mut ray = camera.get_ray(u, v, [cam[2], cam[3]], 0.0);
                for depth in 0..=TRAIN_BOUNCES {
                    let Some(hit) = world.intersect(&ray, crate::ray::TRACE_T_MIN, f32::INFINITY)
                    else {
                        break;
                    };
                    let vertex = root.new_domain(1 + depth as i32);
                    let cos = ray.direction().normalize().dot(hit.rec.normal).abs();
                    let sp = ShadingPoint::new(hit.mat, &ray, &hit.rec, cos);
                    let p = hit.rec.p;
                    // Trained through the same links NEE applies, or the
                    // cell tables would favour lights this receiver cannot use.
                    let class = world.light_class(hit.geom_id);
                    let contrib = list
                        .iter()
                        .enumerate()
                        .map(|(k, light)| {
                            let mut sum = 0.0f32;
                            if !lights.illuminates(k, class) {
                                return sum;
                            }
                            for s in 0..LIGHT_SAMPLES {
                                let d = vertex
                                    .new_domain(8 + (k * LIGHT_SAMPLES + s) as i32)
                                    .draw_sample_f32::<2>();
                                let Some(ls) = light.sample_li(p, d[0], d[1]) else {
                                    continue;
                                };
                                if ls.radiance == Vec3A::ZERO
                                    || ls.pdf.get().is_nan()
                                    || ls.pdf.get() <= 0.0
                                {
                                    continue;
                                }
                                let Some((f, _)) = sp.eval(&ray, ls.direction) else {
                                    continue;
                                };
                                let c = ls.radiance * f;
                                if c == Vec3A::ZERO {
                                    continue;
                                }
                                let shadow = Ray::new(p, ls.direction)
                                    .with_time(ray.time())
                                    .with_mask(lights.shadow_mask(k))
                                    .with_curve_exits_ignored(sp.passes_out_of_curves());
                                // The integrator's visibility, cutouts included:
                                // a light seen through a leaf card is trained at
                                // the share the card lets through.
                                // A thin wall's colour steers selection by its
                                // luminance; the estimate itself never sees it.
                                // Grey — open, blocked, cutouts — it is that
                                // value as it stands, which luminance weights
                                // summing to 1 only up to rounding would move.
                                let through = crate::tracer::surface_visibility(
                                    world,
                                    &shadow,
                                    ls.distance,
                                    vertex.new_domain(8 + (k * LIGHT_SAMPLES + s) as i32),
                                    &mut crate::stats::RayStats::default(),
                                );
                                let through = if through == Vec3A::splat(through.x) {
                                    through.x
                                } else {
                                    lights.luma().of(through)
                                };
                                if through <= 0.0 {
                                    continue;
                                }
                                let e = through * lights.luma().of(c) / ls.pdf.get();
                                if e.is_finite() {
                                    sum += e;
                                }
                            }
                            sum / LIGHT_SAMPLES as f32
                        })
                        .collect();
                    out.push(Receiver { p, contrib });
                    let Some(sample) = sp.scatter_importance(&ray, vertex.new_domain(2)) else {
                        break;
                    };
                    // A subsurface sample's ray is the entry into a random
                    // walk, not a bounce: followed as one, it would cross the
                    // object and train receivers on its far inside. The walk
                    // is the tracer's; training stops here, as it does at an
                    // absorbed sample.
                    if sample.subsurface.is_some() {
                        break;
                    }
                    ray = sample.ray.with_time(ray.time()).with_mask(MASK_INDIRECT);
                }
            }
            out
        })
        .collect();
    let receivers: Vec<Receiver> = rows.into_iter().flatten().collect();
    if receivers.is_empty() {
        return None;
    }

    // The grid spans the receivers' **robust** bounds: per axis, the
    // `OUTLIER` to `1 - OUTLIER` quantiles. The full bounds let a few bounce
    // vertices far outside the set (ALab's exterior) stretch the grid until
    // four cells covered the whole frame. Receivers outside are not trained,
    // and points outside read the global table. Cells are cubic, sized for
    // about `RECEIVERS_PER_CELL` receivers per occupied cell. Receivers lie
    // on surfaces, so occupied cells grow as the resolution squared.
    let finite: Vec<Vec3A> = receivers
        .iter()
        .map(|r| r.p)
        .filter(|p| p.is_finite())
        .collect();
    if finite.is_empty() {
        return None;
    }
    let quantile = |axis: usize, q: f64| {
        let mut v: Vec<f32> = finite.iter().map(|p| p[axis]).collect();
        let k = ((v.len() - 1) as f64 * q).round() as usize;
        *v.select_nth_unstable_by(k, f32::total_cmp).1
    };
    let lo = Vec3A::new(
        quantile(0, OUTLIER),
        quantile(1, OUTLIER),
        quantile(2, OUTLIER),
    );
    let hi = Vec3A::new(
        quantile(0, 1.0 - OUTLIER),
        quantile(1, 1.0 - OUTLIER),
        quantile(2, 1.0 - OUTLIER),
    );
    let extent = (hi - lo).max_element().max(1e-6);
    let res =
        ((receivers.len() as f64 / RECEIVERS_PER_CELL).sqrt() as usize).clamp(1, MAX_RESOLUTION);
    let cell = extent / res as f32;
    // Pad by half a cell so points on the boundary land inside.
    let origin = lo - Vec3A::splat(cell * 0.5);
    let inv_cell = 1.0 / cell;
    let dim = |a: f32| (((a + cell) * inv_cell).ceil() as usize).max(1);
    let size = hi - lo;
    let dims = [dim(size.x), dim(size.y), dim(size.z)];

    // Sequential reduction in receiver order: deterministic.
    let mut sums: std::collections::HashMap<usize, (u32, Vec<f64>)> =
        std::collections::HashMap::new();
    let cell_of = |p: Vec3A| {
        let c = (p - origin) * inv_cell;
        let (x, y, z) = (c.x as usize, c.y as usize, c.z as usize);
        (x < dims[0] && y < dims[1] && z < dims[2]).then(|| (z * dims[1] + y) * dims[0] + x)
    };
    let mut order = Vec::new();
    for r in &receivers {
        let Some(c) = r.p.is_finite().then(|| cell_of(r.p)).flatten() else {
            continue;
        };
        let e = sums.entry(c).or_insert_with(|| {
            order.push(c);
            (0, vec![0.0; n])
        });
        e.0 += 1;
        for (acc, &v) in e.1.iter_mut().zip(&r.contrib) {
            *acc += v as f64;
        }
    }

    // Lights that can emit at all: the global selection gives them a nonzero
    // pick. Only these share the defensive part.
    let live: Vec<bool> = (0..n).map(|k| lights.pmf(k) > 0.0).collect();
    let n_live = live.iter().filter(|&&l| l).count().max(1);
    let last_live = live.iter().rposition(|&l| l)?;

    // The lights seen in a cell or any of its 26 neighbours, trained or not:
    // a receiver that saw a light counts wherever its cell borders.
    let seen_near = |c: usize| -> Vec<bool> {
        let (x, y, z) = (
            c % dims[0],
            (c / dims[0]) % dims[1],
            c / (dims[0] * dims[1]),
        );
        let mut seen = vec![false; n];
        for dz in -1i64..=1 {
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny, nz) = (x as i64 + dx, y as i64 + dy, z as i64 + dz);
                    if nx < 0
                        || ny < 0
                        || nz < 0
                        || nx >= dims[0] as i64
                        || ny >= dims[1] as i64
                        || nz >= dims[2] as i64
                    {
                        continue;
                    }
                    let nc = (nz as usize * dims[1] + ny as usize) * dims[0] + nx as usize;
                    if let Some((_, sum)) = sums.get(&nc) {
                        for (k, &e) in sum.iter().enumerate() {
                            if e > 0.0 && live[k] {
                                seen[k] = true;
                            }
                        }
                    }
                }
            }
        }
        seen
    };

    let mut slot = vec![u32::MAX; dims[0] * dims[1] * dims[2]];
    let mut cdf = Vec::new();
    let mut trained = 0usize;
    for c in order {
        let (count, sum) = &sums[&c];
        let total: f64 = sum.iter().sum();
        if *count < MIN_RECEIVERS || !(total > 0.0 && total.is_finite()) {
            // Too few receivers, or no light seen from any of them: the
            // global distribution is at least no worse than guessing.
            continue;
        }
        slot[c] = trained as u32;
        trained += 1;
        let seen = seen_near(c);
        cdf.extend(cell_table(sum, total, &live, n_live, &seen));
    }
    if trained == 0 {
        return None;
    }
    Some(LightCache {
        origin,
        inv_cell,
        dims,
        slot,
        lights: n,
        cdf,
        last_live,
        receivers: receivers.len(),
        trained_cells: trained,
    })
}

/// One trained cell's CDF: over each live light, `(1 - DEFENSIVE) * E/ΣE`
/// plus the defensive share `DEFENSIVE / n_live`, raised to the visibility
/// floor `SEEN_FLOOR / n_seen` for the lights `seen` in the cell's
/// neighbourhood (`n_seen` of them). What the floors add is taken back from
/// the lights above their floor, in proportion to their excess, so the table
/// still sums to one and a light seen nowhere nearby keeps exactly
/// `DEFENSIVE / n_live`. Where no floor binds, the table is the plain
/// defensive mixture.
///
/// A higher floor for lights at infinity — the uniform `1 / n_live` the power
/// table gives them — was tried against ALab's fireflies and reverted: it
/// left ALab's untrimmed relMSE where it was (the firefly is not the sun's),
/// cost 4% on its trimmed one, and on `domelight`, whose two lights are both
/// at infinity, it forced a 50/50 table and erased the 1.24× the blend had
/// won there.
///
/// The CDF is accumulated in f64 and rounded once per entry; a probability is
/// then an interval of the blended CDF ([`LightCache::pmf_at`]), so the pick
/// and the weights cannot disagree by construction. The last pickable light
/// ends the CDF at exactly one, as in `LightList::select_by`, so no `u`
/// below one falls past it in an unblended cell.
fn cell_table(sum: &[f64], total: f64, live: &[bool], n_live: usize, seen: &[bool]) -> Vec<f32> {
    let n_seen = seen.iter().zip(live).filter(|(s, l)| **s && **l).count();
    let defensive = DEFENSIVE as f64 / n_live as f64;
    let seen_floor = if n_seen > 0 {
        (SEEN_FLOOR as f64 / n_seen as f64).max(defensive)
    } else {
        defensive
    };
    let floors: Vec<f64> = (0..sum.len())
        .map(|k| {
            if !live[k] {
                return 0.0;
            }
            if seen[k] { seen_floor } else { defensive }
        })
        .collect();
    // The floors leave room for the learned part: at most
    // `SEEN_FLOOR + DEFENSIVE`, below one.
    let floor_total: f64 = floors.iter().sum();
    let above_total: f64 = (0..sum.len())
        .filter(|&k| live[k])
        .map(|k| {
            let learned = (1.0 - DEFENSIVE as f64) * sum[k] / total;
            (defensive + learned - floors[k]).max(0.0)
        })
        .sum();
    // The learned mass left once every floor is paid, spread over the lights
    // above their floor in proportion to their excess.
    let scale = if above_total > 0.0 {
        (1.0 - floor_total) / above_total
    } else {
        0.0
    };
    let mut running = 0.0f64;
    let mut cdf = Vec::with_capacity(sum.len());
    let mut last_live = None;
    for (k, &e) in sum.iter().enumerate() {
        let p = if live[k] {
            last_live = Some(k);
            let learned = (1.0 - DEFENSIVE as f64) * e / total;
            floors[k] + scale * (defensive + learned - floors[k]).max(0.0)
        } else {
            0.0
        };
        running += p;
        cdf.push(running as f32);
    }
    if let Some(last) = last_live {
        for c in &mut cdf[last..] {
            *c = 1.0;
        }
    }
    cdf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pmf_of(cdf: &[f32]) -> Vec<f32> {
        let mut prev = 0.0f32;
        cdf.iter()
            .map(|&c| {
                let p = c - prev;
                prev = c;
                p
            })
            .collect()
    }

    /// Every live light keeps the defensive floor through the rounding, a
    /// dead light's interval is empty, and the table ends at exactly one.
    /// Checked where rounding bites: many lights, tiny and uneven
    /// contributions.
    #[test]
    fn each_live_light_keeps_its_floor_and_the_table_ends_at_one() {
        let n = MAX_LIGHTS;
        let sum: Vec<f64> = (0..n)
            .map(|k| {
                if k % 7 == 0 {
                    0.0
                } else {
                    1e-9 * (k as f64).powf(1.7)
                }
            })
            .collect();
        let total: f64 = sum.iter().sum();
        // A few dead lights, the last one among them.
        let live: Vec<bool> = (0..n).map(|k| k % 101 != 3 && k != n - 1).collect();
        let n_live = live.iter().filter(|&&l| l).count();
        let seen: Vec<bool> = sum.iter().map(|&e| e > 0.0).collect();
        let cdf = cell_table(&sum, total, &live, n_live, &seen);
        let pmf = pmf_of(&cdf);
        for k in 0..n {
            if live[k] {
                assert!(
                    pmf[k] >= 0.99 * DEFENSIVE / n_live as f32,
                    "light {k}: {}",
                    pmf[k]
                );
            } else {
                assert_eq!(pmf[k], 0.0, "dead light {k}");
            }
        }
        assert_eq!(*cdf.last().unwrap(), 1.0);
        let total_p: f64 = pmf.iter().map(|&p| p as f64).sum();
        assert!((total_p - 1.0).abs() < 1e-6, "{total_p}");
    }

    /// The visibility floor (design D2): a light seen nowhere nearby keeps
    /// exactly `DEFENSIVE / n_live`; a light seen once nearby, with nothing
    /// from it in this cell, gets `SEEN_FLOOR / n_seen` instead; the rest is
    /// taken from the lights above their floor; and where no floor binds the
    /// table is the plain defensive mixture.
    #[test]
    fn a_light_seen_nearby_gets_the_floor_and_an_unseen_one_the_defensive_share() {
        // Six live lights, two of them seen nearby: the floor `0.15 / 2` is
        // above the defensive `0.3 / 6`, so the test can tell them apart.
        // Light 0 lights this cell; light 1 was seen by a neighbour only;
        // lights 2 to 5 were never seen nearby.
        let live = vec![true; 6];
        let sum = vec![10.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let seen = vec![true, true, false, false, false, false];
        let pmf = pmf_of(&cell_table(&sum, 10.0, &live, 6, &seen));
        let defensive = DEFENSIVE / 6.0;
        let floor = SEEN_FLOOR / 2.0;
        assert!(floor > defensive + 0.02);
        for (k, &p) in pmf.iter().enumerate().skip(2) {
            assert!((p - defensive).abs() < 1e-6, "unseen {k}: {p}");
        }
        assert!((pmf[1] - floor).abs() < 1e-6, "seen nearby: {}", pmf[1]);
        let total: f32 = pmf.iter().sum();
        assert!((total - 1.0).abs() < 1e-6);
        assert!((pmf[0] - (1.0 - floor - 4.0 * defensive)).abs() < 1e-6);

        // Both lights well above the floor: the mixture as it always was.
        let sum = vec![6.0, 4.0];
        let pmf = pmf_of(&cell_table(&sum, 10.0, &[true, true], 2, &[true, true]));
        assert!((pmf[0] - (0.7 * 0.6 + 0.15)).abs() < 1e-6, "{}", pmf[0]);
        assert!((pmf[1] - (0.7 * 0.4 + 0.15)).abs() < 1e-6, "{}", pmf[1]);
    }

    /// A cache of two trained cells side by side along x, and an untrained
    /// row above them.
    fn two_cells(a: &[f64], b: &[f64]) -> LightCache {
        let live = [true, true, true];
        let seen = [true, true, true];
        let mut cdf = cell_table(a, a.iter().sum(), &live, 3, &seen);
        cdf.extend(cell_table(b, b.iter().sum(), &live, 3, &seen));
        LightCache {
            origin: Vec3A::ZERO,
            inv_cell: 1.0,
            dims: [2, 2, 1],
            // Row y = 0 trained (slots 0 and 1), row y = 1 untrained.
            slot: vec![0, 1, u32::MAX, u32::MAX],
            lights: 3,
            cdf,
            last_live: 2,
            receivers: 0,
            trained_cells: 2,
        }
    }

    /// Design D1: along a line crossing the boundary between two trained
    /// cells, every light's probability changes continuously and the blend
    /// sums to one; the pick's probability is the one `pmf_at` reports; and
    /// an untrained neighbour takes no part, so the trained row's tables are
    /// read above it as well.
    #[test]
    fn the_blend_is_continuous_across_a_cell_edge_and_sums_to_one() {
        let cache = two_cells(&[8.0, 1.0, 1.0], &[1.0, 1.0, 8.0]);
        let at = |x: f32, y: f32| {
            let b = cache.blend_at(Vec3A::new(x, y, 0.5)).expect("inside");
            [0, 1, 2].map(|j| cache.pmf_at(&b, j))
        };
        let mut prev = at(0.5, 0.5);
        let steps = 400;
        for i in 1..=steps {
            let x = 0.5 + i as f32 / steps as f32;
            let now = at(x, 0.5);
            let total: f32 = now.iter().sum();
            assert!((total - 1.0).abs() < 1e-5, "x {x}: sums to {total}");
            for j in 0..3 {
                assert!(
                    (now[j] - prev[j]).abs() < 0.01,
                    "x {x}, light {j}: {} -> {}",
                    prev[j],
                    now[j]
                );
            }
            prev = now;
        }
        // At the centres the blend is each cell's own table; between them
        // light 0 falls and light 2 rises.
        let (left, right, mid) = (at(0.5, 0.5), at(1.5, 0.5), at(1.0, 0.5));
        assert!(left[0] > 0.6 && right[2] > 0.6);
        assert!((mid[0] - (left[0] + right[0]) / 2.0).abs() < 1e-6);
        // The untrained row above reads the trained row below it.
        assert_eq!(at(0.5, 1.4), at(0.5, 0.5));
        // Outside the grid: nothing.
        assert!(cache.blend_at(Vec3A::new(-0.1, 0.5, 0.5)).is_none());
        assert!(cache.blend_at(Vec3A::new(0.5, 0.5, 1.5)).is_none());

        // The pick lands in the interval `pmf_at` reports, at every u.
        let b = cache.blend_at(Vec3A::new(1.2, 0.5, 0.5)).unwrap();
        let mut counts = [0usize; 3];
        let n = 100_000;
        for i in 0..n {
            let u = (i as f32 + 0.5) / n as f32;
            let (j, p) = cache.pick(&b, u);
            assert_eq!(p, cache.pmf_at(&b, j));
            counts[j] += 1;
        }
        for (j, &count) in counts.iter().enumerate() {
            let frequency = count as f32 / n as f32;
            assert!(
                (frequency - cache.pmf_at(&b, j)).abs() < 2e-5,
                "light {j}: picked {frequency}, pmf {}",
                cache.pmf_at(&b, j)
            );
        }
        // A `u` at the top of the range lands on the last pickable light,
        // and the last light's interval ends at exactly one — so what the
        // pick reports for it is one minus the CDF before it, whatever the
        // weights' rounding leaves the blended sum at.
        let top = 1.0 - f32::EPSILON / 2.0;
        assert_eq!(cache.pick(&b, top).0, 2);
        assert_eq!(cache.cdf_at(&b, 2), 1.0);
        assert_eq!(cache.pick(&b, top).1, 1.0 - cache.cdf_at(&b, 1));
        // Any point of the grid, including ones whose weights do not sum to
        // one in f32: the last CDF is pinned.
        for i in 0..50 {
            let p = Vec3A::new(0.5 + i as f32 / 49.0, 0.2 + i as f32 / 70.0, 0.5);
            let b = cache.blend_at(p).unwrap();
            assert_eq!(cache.cdf_at(&b, 2), 1.0, "{p}");
            let w: f32 = b.weights.iter().sum();
            assert!((w - 1.0).abs() < 1e-6);
        }
    }
}
