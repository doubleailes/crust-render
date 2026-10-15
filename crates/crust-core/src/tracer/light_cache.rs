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
use crate::light::learned::{DEFENSIVE, LightCache, defensive_table, live_lights};
use crate::light::{Light, LightList};
use crate::material::ShadingPoint;
use crate::ray::{MASK_INDIRECT, Ray};
use crate::rt_world::World;
use crate::{PathSampler, Vec3A};
use rayon::prelude::*;

/// One training path per `TRAIN_STRIDE` x `TRAIN_STRIDE` pixels, at least.
const TRAIN_STRIDE: usize = 4;
/// Receiver-light pairs the pre-pass may estimate, and so retain until the
/// reduction: 32 MiB of `f32` contributions and, for `neural`, 8 MiB of delivered counts,
/// `LIGHT_SAMPLES` times as many light evaluations. The stride grows past `TRAIN_STRIDE` to stay inside it.
/// Resolution times light count is what scales, and unbounded, 3840x2160 at
/// the `MAX_LIGHTS` limit would hold ~2 GiB and evaluate over a billion light
/// samples before the first pass. ALab (47 lights, 640x360) uses 2.0 M.
const MAX_PAIRS: usize = 8 << 20;
/// BSDF bounces after the primary hit, so cells that only indirect paths
/// reach are trained too.
const TRAIN_BOUNCES: usize = 2;
/// Samples per light per receiver.
const LIGHT_SAMPLES: usize = 2;
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

/// One receiver of the training walk ([`walk`]): where it is, and what NEE's
/// own estimate found there for each light.
pub(crate) struct Receiver {
    pub(crate) p: Vec3A,
    /// Per light, the mean estimated NEE contribution, `luminance(L·f)·V/p`
    /// over `LIGHT_SAMPLES` samples: what `learned` sums per cell.
    pub(crate) contrib: Vec<f32>,
    /// Per light, how many of its `LIGHT_SAMPLES` samples **delivered
    /// light**: nonzero radiance toward the receiver (not backfacing), above
    /// its horizon (a nonzero BSDF), and a shadow ray at least partly clear.
    /// What `neural` learns as the light's visibility from here
    /// ([`Receiver::delivered`]); empty unless [`walk`] was asked for
    /// labels, as `learned` never reads them.
    pub(crate) delivered: Vec<u8>,
}

impl Receiver {
    /// The fraction of light `k`'s samples that delivered light, in `[0, 1]`.
    pub(crate) fn delivered(&self, k: usize) -> f32 {
        self.delivered[k] as f32 / LIGHT_SAMPLES as f32
    }
}

/// Whether a learned selection can be trained over `lights` at all: two
/// lights or more, over a power table, and at most `limit`. Past the limit it
/// says so once, at `WARN`, naming `mode` (`learned`, `neural`); the other
/// refusals are silent.
pub(crate) fn trainable(lights: &LightList, limit: usize, mode: &str) -> bool {
    let n = lights.count();
    // Only over a power table: with none, `LightList::density` divides by
    // `n` instead of multiplying by the pmf, and would not describe the
    // per-vertex distribution NEE picks with.
    if n < 2 || lights.selection() != crate::LightSelection::Power {
        return false;
    }
    if n > limit {
        tracing::warn!(
            "crust:lightSelection {mode}: {n} lights is past the {limit} it supports — \
             picking by power instead"
        );
        return false;
    }
    true
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
    if !trainable(lights, MAX_LIGHTS, "learned") {
        return None;
    }
    let n = lights.count();
    let receivers = walk(world, camera, lights, width, height, frame, false);
    if receivers.is_empty() {
        return None;
    }
    let (lo, hi) = robust_bounds(&receivers)?;
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

    let (live, n_live) = live_lights(lights);

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

/// The training walk both learned selections share. Coarse camera paths,
/// one per `TRAIN_STRIDE`² pixels (more past `MAX_PAIRS`) and
/// `TRAIN_BOUNCES` BSDF bounces each, and at every vertex every light
/// estimated through the same `sample_li`, BSDF `eval` and shadow-ray query
/// NEE uses, `LIGHT_SAMPLES` times.
///
/// The receivers come back in row order then path order, whatever the
/// thread count: rows run in parallel and are collected in order. With
/// `labels`, each also counts the samples that delivered light (`neural`'s
/// labels); without, `delivered` stays empty and unallocated. The
/// contributions are the same bits either way.
pub(crate) fn walk(
    world: &World,
    camera: &Camera,
    lights: &LightList,
    width: usize,
    height: usize,
    frame: isize,
    labels: bool,
) -> Vec<Receiver> {
    let n = lights.count();
    // Vertices per path, times lights, times paths, within `MAX_PAIRS`.
    let per_path = (1 + TRAIN_BOUNCES) * n;
    let stride = TRAIN_STRIDE.max(
        ((width * height * per_path) as f64 / MAX_PAIRS as f64)
            .sqrt()
            .ceil() as usize,
    );
    let (gw, gh) = (width.div_ceil(stride), height.div_ceil(stride));
    let list = lights.lights();

    // Parallel over rows, collected in row order: a caller's reduction is then
    // sequential and what it learns the same whatever the thread count.
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
                    let mut hit = world.intersect(&ray, crate::ray::TRACE_T_MIN, f32::INFINITY);
                    // A medium boundary is crossed, not shaded: its empty
                    // closure would train a receiver that sees no light, and
                    // end the path before the surfaces inside and behind it.
                    if world.has_medium_boundaries() {
                        crate::tracer::past_medium_boundaries(world, &ray, &mut hit);
                    }
                    let Some(hit) = hit else {
                        break;
                    };
                    let vertex = root.new_domain(1 + depth as i32);
                    let cos = ray.direction().normalize().dot(hit.rec.normal).abs();
                    let sp = ShadingPoint::new(hit.mat, &ray, &hit.rec, cos);
                    let p = hit.rec.p;
                    // Trained through the same links NEE applies, or the
                    // cell tables would favour lights this receiver cannot use.
                    let class = world.light_class(hit.geom_id);
                    let mut contrib = Vec::with_capacity(n);
                    let mut delivered = Vec::with_capacity(if labels { n } else { 0 });
                    for (k, light) in list.iter().enumerate() {
                        let mut sum = 0.0f32;
                        let mut count = 0u8;
                        if lights.illuminates(k, class) {
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
                                    count += (e > 0.0) as u8;
                                }
                            }
                        }
                        contrib.push(sum / LIGHT_SAMPLES as f32);
                        if labels {
                            delivered.push(count);
                        }
                    }
                    out.push(Receiver {
                        p,
                        contrib,
                        delivered,
                    });
                    let bsdf_dom = vertex.new_domain(2);
                    let Some(sample) =
                        sp.scatter_importance(&ray, || bsdf_dom.draw_sample_f32::<4>())
                    else {
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
    rows.into_iter().flatten().collect()
}

