//! The integrator: one camera path, traced forward a segment per vertex and
//! gathered backward into a radiance estimate (and guiding training samples).
//!
//! Every NEE weight here has a bounce-side twin (`bounce_emission_weight`,
//! `escaped_emission`); change both or neither.

use glam::Vec3A;
use utils::luminance;

use crate::guiding::SampleData;
use crate::hittable::HitRecord;
use crate::material::{Material, ScatterSample, ShadingPoint};
use crate::medium::sample_henyey_greenstein;
use crate::profile::Section;
use crate::ray::Ray;
use crate::rt_world::{World, WorldHit};
use crate::stats::RayStats;
use crate::volume::{PhaseMix, VolumeEvent, Volumes};
use crate::{LightList, PathSampler, profile};

use super::GuidingContext;
use super::settings::SamplingStrategy;

// OpenQMC domain-tree keys. The camera and the path subtree hang off the root
// (per-pixel, per-sample) sampler; each per-event sub-domain hangs off the
// current vertex domain. Distinct keys give independent 4D sub-patterns.
pub(super) const K_CAMERA: i32 = 0; // off root: jitter (0,1) + lens uv (2,3)
const K_PATH: i32 = 1; // off root: the bounce subtree
pub(super) const K_TIME: i32 = 2; // off root: shutter time for motion blur
const K_NEE: i32 = 0; // off vertex: light pick (0) + area uv (1,2)
const K_NEE_SHADOW: i32 = 1; // off vertex: shadow-ray volume transmittance
const K_BSDF: i32 = 2; // off vertex: material scatter block
const K_GUIDE: i32 = 3; // off vertex: guide coin (0) + guide seed (1,2)
const K_PHASE: i32 = 4; // off vertex: phase lobe (0) + HG uv (1,2)
const K_RR: i32 = 5; // off vertex: Russian-roulette survival
const K_MEDIUM: i32 = 6; // off vertex: carried-medium free flight
const K_VOLUME: i32 = 7; // off vertex: volume-region delta tracking

/// Training-only clamp on recorded radiance so a single firefly cannot
/// dominate a directional distribution. Affects the guiding field, never the
/// image estimator.
const TRAIN_RADIANCE_CLAMP: f32 = 1e3;

/// Russian roulette: paths may terminate stochastically once they carry at
/// least this many vertices; the survival probability tracks the path
/// throughput but never drops below the floor, so weights stay bounded.
const RR_START_BOUNCE: usize = 3;
const RR_MIN_PROB: f32 = 0.05;

pub fn ray_color(
    r: &Ray,
    world: &World,
    lights: &LightList,
    volumes: &Volumes,
    depth: i32,
    strategy: SamplingStrategy,
    sampler: PathSampler,
) -> Vec3A {
    let mut no_training = Vec::new();
    let mut stats = RayStats::default();
    // The one-shot entry point (benches and tests), so a scratch per call is
    // the right trade — the renderer's own paths reuse one per work unit.
    let mut scratch = PathScratch::new(depth.max(0) as usize);
    trace_path::<false>(
        r,
        world,
        lights,
        volumes,
        depth,
        strategy,
        0.0,
        sampler,
        None,
        &mut no_training,
        &mut scratch,
        &mut stats,
    )
}

/// Choose the bounce direction and the pdf its contribution is divided by.
///
/// With guiding this is one-sample MIS between the guiding distribution and
/// the material's *continuous* component: pick the guide with probability α,
/// the BSDF otherwise, and divide continuous samples by the mixture pdf
/// `α·p_guide + (1-α)·p_bsdf` — used iff guiding is available at the vertex
/// (trained field + evaluable material), independent of which branch the
/// coin picked. Delta samples (transmission) are a singular component the
/// guide can never produce: they keep their placeholder pdf, are never mixed
/// with a continuous density, and their value is divided by `1-α` to
/// compensate for the coin reducing the delta lobe's selection probability.
/// Are texture-filtering ray cones on? `CRUST_RAY_CONES=0` forces every
/// footprint to zero, which makes every texture point-sample its finest level
/// — the A/B that separates "the mip pyramids changed the image" from "the
/// footprints did". Read once: this is consulted per camera ray.
pub(super) fn ray_cones_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("CRUST_RAY_CONES").as_deref() != Ok("0"))
}

