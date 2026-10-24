use crate::hittable::HitRecord;
use crate::material::closure::{PooledClosure, ResolvedClosure};
use crate::material::{OpenPBR, ResolvedOpenPBR};
use crate::ray::Ray;
use glam::Vec3A;

/// What a subsurface leaf hands the tracer when the closure selects it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SubsurfaceEntry {
    /// World-space direction into the medium from the entry point.
    pub dir: Vec3A,
    /// The target multiple-scattering albedo (the leaf's `color`).
    pub albedo: Vec3A,
    /// Per-channel mean free path, world units (`radius`).
    pub radius: Vec3A,
    /// Henyey–Greenstein anisotropy; the walk clamps it to `[0, 0.99]`
    /// ([`walk_anisotropy`](crate::subsurface::walk_anisotropy)).
    pub anisotropy: f32,
}

/// One direction sampled from a material's importance distribution.
#[derive(Clone)]
pub struct ScatterSample {
    pub ray: Ray,
    /// `brdf * cos(theta_i)` per the codebase convention (the tracer
    /// multiplies by the cosine again).
    pub value: Vec3A,
    /// Solid-angle pdf of the sampled direction. For delta lobes this is a
    /// placeholder 1.0 with any discrete lobe-selection compensation already
    /// folded into `value`.
    pub pdf: f32,
    /// True when the direction came from a delta lobe (e.g. transmission,
    /// TIR fallback). A delta sample's contribution must never be mixed with
    /// a continuous density — no guide-mixture pdf, no light-MIS weight; it
    /// carries its bounce-hit emission at full weight.
    pub delta: bool,
    /// Angular width the sampled lobe adds to the path's texture-filtering
    /// cone ([`crate::RayCone`]), in radians under the small-angle
    /// approximation the cone is built on.
    ///
    /// Deliberately reported by the material rather than derived from `pdf`:
    /// by the time the tracer sees a sample, `pdf` may have been replaced by
    /// the guide/BSDF mixture density, so a near-mirror under a trained
    /// guiding field would report a broad density and blur its own
    /// reflection. It also goes to zero at grazing angles for a cosine lobe,
    /// which says nothing about how wide that lobe is. `0.0` keeps the
    /// arriving cone as it was, which is what a delta lobe wants.
    pub spread: f32,
    /// Set when the material selected a subsurface leaf: the direction is
    /// not a bounce but the entry into a random walk
    /// (`crust_core`'s `subsurface` module), which the tracer runs before the path
    /// resumes at the walk's exit. Such a sample is `delta` — no continuous
    /// density can produce it — and its `value` is the leaf's weight over its
    /// selection probability. The value is the leaf's index, which
    /// [`ShadingPoint::subsurface_entry`] turns into the walk's parameters.
    ///
    /// An index and not the parameters themselves: this sits in padding the
    /// struct already had, where the parameters grew every sample by 64
    /// bytes and cost 0.7% of cornellbox's instructions moving them around
    /// at vertices that never walk.
    pub subsurface: Option<u8>,
}

/// The uniform numbers a [`Material::scatter_importance`] draws its sample
/// from: one block of four in `[0, 1)`, which the integrator stratifies
/// through its QMC sampler and a test writes out by hand.
///
/// A material samples from numbers, never from a sampler: the same material,
/// hit, ray and numbers give the same [`ScatterSample`] whichever sampler
/// produced them, so the integrator can be generic over its sampler while
/// `dyn Material` stays object-safe. The block is drawn only when `u` is
/// called, so a material that needs no numbers for a query — an emitter, a
/// back-facing hit — costs no draw.
///
/// Room to grow: a material that one day needs unbounded numbers gets a
/// `fn rng(&self)` here (a `pcg::Rng` seeded from the same domain), which
/// breaks no implementer and costs nothing until it exists.
#[derive(Clone, Copy)]
pub enum BsdfDraw<'a> {
    /// The integrator's: drawn from the vertex's BSDF domain on demand.
    Lazy(&'a dyn Fn() -> [f32; 4]),
    /// A test's: the four numbers as written.
    Fixed([f32; 4]),
}

impl BsdfDraw<'_> {
    /// The four uniforms. `[0]` picks the lobe, `[1..3]` is the 2D direction
    /// sample and `[3]` a lobe's own discrete choice; a material that draws
    /// fewer reads the prefix.
    #[inline]
    pub fn u(&self) -> [f32; 4] {
        match self {
            BsdfDraw::Lazy(draw) => draw(),
            BsdfDraw::Fixed(u) => *u,
        }
    }
}

