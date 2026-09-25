use crate::environment::{EnvironmentMap, luminance};
use crate::material::Emissive;
use glam::{Affine3A, DVec2, DVec3, Mat3A, Vec3, Vec3A};
use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::Arc;

/// The emitting surface of an area light, decoupled from any material: pure
/// geometry that knows how to sample itself uniformly by area, and — where it
/// has a better strategy — by the solid angle it subtends from a shading
/// point. One shape implementation per supported UsdLux schema (sphere, rect,
/// …).
pub trait LightShape: Send + Sync {
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
    fn inv_pdf_area(&self, _p: Vec3A) -> f32 {
        self.area()
    }

    /// A point on the surface as seen from `from`, sampled by a density over
    /// the *solid angle* the shape subtends there, as `(point, pdf)` with the
    /// pdf in solid-angle measure. `None` (the default) means the shape has no
    /// such strategy from `from`, and [`AreaLight`] falls back to
    /// [`LightShape::sample_point`] by area.
    ///
    /// Two rules make it safe to implement. Whether it answers must depend on
    /// `from` alone — never on `u`, `v` — and [`LightShape::solid_angle_pdf`]
    /// must answer for exactly the same `from`s with the same density, or the
    /// two MIS sides describe different strategies and emission is
    /// double-counted. And every point it returns must be one a ray from
    /// `from` could hit first, i.e. on the side of the shape that faces it.
    fn sample_solid_angle(&self, _from: Vec3A, _u: f32, _v: f32) -> Option<(Vec3A, f32)> {
        None
    }

    /// The solid-angle pdf, seen from `from`, of
    /// [`LightShape::sample_solid_angle`] having produced `p` — the bounce side
    /// of MIS. `None` exactly when `sample_solid_angle` is.
    fn solid_angle_pdf(&self, _from: Vec3A, _p: Vec3A) -> Option<f32> {
        None
    }
}

/// `sin² 1.5°`, pbrt-v4's threshold. Below it a cone's `1 − cos θ_max` is
/// taken as `sin² θ_max / (1 + cos θ_max)` instead of by the subtraction, which
/// in f32 cancels to nothing for a small, distant sphere, and a direction in it
/// is drawn the same cancellation-free way (see [`SubtendedCone::sample`]).
const SMALL_CONE_SIN2: f32 = 0.000_685_23;

/// The cone a sphere subtends from a point outside it.
#[derive(Clone, Copy, Debug)]
struct SubtendedCone {
    /// `sin² θ_max = r² / d²`.
    sin2_max: f32,
    cos_max: f32,
    /// `1 − cos θ_max`, cancellation-free (see [`SMALL_CONE_SIN2`]).
    one_minus_cos_max: f32,
}

impl SubtendedCone {
    /// `None` from inside the sphere (or on it), where there is no cone and
    /// every direction reaches the surface — and for a cone too thin to
    /// represent: a zero radius, `r²/d²` underflowing, or a solid angle so
    /// small that its pdf overflows. Those fall back to area sampling, and
    /// since both [`LightShape`] hooks construct the cone here, they fall back
    /// together.
    fn new(center: Vec3A, radius: f32, from: Vec3A) -> Option<Self> {
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
        let pdf = cone.pdf();
        (pdf.is_finite() && pdf > 0.0).then_some(cone)
    }

    /// Uniform over the cone: `1 / (2π (1 − cos θ_max))`.
    fn pdf(&self) -> f32 {
        1.0 / (2.0 * PI * self.one_minus_cos_max)
    }