// `inline(always)`, as is `escaped_emission`: each is called once per
// `trace_path` instance, and once the integrator was monomorphised on the
// profiler switch LLVM stopped inlining them into either copy — +1.1%
// instructions on cornellbox with profiling off.
#[inline(always)]
fn sample_bounce_direction(
    r: &Ray,
    rec: &HitRecord,
    sp: &ShadingPoint,
    guiding: Option<&GuidingContext>,
    sampler: PathSampler,
) -> Option<ScatterSample> {
    // Distinct sub-domains: the BSDF scatter block, and the guide block whose
    // first dimension is the α-coin and next two are the guide-sampling seed.
    let bsdf_dom = sampler.new_domain(K_BSDF);
    let g = match guiding {
        Some(g) if g.field.trained_at(rec.p) => g,
        _ => return sp.scatter_importance(r, bsdf_dom),
    };
    let alpha = g.field.config().guide_prob;
    let gs = sampler.new_domain(K_GUIDE).draw_sample_f32::<4>();

    if gs[0] < alpha {
        // Guide branch: draw from the field; the material's continuous
        // component supplies the value and the BSDF side of the mixture pdf.
        if let Some((wi, p_guide)) = g.field.sample(rec.p, [gs[1], gs[2]])
            && let Some((value, p_bsdf)) = sp.eval(r, wi)
        {
            let pdf = (alpha * p_guide + (1.0 - alpha) * p_bsdf).max(1e-4);
            return Some(ScatterSample {
                ray: sp.make_ray(wi),
                value,
                pdf,
                delta: false,
                // The field's directional structure is a coarse quadtree
                // and the material never picked a lobe here, so there is no
                // honest narrow answer to give: take the widest. Guiding
                // only steers secondary bounces, where the cone is already
                // near-saturated, so this costs sharpness nowhere it had any.
                spread: crate::RayCone::MAX_SPREAD,
            });
        }
        // Material with no continuous component: pure BSDF sampling.
        sp.scatter_importance(r, bsdf_dom)
    } else {
        // BSDF branch.
        let mut sample = sp.scatter_importance(r, bsdf_dom)?;
        if sample.delta {
            // Only this branch can reach the delta lobe, so the coin scaled
            // its selection probability by 1-α.
            sample.value /= 1.0 - alpha;
            return Some(sample);
        }
        // A continuous sample means the material has a continuous component
        // (`Material::eval`'s contract), so the mixture always applies here —
        // asking `eval` would run a textured material's whole network for a
        // yes that is already known.
        let wi = sample.ray.direction().normalize();
        debug_assert!(
            sp.eval(r, wi).is_some(),
            "a continuous sample from a material with no continuous component"
        );
        let p_guide = g.field.pdf(rec.p, wi);
        sample.pdf = (alpha * p_guide + (1.0 - alpha) * sample.pdf).max(1e-4);
        Some(sample)
    }
}

/// Everything recorded at one path vertex during the forward walk. The
/// backward gather reconstructs the radiance estimate from these exactly as
/// the old recursion did: `R = segment_emit + atten · (emit_here + nee +
/// factor · (next_emit·next_emit_weight + R_incoming))`.
pub(super) struct VertexRec {
    /// Transmittance over the segment that arrived at this vertex —
    /// Beer-Lambert for a carried medium times the volume-region tracking
    /// weight. For volume-scatter vertices this is the full walk weight of
    /// the event (transmittance × albedo compensation), which multiplies
    /// everything at and beyond the vertex, NEE included.
    pub(super) atten: Vec3A,
    /// Volume emission collected along the arriving segment, already
    /// weighted by the tracking-walk weight up to each emission point.
    /// Added OUTSIDE `atten` in the gather — folding it into `emit_here`
    /// would attenuate it a second time.
    pub(super) segment_emit: Vec3A,
    /// Emission counted at this vertex itself: primary and post-scatter
    /// vertices only. Emission at bounce-arrival vertices is owned by the
    /// previous vertex via `next_emit`/`next_emit_weight`.
    pub(super) emit_here: Vec3A,
    /// Direct lighting gathered by NEE at this vertex.
    pub(super) nee: Vec3A,
    /// Local continuation factor toward the next vertex: `value·cos/pdf`
    /// for surface bounces (already compensated for Russian roulette), the
    /// medium albedo for volume scatters, zero when the path was absorbed
    /// or roulette-killed.
    pub(super) factor: Vec3A,
    /// Raw emission of the surface the continuation ray hit, and the MIS
    /// weight it carries in this vertex's estimator. Patched when the next
    /// vertex is processed; the raw value is kept separate because guiding
    /// training records the emission unweighted.
    pub(super) next_emit: Vec3A,
    pub(super) next_emit_weight: f32,
    /// Guiding-training info (continuous surface bounces in training passes).
    pub(super) train: Option<TrainRec>,
}

pub(super) struct TrainRec {
    pub(super) pos: Vec3A,
    pub(super) dir: Vec3A,
    pub(super) cos: f32,
}

/// Buffers a path walk needs, owned by the worker rather than the walk.
///
/// The forward pass records one [`VertexRec`] per vertex and the backward
/// gather reads them, so the walk needs somewhere to put them — but it does
/// not need *fresh* storage. Allocating per camera sample cost one
/// `malloc`/`free` pair per sample, measured at 4.4% of the render on
/// cornellbox (460 800 pairs for 460 800 samples). A work unit renders
/// thousands of samples through the same buffer instead.
///
/// Held per tile (or, in the scanline path, per rayon worker) — the same
/// granularity as `RayStats`, so it is private to one thread by construction
/// and there is nothing to synchronise.
pub(crate) struct PathScratch {
    pub(super) records: Vec<VertexRec>,
}

impl PathScratch {
    /// `max_depth` is a capacity hint only; the walk may push fewer.
    pub(crate) fn new(max_depth: usize) -> Self {
        Self {
            records: Vec::with_capacity(max_depth),
        }
    }
}

/// The state of the previous surface bounce that the next vertex needs to
/// MIS-weight its emission: where it was and the bounce density.
///
/// Whether NEE could have competed there is `!delta` alone. A continuous
/// sample implies the material has a continuous component, which is exactly
/// what `Material::eval` returning `Some` means; the integrator used to ask
/// `eval` again, which for a textured material is a full network run
/// answering a question the sample already had.
struct PrevBounce<'a> {
    pub(super) pos: Vec3A,
    pub(super) pdf: f32,
    pub(super) delta: bool,
    /// What a fresh `eval` would be asked, kept only to check the claim
    /// above in debug builds.
    #[cfg(debug_assertions)]
    pub(super) check: (Ray, HitRecord, &'a dyn Material, Vec3A),
    #[cfg(not(debug_assertions))]
    pub(super) _mat: std::marker::PhantomData<&'a dyn Material>,
}

impl PrevBounce<'_> {
    /// Whether NEE could have sampled what this bounce reached.
    pub(super) fn continuous(&self) -> bool {
        #[cfg(debug_assertions)]
        {
            let (ray, rec, mat, dir) = &self.check;
            debug_assert!(
                self.delta || mat.eval(ray, rec, *dir).is_some(),
                "a continuous sample from a material with no continuous component"
            );
        }
        !self.delta
    }
}

