//! Emitting surfaces: the [`LightShape`] trait, the sphere (sampled by the cone it
//! subtends) and the unit shapes under an affine placement.

use std::f32::consts::PI;

use glam::{Affine3A, Mat3A, Vec3A};

use crate::pdf::{InvPdfArea, PdfSolidAngle};

use super::rect::{RectShape, SphericalRect};

/// The emitting surface of an area light, decoupled from any material: pure
/// geometry that knows how to sample itself uniformly by area, and — where it
/// has a better strategy — by the solid angle it subtends from a shading
/// point. One shape implementation per supported UsdLux schema (sphere, rect,
/// …).
pub trait LightShape: Send + Sync {
    /// Short name for the `--stats` light breakdown.
    fn kind(&self) -> &'static str {
        "area"
    }

    /// A point on the surface, uniform by area, from two unit random numbers.
    fn sample_point(&self, u: f32, v: f32) -> Vec3A;

    /// Outward surface normal at a point known to lie on the shape.
    fn normal_at(&self, p: Vec3A) -> Vec3A;

    /// Total world-space surface area — what `inputs:normalize` divides by.
    fn area(&self) -> f32;

    /// The reciprocal of [`LightShape::sample_point`]'s area density at a
    /// point on the surface. For a shape sampled uniformly by area that is the
    /// area itself, the default. A shape whose sampling is *not* uniform in
    /// world area — a unit sphere mapped through a non-uniform scale is denser
    /// where it is squashed — overrides it. Both halves of MIS read this, so
    /// the density need only be the one actually sampled, not uniform.
    fn inv_pdf_area(&self, _p: Vec3A) -> InvPdfArea {
        InvPdfArea::new(self.area())
    }

    /// The shape's strategy for sampling a point by the *solid angle* it
    /// subtends from `from`, or `None` (the default) where it has none from
    /// there, and [`AreaLight`](super::AreaLight) falls back to
    /// [`LightShape::sample_point`] by area.
    ///
    /// One hook for both halves of MIS: NEE draws from the sampler
    /// ([`SolidAngleSampler::sample`]) and the bounce side asks the same
    /// sampler its density ([`SolidAngleSampler::pdf`]), so they cannot
    /// disagree on which `from`s the strategy covers — the decision is made
    /// here, once, on `from` alone. What remains the implementation's to get
    /// right: every point the sampler returns must be one a ray from `from`
    /// could hit first, i.e. on the side of the shape that faces it.
    #[must_use]
    fn solid_angle_sampler(&self, _from: Vec3A) -> Option<SolidAngleSampler<'_>> {
        None
    }
}

/// The two halves of a shape's solid-angle strategy, as calls: a sample and
/// the density of a point, from the same `from`, through the one sampler
/// [`LightShape::solid_angle_sampler`] returns. Implemented for every shape
/// and not overridable, so neither half can answer where the other does not.
pub trait SolidAngleSampling: LightShape {
    /// A point on the surface as seen from `from`, sampled by the shape's
    /// solid-angle density there, as `(point, pdf)`. `None` where the shape
    /// has no such strategy from `from`.
    #[must_use]
    fn sample_solid_angle(&self, from: Vec3A, u: f32, v: f32) -> Option<(Vec3A, PdfSolidAngle)> {
        self.solid_angle_sampler(from).map(|s| s.sample(u, v))
    }

    /// The solid-angle pdf, seen from `from`, of
    /// [`SolidAngleSampling::sample_solid_angle`] having produced `p` — the
    /// bounce side of MIS. `None` exactly when `sample_solid_angle` is.
    #[must_use]
    fn solid_angle_pdf(&self, from: Vec3A, p: Vec3A) -> Option<PdfSolidAngle> {
        self.solid_angle_sampler(from).map(|s| s.pdf(p))
    }
}

impl<T: LightShape + ?Sized> SolidAngleSampling for T {}

/// A shape's solid-angle strategy from one shading point: what
/// [`LightShape::solid_angle_sampler`] returns once it has decided the
/// strategy applies there.
#[derive(Clone, Copy)]
pub struct SolidAngleSampler<'a>(Strategy<'a>);