/// The receivers' **robust** bounds: per axis, the `OUTLIER` to
/// `1 - OUTLIER` quantiles, or `None` when no receiver is finite. The full
/// bounds let a few bounce vertices far outside the set (ALab's exterior)
/// stretch `learned`'s grid until four cells covered the whole frame.
/// Receivers outside are not trained, and points outside read the global
/// table.
///
/// `learned` then cuts them into cubic cells, sized for about
/// `RECEIVERS_PER_CELL` receivers per occupied cell (receivers lie on
/// surfaces, so occupied cells grow as the resolution squared); `neural`
/// maps them into its network's unit cube.
pub(crate) fn robust_bounds(receivers: &[Receiver]) -> Option<(Vec3A, Vec3A)> {
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
    Some((lo, hi))
}

/// One trained cell's `(cdf, pmf)`: `(1 - DEFENSIVE) * E/ΣE` plus the
/// defensive share spread over the live lights ([`defensive_table`]).
fn cell_table(sum: &[f64], total: f64, live: &[bool], n_live: usize) -> (Vec<f32>, Vec<f32>) {
    let mut cdf = vec![0.0; sum.len()];
    let mut pmf = vec![0.0; sum.len()];
    defensive_table(sum, total, live, n_live, DEFENSIVE, &mut cdf, &mut pmf);
    (cdf, pmf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A training ray that meets a volume-only medium boundary crosses it,
    /// as a path does, and trains the surface behind it. Shaded as a
    /// receiver, the boundary's empty closure saw no light and ended the
    /// path there: the wall behind the fog was never trained.
    #[test]
    fn training_crosses_a_medium_boundary_to_the_wall_behind() {
        let dir = std::env::temp_dir().join("crust_light_cache_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("fog_wall.usda");
        // Three top-level prims, so the stage is imported whole and the
        // bindings resolve.
        std::fs::write(
            &path,
            r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{
    def Scope "Looks"
    {
        def Material "Fog"
        {
            token outputs:mtlx:volume.connect = </World/Looks/Fog/Volume.outputs:out>
            def Shader "Volume"
            {
                uniform token info:id = "ND_volume"
                token inputs:vdf.connect = </World/Looks/Fog/Vdf.outputs:out>
                token outputs:out
            }
            def Shader "Vdf"
            {
                uniform token info:id = "ND_absorption_vdf"
                vector3f inputs:absorption = (0.01, 0.01, 0.01)
                token outputs:out
            }
        }
    }
    def Xform "Geo"
    {
        # A closed box, outward windings, from z = -1 to 1.
        def Mesh "FogBox" (prepend apiSchemas = ["MaterialBindingAPI"])
        {
            uniform token subdivisionScheme = "none"
            rel material:binding = </World/Looks/Fog>
            int[] faceVertexCounts = [4, 4, 4, 4, 4, 4]
            int[] faceVertexIndices = [0, 3, 2, 1, 4, 5, 6, 7, 0, 4, 7, 3, 1, 2, 6, 5, 0, 1, 5, 4, 3, 7, 6, 2]
            point3f[] points = [(-3, -3, -1), (3, -3, -1), (3, 3, -1), (-3, 3, -1), (-3, -3, 1), (3, -3, 1), (3, 3, 1), (-3, 3, 1)]
        }
        def Mesh "Wall"
        {
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(-10, -10, -3), (10, -10, -3), (10, 10, -3), (-10, 10, -3)]
        }
        def SphereLight "A"
        {
            float inputs:radius = 0.1
            double3 xformOp:translate = (-1, 0, -2)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
        def SphereLight "B"
        {
            float inputs:radius = 0.1
            double3 xformOp:translate = (1, 0, -2)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
    def Camera "Cam"
    {
        double3 xformOp:translate = (0, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
"#,
        )
        .expect("write stage");
        let mut scene = crate::Scene::from_usd(&path).expect("stage");
        assert!(scene.world.has_medium_boundaries());
        scene.lights.select_by(crate::LightSelection::Power);
        let cache = train(&scene.world, &scene.camera, &scene.lights, 64, 64, 0)
            .expect("the wall is lit, so there is something to learn");
        assert!(cache.trained_cells > 0);
        assert!(
            cache.lookup(Vec3A::new(0.0, 0.0, -3.0)).is_some(),
            "the wall behind the fog trained no cell"
        );
    }

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
