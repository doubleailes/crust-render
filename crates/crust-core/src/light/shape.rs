//! Emitting surfaces: the [`LightShape`] trait, the sphere (sampled by the cone it
//! subtends) and the unit shapes under an affine placement.

use std::f32::consts::PI;

use glam::{Affine3A, Mat3A, Vec3A};

use crate::config::{DiskSampling, TubeSampling};
use crate::pdf::{InvPdfArea, PdfSolidAngle};
use crate::ray::TRACE_T_MIN;

use super::ellipse::SphericalEllipse;
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

    /// Where the ray from `origin` along `dir` meets the surface, beyond
    /// [`TRACE_T_MIN`]: the same surface the kernel intersects for the
    /// light's geometry (both sides of a flat light, a cylinder's wall but
    /// not its caps), so a bounce-side estimate of a shadow-linked light
    /// finds the light where a bounce ray would. Distances are in units of
    /// `dir`.
    #[must_use]
    fn hits(&self, origin: Vec3A, dir: Vec3A) -> ShapeHits;
}

/// Where a ray meets a light's surface ([`LightShape::hits`]): at most two
/// distances along it, nearest first.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShapeHits {
    t: [f32; 2],
    len: u8,
}

impl ShapeHits {
    /// Adds `t` when it is ahead of the ray's near bound. Pushed in
    /// increasing order.
    pub(super) fn push(&mut self, t: f32) {
        // From below, so a NaN root is rejected too.
        if t > TRACE_T_MIN && t < f32::INFINITY && (self.len as usize) < self.t.len() {
            self.t[self.len as usize] = t;
            self.len += 1;
        }
    }

    /// The distances, nearest first.
    pub fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        self.t[..self.len as usize].iter().copied()
    }

    /// The nearest distance, if the ray meets the surface at all.
    pub fn first(&self) -> Option<f32> {
        self.iter().next()
    }
}

/// The roots of `|oc + t·d|² = r²`, nearest first: the kernel's sphere and
/// cylinder arithmetic (`crust_rt`'s `SpherePrim::hit`), the closest-approach
/// discriminant and the stable root pair, so a light's analytic hit and the
/// kernel's agree to the kernel's own rounding.
fn quadratic_roots(oc: Vec3A, d: Vec3A, r: f32) -> Option<(f32, f32)> {
    let a = d.length_squared();
    if a == 0.0 {
        return None;
    }
    let half_b = oc.dot(d);
    let c = oc.length_squared() - r * r;
    let l = oc - d * (half_b / a);
    let discriminant = a * (r * r - l.length_squared());
    if discriminant < 0.0 {
        return None;
    }
    let q = -half_b - half_b.signum() * discriminant.sqrt();
    if q == 0.0 {
        return None;
    }
    let (t0, t1) = (c / q, q / a);
    Some(if t0 < t1 { (t0, t1) } else { (t1, t0) })
}

/// The two halves of a shape's solid-angle strategy, as calls: a sample and
/// the density of a point, from the same `from`, through the one sampler
/// [`LightShape::solid_angle_sampler`] returns. Implemented for every shape
/// and not overridable, so neither half can answer where the other does not.
///
/// Each answers `None` both where the shape has no strategy from `from` and
/// where the strategy refuses the point (see [`SolidAngleSampler::sample`]);
/// [`AreaLight`](super::AreaLight), which must tell the two apart, asks
/// [`LightShape::solid_angle_sampler`] itself.
pub trait SolidAngleSampling: LightShape {
    /// A point on the surface as seen from `from`, sampled by the shape's
    /// solid-angle density there, as `(point, pdf)`.
    #[must_use]
    fn sample_solid_angle(&self, from: Vec3A, u: f32, v: f32) -> Option<(Vec3A, PdfSolidAngle)> {
        self.solid_angle_sampler(from).and_then(|s| s.sample(u, v))
    }