/// The previous path vertex, as far as emission MIS is concerned: either a
/// surface bounce or a volume-region phase scatter (which runs NEE, so its
/// bounce-hit emission must be MIS-weighted against the same light
/// strategy or it is double-counted). Carried-medium (subsurface) scatters
/// run no NEE and keep `prev = None` instead.
// Large only in debug builds, where `PrevBounce` carries its `check` copy of
// the hit; boxing it would allocate once per bounce for a debug assertion.
#[allow(clippy::large_enum_variant)]
enum PrevVertex<'a> {
    Surface(PrevBounce<'a>),
    Phase {
        /// The scatter point (the light strategy's pdf is evaluated from it).
        pos: Vec3A,
        /// Solid-angle pdf of the sampled phase direction.
        pdf: f32,
    },
}

/// MIS weight for emission reached by the previous vertex's bounce ray.
/// Delta samples are invisible to light sampling (their lobe is excluded
/// from eval), so the bounce carries the emission whole — likewise at
/// vertices where NEE is inactive, and for emissive geometry with no
/// light-list entry, which NEE can never sample. Otherwise the competing
/// density is the same strategy the NEE side uses: the light list's
/// selection probability for this light times the light's own sampling pdf
/// — and a light selected with probability zero is one NEE never samples, so
/// it too keeps its emission whole.
fn bounce_emission_weight(
    prev: &PrevVertex,
    lights: &LightList,
    hit: &WorldHit,
    strategy: SamplingStrategy,
) -> f32 {
    let (from, bounce_pdf) = match prev {
        PrevVertex::Surface(p) => {
            if !p.continuous() {
                return strategy.unopposed_weight();
            }
            (p.pos, p.pdf)
        }
        PrevVertex::Phase { pos, pdf } => (*pos, *pdf),
    };
    match lights.find_by_geom_at(hit.geom_id, from) {
        Some((light, pmf)) if pmf > 0.0 => {
            // A zero pdf is a point NEE refuses to sample (an edge-on point
            // of an area-sampled light, say): nothing competes for it,
            // exactly as for a light NEE never picks.
            let point_pdf = light.pdf_at_point(from, hit.rec.p);
            if point_pdf <= 0.0 {
                return strategy.unopposed_weight();
            }
            let light_pdf = lights.density(point_pdf, pmf).max(1e-6);
            strategy.bounce_weight(bounce_pdf, light_pdf)
        }
        _ => strategy.unopposed_weight(),
    }
}

/// The infinite-light half of bounce-side MIS: what a ray that left the
/// scene along `direction` picks up.
///
/// This is [`bounce_emission_weight`]'s mirror. A light with geometry is
/// found by a bounce ray *hitting* it and weighted by `pdf_at_point`; a
/// light at infinity is found by a bounce ray *escaping* along a direction
/// it covers, and weighted by the pdf reported from `Light::escaped`. Both
/// must use the same density NEE used, or emission is double-counted.
///
/// Returns the MIS-weighted radiance and whether any light covered the
/// direction — the caller falls back to the sky gradient when nothing did.
#[inline(always)]
fn escaped_emission(
    prev: &Option<PrevVertex>,
    lights: &LightList,
    direction: Vec3A,
    strategy: SamplingStrategy,
) -> (Vec3A, bool) {
    if lights.count() == 0 {
        return (Vec3A::ZERO, false);
    }
    // As on the bounce-hit path, a delta or non-evaluable previous vertex
    // means NEE could not have found this light, so there is no competing
    // strategy and the emission is taken whole.
    let competing = match prev {
        Some(PrevVertex::Surface(p)) => p.continuous().then_some((p.pos, p.pdf)),
        Some(PrevVertex::Phase { pos, pdf }) => Some((*pos, *pdf)),
        // Primary rays, and rays leaving a carried-medium scatter, run no
        // NEE — full weight, exactly as `prev = None` means elsewhere.
        None => None,
    };
    let mut radiance = Vec3A::ZERO;
    let mut covered = false;
    let from = competing.map_or(Vec3A::ZERO, |(p, _)| p);
    // The pmf at the vertex the escaping ray left: the one its NEE picked with.
    for (light, pmf) in lights.iter_at(from) {
        let Some((emitted, pdf)) = light.escaped(from, direction) else {
            continue;
        };
        covered = true;
        let weight = match competing {
            Some((_, bounce_pdf)) if strategy.samples_lights() && pmf > 0.0 => {
                let light_pdf = lights.density(pdf, pmf).max(1e-6);
                strategy.bounce_weight(bounce_pdf, light_pdf)
            }
            // No NEE ran for this vertex, the strategy does not sample lights
            // at all, or the selection never picks this one: nothing competes.
            _ => strategy.unopposed_weight(),
        };
        radiance += emitted * weight;
    }
    (radiance, covered)
}

/// NEE shadow test used at surface and volume vertices alike: ZERO when a
/// surface occludes the segment, otherwise the volumetric transmittance
/// through every region it crosses (stochastic for heterogeneous regions,
/// exact for homogeneous ones). MIS weights are unaffected — transmittance
/// is part of the integrand on both strategies, not of either pdf.
fn shadow_transmittance<const PROFILE: bool>(
    world: &World,
    volumes: &Volumes,
    shadow_ray: &Ray,
    distance: f32,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    // Dedicated occlusion query: any hit in range means full shadow, so the
    // early-exit traversal beats searching for the closest hit.
    let _p = profile::scope_if::<PROFILE>(Section::Occlusion);
    stats.shadow_rays += 1;
    if world.occluded(shadow_ray, 0.001, distance - 0.001) {
        stats.shadow_occluded += 1;
        return Vec3A::ZERO;
    }
    if volumes.is_empty() {
        return Vec3A::ONE;
    }
    let mut rng = vertex.new_domain(K_NEE_SHADOW).rng();
    volumes.transmittance(shadow_ray, 0.001, distance - 0.001, &mut rng)
}

