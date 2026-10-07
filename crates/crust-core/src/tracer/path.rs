//! The integrator: one camera path, traced forward a segment per vertex and
//! gathered backward into a radiance estimate (and guiding training samples).
//!
//! Every NEE weight here has a bounce-side twin (`bounce_emission_weight`,
//! `escaped_emission`); change both or neither.

use glam::Vec3A;
use utils::exp3;

use crate::aov::FirstHit;
use crate::guiding::SampleData;
use crate::hittable::HitRecord;
use crate::material::{Material, ScatterSample, ShadingPoint};
use crate::medium::sample_henyey_greenstein;
use crate::pdf::PdfSolidAngle;
use crate::profile::Section;
use crate::ray::{Ray, RayMask, TRACE_T_MIN};
use crate::rt_world::{World, WorldHit};
use crate::stats::RayStats;
use crate::subsurface::{ExitLambertian, WalkCost, random_walk};
use crate::volume::{PhaseMix, VolumeEvent, Volumes};
use crate::{EVERY_CLASS, Light, LightList, PathSampler, profile};

use super::GuidingContext;
use super::route::{Arrival, NO_EVENT, Route, RouteCtx};
use super::settings::SamplingStrategy;
use crate::lpe::LobeSplit;

// OpenQMC domain-tree keys. The camera and the path subtree hang off the root
// (per-pixel, per-sample) sampler; each per-event sub-domain hangs off the
// current vertex domain. Distinct keys give independent 4D sub-patterns.
pub(super) const K_CAMERA: i32 = 0; // off root: jitter (0,1) + lens uv (2,3)
const K_PATH: i32 = 1; // off root: the bounce subtree
pub(super) const K_TIME: i32 = 2; // off root: shutter time for motion blur
const K_NEE: i32 = 0; // off vertex: light pick (0) + area uv (1,2)
#[cfg(test)]
pub(super) const K_NEE_TEST: i32 = K_NEE;
const K_NEE_SHADOW: i32 = 1; // off vertex: shadow-ray volume transmittance
const K_BSDF: i32 = 2; // off vertex: material scatter block
const K_GUIDE: i32 = 3; // off vertex: guide coin (0) + its rng() for the descent
const K_PHASE: i32 = 4; // off vertex: phase lobe (0) + HG uv (1,2)
const K_RR: i32 = 5; // off vertex: Russian-roulette survival
const K_MEDIUM: i32 = 6; // off vertex: carried-medium free flight
const K_VOLUME: i32 = 7; // off vertex: volume-region delta tracking
const K_SSS: i32 = 8; // off vertex: the subsurface random walk (per step below)
const K_CUTOUT: i32 = 9; // off vertex: presence at each cutout the segment meets
const K_THIN: i32 = 10; // off vertex, per crossing: thin wall's T estimate (0) + pass coin (1)
const K_NEE_THIN: i32 = 11; // off vertex, per crossing: a shadow ray's thin-wall T estimates
const K_CROSS: i32 = 12; // off vertex: volume transmittance to hidden lights crossed past the depth
const K_NEE_SAMPLES: i32 = 13; // off vertex: light sample i of several hangs off .new_domain(i)

/// The sampler light sample `index` at a vertex draws its pick, its point
/// on the light and its shadow ray from: the vertex itself for the first
/// sample, so a one-sample render is the renderer before counts existed, bit
/// for bit; a sub-domain of its own under [`K_NEE_SAMPLES`] for every later
/// sample, so the samples' draws are independent of one another (the pick is
/// then stratified by [`stratified_pick`]).
#[inline(always)]
pub(super) fn nee_sampler(vertex: PathSampler, index: u32) -> PathSampler {
    if index == 0 {
        vertex
    } else {
        vertex.new_domain(K_NEE_SAMPLES).new_domain(index as i32)
    }
}

/// The light-pick coordinate of light sample `index` of `count` at a vertex:
/// `(index + u) / count`, so the `count` samples' picks each fall in their
/// own slice of `[0, 1)` and, the pick being a monotone CDF inversion, a
/// light of selection probability `p` is picked `count · p` times, give or
/// take one, instead of a binomial number of times. One sample's coordinate
/// is `u` itself. Only the pick is stratified: a point-on-light coordinate
/// tied to the same slice would make which part of a light is sampled
/// depend on which light was picked, which is a bias, not a stratification.
#[inline(always)]
pub(super) fn stratified_pick(u: f32, count: u32, index: u32) -> f32 {
    if count == 1 {
        u
    } else {
        (index as f32 + u) / count as f32
    }
}

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

/// Russian roulette at vertex `depth`. Past [`RR_START_BOUNCE`] the path
/// survives with a probability tracking its throughput `beta`, floored at
/// [`RR_MIN_PROB`], and a survivor's `beta` and continuation `factor` are
/// divided by it. Returns that probability — 1 when the vertex is not tested
/// or survival is certain — or `None` when the path is killed, leaving both
/// untouched. The one roulette every vertex kind (surface bounce, region
/// phase scatter, carried-medium scatter) applies.
#[inline(always)]
fn russian_roulette(
    depth: usize,
    v: PathSampler,
    beta: &mut Vec3A,
    factor: &mut Vec3A,
    stats: &mut RayStats,
) -> Option<f32> {
    if depth < RR_START_BOUNCE {
        return Some(1.0);
    }
    stats.rr_tested += 1;
    let p_survive = beta.max_element().clamp(RR_MIN_PROB, 1.0);
    if p_survive < 1.0 {
        if v.new_domain(K_RR).draw_rnd_f32::<1>()[0] >= p_survive {
            stats.rr_killed += 1;
            return None;
        }
        *factor /= p_survive;
        *beta /= p_survive;
    }
    Some(p_survive)
}

/// What every path of a render traces against: the scene, and the render
/// settings the integrator reads per path. Fixed for the whole render (a
/// guided pass's field included), so the renderer builds one per pixel and
/// every sample borrows it.
///
/// Consumed only by the inlined `trace_path`, which destructures it on entry.
/// Never hand `&PathContext` on to a function LLVM keeps out of line: its
/// address then escapes, the struct must stay in memory, and every field is
/// reloaded after every opaque call — passing it to `volume_nee` and
/// `mixed_hair_shadow` cost cornellbox 0.08% of its instructions, and passing
/// it by value 0.23% (callgrind, 2 spp). Pass those helpers the fields.
#[derive(Clone, Copy)]
pub(super) struct PathContext<'a> {
    pub(super) world: &'a World,
    pub(super) lights: &'a LightList,
    pub(super) volumes: &'a Volumes,
    /// The longest path, in vertices.
    pub(super) depth: i32,
    pub(super) strategy: SamplingStrategy,
    /// The firefly clamp on indirect light, `None` when off.
    pub(super) indirect_clamp: Option<f32>,
    /// The guiding field and whether this pass trains it.
    pub(super) guiding: Option<&'a GuidingContext<'a>>,
    /// Light samples NEE takes at the first vertex of a path, and at every
    /// later surface or volume vertex (each at least 1).
    pub(super) light_samples: u32,
    pub(super) light_samples_indirect: u32,
}

/// One camera path's radiance, with one light sample per vertex — see
/// [`ray_color_with_light_samples`].
pub fn ray_color(
    r: &Ray,
    world: &World,
    lights: &LightList,
    volumes: &Volumes,
    depth: i32,
    strategy: SamplingStrategy,
    sampler: PathSampler,
) -> Vec3A {
    ray_color_with_light_samples(r, world, lights, volumes, depth, strategy, (1, 1), sampler)
}

/// One camera path's radiance: the one-shot entry point for tests and
/// benches, with `light_samples` NEE samples at the (camera, later) vertices
/// as [`RenderSettings::with_light_samples`](super::RenderSettings::with_light_samples)
/// takes them. Unclamped, unguided.
#[allow(clippy::too_many_arguments)]
pub fn ray_color_with_light_samples(
    r: &Ray,
    world: &World,
    lights: &LightList,
    volumes: &Volumes,
    depth: i32,
    strategy: SamplingStrategy,
    light_samples: (u32, u32),
    sampler: PathSampler,
) -> Vec3A {
    let mut no_training = Vec::new();
    let mut stats = RayStats::default();
    // The one-shot entry point (benches and tests), so a scratch per call is
    // the right trade — the renderer's own paths reuse one per work unit.
    let mut scratch = PathScratch::new(depth.max(0) as usize);
    let cx = PathContext {
        world,
        lights,
        volumes,
        depth,
        strategy,
        indirect_clamp: None,
        guiding: None,
        light_samples: light_samples.0.max(1),
        light_samples_indirect: light_samples.1.max(1),
    };
    trace_path::<false, false>(r, &cx, sampler, &mut no_training, &mut scratch, &mut stats)
}

