//! [`RectShape`]: the rectangle light's surface, sampled by the spherical
//! rectangle it subtends (Ureña, Fajardo & King 2013) in `f64`.

use glam::{DVec2, DVec3, Vec3, Vec3A};

use super::shape::{LightShape, ShapeHits, SolidAngleSampler};
use crate::pdf::PdfSolidAngle;

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
#[derive(Clone)]
pub struct RectShape {
    pub origin: Vec3A,
    pub edge_u: Vec3A,
    pub edge_v: Vec3A,
    pub normal: Vec3A,
    /// The orthonormal frame the spherical-rectangle map works in, `None` for
    /// a parallelogram that is not a rectangle (a sheared light).
    pub(super) frame: Option<RectFrame>,
}

/// A rectangle's own frame, in f64: the solid angle it subtends is a sum of
/// four angles less `2π`, which cancels in f32 long before the light is
/// small enough for area sampling to take over.
#[derive(Clone, Copy, Debug)]
pub(super) struct RectFrame {
    pub(super) origin: DVec3,
    pub(super) x: DVec3,
    pub(super) y: DVec3,
    pub(super) z: DVec3,
    /// Edge lengths along `x` and `y`.
    pub(super) width: f64,
    pub(super) height: f64,
    /// The unit normal the light emits along, which `z` is `±`.
    pub(super) normal: DVec3,
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
/// the angle between them in f64, before it is treated as a sheared
/// parallelogram and area-sampled.
///
/// The spherical-rectangle map samples an exact rectangle, and the point it
/// returns is placed through the light's own edges, so any shear it lets
/// through is a mismatch between the shape sampled and the density reported —
/// a bias of the order of this cosine. It is therefore set at the level f32
/// itself leaves on an unsheared light: the edges of a rotated rectangle come
/// out of an f32 transform perpendicular to a few parts in 10⁷, and an
/// authored shear is orders of magnitude above it.
const RECT_ORTHOGONALITY: f64 = 1e-6;

impl RectShape {
    pub fn new(origin: Vec3A, edge_u: Vec3A, edge_v: Vec3A, normal: Vec3A) -> Self {
        let normal = normal.normalize();
        let u = DVec3::from(Vec3::from(edge_u));
        let v = DVec3::from(Vec3::from(edge_v));
        let (lu, lv) = (u.length(), v.length());
        let rectangle = lu > 0.0
            && lv > 0.0
            && (lu * lv).is_finite()
            && u.dot(v).abs() <= RECT_ORTHOGONALITY * lu * lv;
        let frame = rectangle.then(|| {
            let x = u / lu;
            // Gram-Schmidt, so the frame is orthonormal to f64 precision even
            // though the edges are only perpendicular to f32's. The height is
            // the edge's component *across* `edge_u`, which makes the sampled
            // rectangle's area the parallelogram's exactly; what shear is left
            // is below `RECT_ORTHOGONALITY`.
            let across = v - v.dot(x) * x;
            let height = across.length();
            RectFrame {
                origin: DVec3::from(Vec3::from(origin)),
                x,
                y: across / height,
                z: x.cross(across / height),
                width: lu,
                height,
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
    /// angle outside `[MIN_SPHERICAL_RECT_SR, MAX_SPHERICAL_RECT_SR]`. Its
    /// [`LightShape::solid_angle_sampler`] is built from it, so both MIS
    /// halves answer for exactly the same `from`s.
    pub(super) fn spherical_rect(&self, from: Vec3A) -> Option<SphericalRect> {
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
pub(super) struct SphericalRect {
    pub(super) x0: f64,
    pub(super) x1: f64,
    pub(super) y0: f64,
    pub(super) y1: f64,
    pub(super) h: f64,
    /// `z` components of the inner normals of the planes through the
    /// shading point and the edges `y = y0` and `y = y1`.
    pub(super) b0: f64,
    pub(super) b1: f64,
    /// `(cos, sin)` of `g2 + g3`, the quad's internal angles at its two
    /// `x = x0` corners — all the sampler needs of that sum, and what the
    /// product of their complex numbers is once normalised.
    pub(super) g23: DVec2,
    /// `g0 + g1 + g2 + g3 − 2π`, the solid angle it subtends.
    pub(super) solid_angle: f64,
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
    pub(super) fn new(frame: &RectFrame, d: DVec3) -> Option<Self> {
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

    /// Uniform over the solid angle. Finite and positive: a rectangle is only
    /// sampled this way with a solid angle in
    /// `[MIN_SPHERICAL_RECT_SR, MAX_SPHERICAL_RECT_SR]`.
    pub(super) fn pdf(&self) -> PdfSolidAngle {
        PdfSolidAngle::from_measure((1.0 / self.solid_angle) as f32)
    }

    /// The rectangle's own `(s, t) ∈ [0, 1]²` of a point uniform in the solid
    /// angle it subtends. Area-preserving from `(u, v)`, so the sampler's
    /// stratification carries onto the sphere of directions.
    pub(super) fn sample(&self, u: f64, v: f64) -> (f64, f64) {
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
    fn kind(&self) -> &'static str {
        "rect"
    }

    fn sample_point(&self, u: f32, v: f32) -> Vec3A {
        self.origin + u * self.edge_u + v * self.edge_v
    }

    fn normal_at(&self, _p: Vec3A) -> Vec3A {
        self.normal
    }

    fn area(&self) -> f32 {
        self.edge_u.cross(self.edge_v).length()
    }

    #[inline(always)]
    fn solid_angle_sampler(&self, from: Vec3A) -> Option<SolidAngleSampler<'_>> {
        self.spherical_rect(from)
            .map(|rect| SolidAngleSampler::rect(self, rect))
    }

    /// The parallelogram from either side, as its two triangles are hit.
    fn hits(&self, origin: Vec3A, dir: Vec3A) -> ShapeHits {
        let mut hits = ShapeHits::default();
        let n = self.edge_u.cross(self.edge_v);
        let (denom, n2) = (n.dot(dir), n.length_squared());
        if denom == 0.0 || n2 == 0.0 {
            return hits;
        }
        let t = n.dot(self.origin - origin) / denom;
        // `rel = a·edge_u + b·edge_v`, solved by the two cross products.
        let rel = origin + t * dir - self.origin;
        let a = rel.cross(self.edge_v).dot(n) / n2;
        let b = self.edge_u.cross(rel).dot(n) / n2;
        if (0.0..=1.0).contains(&a) && (0.0..=1.0).contains(&b) {
            hits.push(t);
        }
        hits
    }
}
