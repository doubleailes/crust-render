//! OpenPBR Surface — Academy Software Foundation shading model.
//!
//! Full parameter set matches the OpenPBR MaterialX reference
//! (https://academysoftwarefoundation.github.io/OpenPBR/). Every field
//! defaults to the spec value, so USD scenes can specify only what they
//! need. Formulas are aligned against the MaterialX nodegraph and the
//! Adobe OpenPBR BSDF reference (github.com/adobe/openpbr-bsdf) — the
//! full alignment record, item by item with commits, lives in
//! `docs/openpbr_reference_alignment.md`.
//!
//! ## Implemented
//!
//! - Base: EON diffuse (energy-preserving Fujii Oren-Nayar), F82-tint
//!   metal, dielectric specular — anisotropic GGX with VNDF sampling,
//!   multi-lobe one-sample MIS (`eval_all`/`pdf_all` are the single
//!   shared composition, so eval/sample/pdf stay consistent).
//! - Transmission: continuous Walter BTDF (thick), Cauchy/Abbe physical
//!   dispersion per channel, thin-wall window model (delta, with
//!   `(1−R)/(1+R)` energy and view-dependent tint).
//! - Interior media: unified transmission + subsurface volume (van de
//!   Hulst albedo inversion, `transmission_scatter`) carried by refracted
//!   rays via `interior_medium`.
//! - Coat: untinted reflection lobe + per-passage view-dependent
//!   absorption (`√coat_color` per crossing, refracted path length),
//!   multi-bounce darkening, coat-attenuated emission.
//! - Fuzz (Charlie sheen) and 3-wavelength thin-film interference on both
//!   the dielectric and metal Fresnel.
//!
//! ## Not implemented (see the alignment doc for the full gap list)
//!
//! - Microfacet multiple-scattering energy compensation (Adobe uses LUTs;
//!   crust couples diffuse↔specular with a flat `1 − F_avg`).
//! - Random-walk subsurface entry: SSS without transmission renders as
//!   tinted diffuse — its volume is only reachable through refraction.
//! - `geometry_opacity` (host-renderer cutout) and authored geometry
//!   normals/tangents.

use glam::Vec3A;
use utils::cosine_hemisphere;

use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::material::brdf::*;
use crate::material::{Material, ScatterSample};
use crate::medium::Medium;
use crate::ray::Ray;

mod lobes;
mod transmission;

use lobes::{Frame, Lobe, LobePmf, coat_passage, eval_all, pdf_all};
use transmission::{
    lobe_spread, sample_transmission_rough, sample_transmission_thin, transmission_is_continuous,
};

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[inline]
fn white() -> Vec3A {
    Vec3A::new(0.8, 0.8, 0.8)
}
#[inline]
fn subsurface_radius_scale_default() -> Vec3A {
    Vec3A::new(1.0, 0.5, 0.25)
}
#[inline]
fn base_default() -> Vec3A {
    Vec3A::new(0.8, 0.8, 0.8)
}
#[derive(Debug, Clone)]
pub struct OpenPBR {
    // --- base -----------------------------------------------------------
    pub base_weight: f32,
    pub base_color: Vec3A,
    pub base_diffuse_roughness: f32,
    pub base_metalness: f32,

    // --- specular -------------------------------------------------------
    pub specular_weight: f32,
    pub specular_color: Vec3A,
    pub specular_roughness: f32,
    pub specular_ior: f32,
    pub specular_roughness_anisotropy: f32,

    // --- transmission (phase 3) -----------------------------------------
    pub transmission_weight: f32,
    pub transmission_color: Vec3A,
    pub transmission_depth: f32,
    pub transmission_scatter: Vec3A,
    pub transmission_scatter_anisotropy: f32,
    pub transmission_dispersion_scale: f32,
    pub transmission_dispersion_abbe_number: f32,