/// Are texture-filtering ray cones on? `CRUST_RAY_CONES=0` forces every
/// footprint to zero, which makes every texture point-sample its finest level
/// — the A/B that separates "the mip pyramids changed the image" from "the
/// footprints did". Consulted per camera ray, so it reads the parsed
/// [`crate::config()`], never the environment.
pub(super) fn ray_cones_enabled() -> bool {
    crate::config().ray_cones
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
//
// `inline(always)`, as is `escaped_emission`: each is called once per
// `trace_path` instance, and once the integrator was monomorphised on the
// profiler switch LLVM stopped inlining them into either copy — +1.1%
// instructions on cornellbox with profiling off.
#[inline(always)]
fn sample_bounce_direction<const AOV: bool>(
    r: &Ray,
    rec: &HitRecord,
    sp: &ShadingPoint,
    guiding: Option<&GuidingContext>,
    sampler: PathSampler,
    split: Option<&mut LobeSplit>,
) -> Option<ScatterSample> {
    // The material's own sample — split by lobe at the direction it drew
    // (see `ShadingPoint::scatter_split`) when light path expressions want
    // the split, which only the AOV instantiation can ask for.
    let mut split = if AOV { split } else { None };
    let mut scatter = |dom| match split.as_deref_mut() {
        Some(out) => sp.scatter_split(r, dom, out),
        None => sp.scatter_importance(r, dom),
    };
    // Distinct sub-domains: the BSDF scatter block, and the guide block whose
    // first dimension is the α-coin and whose incidental stream drives the
    // quadtree descent.
    let bsdf_dom = sampler.new_domain(K_BSDF);
    let g = match guiding {
        Some(g) if g.field.trained_at(rec.p) => g,
        _ => return scatter(bsdf_dom),
    };
    let alpha = g.field.config().guide_prob;
    let guide_dom = sampler.new_domain(K_GUIDE);
    let gs = guide_dom.draw_sample_f32::<1>();

    if gs[0] < alpha {
        // Guide branch: draw from the field; the material's continuous
        // component supplies the value and the BSDF side of the mixture pdf.
        if let Some((wi, p_guide)) = g.field.sample(rec.p, &mut guide_dom.rng())
            && let Some((value, p_bsdf)) = sp.eval(r, wi)
        {
            if AOV && let Some(out) = split {
                // `value` is `eval` toward `wi`, so the split is `eval`'s.
                sp.eval_lobes(r, wi, out);
            }
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
        scatter(bsdf_dom)
    } else {
        // BSDF branch.
        let mut sample = scatter(bsdf_dom)?;
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
    /// The hidden light sources the continuation ray crossed on its way
    /// there ([`Crossing`]): their emission, attenuated and MIS-weighted
    /// each by its own light, summed in crossing order — and the same
    /// unweighted, for guiding training. Zero for a segment that crossed
    /// none, so the gather adds nothing. The light path expressions keep
    /// each crossing apart (`Route::cross`).
    pub(super) crossed: Vec3A,
    pub(super) crossed_raw: Vec3A,
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
    /// What the camera ray met at vertex 0 — written by the AOV
    /// instantiation of [`trace_path`] only, read by the film after it.
    pub(super) first: FirstHit,
    /// The light path expressions and albedo the render asks for, set on
    /// the scratch of an AOV pass only.
    pub(super) route_ctx: Option<std::sync::Arc<RouteCtx>>,
    /// The path's routing record and per-sample results ([`Route`]).
    pub(super) route: Route,
    /// Per-lobe shares — NEE's toward the light, the bounce's toward the
    /// direction sampled — filled by the AOV instantiation only. Here rather
    /// than on the walk's stack: building them per path cost the beauty-only
    /// render 0.28% of its instructions even though nothing read them.
    nee_split: LobeSplit,
    bounce_split: LobeSplit,
    /// The thin walls the current segment passed and how it ended — written
    /// by [`pass_cutouts`] in a world with straight transmission only.
    thin: ThinWalls,
}

/// What [`pass_cutouts`] leaves for the vertex that ends a segment past thin
/// walls (surfaces with straight transmission) or hidden light sources. Lives
/// in [`PathScratch`] so that a world without any never touches it.
#[derive(Default)]
struct ThinWalls {
    /// Each thin wall passed, in order: its `t` along the segment's ray and
    /// the pass weight `P / q`, which everything past it carries.
    passes: Vec<(f32, Vec3A)>,
    /// Every pass of the segment in order, cutouts too: whether it was a
    /// thin wall. The light path expression events (`TS` or `Ts`).
    kinds: Vec<bool>,
    /// `α / (1 − q)` when the segment ended meeting a thin wall it could
    /// have passed, 1 otherwise.
    meet: f32,
    /// Whether it ended so: the vertex then scatters through the material
    /// without its straight transmission, which the passes carry instead.
    reduced: bool,
    /// The first thin wall passed, as the camera's first hit when this is
    /// the camera's segment: its `t` and the hit there.
    first: Option<(f32, HitRecord)>,
    /// The hidden light sources the segment crossed, in order. Written by
    /// every [`pass_cutouts`], in a world with any.
    crossed: Vec<Crossing>,
}

impl ThinWalls {
    /// The product of the pass weights before `t` along the segment.
    fn before(&self, t: f32) -> Vec3A {
        passes_before(&self.passes, t)
    }
}

/// The product of the pass weights in `passes` before `t`.
///
/// (`passes` is ordered by `t`: a segment meets its walls in order.)
fn passes_before(passes: &[(f32, Vec3A)], t: f32) -> Vec3A {
    passes
        .iter()
        .take_while(|p| p.0 < t)
        .fold(Vec3A::ONE, |w, p| w * p.1)
}

/// A hidden light source a bounce segment crossed: an emitter and nothing
/// else, whose emission the segment collects on its way past
/// ([`World::is_transparent_emitter`]; lighting design record, "Hidden
/// lights are transparent emitters").
#[derive(Clone, Copy)]
struct Crossing {
    /// Where, along the segment's ray.
    t: f32,
    geom_id: u32,
    p: Vec3A,
    /// What the source emits toward the ray there, unattenuated.
    emitted: Vec3A,
    /// How many pass-throughs (cutouts, thin walls) the segment passed
    /// before it: the prefix of the arrival's events its `L` follows.
    passes: u16,
    /// The weight the segment carried to it — volume tracking, thin-wall
    /// passes, the carried medium — or `None` when an event (a volume or
    /// medium scatter) ended the segment first and it was never reached.
    reach: Option<Vec3A>,
}

impl Crossing {
    fn at(ray: &Ray, h: &WorldHit, passes: usize) -> Crossing {
        let cos_o = ray.direction().normalize().dot(h.rec.normal).abs();
        Crossing {
            t: h.rec.t,
            geom_id: h.geom_id,
            p: h.rec.p,
            emitted: h.mat.emitted_at(ray, &h.rec, cos_o),
            passes: passes.min(u16::MAX as usize) as u16,
            reach: None,
        }
    }
}

impl PathScratch {
    /// `max_depth` is a capacity hint only; the walk may push fewer.
    pub(crate) fn new(max_depth: usize) -> Self {
        Self {
            records: Vec::with_capacity(max_depth),
            sss_exit: PendingExit::default(),
            first: FirstHit::Escaped,
            route_ctx: None,
            route: Route::default(),
            nee_split: LobeSplit::default(),
            bounce_split: LobeSplit::default(),
            thin: ThinWalls::default(),
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
    /// How many light samples NEE took at that surface: the bounce competes
    /// against all of them (multi-sample MIS), so the light density it is
    /// weighted against is this many times the per-sample density.
    pub(super) nee_count: u32,
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
        /// The light samples `volume_nee` took there (see `PrevBounce`).
        nee_count: u32,
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
    bounce_emission_weight_at(prev, lights, hit.geom_id, hit.rec.p, strategy)
}

/// [`bounce_emission_weight`] for the emitter `geom_id`, reached at `p`: what
/// a hidden light source the bounce crossed ([`Crossing`]) is weighted by.
#[inline(always)]
fn bounce_emission_weight_at(
    prev: &PrevVertex,
    lights: &LightList,
    geom_id: u32,
    point: Vec3A,
    strategy: SamplingStrategy,
) -> f32 {
    // The receiver is the vertex the bounce left, and its links apply
    // whatever the bounce's lobe: a light that does not illuminate it gives
    // nothing on this side, exactly as NEE there skips it.
    let (from, class, competing, nee_count) = match prev {
        PrevVertex::Surface(p) => (p.pos, p.class, p.continuous().then_some(p.pdf), p.nee_count),
        PrevVertex::Phase {
            pos,
            pdf,
            class,
            nee_count,
        } => (*pos, *class, Some(*pdf), *nee_count),
    };
    // Nothing competes after a delta bounce, and without links nothing can
    // filter it either: skip the light lookup, as before links existed.
    if competing.is_none() && lights.links().is_none() {
        return strategy.unopposed_weight();
    }
    let Some((index, pmf)) = lights.find_index_by_geom_at(geom_id, from) else {
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
        let Some(point_pdf) = light.pdf_at_point(from, point) else {
            return strategy.unopposed_weight();
        };
        let light_pdf = lights.density(point_pdf, pmf, nee_count).max(1e-6);
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
        Some(PrevVertex::Surface(p)) => p.continuous().then_some((p.pos, p.pdf, p.nee_count)),
        Some(PrevVertex::Phase {
            pos,
            pdf,
            nee_count,
            ..
        }) => Some((*pos, *pdf, *nee_count)),
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
    let from = competing.map_or(Vec3A::ZERO, |(p, _, _)| p);
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
            (Some((_, bounce_pdf, count)), Some(pdf)) if strategy.samples_lights() && pmf > 0.0 => {
                let light_pdf = lights.density(pdf, pmf, count).max(1e-6);
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

/// [`escaped_emission`], one light at a time: `f(light, contribution)` for
/// each light at infinity it adds (`None` for a backdrop), in its order, so
/// the contributions sum to its result bit for bit (the AOV instantiation
/// asserts it in debug builds). The light-path-expression routing's view of
/// an escape: each light's share ends an `L` event with that light's tag.
/// A pair with `escaped_emission`: change one, change both.
fn escaped_split(
    prev: &Option<PrevVertex>,
    lights: &LightList,
    direction: Vec3A,
    mask: RayMask,
    strategy: SamplingStrategy,
    mut f: impl FnMut(Option<usize>, Vec3A),
) {
    if lights.escapes_to_backdrop(mask) {
        for backdrop in lights.backdrops() {
            if let Some((emitted, _)) = backdrop.escaped(Vec3A::ZERO, direction) {
                f(None, emitted * strategy.unopposed_weight());
            }
        }
        return;
    }
    if lights.count() == 0 {
        return;
    }
    let competing = match prev {
        Some(PrevVertex::Surface(p)) => p.continuous().then_some((p.pos, p.pdf, p.nee_count)),
        Some(PrevVertex::Phase {
            pos,
            pdf,
            nee_count,
            ..
        }) => Some((*pos, *pdf, *nee_count)),
        None => None,
    };
    let class = match prev {
        Some(PrevVertex::Surface(p)) => p.class,
        Some(PrevVertex::Phase { class, .. }) => *class,
        None => EVERY_CLASS,
    };
    let from = competing.map_or(Vec3A::ZERO, |(p, _, _)| p);
    for (index, light, pmf) in lights.infinite_indexed_seen_by(from, mask) {
        if !lights.illuminates(index, class) {
            continue;
        }
        if competing.is_some() && lights.nee_only(index) && strategy.samples_lights() {
            continue;
        }
        let Some((emitted, pdf)) = light.escaped(from, direction) else {
            continue;
        };
        let weight = match (competing, pdf) {
            (Some((_, bounce_pdf, count)), Some(pdf)) if strategy.samples_lights() && pmf > 0.0 => {
                let light_pdf = lights.density(pdf, pmf, count).max(1e-6);
                strategy.bounce_weight(bounce_pdf, light_pdf)
            }
            _ => strategy.unopposed_weight(),
        };
        f(Some(index), emitted * weight);
    }
}

/// A mixed fibre vertex's NEE toward the tube's far side: `tr_pass` is the
/// transmittance the ray passing out of curve tubes found; this traces the
/// other one (`ray`, which does not pass) and weights each share by its own.
struct MixedHairNee {
    /// `f_fibres · tr_pass + f_others · tr_wall`.
    shaded: Vec3A,
    tr_pass: Vec3A,
    tr_wall: Vec3A,
    /// Bit `i` set when `eval_lobes` share `i` is a fibre's.
    hair_leaves: u8,
}

impl MixedHairNee {
    /// The transmittance `eval_lobes` share `i` sees.
    fn tr(&self, i: usize) -> Vec3A {
        if self.hair_leaves & (1 << i) != 0 {
            self.tr_pass
        } else {
            self.tr_wall
        }
    }
}

#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn mixed_hair_shadow<const PROFILE: bool>(
    sp: &ShadingPoint,
    world: &World,
    volumes: &Volumes,
    ray: Ray,
    distance: f32,
    tr_pass: Vec3A,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> MixedHairNee {
    let tr_wall = shadow_transmittance::<PROFILE>(world, volumes, &ray, distance, vertex, stats);
    let (hair, other, hair_leaves) = sp.eval_hair_split(ray.direction());
    MixedHairNee {
        shaded: hair * tr_pass + other * tr_wall,
        tr_pass,
        tr_wall,
        hair_leaves,
    }
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
    let Some(through) = visible(world, shadow_ray, distance, vertex, stats) else {
        stats.shadow_occluded += 1;
        return Vec3A::ZERO;
    };
    if volumes.is_empty() {
        return through;
    }
    let mut rng = vertex.new_domain(K_NEE_SHADOW).rng();
    through * volumes.transmittance(shadow_ray, TRACE_T_MIN, distance - TRACE_T_MIN, &mut rng)
}

/// How much of a shadow ray toward a light sample `distance` away surfaces
/// let through, per channel: 1 when nothing blocks it, 0 when an opaque
/// surface does, and otherwise the product over the pass-throughs it crosses
/// of `P = (1 − α) + α · T` — `1 − opacity` at a cutout, and the straight
/// transmittance `T` at a thin wall ([`cutout_through`]). Deterministic in
/// the presence where the bounce side is stochastic ([`pass_cutouts`]), so
/// the product is the lower-variance estimate of the same visibility.
///
/// `vertex` is the domain a thin wall's `T` estimate draws from (MaterialX
/// thin walls; see [`ShadingPoint::straight_transmittance`]), under
/// [`K_NEE_THIN`]; nothing else reads it.
///
/// The one answer to "does this light reach here" that NEE
/// ([`shadow_transmittance`]) and the learned light cache's training share:
/// the cache must train on the visibility the integrator renders with.
#[inline]
pub(crate) fn surface_visibility(
    world: &World,
    ray: &Ray,
    distance: f32,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    visible(world, ray, distance, vertex, stats).unwrap_or(Vec3A::ZERO)
}

/// [`surface_visibility`], `None` where it is zero: NEE branches on that
/// without comparing a colour (`Vec3A == ZERO` at every shadow ray was
/// measurable in a world with no pass-through at all).
#[inline(always)]
fn visible(
    world: &World,
    ray: &Ray,
    distance: f32,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Option<Vec3A> {
    let t_max = shadow_t_max(distance);
    if !world.occluded(ray, TRACE_T_MIN, t_max) {
        return Some(Vec3A::ONE);
    }
    // Blocked — unless only pass-throughs block it, which the any-hit query
    // cannot tell apart. An open segment crosses none either, so it keeps
    // the fast answer.
    if !world.has_pass_throughs() {
        return None;
    }
    let through = cutout_visibility(world, ray, t_max, vertex, stats);
    (through != Vec3A::ZERO).then_some(through)
}

/// [`cutout_through`] out of line: only a blocked shadow ray in a world with
/// pass-throughs reaches it.
#[cold]
#[inline(never)]
fn cutout_visibility(
    world: &World,
    ray: &Ray,
    t_max: f32,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    cutout_through(world, ray, t_max, vertex, stats)
}

/// `T(ω)` of the material hit at `rec` by `ray`: its straight transmittance
/// through the [`ShadingPoint`] the hit resolves to, so it is the BSDF
/// [`Material::resolve`] returns that answers — the one the vertex shades
/// with when the path meets the surface instead.
fn straight_transmittance(
    mat: &dyn Material,
    ray: &Ray,
    rec: &HitRecord,
    sampler: PathSampler,
) -> Vec3A {
    let cos_o = ray.direction().normalize().dot(rec.normal).abs();
    let t = ShadingPoint::new(mat, ray, rec, cos_o).straight_transmittance(ray, sampler);
    if t.is_finite() { t } else { Vec3A::ZERO }
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
/// `(0.001, ∞)`, LLVM propagates those two constants into the kernel, and a
/// caller asking for other bounds costs every ray in every scene. The
/// origin goes to [`resume_before`] the hit, not onto it.
fn restarted(ray: &Ray, t: f32) -> Ray {
    let cone = ray.cone();
    Ray::new(ray.at(t), ray.direction())
        .with_time(ray.time())
        .with_mask(ray.mask())
        .with_curve_exits_ignored(ray.rt().ignore_curve_exits)
        .with_cone(crate::RayCone {
            width: cone.width_at(t * ray.direction().length()),
            spread: cone.spread,
        })
}

/// The next hit along `ray` past the one at `t`, its `t` still measured
/// along `ray`.
fn hit_past<'w>(world: &'w World, ray: &Ray, t: f32) -> Option<WorldHit<'w>> {
    let t = resume_before(t);
    world
        .intersect(&restarted(ray, t), TRACE_T_MIN, f32::INFINITY)
        .map(|mut next| {
            next.rec.t += t;
            next
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
    t - TRACE_T_MIN + t.abs().max(1.0) * 1e-5
}

/// The surface a pass-through walk last crossed, and where: the one hit the
/// restarted segment must not report again.
///
/// [`resume_before`]'s step assumes the kernel's `t` is accurate to better
/// than 1e-5·t. The analytic sphere's was not — its textbook discriminant
/// lost 1e-4 from 8 units away — and the restarted segment met the entry it
/// had just passed, so a hidden sphere light was collected twice on a third
/// of the bounces that crossed it (`every_strategy_agrees_on_a_small_far_hidden_light`).
/// The kernel is fixed; this is the backstop for every primitive: a hit on
/// the same surface, from the same side, within [`RE_HIT_WINDOW`] of the
/// crossing just recorded is a re-hit, since no closed or single-sided
/// surface is entered twice in a row from the same side. "The same surface"
/// is `(geom_id, prim_id, placement)`: inside a nested prototype every
/// placement of a leaf part reports one `geom_id` (`InstanceHitId::As`),
/// and without the placement a second instanced card stacked within the
/// window read as a re-hit of the first
/// (`stacked_placements_sharing_an_id_are_both_crossed`).
#[derive(Clone, Copy)]
pub(super) struct LastCrossing {
    geom_id: u32,
    prim_id: u32,
    placement: u32,
    front_face: bool,
    /// Along the segment's own ray.
    t: f32,
}

/// How far past a crossing, relative to its distance (and absolute below
/// 1, as [`resume_before`]'s step), a hit on the same surface is its
/// rounding rather than a second surface: 1000× the kernel's pinned sphere
/// and cylinder error (`a_small_far_sphere_reports_an_accurate_distance`),
/// and four times the worst triangle re-hit counted on the samples
/// (2.5e-4·t, a hidden rect light crossed nearly edge-on). Safe at that
/// width because the surface identity includes the placement: a real
/// second surface on the same primitive is the other side.
pub(super) const RE_HIT_WINDOW: f32 = 1e-3;

impl LastCrossing {
    /// The crossing of `h`, at `t` along the segment's ray.
    pub(super) fn of(h: &WorldHit, t: f32) -> LastCrossing {
        LastCrossing {
            geom_id: h.geom_id,
            prim_id: h.prim_id,
            placement: h.placement,
            front_face: h.rec.front_face,
            t,
        }
    }

    /// Whether `h`, at `t` along the segment's ray, is this crossing met
    /// again.
    pub(super) fn repeats(self, h: &WorldHit, t: f32) -> bool {
        h.geom_id == self.geom_id
            && h.prim_id == self.prim_id
            && h.placement == self.placement
            && h.rec.front_face == self.front_face
            && t <= self.t + self.t.abs().max(1.0) * RE_HIT_WINDOW
    }
}

/// Where a shadow ray toward a light sample `distance` away stops: short of
/// the light's own surface, which is in the shadow mask when the camera sees
/// it (a visible source occludes other lights). The tracer's 0.001, or a relative step once that falls below the
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
fn shadow_t_max(distance: f32) -> f32 {
    (distance - TRACE_T_MIN).min(distance * (1.0 - 1e-6))
}

/// The fraction of the segment `(0.001, t_max)` of `ray` that pass-throughs
/// let through, per channel: `Π P` over every hit, with
/// `P = (1 − α) + α · T` — `α` the opacity (1 without a cutout) and `T` the
/// straight transmittance (0 without one) — or 0 at the first hit on a
/// material with neither. It follows at most [`MAX_CUTOUT_CROSSINGS`]
/// crossings and then asks once more, where any hit blocks — the same bound
/// [`pass_cutouts`] keeps, which treats the hit past its last crossing as
/// present, so a stack exactly that deep is clear on both sides.
///
/// `P` is Typhoon's `_CombinePresenceAndTransmissionVisibility`. Each `T`
/// is evaluated at the crossing, from `vertex` under [`K_NEE_THIN`].
///
/// Reached through [`surface_visibility`].
fn cutout_through(
    world: &World,
    ray: &Ray,
    t_max: f32,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    if world.has_straight_transmission() {
        return walls_through(world, ray, t_max, vertex, stats);
    }
    // Cutouts alone: the grey product, in the scalar the walk always kept.
    let (mut t, mut segment) = (0.0, ray.clone());
    let mut kept = 1.0;
    let mut last: Option<LastCrossing> = None;
    let mut crossing = 0;
    loop {
        stats.cutout_rays += 1;
        let hit = world.intersect(&segment, TRACE_T_MIN, f32::INFINITY);
        let Some(h) = hit.filter(|h| t + h.rec.t < t_max) else {
            return Vec3A::splat(kept);
        };
        let at = t + h.rec.t;
        if last.is_some_and(|l| l.repeats(&h, at)) {
            // The surface just crossed, reported again: step past it.
            t = resume_before(at);
            segment = restarted(ray, t);
            continue;
        }
        if crossing == MAX_CUTOUT_CROSSINGS || !h.mat.has_cutout() {
            return Vec3A::ZERO;
        }
        kept *= 1.0 - h.mat.opacity(ray, &point_sampled(&h.rec));
        if kept <= 0.0 {
            return Vec3A::ZERO;
        }
        last = Some(LastCrossing::of(&h, at));
        crossing += 1;
        t = resume_before(at);
        segment = restarted(ray, t);
    }
}

/// [`cutout_through`] in a world with thin walls: the same walk, with each
/// crossing's `P` coloured by the straight transmittance. Apart, so a world
/// with cutouts alone walks exactly the loop it always did.
#[inline(never)]
fn walls_through(
    world: &World,
    ray: &Ray,
    t_max: f32,
    vertex: PathSampler,
    stats: &mut RayStats,
) -> Vec3A {
    let (mut t, mut segment) = (0.0, ray.clone());
    let mut kept = Vec3A::ONE;
    let mut last: Option<LastCrossing> = None;
    let mut crossing = 0;
    loop {
        stats.cutout_rays += 1;
        let hit = world.intersect(&segment, TRACE_T_MIN, f32::INFINITY);
        let Some(h) = hit.filter(|h| t + h.rec.t < t_max) else {
            return kept;
        };
        let at = t + h.rec.t;
        if last.is_some_and(|l| l.repeats(&h, at)) {
            t = resume_before(at);
            segment = restarted(ray, t);
            continue;
        }
        let cutout = h.mat.has_cutout();
        let straight = h.mat.has_straight_transmission();
        if crossing == MAX_CUTOUT_CROSSINGS || !(cutout || straight) {
            return Vec3A::ZERO;
        }
        let rec = point_sampled(&h.rec);
        let opacity = if cutout {
            h.mat.opacity(ray, &rec)
        } else {
            1.0
        };
        kept *= if straight {
            let sampler = vertex.new_domain(K_NEE_THIN).new_domain(crossing as i32);
            let tr = straight_transmittance(h.mat, ray, &rec, sampler);
            Vec3A::splat(1.0 - opacity) + opacity * tr
        } else {
            Vec3A::splat(1.0 - opacity)
        };
        if kept.max_element() <= 0.0 {
            return Vec3A::ZERO;
        }
        last = Some(LastCrossing::of(&h, at));
        crossing += 1;
        t = resume_before(at);
        segment = restarted(ray, t);
    }
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
/// A thin wall — a hit whose material has straight transmission — is the
/// same rule with a coloured pass (`thin`; design record "Thin walls are
/// pass-throughs"). Of `P = (1 − α) + α · T`, the fraction of the ray
/// continuing straight, it passes with probability `q = max_c P_c` carrying
/// `P / q`, and otherwise meets the wall carrying `α / (1 − q)`, to scatter
/// through the material without its straight transmission. For a grey `P`
/// that is exactly the cutout rule; it is kept apart all the same, so a
/// world without thin walls draws and computes what it always did.
/// Returns whether any of that happened — whether `thin` speaks for this
/// segment.
///
/// A hidden light source ([`World::is_transparent_emitter`]) is passed
/// always, and recorded in `thin.crossed` for the vertex to collect its
/// emission; it is no pass-through event (`stats.cutout_passes`, the `Ts`
/// count) and spends the same crossing budget. Shadow rays never meet one, so
/// this has no shadow-side twin: their masks leave it out.
///
/// Its shadow-side twin is [`cutout_through`].
#[cold]
#[inline(never)]
fn pass_cutouts<'w>(
    world: &'w World,
    ray: &Ray,
    hit: &mut Option<WorldHit<'w>>,
    vertex: PathSampler,
    thin: &mut ThinWalls,
    stats: &mut RayStats,
) -> bool {
    let emitters = world.has_transparent_emitters();
    if emitters {
        thin.crossed.clear();
    }
    if world.has_straight_transmission() {
        return pass_walls(world, ray, hit, vertex, thin, stats);
    }
    // Cutouts alone: the loop as it always was.
    let mut rng = None;
    let mut passed = 0;
    let mut last: Option<LastCrossing> = None;
    let mut crossing = 0;
    loop {
        let Some(h) = hit.as_ref() else {
            return false;
        };
        if last.is_some_and(|l| l.repeats(h, h.rec.t)) {
            // The surface just crossed, reported again: step past it, on
            // the same crossing budget.
            stats.cutout_rays += 1;
            *hit = hit_past(world, ray, h.rec.t);
            continue;
        }
        if crossing == MAX_CUTOUT_CROSSINGS {
            break;
        }
        crossing += 1;
        if emitters && world.is_transparent_emitter(h.geom_id) {
            last = Some(LastCrossing::of(h, h.rec.t));
            thin.crossed.push(Crossing::at(ray, h, passed));
            stats.cutout_rays += 1;
            *hit = hit_past(world, ray, h.rec.t);
            continue;
        }
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
        last = Some(LastCrossing::of(h, h.rec.t));
        passed += 1;
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
    false
}

/// [`pass_cutouts`] in a world with thin walls: cutouts as there, and the
/// coloured pass at each thin wall, recorded in `thin`. Apart, so a world
/// with cutouts alone runs exactly the loop it always did (folded into one,
/// `materialx_cutout` ran 0.7% more instructions).
#[inline(never)]
fn pass_walls<'w>(
    world: &'w World,
    ray: &Ray,
    hit: &mut Option<WorldHit<'w>>,
    vertex: PathSampler,
    thin: &mut ThinWalls,
    stats: &mut RayStats,
) -> bool {
    thin.passes.clear();
    thin.kinds.clear();
    thin.first = None;
    thin.meet = 1.0;
    thin.reduced = false;
    let emitters = world.has_transparent_emitters();
    let mut rng = None;
    let mut last: Option<LastCrossing> = None;
    let mut crossing = 0;
    while let Some(h) = hit.as_ref() {
        if last.is_some_and(|l| l.repeats(h, h.rec.t)) {
            stats.cutout_rays += 1;
            *hit = hit_past(world, ray, h.rec.t);
            continue;
        }
        if crossing == MAX_CUTOUT_CROSSINGS {
            break;
        }
        // Numbered as the walk always was, emitter crossings included: the
        // thin-wall draws below are keyed on it.
        let crossing_index = crossing;
        crossing += 1;
        if emitters && world.is_transparent_emitter(h.geom_id) {
            last = Some(LastCrossing::of(h, h.rec.t));
            thin.crossed.push(Crossing::at(ray, h, thin.kinds.len()));
            stats.cutout_rays += 1;
            *hit = hit_past(world, ray, h.rec.t);
            continue;
        }
        let cutout = h.mat.has_cutout();
        let straight = h.mat.has_straight_transmission();
        if !(cutout || straight) {
            break;
        }
        let rec = point_sampled(&h.rec);
        let opacity = if cutout {
            h.mat.opacity(ray, &rec)
        } else {
            1.0
        };
        if straight {
            let draws = vertex.new_domain(K_THIN).new_domain(crossing_index as i32);
            let tr = straight_transmittance(h.mat, ray, &rec, draws.new_domain(0));
            let p = Vec3A::splat(1.0 - opacity) + opacity * tr;
            let q = p.max_element();
            let q = if q.is_finite() {
                q.clamp(0.0, 1.0)
            } else {
                0.0
            };
            if draws.new_domain(1).draw_rnd_f32::<1>()[0] >= q {
                thin.meet = opacity / (1.0 - q);
                thin.reduced = true;
                break;
            }
            thin.passes.push((h.rec.t, p / q));
            if thin.first.is_none() {
                thin.first = Some((h.rec.t, h.rec));
            }
        } else {
            if opacity >= 1.0 {
                break;
            }
            let u = rng
                .get_or_insert_with(|| vertex.new_domain(K_CUTOUT).rng())
                .next_f32();
            if u < opacity {
                break;
            }
        }
        last = Some(LastCrossing::of(h, h.rec.t));
        thin.kinds.push(straight);
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
    thin.reduced || !thin.passes.is_empty()
}

/// The camera's first hit when its segment passed a thin wall before `t`:
/// the wall. A thin wall is glass, not a hole, so the data AOVs keep seeing
/// it where it was a vertex before it became a pass-through — only a
/// cutout is not a hit. The normal is the geometric one, facing the ray:
/// the wall was never shaded.
fn first_wall(thin: &ThinWalls, walls: bool, t: f32) -> Option<FirstHit> {
    let (t_wall, rec) = thin.first.filter(|_| walls)?;
    (t_wall < t).then_some(FirstHit::Surface {
        p: rec.p,
        n: rec.normal,
        uv: rec.uv,
    })
}

/// The arriving segment's volume event when it passed thin walls: the
/// regions sampled a piece at a time between the walls, each piece's
/// transmittance, emission and scatter weight carrying the pass weights of
/// the walls before it — a wall past the event never mattered. With no
/// regions, only the pass weights before `t_lim`. Delta tracking is
/// memoryless, so sampling `(0.001, t_lim)` in pieces is the same process
/// as sampling it whole.
///
/// A segment that reaches the thin wall it met (no carried-medium event
/// first) also carries the meet weight `α / (1 − q)`
/// in its transmittance: that vertex is the share of the wall that is not
/// its straight transmission. Folded in here rather than applied to the
/// vertex, which would cost every vertex of every scene a branch.
#[cold]
#[inline(never)]
fn volume_event_past_walls(
    volumes: &Volumes,
    ray: &Ray,
    t_surf: f32,
    t_med: f32,
    thin: &ThinWalls,
    vertex: PathSampler,
) -> VolumeEvent {
    let t_lim = t_surf.min(t_med);
    let meet = if t_med < t_surf { 1.0 } else { thin.meet };
    if volumes.is_empty() {
        return VolumeEvent::Passthrough {
            transmittance: thin.before(t_lim) * meet,
            emitted: Vec3A::ZERO,
        };
    }
    let mut rng = vertex.new_domain(K_VOLUME).rng();
    let (mut w, mut emitted, mut t0) = (Vec3A::ONE, Vec3A::ZERO, TRACE_T_MIN);
    let walls = thin.passes.iter().take_while(|p| p.0 < t_lim).copied();
    for (t1, pass) in walls.chain(std::iter::once((t_lim, Vec3A::ONE))) {
        match volumes.sample_interaction(ray, t0, t1, &mut rng) {
            VolumeEvent::Scatter {
                t,
                p,
                weight,
                phase,
                emitted: e,
                class,
            } => {
                return VolumeEvent::Scatter {
                    t,
                    p,
                    weight: w * weight,
                    phase,
                    emitted: emitted + w * e,
                    class,
                };
            }
            VolumeEvent::Passthrough {
                transmittance,
                emitted: e,
            } => {
                emitted += w * e;
                w *= transmittance * pass;
            }
        }
        t0 = t1;
    }
    VolumeEvent::Passthrough {
        transmittance: w * meet,
        emitted,
    }
}

/// The arriving segment's volume event when it crossed hidden light sources:
/// [`volume_event_past_walls`] (or the plain tracking when `walls` is false)
/// sampled a piece at a time between the crossings too, so that each
/// crossing learns the weight the segment carried to it (`reach`) — or that
/// an event before it means it was never reached. Delta tracking is
/// memoryless, so the pieces are the same process as the whole.
///
/// The carried medium's share of each reach is the caller's
/// ([`medium_arrival`]); a crossing past `t_med` is not reached.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn volume_event_crossing(
    volumes: &Volumes,
    ray: &Ray,
    t_surf: f32,
    t_med: f32,
    thin: &mut ThinWalls,
    walls: bool,
    vertex: PathSampler,
) -> VolumeEvent {
    let t_lim = t_surf.min(t_med);
    let meet = if !walls || t_med < t_surf {
        1.0
    } else {
        thin.meet
    };
    let passes: &[(f32, Vec3A)] = if walls { &thin.passes } else { &[] };
    let crossed = &mut thin.crossed;
    let mut rng = (!volumes.is_empty()).then(|| vertex.new_domain(K_VOLUME).rng());
    let (mut w, mut emitted, mut t0) = (Vec3A::ONE, Vec3A::ZERO, TRACE_T_MIN);
    let (mut i, mut j) = (0, 0);
    loop {
        // The next breakpoint: a wall, a crossing, or the segment's end.
        let wall = passes.get(i).map_or(f32::INFINITY, |p| p.0);
        let cross = crossed.get(j).map_or(f32::INFINITY, |c| c.t);
        let t1 = wall.min(cross).min(t_lim);
        if let Some(rng) = &mut rng {
            match volumes.sample_interaction(ray, t0, t1, rng) {
                VolumeEvent::Scatter {
                    t,
                    p,
                    weight,
                    phase,
                    emitted: e,
                    class,
                } => {
                    return VolumeEvent::Scatter {
                        t,
                        p,
                        weight: w * weight,
                        phase,
                        emitted: emitted + w * e,
                        class,
                    };
                }
                VolumeEvent::Passthrough {
                    transmittance,
                    emitted: e,
                } => {
                    emitted += w * e;
                    w *= transmittance;
                }
            }
        }
        if t1 >= t_lim {
            break;
        }
        if wall <= cross {
            w *= passes[i].1;
            i += 1;
        } else {
            crossed[j].reach = Some(w);
            j += 1;
        }
        t0 = t1;
    }
    VolumeEvent::Passthrough {
        transmittance: w * meet,
        emitted,
    }
}

/// The carried medium's attenuation of a segment that reaches `t` along
/// `ray` without scattering in it. For a *scattering* medium the arrival
/// already paid e^{−σ̄·t} through the free-flight competition, so only the
/// chromatic correction e^{(σ̄−σₜ)·t} remains — exactly ONE for gray media.
/// Non-scattering media (glass tint) keep pure Beer-Lambert.
#[inline(always)]
fn medium_arrival(ray: &Ray, t: f32) -> Vec3A {
    match ray.medium() {
        Some(m) if m.is_scattering() => {
            let sigma_bar = m.sigma_t_max().max(1e-4);
            let e = (Vec3A::splat(sigma_bar) - (m.sigma_a + m.sigma_s)) * t;
            exp3(e)
        }
        Some(m) => m.transmittance(t),
        None => Vec3A::ONE,
    }
}

/// Hands the hidden light sources a segment reached to `last`, the record of
/// the vertex the segment left: each one's emission, times its `reach` and
/// `scale(t)`, MIS-weighted for its own light exactly as a bounce that ended
/// on it would be ([`bounce_emission_weight`]) — or whole after a vertex
/// with no MIS record. With light path expressions, each is its own `L` event
/// after the pass-throughs before it (`arrival`'s prefix).
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn collect_crossings(
    crossed: &[Crossing],
    scale: impl Fn(f32) -> Vec3A,
    prev: &Option<PrevVertex>,
    lights: &LightList,
    strategy: SamplingStrategy,
    last: &mut VertexRec,
    routing: Option<&RouteCtx>,
    route: &mut Route,
    arrival: Arrival,
) {
    for c in crossed {
        let Some(reach) = c.reach else {
            continue;
        };
        if c.emitted == Vec3A::ZERO {
            continue;
        }
        let e = reach * scale(c.t) * c.emitted;
        let w = prev.as_ref().map_or(1.0, |p| {
            bounce_emission_weight_at(p, lights, c.geom_id, c.p, strategy)
        });
        last.crossed += e * w;
        last.crossed_raw += e;
        if let Some(ctx) = routing {
            route.cross(
                ctx.emitter(lights, c.geom_id),
                e * w,
                arrival.prefix(c.passes),
            );
        }
    }
}

/// Direct lighting at a volume-region scatter point. The exact mirror of
/// the surface NEE block: same light-selection strategy, with the
/// phase function (value == pdf for the HG mixture) in place of
/// `brdf·cos`, and the same phase pdf as the competing bounce density that
/// `bounce_emission_weight`'s `Phase` arm uses.
///
/// `count` light samples, averaged, each weighted against the phase bounce
/// with the count times the light density (`LightList::density`), as the
/// surface block does. With light path expressions each sample is its own
/// `L` event at the open route vertex, so `route.vertex` must have been
/// called first.
#[allow(clippy::too_many_arguments)]
fn volume_nee<const PROFILE: bool>(
    p: Vec3A,
    wi: Vec3A,
    phase: &PhaseMix,
    class: u16,
    world: &World,
    volumes: &Volumes,
    lights: &LightList,
    strategy: SamplingStrategy,
    count: u32,
    vertex: PathSampler,
    time: f32,
    routing: Option<&RouteCtx>,
    route: &mut Route,
    stats: &mut RayStats,
) -> Vec3A {
    if !strategy.samples_lights() {
        return Vec3A::ZERO;
    }
    let _p = profile::scope_if::<PROFILE>(Section::VolumeLighting);
    let mut total = Vec3A::ZERO;
    for i in 0..count {
        let sampler = nee_sampler(vertex, i);
        let nee = sampler.new_domain(K_NEE).draw_sample_f32::<4>();
        // As at a surface: a light that does not illuminate this region gives
        // nothing, and its pick probability stays what it was.
        let Some((index, pmf)) = lights.pick_index_at(p, stratified_pick(nee[0], count, i)) else {
            continue;
        };
        if !lights.illuminates(index, class) {
            continue;
        }
        let light = lights.light(index);
        let Some(s) = light.sample_li(p, nee[1], nee[2]) else {
            continue;
        };
        stats.light_samples += 1;
        // As at a surface vertex: no shadow ray for a connection already known
        // to carry nothing (a one-sided light seen from behind). Bit-identical,
        // since the ray's own draws come from `K_NEE_SHADOW`.
        let phase_val = phase.pdf(wi.dot(s.direction));
        if s.radiance * phase_val == Vec3A::ZERO {
            continue;
        }
        let shadow_ray = Ray::new(p, s.direction)
            .with_time(time)
            .with_mask(lights.shadow_mask(index));
        let tr = shadow_transmittance::<PROFILE>(
            world,
            volumes,
            &shadow_ray,
            s.distance,
            sampler,
            stats,
        );
        if tr == Vec3A::ZERO {
            continue;
        }
        // `count` times the per-sample density: dividing by it is what
        // averages the `count` samples (Veach's multi-sample estimator sums
        // `w · f / (n_i · p_i)`), so no further `1/count` is applied.
        let light_pdf = lights.density(s.pdf, pmf, count).max(1e-6);
        // The phase function is its own pdf, in solid angle. A shadow-linked
        // light has no competing bounce strategy (see `bounce_emission_weight`).
        let weight = if lights.nee_only(index) {
            1.0
        } else {
            strategy.light_weight(light_pdf, PdfSolidAngle::from_measure(phase_val))
        };
        let contribution = s.radiance * phase_val * tr * weight / light_pdf.get();
        total += contribution;
        if let Some(ctx) = routing {
            route.nee(
                ctx.light(index),
                std::iter::once((ctx.volume(), contribution)),
            );
        }
    }
    total
}

/// One light sample at a surface vertex, drawn from `nee_v` with its pick
/// coordinate `pick_u`: the sample's share of the vertex's NEE term, and its
/// `L` event when routed. `nee_count` is the vertex's light sample count
/// (the MIS density's factor), `class_here` the receiver's light-link class,
/// `exiting` a subsurface walk's exit (no lobe event of its own).
///
/// `inline(always)`: the vertex's first sample is this block as it stood
/// before several samples existed, straight-line in `trace_path`. Wrapping
/// the block in a `for` over the count cost cornellbox 0.57% of its
/// instructions at one sample (callgrind, 2 spp: the range iterator and the
/// `then` closure inside it stopped folding), so the extra samples loop in
/// [`extra_surface_light_samples`] over another inlined copy instead. The
/// inputs are plain parameters, not a struct: a struct built per vertex and
/// handed to that out-of-line loop was materialised in memory and reloaded
/// at every vertex, loop taken or not (+0.96%; by reference, +1.4%).
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn surface_light_sample<const PROFILE: bool>(
    world: &World,
    volumes: &Volumes,
    lights: &LightList,
    strategy: SamplingStrategy,
    ray: &Ray,
    rec: &HitRecord,
    sp: &ShadingPoint<'_>,
    class_here: u16,
    guiding_here: Option<&GuidingContext<'_>>,
    nee_count: u32,
    exiting: bool,
    routing: Option<&RouteCtx>,
    nee_v: PathSampler,
    pick_u: f32,
    nee_s: [f32; 4],
    route: &mut Route,
    split: &mut LobeSplit,
    stats: &mut RayStats,
) -> Vec3A {
    let mut nee = Vec3A::ZERO;
    // `sample_li` returns `None` when the light cannot be reached from
    // this point at all — below a dome's horizon, or a degenerate
    // coincident point.
    // A picked light that does not illuminate this receiver contributes
    // nothing, and its pick probability stays what it was, so the bounce
    // side, which zeroes the same light, still describes one strategy.
    // Plain `if let` chains, no closures: this function is inlined twice
    // (the vertex's first sample, and the cold loop for the rest), and a
    // closure with two call sites is one LLVM no longer inlines — the pick
    // and the visibility test below each became an out-of-line call per
    // sample, 0.65% of cornellbox's instructions apiece (callgrind, 2 spp).
    if strategy.samples_lights()
        && let Some((light_index, pmf)) = lights.pick_index_at(rec.p, pick_u)
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
        let connection = if ls.radiance != Vec3A::ZERO
            && let Some((f, pdf)) = sp.eval(ray, light_dir_unit)
            && ls.radiance * f != Vec3A::ZERO
        {
            let shadow_ray = Ray::new(rec.p, light_dir_unit)
                .with_time(ray.time())
                .with_mask(lights.shadow_mask(light_index))
                .with_curve_exits_ignored(sp.passes_out_of_curves());
            let tr = shadow_transmittance::<PROFILE>(
                world,
                volumes,
                &shadow_ray,
                ls.distance,
                nee_v,
                stats,
            );
            if tr != Vec3A::ZERO {
                Some((f, pdf, tr))
            } else {
                None
            }
        } else {
            None
        };
        if let Some((brdf_value, brdf_pdf, shadow_tr)) = connection {
            // Toward a light on the tube's far side, a fibre sharing the
            // closure with a transmitting leaf needs a second answer: the
            // ray above passed out of the strand (the fibre's light); the
            // other leaf's meets the far wall. A ray that passes sees a
            // subset of the hits one that does not sees, so the connection
            // already stands or falls with it. Rare, so out of line — the
            // common path stays the expression it always was.
            let mixed = if sp.mixes_hair() && rec.normal.dot(light_dir_unit) < 0.0 {
                Some(mixed_hair_shadow::<PROFILE>(
                    sp,
                    world,
                    volumes,
                    Ray::new(rec.p, light_dir_unit)
                        .with_time(ray.time())
                        .with_mask(lights.shadow_mask(light_index)),
                    ls.distance,
                    shadow_tr,
                    nee_v,
                    stats,
                ))
            } else {
                None
            };
            let light_pdf = lights.density(ls.pdf, pmf, nee_count).max(1e-6);
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
            nee = match &mixed {
                None => ls.radiance * brdf_value * shadow_tr * weight / light_pdf.get(),
                Some(m) => ls.radiance * m.shaded * weight / light_pdf.get(),
            };
            if let Some(ctx) = routing {
                // Each lobe's share, in the beauty's own expression. A
                // walk's exit is part of the walk's event: no event here.
                sp.eval_lobes(ray, light_dir_unit, split);
                let light_pdf = light_pdf.get();
                route.nee(
                    ctx.light(light_index),
                    split.iter().enumerate().map(|(i, (e, f))| {
                        let tr = mixed.as_ref().map_or(shadow_tr, |m| m.tr(i));
                        (
                            if exiting { NO_EVENT } else { ctx.lobe(e) },
                            ls.radiance * f * tr * weight / light_pdf,
                        )
                    }),
                );
            }
        }
    }
    nee
}

/// Light samples 1 … `count − 1` of a surface vertex, each from its own
/// sub-domain with its stratified pick (`nee_sampler`, `stratified_pick`):
/// their summed share of the vertex's NEE term. Out of line and cold, so the
/// one-sample vertex never sees the loop; its parameters are
/// [`surface_light_sample`]'s.
#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn extra_surface_light_samples<const PROFILE: bool>(
    world: &World,
    volumes: &Volumes,
    lights: &LightList,
    strategy: SamplingStrategy,
    ray: &Ray,
    rec: &HitRecord,
    sp: &ShadingPoint<'_>,
    class_here: u16,
    guiding_here: Option<&GuidingContext<'_>>,
    nee_count: u32,
    exiting: bool,
    routing: Option<&RouteCtx>,
    v: PathSampler,
    route: &mut Route,
    split: &mut LobeSplit,
    stats: &mut RayStats,
) -> Vec3A {
    let mut total = Vec3A::ZERO;
    for index in 1..nee_count {
        let nee_v = nee_sampler(v, index);
        let nee_s = nee_v.new_domain(K_NEE).draw_sample_f32::<4>();
        let pick_u = stratified_pick(nee_s[0], nee_count, index);
        total += surface_light_sample::<PROFILE>(
            world,
            volumes,
            lights,
            strategy,
            ray,
            rec,
            sp,
            class_here,
            guiding_here,
            nee_count,
            exiting,
            routing,
            nee_v,
            pick_u,
            nee_s,
            route,
            split,
            stats,
        );
    }
    total
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
///
/// `AOV` is the film's instantiation: it also records what the camera ray met
/// at vertex 0 in `scratch.first` (see [`FirstHit`]). It observes only — no
/// draw, no weight — so both instantiations return the same radiance, and
/// with `AOV = false` every `if AOV` block compiles away, leaving the
/// function the beauty-only render has always run.
#[inline(always)]
pub(super) fn trace_path<const PROFILE: bool, const AOV: bool>(
    r: &Ray,
    cx: &PathContext<'_>,
    sampler: PathSampler,
    train_out: &mut Vec<SampleData>,
    scratch: &mut PathScratch,
    stats: &mut RayStats,
) -> Vec3A {
    let PathContext {
        world,
        lights,
        volumes,
        depth,
        strategy,
        indirect_clamp,
        guiding,
        light_samples,
        light_samples_indirect,
    } = *cx;
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
    let first = &mut scratch.first;
    if AOV {
        *first = FirstHit::Escaped;
    }
    // The routing: the render's expressions, and this path's record. Both
    // `None`/unused outside the AOV instantiation.
    let route_ctx = if AOV {
        scratch.route_ctx.as_deref()
    } else {
        None
    };
    let routing = route_ctx.filter(|c| c.routes());
    let albedo_on = route_ctx.is_some_and(|c| c.albedo);
    let diffuse_filter_on = route_ctx.is_some_and(|c| c.diffuse_filter);
    let route = &mut scratch.route;
    if AOV {
        route.begin();
    }
    let split = &mut scratch.nee_split;
    let bounce_split = &mut scratch.bounce_split;
    let thin = &mut scratch.thin;

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
                let mut hit = {
                    let _p = profile::scope_if::<PROFILE>(Section::Trace);
                    world.intersect(&ray, TRACE_T_MIN, f32::INFINITY)
                };
                let cut0 = stats.cutout_passes;
                let mut walls = false;
                let mut crossed = false;
                if world.has_bounce_pass_throughs() {
                    walls = pass_cutouts(world, &ray, &mut hit, v, thin, stats);
                    crossed = world.has_transparent_emitters() && !thin.crossed.is_empty();
                }
                let arrival = if AOV {
                    let a = match routing {
                        Some(ctx) if walls => route.arrival_through(ctx, &thin.kinds),
                        _ => Arrival::cutouts((stats.cutout_passes - cut0) as u16),
                    };
                    route.terminal_arrival(a);
                    a
                } else {
                    Arrival::default()
                };
                if crossed {
                    // Every hidden light on the way, attenuated as the hit
                    // below is.
                    let ThinWalls {
                        passes, crossed, ..
                    } = &mut *thin;
                    let mut rng = (!volumes.is_empty()).then(|| v.new_domain(K_CROSS).rng());
                    for c in crossed.iter_mut() {
                        let mut reach = if walls {
                            passes_before(passes, c.t)
                        } else {
                            Vec3A::ONE
                        };
                        if let Some(m) = ray.medium() {
                            reach *= m.transmittance(c.t);
                        }
                        if let Some(rng) = &mut rng {
                            reach *= volumes.transmittance(&ray, TRACE_T_MIN, c.t, rng);
                        }
                        c.reach = Some(reach);
                    }
                    let last = records.last_mut().expect("prev implies a record");
                    collect_crossings(
                        crossed,
                        |_| Vec3A::ONE,
                        &prev,
                        lights,
                        strategy,
                        last,
                        routing,
                        route,
                        arrival,
                    );
                }
                if let Some(hit) = hit {
                    let cos_o = ray.direction().normalize().dot(hit.rec.normal).abs();
                    let mut emitted = hit.mat.emitted_at(&ray, &hit.rec, cos_o);
                    if emitted.length_squared() > 0.0 {
                        if let Some(m) = ray.medium() {
                            emitted *= m.transmittance(hit.rec.t);
                        }
                        if !volumes.is_empty() {
                            let mut rng = v.new_domain(K_VOLUME).rng();
                            emitted *=
                                volumes.transmittance(&ray, TRACE_T_MIN, hit.rec.t, &mut rng);
                        }
                        if walls {
                            emitted *= thin.before(hit.rec.t) * thin.meet;
                        }
                        let last = records.last_mut().expect("prev implies a record");
                        last.next_emit = emitted;
                        last.next_emit_weight = bounce_emission_weight(p, lights, &hit, strategy);
                        if let Some(ctx) = routing {
                            route.next_emit(ctx.emitter(lights, hit.geom_id));
                        }
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
            world.intersect(&ray, TRACE_T_MIN, f32::INFINITY)
        };
        // Patched in place, on the cold side only: an `if` that yields the
        // hit from either arm copies all of it at every vertex (+0.8% of
        // cornellbox's instructions, which has no cutout).
        let cut0 = stats.cutout_passes;
        let mut walls = false;
        let mut crossed = false;
        if world.has_bounce_pass_throughs() && !exiting {
            walls = pass_cutouts(world, &ray, &mut hit_opt, v, thin, stats);
            crossed = world.has_transparent_emitters() && !thin.crossed.is_empty();
        }
        // Cutouts and thin walls passed on the way here: `Ts` events before
        // this vertex.
        let arrival_ts = if AOV {
            match routing {
                Some(ctx) if walls => route.arrival_through(ctx, &thin.kinds),
                _ => Arrival::cutouts((stats.cutout_passes - cut0) as u16),
            }
        } else {
            Arrival::default()
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
        // Thin walls passed on the way are the delta interfaces they were as
        // vertices: their pass weights tint the albedo found behind them.
        if albedo_on && walls {
            route.albedo_through(thin.before(t_surf.min(t_med)));
        }
        // A walk's exit has no arriving segment outside the object.
        let event = if crossed {
            volume_event_crossing(volumes, &ray, t_surf, t_med, thin, walls, v)
        } else if walls {
            volume_event_past_walls(volumes, &ray, t_surf, t_med, thin, v)
        } else if volumes.is_empty() || exiting {
            VolumeEvent::Passthrough {
                transmittance: Vec3A::ONE,
                emitted: Vec3A::ZERO,
            }
        } else {
            let _p = profile::scope_if::<PROFILE>(Section::Volume);
            let mut rng = v.new_domain(K_VOLUME).rng();
            volumes.sample_interaction(&ray, TRACE_T_MIN, t_surf.min(t_med), &mut rng)
        };

        // The hidden lights the segment reached before its event belong to
        // the vertex it left, like the emission at a bounce's end.
        if crossed && let Some(last) = records.last_mut() {
            collect_crossings(
                &thin.crossed,
                |t| medium_arrival(&ray, t),
                &prev,
                lights,
                strategy,
                last,
                routing,
                route,
                arrival_ts,
            );
        }

        let (vol_tr, vol_emit) = match event {
            VolumeEvent::Scatter {
                t,
                p,
                weight,
                phase,
                emitted,
                class,
            } => {
                stats.volume_scatters += 1;
                // === Volume-region scatter vertex ===
                if AOV && records.is_empty() {
                    *first = first_wall(thin, walls, t).unwrap_or(FirstHit::Volume { p });
                }
                let wi = ray.direction().normalize();
                let ps = v.new_domain(K_PHASE).draw_sample_f32::<4>();
                let dir = phase.sample(wi, ps[0], [ps[1], ps[2]]);
                let phase_pdf = phase.pdf(wi.dot(dir)).max(1e-6);
                // The routed vertex opens before its NEE events: `V`, then
                // each light sample's `L`.
                if routing.is_some() {
                    route.vertex(arrival_ts);
                }
                let nee_count = if records.is_empty() {
                    light_samples
                } else {
                    light_samples_indirect
                };
                let nee = volume_nee::<PROFILE>(
                    p,
                    wi,
                    &phase,
                    class,
                    world,
                    volumes,
                    lights,
                    strategy,
                    nee_count,
                    v,
                    ray.time(),
                    routing,
                    route,
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
                    crossed: Vec3A::ZERO,
                    crossed_raw: Vec3A::ZERO,
                    train: None,
                };
                beta *= weight;
                let survived =
                    russian_roulette(records.len(), v, &mut beta, &mut vrec.factor, stats)
                        .is_some();
                if !survived {
                    vrec.factor = Vec3A::ZERO;
                }
                if let Some(ctx) = routing {
                    // The vertex and its NEE events were recorded by
                    // `volume_nee`; the bounce leaves through the phase.
                    route.bounce(std::iter::once((ctx.volume(), vrec.factor)));
                }
                if albedo_on {
                    route.albedo_at(Vec3A::ONE);
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
                    nee_count,
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
            if AOV && records.is_empty() {
                *first = first_wall(thin, walls, t_med).unwrap_or(FirstHit::Volume { p: pos });
            }
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
            let factor = medium.sigma_s / sigma_bar * exp3(e);
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
                crossed: Vec3A::ZERO,
                crossed_raw: Vec3A::ZERO,
                train: None,
            };
            beta *= vol_tr * factor;
            let survived =
                russian_roulette(records.len(), v, &mut beta, &mut vrec.factor, stats).is_some();
            if !survived {
                vrec.factor = Vec3A::ZERO;
            }
            if let Some(ctx) = routing {
                // A medium scatter is a `V` event; it runs no NEE.
                route.vertex(arrival_ts);
                route.bounce(std::iter::once((ctx.volume(), vrec.factor)));
            }
            if albedo_on {
                route.albedo_at(Vec3A::ONE);
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
            if AOV
                && records.is_empty()
                && let Some(wall) = first_wall(thin, walls, f32::INFINITY)
            {
                *first = wall;
            }
            // === Background ===
            // A ray leaving the scene is how lights at infinity are found
            // by chance, so it is a bounce-side MIS event just like hitting
            // an emissive surface.
            let unit_direction = Vec3A::normalize(ray.direction());
            // With light path expressions, each light's share is routed by
            // its own `L`, and the background is their sum — the additions
            // `escaped_emission` makes, in its order, so bit for bit its
            // answer (asserted in debug builds) without evaluating every
            // light at infinity twice.
            let background = if let Some(ctx) = routing {
                route.escape_lights_begin();
                escaped_split(
                    &prev,
                    lights,
                    unit_direction,
                    ray.mask(),
                    strategy,
                    |light, e| {
                        let sym = light.map_or(ctx.backdrop(), |i| ctx.light(i));
                        route.escape_light(sym, e);
                    },
                );
                let total = route.escaped_total();
                debug_assert!(
                    total.to_array().map(f32::to_bits)
                        == escaped_emission(&prev, lights, unit_direction, ray.mask(), strategy)
                            .to_array()
                            .map(f32::to_bits),
                    "escaped_split must sum to escaped_emission"
                );
                total
            } else {
                escaped_emission(&prev, lights, unit_direction, ray.mask(), strategy)
            };
            // Segment emission is already weighted; the background pays the
            // volume transmittance of the final segment.
            terminal = vol_emit + vol_tr * background;
            if AOV {
                route.terminal_arrival(arrival_ts);
                if routing.is_some() {
                    route.escape_begin(vol_emit, vol_tr, background);
                }
            }
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
        // transmittance times the carried medium's (for a scattering medium
        // only its chromatic correction, t_med ≥ t_surf having paid the rest).
        let med_arrival = medium_arrival(&ray, rec.t);
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
        let mut sp = {
            let _p = profile::scope_if::<PROFILE>(Section::EvalBsdfs);
            ShadingPoint::new(mat, &ray, &rec, cos_o)
        };
        // Met a thin wall it could have passed: the straight transmission is
        // the passes' to carry, not this vertex's.
        if walls && thin.reduced {
            sp.exclude_straight();
        }
        if diffuse_filter_on && records.is_empty() {
            // The first hit's diffuse colour: the raw light AOVs' divisor.
            route.diffuse_filter = sp.diffuse_filter();
        }
        if AOV && records.is_empty() {
            *first = first_wall(thin, walls, rec.t).unwrap_or(FirstHit::Surface {
                p: rec.p,
                n: sp.normal(),
                uv: rec.uv,
            });
        }
        let emitted = sp.emitted();
        let mut emit_here = Vec3A::ZERO;
        match &prev {
            Some(p) => {
                if emitted.length_squared() > 0.0 {
                    let last = records.last_mut().expect("prev implies a record");
                    last.next_emit = atten * emitted;
                    last.next_emit_weight = bounce_emission_weight(p, lights, &hit, strategy);
                    if let Some(ctx) = routing {
                        route.next_emit(ctx.emitter(lights, hit.geom_id));
                    }
                }
            }
            None => emit_here = emitted,
        }
        if let Some(ctx) = routing {
            route.vertex(arrival_ts);
            if emit_here.length_squared() > 0.0 {
                route.emit(ctx.emitter(lights, hit.geom_id));
            }
        }

        // Guide secondary bounces only: primary vertices vary per pixel far
        // below the guiding field's spatial resolution, so guiding them adds
        // parallax-mismatch variance instead of removing any.
        //
        // Nor where a fibre shares the closure with a transmitting leaf: a
        // guided direction would carry both shares on one ray, and only one
        // of them passes out of the strand (`ResolvedClosure::scatter_choosing`
        // splits them; the guide branch has no split). Off for both MIS
        // sides alike, as the decision is the vertex's.
        let guiding_here = if prev.is_some() && !sp.mixes_hair() {
            guiding
        } else {
            None
        };

        // === 1. Direct Lighting via Light Sampling ===
        // The light strategy is "pick one light with the light list's
        // selection probability `pmf` (power-proportional by default), then
        // sample a point on it with its own `sample_li`", so its solid-angle
        // density is `light.pdf · pmf`. `bounce_emission_weight` evaluates
        // the same expression for a bounce-hit light — both MIS weights must
        // describe the same strategy or emission is double-counted.
        //
        // `nee_count` light samples here — N at the first vertex of the path,
        // M after it — each from its own sub-domain, with their picks
        // stratified (`stratified_pick`), and each weighted against the
        // bounce with, and divided by, the count times the light density
        // (multi-sample MIS, `LightList::density`: Veach's estimator sums
        // `w · f / (n_i · p_i)`, so the division is also the average). With
        // one sample the loop is the block it always was, bit for bit.
        let lighting = profile::scope_if::<PROFILE>(Section::SurfaceLighting);
        let nee_count = if records.is_empty() {
            light_samples
        } else {
            light_samples_indirect
        };
        // The first sample draws from the vertex itself, as the one sample
        // always did; the others, when there are any, from their own
        // sub-domains in `extra_surface_light_samples`.
        let nee_s = v.new_domain(K_NEE).draw_sample_f32::<4>();
        let mut nee = surface_light_sample::<PROFILE>(
            world,
            volumes,
            lights,
            strategy,
            &ray,
            &rec,
            &sp,
            class_here,
            guiding_here,
            nee_count,
            exiting,
            routing,
            v,
            stratified_pick(nee_s[0], nee_count, 0),
            nee_s,
            route,
            split,
            stats,
        );
        if nee_count > 1 {
            nee += extra_surface_light_samples::<PROFILE>(
                world,
                volumes,
                lights,
                strategy,
                &ray,
                &rec,
                &sp,
                class_here,
                guiding_here,
                nee_count,
                exiting,
                routing,
                v,
                route,
                split,
                stats,
            );
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
            crossed: Vec3A::ZERO,
            crossed_raw: Vec3A::ZERO,
            train: None,
        };

        // === 2. Indirect Lighting via BSDF (or guided) Sampling ===
        let mut bounce = {
            let _p = profile::scope_if::<PROFILE>(Section::Bounce);
            sample_bounce_direction::<AOV>(
                &ray,
                &rec,
                &sp,
                guiding_here,
                v,
                routing.is_some().then_some(&mut *bounce_split),
            )
        };
        // A subsurface leaf was selected: walk the interior now. The walk is
        // part of this surface event — the entry's record carries its weight
        // and the exit is the next vertex — so it spends no path depth.
        // (Replaced only when it walks: passing the sample through a `match`
        // or a rebinding moves all of it at every vertex.)
        // The event a walk's entry is, known only before the walk replaces
        // the sample with its exit.
        let walk_event = if AOV {
            bounce
                .as_ref()
                .filter(|s| s.subsurface.is_some())
                .map(|s| sp.delta_event(s))
        } else {
            None
        };
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
        // The albedo: through delta interfaces to the first surface that is
        // not one (a walk's entry is a surface, not an interface).
        if albedo_on && !exiting {
            match &bounce {
                Some(s) if s.delta && walk_event.is_none() => {
                    route.albedo_through(s.value / s.pdf);
                }
                _ => route.albedo_at(sp.albedo()),
            }
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
            let roulette = russian_roulette(records.len(), v, &mut beta, &mut factor, stats);
            let survived = roulette.is_some();
            let rr_survive = roulette.unwrap_or(1.0);

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
                if let Some(ctx) = routing {
                    // The continuation per lobe, in the beauty's expression:
                    // `value / pdf`, then the roulette's compensation.
                    if exiting {
                        route.bounce(std::iter::once((NO_EVENT, factor)));
                    } else if let Some(e) = walk_event {
                        route.bounce(std::iter::once((ctx.lobe(e), factor)));
                    } else if sample.delta {
                        route.bounce(std::iter::once((ctx.lobe(sp.delta_event(&sample)), factor)));
                    } else {
                        let pdf = sample.pdf;
                        route.bounce(bounce_split.iter().map(|(e, f)| {
                            let x = f / pdf;
                            (
                                ctx.lobe(e),
                                if rr_survive < 1.0 { x / rr_survive } else { x },
                            )
                        }));
                    }
                }
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
                    continue;
                }
                prev = Some(PrevVertex::Surface(PrevBounce {
                    pos: rec.p,
                    // A BSDF (or guide-mixture) pdf, in solid angle.
                    pdf: PdfSolidAngle::from_measure(sample.pdf),
                    delta: sample.delta,
                    class: class_here,
                    nee_count,
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
        if AOV && index == 0 {
            // What arrives at the primary vertex from beyond it: the input
            // of the beauty's indirect clamp, whose factor the AOVs reuse.
            route.r1 = radiance;
        }
        if let Some(t) = &vrec.train {
            // The full incident radiance (reflected + the raw hit emission),
            // weighted by the cosine to match this tracer's estimator. One
            // cosine, not two: the material's value already carries it and the
            // integrator no longer applies a second.
            train_out.push(SampleData {
                pos: t.pos,
                dir: t.dir,
                radiance: (lights
                    .luma()
                    .of(radiance + vrec.next_emit + vrec.crossed_raw)
                    * t.cos)
                    .min(TRAIN_RADIANCE_CLAMP),
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
                        + vrec.factor * (vrec.next_emit * vrec.next_emit_weight + vrec.crossed))
                + clamp_indirect(vrec.atten * (vrec.factor * radiance), limit)
        } else {
            vrec.segment_emit
                + vrec.atten
                    * (vrec.emit_here
                        + vrec.nee
                        + vrec.factor
                            * (vrec.next_emit * vrec.next_emit_weight + vrec.crossed + radiance))
        };
    }
    if AOV {
        if albedo_on {
            route.finish_albedo();
        }
        if let Some(ctx) = routing {
            route.gather(ctx, records, indirect_clamp);
        }
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
            placement: 0,
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
