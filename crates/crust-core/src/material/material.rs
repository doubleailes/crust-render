use crate::hittable::HitRecord;
use crate::ray::Ray;
use crate::PathSampler;
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
}

/// The `Material` trait defines the behavior of materials in the ray tracing system.
/// Materials determine how rays interact with surfaces, including scattering and emission.
pub trait Material: Send + Sync {
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
    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        let _ = (r_in, rec, wi);
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
}
