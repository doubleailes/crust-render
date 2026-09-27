//! [`AreaLight`]: a [`LightShape`] paired with the [`Emissive`] its geometry
//! carries — the one light with a position.

use std::sync::Arc;

use glam::Vec3A;
use utils::luminance;

use crate::material::Emissive;
use crate::pdf::{InvPdfArea, PdfSolidAngle};

use super::rect::RectShape;
use super::shape::{AffineShape, LightShape, SphereShape};
use super::{Light, LightSample};

/// The emitting surface of an [`AreaLight`]: one of the crate's
/// [`LightShape`]s, dispatched by `match` rather than through a
/// `Box<dyn LightShape>`.
///
/// The set is closed — one variant per supported UsdLux shape — and NEE asks
/// the shape three or four questions per sample (`sample_solid_angle`,
/// `sample_point`, `normal_at`, `inv_pdf_area`). As an enum each is a jump
/// the optimiser can inline into [`AreaLight::sample_li`] instead of a vtable
/// hop behind a heap pointer: the move `crust_rt`'s `PrimNode` made. The
/// trait stays the contract every variant implements; the enum is only the
/// dispatch.
#[derive(Clone)]
pub enum AreaShape {
    Sphere(SphereShape),
    Affine(AffineShape),
    Rect(RectShape),
}

impl From<SphereShape> for AreaShape {
    fn from(s: SphereShape) -> Self {
        AreaShape::Sphere(s)
    }
}

impl From<AffineShape> for AreaShape {
    fn from(s: AffineShape) -> Self {
        AreaShape::Affine(s)
    }
}

impl From<RectShape> for AreaShape {
    fn from(s: RectShape) -> Self {
        AreaShape::Rect(s)
    }
}

/// Forwards one [`LightShape`] method to the variant.
macro_rules! dispatch {
    ($self:ident, $s:ident => $call:expr) => {
        match $self {
            AreaShape::Sphere($s) => $call,
            AreaShape::Affine($s) => $call,
            AreaShape::Rect($s) => $call,
        }
    };
}

impl LightShape for AreaShape {
    #[inline]
    fn kind(&self) -> &'static str {
        dispatch!(self, s => s.kind())
    }

    #[inline]
    fn sample_point(&self, u: f32, v: f32) -> Vec3A {
        dispatch!(self, s => s.sample_point(u, v))
    }

    #[inline]
    fn normal_at(&self, p: Vec3A) -> Vec3A {
        dispatch!(self, s => s.normal_at(p))
    }

    #[inline]
    fn area(&self) -> f32 {
        dispatch!(self, s => s.area())
    }

    #[inline]
    fn inv_pdf_area(&self, p: Vec3A) -> InvPdfArea {
        dispatch!(self, s => s.inv_pdf_area(p))
    }

    #[inline]
    fn sample_solid_angle(&self, from: Vec3A, u: f32, v: f32) -> Option<(Vec3A, PdfSolidAngle)> {
        dispatch!(self, s => s.sample_solid_angle(from, u, v))
    }

    #[inline]
    fn solid_angle_pdf(&self, from: Vec3A, p: Vec3A) -> Option<PdfSolidAngle> {
        dispatch!(self, s => s.solid_angle_pdf(from, p))
    }
}

/// A geometric area light: an [`AreaShape`] paired with the [`Emissive`]
/// material its scene geometry carries (Cornell-box semantics — the same
/// surface is both light and visible object).
#[derive(Clone)]
pub struct AreaLight {
    pub(super) shape: AreaShape,
    pub(super) material: Arc<Emissive>,
    /// The world `geom_id` of the emissive geometry this light shares its
    /// surface with — how bounce hits are attributed back to the light.
    pub(super) geom_id: u32,
}

impl AreaLight {
    pub fn new(shape: impl Into<AreaShape>, material: Arc<Emissive>, geom_id: u32) -> Self {
        Self {
            shape: shape.into(),
            material,
            geom_id,
        }
    }

    /// Solid-angle pdf, as seen from `from`, of the strategy
    /// [`Light::sample_li`] used to reach `light_point`: the shape's own
    /// solid-angle density where it has one, otherwise that of sampling
    /// uniformly by area, `dist² / (cos(θ_light) · area)`, where θ_light is the
    /// angle between the light's surface normal at `light_point` and the
    /// direction back toward the shaded point.
    ///
    /// `None` where the area density is infinite (an edge-on point, see
    /// [`AreaLight::pdf_toward`]): `sample_li` refuses such a sample, so NEE
    /// never delivers that point and the bounce side must keep its emission
    /// whole.
    pub(super) fn solid_angle_pdf(&self, from: Vec3A, light_point: Vec3A) -> Option<PdfSolidAngle> {
        if let Some(pdf) = self.shape.solid_angle_pdf(from, light_point) {
            return Some(pdf);
        }
        let direction = light_point - from;
        let dir_to_light = direction.normalize();
        let light_normal = self.shape.normal_at(light_point);
        self.pdf_toward(direction, dir_to_light, light_normal, light_point)
    }