    // --- subsurface (phase 5) -------------------------------------------
    pub subsurface_weight: f32,
    pub subsurface_color: Vec3A,
    pub subsurface_radius: f32,
    pub subsurface_radius_scale: Vec3A,
    pub subsurface_scatter_anisotropy: f32,

    // --- fuzz -----------------------------------------------------------
    pub fuzz_weight: f32,
    pub fuzz_color: Vec3A,
    pub fuzz_roughness: f32,

    // --- coat (phase 2) -------------------------------------------------
    pub coat_weight: f32,
    pub coat_color: Vec3A,
    pub coat_roughness: f32,
    pub coat_roughness_anisotropy: f32,
    pub coat_ior: f32,
    pub coat_darkening: f32,

    // --- thin-film (phase 2) --------------------------------------------
    pub thin_film_weight: f32,
    pub thin_film_thickness: f32,
    pub thin_film_ior: f32,

    // --- emission -------------------------------------------------------
    pub emission_luminance: f32,
    pub emission_color: Vec3A,

    // --- geometry -------------------------------------------------------
    pub geometry_opacity: f32,
    pub geometry_thin_walled: bool,

    // --- textures -------------------------------------------------------
    /// Per-face texture driving `base_color`. When present and the hit carries
    /// a face id, it *replaces* `base_color` — which stays authored as the
    /// fallback for hits with no face identity (and for hosts that decode no
    /// Ptex), so an unresolved texture degrades to a flat plausible colour.
    pub base_color_ptex: Option<crate::PtexRef>,

    // --- derived --------------------------------------------------------
    /// The interior medium, built on first use. Not a parameter: leave it at
    /// its default (`..OpenPBR::default()`). See [`InteriorCache`].
    pub interior: InteriorCache,
}

/// [`OpenPBR`]'s interior medium, computed the first time a ray refracts
/// into the material and copied into every refraction after it — instead of
/// rebuilding the `Medium` (three logarithms, a van de Hulst inversion and a
/// blend) for each one. The medium depends only
/// on the transmission and subsurface parameters.
///
/// Cloning yields an **empty** cache, never a copy: an `OpenPBR` is cloned to
/// be changed — the per-hit Ptex substitution, a pattern material's
/// per-query parameter set — and a copied medium would outlive the
/// parameters it was built from. A material is shaded behind an `Arc`,
/// immutably, so the one it caches cannot go stale; do not change the
/// parameters of an `OpenPBR` you have already rendered with.
#[derive(Default)]
pub struct InteriorCache(std::sync::OnceLock<Option<Medium>>);

impl Clone for InteriorCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for InteriorCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InteriorCache")
    }
}

impl Default for OpenPBR {
    fn default() -> Self {
        Self {
            base_weight: 1.0,
            base_color: base_default(),
            base_diffuse_roughness: 0.0,
            base_metalness: 0.0,
            specular_weight: 1.0,
            specular_color: Vec3A::ONE,
            specular_roughness: 0.3,
            specular_ior: 1.5,
            specular_roughness_anisotropy: 0.0,
            transmission_weight: 0.0,
            transmission_color: Vec3A::ONE,
            transmission_depth: 0.0,
            transmission_scatter: Vec3A::ZERO,
            transmission_scatter_anisotropy: 0.0,
            transmission_dispersion_scale: 0.0,
            transmission_dispersion_abbe_number: 20.0,
            subsurface_weight: 0.0,
            subsurface_color: white(),
            subsurface_radius: 1.0,
            subsurface_radius_scale: subsurface_radius_scale_default(),
            subsurface_scatter_anisotropy: 0.0,
            fuzz_weight: 0.0,
            fuzz_color: Vec3A::ONE,
            fuzz_roughness: 0.5,
            coat_weight: 0.0,
            coat_color: Vec3A::ONE,
            coat_roughness: 0.0,
            coat_roughness_anisotropy: 0.0,
            coat_ior: 1.6,
            coat_darkening: 1.0,
            thin_film_weight: 0.0,
            thin_film_thickness: 0.5,
            thin_film_ior: 1.4,
            emission_luminance: 0.0,
            emission_color: Vec3A::ONE,
            geometry_opacity: 1.0,
            geometry_thin_walled: false,
            base_color_ptex: None,
            interior: InteriorCache::default(),
        }
    }
}