/// Direct lighting at a volume-region scatter point. The exact mirror of
/// the surface NEE block: same light-selection strategy, with the
/// phase function (value == pdf for the HG mixture) in place of
/// `brdf·cos`, and the same phase pdf as the competing bounce density that
/// `bounce_emission_weight`'s `Phase` arm uses.
#[allow(clippy::too_many_arguments)]
fn volume_nee<const PROFILE: bool>(
    p: Vec3A,
    wi: Vec3A,
    phase: &PhaseMix,
    world: &World,
    volumes: &Volumes,
    lights: &LightList,
    strategy: SamplingStrategy,
    vertex: PathSampler,
    time: f32,
    stats: &mut RayStats,
) -> Vec3A {
    if !strategy.samples_lights() {
        return Vec3A::ZERO;
    }
    let _p = profile::scope_if::<PROFILE>(Section::VolumeLighting);
    let nee = vertex.new_domain(K_NEE).draw_sample_f32::<4>();
    let Some((light, pmf)) = lights.pick_at(p, nee[0]) else {
        return Vec3A::ZERO;
    };
    let Some(s) = light.sample_li(p, nee[1], nee[2]) else {
        return Vec3A::ZERO;
    };
    stats.light_samples += 1;
    // As at a surface vertex: no shadow ray for a connection already known
    // to carry nothing (a one-sided light seen from behind). Bit-identical,
    // since the ray's own draws come from `K_NEE_SHADOW`.
    let phase_val = phase.pdf(wi.dot(s.direction));
    if s.radiance * phase_val == Vec3A::ZERO {
        return Vec3A::ZERO;
    }
    let shadow_ray = Ray::new(p, s.direction)
        .with_time(time)
        .with_mask(crate::ray::MASK_SHADOW);
    let tr =
        shadow_transmittance::<PROFILE>(world, volumes, &shadow_ray, s.distance, vertex, stats);
    if tr == Vec3A::ZERO {
        return Vec3A::ZERO;
    }
    let light_pdf = lights.density(s.pdf, pmf).max(1e-6);
    let weight = strategy.light_weight(light_pdf, phase_val);
    s.radiance * phase_val * tr * weight / light_pdf
}