/// The `Material` trait defines the behavior of materials in the ray tracing system.
/// Materials determine how rays interact with surfaces, including scattering and emission.
pub trait Material: Send + Sync {
    /// Short name for the `--stats` material breakdown.
    fn kind(&self) -> &'static str {
        "custom"
    }

    /// Samples an outgoing direction from the material's own importance
    /// distribution.
    ///
    /// # Parameters
    /// - `r_in`: The incoming ray.
    /// - `rec`: The hit record containing information about the intersection.
    /// - `draw`: The uniform numbers dedicated to this scatter event (see
    ///   [`BsdfDraw`]); the material reads at most one block of four, and
    ///   only if it samples at all.
    ///
    /// # Returns
    /// - `Some(sample)` describing the sampled bounce (see [`ScatterSample`]).
    /// - `None` if the material does not scatter the ray.
    ///
    /// # Contract
    /// A pure function of its arguments: the same material, hit, ray and
    /// numbers give the same sample, whatever produced the numbers.
    #[must_use]
    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        draw: BsdfDraw<'_>,
    ) -> Option<ScatterSample>;

    /// Evaluates the *continuous* part of the BSDF toward a given
    /// world-space unit direction `wi`, without sampling. This is what MIS
    /// against an external sampling strategy (light sampling, path guiding)
    /// needs and `scatter_importance` cannot provide, since the latter picks
    /// its own direction. Delta lobes (transmission) are excluded by
    /// definition: they cover a measure-zero set of directions, are never
    /// produced by an external continuous sampler, and are compensated at
    /// full weight when sampled directly.
    ///
    /// # Returns
    /// - `Some((value, pdf))` where `value` follows the codebase convention of
    ///   `brdf * cos(theta_i)` (the tracer multiplies by the cosine again) and
    ///   `pdf` is the (possibly defective, if delta lobes take part of the
    ///   lobe-selection mass) solid-angle density `scatter_importance`
    ///   assigns to continuous samples at `wi`.
    /// - `None` if the material has no continuous component at all (pure
    ///   emitters).
    ///
    /// # Contract
    /// Whether this returns `None` must depend only on the material and hit
    /// state, never on `wi` — the integrator uses "eval is available" to pick
    /// a single estimator per vertex before choosing a direction. Rejecting a
    /// particular direction (e.g. below the hemisphere) must instead return
    /// `Some((Vec3A::ZERO, pdf))` with a small positive pdf.
    ///
    /// And a non-delta sample from `scatter_importance` at a hit means this
    /// returns `Some` at that hit — a continuous sample *is* a draw from the
    /// continuous component. The integrator relies on it to read "could NEE
    /// have competed here" off the sample instead of asking `eval`, which
    /// for a textured material re-runs the whole pattern network.
    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        let _ = (r_in, rec, wi);
        None
    }

    /// This material resolved at one hit: the `OpenPBR` its pattern network
    /// (or textures) reduce to there, the record to shade it with — the hit's
    /// own, with any shading normal the network produces applied — and the
    /// hit's emission toward `cos_theta_o`.
    ///
    /// The integrator calls this once per path vertex and routes every query
    /// at that vertex — emission, the scatter, NEE's `eval`, guiding's `eval`
    /// and `make_ray` — through the result ([`ShadingPoint`]), so a textured
    /// material runs its network once per vertex instead of once per query.
    /// It is the OSL / pbrt-v4 split between running a shader and using the
    /// BSDF it produced.
    ///
    /// `None` (the default) means there is no per-hit work to share and the
    /// queries go to the material itself. Otherwise every field must answer
    /// exactly as this material's own methods would at `rec`:
    /// [`Resolution::emitted`] as [`Material::emitted_at`], and the `OpenPBR`
    /// *fully* resolved — its own per-hit lookups (Ptex) already applied, see
    /// [`OpenPBR::into_resolved`] — for the BSDF queries.
    #[must_use]
    fn resolve<'w>(
        &self,
        r_in: &Ray,
        rec: &HitRecord<'w>,
        cos_theta_o: f32,
    ) -> Option<Resolution<'w>> {
        let _ = (r_in, rec, cos_theta_o);
        None
    }

    /// This material as a plain [`OpenPBR`], when it is one — the
    /// integrator's static fast path. Where [`Material::resolve`] says there is
    /// no per-hit work, a [`ShadingPoint`] over an `OpenPBR` queries it through
    /// its concrete methods instead of four or five virtual calls per vertex.
    /// Only `OpenPBR` overrides it; the answers are the same code either way.
    fn as_openpbr(&self) -> Option<&OpenPBR> {
        None
    }

    /// The albedo the `albedo` AOV reports at `rec`, for a material the
    /// integrator queries directly (one that resolves to no `OpenPBR` and no
    /// MaterialX closure). `None`, the default, is reported as 1 — OIDN's
    /// documented fallback for a surface with no albedo.
    fn albedo(&self, rec: &HitRecord) -> Option<Vec3A> {
        let _ = rec;
        None
    }

    /// Builds the continuation ray for an externally chosen direction `wi`
    /// (e.g. drawn from the guiding field). Materials that tag rays with an
    /// interior medium on transmission must do the same here, so a guided
    /// direction crosses the interface exactly like a BSDF-sampled one.
    fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        Ray::new(rec.p, wi)
    }

    /// The per-face (Ptex) texture this material samples, if any — i.e.
    /// whether it reads [`HitRecord::face_id`] and `face_uv`.
    ///
    /// Resolving a triangle hit back to its source polygon needs a side table
    /// as large as the triangle list, so the importer builds one only for
    /// geometry whose material will actually consult it. A production stage is
    /// overwhelmingly untextured meshes; they should pay nothing.
    ///
    /// The importer also uses this to cross-check the texture's face count
    /// against the mesh's, which is the one cheap way to catch a texture bound
    /// to the wrong geometry before it silently mis-shades everything.
    fn face_texture(&self) -> Option<&dyn crate::PtexTexture> {
        None
    }

    /// Whether this material reads [`HitRecord::uv`] — i.e. samples a
    /// UV-addressed texture or a normal map.
    ///
    /// Gates the per-triangle UV table exactly as [`Material::face_texture`]
    /// gates the per-face one, and for the same reason: the table is as large
    /// as the triangle list (36 bytes a triangle, corner UVs plus a tangent),
    /// and a production stage is overwhelmingly geometry that never reads a
    /// texture coordinate. Those meshes should pay nothing.
    fn uses_uv(&self) -> bool {
        false
    }

    /// What a curve bound to this material must carry to its hits: the
    /// chart, the strand (`curve:v`, `curve:index`, `curve:random`), and
    /// which primvars by name — see [`crate::StrandTable`], which the import
    /// builds only when [`CurveNeeds::any`] says so.
    ///
    /// The default asks for the chart exactly when [`Material::uses_uv`]
    /// does, and for nothing else.
    fn curve_needs(&self) -> CurveNeeds<'_> {
        CurveNeeds {
            chart: self.uses_uv(),
            ..CurveNeeds::NOTHING
        }
    }

    /// The primvars this material reads by name, in the order its program
    /// numbers them ([`crust_mtlx::Program::primvars`]) — on a mesh, what the
    /// import decodes into the mesh's [`crate::PrimvarTable::shading`] and a
    /// hit carries as a [`crate::MeshHit`]; a curve reads the same list
    /// through [`Material::curve_needs`]. Empty by default: a material that
    /// reads none takes no mesh off `World::intersect`'s plain path.
    fn primvars(&self) -> &[String] {
        &[]
    }

    /// The primvar holding the chart this material reads, when it names one
    /// other than the conventional `st`.
    ///
    /// A `UsdPrimvarReader_float2`'s `varname` is how a USD network says which
    /// UV set a texture uses, and exporters do not all say `st`: ALab's
    /// published assets read `perfuv`. The importer tries this primvar before
    /// its built-in list (`st`, `uv`, `st0`, `UVMap`), so a mesh whose only
    /// chart has another name is still textured. `None` — every material but
    /// a preview surface naming its reader — keeps that list unchanged.
    fn uv_primvar(&self) -> Option<&str> {
        None
    }

    /// Whether [`Material::opacity`] can be below 1 anywhere — known when the
    /// material is built, never per hit.
    ///
    /// The world counts these at commit ([`crate::World::has_cutouts`]), and
    /// a world with none keeps the integrator's opaque fast paths: the
    /// any-hit shadow query and a single closest hit per segment. A material
    /// answering `false` is never asked its opacity.
    fn has_cutout(&self) -> bool {
        false
    }

    /// Whether a hit on this material can let a ray continue straight
    /// through, unscattered: a thin-walled transmissive surface, whose
    /// transmission is a delta lobe that keeps the ray's direction — known
    /// when the material is built, never per hit.
    ///
    /// The world counts these at commit ([`crate::World::has_straight_transmission`])
    /// and the integrator then treats such a surface as a pass-through, as it
    /// does a cutout: a shadow ray is attenuated by the straight
    /// transmittance instead of blocked, and a path passes the surface with
    /// it or meets the rest of the BSDF ([`ShadingPoint::straight_transmittance`],
    /// [`ShadingPoint::exclude_straight`]). Answering `true` for a material
    /// that turns out to have none at a hit is safe — it costs a walk, not a
    /// bias. The straight lobe itself is reported only through
    /// [`Material::resolve`] (an `OpenPBR` or a MaterialX closure); a
    /// material queried directly reports none.
    fn has_straight_transmission(&self) -> bool {
        false
    }

    /// Whether light can arrive at this surface from below its horizon: any
    /// transmission, refractive, diffuse or thin-walled. Known when the
    /// material is built, never per hit, and conservative: `true` (the
    /// default) is always safe. A surface that answers `false` lets
    /// [`LightSelection::Tree`](crate::LightSelection::Tree) rank a light
    /// wholly below its horizon at its defensive share alone; answering
    /// `false` for a surface that does transmit costs noise from behind, never
    /// bias.
    fn transmits(&self) -> bool {
        true
    }

    /// The surface's presence at a hit, in [0, 1]: MaterialX `surface`'s
    /// `opacity`. A ray meets the surface with this probability and otherwise
    /// passes through it untouched — no vertex, no depth, no change of medium
    /// — and a shadow ray is attenuated by `1 − opacity` at each crossing.
    /// Asked before the hit is shaded, so it must not need a
    /// [`ShadingPoint`]; only asked when [`Material::has_cutout`] says so.
    fn opacity(&self, r_in: &Ray, rec: &HitRecord) -> f32 {
        let _ = (r_in, rec);
        1.0
    }

    /// Whether this material is a **medium boundary**: a volume-only
    /// material (a USD `Material` whose only terminal is `volume`, a MaterialX
    /// `volumematerial`) that scatters nothing at its surface. A path crosses
    /// it untouched — no vertex, no depth — entering
    /// [`Material::boundary_medium`] through a front face and leaving it
    /// through a back one, and a shadow ray crosses it the same way, paying
    /// the interior's transmittance. Known when the material is built; the
    /// world counts these at commit ([`crate::World::has_medium_boundaries`]).
    ///
    /// Typhoon's `IsVolumeOnlyBoundary`. A boundary is never shaded: nothing
    /// asks it to scatter, evaluate or emit.
    fn is_medium_boundary(&self) -> bool {
        false
    }

    /// The medium inside a medium boundary at a hit, `None` for vacuum.
    /// Only asked when [`Material::is_medium_boundary`] says so.
    fn boundary_medium(&self, r_in: &Ray, rec: &HitRecord) -> Option<crate::medium::Medium> {
        let _ = (r_in, rec);
        None
    }

    /// Returns the emitted color of the material.
    ///
    /// This method is used for materials that emit light, such as light sources.
    /// By default, it returns black (no emission).
    fn emitted(&self) -> Vec3A {
        Vec3A::new(0.0, 0.0, 0.0)
    }

    /// Emitted radiance toward a direction making angle `θ` with the surface
    /// normal (`cos_theta_o` ≥ 0). Defaults to the isotropic `emitted()`;
    /// materials whose emission is view-dependent (e.g. OpenPBR's emission
    /// seen through its coat) override this. The integrator uses it at hit
    /// points, where the outgoing direction is known; `emitted()` remains
    /// the direction-free radiance used by the light list.
    fn emitted_directional(&self, cos_theta_o: f32) -> Vec3A {
        let _ = cos_theta_o;
        self.emitted()
    }

    /// Emitted radiance at a *specific hit*, toward a direction making angle
    /// `θ` with the normal. This is the emission entry point the integrator
    /// calls at a surface hit.
    ///
    /// The default forwards to [`Material::emitted_directional`], so a
    /// material whose emission is a constant of the material — every one here
    /// but the MaterialX adapter — needs no opinion. It exists for the one
    /// that cannot answer without a shading point: a MaterialX graph's
    /// emission can come out of an `image` node, making it a function of
    /// `rec.uv`, which neither `emitted()` nor `emitted_directional()` can
    /// see. Keeping those two hit-free is deliberate — it is what lets the
    /// coat's angular emission factor stay unit-testable against a bare
    /// cosine instead of a manufactured hit.
    ///
    /// [`Material::emitted()`] remains the hit-free radiance the **light
    /// list** reads, and the two must agree for any material paired with an
    /// `AreaLight`. A material that emits only through this method must
    /// therefore never become a light-list entry: NEE would sample it at zero
    /// radiance while the bounce side saw the real value, and the MIS pair
    /// would no longer describe the same emitter.
    fn emitted_at(&self, r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Vec3A {
        let _ = (r_in, rec);
        self.emitted_directional(cos_theta_o)
    }

    /// The emission when it is one constant over the whole surface and
    /// toward every viewer: `Some(c)` only when [`Material::emitted_at`]
    /// returns exactly `c`, bit for bit, at every hit and every angle (pinned
    /// for every material by `crust-core/tests/resolve.rs`). `None`, the
    /// default, when it can vary or the material cannot tell — a textured or
    /// primvar-driven glow, or one faded by a coat or a fuzz.
    ///
    /// Stricter than [`Material::emitted`], which a coated `OpenPBR` answers
    /// with its uncoated emission. What a `MeshLightAPI` mesh is lit by under
    /// `materialGlowTintsLight`: only a uniform glow can be a light's one
    /// radiance.
    fn uniform_emission(&self) -> Option<Vec3A> {
        None
    }

    /// Whether this material can emit anywhere — known when it is built.
    /// Defaults to a non-zero [`Material::emitted`]; a material that emits
    /// only through [`Material::emitted_at`] must override it. An in-place
    /// update refuses a material that starts or stops emitting
    /// ([`crate::live`]).
    fn may_emit(&self) -> bool {
        self.emitted() != Vec3A::ZERO
    }
}