    /// A direction uniform over the cone, as `(sin² θ, cos θ)` of its angle θ
    /// off the axis: `1 − cos θ = u (1 − cos θ_max)`, which is what makes it
    /// uniform in solid angle. The small-cone branch keeps that exact rather
    /// than approximating it — pbrt-v4 draws `sin² θ = u sin² θ_max` there,
    /// whose density is proportional to `cos θ` and so disagrees with
    /// [`SubtendedCone::pdf`] by up to `sin² θ_max / 4` — and takes `sin² θ` as
    /// `t (2 − t)` rather than `1 − cos² θ`, which would cancel.
    fn sample(&self, u: f32) -> (f32, f32) {
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
pub struct SphereShape {
    pub center: Vec3A,
    pub radius: f32,
}

impl LightShape for SphereShape {
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

    fn sample_solid_angle(&self, from: Vec3A, u: f32, v: f32) -> Option<(Vec3A, f32)> {
        sample_sphere_cone(self.center, self.radius, from, u, v)
    }

    fn solid_angle_pdf(&self, from: Vec3A, _p: Vec3A) -> Option<f32> {
        SubtendedCone::new(self.center, self.radius, from).map(|cone| cone.pdf())
    }
}

/// A point on a sphere, uniform over the cone it subtends from `from`, and
/// that cone's (constant) solid-angle pdf. `None` from inside the sphere.
/// Shared by [`SphereShape`] and, in its local space, by an [`AffineShape`]
/// sphere.
fn sample_sphere_cone(
    center: Vec3A,
    radius: f32,
    from: Vec3A,
    u: f32,
    v: f32,
) -> Option<(Vec3A, f32)> {
    let cone = SubtendedCone::new(center, radius, from)?;
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
    Some((center + radius * n, cone.pdf()))
}

/// Rectangular light surface (UsdLux `RectLight`): the parallelogram
/// `origin + u·edge_u + v·edge_v`, emitting from the side its `normal`
/// faces. Per the UsdLux convention the importer orients the normal along
/// the light's local -Z.
///
/// Seen from its emitting side, a true rectangle is sampled uniformly over
/// the **solid angle it subtends** (Ureña, Fajardo & King 2013, "An
/// Area-Preserving Parametrization for Spherical Rectangles"; pbrt-v4's
/// `SampleSphericalRectangle`, Cycles' `area.h`), not by area: by area, a
/// panel large or near relative to its distance weights each sample by a
/// `cos θ_l / r²` that varies by orders of magnitude across it. Area
/// sampling remains the fallback wherever the map does not apply or does not
/// pay — see [`RectShape::spherical_rect`].
pub struct RectShape {
    pub origin: Vec3A,
    pub edge_u: Vec3A,
    pub edge_v: Vec3A,
    pub normal: Vec3A,
    /// The orthonormal frame the spherical-rectangle map works in, `None` for
    /// a parallelogram that is not a rectangle (a sheared light).
    frame: Option<RectFrame>,
}

/// A rectangle's own frame, in f64: the solid angle it subtends is a sum of
/// four angles less `2π`, which cancels in f32 long before the light is
/// small enough for area sampling to take over.
#[derive(Clone, Copy, Debug)]
struct RectFrame {
    origin: DVec3,
    x: DVec3,
    y: DVec3,
    z: DVec3,
    /// Edge lengths along `x` and `y`.
    width: f64,
    height: f64,
    /// The unit normal the light emits along, which `z` is `±`.
    normal: DVec3,
}

/// Below this solid angle (sr) a rectangle is area-sampled: `cos θ_l / r²`
/// is nearly constant across it, so area sampling is already close to
/// optimal and cheaper. pbrt-v4's `BilinearPatch::MinSphericalSampleArea`.
const MIN_SPHERICAL_RECT_SR: f64 = 1e-4;

/// Above this (sr) — a shading point almost on the light's plane, where the
/// rectangle fills nearly a hemisphere — the map's `sin(a_u)` divisions
/// degenerate, and the rectangle is area-sampled. pbrt-v4's
/// `MaxSphericalSampleArea`.
const MAX_SPHERICAL_RECT_SR: f64 = 6.22;

/// How far from perpendicular a rectangle's edges may be, as the cosine of
/// the angle between them, before it is treated as a sheared parallelogram.
/// The same tolerance the importer uses to decide whether a light's
/// transformed −Z is still perpendicular to it.
const RECT_ORTHOGONALITY: f32 = 1e-5;

impl RectShape {
    pub fn new(origin: Vec3A, edge_u: Vec3A, edge_v: Vec3A, normal: Vec3A) -> Self {
        let normal = normal.normalize();
        let (lu, lv) = (edge_u.length(), edge_v.length());
        let rectangle = lu > 0.0
            && lv > 0.0
            && (lu * lv).is_finite()
            && edge_u.dot(edge_v).abs() <= RECT_ORTHOGONALITY * lu * lv;
        let frame = rectangle.then(|| {
            let x = DVec3::from(Vec3::from(edge_u)).normalize();
            // Gram-Schmidt, so the frame is orthonormal to f64 precision even
            // though the edges are only perpendicular to f32's.
            let v = DVec3::from(Vec3::from(edge_v));
            let y = (v - v.dot(x) * x).normalize();
            RectFrame {
                origin: DVec3::from(Vec3::from(origin)),
                x,
                y,
                z: x.cross(y),
                width: lu as f64,
                height: lv as f64,
                normal: DVec3::from(Vec3::from(normal)),
            }
        });
        Self {
            origin,
            edge_u,
            edge_v,
            normal,
            frame,
        }
    }

    /// The spherical rectangle this light projects to from `from`, or `None`
    /// where it is area-sampled instead: a sheared parallelogram (the map
    /// needs a true rectangle); a shading point on or behind the emitting
    /// side, from which a one-sided light emits nothing anyway; and a solid
    /// angle outside `[MIN_SPHERICAL_RECT_SR, MAX_SPHERICAL_RECT_SR]`. Both
    /// [`LightShape`] hooks construct it here, which is what makes them
    /// answer for exactly the same `from`s.
    fn spherical_rect(&self, from: Vec3A) -> Option<SphericalRect> {
        let frame = self.frame.as_ref()?;
        let d = frame.origin - DVec3::from(Vec3::from(from));
        // Strictly in front of the emitting side.
        if d.dot(frame.normal) >= 0.0 {
            return None;
        }
        let rect = SphericalRect::new(frame, d)?;
        (MIN_SPHERICAL_RECT_SR..=MAX_SPHERICAL_RECT_SR)
            .contains(&rect.solid_angle)
            .then_some(rect)
    }
}

/// A rectangle as seen from a shading point, in the rectangle's frame with
/// the shading point at the origin (Ureña et al. 2013, §3): the rectangle
/// spans `[x0, x1] × [y0, y1]` in the plane `z = −h`, `h > 0`.
#[derive(Clone, Copy, Debug)]
struct SphericalRect {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    h: f64,
    /// `z` components of the inner normals of the planes through the
    /// shading point and the edges `y = y0` and `y = y1`.
    b0: f64,
    b1: f64,
    /// `(cos, sin)` of `g2 + g3`, the quad's internal angles at its two
    /// `x = x0` corners — all the sampler needs of that sum, and what the
    /// product of their complex numbers is once normalised.
    g23: DVec2,
    /// `g0 + g1 + g2 + g3 − 2π`, the solid angle it subtends.
    solid_angle: f64,
}

impl SphericalRect {
    /// `d` is the rectangle's origin corner relative to the shading point.
    ///
    /// The paper builds the four planes through the shading point and each
    /// edge, normalises their normals and takes the angle between each
    /// adjacent pair. In this frame those normals are axis-aligned in closed
    /// form — the plane through the edge `y = y0` has normal `∝ (0, −h, −y0)`,
    /// and so on around — so the internal angle at a corner `(x, y)` comes out
    /// as `atan2(h·|v|, ±x·y)`, `|v|` the distance to that corner. Only sums of
    /// the angles are ever used, and a sum of arguments is the argument of a
    /// product. The solid angle is one `atan2` of the product of all four
    /// complex numbers, and the sampler needs only the cosine and sine of
    /// `g2 + g3`, which the normalised product of two *is* — so one `atan2` in
    /// all, and no `asin`. The
    /// product's imaginary part is where a small solid angle lives, and f64
    /// keeps it: a rectangle's `Σg − 2π` cancels in f32 at the sizes area
    /// sampling hands over at.
    fn new(frame: &RectFrame, d: DVec3) -> Option<Self> {
        let x0 = d.dot(frame.x);
        let y0 = d.dot(frame.y);
        // The paper's frame puts the rectangle below the shading point; the
        // mirror in z changes no angle or area.
        let h = d.dot(frame.z).abs();
        if h <= 0.0 {
            return None;
        }
        let (x1, y1) = (x0 + frame.width, y0 + frame.height);
        let (h2, x02, x12, y02, y12) = (h * h, x0 * x0, x1 * x1, y0 * y0, y1 * y1);
        // `(cos, sin)` of each internal angle, both scaled by the same
        // positive factor.
        let corner = |cos: f64, r2: f64| DVec2::new(cos, h * r2.sqrt());
        let g0 = corner(x1 * y0, x12 + y02 + h2);
        let g1 = corner(-x1 * y1, x12 + y12 + h2);
        let g2 = corner(x0 * y1, x02 + y12 + h2);
        let g3 = corner(-x0 * y0, x02 + y02 + h2);
        let mul = |a: DVec2, b: DVec2| DVec2::new(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
        // Each angle is in (0, π), so a sum of two is in (0, 2π); and the
        // four sum to 2π plus a solid angle in (0, 2π). Both are the argument
        // of a product taken in [0, 2π).
        let arg = |z: DVec2| {
            let a = z.y.atan2(z.x);
            if a < 0.0 {
                a + 2.0 * std::f64::consts::PI
            } else {
                a
            }
        };
        let g23 = mul(g2, g3);
        let solid_angle = arg(mul(mul(g0, g1), g23));
        let g23 = g23 / g23.length();
        (solid_angle.is_finite() && g23.is_finite()).then_some(Self {
            x0,
            x1,
            y0,
            y1,
            h,
            b0: -y0 / (h2 + y02).sqrt(),
            b1: y1 / (h2 + y12).sqrt(),
            g23,
            solid_angle,
        })
    }

    fn pdf(&self) -> f32 {
        (1.0 / self.solid_angle) as f32
    }

    /// The rectangle's own `(s, t) ∈ [0, 1]²` of a point uniform in the solid
    /// angle it subtends. Area-preserving from `(u, v)`, so the sampler's
    /// stratification carries onto the sphere of directions.
    fn sample(&self, u: f64, v: f64) -> (f64, f64) {
        // The azimuthal slice holding a fraction `u` of the solid angle (the
        // paper's eq. 7-9, whose `u (g0 + g1 − 2π) + (u − 1)(g2 + g3)` is
        // `u Ω − (g2 + g3)`), by the angle-difference identities.
        let (sin_uo, cos_uo) = (u * self.solid_angle).sin_cos();
        let cos_au = cos_uo * self.g23.x + sin_uo * self.g23.y;
        let sin_au = sin_uo * self.g23.x - cos_uo * self.g23.y;
        let fu = (cos_au * self.b0 - self.b1) / sin_au;
        // `fu` is ±∞ where `sin(a_u)` is zero, which lands `cu` on 0 — the
        // right limit. A 0/0 there is the only NaN; it has the same limit.
        let mut cu = (1.0 / (fu * fu + self.b0 * self.b0).sqrt()).copysign(fu);
        if cu.is_nan() {
            cu = 0.0;
        }
        let cu = cu.clamp(-1.0 + f64::EPSILON, 1.0 - f64::EPSILON);
        let xu = (cu * self.h / (1.0 - cu * cu).sqrt())
            .max(self.x0)
            .min(self.x1);
        // Then the elevation within it, uniform in `h = cos` of the angle to
        // the y axis (eq. 10-11).
        let dd2 = xu * xu + self.h * self.h;
        let h0 = self.y0 / (dd2 + self.y0 * self.y0).sqrt();
        let h1 = self.y1 / (dd2 + self.y1 * self.y1).sqrt();
        let hv = h0 + v * (h1 - h0);
        let hv2 = hv * hv;
        let yv = if hv2 < 1.0 - 1e-12 {
            hv * (dd2 / (1.0 - hv2)).sqrt()
        } else {
            self.y1
        };
        let yv = yv.max(self.y0).min(self.y1);
        let s = (xu - self.x0) / (self.x1 - self.x0);
        let t = (yv - self.y0) / (self.y1 - self.y0);
        (s.clamp(0.0, 1.0), t.clamp(0.0, 1.0))
    }
}

impl LightShape for RectShape {
    fn sample_point(&self, u: f32, v: f32) -> Vec3A {
        self.origin + u * self.edge_u + v * self.edge_v
    }

    fn normal_at(&self, _p: Vec3A) -> Vec3A {
        self.normal
    }

    fn area(&self) -> f32 {
        self.edge_u.cross(self.edge_v).length()
    }

    /// The point is returned through the rectangle's own `(s, t)` rather than
    /// the map's local coordinates, so it lies on the light exactly as an
    /// area sample does — on the triangles a bounce ray hits, and at the
    /// texel a textured card looks up.
    fn sample_solid_angle(&self, from: Vec3A, u: f32, v: f32) -> Option<(Vec3A, f32)> {
        let rect = self.spherical_rect(from)?;
        let (s, t) = rect.sample(u as f64, v as f64);
        Some((self.sample_point(s as f32, t as f32), rect.pdf()))
    }

    fn solid_angle_pdf(&self, from: Vec3A, _p: Vec3A) -> Option<f32> {
        self.spherical_rect(from).map(|rect| rect.pdf())
    }
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
    fn local_area(self) -> f32 {
        match self {
            UnitShape::Sphere => 4.0 * PI,
            UnitShape::Disk => PI,
            UnitShape::Cylinder => 2.0 * PI,
        }
    }

    /// A point uniform by *local* area.
    fn sample(self, u: f32, v: f32) -> Vec3A {
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
    fn normal(self, p: Vec3A) -> Vec3A {
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
/// (see [`LightShape::sample_solid_angle`] below). Otherwise — a disk, a tube,
/// or a point inside the sphere — sampling is uniform in the shape's *local*
/// area and mapped through the placement, so in world space it is denser
/// where the placement compresses the surface. That is fine — MIS needs a density both sides agree on, not a
/// uniform one — and [`LightShape::inv_pdf_area`] reports it exactly: an
/// affine map `M` scales the area element at a point with unit local normal
/// `n` by `|det M| · |M⁻ᵀ n|`. For a disk that factor is constant, so the
/// sampling is uniform after all.
pub struct AffineShape {
    unit: UnitShape,
    light_to_world: Affine3A,
    world_to_light: Affine3A,
    /// `M⁻ᵀ`, the normal transform.
    normal_mat: Mat3A,
    abs_det: f32,
    area: f32,
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
    fn area_scale(&self, n: Vec3A) -> f32 {
        self.abs_det * (self.normal_mat * n).length()
    }

    /// World-space area, integrating the area scale over the local surface.
    /// Exact for the disk (the scale is constant) and for any similarity;
    /// under a non-uniform scale the sphere's and tube's scale varies over
    /// the surface and a midpoint rule over a fine grid is well inside f32
    /// precision — done once, at import. Replaces hdEmbree's closed-form
    /// approximations (Knud Thomsen's ellipsoid, Ramanujan's ellipse
    /// perimeter), which are close but not what the spec asks for.
    fn integrate_area(&self) -> f32 {
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

    fn inv_pdf_area(&self, p: Vec3A) -> f32 {
        let local = self.world_to_light.transform_point3a(p);
        self.unit.local_area() * self.area_scale(self.unit.normal(local))
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
    fn sample_solid_angle(&self, from: Vec3A, u: f32, v: f32) -> Option<(Vec3A, f32)> {
        if self.unit != UnitShape::Sphere {
            return None;
        }
        let from_local = self.world_to_light.transform_point3a(from);
        let (p_local, local_pdf) = sample_sphere_cone(Vec3A::ZERO, 1.0, from_local, u, v)?;
        Some((
            self.light_to_world.transform_point3a(p_local),
            self.world_solid_angle_pdf(local_pdf, p_local - from_local),
        ))
    }

    fn solid_angle_pdf(&self, from: Vec3A, p: Vec3A) -> Option<f32> {
        if self.unit != UnitShape::Sphere {
            return None;
        }
        let from_local = self.world_to_light.transform_point3a(from);
        let cone = SubtendedCone::new(Vec3A::ZERO, 1.0, from_local)?;
        let p_local = self.world_to_light.transform_point3a(p);
        Some(self.world_solid_angle_pdf(cone.pdf(), p_local - from_local))
    }
}

impl AffineShape {
    /// A local solid-angle density as a world one, for the direction
    /// `to_local` (from the shading point toward the surface, in local space).
    /// The placement sends a local direction `ω` to `Mω / |Mω|`, which scales
    /// solid angle by `|det M| / |Mω|³`; a density scales by the reciprocal.
    fn world_solid_angle_pdf(&self, local_pdf: f32, to_local: Vec3A) -> f32 {
        let stretch = (self.light_to_world.matrix3 * to_local.normalize()).length();
        local_pdf * stretch * stretch * stretch / self.abs_det
    }
}

/// One sampled connection from a shading point to a light: where to aim
/// the shadow ray, how far it must reach, the radiance arriving from that
/// direction, and the solid-angle density of having chosen it.
///
/// Directions rather than points, because a light can be at infinity — a
/// `DistantLight` or a `DomeLight` has no surface point to aim at.
#[derive(Clone, Copy, Debug)]
pub struct LightSample {
    /// Unit direction from the shading point toward the light.
    pub direction: Vec3A,
    /// How far the shadow ray must be traced. `f32::INFINITY` for lights
    /// at infinity — nothing beyond the scene can occlude them.
    pub distance: f32,
    /// Radiance arriving along `direction`.
    pub radiance: Vec3A,
    /// Solid-angle pdf of this direction under the light's own sampling.
    ///
    /// Always finite and positive: crust has no delta lights. A
    /// `DistantLight` with a zero `angle` is widened to a small but real
    /// cone rather than being made singular, which keeps one MIS path
    /// through the integrator instead of two.
    pub pdf: f32,
}

/// The `Light` trait is what the integrator's light-sampling strategy (NEE)
/// needs from a light: a direction to aim a shadow ray, the solid-angle
/// density of that choice for MIS, the radiance it carries, and — for
/// lights with scene geometry — the geometry id that lets a bounce ray
/// recognize the light it hit.
///
/// The MIS pairing is the thing to be careful with. Every light has two
/// ways of being found: NEE samples it directly, and a bounce ray may
/// arrive at it by chance. Both sides must evaluate the *same* density or
/// emission is double-counted. For lights with geometry that second path
/// is a bounce hit, weighted with [`Light::pdf_at_point`]; for lights at
/// infinity it is a ray escaping the scene, weighted with
/// [`Light::escaped`]. A light implements whichever applies.
pub trait Light: Send + Sync {
    /// Samples a direction from `from` toward the light. `None` when the
    /// light cannot be reached from there (below a dome's horizon, say).
    ///
    /// # Parameters
    /// - `u`, `v`: Unit random numbers driving the sample.
    fn sample_li(&self, from: Vec3A, u: f32, v: f32) -> Option<LightSample>;

    /// Solid-angle pdf, as seen from `from`, of [`Light::sample_li`] having
    /// produced `light_point` — the bounce side of MIS for a light whose
    /// geometry a ray hit. Lights at infinity have no such point and keep
    /// the default.
    fn pdf_at_point(&self, _from: Vec3A, _light_point: Vec3A) -> f32 {
        0.0
    }

    /// For a ray that escaped the scene along `direction`: the radiance it
    /// picks up and the solid-angle pdf NEE would have used for that
    /// direction, as `(radiance, pdf)`. This is the bounce side of MIS for
    /// lights at infinity. `None` for lights with finite geometry, and for
    /// directions this light does not cover.
    fn escaped(&self, _from: Vec3A, _direction: Vec3A) -> Option<(Vec3A, f32)> {
        None
    }

    /// The `geom_id` of this light's scene geometry in the world, used to
    /// recognize the light when a bounce ray hits it. `None` for lights
    /// with no geometry in the world.
    fn geom_id(&self) -> Option<u32> {
        None
    }

    /// The light's emitted power as a luminance flux — what
    /// [`LightSelection::Power`] divides shadow rays by. It is a sampling
    /// weight, never a shading quantity, so it must be proportionate across
    /// lights and positive wherever the light emits, not exact.
    ///
    /// `None` for a light at infinity, which has no finite power to compare.
    /// pbrt-v4's `PowerLightSampler` gives one the flux it sends into the
    /// scene's bounding sphere; measured here, that let a sun take 88% of the
    /// shadow rays from its dome and made `samples/domelight.usda` 1.4× noisier,
    /// because in the sun's shadows the dome is the only light. So power
    /// selection gives lights at infinity a fixed share instead, as pbrt-v4's
    /// BVH sampler and Karma do.
    fn power(&self) -> Option<f32>;
}

/// A geometric area light: any [`LightShape`] paired with the [`Emissive`]
/// material its scene geometry carries (Cornell-box semantics — the same
/// surface is both light and visible object).
pub struct AreaLight {
    shape: Box<dyn LightShape>,
    material: Arc<Emissive>,
    /// The world `geom_id` of the emissive geometry this light shares its
    /// surface with — how bounce hits are attributed back to the light.
    geom_id: u32,
}

impl AreaLight {
    pub fn new(shape: Box<dyn LightShape>, material: Arc<Emissive>, geom_id: u32) -> Self {
        Self {
            shape,
            material,
            geom_id,
        }
    }

    /// Solid-angle pdf, as seen from `from`, of the strategy
    /// [`Light::sample_li`] used to reach `light_point`: the shape's own
    /// solid-angle density where it has one, otherwise that of sampling
    /// uniformly by area, `dist² / (cos(θ_light) · area)`, where θ_light is the
    /// angle between the light's surface normal at `light_point` and the
    /// direction back toward the shaded point. Back-facing points clamp the
    /// cosine to zero, so their pdf explodes and both MIS strategies agree
    /// the contribution is negligible — area lights are effectively
    /// one-sided.
    fn solid_angle_pdf(&self, from: Vec3A, light_point: Vec3A) -> f32 {
        if let Some(pdf) = self.shape.solid_angle_pdf(from, light_point) {
            return pdf;
        }
        let direction = light_point - from;
        let dir_to_light = direction.normalize();
        let light_normal = self.shape.normal_at(light_point);
        self.pdf_toward(direction, dir_to_light, light_normal, light_point)
    }

    /// [`AreaLight::solid_angle_pdf`] with the normal already in hand.
    fn pdf_toward(
        &self,
        direction: Vec3A,
        dir_to_light: Vec3A,
        light_normal: Vec3A,
        light_point: Vec3A,
    ) -> f32 {
        let distance_squared = direction.length_squared();
        let cosine = f32::max(light_normal.dot(-dir_to_light), 0.0);
        distance_squared / (cosine * self.shape.inv_pdf_area(light_point) + 1e-4)
    }
}

impl Light for AreaLight {
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
        // emitting side is the same cosine the pdf clamps.
        let front = light_normal.dot(-dir_to_light) > 0.0;
        Some(LightSample {
            direction,
            distance,
            radiance: self
                .material
                .radiance_toward(light_point, -dir_to_light, front),
            pdf: solid_angle.map_or_else(
                || self.pdf_toward(to_light, dir_to_light, light_normal, light_point),
                |(_, pdf)| pdf,
            ),
        })
    }

    fn pdf_at_point(&self, from: Vec3A, light_point: Vec3A) -> f32 {
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
                (self.shape.normal_at(p), self.shape.inv_pdf_area(p))
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

/// A `UsdLuxDistantLight`: parallel light from infinitely far away, as the
/// sun is.
///
/// Two conventions worth stating, because renderers differ.
///
/// **The cone is always real.** UsdLux gives the source an angular diameter
/// (`inputs:angle`, default 0.53° — the sun's), and authors may set it to
/// zero for perfectly sharp shadows. Rather than making that a delta light,
/// which would need a second MIS path through the integrator, a zero angle
/// is widened to [`MIN_DISTANT_ANGLE_DEG`]. The resulting penumbra is far
/// below a pixel at any sane scene scale, and MIS handles the rest: when a
/// bounce ray happens into the tiny cone the light pdf is enormous, so the
/// bounce side's weight collapses to nothing and no firefly survives.
///
/// **The light stores radiance, and there are two ways to ask for it.**
/// UsdLux says `intensity` is the source's *luminance* in nits
/// ([`DistantLight::with_radiance`]); with `inputs:normalize` it divides by
/// `π·sin²θ`, which makes `intensity` the *illuminance* on a surface facing
/// the light ([`DistantLight::new`]). The importer decides which — see
/// `emit_distant_light` — and also owns what widening a zero angle means for
/// each: a zero-angle light is a delta, whose `intensity` the spec and
/// hdEmbree both deliver as irradiance, so it goes through `new` too.
pub struct DistantLight {
    /// Unit direction the light travels *toward* (the direction photons
    /// move), so a shading point is lit from `-direction`.
    direction: Vec3A,
    /// Radiance inside the cone.
    radiance: Vec3A,
    /// Half-angle of the source cone, in radians.
    cos_half_angle: f32,
    /// Solid angle of the cone, `2π(1 − cos θ)`.
    solid_angle: f32,
}

/// The floor a `DistantLight`'s angular diameter is clamped to, in degrees.
/// Small enough to read as a sharp shadow, large enough that the cone stays
/// a genuine solid angle with a finite pdf.
pub const MIN_DISTANT_ANGLE_DEG: f32 = 0.05;

impl DistantLight {
    /// A distant light delivering `irradiance` to a surface facing it,
    /// however wide the cone. `direction` is the direction the light travels
    /// toward (UsdLux's convention: a distant light points down its local
    /// -Z). `angle_deg` is the source's angular *diameter*, as `inputs:angle`
    /// gives it.
    ///
    /// The radiance is `E / (π·sin²θ)` — the *cosine-weighted* solid angle,
    /// which is what makes `E` exact on the facing surface — not `E / Ω`,
    /// which undershoots by `cos²(θ/2)`: 1.7% at a 30° diameter, nothing at
    /// the sun's.
    pub fn new(direction: Vec3A, irradiance: Vec3A, angle_deg: f32) -> Self {
        let half = 0.5 * Self::clamp_diameter(angle_deg).to_radians();
        Self::with_radiance(
            direction,
            irradiance / projected_cone_solid_angle(half).max(1e-12),
            angle_deg,
        )
    }

    /// A distant light of the given radiance (nits) inside its cone.
    pub fn with_radiance(direction: Vec3A, radiance: Vec3A, angle_deg: f32) -> Self {
        let half_angle = 0.5 * Self::clamp_diameter(angle_deg).to_radians();
        let cos_half_angle = half_angle.cos();
        Self {
            direction: direction.normalize(),
            radiance,
            cos_half_angle,
            solid_angle: 2.0 * std::f32::consts::PI * (1.0 - cos_half_angle),
        }
    }

    /// The angular diameter actually used: the widening floor and a ceiling
    /// short of a full hemisphere.
    pub fn clamp_diameter(angle_deg: f32) -> f32 {
        angle_deg.clamp(MIN_DISTANT_ANGLE_DEG, 179.0)
    }

    /// Radiance within the cone.
    fn radiance(&self) -> Vec3A {
        self.radiance
    }

    /// Uniform-cone pdf, constant inside the cone.
    fn cone_pdf(&self) -> f32 {
        1.0 / self.solid_angle.max(1e-12)
    }

    /// Is `direction` (pointing away from the shaded point) inside the
    /// cone of directions this light occupies?
    fn covers(&self, direction: Vec3A) -> bool {
        direction.dot(-self.direction) >= self.cos_half_angle
    }
}

impl Light for DistantLight {
    fn sample_li(&self, _from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        // Uniform direction within the cone around `-direction`.
        let cos_theta = 1.0 - u * (1.0 - self.cos_half_angle);
        let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
        let phi = 2.0 * std::f32::consts::PI * v;
        let local = Vec3A::new(sin_theta * phi.cos(), sin_theta * phi.sin(), cos_theta);
        Some(LightSample {
            direction: utils::align_to_normal(local, -self.direction).normalize(),
            // Nothing beyond the scene can occlude a light at infinity.
            distance: f32::INFINITY,
            radiance: self.radiance(),
            pdf: self.cone_pdf(),
        })
    }

    fn escaped(&self, _from: Vec3A, direction: Vec3A) -> Option<(Vec3A, f32)> {
        self.covers(direction)
            .then(|| (self.radiance(), self.cone_pdf()))
    }

    /// At infinity: no finite power (see [`Light::power`]).
    fn power(&self) -> Option<f32> {
        None
    }
}

/// The cosine-weighted solid angle of a cone of half-angle `half` (≤ π/2)
/// seen along its axis, `π·sin²θ`: the irradiance unit radiance inside it
/// delivers to a surface facing it.
///
/// Evaluated as `π·(1 − c)(1 + c)` from the f32 cosine `c` — the same
/// cosine [`DistantLight`] bounds its cone with — rather than from `sin θ`.
/// The two agree mathematically, but the cone crust actually samples and
/// tests against is the one the *rounded* cosine bounds, and for a sun-sized
/// cone that rounding is 2.4e-4 of the solid angle: dividing by `π·sin²θ`
/// would deliver the authored illuminance to that accuracy, dividing by this
/// delivers it exactly. (`1 − c` is itself exact — Sterbenz — so this is
/// also free of the cancellation that made `2π(1 − cos θ)` computed the
/// obvious way in f32 wrong by the same 2.4e-4.)
pub fn projected_cone_solid_angle(half: f32) -> f32 {
    let c = half.clamp(0.0, 0.5 * PI).cos();
    PI * (1.0 - c) * (1.0 + c)
}

/// A `UsdLuxDomeLight`: an infinite environment surrounding the scene.
///
/// Covers every direction, so once one exists it *is* the background — the
/// integrator's built-in sky gradient stops applying, because
/// [`Light::escaped`] answers for every ray that leaves.
///
/// Radiance is a uniform `tint` multiplied by an optional lat-long
/// [`EnvironmentMap`]. With a map, directions are importance-sampled from
/// its luminance so a small bright sun in an HDRI does not become a firefly
/// farm; without one, directions are sampled uniformly over the sphere.
///
/// `orientation` maps *world* directions into the dome's own space, so a
/// rotated dome prim rotates the sky. It is the inverse of the prim's
/// world transform, cached once.
pub struct DomeLight {
    tint: Vec3A,
    map: Option<Arc<EnvironmentMap>>,
    /// World → dome-local rotation.
    world_to_light: Mat3A,
    /// Dome-local → world rotation.
    light_to_world: Mat3A,
}

impl DomeLight {
    pub fn new(tint: Vec3A, map: Option<Arc<EnvironmentMap>>, light_to_world: Mat3A) -> Self {
        Self {
            tint,
            map,
            world_to_light: light_to_world.inverse(),
            light_to_world,
        }
    }

    /// Radiance arriving from a world-space `direction`.
    fn radiance_toward(&self, direction: Vec3A) -> Vec3A {
        match &self.map {
            Some(map) => self.tint * map.radiance(self.world_to_light * direction),
            None => self.tint,
        }
    }

    /// Solid-angle pdf of a world-space `direction` under this dome's own
    /// sampling: the map's distribution, or uniform over the sphere.
    fn pdf_toward(&self, direction: Vec3A) -> f32 {
        match &self.map {
            Some(map) => map.pdf(self.world_to_light * direction),
            None => 1.0 / (4.0 * std::f32::consts::PI),
        }
    }
}

impl Light for DomeLight {
    fn sample_li(&self, _from: Vec3A, u: f32, v: f32) -> Option<LightSample> {
        let (direction, radiance, pdf) = match &self.map {
            Some(map) => {
                let (local, radiance, pdf) = map.sample(u, v)?;
                ((self.light_to_world * local).normalize(), radiance, pdf)
            }
            None => {
                // Uniform over the sphere.
                let z = 1.0 - 2.0 * u;
                let r = (1.0 - z * z).max(0.0).sqrt();
                let phi = std::f32::consts::TAU * v;
                (
                    Vec3A::new(r * phi.cos(), z, r * phi.sin()),
                    Vec3A::ONE,
                    1.0 / (4.0 * std::f32::consts::PI),
                )
            }
        };
        (pdf > 0.0).then(|| LightSample {
            direction,
            // Nothing in the scene can occlude the environment beyond it.
            distance: f32::INFINITY,
            radiance: self.tint * radiance,
            pdf,
        })
    }

    fn escaped(&self, _from: Vec3A, direction: Vec3A) -> Option<(Vec3A, f32)> {
        // A dome covers every direction, so every escaping ray finds it.
        Some((self.radiance_toward(direction), self.pdf_toward(direction)))
    }

    /// At infinity: no finite power (see [`Light::power`]).
    fn power(&self) -> Option<f32> {
        None
    }
}

/// How NEE chooses which light to sample at a vertex (`crust:lightSelection`,
/// `--light-selection`). Measured in `docs/light_sampling.md` §3.8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightSelection {
    /// One in N, whatever the lights emit: the renderer's behaviour before
    /// selection was a choice, reproduced bit for bit (see
    /// [`LightList::density`]), and the A/B for [`LightSelection::Power`].
    Uniform,
    /// The default: by power (Shirley et al. 1996; pbrt-v4's
    /// `PowerLightSampler`), made defensive. Lights at infinity keep their
    /// uniform share, since they have no comparable power ([`Light::power`]).
    /// The finite lights split the rest [`DEFENSIVE_SHARE`] evenly and the
    /// remainder in proportion to power, so no light falls below half its
    /// uniform share — power is blind to distance and visibility, and the
    /// even half is what bounds the cost where that blindness is wrong. It
    /// pays off when a few lights outshine many that light the same things
    /// (4.6× lower relMSE on a key among seven dim fills) and costs a few
    /// percent where each light owns its own region (6% on `veach_mis`).
    #[default]
    Power,
}

/// Under [`LightSelection::Power`], the share of the finite lights' shadow
/// rays split evenly among them rather than by power (Hesterberg's
/// defensive importance sampling).
pub const DEFENSIVE_SHARE: f64 = 0.5;

/// The scene's lights, and how NEE picks one of them.
///
/// The pick's probability is half of the light strategy's MIS density (the
/// other half is the light's own `sample_li` pdf), so whatever
/// [`LightList::pick`] reports, [`LightList::find_by_geom`] and
/// [`LightList::iter`] report the same number for the same light: the bounce
/// side weights emission it found by chance with it, and the two sides must
/// describe one strategy or emission is double-counted.
pub struct LightList {
    /// The lights. Private, so that [`LightList::add`] is the only way in:
    /// it keeps the geometry index and the selection in step with this
    /// vector, and a light pushed past it would be sampled by NEE yet
    /// unattributed on the bounce side. Read it through
    /// [`LightList::lights`].
    lights: Vec<Arc<dyn Light>>,
    /// Per-light selection probability, empty while the selection is
    /// uniform — [`LightList::select_by`] fills it.
    pmf: Vec<f32>,
    /// Inclusive running sum of `pmf`, ending at exactly 1.
    cdf: Vec<f32>,
    /// `geom_id → index into lights`, so a bounce hit finds its light in O(1)
    /// rather than by scanning the list on every emissive hit.
    by_geom: HashMap<u32, usize>,
}

impl Default for LightList {
    /// Creates a new, empty `LightList` as the default implementation.
    fn default() -> Self {
        Self::new()
    }
}

impl LightList {
    /// Creates a new, empty `LightList`, selecting uniformly until
    /// [`LightList::select_by`] says otherwise.
    pub fn new() -> Self {
        Self {
            lights: Vec::new(),
            pmf: Vec::new(),
            cdf: Vec::new(),
            by_geom: HashMap::new(),
        }
    }

    /// Adds a light source. The selection falls back to uniform until the
    /// next [`LightList::select_by`], since a table built over the old list
    /// would describe the wrong one.
    pub fn add(&mut self, light: Arc<dyn Light>) {
        if let Some(id) = light.geom_id() {
            self.by_geom.insert(id, self.lights.len());
        }
        self.lights.push(light);
        self.pmf.clear();
        self.cdf.clear();
    }

    /// Builds the selection over the current lights (see [`LightSelection`]).
    ///
    /// A finite light whose power comes out non-finite or non-positive — a
    /// black one — gets probability zero, so NEE never spends a ray on it,
    /// and the bounce side, seeing the same zero, keeps its emission at full
    /// weight, so nothing is lost. With nothing left to pick from, the
    /// selection stays uniform.
    ///
    /// The table is inverted by its CDF rather than an alias table, and that
    /// is deliberate: the map from `u` to light stays monotone, so the
    /// stratified samples that pick light *k* are still one contiguous slice
    /// of the pick dimension, as under uniform picking.
    pub fn select_by(&mut self, selection: LightSelection) {
        self.pmf.clear();
        self.cdf.clear();
        if selection == LightSelection::Uniform || self.lights.is_empty() {
            return;
        }
        // `None` at infinity; `Some(0)` for a finite light that emits nothing.
        let powers: Vec<Option<f64>> = self
            .lights
            .iter()
            .map(|l| {
                l.power().map(|p| {
                    let p = p as f64;
                    if p.is_finite() && p > 0.0 { p } else { 0.0 }
                })
            })
            .collect();
        let infinite = powers.iter().filter(|p| p.is_none()).count();
        let lit: Vec<f64> = powers
            .iter()
            .flatten()
            .copied()
            .filter(|&p| p > 0.0)
            .collect();
        let live = infinite + lit.len();
        if live == 0 {
            return;
        }
        let finite_share = lit.len() as f64 / live as f64;
        let lit_total: f64 = lit.iter().sum();
        let weights: Vec<f64> = powers
            .iter()
            .map(|p| match *p {
                None => 1.0 / live as f64,
                Some(p) if p > 0.0 => {
                    finite_share
                        * (DEFENSIVE_SHARE / lit.len() as f64
                            + (1.0 - DEFENSIVE_SHARE) * p / lit_total)
                }
                Some(_) => 0.0,
            })
            .collect();
        let total: f64 = weights.iter().sum();
        let mut running = 0.0f64;
        for (index, w) in weights.into_iter().enumerate() {
            running += w;
            self.pmf.push((w / total) as f32);
            self.cdf.push((running / total) as f32);
            tracing::debug!(
                "light {index} (geom {:?}): power {:?}, picked with probability {:.4}",
                self.lights[index].geom_id(),
                powers[index],
                w / total
            );
        }
        // The last light with any power ends the CDF at exactly one, so no
        // `u` below one can fall past it.
        if let Some(last) = self.pmf.iter().rposition(|&p| p > 0.0) {
            for c in &mut self.cdf[last..] {
                *c = 1.0;
            }
        }
    }

    /// Which strategy [`LightList::pick`] is using.
    pub fn selection(&self) -> LightSelection {
        if self.pmf.is_empty() {
            LightSelection::Uniform
        } else {
            LightSelection::Power
        }
    }

    /// The probability [`LightList::pick`] chooses light `index`.
    pub fn pmf(&self, index: usize) -> f32 {
        match self.pmf.get(index) {
            Some(&p) => p,
            None => 1.0 / self.lights.len() as f32,
        }
    }

    /// The light strategy's solid-angle density for a light chosen with
    /// probability `pmf` (from [`LightList::pick`], [`LightList::find_by_geom`]
    /// or [`LightList::iter`]) whose own `sample_li` density is `light_pdf`:
    /// their product. Both MIS halves route through here, so they cannot
    /// disagree on it. Under uniform selection it is the division
    /// `light_pdf / n` it always was, not a multiplication by `1/n`, which
    /// rounds differently when `n` is not a power of two — so the default
    /// renders bit-identically to the renderer before selection was a choice.
    pub fn density(&self, light_pdf: f32, pmf: f32) -> f32 {
        if self.pmf.is_empty() {
            light_pdf / self.lights.len() as f32
        } else {
            light_pdf * pmf
        }
    }

    /// Picks a light from one `[0, 1)` sample `u`, with the probability it
    /// was picked. `None` only for an empty list.
    pub fn pick(&self, u: f32) -> Option<(&Arc<dyn Light>, f32)> {
        let n = self.lights.len();
        if n == 0 {
            return None;
        }
        let index = if self.cdf.is_empty() {
            // Guard against `u == 1.0 - epsilon` rounding to `n`.
            ((u * n as f32) as usize).min(n - 1)
        } else {
            // The first light whose running sum exceeds `u`; a zero-power
            // light's slice of the CDF is empty, so it is never landed on.
            self.cdf.partition_point(|&c| c <= u).min(n - 1)
        };
        Some((&self.lights[index], self.pmf(index)))
    }

    /// Finds the light whose scene geometry has world id `geom_id`, with its
    /// selection probability. Used by the integrator to attribute a
    /// bounce-hit emissive surface to its light for MIS; emissive geometry
    /// with no light-list entry returns `None`.
    pub fn find_by_geom(&self, geom_id: u32) -> Option<(&Arc<dyn Light>, f32)> {
        let &index = self.by_geom.get(&geom_id)?;
        Some((&self.lights[index], self.pmf(index)))
    }

    /// Every light with its selection probability.
    pub fn iter(&self) -> impl Iterator<Item = (&Arc<dyn Light>, f32)> {
        self.lights
            .iter()
            .enumerate()
            .map(|(index, light)| (light, self.pmf(index)))
    }

    /// The lights, in the order they were added.
    pub fn lights(&self) -> &[Arc<dyn Light>] {
        &self.lights
    }

    /// Returns the number of lights in the `LightList`.
    pub fn count(&self) -> usize {
        self.lights.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The small-cone branch must draw `1 − cos θ = u (1 − cos θ_max)` — what
    /// its constant pdf claims — and not pbrt-v4's `sin² θ = u sin² θ_max`,
    /// which is off by up to `sin² θ_max / 4`: 7.5e-5 at `u = ½` just under
    /// the threshold, far above f32's resolution here.
    #[test]
    fn small_cone_samples_are_uniform_in_solid_angle() {
        // sin² θ_max = 6e-4, just under `SMALL_CONE_SIN2`.
        let cone = SubtendedCone::new(Vec3A::ZERO, 6e-4f32.sqrt(), Vec3A::new(0.0, 0.0, 1.0))
            .expect("outside the sphere");
        assert!(cone.sin2_max < SMALL_CONE_SIN2);
        for u in [0.1f32, 0.25, 0.5, 0.75, 0.9, 1.0] {
            let (sin2, cos) = cone.sample(u);
            // `sin² / (1 + cos)` is `1 − cos θ` without the cancellation.
            let ratio = (sin2 / (1.0 + cos)) / (u * cone.one_minus_cos_max);
            assert!((ratio - 1.0).abs() < 1e-5, "u = {u}: ratio {ratio}");
            assert!((sin2 + cos * cos - 1.0).abs() < 1e-6);
        }
        // Both branches agree on `1 − cos θ_max` across the threshold.
        let exact = 1.0 - (1.0 - 6e-4f64).sqrt();
        assert!(((cone.one_minus_cos_max as f64) / exact - 1.0).abs() < 1e-6);
    }

    /// A cone too thin for its pdf to be finite is no cone: both hooks fall
    /// back to area sampling rather than hand MIS an infinite density, whose
    /// square is `inf / inf = NaN` in the power heuristic.
    #[test]
    fn a_cone_whose_pdf_overflows_is_refused() {
        let shape = SphereShape {
            center: Vec3A::new(0.0, 0.0, 10.0),
            radius: 1e-20,
        };
        assert!(shape.sample_solid_angle(Vec3A::ZERO, 0.5, 0.5).is_none());
        assert!(shape.solid_angle_pdf(Vec3A::ZERO, shape.center).is_none());
        // ...while an ordinary tiny, distant sphere still samples its cone,
        // at a finite density.
        let small = SphereShape {
            center: Vec3A::new(0.0, 0.0, 1e3),
            radius: 1e-3,
        };
        let (_, pdf) = small.sample_solid_angle(Vec3A::ZERO, 0.5, 0.5).unwrap();
        assert!(pdf.is_finite() && pdf > 0.0);
        assert_eq!(small.solid_angle_pdf(Vec3A::ZERO, small.center), Some(pdf));
    }

    #[test]
    fn sphere_shape_samples_lie_on_surface() {
        let shape = SphereShape {
            center: Vec3A::new(1.0, 2.0, 3.0),
            radius: 0.5,
        };
        for (u, v) in [(0.0, 0.0), (0.25, 0.75), (0.99, 0.5), (0.5, 0.01)] {
            let p = shape.sample_point(u, v);
            let d = (p - shape.center).length();
            assert!((d - shape.radius).abs() < 1e-5, "sample off surface: {d}");
            let n = shape.normal_at(p);
            assert!((n.length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn rect_shape_samples_lie_in_rect() {
        let shape = RectShape::new(
            Vec3A::new(-1.0, 5.0, -2.0),
            Vec3A::new(2.0, 0.0, 0.0),
            Vec3A::new(0.0, 0.0, 4.0),
            Vec3A::new(0.0, -1.0, 0.0),
        );
        assert!((shape.area() - 8.0).abs() < 1e-5);
        let p = shape.sample_point(0.5, 0.5);
        assert!((p - Vec3A::new(0.0, 5.0, 0.0)).length() < 1e-5);
        assert_eq!(shape.normal_at(p), Vec3A::new(0.0, -1.0, 0.0));
    }

    #[test]
    fn area_light_pdf_is_positive_facing_side() {
        let light = AreaLight::new(
            Box::new(SphereShape {
                center: Vec3A::new(0.0, 5.0, 0.0),
                radius: 1.0,
            }),
            Arc::new(Emissive::new(Vec3A::splat(10.0))),
            0,
        );
        // Nearest point on the sphere as seen from below.
        let pdf = light.pdf_at_point(Vec3A::ZERO, Vec3A::new(0.0, 4.0, 0.0));
        assert!(pdf.is_finite() && pdf > 0.0);

        // The sampled connection agrees: it aims upward at the light, stops
        // at a finite distance, and reports the same emission and a pdf of
        // the same shape.
        let s = light
            .sample_li(Vec3A::ZERO, 0.3, 0.7)
            .expect("a sphere overhead is always reachable");
        assert!(s.direction.is_normalized());
        assert!(s.distance.is_finite() && s.distance > 0.0);
        assert_eq!(s.radiance, Vec3A::splat(10.0));
        assert!(s.pdf.is_finite() && s.pdf > 0.0);

        // `sample_li` and `pdf_at_point` are the two MIS sides of one
        // strategy and must agree on the density of the same direction.
        let point = Vec3A::ZERO + s.direction * s.distance;
        let from_point = light.pdf_at_point(Vec3A::ZERO, point);
        assert!(
            (s.pdf - from_point).abs() <= 1e-3 * s.pdf.max(from_point),
            "MIS sides disagree: sample_li {} vs pdf_at_point {}",
            s.pdf,
            from_point
        );

        // An area light has geometry and no escaped-ray contribution.
        assert_eq!(light.geom_id(), Some(0));
        assert!(light.escaped(Vec3A::ZERO, Vec3A::Y).is_none());
    }

    /// The cone convention: every sampled direction lies inside the
    /// source cone, and `escaped` agrees about exactly which directions
    /// those are. Disagreement here would mean NEE and the bounce side
    /// find the light in different sets of directions.
    #[test]
    fn distant_light_cone_is_consistent() {
        let dir = Vec3A::new(0.3, -1.0, 0.2).normalize();
        let light = DistantLight::new(dir, Vec3A::splat(2.0), 10.0);

        let mut rng = openqmc::pcg::Rng::new(7);
        for _ in 0..2000 {
            let s = light
                .sample_li(Vec3A::ZERO, rng.next_f32(), rng.next_f32())
                .expect("a distant light is reachable from anywhere");
            assert!(s.direction.is_normalized());
            assert!(
                s.distance.is_infinite(),
                "a light at infinity cannot be occluded by anything in the scene"
            );
            // Sampled directions must be ones `escaped` also covers.
            let (radiance, pdf) = light
                .escaped(Vec3A::ZERO, s.direction)
                .expect("sample_li produced a direction escaped() does not cover");
            assert_eq!(radiance, s.radiance);
            assert!(
                (pdf - s.pdf).abs() < 1e-3 * s.pdf,
                "MIS sides disagree on the pdf: {} vs {}",
                s.pdf,
                pdf
            );
        }

        // And nothing outside the cone is covered: the opposite hemisphere
        // and a direction just past the half-angle both miss.
        assert!(light.escaped(Vec3A::ZERO, dir).is_none());
        let outside = utils::align_to_normal(
            Vec3A::new(20f32.to_radians().sin(), 0.0, 20f32.to_radians().cos()),
            -dir,
        )
        .normalize();
        assert!(
            light.escaped(Vec3A::ZERO, outside).is_none(),
            "a direction 20° off-axis is outside a 10° cone"
        );
    }

    /// `DistantLight::new`'s energy convention: its argument is the
    /// *irradiance* on a surface facing the light, and radiance is derived
    /// over the cone. So widening the angle must soften shadows without
    /// changing exposure — `L · π sin²θ` stays put. (Whether an authored
    /// `intensity` means that or a radiance is the importer's decision:
    /// `inputs:normalize`.)
    #[test]
    fn distant_light_irradiance_is_angle_invariant() {
        let dir = -Vec3A::Y;
        let e = Vec3A::new(3.0, 2.0, 1.0);
        for angle in [0.0f32, 0.53, 5.0, 30.0] {
            let light = DistantLight::new(dir, e, angle);
            let s = light.sample_li(Vec3A::ZERO, 0.4, 0.6).expect("reachable");
            // Radiance integrated against the facing surface's cosine over
            // the cone returns the authored irradiance, whatever the angle.
            let half = 0.5 * DistantLight::clamp_diameter(angle).to_radians();
            let recovered = s.radiance * projected_cone_solid_angle(half);
            assert!(
                (recovered - e).length() < 1e-3 * e.length(),
                "angle {angle}°: irradiance {recovered:?} != authored {e:?}"
            );
        }
    }

    /// A zero angle is widened rather than made singular, so the pdf stays
    /// finite and the integrator needs no delta-light path.
    #[test]
    fn distant_light_zero_angle_stays_finite() {
        let light = DistantLight::new(-Vec3A::Y, Vec3A::ONE, 0.0);
        let s = light.sample_li(Vec3A::ZERO, 0.5, 0.5).expect("reachable");
        assert!(s.pdf.is_finite() && s.pdf > 0.0, "pdf = {}", s.pdf);
        assert!(
            s.radiance.is_finite(),
            "radiance must stay finite: {:?}",
            s.radiance
        );
        // Still a *tight* cone: a degree off-axis is outside it.
        let off = utils::align_to_normal(
            Vec3A::new(1f32.to_radians().sin(), 0.0, 1f32.to_radians().cos()),
            Vec3A::Y,
        )
        .normalize();
        assert!(light.escaped(Vec3A::ZERO, off).is_none());
    }

    /// A distant light has no scene geometry, so bounce rays must never try
    /// to attribute a *hit* to it.
    #[test]
    fn distant_light_has_no_geometry() {
        let light = DistantLight::new(-Vec3A::Y, Vec3A::ONE, 1.0);
        assert_eq!(light.geom_id(), None);
        assert_eq!(light.pdf_at_point(Vec3A::ZERO, Vec3A::Y), 0.0);
    }

    #[test]
    fn find_by_geom_matches_by_id() {
        let mat = Arc::new(Emissive::new(Vec3A::splat(1.0)));
        let mut lights = LightList::new();
        lights.add(Arc::new(AreaLight::new(
            Box::new(SphereShape {
                center: Vec3A::ZERO,
                radius: 1.0,
            }),
            mat,
            7,
        )));

        assert!(lights.find_by_geom(7).is_some());
        assert!(lights.find_by_geom(8).is_none());
    }
}