impl OpenPBR {
    /// Pure diffuse surface (the old `Lambertian` preset).
    pub fn diffuse(base_color: Vec3A) -> Self {
        OpenPBR {
            base_color,
            specular_weight: 0.0,
            ..OpenPBR::default()
        }
    }

    /// Metallic surface (the old `Metal` preset; `roughness` plays the role
    /// of fuzz).
    pub fn metal(base_color: Vec3A, roughness: f32) -> Self {
        OpenPBR {
            base_color,
            base_metalness: 1.0,
            specular_roughness: roughness.clamp(0.0, 1.0),
            ..OpenPBR::default()
        }
    }

    /// Smooth transmissive dielectric (the old `Dielectric` preset).
    pub fn glass(ior: f32) -> Self {
        OpenPBR {
            transmission_weight: 1.0,
            specular_ior: ior,
            specular_roughness: 0.01,
            ..OpenPBR::default()
        }
    }

    /// Glossy dielectric/metal mix (the old `CookTorrance` preset).
    pub fn glossy(base_color: Vec3A, roughness: f32, metalness: f32) -> Self {
        OpenPBR {
            base_color,
            specular_roughness: roughness.clamp(0.05, 1.0),
            base_metalness: metalness.clamp(0.0, 1.0),
            ..OpenPBR::default()
        }
    }

    /// The unified interior medium of the closed surface, per Adobe's
    /// reference (`openpbr_prepare_volume`): the transmission volume
    /// (extinction from color/depth plus `transmission_scatter`) and the
    /// subsurface volume (van de Hulst albedo inversion) blended by their
    /// relative fractions of the dielectric base — transmission supersedes
    /// subsurface, so the fractions are `t` and `(1 − t)·s`. Attached to
    /// rays refracting into the front face; `None` when the interior
    /// neither absorbs nor scatters (zero-depth clear glass), so inert
    /// interiors skip medium tracking entirely.
    ///
    /// Built once per material ([`InteriorCache`]); each call is a copy.
    fn interior_medium(&self) -> Option<Medium> {
        *self.interior.0.get_or_init(|| self.build_interior_medium())
    }

    fn build_interior_medium(&self) -> Option<Medium> {
        let trans_frac = self.transmission_weight;
        let sss_frac = (1.0 - self.transmission_weight) * self.subsurface_weight;
        let total = trans_frac + sss_frac;
        if total <= 0.0 {
            return None;
        }
        let trans_volume = Medium::from_transmission(
            self.transmission_color,
            self.transmission_depth,
            self.transmission_scatter,
            self.transmission_scatter_anisotropy,
        );
        let medium = if sss_frac > 0.0 {
            let sss_volume = Medium::from_subsurface(
                self.subsurface_color,
                self.subsurface_radius,
                self.subsurface_radius_scale,
                self.subsurface_scatter_anisotropy,
            );
            Medium::blend(
                &trans_volume,
                trans_frac / total,
                &sss_volume,
                sss_frac / total,
            )
        } else {
            trans_volume
        };
        if medium.sigma_t_max() <= 1e-6 {
            None
        } else {
            Some(medium)
        }
    }
}

/// An [`OpenPBR`] with its per-hit lookups applied ([`OpenPBR::into_resolved`]):
/// what the integrator's BSDF queries run on at a textured vertex.
///
/// It answers the BSDF queries and nothing else — no emission method, because
/// emission is read from the parameters before resolution. The parameters
/// themselves stay readable ([`ResolvedOpenPBR::params`]) for probes.
#[derive(Debug, Clone)]
pub struct ResolvedOpenPBR(OpenPBR);