/// The integrator: an iterative path tracer in two passes. The forward walk
/// traces one segment per bounce (each hit serves both as the previous
/// vertex's potential light hit and as the next vertex — the old recursion
/// intersected every segment twice), records a `VertexRec` per vertex, and
/// applies Russian roulette past `RR_START_BOUNCE`. The backward gather
/// then folds the records into the radiance estimate and emits guiding
/// training samples, which need the radiance arriving from the rest of the
/// path and therefore cannot be computed forward.
#[allow(clippy::too_many_arguments)]
pub(super) fn trace_path<const PROFILE: bool>(
    r: &Ray,
    world: &World,
    lights: &LightList,
    volumes: &Volumes,
    depth: i32,
    strategy: SamplingStrategy,
    indirect_clamp: f32,
    sampler: PathSampler,
    guiding: Option<&GuidingContext>,
    train_out: &mut Vec<SampleData>,
    scratch: &mut PathScratch,
    stats: &mut RayStats,
) -> Vec3A {
    let training = guiding.is_some_and(|g| g.training);
    // The bounce subtree; each vertex derives its own domain off this by depth.
    let path = sampler.new_domain(K_PATH);
    // Borrowed, not allocated — see `PathScratch`. Capacity carries over from
    // the previous sample, so after the first walk this is free.
    let records = &mut scratch.records;
    records.clear();
    let mut ray = r.clone();
    let mut remaining = depth;
    // Set after surface bounces and volume-region phase scatters; `None`
    // at the primary vertex and after carried-medium (subsurface)
    // scatters, where the next vertex's emission counts fully.
    let mut prev: Option<PrevVertex> = None;
    // Running throughput. Only drives the roulette survival probability —
    // the estimate itself is rebuilt by the backward gather.
    let mut beta = Vec3A::ONE;
    // Radiance entering the path from beyond the last vertex.
    let mut terminal = Vec3A::ZERO;

    loop {
        // This vertex's domain: `records.len()` is the vertex index (nothing
        // has been pushed for it yet). Every per-event draw hangs off `v`.
        let v = path.new_domain(records.len() as i32);

        if remaining <= 0 {
            stats.ended_depth += 1;
            // Depth exhausted. The old recursion still counted bounce-hit
            // emission at the last vertex (its `add_emission` term traced
            // the ray itself) but never the background — reproduce both,
            // attenuating through any media the final segment crosses.
            if let Some(p) = &prev {
                stats.closest_hit += 1;
                let hit = {
                    let _p = profile::scope_if::<PROFILE>(Section::Trace);
                    world.intersect(&ray, 0.001, f32::INFINITY)
                };
                if let Some(hit) = hit {
                    let cos_o = ray.direction().normalize().dot(hit.rec.normal).abs();
                    let mut emitted = hit.mat.emitted_at(&ray, &hit.rec, cos_o);
                    if emitted.length_squared() > 0.0 {
                        if let Some(m) = ray.medium() {
                            emitted *= m.transmittance(hit.rec.t);
                        }
                        if !volumes.is_empty() {
                            let mut rng = v.new_domain(K_VOLUME).rng();
                            emitted *= volumes.transmittance(&ray, 0.001, hit.rec.t, &mut rng);
                        }
                        let last = records.last_mut().expect("prev implies a record");
                        last.next_emit = emitted;
                        last.next_emit_weight = bounce_emission_weight(p, lights, &hit, strategy);
                    }
                }
            }
            break;
        }

        stats.closest_hit += 1;
        let hit_opt = {
            let _p = profile::scope_if::<PROFILE>(Section::Trace);
            world.intersect(&ray, 0.001, f32::INFINITY)
        };
        let t_surf = hit_opt.as_ref().map_or(f32::INFINITY, |h| h.rec.t);

        // Free-flight candidate in the carried homogeneous medium
        // (subsurface / participating glass interiors) — analog sampling
        // at the extinction majorant. Incidental (unbounded across bounces),
        // so it uses the PRNG side rather than a stratified dimension.
        let t_med = match ray.medium() {
            Some(m) if m.is_scattering() => {
                let sigma_t_max = m.sigma_t_max().max(1e-4);
                -(v.new_domain(K_MEDIUM).draw_rnd_f32::<1>()[0].ln()) / sigma_t_max
            }
            _ => f32::INFINITY,
        };

        // Volume-region interaction, clipped to whatever event would
        // otherwise end the segment. Because the walk is bounded by
        // `t_lim`, a real collision is the nearest event by construction —
        // the competition between the carried medium, the surface and the
        // regions is exact (superposed processes), and the `Passthrough`
        // weight is precisely the region transmittance up to the winner.
        let t_lim = t_surf.min(t_med);
        let event = if volumes.is_empty() {
            VolumeEvent::Passthrough {
                transmittance: Vec3A::ONE,
                emitted: Vec3A::ZERO,
            }
        } else {
            let _p = profile::scope_if::<PROFILE>(Section::Volume);
            let mut rng = v.new_domain(K_VOLUME).rng();
            volumes.sample_interaction(&ray, 0.001, t_lim, &mut rng)
        };

        let (vol_tr, vol_emit) = match event {
            VolumeEvent::Scatter {
                p,
                weight,
                phase,
                emitted,
                ..
            } => {
                stats.volume_scatters += 1;
                // === Volume-region scatter vertex ===
                let wi = ray.direction().normalize();
                let ps = v.new_domain(K_PHASE).draw_sample_f32::<4>();
                let dir = phase.sample(wi, ps[0], [ps[1], ps[2]]);
                let phase_pdf = phase.pdf(wi.dot(dir)).max(1e-6);
                let nee = volume_nee::<PROFILE>(
                    p,
                    wi,
                    &phase,
                    world,
                    volumes,
                    lights,
                    strategy,
                    v,
                    ray.time(),
                    stats,
                );

                // The walk weight goes into `atten` (it multiplies NEE and
                // everything beyond); the continuation factor is ONE
                // because the HG value and pdf cancel exactly. Volume
                // vertices are not trained on — the field guides surface
                // bounces only.
                let mut vrec = VertexRec {
                    atten: weight,
                    segment_emit: emitted,
                    emit_here: Vec3A::ZERO,
                    nee,
                    factor: Vec3A::ONE,
                    next_emit: Vec3A::ZERO,
                    next_emit_weight: 1.0,
                    train: None,
                };
                beta *= weight;
                let mut survived = true;
                if records.len() >= RR_START_BOUNCE {
                    stats.rr_tested += 1;
                    let p_survive = beta.max_element().clamp(RR_MIN_PROB, 1.0);
                    if p_survive < 1.0 {
                        if v.new_domain(K_RR).draw_rnd_f32::<1>()[0] >= p_survive {
                            survived = false;
                            stats.rr_killed += 1;
                            vrec.factor = Vec3A::ZERO;
                        } else {
                            vrec.factor /= p_survive;
                            beta /= p_survive;
                        }
                    }
                }
                if !survived {
                    stats.vertices += 1;
                    records.push(vrec);
                    break;
                }
                prev = Some(PrevVertex::Phase {
                    pos: p,
                    pdf: phase_pdf,
                });
                stats.vertices += 1;
                records.push(vrec);
                // Preserve the carried medium: scattering in fog inside a
                // glass interior must keep attenuating in the glass.
                // A phase function scatters over the whole sphere, so the
                // cone saturates here exactly as a diffuse bounce does; only
                // the width it reached on the way in carries forward.
                let cone = ray.cone().scattered(
                    ray.cone().width_at((p - ray.origin()).length()),
                    crate::RayCone::MAX_SPREAD,
                );
                ray = match ray.medium() {
                    Some(m) => Ray::new_in_medium(p, dir, m.clone()),
                    None => Ray::new(p, dir),
                }
                .with_time(ray.time())
                .with_mask(crate::ray::MASK_INDIRECT)
                .with_cone(cone);
                remaining -= 1;
                continue;
            }
            VolumeEvent::Passthrough {
                transmittance,
                emitted,
            } => (transmittance, emitted),
        };

        if t_med < t_surf {
            // === Carried-medium scatter vertex (subsurface interiors) ===
            stats.medium_scatters += 1;
            let medium = ray.medium().expect("t_med implies a medium").clone();
            let sigma_bar = medium.sigma_t_max().max(1e-4);
            let pos = ray.at(t_med);
            let phase_uv = v.new_domain(K_PHASE).draw_sample_f32::<2>();
            let dir = sample_henyey_greenstein(
                ray.direction().normalize(),
                medium.g,
                phase_uv[0],
                phase_uv[1],
            );
            // Weighted analog estimator: sampling was at rate σ̄ (the
            // max-channel extinction), so the event pays σₛ/σ̄ with the
            // chromatic correction e^{(σ̄−σₜ)·t} per channel. For a gray
            // medium this is exactly the single-scattering albedo — the
            // old code's `factor = albedo` with an extra Beer-Lambert on
            // top double-counted extinction.
            let e = (Vec3A::splat(sigma_bar) - (medium.sigma_a + medium.sigma_s)) * t_med;
            let factor = medium.sigma_s / sigma_bar * Vec3A::new(e.x.exp(), e.y.exp(), e.z.exp());
            // Subsurface vertices run no NEE (their shadow rays are
            // blocked by the enclosing surface), so `prev = None` keeps
            // the next hit's emission at full weight — the pairing that
            // avoids double counting.
            let mut vrec = VertexRec {
                atten: vol_tr,
                segment_emit: vol_emit,
                emit_here: Vec3A::ZERO,
                nee: Vec3A::ZERO,
                factor,
                next_emit: Vec3A::ZERO,
                next_emit_weight: 1.0,
                train: None,
            };
            beta *= vol_tr * factor;
            let mut survived = true;
            if records.len() >= RR_START_BOUNCE {
                stats.rr_tested += 1;
                let p_survive = beta.max_element().clamp(RR_MIN_PROB, 1.0);
                if p_survive < 1.0 {
                    if v.new_domain(K_RR).draw_rnd_f32::<1>()[0] >= p_survive {
                        survived = false;
                        stats.rr_killed += 1;
                        vrec.factor = Vec3A::ZERO;
                    } else {
                        vrec.factor /= p_survive;
                        beta /= p_survive;
                    }
                }
            }
            if !survived {
                stats.vertices += 1;
                records.push(vrec);
                break;
            }
            stats.vertices += 1;
            records.push(vrec);
            let cone = ray.cone().scattered(
                ray.cone().width_at((pos - ray.origin()).length()),
                crate::RayCone::MAX_SPREAD,
            );
            ray = Ray::new_in_medium(pos, dir, medium)
                .with_time(ray.time())
                .with_mask(crate::ray::MASK_INDIRECT)
                .with_cone(cone);
            remaining -= 1;
            prev = None;
            continue;
        }

        let Some(hit) = hit_opt else {
            stats.ended_escaped += 1;
            // === Background ===
            // A ray leaving the scene is how lights at infinity are found
            // by chance, so it is a bounce-side MIS event just like hitting
            // an emissive surface.
            let unit_direction = Vec3A::normalize(ray.direction());
            let (mut background, covered) =
                escaped_emission(&prev, lights, unit_direction, strategy);
            if !covered {
                // Nothing at infinity covers this direction — keep the
                // built-in sky gradient so scenes without an environment
                // light look as they always have.
                let t = 0.5 * (unit_direction.y + 1.0);
                background += (1.0 - t) * Vec3A::new(1.0, 1.0, 1.0) + t * Vec3A::new(0.5, 0.7, 1.0);
            }
            // Segment emission is already weighted; the background pays the
            // volume transmittance of the final segment.
            terminal = vol_emit + vol_tr * background;
            break;
        };
        let rec: HitRecord = hit.rec;
        let mat = hit.mat;
        // The cone's perpendicular cross-section where it met this surface.
        // `World::intersect` has already derived the shader-facing texture
        // widths from the same quantity; this is the bounce side of it, kept
        // free of the grazing `1/|cos|` stretch so it cannot compound.
        let cone_width_here = ray.cone().width_at(rec.t * ray.direction().length());

        // Attenuation across the arriving segment: volume-region
        // transmittance times the carried medium's. For a *scattering*
        // medium the surface arrival already paid e^{−σ̄·t} through the
        // free-flight competition (t_med ≥ t_surf), so only the chromatic
        // correction e^{(σ̄−σₜ)·t} remains — exactly ONE for gray media.
        // Non-scattering media (glass tint) keep pure Beer-Lambert.
        let med_arrival = match ray.medium() {
            Some(m) if m.is_scattering() => {
                let sigma_bar = m.sigma_t_max().max(1e-4);
                let e = (Vec3A::splat(sigma_bar) - (m.sigma_a + m.sigma_s)) * rec.t;
                Vec3A::new(e.x.exp(), e.y.exp(), e.z.exp())
            }
            Some(m) => m.transmittance(rec.t),
            None => Vec3A::ONE,
        };
        let atten = vol_tr * med_arrival;

        // Emission accounting: a vertex reached by a bounce hands its
        // emission to the previous vertex's record, MIS-weighted —
        // counting it here too would double it. At the primary vertex and
        // after carried-medium scatters it counts here, in full. Either
        // way the emission pays the arriving segment's attenuation (an
        // emitter seen through tinted glass or smoke must dim).
        let cos_o = ray.direction().normalize().dot(rec.normal).abs();
        // The material's per-hit work (pattern network, textures), done once
        // for every query at this vertex: the emission here, NEE's `eval`, the
        // scatter, and guiding's `eval` / `make_ray`. Every surface vertex
        // scatters, so this is never wasted work.
        let sp = {
            let _p = profile::scope_if::<PROFILE>(Section::EvalBsdfs);
            ShadingPoint::new(mat, &ray, &rec, cos_o)
        };
        let emitted = sp.emitted();
        let mut emit_here = Vec3A::ZERO;
        match &prev {
            Some(p) => {
                if emitted.length_squared() > 0.0 {
                    let last = records.last_mut().expect("prev implies a record");
                    last.next_emit = atten * emitted;
                    last.next_emit_weight = bounce_emission_weight(p, lights, &hit, strategy);
                }
            }
            None => emit_here = emitted,
        }

        // Guide secondary bounces only: primary vertices vary per pixel far
        // below the guiding field's spatial resolution, so guiding them adds
        // parallax-mismatch variance instead of removing any.
        let guiding_here = if prev.is_some() { guiding } else { None };

        // === 1. Direct Lighting via Light Sampling ===
        // The light strategy is "pick one light with the light list's
        // selection probability `pmf` (power-proportional by default), then
        // sample a point on it with its own `sample_li`", so its solid-angle
        // density is `light.pdf · pmf`. `bounce_emission_weight` evaluates
        // the same expression for a bounce-hit light — both MIS weights must
        // describe the same strategy or emission is double-counted.
        let mut nee = Vec3A::ZERO;
        let lighting = profile::scope_if::<PROFILE>(Section::SurfaceLighting);
        let nee_s = v.new_domain(K_NEE).draw_sample_f32::<4>();
        // `sample_li` returns `None` when the light cannot be reached from
        // this point at all — below a dome's horizon, or a degenerate
        // coincident point.
        if let Some((light, pmf)) = strategy
            .samples_lights()
            .then(|| lights.pick_at(rec.p, nee_s[0]))
            .flatten()
            && let Some(ls) = light.sample_li(rec.p, nee_s[1], nee_s[2])
        {
            stats.light_samples += 1;
            let light_dir_unit = ls.direction;

            // A connection carries light only if the light's radiance, the
            // BSDF and the visibility toward it are all non-zero, and the
            // three tests run cheapest first. Radiance is free (a shaped
            // light outside its cone carries zero). The BSDF — delta and
            // transmissive materials return None from `eval`, since they
            // cannot see a light-sampled direction and pick up emission via
            // BSDF sampling instead, and a light below the horizon gets a
            // zero value — is cheaper than the shadow ray: the shading point
            // already ran any pattern network, so `eval` reads no texture.
            // (Before that split a textured `eval` went after the ray, so an
            // occluded light never paid for the network.) Either order is
            // bit-identical: a skipped test's contribution would be exactly
            // zero, and the shadow ray's own draws come from `K_NEE_SHADOW`,
            // which nothing else reads.
            let mut visibility = || {
                let shadow_ray = Ray::new(rec.p, light_dir_unit)
                    .with_time(ray.time())
                    .with_mask(crate::ray::MASK_SHADOW);
                let tr = shadow_transmittance::<PROFILE>(
                    world,
                    volumes,
                    &shadow_ray,
                    ls.distance,
                    v,
                    stats,
                );
                (tr != Vec3A::ZERO).then_some(tr)
            };
            let connection = if ls.radiance == Vec3A::ZERO {
                None
            } else {
                sp.eval(&ray, light_dir_unit)
                    .filter(|(f, _)| ls.radiance * *f != Vec3A::ZERO)
                    .and_then(|(f, pdf)| visibility().map(|tr| (f, pdf, tr)))
            };
            if let Some((brdf_value, brdf_pdf, shadow_tr)) = connection {
                let light_pdf = lights.density(ls.pdf, pmf).max(1e-6);
                // The competing strategy for this MIS weight is the
                // bounce sampler, whose density toward the light is the
                // guide/BSDF mixture whenever guiding is available at
                // this vertex — using the plain BSDF pdf here while the
                // bounce side weights with the mixture makes the two
                // weights sum past one and double-counts emission.
                let bounce_pdf = match guiding_here {
                    Some(g) if g.field.trained_at(rec.p) => {
                        let alpha = g.field.config().guide_prob;
                        alpha * g.field.pdf(rec.p, light_dir_unit) + (1.0 - alpha) * brdf_pdf
                    }
                    _ => brdf_pdf,
                };
                let weight = strategy.light_weight(light_pdf, bounce_pdf);
                // `brdf_value` already carries the geometric cosine —
                // `Material::eval` returns `brdf · |cos|` (unsigned, so a
                // continuous transmission lobe can see a light behind the
                // ray-facing normal). Applying it again here is what used
                // to make this an integral of `brdf · cos²`.
                nee += ls.radiance * brdf_value * shadow_tr * weight / light_pdf;
            }
        }
        drop(lighting);

        let mut vrec = VertexRec {
            atten,
            segment_emit: vol_emit,
            emit_here,
            nee,
            factor: Vec3A::ZERO,
            next_emit: Vec3A::ZERO,
            next_emit_weight: 1.0,
            train: None,
        };

        // === 2. Indirect Lighting via BSDF (or guided) Sampling ===
        let bounce = {
            let _p = profile::scope_if::<PROFILE>(Section::Bounce);
            sample_bounce_direction(&ray, &rec, &sp, guiding_here, v)
        };
        if bounce.is_none() {
            stats.ended_absorbed += 1;
        }
        if let Some(sample) = bounce {
            let dir = sample.ray.direction().normalize();
            // `sample.value` is the material's `brdf · |cos|` (delta lobes
            // carry their whole throughput there instead), so the estimator is
            // just `value / pdf`. This used to multiply by the cosine a second
            // time, making every bounce an integral of `brdf · cos²` — a
            // Lambertian surface then reflected 2/3 of its albedo. NEE applied
            // the same extra factor, so the two stayed consistent with each
            // other and every `--strategy` agreed on the dimmed answer, which
            // is why no MIS test caught it; the furnace test did.
            let mut factor = sample.value / sample.pdf;

            // Russian roulette on the continuation: survive with probability
            // tracking the throughput, dividing it out on survival. Applies
            // to the whole continuation (bounce-hit emission included).
            beta *= atten * factor;
            let mut survived = true;
            if records.len() >= RR_START_BOUNCE {
                stats.rr_tested += 1;
                let p_survive = beta.max_element().clamp(RR_MIN_PROB, 1.0);
                if p_survive < 1.0 {
                    if v.new_domain(K_RR).draw_rnd_f32::<1>()[0] >= p_survive {
                        survived = false;
                        stats.rr_killed += 1;
                    } else {
                        factor /= p_survive;
                        beta /= p_survive;
                    }
                }
            }

            if survived {
                // Training samples cover continuous surface bounces only —
                // the guide can never produce a delta direction. The
                // radiance is filled in by the backward gather.
                if training && !sample.delta {
                    vrec.train = Some(TrainRec {
                        pos: rec.p,
                        dir,
                        cos: rec.normal.dot(dir).abs(),
                    });
                }
                vrec.factor = factor;
                prev = Some(PrevVertex::Surface(PrevBounce {
                    pos: rec.p,
                    pdf: sample.pdf,
                    delta: sample.delta,
                    #[cfg(debug_assertions)]
                    check: (ray.clone(), rec, mat, dir),
                    #[cfg(not(debug_assertions))]
                    _mat: std::marker::PhantomData,
                }));
                stats.vertices += 1;
                records.push(vrec);
                // Materials build the scattered ray without path context;
                // stamp the path's shutter time, the indirect category, and
                // the texture-filtering cone this bounce leaves behind. The
                // cone starts at the width it arrived with — the
                // perpendicular cross-section, *not* the grazing-stretched
                // footprint `World::intersect` handed the shader — and picks
                // up the sampled lobe's own angular width.
                ray = sample
                    .ray
                    .with_time(ray.time())
                    .with_mask(crate::ray::MASK_INDIRECT)
                    .with_cone(ray.cone().scattered(cone_width_here, sample.spread));
                remaining -= 1;
                continue;
            }
        }

        // Absorbed or roulette-killed: this vertex's own gathers stand
        // (factor stays zero), the path ends here.
        stats.vertices += 1;
        records.push(vrec);
        break;
    }

    // Backward gather: fold the records into the estimate, deepest vertex
    // first, emitting guiding training samples along the way. `radiance` is
    // what the old recursion returned to each vertex from its continuation
    // (next vertex's emission suppressed — its MIS-weighted share enters
    // separately through `next_emit`).
    let _gather = profile::scope_if::<PROFILE>(Section::Contributions);
    let mut radiance = terminal;
    for (index, vrec) in records.iter().enumerate().rev() {
        if let Some(t) = &vrec.train {
            // The full incident radiance (reflected + the raw hit emission),
            // weighted by the cosine to match this tracer's estimator. One
            // cosine, not two: the material's value already carries it and the
            // integrator no longer applies a second.
            train_out.push(SampleData {
                pos: t.pos,
                dir: t.dir,
                radiance: (luminance(radiance + vrec.next_emit) * t.cos).min(TRAIN_RADIANCE_CLAMP),
            });
        }
        // With a single record the continuation is only `terminal`: the
        // primary bounce escaped, and what it found at infinity (a dome, a
        // sun, the sky) is the bounce half of *direct* light, whose NEE half
        // is exact. So only a deeper vertex makes the continuation indirect.
        radiance = if index == 0 && indirect_clamp > 0.0 && records.len() > 1 {
            // The primary vertex splits into what `clamp_indirect` leaves
            // alone and the continuation it clamps. Kept off the ordinary
            // expression below, which a disabled clamp must reproduce to the
            // bit.
            vrec.segment_emit
                + vrec.atten
                    * (vrec.emit_here
                        + vrec.nee
                        + vrec.factor * (vrec.next_emit * vrec.next_emit_weight))
                + clamp_indirect(vrec.atten * (vrec.factor * radiance), indirect_clamp)
        } else {
            vrec.segment_emit
                + vrec.atten
                    * (vrec.emit_here
                        + vrec.nee
                        + vrec.factor * (vrec.next_emit * vrec.next_emit_weight + radiance))
        };
    }
    radiance
}

