//! [`PatternMaterial`]: a material that runs a pattern network at a hit and
//! shades the [`OpenPBR`] it reduces to — MaterialX graphs and textured
//! `UsdPreviewSurface`s — with its [`Material`] impl written once.
//!
//! Both used to spell the same impl out by hand: forward `scatter_importance`
//! and `eval` through a run of the network, gate emission on "can this emit
//! at all" so a non-emissive surface never runs its network to find out, and
//! build a [`Resolution`] that takes the emission from the parameters
//! *before* `into_resolved`. The next pattern material could get any of those
//! subtly wrong; with the blanket impl it only says how to run its network.
//! Monomorphised per material, so the generated code is what the hand-written
//! impls were.

use glam::Vec3A;

use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::material::{Material, OpenPBR, Resolution, ScatterSample};
use crate::ray::Ray;

/// What a pattern material supplies; [`Material`] follows from it.
///
/// The hooks are named apart from `Material`'s methods so a call site with
/// both traits in scope is never ambiguous.
pub(crate) trait PatternMaterial: Send + Sync {
    /// [`Material::kind`].
    fn pattern_kind(&self) -> &'static str;

    /// Runs the network at a hit: the `OpenPBR` it reduces to — its own Ptex
    /// lookups not yet applied — and the record with the network's shading
    /// normal applied.
    fn run(&self, r_in: &Ray, rec: &HitRecord) -> (OpenPBR, HitRecord);

    /// Whether the surface can emit at all. Asked before any emission query
    /// runs the network: `emitted_at` is asked of every surface hit, and a
    /// surface that cannot emit must not run its network to learn it.
    fn can_emit(&self) -> bool;

    /// The parameters emission is read from at a hit — [`PatternMaterial::run`]'s,
    /// unless the material can produce them for less (no shading normal).
    fn emission_params(&self, r_in: &Ray, rec: &HitRecord) -> OpenPBR {
        self.run(r_in, rec).0
    }

    /// [`Material::make_ray`].
    fn pattern_make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray;

    /// [`Material::face_texture`].
    fn pattern_face_texture(&self) -> Option<&dyn crate::PtexTexture> {
        None
    }

    /// [`Material::uv_primvar`].
    fn pattern_uv_primvar(&self) -> Option<&str> {
        None
    }

    /// The parameters whose emission is the material's *hit-free* emission
    /// ([`Material::emitted`], [`Material::emitted_directional`]), when it has
    /// one: its constants, if nothing the network computes feeds emission.
    /// `None` (the default) is no hit-free emission — the hit-aware
    /// `emitted_at` answers instead, and such a material must never become a
    /// light-list entry.
    fn constant_emission(&self) -> Option<&OpenPBR> {
        None
    }

    /// [`Material::has_cutout`].
    fn pattern_has_cutout(&self) -> bool {
        false
    }

    /// [`Material::has_straight_transmission`].
    fn pattern_has_straight_transmission(&self) -> bool {
        false
    }

    /// [`Material::opacity`].
    fn pattern_opacity(&self, r_in: &Ray, rec: &HitRecord) -> f32 {
        let _ = (r_in, rec);
        1.0
    }
}

impl<T: PatternMaterial> Material for T {
    fn kind(&self) -> &'static str {
        self.pattern_kind()
    }

    /// Runs the network and samples its `OpenPBR`. The integrator does not
    /// come through here — it resolves once per vertex ([`Material::resolve`])
    /// and queries the result — so this serves direct callers (tests, probes).
    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
    ) -> Option<ScatterSample> {
        let (params, rec) = self.run(r_in, rec);
        params.scatter_importance(r_in, &rec, sampler)
    }

    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        let (params, rec) = self.run(r_in, rec);
        params.eval(r_in, &rec, wi)
    }

    fn resolve(&self, r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Option<Resolution> {
        let (params, rec) = self.run(r_in, rec);
        // `emitted_at`'s answer from the run already made rather than a second
        // one — and, by construction, from the parameters before Ptex is
        // applied, as `emitted_at` reads them.
        Some(Resolution::new(params, rec, self.can_emit(), cos_theta_o))
    }

    fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        self.pattern_make_ray(rec, wi)
    }

    fn face_texture(&self) -> Option<&dyn crate::PtexTexture> {
        self.pattern_face_texture()
    }

    /// Unconditionally true rather than "does the network hold a texture": a
    /// network with no image can still carry a normal map over a constant, or
    /// a texcoord-driven procedural, and both need the chart. The cost of an
    /// unnecessary table is bounded; shading a textured surface at (0, 0)
    /// everywhere is not obviously wrong on screen, which is the failure
    /// worth avoiding.
    fn uses_uv(&self) -> bool {
        true
    }

    fn uv_primvar(&self) -> Option<&str> {
        self.pattern_uv_primvar()
    }

    fn has_cutout(&self) -> bool {
        self.pattern_has_cutout()
    }

    fn opacity(&self, r_in: &Ray, rec: &HitRecord) -> f32 {
        self.pattern_opacity(r_in, rec)
    }

    fn has_straight_transmission(&self) -> bool {
        self.pattern_has_straight_transmission()
    }

    fn emitted(&self) -> Vec3A {
        self.constant_emission()
            .map_or(Vec3A::ZERO, |c| c.emitted())
    }

    fn emitted_directional(&self, cos_theta_o: f32) -> Vec3A {
        self.constant_emission()
            .map_or(Vec3A::ZERO, |c| c.emitted_directional(cos_theta_o))
    }

    /// `cos_theta_o` is used as the tracer measured it, against the ray-facing
    /// geometric normal, rather than recomputed against any normal the
    /// network produces: the coat slab emission passes through belongs to the
    /// geometric interface, and letting a normal map perturb its Fresnel
    /// falloff would make emission flicker at pixel scale for nothing.
    fn emitted_at(&self, r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Vec3A {
        if !self.can_emit() {
            return Vec3A::ZERO;
        }
        self.emission_params(r_in, rec)
            .emitted_directional(cos_theta_o)
    }
}
