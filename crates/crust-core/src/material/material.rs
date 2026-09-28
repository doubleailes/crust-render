use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::material::closure::{PooledClosure, ResolvedClosure};
use crate::material::{OpenPBR, ResolvedOpenPBR};
use crate::ray::Ray;
use glam::Vec3A;

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
    /// ([`crate::subsurface`]), which the tracer runs before the path
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
    /// - `sampler`: A QMC sampler domain (by value) dedicated to this scatter
    ///   event; the material draws its own ≤4-dimension block from it.
    ///
    /// # Returns
    /// - `Some(sample)` describing the sampled bounce (see [`ScatterSample`]).
    /// - `None` if the material does not scatter the ray.
    #[must_use]
    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
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
    fn resolve(&self, r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Option<Resolution> {
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
}

/// What [`Material::resolve`] hands back for one hit.
///
/// Built only by [`Resolution::new`] (and, for a bare `OpenPBR`,
/// `Resolution::of_openpbr`), which read the emission from the parameters
/// *before* resolving them: the order the trap in the materials design record
/// is about is now the only one that compiles.
pub struct Resolution {
    /// The fully resolved BSDF.
    pub(crate) bsdf: ResolvedBsdf,
    /// The record the BSDF shades with (the shading normal applied).
    pub(crate) rec: HitRecord,
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

impl Resolution {
    /// The resolution of a MaterialX closure tree at `rec`: `emitted` is the
    /// graph's emission there, read from the same evaluated program as the
    /// closure — there is no parameter set for a later lookup to replace, so
    /// the order the OpenPBR constructor enforces is trivially kept.
    pub fn closure(emitted: Vec3A, bsdf: PooledClosure, rec: HitRecord) -> Resolution {
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
    pub fn new(params: OpenPBR, rec: HitRecord, can_emit: bool, cos_theta_o: f32) -> Resolution {
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
        rec: &HitRecord,
        cos_theta_o: f32,
    ) -> Option<Resolution> {
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
    pub fn rec(&self) -> &HitRecord {
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
    rec: HitRecord,
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
    pub fn new(mat: &'a dyn Material, r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Self {
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

    /// [`Material::emitted_at`] at this hit.
    pub fn emitted(&self) -> Vec3A {
        self.emitted
    }

    /// [`Material::scatter_importance`] at this hit.
    #[must_use]
    #[inline]
    pub fn scatter_importance(&self, r_in: &Ray, sampler: PathSampler) -> Option<ScatterSample> {
        match &self.bsdf {
            Resolved::Material(m) => m.scatter_importance(r_in, &self.rec, sampler),
            // No per-hit work means no Ptex at this hit, which is exactly when
            // `OpenPBR::scatter_importance` runs `scatter_resolved` on itself.
            Resolved::Plain(m) => m.scatter_resolved(r_in, &self.rec, sampler),
            Resolved::OpenPBR(m) => m.scatter(r_in, &self.rec, sampler),
            Resolved::Closure(c) => c.scatter(r_in, &self.rec, sampler),
        }
    }

    /// [`Material::eval`] at this hit.
    pub fn eval(&self, r_in: &Ray, wi: Vec3A) -> Option<(Vec3A, f32)> {
        match &self.bsdf {
            Resolved::Material(m) => m.eval(r_in, &self.rec, wi),
            Resolved::Plain(m) => m.eval_resolved(r_in, &self.rec, wi),
            Resolved::OpenPBR(m) => m.eval(r_in, &self.rec, wi),
            Resolved::Closure(c) => c.eval(r_in, &self.rec, wi),
        }
    }

    /// The random walk a sample with [`ScatterSample::subsurface`] set enters
    /// toward `dir`: `None` for any other sample.
    pub fn subsurface_entry(
        &self,
        sample: &ScatterSample,
    ) -> Option<crate::subsurface::SubsurfaceEntry> {
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