impl ResolvedOpenPBR {
    /// The resolved parameters, for probes and tests.
    pub fn params(&self) -> &OpenPBR {
        &self.0
    }

    /// [`Material::scatter_importance`] on the resolved parameters.
    #[inline]
    pub(crate) fn scatter(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
    ) -> Option<ScatterSample> {
        self.0.scatter_resolved(r_in, rec, sampler)
    }

    /// [`Material::eval`] on the resolved parameters.
    #[inline]
    pub(crate) fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        self.0.eval_resolved(r_in, rec, wi)
    }

    /// [`Material::make_ray`] on the resolved parameters.
    #[inline]
    pub fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        self.0.make_ray(rec, wi)
    }
}

// ---------------------------------------------------------------------------
// Material impl
// ---------------------------------------------------------------------------

impl OpenPBR {
    /// Substitutes any per-face texture lookups for this hit, yielding the
    /// parameter set the BSDF should actually be evaluated with — or `None`
    /// when nothing is textured and `self` can be used directly.
    ///
    /// Resolving here, once, keeps every lobe downstream oblivious to
    /// texturing: `LobePmf::from_params` then derives its lobe-selection
    /// probabilities from the *textured* albedo for free, which matters —
    /// sampling a black region as though it had a mid-grey diffuse lobe would
    /// be unbiased but needlessly noisy.
    ///
    /// The returned copy carries no texture, so it cannot recurse.
    /// This material with its per-hit lookups (Ptex `base_color`) applied at
    /// `rec` — what [`Material::resolve`] implementations hand back.
    ///
    /// Consumes `self` and returns a different type on purpose: a
    /// [`ResolvedOpenPBR`] has the BSDF queries and no emission method, so
    /// emission — which must be read from the parameters *before* resolution
    /// (the coat factor reads `base_color`, which Ptex replaces) — cannot be
    /// taken after it. [`Resolution::new`](crate::material::Resolution::new)
    /// does both in the right order.
    pub fn into_resolved(self, rec: &HitRecord) -> ResolvedOpenPBR {
        match self.shaded(rec) {
            Some(m) => m,
            None => ResolvedOpenPBR(self),
        }
    }

    /// [`OpenPBR::into_resolved`] from a borrow: `None` when nothing is
    /// textured and `self` can be queried in place.
    #[inline(always)]
    pub(crate) fn resolved_at(&self, rec: &HitRecord) -> Option<ResolvedOpenPBR> {
        self.shaded(rec)
    }

    fn shaded(&self, rec: &HitRecord) -> Option<ResolvedOpenPBR> {
        let tex = self.base_color_ptex.as_ref()?;
        if rec.face_id == HitRecord::NO_FACE {
            // Geometry the importer could not give a face identity (a sphere,
            // a curve, an n-gon): fall back to the authored constant.
            return None;
        }
        let (u, v) = rec.face_uv;
        Some(ResolvedOpenPBR(OpenPBR {
            base_color: tex.eval(rec.face_id, u, v, rec.face_width),
            base_color_ptex: None,
            ..self.clone()
        }))
    }