/// What a curve bound to a material must carry to its hits
/// ([`Material::curve_needs`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CurveNeeds<'a> {
    /// The chart (`primvars:st`, or the material's own), for `texcoord` and
    /// images.
    pub chart: bool,
    /// The strand: `curve:v`, `curve:index` and `curve:random`, or any
    /// primvar (which needs the strand to be interpolated along).
    pub strand: bool,
    /// The primvars read by name, in the order the material's program
    /// numbers them ([`crust_mtlx::Program::primvars`]).
    pub primvars: &'a [String],
}

impl CurveNeeds<'_> {
    /// Nothing at all: what a curve with such a material carries is no
    /// table.
    pub const NOTHING: CurveNeeds<'static> = CurveNeeds {
        chart: false,
        strand: false,
        primvars: &[],
    };

    /// Whether the curve needs a strand table at all.
    pub fn any(&self) -> bool {
        self.chart || self.strand || !self.primvars.is_empty()
    }
}

/// What [`Material::resolve`] hands back for one hit.
///
/// Built only by [`Resolution::new`] (and, for a bare `OpenPBR`,
/// `Resolution::of_openpbr`), which read the emission from the parameters
/// *before* resolving them: the order the trap in the materials design record
/// is about is now the only one that compiles.
pub struct Resolution<'w> {
    /// The fully resolved BSDF.
    pub(crate) bsdf: ResolvedBsdf,
    /// The record the BSDF shades with (the shading normal applied).
    pub(crate) rec: HitRecord<'w>,
    /// [`Material::emitted_at`] at this hit, computed from the parameters the
    /// network already produced. Taken *before* Ptex is applied, as
    /// `emitted_at` itself does: OpenPBR's coat emission factor reads
    /// `base_color`, and a resolved Ptex lookup would change it.
    pub(crate) emitted: Vec3A,
}