    /// The solid-angle pdf, seen from `from`, of
    /// [`SolidAngleSampling::sample_solid_angle`] having produced `p` — the
    /// bounce side of MIS.
    #[must_use]
    fn solid_angle_pdf(&self, from: Vec3A, p: Vec3A) -> Option<PdfSolidAngle> {
        self.solid_angle_sampler(from).and_then(|s| s.pdf(p))
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
    /// A disk under an affine placement: the unit disk's spherical ellipse
    /// in local space, mapped out.
    Ellipse {
        shape: &'a AffineShape,
        from_local: Vec3A,
        ellipse: SphericalEllipse,
    },
    /// A one-sided tube seen from outside: the azimuth uniform over the arc
    /// of wall that faces `from`, `|φ − φ₀| < half_arc` in local space, and
    /// the axial position uniform or equiangular along that wall line.
    Tube {
        shape: &'a AffineShape,
        view: TubeView,
    },
}

/// A tube as seen from one shading point outside it: what its strategy
/// needs of `from` (see the lighting design record, "Disk and tube lights").
#[derive(Clone, Copy)]
pub(super) struct TubeView {
    from: Vec3A,
    /// `(f.y, f.z)` of `from` in local space, whose length is `ρ > 1`.
    across: (f32, f32),
    /// The arc's centre and half-width, `w = acos(1/ρ)`.
    phi0: f32,
    half_arc: f32,
    equiangular: bool,
}

impl<'a> SolidAngleSampler<'a> {
    pub(super) fn rect(shape: &'a RectShape, rect: SphericalRect) -> Self {
        SolidAngleSampler(Strategy::Rect { shape, rect })
    }

    /// A point on the surface from two unit random numbers, and its
    /// solid-angle pdf. `None` for a point the strategy refuses because its
    /// density there is not finite — a tube's silhouette — which
    /// [`SolidAngleSampler::pdf`] refuses too, so NEE never delivers it and
    /// the bounce side keeps its emission whole. Every other strategy always
    /// answers.
    #[inline(always)]
    pub fn sample(&self, u: f32, v: f32) -> Option<(Vec3A, PdfSolidAngle)> {
        match self.0 {
            Strategy::Cone {
                center,
                radius,
                from,
                cone,
            } => Some((point_on_cone(center, radius, from, &cone, u, v), cone.pdf())),
            Strategy::AffineCone {
                shape,
                from_local,
                cone,
            } => {
                let p_local = point_on_cone(Vec3A::ZERO, 1.0, from_local, &cone, u, v);
                Some((
                    shape.light_to_world.transform_point3a(p_local),
                    shape.world_solid_angle_pdf(cone.pdf(), p_local - from_local),
                ))
            }
            // The point is returned through the rectangle's own `(s, t)`
            // rather than the map's local coordinates, so it lies on the
            // light exactly as an area sample does — on the triangles a
            // bounce ray hits, and at the texel a textured card looks up.
            Strategy::Rect { shape, rect } => {
                let (s, t) = rect.sample(u as f64, v as f64);
                Some((shape.sample_point(s as f32, t as f32), rect.pdf()))
            }
            // Out of line, both, handed their state by value and answering
            // in registers — a point, then its density — so this match's
            // result never leaves them. Each other way cost every sphere and
            // rect sample on `veach_mis`, which has neither shape, 2–14% of
            // `sample_li`'s instructions (callgrind): inlined bodies, a
            // borrowed sampler copied to memory, or a returned
            // `Option<(Vec3A, _)>`, which merged every arm's answer in memory.
            // Each point's density is the one `pdf` gives it, so the two
            // halves of MIS agree on it bit for bit.
            Strategy::Ellipse {
                shape,
                from_local,
                ellipse,
            } => {
                let p = shape.ellipse_point(from_local, ellipse, u, v);
                p.is_finite()
                    .then(|| (p, shape.ellipse_pdf(from_local, ellipse, p)))
            }
            Strategy::Tube { shape, view } => {
                let p = shape.tube_point(view, u, v);
                shape.tube_pdf(view, p).map(|pdf| (p, pdf))
            }
        }
    }

    /// The solid-angle pdf of [`SolidAngleSampler::sample`] having produced
    /// `p`, a point on the surface. `None` exactly where `sample` refuses.
    #[inline(always)]
    pub fn pdf(&self, p: Vec3A) -> Option<PdfSolidAngle> {
        match self.0 {
            Strategy::Cone { cone, .. } => Some(cone.pdf()),
            Strategy::AffineCone {
                shape,
                from_local,
                cone,
            } => {
                let p_local = shape.world_to_light.transform_point3a(p);
                Some(shape.world_solid_angle_pdf(cone.pdf(), p_local - from_local))
            }
            Strategy::Rect { rect, .. } => Some(rect.pdf()),
            Strategy::Ellipse {
                shape,
                from_local,
                ellipse,
            } => Some(shape.ellipse_pdf(from_local, ellipse, p)),
            Strategy::Tube { shape, view } => shape.tube_pdf(view, p),
        }
    }
}

/// The straight line of a tube's wall at one azimuth, in world units, as the
/// equiangular density sees it from a shading point: the point's
/// foot at `s0` along it from the `x = −½` end, its distance `h` from it, its
/// world `length`, and the angles `θ = atan((s − s0) / h)` of its two ends.
struct WallLine {
    s0: f32,
    h: f32,
    length: f32,
    theta_a: f32,
    theta_b: f32,
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

