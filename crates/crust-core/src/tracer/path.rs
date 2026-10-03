//! The integrator: one camera path, traced forward a segment per vertex and
//! gathered backward into a radiance estimate (and guiding training samples).
//!
//! Every NEE weight here has a bounce-side twin (`bounce_emission_weight`,
//! `escaped_emission`); change both or neither.

use glam::Vec3A;
use utils::{exp3, luminance};

use crate::guiding::SampleData;
use crate::hittable::HitRecord;
use crate::material::{Material, ScatterSample, ShadingPoint};
use crate::medium::{Medium, sample_henyey_greenstein};
use crate::pdf::PdfSolidAngle;
use crate::profile::Section;
use crate::ray::{Ray, RayMask, TRACE_T_MIN};
use crate::rt_world::{World, WorldHit};
use crate::stats::RayStats;
use crate::subsurface::{ExitLambertian, WalkCost, random_walk};
use crate::volume::{PhaseMix, VolumeEvent, Volumes};
use crate::{EVERY_CLASS, Light, LightList, PathSampler, profile};

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
const K_SSS: i32 = 8; // off vertex: the subsurface random walk (per step below)
const K_CUTOUT: i32 = 9; // off vertex: presence at each cutout the segment meets

/// The white Lambertian every random walk exits through.
static SSS_EXIT: ExitLambertian = ExitLambertian;

/// Training-only clamp on recorded radiance so a single firefly cannot
/// dominate a directional distribution. Affects the guiding field, never the
/// image estimator.
const TRAIN_RADIANCE_CLAMP: f32 = 1e3;

/// Russian roulette: paths may terminate stochastically once they carry at
/// least this many vertices; the survival probability tracks the path
/// throughput but never drops below the floor, so weights stay bounded.
const RR_START_BOUNCE: usize = 3;
const RR_MIN_PROB: f32 = 0.05;

/// Russian roulette on a vertex's continuation, once the path carries at
/// least [`RR_START_BOUNCE`] vertices (`vertex` is this one's index): survive
/// with a probability tracking the throughput `beta`, and on survival divide
/// it out of both `beta` and the vertex's continuation `factor`. A killed
/// path zeroes `factor` and returns `false`; the vertex's own gathers stand.
#[inline(always)]
fn roulette(
    beta: &mut Vec3A,
    factor: &mut Vec3A,
    vertex: usize,
    v: PathSampler,
    stats: &mut RayStats,
) -> bool {
    if vertex < RR_START_BOUNCE {
        return true;
    }
    stats.rr_tested += 1;
    let p_survive = beta.max_element().clamp(RR_MIN_PROB, 1.0);
    if p_survive < 1.0 {
        if v.new_domain(K_RR).draw_rnd_f32::<1>()[0] >= p_survive {
            stats.rr_killed += 1;
            *factor = Vec3A::ZERO;
            return false;
        }
        *factor /= p_survive;
        *beta /= p_survive;
    }
    true
}

/// The chromatic correction `e^{(σ̄−σₜ)·t}` per channel that a scattering
/// medium owes over a free flight of length `t` sampled at its majorant
/// `sigma_bar` (the max-channel extinction): the distance sampling already
/// paid `e^{−σ̄·t}`, so only the per-channel difference remains — exactly
/// ONE for a gray medium.
#[inline(always)]
fn chromatic_correction(m: &Medium, sigma_bar: f32, t: f32) -> Vec3A {
    exp3((Vec3A::splat(sigma_bar) - (m.sigma_a + m.sigma_s)) * t)
}

/// The ray a phase-function scatter at `p` (a volume region's, or the
/// carried medium's) leaves along `dir`: in the same carried medium —
/// scattering in fog inside a glass interior must keep attenuating in the
/// glass — at the path's shutter time, as an indirect ray. A phase function
/// scatters over the whole sphere, so the cone saturates here exactly as a
/// diffuse bounce does; only the width it reached on the way in carries
/// forward.
#[inline(always)]
fn phase_scattered(ray: &Ray, p: Vec3A, dir: Vec3A) -> Ray {
    let cone = ray.cone().scattered(
        ray.cone().width_at((p - ray.origin()).length()),
        crate::RayCone::MAX_SPREAD,
    );
    match ray.medium() {
        Some(m) => Ray::new_in_medium(p, dir, *m),
        None => Ray::new(p, dir),
    }
    .with_time(ray.time())
    .with_mask(crate::ray::MASK_INDIRECT)
    .with_cone(cone)
}

/// What every level of the integrator reads and none of it changes: the
/// scene, the light strategy and the pass's settings. Built once per pass
/// (once per call for [`ray_color`]) and handed down by reference.
///
/// Borrowed by every helper rather than unpacked into their parameter
/// lists. `trace_path` and the per-vertex helpers it calls are inlined, so
/// LLVM sees through the reference to the fields, as it did to `&self.world`
/// and `self.settings` before the struct existed.
#[derive(Clone, Copy)]
pub(super) struct PathContext<'a> {
    pub(super) world: &'a World,
    pub(super) lights: &'a LightList,
    pub(super) volumes: &'a Volumes,
    /// Maximum path depth: vertices a path may spend.
    pub(super) depth: i32,
    pub(super) strategy: SamplingStrategy,
    /// The firefly clamp on what the primary vertex's continuation carries
    /// back (see [`clamp_indirect`]); `None` leaves the estimate unbiased.
    pub(super) indirect_clamp: Option<f32>,
    /// The pass's guiding field, if it has one.
    pub(super) guiding: Option<&'a GuidingContext<'a>>,
}

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
    let ctx = PathContext {
        world,
        lights,
        volumes,
        depth,
        strategy,
        indirect_clamp: None,
        guiding: None,
    };
    trace_path::<false>(&ctx, r, sampler, &mut no_training, &mut scratch, &mut stats)
}

// `inline(always)`, as is `escaped_emission`: each is called once per
// `trace_path` instance, and once the integrator was monomorphised on the
// profiler switch LLVM stopped inlining them into either copy — +1.1%
// instructions on cornellbox with profiling off.
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
            // Floored on this side only: see `GuidingField::mixture_pdf`.
            let pdf = g.field.mixture_pdf(p_guide, p_bsdf).max(1e-4);
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
                subsurface: None,
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
        // Floored on this side only: see `GuidingField::mixture_pdf`.
        sample.pdf = g
            .field
            .mixture_pdf(g.field.pdf(rec.p, wi), sample.pdf)
            .max(1e-4);
        Some(sample)
    }
}

/// Everything recorded at one path vertex during the forward walk. The
/// backward gather folds these into the radiance estimate, vertex by vertex:
/// `R = segment_emit + atten · (emit_here + nee +
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
    /// Where the last random walk left its object — read only while the
    /// path's own flag says a walk is pending. Kept here, not in the path,
    /// so a path that never walks never initialises it.
    sss_exit: PendingExit,
}