/// The BSDF a [`Resolution`] carries: resolved OpenPBR parameters, or a
/// MaterialX closure tree collapsed at the hit.
// Lives on the stack for one path vertex; see `Resolved`.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ResolvedBsdf {
    OpenPBR(ResolvedOpenPBR),
    Closure(PooledClosure),
}

impl<'w> Resolution<'w> {
    /// The resolution of a MaterialX closure tree at `rec`: `emitted` is the
    /// graph's emission there, read from the same evaluated program as the
    /// closure — there is no parameter set for a later lookup to replace, so
    /// the order the OpenPBR constructor enforces is trivially kept.
    pub fn closure(emitted: Vec3A, bsdf: PooledClosure, rec: HitRecord<'w>) -> Resolution<'w> {
        Resolution {
            bsdf: ResolvedBsdf::Closure(bsdf),
            rec,
            emitted,
        }
    }

    /// The resolution of `params` — a network's output, its own Ptex lookups
    /// not yet applied — shading with `rec`: the emission toward
    /// `cos_theta_o` read from `params` as they are (zero unless `can_emit`),
    /// then `params` resolved at `rec`.
    #[inline]
    pub fn new(
        params: OpenPBR,
        rec: HitRecord<'w>,
        can_emit: bool,
        cos_theta_o: f32,
    ) -> Resolution<'w> {
        let emitted = if can_emit {
            params.emitted_directional(cos_theta_o)
        } else {
            Vec3A::ZERO
        };
        Resolution {
            bsdf: ResolvedBsdf::OpenPBR(params.into_resolved(&rec)),
            rec,
            emitted,
        }
    }