    fn hits(&self, origin: Vec3A, dir: Vec3A) -> ShapeHits {
        let mut hits = ShapeHits::default();
        if let Some((near, far)) = quadratic_roots(origin - self.center, dir, self.radius) {
            hits.push(near);
            hits.push(far);
        }
        hits
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

    /// Where the local ray `o + t·d` meets the unit surface: the sphere, the
    /// disk (either side), or the tube's wall within `|x| ≤ ½`.
    pub(super) fn hits(self, o: Vec3A, d: Vec3A) -> ShapeHits {
        let mut hits = ShapeHits::default();
        match self {
            UnitShape::Sphere => {
                if let Some((near, far)) = quadratic_roots(o, d, 1.0) {
                    hits.push(near);
                    hits.push(far);
                }
            }
            UnitShape::Disk => {
                if d.z != 0.0 {
                    let t = -o.z / d.z;
                    let p = o + t * d;
                    if p.x * p.x + p.y * p.y <= 1.0 {
                        hits.push(t);
                    }
                }
            }
            UnitShape::Cylinder => {
                let across = Vec3A::new(0.0, 1.0, 1.0);
                if let Some((near, far)) = quadratic_roots(o * across, d * across, 1.0) {
                    for t in [near, far] {
                        if (o.x + t * d.x).abs() <= 0.5 {
                            hits.push(t);
                        }
                    }
                }
            }
        }
        hits
    }
}

/// A [`UnitShape`] under an arbitrary invertible affine placement — which is
/// what the spec's "world-space surface area … including any scaling applied
/// to the light by its transform stack" requires of a squashed sphere, an
/// elliptical disk or an elliptical tube.
///
/// A sphere seen from outside is sampled by its visible cone in local space,
/// a one-sided tube seen from outside by the arc of wall facing the point,
/// and a disk seen from in front, with `CRUST_DISK_SAMPLING=ellipse`, by its
/// spherical ellipse (see [`LightShape::solid_angle_sampler`] below).
/// Otherwise — a point inside the sphere or the tube, a disk seen from behind
/// or out of its band, or either strategy switched off — sampling is uniform
/// in the shape's *local* area and mapped through the placement, so in world
/// space it is denser where the placement compresses the surface. That is
/// fine — MIS needs a density both sides agree on, not a
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
    /// How a tube is sampled from outside (`CRUST_TUBE_SAMPLING`).
    tube: TubeSampling,
    /// How a disk is sampled from in front (`CRUST_DISK_SAMPLING`).
    disk: DiskSampling,
    /// Whether the emitter on this surface is one-sided, which
    /// [`AreaLight::new`](super::AreaLight::new) sets. A tube's visible-arc
    /// strategy needs it: a two-sided tube seen through an open end shows
    /// its inner wall, which the outer arc never reaches.
    front_only: bool,
}

impl AffineShape {
    /// `None` for a transform that collapses the shape (not invertible).
    /// Disks and tubes are sampled as the process's [`crate::config()`] says;
    /// [`AffineShape::with_sampling`] chooses otherwise.
    pub fn new(unit: UnitShape, light_to_world: Affine3A) -> Option<Self> {
        let det = light_to_world.matrix3.determinant();
        if !det.is_finite() || det.abs() < 1e-30 {
            return None;
        }
        let world_to_light = light_to_world.inverse();
        let config = crate::config();
        let mut shape = Self {
            unit,
            light_to_world,
            world_to_light,
            normal_mat: world_to_light.matrix3.transpose(),
            abs_det: det.abs(),
            area: 0.0,
            tube: config.tube_sampling,
            disk: config.disk_sampling,
            front_only: false,
        };
        shape.area = shape.integrate_area();
        Some(shape)
    }

    /// The same shape, sampled by these strategies whatever the environment
    /// says: both sides of a switch in one process.
    #[must_use]
    pub fn with_sampling(mut self, tube: TubeSampling, disk: DiskSampling) -> Self {
        self.tube = tube;
        self.disk = disk;
        self
    }