/// Scales `indirect` down whole so its largest channel is at most `limit`.
///
/// "Indirect" is everything a camera sample gathers past its **primary**
/// vertex — the radiance the primary vertex's continuation carries back,
/// NEE and emission found at every deeper vertex alike. What stays exact is
/// the primary vertex's own emission, its NEE, and the MIS-weighted emission
/// its bounce ray finds — an emitter it hits, or a light at infinity it
/// escapes to: together the two halves of direct lighting, which is
/// Cycles' split between *Clamp Direct* and *Clamp Indirect* (an emitter hit
/// from the first bounce counts as direct there too). Clamping one MIS half
/// of direct light and not the other would bias the weights against each
/// other, so direct light is left alone entirely.
///
/// One consequence worth knowing: a surface seen *through* glass or in a
/// mirror is a deeper vertex, so its lighting is indirect and clamped, as in
/// Cycles — harmless at a sensible limit (an ordinarily lit wall is far
/// below it) and it removes the caustic fireflies such paths also carry.
///
/// The colour is scaled rather than clamped per channel, so a saturated
/// highlight keeps its hue instead of drifting toward white. A non-finite
/// sample passes through unchanged — a NaN channel compares false, and an
/// infinite peak is excluded explicitly, since scaling by `limit / inf = 0`
/// would turn the infinite channel into a NaN: the clamp bounds bright
/// samples, it does not repair (or further break) broken ones.
#[inline]
pub(super) fn clamp_indirect(indirect: Vec3A, limit: f32) -> Vec3A {
    let peak = indirect.max_element();
    if peak > limit && peak.is_finite() {
        indirect * (limit / peak)
    } else {
        indirect
    }
}