impl PathScratch {
    /// `max_depth` is a capacity hint only; the walk may push fewer.
    pub(crate) fn new(max_depth: usize) -> Self {
        Self {
            records: Vec::with_capacity(max_depth),
            sss_exit: PendingExit::default(),
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
pub(super) struct PrevBounce<'a> {
    pub(super) pos: Vec3A,
    pub(super) pdf: PdfSolidAngle,
    pub(super) delta: bool,
    /// The light-link class of the surface this bounce left: the receiver
    /// whose links decide what the bounce may collect.
    pub(super) class: u16,
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
pub(super) enum PrevVertex<'a> {
    Surface(PrevBounce<'a>),
    Phase {
        /// The scatter point (the light strategy's pdf is evaluated from it).
        pos: Vec3A,
        /// Solid-angle pdf of the sampled phase direction.
        pdf: PdfSolidAngle,
        /// The light-link class of the volume region scattered in.
        class: u16,
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
///
/// `inline(always)` for the reason `escaped_emission` is: called once per
/// `trace_path` instance, and with the lights statically dispatched the
/// integrator grew past where LLVM would inline it on its own.
#[inline(always)]
fn bounce_emission_weight(
    prev: &PrevVertex,
    lights: &LightList,
    hit: &WorldHit,
    strategy: SamplingStrategy,
) -> f32 {
    // The receiver is the vertex the bounce left, and its links apply
    // whatever the bounce's lobe: a light that does not illuminate it gives
    // nothing on this side, exactly as NEE there skips it.
    let (from, class, competing) = match prev {
        PrevVertex::Surface(p) => (p.pos, p.class, p.continuous().then_some(p.pdf)),
        PrevVertex::Phase { pos, pdf, class } => (*pos, *class, Some(*pdf)),
    };
    // Nothing competes after a delta bounce, and without links nothing can
    // filter it either: skip the light lookup, as before links existed.
    if competing.is_none() && lights.links().is_none() {
        return strategy.unopposed_weight();
    }
    let Some((index, pmf)) = lights.find_index_by_geom_at(hit.geom_id, from) else {
        // Emissive geometry with no light-list entry: NEE never samples it.
        return strategy.unopposed_weight();
    };
    if !lights.illuminates(index, class) {
        return 0.0;
    }
    let Some(bounce_pdf) = competing else {
        return strategy.unopposed_weight();
    };
    // A shadow-linked light is NEE's alone at a continuous vertex: this ray
    // is stopped by occluders its shadow rays ignore, so the two strategies
    // disagree on its visibility and cannot be MIS-combined.
    if lights.nee_only(index) && strategy.samples_lights() {
        return 0.0;
    }
    let light = lights.light(index);
    if pmf > 0.0 {
        // No pdf is a point NEE refuses to sample (an edge-on point of
        // an area-sampled light, say): nothing competes for it, exactly
        // as for a light NEE never picks.
        let Some(point_pdf) = light.pdf_at_point(from, hit.rec.p) else {
            return strategy.unopposed_weight();
        };
        let light_pdf = lights.density(point_pdf, pmf);
        strategy.bounce_weight(bounce_pdf, light_pdf)
    } else {
        strategy.unopposed_weight()
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
/// `mask` is the escaping ray's category. A camera ray sees only the lights
/// visible to the camera, or the backdrops alone when there are any: they
/// stand in front of every other light at infinity, for camera rays only.
/// Camera rays run no NEE, so hiding a light from them moves no MIS weight.
///
/// A direction no light answers is black: there is no built-in sky.
#[inline(always)]
pub(super) fn escaped_emission(
    prev: &Option<PrevVertex>,
    lights: &LightList,
    direction: Vec3A,
    mask: RayMask,
    strategy: SamplingStrategy,
) -> Vec3A {
    let mut radiance = Vec3A::ZERO;
    if lights.escapes_to_backdrop(mask) {
        // No NEE ever competes for a backdrop — nothing can select it — so
        // its emission is taken whole, as for any camera ray.
        for backdrop in lights.backdrops() {
            if let Some((emitted, _)) = backdrop.escaped(Vec3A::ZERO, direction) {
                radiance += emitted * strategy.unopposed_weight();
            }
        }
        return radiance;
    }
    if lights.count() == 0 {
        return radiance;
    }
    // As on the bounce-hit path, a delta or non-evaluable previous vertex
    // means NEE could not have found this light, so there is no competing
    // strategy and the emission is taken whole.
    let competing = match prev {
        Some(PrevVertex::Surface(p)) => p.continuous().then_some((p.pos, p.pdf)),
        Some(PrevVertex::Phase { pos, pdf, .. }) => Some((*pos, *pdf)),
        // Primary rays, and rays leaving a carried-medium scatter, run no
        // NEE — full weight, exactly as `prev = None` means elsewhere.
        None => None,
    };
    // The receiver whose light links apply: the surface or volume region
    // the ray left. The camera has none, and sees every light.
    let class = match prev {
        Some(PrevVertex::Surface(p)) => p.class,
        Some(PrevVertex::Phase { class, .. }) => *class,
        None => EVERY_CLASS,
    };
    let from = competing.map_or(Vec3A::ZERO, |(p, _)| p);
    // The pmf at the vertex the escaping ray left: the one its NEE picked with.
    // Only lights at infinity can answer `escaped`; the rest are skipped.
    for (index, light, pmf) in lights.infinite_indexed_seen_by(from, mask) {
        if !lights.illuminates(index, class) {
            continue;
        }
        // NEE's alone at a continuous vertex (see `bounce_emission_weight`).
        if competing.is_some() && lights.nee_only(index) && strategy.samples_lights() {
            continue;
        }
        let Some((emitted, pdf)) = light.escaped(from, direction) else {
            continue;
        };
        let weight = match (competing, pdf) {
            (Some((_, bounce_pdf)), Some(pdf)) if strategy.samples_lights() && pmf > 0.0 => {
                let light_pdf = lights.density(pdf, pmf);
                strategy.bounce_weight(bounce_pdf, light_pdf)
            }
            // No NEE ran for this vertex, the strategy does not sample lights
            // at all, the selection never picks this one, or the light never
            // samples this direction: nothing competes.
            _ => strategy.unopposed_weight(),
        };
        radiance += emitted * weight;
    }
    radiance
}

/// NEE shadow test used at surface and volume vertices alike: ZERO when the
/// surfaces block the segment, otherwise what they let through
/// ([`surface_visibility`]: 1, or a cutout's share) times the volumetric
/// transmittance through every region it crosses (stochastic for
/// heterogeneous regions, exact for homogeneous ones). MIS weights are
/// unaffected — transmittance is part of the integrand on both strategies,
/// not of either pdf.
fn shadow_transmittance<const PROFILE: bool>(
    ctx: &PathContext,
    shadow_ray: &Ray,
    distance: f32,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    let _p = profile::scope_if::<PROFILE>(Section::Occlusion);
    stats.shadow_rays += 1;
    let through = surface_visibility(ctx.world, shadow_ray, shadow_t_max(distance), stats);
    if through == 0.0 {
        stats.shadow_occluded += 1;
        return Vec3A::ZERO;
    }
    if ctx.volumes.is_empty() {
        return Vec3A::splat(through);
    }
    let mut rng = vertex.new_domain(K_NEE_SHADOW).rng();
    through
        * ctx
            .volumes
            .transmittance(shadow_ray, TRACE_T_MIN, distance - TRACE_T_MIN, &mut rng)
}

/// How many cutouts one segment is followed through, on either side: past
/// this a path treats the next hit as present, and a shadow ray as blocked.
/// Generous — a stack of leaf cards seen edge-on is tens deep — and only
/// there so a pathological stack cannot stall a sample.
const MAX_CUTOUT_CROSSINGS: usize = 256;

/// `ray` restarted at its own parameter `t`, in the same direction, with the
/// cone as wide as it has grown by then: a hit on the result at `t'` is the
/// hit on `ray` at `t + t'`.
///
/// Stepping past a hit moves the origin rather than raising `t_min`, as the
/// subsurface walk's rays do: every `World::intersect` asks for
/// `(TRACE_T_MIN, ∞)`, LLVM propagates those two constants into the kernel, and a
/// caller asking for other bounds costs every ray in every scene. The
/// origin goes to [`resume_before`] the hit, not onto it.
fn restarted(ray: &Ray, t: f32) -> Ray {
    let cone = ray.cone();
    Ray::new(ray.at(t), ray.direction())
        .with_time(ray.time())
        .with_mask(ray.mask())
        .with_cone(crate::RayCone {
            width: cone.width_at(t * ray.direction().length()),
            spread: cone.spread,
        })
}

/// Where to restart a segment that passes a hit at `t` (see [`restarted`]):
/// short of it by [`TRACE_T_MIN`], less a relative step, so the restarted
/// ray's `(TRACE_T_MIN, ∞)` begins just *past* the hit. Restarted on the hit
/// itself, the offset stepped over any surface within 0.001 behind it — a
/// decal or card layered over opaque geometry leaked light through
/// (`a_surface_just_behind_a_cutout_is_not_skipped`). The step is relative,
/// so a far hit is not met again through rounding.
#[inline]
fn resume_before(t: f32) -> f32 {
    t - TRACE_T_MIN + t.abs().max(1.0) * 1e-5
}

/// Where a shadow ray toward a light sample `distance` away stops: short of
/// the light's own surface, which is in the shadow mask (lights occlude each
/// other). [`TRACE_T_MIN`] (0.001), or a relative step once that falls below the
/// rounding of `distance` — at 3·10⁵ (the Moana island's sun quad) an `f32`
/// ulp is 0.03, `distance − 0.001 == distance`, and the ray met the light it
/// was aimed at most of the time. The step is 1e-6 ≈ 8 ulps: 4 was measured
/// to be the least that clears an axis-aligned or a tilted quad at 3·10² to
/// 3·10⁶, and every unit more lets a blocker that close to the light through.
/// Below 1000 the 0.001 is the larger step, so every nearer light gets
/// exactly the bound it always had.
///
/// Surfaces only: volume transmittance has no light surface to stop short of
/// and keeps `distance − 0.001`, which reaches the light at any distance.
#[inline]
pub(crate) fn shadow_t_max(distance: f32) -> f32 {
    (distance - TRACE_T_MIN).min(distance * (1.0 - 1e-6))
}

/// The share of light the surfaces along `ray`'s `(TRACE_T_MIN, t_max)` let
/// through: 1 for an open segment, 0 when it is blocked, and in a world with
/// cutouts, for a segment the any-hit query found blocked, `Π (1 − opacity)`
/// over every hit ([`cutout_through`]). Deterministic where the bounce side is
/// stochastic ([`pass_cutouts`]): both estimate the same visibility, and the
/// product is the lower-variance of the two.
///
/// The one visibility shadow rays see: NEE's ([`shadow_transmittance`]) and
/// the learned light cache's training, whose shadow rays must see what the
/// integrator does. Neither counts a shadow ray here; `stats` gets only the
/// cutout walk's rays.
#[inline(always)]
pub(crate) fn surface_visibility(
    world: &World,
    ray: &Ray,
    t_max: f32,
    stats: &mut RayStats,
) -> f32 {
    // Dedicated occlusion query: any hit in range means full shadow, so the
    // early-exit traversal beats searching for the closest hit.
    if !world.occluded(ray, TRACE_T_MIN, t_max) {
        return 1.0;
    }
    // Blocked — unless only cutouts block it, which the any-hit query cannot
    // tell apart. An open segment crosses no cutout either, so it keeps the
    // fast answer.
    if !world.has_cutouts() {
        return 0.0;
    }
    cutout_through(world, ray, t_max, stats)
}

/// The fraction of the segment `(TRACE_T_MIN, t_max)` of `ray` that cutouts let
/// through: `Π (1 − opacity)` over every hit, or 0 at the first hit on a
/// material without a cutout. It follows at most [`MAX_CUTOUT_CROSSINGS`]
/// cutouts and then asks once more, where any hit blocks — the same bound
/// [`pass_cutouts`] keeps, which treats the hit past its last crossing as
/// present, so a stack exactly that deep is clear on both sides.
///
/// The cold half of [`surface_visibility`], out of line so a world without
/// cutouts carries none of it.
#[cold]
#[inline(never)]
fn cutout_through(world: &World, ray: &Ray, t_max: f32, stats: &mut RayStats) -> f32 {
    let (mut t, mut segment) = (0.0, ray.clone());
    let mut kept = 1.0;
    for crossing in 0..=MAX_CUTOUT_CROSSINGS {
        stats.cutout_rays += 1;
        let hit = world.intersect(&segment, TRACE_T_MIN, f32::INFINITY);
        let Some(h) = hit.filter(|h| t + h.rec.t < t_max) else {
            return kept;
        };
        if crossing == MAX_CUTOUT_CROSSINGS || !h.mat.has_cutout() {
            return 0.0;
        }
        kept *= 1.0 - h.mat.opacity(ray, &point_sampled(&h.rec));
        if kept <= 0.0 {
            return 0.0;
        }
        t = resume_before(t + h.rec.t);
        segment = restarted(ray, t);
    }
    unreachable!("the last crossing returns")
}

/// `rec` with no texture footprint: opacity is point-sampled on both sides.
///
/// A path meets a cutout with a ray cone and a shadow ray has none, so a
/// filtered opacity would answer the bounce side from a coarser mip level
/// than NEE for the same connection, and the two MIS strategies would
/// estimate different visibilities. Point sampling is also the geometry
/// itself: a mip level of an alpha mask is a blurred mask, where
/// stochastic presence already averages the real one over the pixel.
#[inline]
fn point_sampled(rec: &HitRecord) -> HitRecord {
    HitRecord {
        uv_width: 0.0,
        face_width: 0.0,
        ..*rec
    }
}

/// Makes `hit`, a segment's closest hit, the one it actually ends at: each
/// hit on a cutout is met with its opacity's probability and otherwise
/// passed through, to the next hit along the same line, until one is met or
/// the segment escapes. The hit's `t` stays measured along `ray`, so the
/// carried medium, the volume regions and the cone all still measure from
/// the segment's origin. A surface passed through is no vertex: it spends no
/// depth, emits nothing and leaves the previous vertex's MIS record to
/// whatever the segment does reach.
///
/// Its shadow-side twin is [`surface_visibility`].
#[cold]
#[inline(never)]
fn pass_cutouts<'w>(
    world: &'w World,
    ray: &Ray,
    hit: &mut Option<WorldHit<'w>>,
    vertex: PathSampler,
    stats: &mut RayStats,
) {
    let mut rng = None;
    for _ in 0..MAX_CUTOUT_CROSSINGS {
        let Some(h) = hit.as_ref() else {
            return;
        };
        if !h.mat.has_cutout() {
            break;
        }
        let opacity = h.mat.opacity(ray, &point_sampled(&h.rec));
        if opacity >= 1.0 {
            break;
        }
        let u = rng
            .get_or_insert_with(|| vertex.new_domain(K_CUTOUT).rng())
            .next_f32();
        if u < opacity {
            break;
        }
        stats.cutout_passes += 1;
        stats.cutout_rays += 1;
        let t = resume_before(h.rec.t);
        *hit = world
            .intersect(&restarted(ray, t), TRACE_T_MIN, f32::INFINITY)
            .map(|mut next| {
                next.rec.t += t;
                next
            });
    }
}

/// A volume-region scatter point, as [`volume_nee`] lights it.
struct VolumeReceiver<'p> {
    p: Vec3A,
    /// The arriving direction, normalised: the phase function's `wi`.
    wi: Vec3A,
    phase: &'p PhaseMix,
    /// The light-link class of the region(s) scattered in.
    class: u16,
    /// The path's shutter time, for the shadow ray.
    time: f32,
}

/// A surface vertex, as [`surface_nee`] lights it and [`Walk::bounce`]
/// leaves it.
struct SurfaceReceiver<'s> {
    rec: &'s HitRecord,
    sp: &'s ShadingPoint<'s>,
    /// The receiver's light-link class.
    class: u16,
    /// The guiding field when it steers this vertex's bounce (secondary
    /// vertices only): NEE's competing density is then the guide mixture.
    guiding: Option<&'s GuidingContext<'s>>,
    /// The arriving cone's perpendicular cross-section here, which the
    /// bounce's cone starts from.
    cone_width: f32,
}

/// Direct lighting at a volume-region scatter point. The exact mirror of
/// the surface NEE block: same light-selection strategy, with the
/// phase function (value == pdf for the HG mixture) in place of
/// `brdf·cos`, and the same phase pdf as the competing bounce density that
/// `bounce_emission_weight`'s `Phase` arm uses.
fn volume_nee<const PROFILE: bool>(
    ctx: &PathContext,
    at: &VolumeReceiver,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    let &VolumeReceiver {
        p,
        wi,
        phase,
        class,
        time,
    } = at;
    let (lights, strategy) = (ctx.lights, ctx.strategy);
    if !strategy.samples_lights() {
        return Vec3A::ZERO;
    }
    let _p = profile::scope_if::<PROFILE>(Section::VolumeLighting);
    let nee = vertex.new_domain(K_NEE).draw_sample_f32::<4>();
    // As at a surface: a light that does not illuminate this region gives
    // nothing, and its pick probability stays what it was.
    let Some((index, pmf)) = lights.pick_index_at(p, nee[0]) else {
        return Vec3A::ZERO;
    };
    if !lights.illuminates(index, class) {
        return Vec3A::ZERO;
    }
    let light = lights.light(index);
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
        .with_mask(lights.shadow_mask(index));
    let tr = shadow_transmittance::<PROFILE>(ctx, &shadow_ray, s.distance, vertex, stats);
    if tr == Vec3A::ZERO {
        return Vec3A::ZERO;
    }
    let light_pdf = lights.density(s.pdf, pmf);
    // The phase function is its own pdf, in solid angle. A shadow-linked
    // light has no competing bounce strategy (see `bounce_emission_weight`).
    let weight = if lights.nee_only(index) {
        1.0
    } else {
        strategy.light_weight(light_pdf, PdfSolidAngle::from_measure(phase_val))
    };
    s.radiance * phase_val * tr * weight / light_pdf.get()
}

/// Direct lighting at a surface vertex: one light picked by the light
/// list's selection, one point sampled on it, and the connection weighted by
/// MIS against the bounce sampler. The surface twin of [`volume_nee`].
///
/// `inline(always)` like the other once-per-`trace_path` helpers: it is part
/// of every surface vertex, and out of line it is a call the integrator did
/// not pay before it was extracted.
#[inline(always)]
fn surface_nee<const PROFILE: bool>(
    ctx: &PathContext,
    ray: &Ray,
    at: &SurfaceReceiver,
    v: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    let &SurfaceReceiver {
        rec,
        sp,
        class: class_here,
        guiding: guiding_here,
        ..
    } = at;
    let (lights, strategy) = (ctx.lights, ctx.strategy);
    // The light strategy is "pick one light with the light list's
    // selection probability `pmf` (power-proportional by default), then
    // sample a point on it with its own `sample_li`", so its solid-angle
    // density is `light.pdf · pmf`. `bounce_emission_weight` evaluates
    // the same expression for a bounce-hit light — both MIS weights must
    // describe the same strategy or emission is double-counted.
    let mut nee = Vec3A::ZERO;
    let _p = profile::scope_if::<PROFILE>(Section::SurfaceLighting);
    let nee_s = v.new_domain(K_NEE).draw_sample_f32::<4>();
    // `sample_li` returns `None` when the light cannot be reached from
    // this point at all — below a dome's horizon, or a degenerate
    // coincident point.
    // A picked light that does not illuminate this receiver contributes
    // nothing, and its pick probability stays what it was, so the bounce
    // side, which zeroes the same light, still describes one strategy.
    if let Some((light_index, pmf)) = strategy
        .samples_lights()
        .then(|| lights.pick_index_at(rec.p, nee_s[0]))
        .flatten()
        && lights.illuminates(light_index, class_here)
        && let Some(ls) = lights
            .light(light_index)
            .sample_li(rec.p, nee_s[1], nee_s[2])
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
        // Either order is bit-identical: a skipped test's contribution would be exactly
        // zero, and the shadow ray's own draws come from `K_NEE_SHADOW`,
        // which nothing else reads.
        let mut visibility = || {
            let shadow_ray = Ray::new(rec.p, light_dir_unit)
                .with_time(ray.time())
                .with_mask(lights.shadow_mask(light_index));
            let tr = shadow_transmittance::<PROFILE>(ctx, &shadow_ray, ls.distance, v, stats);
            (tr != Vec3A::ZERO).then_some(tr)
        };
        let connection = if ls.radiance == Vec3A::ZERO {
            None
        } else {
            sp.eval(ray, light_dir_unit)
                .filter(|(f, _)| ls.radiance * *f != Vec3A::ZERO)
                .and_then(|(f, pdf)| visibility().map(|tr| (f, pdf, tr)))
        };
        if let Some((brdf_value, brdf_pdf, shadow_tr)) = connection {
            let light_pdf = lights.density(ls.pdf, pmf);
            // The competing strategy for this MIS weight is the
            // bounce sampler, whose density toward the light is the
            // guide/BSDF mixture whenever guiding is available at
            // this vertex — using the plain BSDF pdf here while the
            // bounce side weights with the mixture makes the two
            // weights sum past one and double-counts emission.
            let bounce_pdf = PdfSolidAngle::from_measure(match guiding_here {
                Some(g) if g.field.trained_at(rec.p) => g
                    .field
                    .mixture_pdf(g.field.pdf(rec.p, light_dir_unit), brdf_pdf),
                _ => brdf_pdf,
            });
            // A shadow-linked light is NEE's alone here: the bounce side
            // collects none of it at a continuous vertex.
            let weight = if lights.nee_only(light_index) {
                1.0
            } else {
                strategy.light_weight(light_pdf, bounce_pdf)
            };
            // `brdf_value` already carries the geometric cosine —
            // `Material::eval` returns `brdf · |cos|` (unsigned, so a
            // continuous transmission lobe can see a light behind the
            // ray-facing normal) — so none is applied here: the bounce
            // side's `value / pdf` carries exactly one cosine too.
            nee += ls.radiance * brdf_value * shadow_tr * weight / light_pdf.get();
        }
    }
    nee
}

/// The integrator: an iterative path tracer in two passes. The forward walk
/// traces one segment per bounce (each hit serves both as the previous
/// vertex's potential light hit and as the next vertex, so every segment is
/// intersected once), records a `VertexRec` per vertex, and
/// applies Russian roulette past `RR_START_BOUNCE`. The backward gather
/// then folds the records into the radiance estimate and emits guiding
/// training samples, which need the radiance arriving from the rest of the
/// path and therefore cannot be computed forward.
///
/// Each vertex runs the same phases in the same order: trace the arriving
/// segment, let the carried medium and the volume
/// regions compete for it ([`free_flight`], [`volume_event`]), and then
/// whichever event won — a volume-region scatter
/// ([`Walk::scatter_in_volume`]), a carried-medium scatter
/// ([`Walk::scatter_in_medium`]), an escape ([`Walk::escaped`]) or a surface
/// ([`Walk::surface_vertex`], whose continuation is [`Walk::bounce`]). The
/// phases are `inline(always)`, so this is still one loop to LLVM.
///
/// Forced inline into `render_pixel` (and the `ray_color` wrapper tests call).
/// LLVM inlined it on its
/// own until the cutout branches tipped it over the threshold, and out of
/// line it costs cornellbox 1.4% of its instructions (callgrind, 2 spp);
/// forced, the tree before cutouts measured 0.4% *fewer*.
#[inline(always)]
pub(super) fn trace_path<const PROFILE: bool>(
    ctx: &PathContext,
    r: &Ray,
    sampler: PathSampler,
    train_out: &mut Vec<SampleData>,
    scratch: &mut PathScratch,
    stats: &mut RayStats,
) -> Vec3A {
    // A copy the walk's phases borrow: its fields are then this function's
    // own locals to LLVM, which keeps them in registers across the calls the
    // loop makes rather than reloading them through the caller's (0.05% of
    // cornellbox's instructions).
    let ctx = &PathContext { ..*ctx };
    // The bounce subtree; each vertex derives its own domain off this by depth.
    let path = sampler.new_domain(K_PATH);
    let mut ray = r.clone();
    let mut walk = Walk::start(ctx, &mut ray, scratch);
    // Radiance entering the path from beyond the last vertex.
    let mut terminal = Vec3A::ZERO;

    loop {
        // This vertex's domain: `records.len()` is the vertex index (nothing
        // has been pushed for it yet). Every per-event draw hangs off `v`.
        let v = path.new_domain(walk.records.len() as i32);

        if walk.remaining <= 0 {
            walk.end_at_depth::<PROFILE>(ctx, v, stats);
            break;
        }

        // === The arriving segment ===
        // The hit it ends at, if any: the closest past every cutout it
        // passes — or, when the last vertex entered a random walk, the
        // walk's exit, which needs no segment traced at all. Built here
        // rather than returned by a `Walk` method: returned, the hit was
        // copied out at every vertex (+0.6% of cornellbox's instructions).
        let exiting = walk.sss_pending;
        let mut hit_opt = if exiting {
            walk.sss_pending = false;
            walk.sss_exit.hit()
        } else {
            stats.closest_hit += 1;
            let _p = profile::scope_if::<PROFILE>(Section::Trace);
            ctx.world.intersect(walk.ray, TRACE_T_MIN, f32::INFINITY)
        };
        // Patched in place, on the cold side only: an `if` that yields the
        // hit from either arm copies all of it at every vertex (+0.8% of
        // cornellbox's instructions, which has no cutout).
        if ctx.world.has_cutouts() && !exiting {
            pass_cutouts(ctx.world, walk.ray, &mut hit_opt, v, stats);
        }
        let t_surf = hit_opt.as_ref().map_or(f32::INFINITY, |h| h.rec.t);
        let t_med = free_flight(walk.ray, v);
        let event = volume_event::<PROFILE>(ctx, walk.ray, t_surf.min(t_med), exiting, v);

        // === Volume-region and carried-medium scatter vertices ===
        let segment = match event {
            VolumeEvent::Scatter {
                p,
                weight,
                phase,
                emitted,
                class,
                ..
            } => {
                let receiver = VolumeReceiver {
                    p,
                    wi: walk.ray.direction().normalize(),
                    phase: &phase,
                    class,
                    time: walk.ray.time(),
                };
                if walk.scatter_in_volume::<PROFILE>(ctx, &receiver, weight, emitted, v, stats) {
                    continue;
                }
                break;
            }
            VolumeEvent::Passthrough {
                transmittance,
                emitted,
            } => Segment {
                transmittance,
                emitted,
            },
        };
        if t_med < t_surf {
            if walk.scatter_in_medium(t_med, &segment, v, stats) {
                continue;
            }
            break;
        }

        // === Escape, or a surface vertex ===
        let Some(hit) = hit_opt else {
            stats.ended_escaped += 1;
            terminal = walk.escaped(ctx, &segment);
            break;
        };
        if !walk.surface_vertex::<PROFILE>(ctx, &hit, &segment, v, stats) {
            break;
        }
    }

    gather::<PROFILE>(walk.records, terminal, ctx.indirect_clamp, train_out)
}

/// What the segment arriving at a vertex did on its way there, besides any
/// carried medium: the volume regions' transmittance (the tracking weight up
/// to the vertex) and their emission along it, already weighted by that walk.
struct Segment {
    transmittance: Vec3A,
    emitted: Vec3A,
}

/// The forward walk's state between vertices: the ray the next vertex
/// arrives along, what the last vertex left for it, and where the records
/// go. A local of [`trace_path`] whose methods are all `inline(always)`, so
/// its fields stay the loop's own variables to LLVM.
struct Walk<'a, 's> {
    /// The ray the next vertex arrives along. Borrowed from `trace_path`
    /// rather than owned: its address escapes into the intersection kernel
    /// at every vertex, and LLVM keeps a struct whose field's address
    /// escapes in memory whole — owned here, the other fields were stored
    /// and reloaded at every vertex (+0.05% of cornellbox's instructions,
    /// +0.07% of fog's).
    ray: &'s mut Ray,
    /// Path depth left to spend.
    remaining: i32,
    /// Set after surface bounces and volume-region phase scatters; `None`
    /// at the primary vertex and after carried-medium (subsurface)
    /// scatters, where the next vertex's emission counts fully.
    prev: Option<PrevVertex<'a>>,
    /// Running throughput. Only drives the roulette survival probability —
    /// the estimate itself is rebuilt by the backward gather.
    beta: Vec3A,
    /// Set when the last vertex entered a random walk: the next vertex is
    /// its exit, shaded without tracing the segment that reaches it (the walk
    /// already did, inside the object). A flag beside a slot rather than an
    /// `Option<WalkExit>`: taking a 150-byte option at every vertex cost
    /// cornellbox, which never walks, 1% of its instructions.
    sss_pending: bool,
    /// Record guiding training samples (a training pass)?
    training: bool,
    /// One per vertex, read by the backward gather. Borrowed, not allocated —
    /// see [`PathScratch`].
    records: &'s mut Vec<VertexRec>,
    sss_exit: &'s mut PendingExit,
}

impl<'a, 's> Walk<'a, 's> {
    /// A walk about to trace the camera ray `ray`.
    #[inline(always)]
    fn start(ctx: &PathContext, ray: &'s mut Ray, scratch: &'s mut PathScratch) -> Self {
        // Capacity carries over from the previous sample, so after the first
        // walk this is free.
        scratch.records.clear();
        Walk {
            ray,
            remaining: ctx.depth,
            prev: None,
            beta: Vec3A::ONE,
            sss_pending: false,
            training: ctx.guiding.is_some_and(|g| g.training),
            records: &mut scratch.records,
            sss_exit: &mut scratch.sss_exit,
        }
    }

    /// Ends this vertex with `vrec`: the path holds one more vertex.
    #[inline(always)]
    fn record(&mut self, vrec: VertexRec, stats: &mut RayStats) {
        stats.vertices += 1;
        self.records.push(vrec);
    }

    /// Depth exhausted: no further vertex, but the last bounce still
    /// collects the emission of the surface it hits, MIS-weighted and
    /// attenuated through any media the final segment crosses. A ray
    /// escaping here collects nothing from lights at infinity.
    #[inline(always)]
    fn end_at_depth<const PROFILE: bool>(
        &mut self,
        ctx: &PathContext,
        v: PathSampler,
        stats: &mut RayStats,
    ) {
        stats.ended_depth += 1;
        let Some(p) = &self.prev else {
            return;
        };
        let (world, ray) = (ctx.world, &*self.ray);
        stats.closest_hit += 1;
        let mut hit = {
            let _p = profile::scope_if::<PROFILE>(Section::Trace);
            world.intersect(ray, TRACE_T_MIN, f32::INFINITY)
        };
        if world.has_cutouts() {
            pass_cutouts(world, ray, &mut hit, v, stats);
        }
        if let Some(hit) = hit {
            let cos_o = ray.direction().normalize().dot(hit.rec.normal).abs();
            let mut emitted = hit.mat.emitted_at(ray, &hit.rec, cos_o);
            if emitted.length_squared() > 0.0 {
                if let Some(m) = ray.medium() {
                    emitted *= m.transmittance(hit.rec.t);
                }
                if !ctx.volumes.is_empty() {
                    let mut rng = v.new_domain(K_VOLUME).rng();
                    emitted *= ctx
                        .volumes
                        .transmittance(ray, TRACE_T_MIN, hit.rec.t, &mut rng);
                }
                let last = self.records.last_mut().expect("prev implies a record");
                last.next_emit = emitted;
                last.next_emit_weight = bounce_emission_weight(p, ctx.lights, &hit, ctx.strategy);
            }
        }
    }

    /// A volume-region scatter vertex at `at`, reached with the tracking
    /// walk's `weight` and its `emitted` light: NEE through the phase
    /// function, then a phase-sampled continuation. `false` when roulette
    /// ends the path here.
    #[inline(always)]
    fn scatter_in_volume<const PROFILE: bool>(
        &mut self,
        ctx: &PathContext,
        at: &VolumeReceiver,
        weight: Vec3A,
        emitted: Vec3A,
        v: PathSampler,
        stats: &mut RayStats,
    ) -> bool {
        stats.volume_scatters += 1;
        let ps = v.new_domain(K_PHASE).draw_sample_f32::<4>();
        let dir = at.phase.sample(at.wi, ps[0], [ps[1], ps[2]]);
        let phase_pdf = at.phase.pdf(at.wi.dot(dir)).max(1e-6);
        let nee = volume_nee::<PROFILE>(ctx, at, v, stats);

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
        self.beta *= weight;
        if !roulette(
            &mut self.beta,
            &mut vrec.factor,
            self.records.len(),
            v,
            stats,
        ) {
            self.record(vrec, stats);
            return false;
        }
        self.prev = Some(PrevVertex::Phase {
            pos: at.p,
            pdf: PdfSolidAngle::from_measure(phase_pdf),
            class: at.class,
        });
        self.record(vrec, stats);
        *self.ray = phase_scattered(self.ray, at.p, dir);
        self.remaining -= 1;
        true
    }

    /// A carried-medium scatter vertex (subsurface interiors) at `t_med`
    /// along the segment. `false` when roulette ends the path here.
    #[inline(always)]
    fn scatter_in_medium(
        &mut self,
        t_med: f32,
        segment: &Segment,
        v: PathSampler,
        stats: &mut RayStats,
    ) -> bool {
        stats.medium_scatters += 1;
        let ray = &*self.ray;
        let medium = *ray.medium().expect("t_med implies a medium");
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
        let factor = medium.sigma_s / sigma_bar * chromatic_correction(&medium, sigma_bar, t_med);
        // Subsurface vertices run no NEE (their shadow rays are
        // blocked by the enclosing surface), so `prev = None` keeps
        // the next hit's emission at full weight — the pairing that
        // avoids double counting.
        let mut vrec = VertexRec {
            atten: segment.transmittance,
            segment_emit: segment.emitted,
            emit_here: Vec3A::ZERO,
            nee: Vec3A::ZERO,
            factor,
            next_emit: Vec3A::ZERO,
            next_emit_weight: 1.0,
            train: None,
        };
        self.beta *= segment.transmittance * factor;
        if !roulette(
            &mut self.beta,
            &mut vrec.factor,
            self.records.len(),
            v,
            stats,
        ) {
            self.record(vrec, stats);
            return false;
        }
        self.record(vrec, stats);
        // Still inside `medium`: the ray it scattered in carries it.
        *self.ray = phase_scattered(self.ray, pos, dir);
        self.remaining -= 1;
        self.prev = None;
        true
    }

    /// The radiance a ray leaving the scene brings back: whatever lights at
    /// infinity it finds, through the final segment.
    #[inline(always)]
    fn escaped(&self, ctx: &PathContext, segment: &Segment) -> Vec3A {
        // A ray leaving the scene is how lights at infinity are found
        // by chance, so it is a bounce-side MIS event just like hitting
        // an emissive surface.
        let unit_direction = Vec3A::normalize(self.ray.direction());
        let background = escaped_emission(
            &self.prev,
            ctx.lights,
            unit_direction,
            self.ray.mask(),
            ctx.strategy,
        );
        // Segment emission is already weighted; the background pays the
        // volume transmittance of the final segment.
        segment.emitted + segment.transmittance * background
    }

    /// A surface vertex at `hit`: the arriving segment's attenuation, the
    /// emission found here, NEE, and then the [`bounce`](Self::bounce) that
    /// continues the path. `false` when the path ends here.
    #[inline(always)]
    fn surface_vertex<const PROFILE: bool>(
        &mut self,
        ctx: &PathContext,
        hit: &WorldHit<'a>,
        segment: &Segment,
        v: PathSampler,
        stats: &mut RayStats,
    ) -> bool {
        let ray = &*self.ray;
        let rec: HitRecord = hit.rec;
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
                chromatic_correction(m, sigma_bar, rec.t)
            }
            Some(m) => m.transmittance(rec.t),
            None => Vec3A::ONE,
        };
        let atten = segment.transmittance * med_arrival;

        // Emission accounting: a vertex reached by a bounce hands its
        // emission to the previous vertex's record, MIS-weighted —
        // counting it here too would double it. At the primary vertex and
        // after carried-medium scatters it counts here, in full. Either
        // way the emission pays the arriving segment's attenuation (an
        // emitter seen through tinted glass or smoke must dim).
        let cos_o = ray.direction().normalize().dot(rec.normal).abs();
        // The receiver's light-link class, for NEE here and for whatever the
        // bounce leaving this vertex collects.
        let class_here = ctx.world.light_class(hit.geom_id);
        // The material's per-hit work (pattern network, textures), done once
        // for every query at this vertex: the emission here, NEE's `eval`, the
        // scatter, and guiding's `eval` / `make_ray`. Every surface vertex
        // scatters, so this is never wasted work.
        let sp = {
            let _p = profile::scope_if::<PROFILE>(Section::EvalBsdfs);
            ShadingPoint::new(hit.mat, ray, &rec, cos_o)
        };
        let emitted = sp.emitted();
        let mut emit_here = Vec3A::ZERO;
        match &self.prev {
            Some(p) => {
                if emitted.length_squared() > 0.0 {
                    let last = self.records.last_mut().expect("prev implies a record");
                    last.next_emit = atten * emitted;
                    last.next_emit_weight =
                        bounce_emission_weight(p, ctx.lights, hit, ctx.strategy);
                }
            }
            None => emit_here = emitted,
        }

        // Guide secondary bounces only: primary vertices vary per pixel far
        // below the guiding field's spatial resolution, so guiding them adds
        // parallax-mismatch variance instead of removing any.
        let guiding_here = if self.prev.is_some() {
            ctx.guiding
        } else {
            None
        };

        // === 1. Direct Lighting via Light Sampling ===
        let receiver = SurfaceReceiver {
            rec: &rec,
            sp: &sp,
            class: class_here,
            guiding: guiding_here,
            cone_width: cone_width_here,
        };
        let nee = surface_nee::<PROFILE>(ctx, self.ray, &receiver, v, stats);

        let vrec = VertexRec {
            atten,
            segment_emit: segment.emitted,
            emit_here,
            nee,
            factor: Vec3A::ZERO,
            next_emit: Vec3A::ZERO,
            next_emit_weight: 1.0,
            train: None,
        };
        self.bounce::<PROFILE>(ctx, hit, &receiver, vrec, v, stats)
    }

    /// The continuation of the surface vertex `at` (on `hit`, whose record so
    /// far is `vrec`): sample the bounce — walking the interior first when a
    /// subsurface lobe was picked — record the vertex and, unless the sample
    /// is absorbed or roulette kills it, aim the walk along it. `false` when
    /// the path ends here.
    #[inline(always)]
    fn bounce<const PROFILE: bool>(
        &mut self,
        ctx: &PathContext,
        hit: &WorldHit<'a>,
        at: &SurfaceReceiver,
        mut vrec: VertexRec,
        v: PathSampler,
        stats: &mut RayStats,
    ) -> bool {
        let rec = at.rec;
        // === 2. Indirect Lighting via BSDF (or guided) Sampling ===
        let mut bounce = {
            let _p = profile::scope_if::<PROFILE>(Section::Bounce);
            sample_bounce_direction(self.ray, rec, at.sp, at.guiding, v)
        };
        // A subsurface leaf was selected: walk the interior now. The walk is
        // part of this surface event — the entry's record carries its weight
        // and the exit is the next vertex — so it spends no path depth.
        // (Replaced only when it walks: passing the sample through a `match`
        // or a rebinding moves all of it at every vertex.)
        if let Some(sample) = bounce.take_if(|s| s.subsurface.is_some()) {
            // A `--profile` section like `Trace` or `EvalBsdfs`, one per walk
            // rather than per ray: monomorphised on `PROFILE`, it compiles
            // away when profiling is off (cornellbox: +0.0002% instructions),
            // and `--stats` keeps its integer counters (`sss_*`) either way.
            let _p = profile::scope_if::<PROFILE>(Section::Subsurface);
            bounce = walk_subsurface(
                ctx.world,
                hit,
                self.ray,
                at.sp,
                sample,
                v,
                self.sss_exit,
                stats,
            );
            self.sss_pending = bounce.is_some();
        }
        if bounce.is_none() {
            stats.ended_absorbed += 1;
        }
        if let Some(sample) = bounce {
            let dir = sample.ray.direction().normalize();
            // `sample.value` is the material's `brdf · |cos|` (delta lobes
            // carry their whole throughput there instead), so the estimator is
            // just `value / pdf`, with no cosine of its own — as on the NEE
            // side. `a_diffuse_ball_in_a_white_furnace_reflects_albedo_times_radiance`
            // pins it.
            let mut factor = sample.value / sample.pdf;

            // Russian roulette on the continuation: survive with probability
            // tracking the throughput, dividing it out on survival. Applies
            // to the whole continuation (bounce-hit emission included).
            self.beta *= vrec.atten * factor;
            if roulette(&mut self.beta, &mut factor, self.records.len(), v, stats) {
                // Training samples cover continuous surface bounces only —
                // the guide can never produce a delta direction. The
                // radiance is filled in by the backward gather.
                if self.training && !sample.delta {
                    vrec.train = Some(TrainRec {
                        pos: rec.p,
                        dir,
                        cos: rec.normal.dot(dir).abs(),
                    });
                }
                vrec.factor = factor;
                let cone_width_here = at.cone_width;
                if self.sss_pending {
                    // The exit vertex sees the walk arrive from outside,
                    // along its last direction; what it emits there leaves
                    // the object, so nothing is owed to this record.
                    self.record(vrec, stats);
                    self.prev = None;
                    let exit = &*self.sss_exit;
                    *self.ray = Ray::new(exit.rec.p, -exit.dir)
                        .with_time(self.ray.time())
                        .with_mask(crate::ray::MASK_INDIRECT)
                        .with_cone(
                            self.ray
                                .cone()
                                .scattered(cone_width_here, crate::RayCone::MAX_SPREAD),
                        );
                    return true;
                }
                self.prev = Some(PrevVertex::Surface(PrevBounce {
                    pos: rec.p,
                    // A BSDF (or guide-mixture) pdf, in solid angle.
                    pdf: PdfSolidAngle::from_measure(sample.pdf),
                    delta: sample.delta,
                    class: at.class,
                    #[cfg(debug_assertions)]
                    check: (self.ray.clone(), *rec, hit.mat, dir),
                    #[cfg(not(debug_assertions))]
                    _mat: std::marker::PhantomData,
                }));
                self.record(vrec, stats);
                // Materials build the scattered ray without path context;
                // stamp the path's shutter time, the indirect category, and
                // the texture-filtering cone this bounce leaves behind. The
                // cone starts at the width it arrived with — the
                // perpendicular cross-section, *not* the grazing-stretched
                // footprint `World::intersect` handed the shader — and picks
                // up the sampled lobe's own angular width.
                *self.ray = sample
                    .ray
                    .with_time(self.ray.time())
                    .with_mask(crate::ray::MASK_INDIRECT)
                    .with_cone(self.ray.cone().scattered(cone_width_here, sample.spread));
                self.remaining -= 1;
                return true;
            }
        }

        // Absorbed or roulette-killed: this vertex's own gathers stand
        // (factor stays zero), the path ends here.
        self.record(vrec, stats);
        false
    }
}

/// Free-flight candidate in the carried homogeneous medium (subsurface /
/// participating glass interiors) — analog sampling at the extinction
/// majorant; infinite outside a scattering medium. Incidental (unbounded
/// across bounces), so it uses the PRNG side rather than a stratified
/// dimension.
#[inline(always)]
fn free_flight(ray: &Ray, v: PathSampler) -> f32 {
    match ray.medium() {
        Some(m) if m.is_scattering() => {
            let sigma_t_max = m.sigma_t_max().max(1e-4);
            -(v.new_domain(K_MEDIUM).draw_rnd_f32::<1>()[0].ln()) / sigma_t_max
        }
        _ => f32::INFINITY,
    }
}

/// Volume-region interaction along `ray`, clipped at `t_lim` — whatever
/// event would otherwise end the segment. Because the walk is bounded by
/// `t_lim`, a real collision is the nearest event by construction — the
/// competition between the carried medium, the surface and the regions is
/// exact (superposed processes), and the `Passthrough` weight is precisely
/// the region transmittance up to the winner.
#[inline(always)]
fn volume_event<const PROFILE: bool>(
    ctx: &PathContext,
    ray: &Ray,
    t_lim: f32,
    exiting: bool,
    v: PathSampler,
) -> VolumeEvent {
    // A walk's exit has no arriving segment outside the object.
    if ctx.volumes.is_empty() || exiting {
        VolumeEvent::Passthrough {
            transmittance: Vec3A::ONE,
            emitted: Vec3A::ZERO,
        }
    } else {
        let _p = profile::scope_if::<PROFILE>(Section::Volume);
        let mut rng = v.new_domain(K_VOLUME).rng();
        ctx.volumes
            .sample_interaction(ray, TRACE_T_MIN, t_lim, &mut rng)
    }
}

/// Backward gather: fold the records into the estimate, deepest vertex
/// first, emitting guiding training samples along the way. `radiance` is
/// what each vertex receives from its continuation (the next vertex's
/// emission left out — its MIS-weighted share enters separately through
/// `next_emit`); `terminal` is what enters the path beyond its last vertex.
#[inline(always)]
fn gather<const PROFILE: bool>(
    records: &[VertexRec],
    terminal: Vec3A,
    indirect_clamp: Option<f32>,
    train_out: &mut Vec<SampleData>,
) -> Vec3A {
    let _gather = profile::scope_if::<PROFILE>(Section::Contributions);
    let mut radiance = terminal;
    for (index, vrec) in records.iter().enumerate().rev() {
        if let Some(t) = &vrec.train {
            // The full incident radiance (reflected + the raw hit emission),
            // weighted by the one cosine this tracer's estimator carries (in
            // the material's value).
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
        radiance = if let Some(limit) = indirect_clamp
            && index == 0
            && records.len() > 1
        {
            // The primary vertex splits into what `clamp_indirect` leaves
            // alone and the continuation it clamps. Kept off the ordinary
            // expression below, which a disabled clamp must reproduce to the
            // bit.
            vrec.segment_emit
                + vrec.atten
                    * (vrec.emit_here
                        + vrec.nee
                        + vrec.factor * (vrec.next_emit * vrec.next_emit_weight))
                + clamp_indirect(vrec.atten * (vrec.factor * radiance), limit)
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

/// Where a random walk left its object: the next vertex, and the geometry it
/// is on.
#[derive(Default)]
struct PendingExit {
    rec: HitRecord,
    dir: Vec3A,
    owner: u32,
}

impl PendingExit {
    /// The exit as the hit the next vertex shades, on the exit Lambertian.
    ///
    /// Out of line for the same reason as [`walk_subsurface`]: built inline
    /// beside `World::intersect`, the two sources of the vertex's hit made
    /// LLVM copy the record at every vertex instead of writing it in place.
    #[cold]
    #[inline(never)]
    fn hit(&self) -> Option<WorldHit<'static>> {
        Some(WorldHit {
            rec: HitRecord { t: 0.0, ..self.rec },
            mat: &SSS_EXIT,
            geom_id: self.owner,
            prim_id: 0,
        })
    }
}

/// Runs the random walk a subsurface `sample` at `hit` enters, and returns
/// the sample weighted by the walk's throughput with its exit parked in
/// `sss_exit` — or `None` when the walk was absorbed.
///
/// Out of line and cold on purpose: inlined, the walk made `trace_path` too
/// large for LLVM to inline into `render_pixel`, and cornellbox — which never
/// walks — ran 2.4% more instructions.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn walk_subsurface(
    world: &World,
    hit: &WorldHit,
    ray: &Ray,
    sp: &ShadingPoint,
    mut sample: ScatterSample,
    v: PathSampler,
    sss_exit: &mut PendingExit,
    stats: &mut RayStats,
) -> Option<ScatterSample> {
    let entry = sp.subsurface_entry(&sample)?;
    stats.sss_walks += 1;
    let mut cost = WalkCost::default();
    let exit = random_walk(
        world,
        hit.geom_id,
        hit.rec.p,
        hit.rec.normal,
        hit.rec.front_face,
        &entry,
        ray.time(),
        v.new_domain(K_SSS),
        &mut cost,
    );
    stats.sss_steps += u64::from(cost.steps);
    stats.sss_rays += u64::from(cost.rays);
    let exit = exit?;
    stats.sss_exits += 1;
    sample.value *= exit.weight;
    *sss_exit = PendingExit {
        rec: exit.rec,
        dir: exit.dir,
        owner: hit.geom_id,
    };
    Some(sample)
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