    pub(super) fn set_front_only(&mut self, front_only: bool) {
        self.front_only = front_only;
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
        // Under a placement that scales the curved axes uniformly (any
        // similarity, for the sphere) the area scale is the same at every
        // normal, so the grid below would only re-add one value 131k times —
        // at a million translated sphere lights, that grid was the import.
        let uniform = match self.unit {
            UnitShape::Disk => true,
            UnitShape::Sphere => self.scales_uniformly(&[0, 1, 2]),
            UnitShape::Cylinder => self.scales_uniformly(&[1, 2]),
        };
        let mean = match self.unit {
            UnitShape::Disk => self.area_scale(-Vec3A::Z) as f64,
            UnitShape::Sphere if uniform => self.area_scale(Vec3A::Z) as f64,
            UnitShape::Cylinder if uniform => self.area_scale(Vec3A::Y) as f64,
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

    /// Whether the placement's columns are mutually perpendicular and those
    /// in `axes` equally long, exactly up to a few ulps — then
    /// [`AffineShape::area_scale`] is constant over normals spanned by `axes`.
    fn scales_uniformly(&self, axes: &[usize]) -> bool {
        const TOL: f32 = 4.0 * f32::EPSILON;
        let m = self.light_to_world.matrix3;
        let cols = [m.x_axis, m.y_axis, m.z_axis];
        let len = cols.map(|c| c.length());
        let s = len[axes[0]];
        let perpendicular =
            |a: usize, b: usize| cols[a].dot(cols[b]).abs() <= TOL * len[a] * len[b];
        axes.iter().all(|&a| (len[a] - s).abs() <= TOL * s)
            && perpendicular(0, 1)
            && perpendicular(0, 2)
            && perpendicular(1, 2)
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
    /// direction map (see [`AffineShape::world_solid_angle_pdf`]), so unlike
    /// the round sphere's it varies across the cap.
    ///
    /// A disk seen strictly from in front is sampled by the spherical
    /// ellipse the unit disk subtends in local space, within a solid-angle
    /// band (`SphericalEllipse::new`), the same way. A one-sided tube seen
    /// from outside is sampled over the arc of wall facing `from`, which an
    /// affine map preserves for the same reason (the lighting design record,
    /// "Disk and tube lights").
    #[inline(always)]
    fn solid_angle_sampler(&self, from: Vec3A) -> Option<SolidAngleSampler<'_>> {
        match self.unit {
            UnitShape::Sphere => {
                let from_local = self.world_to_light.transform_point3a(from);
                let cone = SubtendedCone::new(Vec3A::ZERO, 1.0, from_local)?;
                Some(SolidAngleSampler(Strategy::AffineCone {
                    shape: self,
                    from_local,
                    cone,
                }))
            }
            // Their setup out of line, as their strategies' bodies are (see
            // `SolidAngleSampler::sample`), behind the switch, which a disk or
            // tube that keeps area sampling reads without a call.
            UnitShape::Disk if self.disk != DiskSampling::Area => {
                let from_local = self.world_to_light.transform_point3a(from);
                let ellipse = SphericalEllipse::new(from_local)?;
                Some(SolidAngleSampler(Strategy::Ellipse {
                    shape: self,
                    from_local,
                    ellipse,
                }))
            }
            UnitShape::Cylinder if self.tube != TubeSampling::Area && self.front_only => {
                let view = self.tube_view(from)?;
                Some(SolidAngleSampler(Strategy::Tube { shape: self, view }))
            }
            UnitShape::Disk | UnitShape::Cylinder => None,
        }
    }

    /// In local space, where the surface is the unit one: an affine map
    /// sends the line `o + t·d` to `o' + t·d'`, so `t` is the same in both.
    fn hits(&self, origin: Vec3A, dir: Vec3A) -> ShapeHits {
        self.unit.hits(
            self.world_to_light.transform_point3a(origin),
            self.world_to_light.matrix3 * dir,
        )
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

    /// The tube as seen from `from` for its visible-arc strategy,
    /// `None` on the wall or inside it. The caller has checked the switch and
    /// that the emitter is one-sided.
    #[inline(never)]
    fn tube_view(&self, from: Vec3A) -> Option<TubeView> {
        let f = self.world_to_light.transform_point3a(from);
        let rho2 = f.y * f.y + f.z * f.z;
        // On the wall or inside the tube, a one-sided tube shows only
        // its back, which area sampling already finds dark for free.
        if !(rho2 > 1.0 && rho2.is_finite()) {
            return None;
        }
        let half_arc = (1.0 / rho2.sqrt()).acos();
        if half_arc.is_nan() || half_arc <= 0.0 {
            return None;
        }
        Some(TubeView {
            from,
            across: (f.y, f.z),
            phi0: f.z.atan2(f.y),
            half_arc,
            equiangular: self.tube == TubeSampling::Equiangular,
        })
    }

    /// The disk's spherical-ellipse sample: on the unit disk in local space,
    /// as an area sample is, then placed. NaN, refused by the caller, if the
    /// map cannot be rebuilt from `from_local` — which `ellipse` was built
    /// from, so never.
    #[inline(never)]
    fn ellipse_point(&self, from_local: Vec3A, ellipse: SphericalEllipse, u: f32, v: f32) -> Vec3A {
        match ellipse.sample(from_local, u as f64, v as f64) {
            Some(p) => self
                .light_to_world
                .transform_point3a(Vec3A::from(p.as_vec3())),
            None => Vec3A::NAN,
        }
    }

    /// The disk's spherical-ellipse density at the world point `p`: the local
    /// one, uniform, through the direction map's Jacobian, as for the
    /// squashed sphere.
    #[inline(never)]
    fn ellipse_pdf(&self, from_local: Vec3A, ellipse: SphericalEllipse, p: Vec3A) -> PdfSolidAngle {
        let p_local = self.world_to_light.transform_point3a(p);
        self.world_solid_angle_pdf(ellipse.pdf(), p_local - from_local)
    }

    /// The tube's sample: the azimuth uniform over the arc
    /// from `v`, the axial position from `u`, uniform or equiangular along
    /// that wall line.
    #[inline(never)]
    fn tube_point(&self, view: TubeView, u: f32, v: f32) -> Vec3A {
        let (sin, cos) = (view.phi0 + view.half_arc * (2.0 * v - 1.0)).sin_cos();
        let x = if view.equiangular {
            let line = self.wall_line(view.from, cos, sin);
            let theta = line.theta_a + u * (line.theta_b - line.theta_a);
            let s = (line.s0 + line.h * theta.tan()).clamp(0.0, line.length);
            s / line.length - 0.5
        } else {
            u - 0.5
        };
        self.light_to_world
            .transform_point3a(Vec3A::new(x, cos, sin))
    }

    /// The tube's wall line at the azimuth `(cos φ, sin φ)`, in world space,
    /// as seen from `from`. The line is straight under any affine
    /// placement, which is what makes the equiangular density follow the
    /// real `1/r²` on a tube scaled `(length, r, r)`.
    fn wall_line(&self, from: Vec3A, cos: f32, sin: f32) -> WallLine {
        let start = self
            .light_to_world
            .transform_point3a(Vec3A::new(-0.5, cos, sin));
        let axis = self.light_to_world.matrix3.x_axis;
        let length = axis.length();
        let along = axis / length;
        let d = from - start;
        let s0 = d.dot(along);
        // Positive from outside the tube: `from` on the line needs `ρ = 1`.
        let h = d.cross(along).length();
        WallLine {
            s0,
            h,
            length,
            theta_a: (-s0 / h).atan(),
            theta_b: ((length - s0) / h).atan(),
        }
    }

    /// The tube strategy's solid-angle density at the world point `p` on
    /// the wall, seen from `from` (local `across` = `(f.y, f.z)`): `1 / 2w`
    /// over the arc times the axial density, through the area
    /// scale and the area-to-solid-angle Jacobian. `None` for a point that
    /// does not face `from` (outside the arc, where a one-sided tube is dark
    /// and NEE never samples) or seen edge-on, where it is infinite.
    #[inline(never)]
    fn tube_pdf(&self, view: TubeView, p: Vec3A) -> Option<PdfSolidAngle> {
        let TubeView {
            from,
            across,
            half_arc,
            equiangular,
            ..
        } = view;
        let local = self.world_to_light.transform_point3a(p);
        let n_local = Vec3A::new(0.0, local.y, local.z).normalize_or(Vec3A::Y);
        // `n · (f − p) > 0` on the unit wall: the point faces `from`, i.e.
        // lies strictly inside the arc.
        if n_local.y * across.0 + n_local.z * across.1 <= 1.0 {
            return None;
        }
        let to = p - from;
        let dist2 = to.length_squared();
        let n_world = (self.normal_mat * n_local).normalize_or(Vec3A::Z);
        let cos = n_world.dot(-to.normalize()).abs();
        let arc_area = 2.0 * half_arc * self.area_scale(n_local);
        if !equiangular {
            return InvPdfArea::new(arc_area).to_solid_angle(dist2, cos);
        }
        // The equiangular axial density in world arc length is
        // `h / ((θb − θa) (h² + (s − s0)²))`, and `h² + (s − s0)²` is the
        // distance squared, which the area-to-solid-angle Jacobian cancels:
        // the solid-angle density is `L h / ((θb − θa) · 2w · scale · cos)`.
        let line = self.wall_line(from, n_local.y, n_local.z);
        let pdf = line.length * line.h / ((line.theta_b - line.theta_a) * arc_area * cos);
        if cos > 0.0 {
            PdfSolidAngle::new(pdf)
        } else {
            None
        }
    }
}
