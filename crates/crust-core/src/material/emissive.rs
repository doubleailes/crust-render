use crate::PathSampler;
use crate::hittable::HitRecord;
use crate::lux::{RectTexture, Shaping};
use crate::material::{Material, ScatterSample};
use crate::ray::Ray;
use glam::Vec3A;
use std::sync::Arc;

/// A purely emissive surface material. Emission is all it knows — the shape
/// of the light it belongs to lives in a `LightShape` on the light side
/// (`light.rs`), and the two are tied together by binding the same
/// `Arc<Emissive>` to both the scene geometry and the `AreaLight`.
///
/// Two kinds. [`Emissive::new`] is a plain glowing surface: the same
/// radiance from both sides and in every direction, as it always was.
/// [`Emissive::light`] is a UsdLux light's surface: **one-sided** — a
/// `RectLight` or `DiskLight` emits "from one side", a sphere or cylinder
/// outward — and optionally shaped by `ShapingAPI`. Either way the radiance
/// is answered by [`Emissive::radiance_toward`], which both halves of MIS
/// call: NEE from `AreaLight::sample_li`, and a bounce ray that hits the
/// surface through [`Material::emitted_at`].
#[derive(Debug, Clone)]
pub struct Emissive {
    color: Vec3A,
    /// Emits only toward the geometry's front (outward) side.
    one_sided: bool,
    /// Directional falloff; `None` is uniform.
    shaping: Option<Arc<Shaping>>,
    /// Positional colour map (`RectLight`'s `inputs:texture:file`).
    texture: Option<Arc<RectTexture>>,
}

impl Emissive {
    pub fn new(color: Vec3A) -> Self {
        Emissive {
            color,
            one_sided: false,
            shaping: None,
            texture: None,
        }
    }

    /// A light source's emitting surface: one-sided, with an optional
    /// shaping. A neutral shaping is dropped, so an unshaped light costs
    /// nothing per evaluation.
    pub fn light(color: Vec3A, shaping: Option<Shaping>) -> Self {
        Emissive {
            color,
            one_sided: true,
            shaping: shaping.filter(|s| !s.is_neutral()).map(Arc::new),
            texture: None,
        }
    }

    /// Multiplies the emission by a colour map over the light's surface.
    pub fn with_texture(mut self, texture: RectTexture) -> Self {
        self.texture = Some(Arc::new(texture));
        self
    }

    /// Whether emission is the same everywhere and in every direction — the
    /// case `emitted_at` answers without evaluating anything.
    fn is_uniform(&self) -> bool {
        !self.one_sided && self.shaping.is_none() && self.texture.is_none()
    }

    pub fn color(&self) -> Vec3A {
        self.color
    }

    /// The flux this emission carries off a surface of total `area`, whose
    /// projected area toward a unit direction ω (counting the side ω leaves
    /// from) is `projected_area(ω)`. It weighs lights for power-proportional
    /// selection, which needs proportion rather than precision.
    ///
    /// Emission factors as `color · texture(p) · shaping(ω)`, and that is what
    /// makes this cheap and exact where it can be. An unshaped side emits
    /// `π · area` per unit radiance whatever its shape (`∫ P(ω) dω = π A` for
    /// any surface), so only a shaped light integrates, over directions, with
    /// [`Shaping::integrate`]. The texture only ever sits on a flat rect,
    /// whose projected area is the same at every point, so its area average
    /// factors out exactly as [`RectTexture::mean`].
    pub fn flux(&self, area: f32, projected_area: impl Fn(Vec3A) -> f32) -> Vec3A {
        let sides = if self.one_sided { 1.0 } else { 2.0 };
        let texture = self.texture.as_ref().map_or(Vec3A::ONE, |t| t.mean());
        let directional = match &self.shaping {
            None => Vec3A::splat(std::f32::consts::PI * area),
            Some(s) => s.integrate(projected_area),
        };
        self.color * texture * directional * sides
    }

    /// Radiance leaving the surface at world point `p` along the unit world
    /// direction `emission_dir`. `front` says whether that direction is on
    /// the surface's emitting side; a one-sided emitter is dark from behind.
    pub fn radiance_toward(&self, p: Vec3A, emission_dir: Vec3A, front: bool) -> Vec3A {
        if self.one_sided && !front {
            return Vec3A::ZERO;
        }
        let mut radiance = self.color;
        if let Some(t) = &self.texture {
            radiance *= t.at(p);
        }
        if let Some(s) = &self.shaping {
            radiance *= s.factor(emission_dir);
        }
        radiance
    }
}

impl Material for Emissive {
    fn emitted(&self) -> Vec3A {
        self.color
    }

    /// The hit-aware entry point, so a bounce ray sees exactly what NEE
    /// sampled: the ray arrives travelling *toward* the surface, so the
    /// emission it collects leaves along the reverse of its direction, and
    /// `front_face` is the kernel's word for "arrived on the outward side".
    fn emitted_at(&self, r_in: &Ray, rec: &HitRecord, _cos_theta_o: f32) -> Vec3A {
        if self.is_uniform() {
            return self.color;
        }
        self.radiance_toward(rec.p, -r_in.direction().normalize(), rec.front_face)
    }

    // Emissive surfaces do not scatter.
    fn scatter_importance(
        &self,
        _r_in: &Ray,
        _rec: &HitRecord,
        _sampler: PathSampler,
    ) -> Option<ScatterSample> {
        None
    }
}