#[derive(Clone, Copy)]
enum Strategy<'a> {
    /// A round sphere, uniform over the cone it subtends.
    Cone {
        center: Vec3A,
        radius: f32,
        from: Vec3A,
        cone: SubtendedCone,
    },
    /// A sphere under an affine placement: the unit sphere's cone in local
    /// space, mapped out.
    AffineCone {
        shape: &'a AffineShape,
        from_local: Vec3A,
        cone: SubtendedCone,
    },
    /// A rectangle, uniform over the spherical rectangle it subtends.
    Rect {
        shape: &'a RectShape,
        rect: SphericalRect,
    },
}

impl<'a> SolidAngleSampler<'a> {
    pub(super) fn rect(shape: &'a RectShape, rect: SphericalRect) -> Self {
        SolidAngleSampler(Strategy::Rect { shape, rect })
    }

    /// A point on the surface from two unit random numbers, and its
    /// solid-angle pdf.
    #[inline(always)]
    pub fn sample(&self, u: f32, v: f32) -> (Vec3A, PdfSolidAngle) {
        match self.0 {
            Strategy::Cone {
                center,
                radius,
                from,
                cone,
            } => (point_on_cone(center, radius, from, &cone, u, v), cone.pdf()),
            Strategy::AffineCone {
                shape,
                from_local,
                cone,
            } => {
                let p_local = point_on_cone(Vec3A::ZERO, 1.0, from_local, &cone, u, v);
                (
                    shape.light_to_world.transform_point3a(p_local),
                    shape.world_solid_angle_pdf(cone.pdf(), p_local - from_local),
                )
            }
            // The point is returned through the rectangle's own `(s, t)`
            // rather than the map's local coordinates, so it lies on the
            // light exactly as an area sample does — on the triangles a
            // bounce ray hits, and at the texel a textured card looks up.
            Strategy::Rect { shape, rect } => {
                let (s, t) = rect.sample(u as f64, v as f64);
                (shape.sample_point(s as f32, t as f32), rect.pdf())
            }
        }
    }

    /// The solid-angle pdf of [`SolidAngleSampler::sample`] having produced
    /// `p`, a point on the surface.
    #[inline(always)]
    pub fn pdf(&self, p: Vec3A) -> PdfSolidAngle {
        match self.0 {
            Strategy::Cone { cone, .. } => cone.pdf(),
            Strategy::AffineCone {
                shape,
                from_local,
                cone,
            } => {
                let p_local = shape.world_to_light.transform_point3a(p);
                shape.world_solid_angle_pdf(cone.pdf(), p_local - from_local)
            }
            Strategy::Rect { rect, .. } => rect.pdf(),
        }
    }
}

/// `sin² 1.5°`, pbrt-v4's threshold. Below it a cone's `1 − cos θ_max` is
/// taken as `sin² θ_max / (1 + cos θ_max)` instead of by the subtraction, which
/// in f32 cancels to nothing for a small, distant sphere, and a direction in it
/// is drawn the same cancellation-free way (see [`SubtendedCone::sample`]).
pub(super) const SMALL_CONE_SIN2: f32 = 0.000_685_23;

/// The cone a sphere subtends from a point outside it.
#[derive(Clone, Copy, Debug)]
pub(super) struct SubtendedCone {
    /// `sin² θ_max = r² / d²`.
    pub(super) sin2_max: f32,
    pub(super) cos_max: f32,
    /// `1 − cos θ_max`, cancellation-free (see [`SMALL_CONE_SIN2`]).
    pub(super) one_minus_cos_max: f32,
}

impl SubtendedCone {
    /// `None` from inside the sphere (or on it), where there is no cone and
    /// every direction reaches the surface — and for a cone too thin to
    /// represent: a zero radius, `r²/d²` underflowing, or a solid angle so
    /// small that its pdf overflows. Those fall back to area sampling, and
    /// since both [`LightShape`] hooks construct the cone here, they fall back
    /// together.
    pub(super) fn new(center: Vec3A, radius: f32, from: Vec3A) -> Option<Self> {
        let d2 = (center - from).length_squared();
        let r2 = radius * radius;
        if d2 <= r2 {
            return None;
        }
        let sin2_max = r2 / d2;
        if sin2_max.is_nan() || sin2_max <= 0.0 {
            return None;
        }
        let cos_max = (1.0 - sin2_max).max(0.0).sqrt();
        let one_minus_cos_max = if sin2_max < SMALL_CONE_SIN2 {
            sin2_max / (1.0 + cos_max)
        } else {
            1.0 - cos_max
        };
        let cone = Self {
            sin2_max,
            cos_max,
            one_minus_cos_max,
        };
        PdfSolidAngle::new(cone.pdf().get()).map(|_| cone)
    }