    pub(crate) fn scatter_resolved(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
    ) -> Option<ScatterSample> {
        let frame = Frame::new(rec.normal);
        let v_world = -r_in.direction().normalize();
        let v_local = frame.to_local(v_world);
        if v_local.z <= 0.0 {
            return None;
        }

        // One 4D block from the BSDF domain: `s[0]` picks the lobe, `s[1..3]`
        // is the 2D direction sample, and `s[3]` selects the dispersion channel
        // for thick transmission.
        let s = sampler.draw_sample_f32::<4>();
        let dir_uv = [s[1], s[2]];

        let pmf = LobePmf::from_params(self);
        let lobe = pmf.pick(s[0]);

        if matches!(lobe, Lobe::Transmission) {
            // Thick refraction — dispersive or not — is a continuous Walter
            // BTDF lobe: value and pdf come from the same full-sphere
            // eval_all/pdf_all composition as the reflection lobes, so
            // sampling and evaluation agree exactly. Dispersion samples one
            // channel's IOR; eval_all/pdf_all answer with the three-channel
            // BTDF value and the channel-averaged mixture pdf.
            if transmission_is_continuous(self) {
                let l_local =
                    sample_transmission_rough(self, v_local, rec.front_face, s[3], dir_uv)?;
                let l_world = frame.to_world(l_local);
                let pdf = pdf_all(self, &pmf, v_local, l_local, rec.front_face).max(1e-4);
                let brdf = eval_all(self, v_local, l_local, rec.front_face);
                let ray = match (rec.front_face, self.interior_medium()) {
                    (true, Some(medium)) => {
                        Ray::new_in_medium(rec.p + l_world * 1e-4, l_world, medium)
                    }
                    _ => Ray::new(rec.p + l_world * 1e-4, l_world),
                };
                return Some(ScatterSample {
                    ray,
                    value: brdf * l_local.z.abs(),
                    pdf,
                    delta: false,
                    spread: lobe_spread(self, lobe),
                });
            }

            // Thin-walled transmission stays a delta lobe (placeholder pdf,
            // never mixed with a continuous density).
            let (scattered, throughput, pdf) = sample_transmission_thin(self, r_in, rec);
            // Divide by the lobe-selection probability so the mixture
            // estimator stays unbiased.
            let p_select = pmf.p_transmission.max(1e-4);
            return Some(ScatterSample {
                ray: scattered,
                value: throughput / p_select,
                pdf,
                delta: true,
                // A thin wall is a delta interface: the ray passes straight
                // through, so the cone it arrived with is the cone it leaves
                // with.
                spread: 0.0,
            });
        }

        // Reflection lobes — sample a direction from the picked lobe.
        let l_local = match lobe {
            Lobe::Diffuse | Lobe::Fuzz => cosine_hemisphere(dir_uv),
            Lobe::Specular => {
                let (ax, ay) = roughness_to_alpha_aniso(
                    self.specular_roughness,
                    self.specular_roughness_anisotropy,
                );
                let h_local = sample_vndf_ggx_aniso_local(v_local, ax, ay, dir_uv);
                let l = 2.0 * v_local.dot(h_local) * h_local - v_local;
                if l.z <= 0.0 {
                    return None;
                }
                l
            }
            Lobe::Coat => {
                let (ax, ay) =
                    roughness_to_alpha_aniso(self.coat_roughness, self.coat_roughness_anisotropy);
                let h_local = sample_vndf_ggx_aniso_local(v_local, ax, ay, dir_uv);
                let l = 2.0 * v_local.dot(h_local) * h_local - v_local;
                if l.z <= 0.0 {
                    return None;
                }
                l
            }
            Lobe::Transmission => unreachable!(),
        };

        // Mixture PDF and full-mixture BRDF value. `pdf_all` weights by the
        // full lobe PMF (including any delta-transmission share), so this is
        // the exact — defective when a delta lobe takes selection mass —
        // density of the continuous sampling procedure.
        let pdf = pdf_all(self, &pmf, v_local, l_local, rec.front_face).max(1e-4);
        let brdf = eval_all(self, v_local, l_local, rec.front_face);
        let n_dot_l = l_local.z.max(0.0);

        let l_world = frame.to_world(l_local);
        // Convention across this codebase's materials: return brdf * cos as
        // the "throughput" and the tracer multiplies by cos again. Match.
        Some(ScatterSample {
            ray: Ray::new(rec.p, l_world),
            value: brdf * n_dot_l,
            pdf,
            delta: false,
            spread: lobe_spread(self, lobe),
        })
    }

