//! [`AreaLight`]: a [`LightShape`] paired with the [`Emissive`] its geometry
//! carries — the one light with a position.

use std::sync::Arc;

use glam::Vec3A;

use crate::material::Emissive;
use crate::pdf::{InvPdfArea, PdfSolidAngle};

use super::mesh::MeshShape;
use super::rect::RectShape;
use super::shape::{
    AffineShape, LightShape, ShapeExtent, ShapeHits, SolidAngleSampler, SphereShape,
};
use super::{FoundAlong, Light, LightBounds, LightSample};

/// The emitting surface of an [`AreaLight`]: one of the crate's
/// [`LightShape`]s, dispatched by `match` rather than through a
/// `Box<dyn LightShape>`.
///
/// The set is closed — one variant per supported UsdLux shape, and the mesh of
/// a `MeshLightAPI` prim — and NEE asks
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
    Mesh(MeshShape),
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

impl From<MeshShape> for AreaShape {
    fn from(s: MeshShape) -> Self {
        AreaShape::Mesh(s)
    }
}

/// Forwards one [`LightShape`] method to the variant.
macro_rules! dispatch {
    ($self:ident, $s:ident => $call:expr) => {
        match $self {
            AreaShape::Sphere($s) => $call,
            AreaShape::Affine($s) => $call,
            AreaShape::Rect($s) => $call,
            AreaShape::Mesh($s) => $call,
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
    fn sample_point_on(&self, u: f32, v: f32) -> (Vec3A, u32) {
        dispatch!(self, s => s.sample_point_on(u, v))
    }

    #[inline]
    fn normal_at(&self, p: Vec3A, prim: u32) -> Vec3A {
        dispatch!(self, s => s.normal_at(p, prim))
    }

    #[inline]
    fn area(&self) -> f32 {
        dispatch!(self, s => s.area())
    }

    #[inline]
    fn inv_pdf_area(&self, p: Vec3A) -> InvPdfArea {
        dispatch!(self, s => s.inv_pdf_area(p))
    }

    #[inline(always)]
    fn solid_angle_sampler(&self, from: Vec3A) -> Option<SolidAngleSampler<'_>> {
        dispatch!(self, s => s.solid_angle_sampler(from))
    }

    #[inline]
    fn hits(&self, origin: Vec3A, dir: Vec3A) -> ShapeHits {
        dispatch!(self, s => s.hits(origin, dir))
    }
    fn extent(&self) -> ShapeExtent {
        dispatch!(self, s => s.extent())
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
    /// The light, with its shape told whether the emitter is one-sided —
    /// which a tube's visible-arc strategy needs (`AffineShape`).
    pub fn new(shape: impl Into<AreaShape>, material: Arc<Emissive>, geom_id: u32) -> Self {
        let mut shape = shape.into();
        if let AreaShape::Affine(affine) = &mut shape {
            affine.set_front_only(material.is_one_sided());
        }
        Self {
            shape,
            material,
            geom_id,
        }
    }

    /// The light's shape.
    pub fn shape(&self) -> &AreaShape {
        &self.shape
    }

    /// The `Emissive` this light shares with its geometry's material slot:
    /// the one answer to what it emits ([`Emissive::radiance_toward`]).
    pub fn emitter(&self) -> &Arc<Emissive> {
        &self.material
    }

    /// Solid-angle pdf, as seen from `from`, of the strategy
    /// [`Light::sample_li`] used to reach `light_point`, on primitive `prim`
    /// ([`LightShape::normal_at`]): the shape's own
    /// solid-angle density where it has a strategy from `from`, otherwise
    /// that of sampling uniformly by area, `dist² / (cos(θ_light) · area)`,
    /// where θ_light is the angle between the light's surface normal at
    /// `light_point` and the direction back toward the shaded point.
    ///
    /// `None` where that density is infinite (an edge-on point, see
    /// [`AreaLight::pdf_toward`], or a point the shape's strategy refuses):
    /// `sample_li` refuses such a sample, so NEE never delivers that point
    /// and the bounce side must keep its emission whole. A strategy's `None`
    /// is final — falling through to the area density would weigh a bounce
    /// hit against samples NEE never draws.
    pub(super) fn solid_angle_pdf(
        &self,
        from: Vec3A,
        light_point: Vec3A,
        prim: u32,
    ) -> Option<PdfSolidAngle> {
        if let Some(sampler) = self.shape.solid_angle_sampler(from) {
            return sampler.pdf(light_point);
        }
        let direction = light_point - from;
        let dir_to_light = direction.normalize();
        let light_normal = self.shape.normal_at(light_point, prim);
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

    /// What a ray from `from` along the unit `dir` finds of this light: each
    /// point where it meets the surface, nearest first, with the radiance
    /// emitted back toward `from` there and NEE's density for that point —
    /// the answers [`Light::sample_li`] gives for the same point
    /// ([`Emissive::radiance_toward`], [`Light::pdf_at_point`]). Points that
    /// emit nothing toward `from` (a one-sided light's back) are skipped.
    pub(super) fn found_along(&self, from: Vec3A, dir: Vec3A, mut f: impl FnMut(FoundAlong)) {
        for (t, prim) in self.shape.hits(from, dir).iter_on() {
            let point = from + t * dir;
            let front = self.shape.normal_at(point, prim).dot(-dir) > 0.0;
            let radiance = self.material.radiance_toward(point, -dir, front);
            if radiance == Vec3A::ZERO {
                continue;
            }
            f(FoundAlong {
                distance: t,
                radiance,
                pdf: self.pdf_at_point(from, point, prim),
            });
        }
    }
}

impl AreaLight {
    /// [`Light::sample_li`] on a mesh light: [`Light::sample_li`]'s area
    /// branch, with the triangle the point lies on handed to its normal.
    ///
    /// Apart and out of line so the analytic shapes' sample keeps its own
    /// code: carrying the triangle through it (a `(point, prim)` from the
    /// shape, a fourth arm in each dispatch) cost `veach_mis`, which has no
    /// mesh light, 2.3% of `sample_li`'s instructions (callgrind).
    #[inline(never)]
    fn sample_mesh(&self, mesh: &MeshShape, from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        let (light_point, prim) = mesh.sample_point_on(u, v);
        let to_light = light_point - from;
        let distance = to_light.length();
        if distance < 1e-6 {
            return None;
        }
        let direction = to_light / distance;
        // As in `sample_li`: `normalize`, so the pdf is the bounce side's.
        let dir_to_light = to_light.normalize();
        let light_normal = mesh.normal_at(light_point, prim);
        let front = light_normal.dot(-dir_to_light) > 0.0;
        let pdf = self.pdf_toward(to_light, dir_to_light, light_normal, light_point)?;
        Some(LightSample {
            direction,
            distance,
            radiance: self
                .material
                .radiance_toward(light_point, -dir_to_light, front),
            pdf,
        })
    }
}

impl Light for AreaLight {
    fn kind(&self) -> &'static str {
        self.shape.kind()
    }

    fn sample_li(&self, from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        if let AreaShape::Mesh(mesh) = &self.shape {
            return self.sample_mesh(mesh, from, u, v);
        }
        // The shape's solid-angle strategy where it has one from here, area
        // sampling otherwise. `pdf_at_point` makes the same choice through
        // `solid_angle_pdf`, which is what keeps the two MIS sides one strategy.
        // A point the strategy refuses is refused here too, never re-drawn
        // by area: which strategy applies is decided by `from` alone.
        let solid_angle = match self.shape.solid_angle_sampler(from) {
            Some(sampler) => Some(sampler.sample(u, v)?),
            None => None,
        };
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
        let light_normal = self.shape.normal_at(light_point, 0);
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

    fn pdf_at_point(&self, from: Vec3A, light_point: Vec3A, prim: u32) -> Option<PdfSolidAngle> {
        self.solid_angle_pdf(from, light_point, prim)
    }

    fn geom_id(&self) -> Option<u32> {
        Some(self.geom_id)
    }

    /// The emission's flux ([`Emissive::flux`]), with the projected area a
    /// shaped light integrates against taken from a fixed grid of points
    /// drawn from the shape's own area sampler, each weighted by the
    /// reciprocal of its density — exact for a flat light, whose normal is
    /// the same everywhere, and a close quadrature for a curved one.
    fn power(&self, luma: utils::Luma) -> Option<f32> {
        const GRID: usize = 16;
        let points: Vec<(Vec3A, f32)> = (0..GRID * GRID)
            .map(|k| {
                let u = ((k / GRID) as f32 + 0.5) / GRID as f32;
                let v = ((k % GRID) as f32 + 0.5) / GRID as f32;
                let (p, prim) = self.shape.sample_point_on(u, v);
                (
                    self.shape.normal_at(p, prim),
                    self.shape.inv_pdf_area(p).get(),
                )
            })
            .collect();
        let projected_area = |w: Vec3A| {
            points
                .iter()
                .map(|&(n, area)| area * n.dot(w).max(0.0))
                .sum::<f32>()
                / points.len() as f32
        };
        Some(luma.of(self.material.flux(self.shape.area(), projected_area)))
    }
    /// The shape's box, with the tighter of two direction bounds:
    /// - the surface's own: a flat one-sided light emits into the hemisphere
    ///   about its normal (`θo = 0`, `θe = π/2`), a flat two-sided one into
    ///   both, a curved one anywhere (`θo = π`);
    /// - its shaping cone, when one is authored: every direction farther
    ///   than the cone angle `a` from the shaping axis is dark
    ///   (`Shaping::factor`), whatever the shape, so `θe = min(a, π/2)` and
    ///   `θo = a − θe` (the split keeps `cos θ'` positive inside the cone).
    ///
    /// The second is taken when its spread `a` is below the first's: `π/2`
    /// one-sided flat, `π` otherwise.
    ///
    /// Its flux is [`Emissive::peak_flux`], not [`Light::power`]: see
    /// [`Light::bounds`].
    fn bounds(&self, luma: utils::Luma) -> Option<LightBounds> {
        let phi = luma.of(self.material.peak_flux(self.shape.area()));
        if !(phi.is_finite() && phi > 0.0) {
            return None;
        }
        let extent = self.shape.extent();
        let one_sided = self.material.is_one_sided();
        let half_pi = 0.5 * std::f32::consts::PI;
        let (mut w, mut cos_theta_o, mut cos_theta_e, mut two_sided, spread) = match extent.normal {
            Some(n) if one_sided => (n, 1.0, 0.0, false, half_pi),
            Some(n) => (n, 1.0, 0.0, true, std::f32::consts::PI),
            None => (Vec3A::Z, -1.0, 0.0, false, std::f32::consts::PI),
        };
        if let Some(shaping) = self.material.shaping() {
            let a = shaping
                .cone_angle_deg
                .to_radians()
                .clamp(0.0, std::f32::consts::PI);
            if a < spread {
                let theta_e = a.min(half_pi);
                w = shaping.axis();
                cos_theta_o = (a - theta_e).cos();
                cos_theta_e = theta_e.cos();
                two_sided = false;
            }
        }
        Some(LightBounds {
            min: extent.min,
            max: extent.max,
            w,
            phi,
            cos_theta_o,
            cos_theta_e,
            two_sided,
        })
    }
}