    /// A bare `OpenPBR`'s resolution at `rec`, `None` when it has no per-hit
    /// work: the same order as [`Resolution::new`], without copying the
    /// parameters to get there.
    #[inline(always)]
    pub(crate) fn of_openpbr(
        unresolved: &OpenPBR,
        rec: &HitRecord<'w>,
        cos_theta_o: f32,
    ) -> Option<Resolution<'w>> {
        unresolved.resolved_at(rec).map(|bsdf| Resolution {
            bsdf: ResolvedBsdf::OpenPBR(bsdf),
            rec: *rec,
            emitted: unresolved.emitted_directional(cos_theta_o),
        })
    }

    /// The resolved OpenPBR parameters, when the BSDF is OpenPBR.
    pub fn openpbr(&self) -> Option<&ResolvedOpenPBR> {
        match &self.bsdf {
            ResolvedBsdf::OpenPBR(m) => Some(m),
            ResolvedBsdf::Closure(_) => None,
        }
    }

    /// The collapsed MaterialX closure, when the BSDF is one.
    pub fn closure_bsdf(&self) -> Option<&ResolvedClosure> {
        match &self.bsdf {
            ResolvedBsdf::Closure(c) => Some(c),
            ResolvedBsdf::OpenPBR(_) => None,
        }
    }

    /// The record the BSDF shades with.
    pub fn rec(&self) -> &HitRecord<'w> {
        &self.rec
    }

    /// [`Material::emitted_at`] at this hit.
    pub fn emitted(&self) -> Vec3A {
        self.emitted
    }
}