    /// Uniform over the cone: `1 / (2π (1 − cos θ_max))`. Finite and positive
    /// for every cone [`SubtendedCone::new`] returns.
    pub(super) fn pdf(&self) -> PdfSolidAngle {
        PdfSolidAngle::from_measure(1.0 / (2.0 * PI * self.one_minus_cos_max))
    }

    /// A direction uniform over the cone, as `(sin² θ, cos θ)` of its angle θ
    /// off the axis: `1 − cos θ = u (1 − cos θ_max)`, which is what makes it
    /// uniform in solid angle. The small-cone branch keeps that exact rather
    /// than approximating it — pbrt-v4 draws `sin² θ = u sin² θ_max` there,
    /// whose density is proportional to `cos θ` and so disagrees with
    /// [`SubtendedCone::pdf`] by up to `sin² θ_max / 4` — and takes `sin² θ` as
    /// `t (2 − t)` rather than `1 − cos² θ`, which would cancel.
    pub(super) fn sample(&self, u: f32) -> (f32, f32) {
        if self.sin2_max < SMALL_CONE_SIN2 {
            let t = u * self.one_minus_cos_max;
            (t * (2.0 - t), 1.0 - t)
        } else {
            let cos = (self.cos_max - 1.0) * u + 1.0;
            (1.0 - cos * cos, cos)
        }
    }
}

/// Spherical light surface (UsdLux `SphereLight`).
///
/// Seen from outside, it is sampled uniformly over the **cone it subtends**
/// (Shirley, Wang & Zimmerman 1996; pbrt-v4's `Sphere::Sample`), not over its
/// area: area sampling spends at least half its samples on the hemisphere
/// facing away — every one of them a shadow ray the sphere itself occludes —
/// and weights the rest by a `cos/r²` that blows up at the silhouette. From
/// inside, where there is no cone, it falls back to area sampling.
#[derive(Clone)]
pub struct SphereShape {
    pub center: Vec3A,
    pub radius: f32,
}

impl LightShape for SphereShape {
    fn kind(&self) -> &'static str {
        "sphere"
    }

    fn sample_point(&self, u: f32, v: f32) -> Vec3A {
        let theta = 2.0 * std::f32::consts::PI * u;
        let phi = (1.0 - 2.0 * v).acos();
        let n = Vec3A::new(phi.sin() * theta.cos(), phi.sin() * theta.sin(), phi.cos());
        self.center + self.radius * n
    }

    fn normal_at(&self, p: Vec3A) -> Vec3A {
        (p - self.center).normalize()
    }

    fn area(&self) -> f32 {
        4.0 * std::f32::consts::PI * self.radius * self.radius
    }

    #[inline(always)]
    fn solid_angle_sampler(&self, from: Vec3A) -> Option<SolidAngleSampler<'_>> {
        let cone = SubtendedCone::new(self.center, self.radius, from)?;
        Some(SolidAngleSampler(Strategy::Cone {
            center: self.center,
            radius: self.radius,
            from,
            cone,
        }))
    }
}

/// A point on a sphere, uniform over the cone it subtends from `from`
/// (`cone`, which [`SubtendedCone::new`] built for the same three). Shared by
/// [`SphereShape`] and, in its local space, by an [`AffineShape`] sphere.
#[inline]
fn point_on_cone(
    center: Vec3A,
    radius: f32,
    from: Vec3A,
    cone: &SubtendedCone,
    u: f32,
    v: f32,
) -> Vec3A {
    // A direction uniform in the cone, as its angle θ off the axis toward the
    // centre...
    let (sin2_theta, cos_theta) = cone.sample(u);
    // ...then the point it meets on the sphere in closed form, as the angle α
    // at the centre between the axis and that point (the law of
    // sines/cosines), rather than by intersecting a ray. `sin² θ / sin θ_max`
    // plus `cos θ · √(1 − sin² θ / sin² θ_max)` is pbrt-v4's arrangement.
    let cos_alpha = sin2_theta / cone.sin2_max.sqrt()
        + cos_theta * (1.0 - sin2_theta / cone.sin2_max).max(0.0).sqrt();
    let sin_alpha = (1.0 - cos_alpha * cos_alpha).max(0.0).sqrt();
    let phi = 2.0 * PI * v;
    let local = Vec3A::new(sin_alpha * phi.cos(), sin_alpha * phi.sin(), cos_alpha);
    // α is measured from the centre toward `from`, so the point lies on the cap
    // that faces it.
    let n = utils::align_to_normal(local, (from - center).normalize()).normalize();
    center + radius * n
}