    /// The area-sampling density of `light_point` in solid angle, with the
    /// normal already in hand. `None` where it is not finite: the point is
    /// seen edge-on (`cos θ_light = 0`), or the shape is degenerate there.
    ///
    /// The cosine is taken **unsigned**, as in pbrt-v4: the area-to-solid-
    /// angle Jacobian is `d² / |cos θ_light|` whichever side the point is
    /// seen from. Whether that side *emits* is the material's question
    /// (`radiance_toward`'s `front`), not the density's: a one-sided light
    /// seen from behind comes back with zero radiance, which NEE then skips
    /// before tracing its shadow ray, and a two-sided emitter keeps direct
    /// sampling from behind and from inside a sphere.
    ///
    /// That is pbrt-v4's convention (`Shape::Sample` returns no sample,
    /// `Shape::PDF` returns 0). This used to add `1e-4` to the denominator
    /// to keep the pdf finite instead, which inflated every NEE contribution
    /// by `1 + 1e-4/(cos θ_light · area)`: +0.3% on `veach_mis`'s smallest
    /// sphere face-on, +100% on a face-on 1 cm² light in a scene modelled in
    /// metres, and dependent on the scene's units.
    pub(super) fn pdf_toward(
        &self,
        direction: Vec3A,
        dir_to_light: Vec3A,
        light_normal: Vec3A,
        light_point: Vec3A,
    ) -> Option<PdfSolidAngle> {
        let cosine = light_normal.dot(-dir_to_light).abs();
        self.shape
            .inv_pdf_area(light_point)
            .to_solid_angle(direction.length_squared(), cosine)
    }
}

impl Light for AreaLight {
    fn kind(&self) -> &'static str {
        self.shape.kind()
    }

    fn sample_li(&self, from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        // The shape's solid-angle strategy where it has one from here, area
        // sampling otherwise. `pdf_at_point` makes the same choice through
        // `solid_angle_pdf`, which is what keeps the two MIS sides one strategy.
        let solid_angle = self.shape.sample_solid_angle(from, u, v);
        let light_point = solid_angle.map_or_else(|| self.shape.sample_point(u, v), |(p, _)| p);
        let to_light = light_point - from;
        let distance = to_light.length();
        if distance < 1e-6 {
            return None;
        }
        let direction = to_light / distance;
        // `normalize` rather than `direction`, so the pdf is bit-for-bit what
        // `pdf_at_point` computes for the same point on the bounce side.
        let dir_to_light = to_light.normalize();
        let light_normal = self.shape.normal_at(light_point);
        // Emission leaves the light back toward `from`; whether that is the
        // emitting side is the sign of the cosine the pdf takes unsigned.
        let front = light_normal.dot(-dir_to_light) > 0.0;
        // An area sample whose density is infinite is refused rather than
        // given a finite stand-in, which would bias it (see `pdf_toward`).
        // `solid_angle_pdf` refuses the same points, so a bounce ray
        // that hits one keeps its emission at full weight.
        let pdf = match solid_angle {
            Some((_, pdf)) => pdf,
            None => self.pdf_toward(to_light, dir_to_light, light_normal, light_point)?,
        };
        Some(LightSample {
            direction,
            distance,
            radiance: self
                .material
                .radiance_toward(light_point, -dir_to_light, front),
            pdf,
        })
    }

    fn pdf_at_point(&self, from: Vec3A, light_point: Vec3A) -> Option<PdfSolidAngle> {
        self.solid_angle_pdf(from, light_point)
    }

    fn geom_id(&self) -> Option<u32> {
        Some(self.geom_id)
    }

    /// The emission's flux ([`Emissive::flux`]), with the projected area a
    /// shaped light integrates against taken from a fixed grid of points
    /// drawn from the shape's own area sampler, each weighted by the
    /// reciprocal of its density — exact for a flat light, whose normal is
    /// the same everywhere, and a close quadrature for a curved one.
    fn power(&self) -> Option<f32> {
        const GRID: usize = 16;
        let points: Vec<(Vec3A, f32)> = (0..GRID * GRID)
            .map(|k| {
                let u = ((k / GRID) as f32 + 0.5) / GRID as f32;
                let v = ((k % GRID) as f32 + 0.5) / GRID as f32;
                let p = self.shape.sample_point(u, v);
                (self.shape.normal_at(p), self.shape.inv_pdf_area(p).get())
            })
            .collect();
        let projected_area = |w: Vec3A| {
            points
                .iter()
                .map(|&(n, area)| area * n.dot(w).max(0.0))
                .sum::<f32>()
                / points.len() as f32
        };
        Some(luminance(
            self.material.flux(self.shape.area(), projected_area),
        ))
    }
}