/// A material at one hit, with its per-hit work already done (see
/// [`Material::resolve`]). Every query here answers exactly as the
/// material's own method would at the same hit — it only stops repeating the
/// pattern network and texture fetches between queries.
pub struct ShadingPoint<'a> {
    rec: HitRecord<'a>,
    emitted: Vec3A,
    bsdf: Resolved<'a>,
}

// Lives on the stack for one path vertex; boxing the `OpenPBR` to shrink the
// enum would put a heap allocation on every vertex of a textured surface.
#[allow(clippy::large_enum_variant)]
enum Resolved<'a> {
    /// No per-hit work: queries go to the material with the hit's record.
    Material(&'a dyn Material),
    /// No per-hit work on a plain `OpenPBR`: queried in place, statically.
    Plain(&'a OpenPBR),
    /// The material's resolved `OpenPBR`, queried with the record `resolve`
    /// returned.
    OpenPBR(ResolvedOpenPBR),
    /// A MaterialX closure tree collapsed at this vertex.
    Closure(PooledClosure),
}

impl<'a> ShadingPoint<'a> {
    /// Runs `mat`'s per-hit work at `rec`, once. `cos_theta_o` is what
    /// [`Material::emitted_at`] would be given.
    pub fn new(mat: &'a dyn Material, r_in: &Ray, rec: &HitRecord<'a>, cos_theta_o: f32) -> Self {
        match mat.resolve(r_in, rec, cos_theta_o) {
            Some(r) => ShadingPoint {
                rec: r.rec,
                emitted: r.emitted,
                bsdf: match r.bsdf {
                    ResolvedBsdf::OpenPBR(m) => Resolved::OpenPBR(m),
                    ResolvedBsdf::Closure(c) => Resolved::Closure(c),
                },
            },
            None => match mat.as_openpbr() {
                // `OpenPBR`'s `emitted_at` is the default, `emitted_directional`.
                Some(m) => ShadingPoint {
                    rec: *rec,
                    emitted: m.emitted_directional(cos_theta_o),
                    bsdf: Resolved::Plain(m),
                },
                None => ShadingPoint {
                    rec: *rec,
                    emitted: mat.emitted_at(r_in, rec, cos_theta_o),
                    bsdf: Resolved::Material(mat),
                },
            },
        }
    }

    /// [`ShadingPoint::eval`], split by lobe — see [`crate::material::lobe::LobeSplit`].
    /// `false` where `eval` answers `None`. A material queried directly
    /// (no `OpenPBR`, no closure) is one diffuse lobe.
    pub(crate) fn eval_lobes(
        &self,
        r_in: &Ray,
        wi: Vec3A,
        out: &mut crate::material::lobe::LobeSplit,
    ) -> bool {
        use crate::material::lobe::{LobeEvent, LobeLabel, Scatter};
        match &self.bsdf {
            Resolved::Material(m) => {
                out.clear();
                match m.eval(r_in, &self.rec, wi) {
                    Some((value, _)) => {
                        out.push(
                            LobeEvent::reflect(Scatter::Diffuse, LobeLabel::Diffuse),
                            value,
                        );
                        true
                    }
                    None => false,
                }
            }
            Resolved::Plain(m) => m.eval_lobes_resolved(r_in, &self.rec, wi, out),
            Resolved::OpenPBR(m) => m.params().eval_lobes_resolved(r_in, &self.rec, wi, out),
            Resolved::Closure(c) => c.eval_lobes(wi, out),
        }
    }

    /// The share of `event`, the `i`-th entry of a split this shading point
    /// made toward `wi` ([`ShadingPoint::eval_lobes`],
    /// [`ShadingPoint::scatter_split`]), that a light's `inputs:specular`
    /// scales, per channel in [0, 1] — the rest answers to its
    /// `inputs:diffuse` ([`crate::LobeGains`]). By the event's label, but a
    /// fibre's leaf by its terms: R is specular, TT and TRT diffuse.
    pub(crate) fn specular_fraction(
        &self,
        i: usize,
        event: crate::material::lobe::LobeEvent,
        wi: Vec3A,
    ) -> Vec3A {
        match &self.bsdf {
            Resolved::Closure(c) => c.specular_fraction(i, wi),
            _ if event.label.is_specular() => Vec3A::ONE,
            _ => Vec3A::ZERO,
        }
    }

    /// [`ShadingPoint::scatter_importance`], also splitting a continuous
    /// sample's value by lobe into `out` (empty for a delta sample), at
    /// exactly the direction sampled — so the shares sum to `value`.
    pub(crate) fn scatter_split(
        &self,
        r_in: &Ray,
        draw: impl Fn() -> [f32; 4],
        out: &mut crate::material::lobe::LobeSplit,
    ) -> Option<ScatterSample> {
        match &self.bsdf {
            Resolved::Plain(m) => m.scatter_split(r_in, &self.rec, draw, out),
            Resolved::OpenPBR(m) => m.scatter_split(r_in, &self.rec, draw, out),
            Resolved::Closure(c) => c.scatter_split(r_in, &self.rec, draw, out),
            Resolved::Material(m) => {
                let sample = m.scatter_importance(r_in, &self.rec, BsdfDraw::Lazy(&draw))?;
                out.clear();
                if !sample.delta {
                    out.push(
                        crate::material::lobe::LobeEvent::reflect(
                            crate::material::lobe::Scatter::Diffuse,
                            crate::material::lobe::LobeLabel::Diffuse,
                        ),
                        sample.value,
                    );
                }
                Some(sample)
            }
        }
    }

    /// An unbiased estimate of the weight this BSDF gives the arriving ray
    /// `r_in` continuing straight through it — the thin-walled delta
    /// transmission, presence aside. Exact for an `OpenPBR` (the window
    /// model is closed-form); one VNDF draw from `draw`'s two uniforms for a
    /// MaterialX closure, whose rough thin leaf transmits the average of
    /// `1 − F(v·h)` over its microfacets — drawn only when it has one. Zero
    /// where the material has no such lobe.
    pub fn straight_transmittance(&self, r_in: &Ray, draw: impl FnOnce() -> [f32; 2]) -> Vec3A {
        match &self.bsdf {
            Resolved::Material(_) => Vec3A::ZERO,
            Resolved::Plain(m) => m.straight_transmittance(r_in, &self.rec),
            Resolved::OpenPBR(m) => m.straight_transmittance(r_in, &self.rec),
            Resolved::Closure(c) => c.straight_transmittance(draw),
        }
    }

    /// Drops the straight transmission from every later query here: the
    /// BSDF a path scatters through when it meets a thin wall it could have
    /// passed, the integrator having carried the straight share itself.
    /// Sampling and its density renormalise over the remaining lobes, so
    /// `eval`'s pdf stays the density `scatter_importance` samples with.
    ///
    /// Out of line: only a path meeting a thin wall it could have passed
    /// comes here.
    #[cold]
    #[inline(never)]
    pub fn exclude_straight(&mut self) {
        match &mut self.bsdf {
            Resolved::Material(_) => {}
            Resolved::Plain(m) => {
                if m.has_straight_lobe() {
                    let mut r = ResolvedOpenPBR::of_plain(m);
                    r.exclude_straight();
                    self.bsdf = Resolved::OpenPBR(r);
                }
            }
            Resolved::OpenPBR(m) => m.exclude_straight(),
            Resolved::Closure(c) => c.exclude_straight(),
        }
    }

    /// The event of a delta sample this shading point produced.
    pub(crate) fn delta_event(&self, sample: &ScatterSample) -> crate::material::lobe::LobeEvent {
        use crate::material::lobe::{LobeEvent, LobeLabel, Scatter};
        match &self.bsdf {
            Resolved::Closure(c) => c.delta_event(sample),
            Resolved::Plain(m) => m.delta_event(),
            Resolved::OpenPBR(m) => m.params().delta_event(),
            Resolved::Material(_) => {
                LobeEvent::transmit(Scatter::Singular, LobeLabel::Transmission)
            }
        }
    }

    /// The colour of this surface's diffuse reflection lobes, which the raw
    /// light AOVs divide by and `diffuse_albedo` reports. Zero for a
    /// material queried directly (no `OpenPBR`, no closure): crust cannot
    /// tell its diffuse part. `cos_o` is the view cosine this point was
    /// resolved with, which an `OpenPBR`'s fuzz attenuates its base by.
    pub(crate) fn diffuse_filter(&self, cos_o: f32) -> Vec3A {
        match &self.bsdf {
            Resolved::Material(_) => Vec3A::ZERO,
            Resolved::Plain(m) => m.diffuse_filter(cos_o),
            Resolved::OpenPBR(m) => m.params().diffuse_filter(cos_o),
            Resolved::Closure(c) => c.diffuse_filter(),
        }
    }

    /// The albedo the `albedo` AOV reports here, in [0, 1], seen at the view
    /// cosine `cos_o` this point was resolved with.
    pub(crate) fn albedo(&self, cos_o: f32) -> Vec3A {
        match &self.bsdf {
            Resolved::Material(m) => m.albedo(&self.rec).unwrap_or(Vec3A::ONE),
            Resolved::Plain(m) => m.albedo(cos_o),
            Resolved::OpenPBR(m) => m.params().albedo(cos_o),
            Resolved::Closure(c) => c.albedo(),
        }
    }

    /// The shading normal at this hit — after bump and normal mapping, the
    /// one every query here shades with. For the AOVs only. A MaterialX
    /// closure shades each leaf in its own frame over the hit's record, so
    /// its normal is the leaves' ([`ResolvedClosure::shading_normal`]).
    pub(crate) fn normal(&self) -> Vec3A {
        match &self.bsdf {
            Resolved::Closure(c) => c.shading_normal().unwrap_or(self.rec.normal),
            _ => self.rec.normal,
        }
    }

    /// [`Material::emitted_at`] at this hit.
    pub fn emitted(&self) -> Vec3A {
        self.emitted
    }

    /// [`Material::scatter_importance`] at this hit, from the four uniforms
    /// `draw` produces when called. The concrete arms take the closure
    /// itself, so it inlines away; only the `dyn Material` arm boxes it in a
    /// [`BsdfDraw`].
    ///
    /// `inline(always)`: a dispatch the integrator calls once per bounce.
    /// LLVM inlined it on its own until `trace_path` grew the hidden-light
    /// crossings; out of line it cost cornellbox 0.3% of its instructions.
    #[must_use]
    #[inline(always)]
    pub fn scatter_importance(
        &self,
        r_in: &Ray,
        draw: impl Fn() -> [f32; 4],
    ) -> Option<ScatterSample> {
        match &self.bsdf {
            Resolved::Material(m) => m.scatter_importance(r_in, &self.rec, BsdfDraw::Lazy(&draw)),
            // No per-hit work means no Ptex at this hit, which is exactly when
            // `OpenPBR::scatter_importance` runs `scatter_resolved` on itself.
            Resolved::Plain(m) => m.scatter_resolved(r_in, &self.rec, draw),
            Resolved::OpenPBR(m) => m.scatter(r_in, &self.rec, draw),
            Resolved::Closure(c) => c.scatter(r_in, &self.rec, draw),
        }
    }

    /// [`Material::eval`] at this hit.
    ///
    /// Forced inline: a four-arm dispatch called at every NEE connection,
    /// which LLVM stopped inlining once the AOV routing gave it more callers
    /// (+0.5% of cornellbox's instructions as a call).
    #[inline(always)]
    pub fn eval(&self, r_in: &Ray, wi: Vec3A) -> Option<(Vec3A, f32)> {
        match &self.bsdf {
            Resolved::Material(m) => m.eval(r_in, &self.rec, wi),
            Resolved::Plain(m) => m.eval_resolved(r_in, &self.rec, wi),
            Resolved::OpenPBR(m) => m.eval(r_in, &self.rec, wi),
            Resolved::Closure(c) => c.eval(r_in, &self.rec, wi),
        }
    }

    /// Whether rays leaving this hit — its shadow rays too — pass out of
    /// curve tubes: a fibre's do, its model already accounting for light's
    /// path through its own strand. Only a MaterialX closure has fibres.
    #[inline]
    pub fn passes_out_of_curves(&self) -> bool {
        match &self.bsdf {
            Resolved::Closure(c) => c.passes_out_of_curves(),
            _ => false,
        }
    }

    /// Whether a fibre shares this hit with a transmitting leaf, so light
    /// going into the tube must be split by who scatters it — see
    /// [`crate::closure::ResolvedClosure::mixes_hair`].
    #[inline]
    pub fn mixes_hair(&self) -> bool {
        match &self.bsdf {
            Resolved::Closure(c) => c.mixes_hair(),
            _ => false,
        }
    }

    /// [`ShadingPoint::eval`]'s value toward `wi`, as the fibres' share and
    /// the other leaves', and which [`ShadingPoint::eval_lobes`] shares are
    /// fibres' (bit `i` for share `i`). Every share is the others' outside a
    /// closure.
    pub(crate) fn eval_hair_split(&self, wi: Vec3A) -> (Vec3A, Vec3A, u8) {
        match &self.bsdf {
            Resolved::Closure(c) => {
                let (hair, other) = c.eval_hair_split(wi);
                (hair, other, c.hair_leaves())
            }
            _ => (Vec3A::ZERO, Vec3A::ZERO, 0),
        }
    }

    /// The random walk a sample with [`ScatterSample::subsurface`] set enters
    /// toward `dir`: `None` for any other sample.
    pub fn subsurface_entry(&self, sample: &ScatterSample) -> Option<SubsurfaceEntry> {
        match &self.bsdf {
            Resolved::Closure(c) => c.subsurface_entry(sample.subsurface?, sample.ray.direction()),
            _ => None,
        }
    }

    /// [`Material::make_ray`] at this hit.
    pub fn make_ray(&self, wi: Vec3A) -> Ray {
        match &self.bsdf {
            Resolved::Material(m) => m.make_ray(&self.rec, wi),
            Resolved::Plain(m) => Material::make_ray(*m, &self.rec, wi),
            Resolved::OpenPBR(m) => m.make_ray(&self.rec, wi),
            Resolved::Closure(c) => c.make_ray(&self.rec, wi),
        }
    }
}
