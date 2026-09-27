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
//! defensively with a uniform share over every light that can emit.
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
//!   could contribute is ever unsampleable. An untrained cell uses the global
//!   power distribution.
//! - **One strategy on both MIS sides.** NEE picks with the cell of the vertex
//!   it samples from; the bounce side (`bounce_emission_weight`,
//!   `escaped_emission`) asks for the pmf at the *previous* vertex's position,
//!   which is that same vertex. Both read the same stored `f32`.
//! - **Deterministic, and scheduling-free.** The pre-pass depends on the scene
//!   and the settings alone, reduces in receiver order, and is frozen before
//!   any pass starts. So tiled and scanline renders stay bit-identical.

use crate::camera::Camera;
use crate::light::LightList;
use crate::material::ShadingPoint;
use crate::ray::{MASK_INDIRECT, MASK_SHADOW, Ray};
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
    /// `slots x lights` pick probabilities, and their inclusive running sums.
    pmf: Vec<f32>,
    cdf: Vec<f32>,
    /// For the debug line and the tests.
    pub receivers: usize,
    pub trained_cells: usize,
}

impl LightCache {
    /// The trained table for the cell holding `p`, as `(pmf, cdf)`.
    #[inline]
    pub(crate) fn lookup(&self, p: Vec3A) -> Option<(&[f32], &[f32])> {
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
                    let Some(hit) = world.intersect(&ray, 0.001, f32::INFINITY) else {
                        break;
                    };
                    let vertex = root.new_domain(1 + depth as i32);
                    let cos = ray.direction().normalize().dot(hit.rec.normal).abs();
                    let sp = ShadingPoint::new(hit.mat, &ray, &hit.rec, cos);
                    let p = hit.rec.p;
                    let contrib = list
                        .iter()
                        .enumerate()
                        .map(|(k, light)| {
                            let mut sum = 0.0f32;
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
                                    .with_mask(MASK_SHADOW);
                                if world.occluded(&shadow, 0.001, ls.distance - 0.001) {
                                    continue;
                                }
                                let e = utils::luminance(c) / ls.pdf.get();
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

    let mut slot = vec![u32::MAX; dims[0] * dims[1] * dims[2]];
    let (mut pmf, mut cdf) = (Vec::new(), Vec::new());
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
        let (cell_cdf, cell_pmf) = cell_table(sum, total, &live, n_live);
        cdf.extend(cell_cdf);
        pmf.extend(cell_pmf);
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
        pmf,
        cdf,
        receivers: receivers.len(),
        trained_cells: trained,
    })
}

/// One trained cell's `(cdf, pmf)`: `(1 - DEFENSIVE) * E/ΣE` plus the
/// defensive share spread over the live lights.
///
/// The CDF is finalised first, in f32, and each pmf is then the width of its
/// own interval, `cdf[k] - cdf[k-1]`. So the probability NEE divides by, and
/// the bounce side weights with, is the one the pick actually lands with.
/// Rounding `p` and the running sum separately could let the two differ, and
/// the forced final 1.0 could make them differ by more.
fn cell_table(sum: &[f64], total: f64, live: &[bool], n_live: usize) -> (Vec<f32>, Vec<f32>) {
    let mut running = 0.0f64;
    let mut cdf = Vec::with_capacity(sum.len());
    let mut last_live = None;
    for (k, &e) in sum.iter().enumerate() {
        let p = if live[k] {
            last_live = Some(k);
            (1.0 - DEFENSIVE as f64) * e / total + DEFENSIVE as f64 / n_live as f64
        } else {
            0.0
        };
        running += p;
        cdf.push(running as f32);
    }
    // As in `LightList::select_by`: the last pickable light ends the CDF at
    // exactly one, so no `u` below one falls past it.
    if let Some(last) = last_live {
        for c in &mut cdf[last..] {
            *c = 1.0;
        }
    }
    let mut prev = 0.0f32;
    let pmf = cdf
        .iter()
        .map(|&c| {
            let p = c - prev;
            prev = c;
            p
        })
        .collect();
    (cdf, pmf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pmf is exactly its own CDF interval, including the last live
    /// light's, whose boundary is forced to 1.0, and a dead light's, which is
    /// empty. Checked where rounding bites: many lights, tiny and uneven
    /// contributions.
    #[test]
    fn each_pmf_is_exactly_its_cdf_interval() {
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
        let (cdf, pmf) = cell_table(&sum, total, &live, n_live);

        let mut prev = 0.0f32;
        for k in 0..n {
            assert_eq!(pmf[k], cdf[k] - prev, "light {k}");
            prev = cdf[k];
            if live[k] {
                // The defensive floor survives the rounding.
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
}
