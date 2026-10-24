//! [`MeshLightMaterial`]: a `MeshLightAPI` mesh's bound material with the
//! light's emission in place of its own.

use std::sync::Arc;

use glam::Vec3A;

use crate::hittable::HitRecord;
use crate::material::openpbr::ResolvedOpenPBR;
use crate::material::{
    BsdfDraw, CurveNeeds, Emissive, Material, Resolution, ResolvedBsdf, ScatterSample,
};
use crate::ray::Ray;

/// The material of a mesh light's geometry: `inner`, the mesh's bound
/// material, for everything but emission, and the light's [`Emissive`] —
/// the same `Arc` its [`AreaLight`](crate::AreaLight) holds — for emission.
/// So a camera ray, a bounce ray and NEE read one radiance through
/// [`Emissive::radiance_toward`] (lighting design record, "Mesh lights").
///
/// Every emission query answers the light's: [`Material::emitted`],
/// [`Material::emitted_directional`], [`Material::emitted_at`] and the
/// emission of the [`Resolution`] [`Material::resolve`] returns, which is
/// `inner`'s resolution with that one field replaced. An `inner` that is a
/// plain `OpenPBR` (no per-hit work) is resolved to its parameters as they
/// stand rather than queried in place: in place its own emission would
/// answer. The wrapper is per light placement; a material shared with prims
/// that are not lights is not wrapped there.
pub struct MeshLightMaterial {
    inner: Arc<dyn Material>,
    emission: Arc<Emissive>,
}

impl MeshLightMaterial {
    /// `inner` emitting `emission` instead of its own glow.
    pub fn new(inner: Arc<dyn Material>, emission: Arc<Emissive>) -> Self {
        MeshLightMaterial { inner, emission }
    }

    /// The mesh's own material.
    pub fn inner(&self) -> &Arc<dyn Material> {
        &self.inner
    }

    /// The light's emission.
    pub fn emitter(&self) -> &Arc<Emissive> {
        &self.emission
    }
}

impl std::fmt::Debug for MeshLightMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MeshLightMaterial({}, {:?})",
            self.inner.kind(),
            self.emission
        )
    }
}

impl Material for MeshLightMaterial {
    fn kind(&self) -> &'static str {
        "MeshLight"
    }

    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        draw: BsdfDraw<'_>,
    ) -> Option<ScatterSample> {
        self.inner.scatter_importance(r_in, rec, draw)
    }

    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        self.inner.eval(r_in, rec, wi)
    }

    fn resolve<'w>(
        &self,
        r_in: &Ray,
        rec: &HitRecord<'w>,
        cos_theta_o: f32,
    ) -> Option<Resolution<'w>> {
        let emitted = self.emitted_at(r_in, rec, cos_theta_o);
        if let Some(mut r) = self.inner.resolve(r_in, rec, cos_theta_o) {
            r.emitted = emitted;
            return Some(r);
        }
        // `as_openpbr` stays `None` here, so the integrator never queries
        // `inner` in place, where its own emission would answer.
        let m = self.inner.as_openpbr()?;
        let bsdf = match Resolution::of_openpbr(m, rec, cos_theta_o) {
            Some(r) => r.bsdf,
            None => ResolvedBsdf::OpenPBR(ResolvedOpenPBR::of_plain(m)),
        };
        Some(Resolution {
            bsdf,
            rec: *rec,
            emitted,
        })
    }

    fn albedo(&self, rec: &HitRecord) -> Option<Vec3A> {
        self.inner.albedo(rec)
    }

    fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        self.inner.make_ray(rec, wi)
    }

    fn face_texture(&self) -> Option<&dyn crate::PtexTexture> {
        self.inner.face_texture()
    }

    fn uses_uv(&self) -> bool {
        self.inner.uses_uv()
    }

    fn curve_needs(&self) -> CurveNeeds<'_> {
        self.inner.curve_needs()
    }

    fn primvars(&self) -> &[String] {
        self.inner.primvars()
    }

    fn uv_primvar(&self) -> Option<&str> {
        self.inner.uv_primvar()
    }

    fn has_cutout(&self) -> bool {
        self.inner.has_cutout()
    }

    fn has_straight_transmission(&self) -> bool {
        self.inner.has_straight_transmission()
    }

    fn transmits(&self) -> bool {
        self.inner.transmits()
    }

    fn opacity(&self, r_in: &Ray, rec: &HitRecord) -> f32 {
        self.inner.opacity(r_in, rec)
    }

    fn is_medium_boundary(&self) -> bool {
        self.inner.is_medium_boundary()
    }

    fn boundary_medium(&self, r_in: &Ray, rec: &HitRecord) -> Option<crate::medium::Medium> {
        self.inner.boundary_medium(r_in, rec)
    }

    fn emitted(&self) -> Vec3A {
        self.emission.emitted()
    }

    fn emitted_directional(&self, cos_theta_o: f32) -> Vec3A {
        self.emission.emitted_directional(cos_theta_o)
    }

    fn emitted_at(&self, r_in: &Ray, rec: &HitRecord, cos_theta_o: f32) -> Vec3A {
        self.emission.emitted_at(r_in, rec, cos_theta_o)
    }

    fn uniform_emission(&self) -> Option<Vec3A> {
        self.emission.uniform_emission()
    }

    fn may_emit(&self) -> bool {
        self.emission.may_emit()
    }
}