    pub(crate) fn eval_resolved(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        wi: Vec3A,
    ) -> Option<(Vec3A, f32)> {
        // Evaluates the continuous component over the full sphere: the
        // reflection lobes above the ray-facing hemisphere and — for thick
        // transmissive surfaces, dispersive or not — the Walter BTDF below
        // it (per-channel with the channel-mixture pdf when dispersion is
        // active). The only delta lobe left (thin-walled transmission) is
        // excluded per the trait contract; `pdf_all` reports the matching
        // defective density.
        let frame = Frame::new(rec.normal);
        let v_local = frame.to_local(-r_in.direction().normalize());
        if v_local.z <= 0.0 {
            return None;
        }
        let l_local = frame.to_local(wi.normalize());
        let pmf = LobePmf::from_params(self);
        let pdf = pdf_all(self, &pmf, v_local, l_local, rec.front_face).max(1e-4);
        Some((
            eval_all(self, v_local, l_local, rec.front_face) * l_local.z.abs(),
            pdf,
        ))
    }
}

impl Material for OpenPBR {
    fn kind(&self) -> &'static str {
        "OpenPBR"
    }

    fn scatter_importance(
        &self,
        r_in: &Ray,
        rec: &HitRecord,
        sampler: PathSampler,
    ) -> Option<ScatterSample> {
        match self.shaded(rec) {
            Some(m) => m.scatter(r_in, rec, sampler),
            None => self.scatter_resolved(r_in, rec, sampler),
        }
    }

    fn eval(&self, r_in: &Ray, rec: &HitRecord, wi: Vec3A) -> Option<(Vec3A, f32)> {
        match self.shaded(rec) {
            Some(m) => m.eval(r_in, rec, wi),
            None => self.eval_resolved(r_in, rec, wi),
        }
    }

    fn face_texture(&self) -> Option<&dyn crate::PtexTexture> {
        self.base_color_ptex.as_ref().map(|t| &*t.0)
    }

    fn resolve(
        &self,
        _r_in: &Ray,
        rec: &HitRecord,
        cos_theta_o: f32,
    ) -> Option<crate::material::Resolution> {
        // Only a Ptex lookup is per-hit work; an untextured surface is
        // queried in place, with no copy. Emission is the default
        // `emitted_at`: the unresolved material's, constant `base_color` and
        // all.
        crate::material::Resolution::of_openpbr(self, rec, cos_theta_o)
    }

    fn make_ray(&self, rec: &HitRecord, wi: Vec3A) -> Ray {
        // Mirror the ray construction of `scatter_importance` for an
        // externally chosen direction (e.g. from the guiding field), so a
        // guided transmission direction crosses the interface with the same
        // origin offset and interior-medium tagging as a BSDF-sampled one.
        if transmission_is_continuous(self) && rec.normal.dot(wi) < 0.0 {
            if rec.front_face
                && let Some(medium) = self.interior_medium()
            {
                return Ray::new_in_medium(rec.p + wi * 1e-4, wi, medium);
            }
            return Ray::new(rec.p + wi * 1e-4, wi);
        }
        Ray::new(rec.p, wi)
    }

    fn emitted(&self) -> Vec3A {
        self.emission_color * self.emission_luminance
    }

    /// Emission seen through the coat: one outbound passage of the Adobe
    /// reference coating model (`openpbr_compute_emission` scales emission
    /// by the view-side base-layer factor) — `√coat_color` raised to the
    /// refracted path length, the coat's directional Fresnel transmission,
    /// and the multi-bounce darkening, all fading with `coat_weight`.
    fn emitted_directional(&self, cos_theta_o: f32) -> Vec3A {
        let uncoated = self.emission_color * self.emission_luminance;
        if self.coat_weight <= 0.0 {
            return uncoated;
        }
        let dark = coat_darkening_factor(
            self.base_color,
            self.coat_ior,
            self.coat_weight,
            self.coat_darkening,
        );
        uncoated * coat_passage(self, cos_theta_o) * dark
    }
}

#[cfg(test)]
mod tests;
