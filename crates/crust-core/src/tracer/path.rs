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
use crate::medium::{Medium, hg_phase, sample_henyey_greenstein};
use crate::pdf::PdfSolidAngle;
use crate::profile::Section;
use crate::ray::{Ray, RayMask};
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
        None,
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
/// footprints did". Consulted per camera ray, so it reads the parsed
/// [`crate::config()`], never the environment.
pub(super) fn ray_cones_enabled() -> bool {
    crate::config().ray_cones
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
        let light_pdf = lights.density(point_pdf, pmf).max(1e-6);
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
                let light_pdf = lights.density(pdf, pmf).max(1e-6);
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

/// The medium a path entered through a **medium boundary** — a volume-only
/// material ([`Material::is_medium_boundary`]) — and the geometry that owns
/// it: Typhoon's `MediumState`.
///
/// One owner at a time, as in Typhoon: inside an enclosure no other
/// boundary is entered and no refracting surface swaps the medium, and only
/// a crossing of the owner's own boundary leaves it. While a path is inside
/// one, every ray it traces carries [`Enclosure::medium`], whatever the
/// surface it scattered from built, and every shadow ray starts in it.
#[derive(Clone, Copy)]
struct Enclosure {
    medium: Medium,
    /// The boundary's `geom_id`: crossing it again is the way out.
    owner: u32,
    /// The owner's light-link class: the receiver of NEE at scatter points
    /// inside.
    class: u16,
}

/// What crossing a medium boundary at `hit` does to the medium a ray is in:
/// `inside` is updated in place, and the medium the continuing ray carries
/// is returned. The rules are Typhoon's `_UpdatePathMedium`: the owner's own
/// boundary seen from inside leaves; a front face met in vacuum enters the
/// boundary's medium (vacuum stays vacuum); anything else — another
/// boundary from inside an enclosure, any boundary from inside a refracting
/// interior — is crossed without a change.
fn cross_boundary(
    hit: &WorldHit<'_>,
    ray: &Ray,
    inside: &mut Option<Enclosure>,
    world: &World,
) -> Option<Medium> {
    match inside {
        Some(e) if e.owner == hit.geom_id && !hit.rec.front_face => {
            *inside = None;
            None
        }
        Some(e) => Some(e.medium),
        None if hit.rec.front_face && ray.medium().is_none() => {
            let m = hit.mat.boundary_medium(ray, &point_sampled(&hit.rec))?;
            *inside = Some(Enclosure {
                medium: m,
                owner: hit.geom_id,
                class: world.light_class(hit.geom_id),
            });
            Some(m)
        }
        None => ray.medium().copied(),
    }
}

/// NEE shadow test used at surface and volume vertices alike: ZERO when a
/// surface occludes the segment, otherwise the volumetric transmittance
/// through every region it crosses (stochastic for heterogeneous regions,
/// exact for homogeneous ones). MIS weights are unaffected — transmittance
/// is part of the integrand on both strategies, not of either pdf.
///
/// `inside` is the enclosure the shadow ray starts in: its medium attenuates
/// the ray until the ray leaves through the owner's boundary.
///
/// `inline(always)`: the medium-boundary branches grew the integrator past
/// LLVM's budget for inlining this into it, and out of line it cost
/// cornellbox 0.35% of its instructions.
#[inline(always)]
fn shadow_transmittance<const PROFILE: bool>(
    world: &World,
    volumes: &Volumes,
    shadow_ray: &Ray,
    distance: f32,
    inside: Option<&Enclosure>,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    // Dedicated occlusion query: any hit in range means full shadow, so the
    // early-exit traversal beats searching for the closest hit.
    let _p = profile::scope_if::<PROFILE>(Section::Occlusion);
    stats.shadow_rays += 1;
    if world.occluded(shadow_ray, 0.001, distance - 0.001) {
        // Blocked — unless only medium boundaries or cutouts block it, which
        // the any-hit query cannot tell apart. An open segment crosses
        // neither, so it keeps the fast answer.
        if world.has_medium_boundaries() {
            return medium_shadow(world, volumes, shadow_ray, distance, inside, vertex, stats);
        }
        if !world.has_cutouts() {
            stats.shadow_occluded += 1;
            return Vec3A::ZERO;
        }
        return cutout_shadow(world, volumes, shadow_ray, distance, vertex, stats);
    }
    // Open, but starting inside an enclosure: the light is inside it too.
    if inside.is_some() {
        return medium_shadow(world, volumes, shadow_ray, distance, inside, vertex, stats);
    }
    if volumes.is_empty() {
        return Vec3A::ONE;
    }
    let mut rng = vertex.new_domain(K_NEE_SHADOW).rng();
    volumes.transmittance(shadow_ray, 0.001, distance - 0.001, &mut rng)
}

/// How many cutouts one segment is followed through, on either side: past
/// this a path treats the next hit as present, and a shadow ray as blocked.
/// Generous — a stack of leaf cards seen edge-on is tens deep — and only
/// there so a pathological stack cannot stall a sample.
const MAX_CUTOUT_CROSSINGS: usize = 256;

/// How many medium boundaries one path may cross before it is given up as
/// absorbed: generous (a fog bank of a hundred overlapping cards, a deep
/// path in and out of a cloud), and only there so a ray stuck on a boundary
/// cannot stall a sample.
const MAX_PATH_CROSSINGS: usize = 4096;

/// `ray` restarted at its own parameter `t`, in the same direction, with the
/// cone as wide as it has grown by then: a hit on the result at `t'` is the
/// hit on `ray` at `t + t'`.
///
/// Stepping past a hit moves the origin rather than raising `t_min`, as the
/// subsurface walk's rays do: every `World::intersect` asks for
/// `(0.001, ∞)`, LLVM propagates those two constants into the kernel, and a
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

/// `ray` restarted to pass its hit at `t`, as [`restarted`] at
/// [`resume_before`]`(t)`, but with a unit direction and the step back taken
/// in distance: the free flights ahead, and `t` at the next hit, are then
/// distances in the medium the restart enters. A camera ray's direction is
/// not unit (it reaches the focus plane at `t = 1`), and stepping back 0.001
/// of *its* parameter put a hundredth of a unit of fog outside the box.
fn restarted_past(ray: &Ray, t: f32) -> Ray {
    let len = ray.direction().length();
    let dir = ray.direction() / len;
    let back = resume_before(t * len);
    let cone = ray.cone();
    Ray::new(ray.origin() + dir * back, dir)
        .with_time(ray.time())
        .with_mask(ray.mask())
        .with_cone(crate::RayCone {
            width: cone.width_at(back),
            spread: cone.spread,
        })
}

/// Where to restart a segment that passes a hit at `t` (see [`restarted`]):
/// short of it by the tracer's 0.001, less a relative step, so the restarted
/// ray's `(0.001, ∞)` begins just *past* the hit. Restarted on the hit
/// itself, the offset stepped over any surface within 0.001 behind it — a
/// decal or card layered over opaque geometry leaked light through
/// (`a_surface_just_behind_a_cutout_is_not_skipped`). The step is relative,
/// so a far hit is not met again through rounding.
#[inline]
fn resume_before(t: f32) -> f32 {
    t - 0.001 + t.abs().max(1.0) * 1e-5
}

/// [`shadow_transmittance`] for a shadow ray the any-hit query found blocked
/// in a world with cutouts: `Π (1 − opacity)` over every hit up to the light,
/// or zero at the first hit on a material without a cutout, times the volume
/// transmittance. Deterministic where the bounce side is stochastic
/// ([`pass_cutouts`]): both estimate the same visibility, and the product is
/// the lower-variance of the two.
#[cold]
#[inline(never)]
fn cutout_shadow(
    world: &World,
    volumes: &Volumes,
    ray: &Ray,
    distance: f32,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    let t_max = distance - 0.001;
    let through = cutout_through(world, ray, t_max, stats);
    if through == 0.0 {
        stats.shadow_occluded += 1;
        return Vec3A::ZERO;
    }
    if volumes.is_empty() {
        return Vec3A::splat(through);
    }
    let mut rng = vertex.new_domain(K_NEE_SHADOW).rng();
    through * volumes.transmittance(ray, 0.001, t_max, &mut rng)
}

/// [`shadow_transmittance`] in a world with medium boundaries: Typhoon's
/// `_Visibility`. The segment is followed hit by hit, as [`cutout_through`]
/// follows cutouts — a boundary is crossed under [`cross_boundary`]'s rules,
/// a cutout passes `1 − opacity`, anything else blocks — and the medium the
/// ray is in at each stretch attenuates it by Beer–Lambert. Deterministic
/// where the path side tracks free flights: both estimate the same
/// transmittance.
#[cold]
#[inline(never)]
fn medium_shadow(
    world: &World,
    volumes: &Volumes,
    ray: &Ray,
    distance: f32,
    inside: Option<&Enclosure>,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    let t_max = distance - 0.001;
    let mut inside = inside.copied();
    let mut medium = inside.map(|e| e.medium);
    let (mut t, mut segment) = (0.0, ray.clone());
    let mut kept = Vec3A::ONE;
    for crossing in 0..=MAX_CUTOUT_CROSSINGS {
        stats.cutout_rays += 1;
        let hit = world
            .intersect(&segment, 0.001, f32::INFINITY)
            .filter(|h| t + h.rec.t < t_max);
        let end = hit.as_ref().map_or(t_max, |h| t + h.rec.t);
        if let Some(m) = &medium {
            kept *= m.transmittance((end - t).max(0.0));
        }
        let Some(h) = hit else {
            break;
        };
        if crossing == MAX_CUTOUT_CROSSINGS {
            kept = Vec3A::ZERO;
        } else if h.mat.is_medium_boundary() {
            medium = cross_boundary(&h, &segment.clone().with_medium(medium), &mut inside, world);
        } else if h.mat.has_cutout() {
            kept *= 1.0 - h.mat.opacity(ray, &point_sampled(&h.rec));
        } else {
            kept = Vec3A::ZERO;
        }
        if kept.max_element() <= 0.0 {
            stats.shadow_occluded += 1;
            return Vec3A::ZERO;
        }
        t = resume_before(t + h.rec.t);
        segment = restarted(ray, t);
    }
    if volumes.is_empty() {
        return kept;
    }
    let mut rng = vertex.new_domain(K_NEE_SHADOW).rng();
    kept * volumes.transmittance(ray, 0.001, t_max, &mut rng)
}

/// The fraction of the segment `(0.001, t_max)` of `ray` that cutouts let
/// through: `Π (1 − opacity)` over every hit, or 0 at the first hit on a
/// material without a cutout. It follows at most [`MAX_CUTOUT_CROSSINGS`]
/// cutouts and then asks once more, where any hit blocks — the same bound
/// [`pass_cutouts`] keeps, which treats the hit past its last crossing as
/// present, so a stack exactly that deep is clear on both sides.
///
/// Shared by NEE ([`cutout_shadow`]) and the learned light cache's training,
/// whose shadow rays must see the visibility the integrator does.
pub(crate) fn cutout_through(world: &World, ray: &Ray, t_max: f32, stats: &mut RayStats) -> f32 {
    let (mut t, mut segment) = (0.0, ray.clone());
    let mut kept = 1.0;
    for crossing in 0..=MAX_CUTOUT_CROSSINGS {
        stats.cutout_rays += 1;
        let hit = world.intersect(&segment, 0.001, f32::INFINITY);
        let Some(h) = hit.filter(|h| t + h.rec.t < t_max) else {
            return kept;
        };
        // A medium boundary is crossed whole: this is visibility, and the
        // interior's transmittance is not part of it.
        let boundary = h.mat.is_medium_boundary();
        if crossing == MAX_CUTOUT_CROSSINGS || !(boundary || h.mat.has_cutout()) {
            return 0.0;
        }
        if !boundary {
            kept *= 1.0 - h.mat.opacity(ray, &point_sampled(&h.rec));
        }
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
/// Its shadow-side twin is [`cutout_shadow`].
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
            .intersect(&restarted(ray, t), 0.001, f32::INFINITY)
            .map(|mut next| {
                next.rec.t += t;
                next
            });
    }
}

/// Direct lighting at a volume-region scatter point. The exact mirror of
/// the surface NEE block: same light-selection strategy, with the
/// phase function (value == pdf for the HG mixture) in place of
/// `brdf·cos`, and the same phase pdf as the competing bounce density that
/// `bounce_emission_weight`'s `Phase` arm uses.
///
/// Also the NEE at a scatter inside an [`Enclosure`] (`inside`), with the
/// enclosure's single Henyey–Greenstein lobe for `phase` — Typhoon's
/// `_ComputeMediumDirectLighting` — through [`enclosure_nee`].
#[allow(clippy::too_many_arguments)]
fn volume_nee<const PROFILE: bool>(
    p: Vec3A,
    wi: Vec3A,
    phase: Phase<'_>,
    class: u16,
    inside: Option<&Enclosure>,
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
    let tr = shadow_transmittance::<PROFILE>(
        world,
        volumes,
        &shadow_ray,
        s.distance,
        inside,
        vertex,
        stats,
    );
    if tr == Vec3A::ZERO {
        return Vec3A::ZERO;
    }
    let light_pdf = lights.density(s.pdf, pmf).max(1e-6);
    // The phase function is its own pdf, in solid angle. A shadow-linked
    // light has no competing bounce strategy (see `bounce_emission_weight`).
    let weight = if lights.nee_only(index) {
        1.0
    } else {
        strategy.light_weight(light_pdf, PdfSolidAngle::from_measure(phase_val))
    };
    s.radiance * phase_val * tr * weight / light_pdf.get()
}

/// The phase function a [`volume_nee`] connection is weighted by: a region's
/// lobe mixture, or an enclosure's single Henyey–Greenstein lobe. An enum
/// rather than a closure so `volume_nee` has one body: a second
/// monomorphisation gave `LightList::pick_index_at` another call site, LLVM
/// stopped inlining it, and cornellbox — which has neither — paid 0.6% more
/// instructions.
#[derive(Clone, Copy)]
enum Phase<'a> {
    Mix(&'a PhaseMix),
    Hg(f32),
}

impl Phase<'_> {
    #[inline]
    fn pdf(self, cos_theta: f32) -> f32 {
        match self {
            Phase::Mix(m) => m.pdf(cos_theta),
            Phase::Hg(g) => hg_phase(cos_theta, g),
        }
    }
}