/// The canonical local-space shapes UsdLux defines its round lights on,
/// before the light's radius, length and transform are applied: a unit
/// sphere; a unit disk in the XY plane emitting along −Z (`DiskLight`); and
/// a unit-radius, unit-length open tube along X, centred on the origin
/// (`CylinderLight`, which "does not emit light from the flat end-caps").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitShape {
    Sphere,
    Disk,
    Cylinder,
}

impl UnitShape {
    /// Surface area in local space.
    pub(super) fn local_area(self) -> f32 {
        match self {
            UnitShape::Sphere => 4.0 * PI,
            UnitShape::Disk => PI,
            UnitShape::Cylinder => 2.0 * PI,
        }
    }

    /// A point uniform by *local* area.
    pub(super) fn sample(self, u: f32, v: f32) -> Vec3A {
        let phi = 2.0 * PI * v;
        match self {
            UnitShape::Sphere => {
                let z = 1.0 - 2.0 * u;
                let r = (1.0 - z * z).max(0.0).sqrt();
                Vec3A::new(r * phi.cos(), r * phi.sin(), z)
            }
            UnitShape::Disk => {
                let r = u.sqrt();
                Vec3A::new(r * phi.cos(), r * phi.sin(), 0.0)
            }
            UnitShape::Cylinder => Vec3A::new(u - 0.5, phi.cos(), phi.sin()),
        }
    }

    /// The local emitting normal at a local point.
    pub(super) fn normal(self, p: Vec3A) -> Vec3A {
        match self {
            UnitShape::Sphere => p.normalize_or(Vec3A::Z),
            UnitShape::Disk => -Vec3A::Z,
            UnitShape::Cylinder => Vec3A::new(0.0, p.y, p.z).normalize_or(Vec3A::Y),
        }
    }
}

/// A [`UnitShape`] under an arbitrary invertible affine placement — which is
/// what the spec's "world-space surface area … including any scaling applied
/// to the light by its transform stack" requires of a squashed sphere, an
/// elliptical disk or an elliptical tube.
///
/// A sphere seen from outside is sampled by its visible cone in local space
/// (see [`SolidAngleSampling::sample_solid_angle`]). Otherwise — a disk, a tube,
/// or a point inside the sphere — sampling is uniform in the shape's *local*
/// area and mapped through the placement, so in world space it is denser
/// where the placement compresses the surface. That is fine — MIS needs a density both sides agree on, not a
/// uniform one — and [`LightShape::inv_pdf_area`] reports it exactly: an
/// affine map `M` scales the area element at a point with unit local normal
/// `n` by `|det M| · |M⁻ᵀ n|`. For a disk that factor is constant, so the
/// sampling is uniform after all.
#[derive(Clone)]
pub struct AffineShape {
    pub(super) unit: UnitShape,
    pub(super) light_to_world: Affine3A,
    pub(super) world_to_light: Affine3A,
    /// `M⁻ᵀ`, the normal transform.
    pub(super) normal_mat: Mat3A,
    pub(super) abs_det: f32,
    pub(super) area: f32,
}

impl AffineShape {
    /// `None` for a transform that collapses the shape (not invertible).
    pub fn new(unit: UnitShape, light_to_world: Affine3A) -> Option<Self> {
        let det = light_to_world.matrix3.determinant();
        if !det.is_finite() || det.abs() < 1e-30 {
            return None;
        }
        let world_to_light = light_to_world.inverse();
        let mut shape = Self {
            unit,
            light_to_world,
            world_to_light,
            normal_mat: world_to_light.matrix3.transpose(),
            abs_det: det.abs(),
            area: 0.0,
        };
        shape.area = shape.integrate_area();
        Some(shape)
    }

    pub fn unit(&self) -> UnitShape {
        self.unit
    }

    /// How much the placement scales local area at a point with unit local
    /// normal `n`.
    pub(super) fn area_scale(&self, n: Vec3A) -> f32 {
        self.abs_det * (self.normal_mat * n).length()
    }