/// NEE at a scatter inside an enclosure: [`volume_nee`] with its medium's
/// lobe, its owner's light-link class, and shadow rays that start inside it.
/// Out of line, as every medium-boundary path is: a world without one never
/// gets here, and its integrator stays the shape it was.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn enclosure_nee<const PROFILE: bool>(
    p: Vec3A,
    wi: Vec3A,
    inside: &Enclosure,
    world: &World,
    volumes: &Volumes,
    lights: &LightList,
    strategy: SamplingStrategy,
    vertex: PathSampler,
    time: f32,
    stats: &mut RayStats,
) -> Vec3A {
    volume_nee::<PROFILE>(
        p,
        wi,
        Phase::Hg(inside.medium.g),
        inside.class,
        Some(inside),
        world,
        volumes,
        lights,
        strategy,
        vertex,
        time,
        stats,
    )
}

/// The integrator: an iterative path tracer in two passes. The forward walk
/// traces one segment per bounce (each hit serves both as the previous
/// vertex's potential light hit and as the next vertex — the old recursion
/// intersected every segment twice), records a `VertexRec` per vertex, and
/// applies Russian roulette past `RR_START_BOUNCE`. The backward gather
/// then folds the records into the radiance estimate and emits guiding
/// training samples, which need the radiance arriving from the rest of the
/// path and therefore cannot be computed forward.
///
/// Forced inline into `render_pixel` (and the `ray_color` wrapper tests call).
/// LLVM inlined it on its
/// own until the cutout branches tipped it over the threshold, and out of
/// line it costs cornellbox 1.4% of its instructions (callgrind, 2 spp);
/// forced, the tree before cutouts measured 0.4% *fewer*.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
pub(super) fn trace_path<const PROFILE: bool>(
    r: &Ray,
    world: &World,
    lights: &LightList,
    volumes: &Volumes,
    depth: i32,
    strategy: SamplingStrategy,
    indirect_clamp: Option<f32>,
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
    // Set when the last vertex entered a random walk: the next vertex is
    // its exit, shaded without tracing the segment that reaches it (the walk
    // already did, inside the object). A flag beside a slot rather than an
    // `Option<WalkExit>`: taking a 150-byte option at every vertex cost
    // cornellbox, which never walks, 1% of its instructions.
    let mut sss_pending = false;
    let sss_exit = &mut scratch.sss_exit;
    // The medium boundary's interior the path is in (see `Enclosure`), and
    // what the stretches crossed since the last vertex carried: their
    // transmittance and their (already weighted) volume emission, folded
    // into whatever the segment reaches next. `crossings` counts the
    // boundaries crossed before vertex `.0`, so each stretch draws from its
    // own domain. All three stay at rest in a world without boundaries.
    // The medium boundary's interior the path is in (see `Enclosure`), and
    // what the stretches crossed since the last vertex carried: their
    // transmittance and their (already weighted) region emission, folded into
    // whatever the segment reaches next. `crossed` counts the boundaries
    // crossed so far. Every per-vertex check is behind `boundaries`, so a
    // world without one pays a predictable branch for each.
    let boundaries = world.has_medium_boundaries();
    let mut enclosure: Option<Enclosure> = None;
    let mut carry: Option<(Vec3A, Vec3A)> = None;
    let mut crossed = 0usize;

    loop {
        // This vertex's domain: `records.len()` is the vertex index (nothing
        // has been pushed for it yet). Every per-event draw hangs off `v`.
        // A stretch that began at a medium boundary is numbered past the
        // stretch before it — `records.len() + crossed` grows at every step
        // either way — so it cannot repeat that stretch's draws; with nothing
        // crossed the index is the vertex's.
        let v = path.new_domain((records.len() + crossed) as i32);

        if remaining <= 0 {
            stats.ended_depth += 1;
            // Depth exhausted. The old recursion still counted bounce-hit
            // emission at the last vertex (its `add_emission` term traced
            // the ray itself) but never the background — reproduce both,
            // attenuating through any media the final segment crosses.
            if let Some(p) = &prev {
                stats.closest_hit += 1;
                let mut hit = {
                    let _p = profile::scope_if::<PROFILE>(Section::Trace);
                    world.intersect(&ray, 0.001, f32::INFINITY)
                };
                if world.has_cutouts() {
                    pass_cutouts(world, &ray, &mut hit, v, stats);
                }
                // Through any medium boundaries, measuring what their
                // interiors take on the way.
                let medium_tr =
                    boundaries.then(|| pass_boundaries(world, &ray, &mut hit, enclosure, v, stats));
                if let Some(hit) = hit {
                    let cos_o = ray.direction().normalize().dot(hit.rec.normal).abs();
                    let mut emitted = hit.mat.emitted_at(&ray, &hit.rec, cos_o);
                    if emitted.length_squared() > 0.0 {
                        if let Some(tr) = medium_tr {
                            emitted *= tr;
                        } else if let Some(m) = ray.medium() {
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

        let exiting = sss_pending;
        let mut hit_opt = if exiting {
            sss_pending = false;
            sss_exit.hit()
        } else {
            stats.closest_hit += 1;
            let _p = profile::scope_if::<PROFILE>(Section::Trace);
            world.intersect(&ray, 0.001, f32::INFINITY)
        };
        // Patched in place, on the cold side only: an `if` that yields the
        // hit from either arm copies all of it at every vertex (+0.8% of
        // cornellbox's instructions, which has no cutout).
        if world.has_cutouts() && !exiting {
            pass_cutouts(world, &ray, &mut hit_opt, v, stats);
        }
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
        // A walk's exit has no arriving segment outside the object.
        let mut event = if volumes.is_empty() || exiting {
            VolumeEvent::Passthrough {
                transmittance: Vec3A::ONE,
                emitted: Vec3A::ZERO,
            }
        } else {
            let _p = profile::scope_if::<PROFILE>(Section::Volume);
            let mut rng = v.new_domain(K_VOLUME).rng();
            volumes.sample_interaction(&ray, 0.001, t_lim, &mut rng)
        };
        // Fold in what the boundary stretches before this one carried. On
        // the cold side only: rebinding the event through a `match` moves all
        // of it at every vertex.
        if boundaries && let Some((tr, emitted)) = carry.take() {
            event = carried(event, tr, emitted);
        }

        let (vol_tr, vol_emit) = match event {
            VolumeEvent::Scatter {
                p,
                weight,
                phase,
                emitted,
                class,
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
                    Phase::Mix(&phase),
                    class,
                    enclosure.as_ref(),
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
                    pdf: PdfSolidAngle::from_measure(phase_pdf),
                    class,
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
                    Some(m) => Ray::new_in_medium(p, dir, *m),
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
            let e = (Vec3A::splat(sigma_bar) - (medium.sigma_a + medium.sigma_s)) * t_med;
            let factor = medium.sigma_s / sigma_bar * Vec3A::new(e.x.exp(), e.y.exp(), e.z.exp());
            // Subsurface vertices run no NEE (their shadow rays are
            // blocked by the enclosing surface), so `prev = None` keeps
            // the next hit's emission at full weight — the pairing that
            // avoids double counting. Inside an enclosure the boundary lets
            // shadow rays out, so a scatter there runs NEE with its phase
            // function, exactly as a volume-region scatter does, and the
            // event weight moves into `atten` so it multiplies NEE too.
            let enclosed = enclosure.filter(|e| boundaries && e.medium == medium);
            let wi = ray.direction().normalize();
            let (atten, nee, factor) = match &enclosed {
                Some(e) => {
                    let nee = enclosure_nee::<PROFILE>(
                        pos,
                        wi,
                        e,
                        world,
                        volumes,
                        lights,
                        strategy,
                        v,
                        ray.time(),
                        stats,
                    );
                    (vol_tr * factor, nee, Vec3A::ONE)
                }
                None => (vol_tr, Vec3A::ZERO, factor),
            };
            let mut vrec = VertexRec {
                atten,
                segment_emit: vol_emit,
                emit_here: Vec3A::ZERO,
                nee,
                factor,
                next_emit: Vec3A::ZERO,
                next_emit_weight: 1.0,
                train: None,
            };
            beta *= atten * factor;
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
            prev = enclosed.map(|e| PrevVertex::Phase {
                pos,
                pdf: PdfSolidAngle::from_measure(hg_phase(wi.dot(dir), medium.g).max(1e-6)),
                class: e.class,
            });
            continue;
        }

        let Some(hit) = hit_opt else {
            stats.ended_escaped += 1;
            // === Background ===
            // A ray leaving the scene is how lights at infinity are found
            // by chance, so it is a bounce-side MIS event just like hitting
            // an emissive surface.
            let unit_direction = Vec3A::normalize(ray.direction());
            let background = escaped_emission(&prev, lights, unit_direction, ray.mask(), strategy);
            // Segment emission is already weighted; the background pays the
            // volume transmittance of the final segment.
            terminal = vol_emit + vol_tr * background;
            break;
        };
        if boundaries && hit.mat.is_medium_boundary() {
            // === Medium boundary: crossed, not shaded ===
            // No vertex and no depth: the stretch up to here is carried into
            // whatever the segment reaches next, and the path continues in
            // the medium the crossing leaves it in, from just past the hit.
            crossed += 1;
            if crossed > MAX_PATH_CROSSINGS {
                stats.ended_absorbed += 1;
                break;
            }
            carry = Some((vol_tr * medium_arrival(ray.medium(), hit.rec.t), vol_emit));
            let medium = cross_boundary(&hit, &ray, &mut enclosure, world);
            ray = restarted_past(&ray, hit.rec.t).with_medium(medium);
            continue;
        }
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
        let med_arrival = medium_arrival(ray.medium(), rec.t);
        let atten = vol_tr * med_arrival;

        // Emission accounting: a vertex reached by a bounce hands its
        // emission to the previous vertex's record, MIS-weighted —
        // counting it here too would double it. At the primary vertex and
        // after carried-medium scatters it counts here, in full. Either
        // way the emission pays the arriving segment's attenuation (an
        // emitter seen through tinted glass or smoke must dim).
        let cos_o = ray.direction().normalize().dot(rec.normal).abs();
        // The receiver's light-link class, for NEE here and for whatever the
        // bounce leaving this vertex collects.
        let class_here = world.light_class(hit.geom_id);
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
            // (Before that split a textured `eval` went after the ray, so an
            // occluded light never paid for the network.) Either order is
            // bit-identical: a skipped test's contribution would be exactly
            // zero, and the shadow ray's own draws come from `K_NEE_SHADOW`,
            // which nothing else reads.
            let mut visibility = || {
                let shadow_ray = Ray::new(rec.p, light_dir_unit)
                    .with_time(ray.time())
                    .with_mask(lights.shadow_mask(light_index));
                let tr = shadow_transmittance::<PROFILE>(
                    world,
                    volumes,
                    &shadow_ray,
                    ls.distance,
                    enclosure.as_ref(),
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
                let bounce_pdf = PdfSolidAngle::from_measure(match guiding_here {
                    Some(g) if g.field.trained_at(rec.p) => {
                        let alpha = g.field.config().guide_prob;
                        alpha * g.field.pdf(rec.p, light_dir_unit) + (1.0 - alpha) * brdf_pdf
                    }
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
                // ray-facing normal). Applying it again here is what used
                // to make this an integral of `brdf · cos²`.
                nee += ls.radiance * brdf_value * shadow_tr * weight / light_pdf.get();
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
        let mut bounce = {
            let _p = profile::scope_if::<PROFILE>(Section::Bounce);
            sample_bounce_direction(&ray, &rec, &sp, guiding_here, v)
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
            bounce = walk_subsurface(world, &hit, &ray, &sp, sample, v, sss_exit, stats);
            sss_pending = bounce.is_some();
        }
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
                if sss_pending {
                    let exit = &*sss_exit;
                    // The exit vertex sees the walk arrive from outside,
                    // along its last direction; what it emits there leaves
                    // the object, so nothing is owed to this record.
                    stats.vertices += 1;
                    records.push(vrec);
                    prev = None;
                    ray = Ray::new(exit.rec.p, -exit.dir)
                        .with_time(ray.time())
                        .with_mask(crate::ray::MASK_INDIRECT)
                        .with_cone(
                            ray.cone()
                                .scattered(cone_width_here, crate::RayCone::MAX_SPREAD),
                        );
                    if boundaries && let Some(e) = &enclosure {
                        ray = ray.with_medium(Some(e.medium));
                    }
                    continue;
                }
                prev = Some(PrevVertex::Surface(PrevBounce {
                    pos: rec.p,
                    // A BSDF (or guide-mixture) pdf, in solid angle.
                    pdf: PdfSolidAngle::from_measure(sample.pdf),
                    delta: sample.delta,
                    class: class_here,
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
                // Inside an enclosure its medium is the one the path travels
                // in, whatever medium (or none) the surface's ray carries.
                if boundaries && let Some(e) = &enclosure {
                    ray = ray.with_medium(Some(e.medium));
                }
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

/// The carried medium's attenuation over a segment of length `t` that
/// reached a surface. For a *scattering* medium the arrival already paid
/// e^{−σ̄·t} through the free-flight competition (no event before `t`), so
/// only the chromatic correction e^{(σ̄−σₜ)·t} remains — exactly ONE for gray
/// media. Non-scattering media (glass tint) keep pure Beer–Lambert.
#[inline(always)]
fn medium_arrival(medium: Option<&Medium>, t: f32) -> Vec3A {
    match medium {
        Some(m) if m.is_scattering() => {
            let sigma_bar = m.sigma_t_max().max(1e-4);
            let e = (Vec3A::splat(sigma_bar) - (m.sigma_a + m.sigma_s)) * t;
            Vec3A::new(e.x.exp(), e.y.exp(), e.z.exp())
        }
        Some(m) => m.transmittance(t),
        None => Vec3A::ONE,
    }
}

/// `event` preceded by boundary stretches that carried transmittance `tr`
/// and pre-weighted emission `emitted`: the stretches' weight multiplies the
/// event's, and their emission comes before the event's own.
#[cold]
#[inline(never)]
fn carried(event: VolumeEvent, tr: Vec3A, emitted: Vec3A) -> VolumeEvent {
    match event {
        VolumeEvent::Scatter {
            t,
            p,
            weight,
            phase,
            emitted: e,
            class,
        } => VolumeEvent::Scatter {
            t,
            p,
            weight: tr * weight,
            phase,
            emitted: emitted + tr * e,
            class,
        },
        VolumeEvent::Passthrough {
            transmittance,
            emitted: e,
        } => VolumeEvent::Passthrough {
            transmittance: tr * transmittance,
            emitted: emitted + tr * e,
        },
    }
}

/// Makes `hit` the first hit past any medium boundaries, for the one
/// segment that is traced without free flights (the depth-exhausted
/// emission lookup), and returns the Beer–Lambert transmittance of the media
/// it crossed on the way — `ray`'s own up to the first boundary, then
/// whichever each crossing leaves it in. `hit.rec.t` stays measured along
/// `ray`, as [`pass_cutouts`] keeps it.
#[cold]
#[inline(never)]
fn pass_boundaries<'w>(
    world: &'w World,
    ray: &Ray,
    hit: &mut Option<WorldHit<'w>>,
    mut inside: Option<Enclosure>,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    let mut medium = ray.medium().copied();
    let (mut tr, mut from) = (Vec3A::ONE, 0.0);
    for _ in 0..MAX_CUTOUT_CROSSINGS {
        let Some(h) = hit.as_ref() else {
            return tr;
        };
        if let Some(m) = &medium {
            tr *= m.transmittance((h.rec.t - from) * ray.direction().length());
        }
        if !h.mat.is_medium_boundary() {
            return tr;
        }
        medium = cross_boundary(h, &ray.clone().with_medium(medium), &mut inside, world);
        let t = resume_before(h.rec.t);
        from = h.rec.t;
        *hit = world
            .intersect(&restarted(ray, t), 0.001, f32::INFINITY)
            .map(|mut next| {
                next.rec.t += t;
                next
            });
        if world.has_cutouts() {
            pass_cutouts(world, ray, hit, vertex, stats);
        }
    }
    *hit = None;
    Vec3A::ZERO
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