    /// World-space area, integrating the area scale over the local surface.
    /// Exact for the disk (the scale is constant) and for any similarity;
    /// under a non-uniform scale the sphere's and tube's scale varies over
    /// the surface and a midpoint rule over a fine grid is well inside f32
    /// precision — done once, at import. Replaces hdEmbree's closed-form
    /// approximations (Knud Thomsen's ellipsoid, Ramanujan's ellipse
    /// perimeter), which are close but not what the spec asks for.
    pub(super) fn integrate_area(&self) -> f32 {
        let local = self.unit.local_area();
        let mean = match self.unit {
            UnitShape::Disk => self.area_scale(-Vec3A::Z) as f64,
            UnitShape::Cylinder => {
                const N: usize = 4096;
                (0..N)
                    .map(|i| {
                        let phi = 2.0 * PI * (i as f32 + 0.5) / N as f32;
                        self.area_scale(Vec3A::new(0.0, phi.cos(), phi.sin())) as f64
                    })
                    .sum::<f64>()
                    / N as f64
            }
            UnitShape::Sphere => {
                // Uniform-area grid: z and φ both uniform.
                const NZ: usize = 256;
                const NP: usize = 512;
                let mut sum = 0.0f64;
                for i in 0..NZ {
                    let z = 1.0 - 2.0 * (i as f32 + 0.5) / NZ as f32;
                    let r = (1.0 - z * z).max(0.0).sqrt();
                    for j in 0..NP {
                        let phi = 2.0 * PI * (j as f32 + 0.5) / NP as f32;
                        sum += self.area_scale(Vec3A::new(r * phi.cos(), r * phi.sin(), z)) as f64;
                    }
                }
                sum / (NZ * NP) as f64
            }
        };
        (local as f64 * mean) as f32
    }
}

impl LightShape for AffineShape {
    fn kind(&self) -> &'static str {
        match self.unit {
            UnitShape::Sphere => "sphere (affine)",
            UnitShape::Disk => "disk",
            UnitShape::Cylinder => "cylinder",
        }
    }

    fn sample_point(&self, u: f32, v: f32) -> Vec3A {
        self.light_to_world
            .transform_point3a(self.unit.sample(u, v))
    }

    fn normal_at(&self, p: Vec3A) -> Vec3A {
        let local = self.world_to_light.transform_point3a(p);
        (self.normal_mat * self.unit.normal(local)).normalize_or(Vec3A::Z)
    }

    fn area(&self) -> f32 {
        self.area
    }

    fn inv_pdf_area(&self, p: Vec3A) -> InvPdfArea {
        let local = self.world_to_light.transform_point3a(p);
        InvPdfArea::new(self.unit.local_area() * self.area_scale(self.unit.normal(local)))
    }

    /// A squashed sphere is sampled by the cone the *unit* sphere subtends in
    /// local space, mapped through the placement. That is sound because an
    /// affine map preserves visibility on a convex surface: with normals
    /// carried by `M⁻ᵀ`, `n_w · (x − p) = n_l · (x_l − p_l) / |M⁻ᵀ n_l|`, so a
    /// point faces the shading point in world space exactly when it does in
    /// local space, and the sampled cap is the ellipsoid's visible one. The
    /// world density is the local cone's times the solid-angle Jacobian of the
    /// direction map (see `AffineShape::world_solid_angle_pdf`), so unlike
    /// the round sphere's it varies across the cap.
    #[inline(always)]
    fn solid_angle_sampler(&self, from: Vec3A) -> Option<SolidAngleSampler<'_>> {
        if self.unit != UnitShape::Sphere {
            return None;
        }
        let from_local = self.world_to_light.transform_point3a(from);
        let cone = SubtendedCone::new(Vec3A::ZERO, 1.0, from_local)?;
        Some(SolidAngleSampler(Strategy::AffineCone {
            shape: self,
            from_local,
            cone,
        }))
    }
}

impl AffineShape {
    /// A local solid-angle density as a world one, for the direction
    /// `to_local` (from the shading point toward the surface, in local space).
    /// The placement sends a local direction `ω` to `Mω / |Mω|`, which scales
    /// solid angle by `|det M| / |Mω|³`; a density scales by the reciprocal.
    ///
    /// Taken as it comes rather than refused when not finite: the refusal has
    /// to depend on `from` alone (see [`LightShape::sample_solid_angle`]), and
    /// this varies across the cap.
    pub(super) fn world_solid_angle_pdf(
        &self,
        local_pdf: PdfSolidAngle,
        to_local: Vec3A,
    ) -> PdfSolidAngle {
        let stretch = (self.light_to_world.matrix3 * to_local.normalize()).length();
        PdfSolidAngle::from_measure(local_pdf.get() * stretch * stretch * stretch / self.abs_det)
    }
}
