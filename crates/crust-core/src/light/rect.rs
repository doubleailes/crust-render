//! [`RectShape`]: the rectangle light's surface, sampled by the spherical
//! rectangle it subtends (Ureña, Fajardo & King 2013) in `f64`.

use glam::{DVec2, DVec3, Vec3, Vec3A};

use super::cone::ConeCut;
use super::shape::{Kind, LightShape, ShapeHits, SolidAngleSampler};
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
    /// The emitter's shaping, for a rectangle whose shaping axis is its
    /// normal ([`RectShape::set_cone`]).
    pub(super) cone: Option<ConeCut>,
    /// `CRUST_CONE_CLIP`: sample only within the cone's cutoff square.
    pub(super) cone_clip: bool,
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
pub(super) const MIN_SPHERICAL_RECT_SR: f64 = 1e-4;

/// Above this (sr) — a shading point almost on the light's plane, where the
/// rectangle fills nearly a hemisphere — the map's `sin(a_u)` divisions
/// degenerate, and the rectangle is area-sampled. pbrt-v4's
/// `MaxSphericalSampleArea`.
pub(super) const MAX_SPHERICAL_RECT_SR: f64 = 6.22;

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
            cone: None,
            cone_clip: crate::config().cone_clip,
        }
    }

    /// The same light with the cone clip on or off whatever the environment
    /// says: both sides of the switch in one process.
    #[must_use]
    pub fn with_cone_clip(mut self, cone_clip: bool) -> Self {
        self.cone_clip = cone_clip;
        self
    }

    /// Takes the emitter's shaping where the cutoff region is a circle about
    /// a receiver's foot: on a true rectangle whose shaping axis is its
    /// normal. Any other light keeps sampling as if unshaped.
    pub(super) fn set_cone(&mut self, cone: ConeCut) {
        self.cone = (self.frame.is_some() && cone.is_along(self.normal)).then_some(cone);
    }

    /// The shaping this light samples by, if it took one.
    pub fn cone(&self) -> Option<&ConeCut> {
        self.cone.as_ref()
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

    /// What the cone clip (`CRUST_CONE_CLIP`) makes of the light from `from`:
    /// the
    /// spherical rectangle of the part of the light inside the bounding
    /// square of the cone's cutoff circle about `from`'s foot, where the rest
    /// of the light emits nothing toward `from` ([`ConeCut::cutoff_radius`]).
    ///
    /// In the [`SphericalRect`] frame the foot is the origin, so the square
    /// is `[−ρc, ρc]²` and the clip is four comparisons. A square that misses
    /// the light is [`SolidAngleSampler::nothing`]: no sample, and no density
    /// on the bounce side, where the light is dark toward `from` anyway. A
    /// square that holds the whole light is the unclipped strategy, bit for
    /// bit. A clipped one is sampled by its spherical rectangle at any solid
    /// angle the map holds in f64 (the floor that sends a whole small light to
    /// area sampling is for speed, and a clipped light is the one worth the
    /// map), and by area over the whole light above `MAX_SPHERICAL_RECT_SR`,
    /// which only a cone wider than about 87° reaches.
    ///
    /// Its kind is all the strategy keeps ([`RectShape::clip_kind`]); the
    /// clip itself is rebuilt where a sample is drawn or a density asked
    /// (see `shape::Cut`).
    fn clip(&self, cone: &ConeCut, from: Vec3A) -> Clip {
        let Some(frame) = self.frame.as_ref() else {
            return Clip::Whole;
        };
        let d = frame.origin - DVec3::from(Vec3::from(from));
        let h = d.dot(frame.z).abs();
        if d.dot(frame.normal) >= 0.0 || h <= 0.0 {
            return Clip::Whole;
        }
        let Some(rc) = cone.cutoff_radius(h) else {
            return Clip::Whole;
        };
        let (x0, y0) = (d.dot(frame.x), d.dot(frame.y));
        // The light's bounds as `SphericalRect::new` forms them.
        let (x1, y1) = (x0 + frame.width, y0 + frame.height);
        let (cx0, cx1, cy0, cy1) = (x0.max(-rc), x1.min(rc), y0.max(-rc), y1.min(rc));
        if !(cx0 < cx1 && cy0 < cy1) {
            return Clip::Nothing;
        }
        if (cx0, cx1, cy0, cy1) == (x0, x1, y0, y1) {
            return Clip::Whole;
        }
        match SphericalRect::from_bounds(cx0, cx1, cy0, cy1, h) {
            Some(rect) if rect.solid_angle > 0.0 && rect.solid_angle <= MAX_SPHERICAL_RECT_SR => {
                Clip::Part(ClippedRect::new(rect, DVec2::new(x0, y0)))
            }
            _ => Clip::Area,
        }
    }

    /// The kind of [`RectShape::clip`] from `from`, out of line.
    #[inline(never)]
    pub(super) fn clip_kind(&self, cone: &ConeCut, from: Vec3A) -> Kind {
        match self.clip(cone, from) {
            Clip::Whole => Kind::Whole,
            Clip::Nothing => Kind::Nothing,
            Clip::Part(_) => Kind::Part,
            Clip::Area => Kind::Area,
        }
    }

    /// A point of the clipped strategy from `from`, placed through the light's
    /// own `(s, t)` so it lies where a bounce ray hits, and its density (`0`
    /// where the clip does not apply, which its kind rules out).
    #[inline(never)]
    pub(super) fn clip_sample(&self, from: Vec3A, u: f32, v: f32) -> (Vec3A, f32) {
        let (Some(cone), Some(frame)) = (self.cone.as_ref(), self.frame.as_ref()) else {
            return (Vec3A::ZERO, 0.0);
        };
        let Clip::Part(rect) = self.clip(cone, from) else {
            return (Vec3A::ZERO, 0.0);
        };
        let (x, y) = rect.sample_xy(f64::from(u), f64::from(v));
        let s = (x - rect.full.x) / frame.width;
        let t = (y - rect.full.y) / frame.height;
        let p = self.sample_point(s.clamp(0.0, 1.0) as f32, t.clamp(0.0, 1.0) as f32);
        (p, rect.pdf().get())
    }

    /// The clipped strategy's density from `from` at `p`, a point on the
    /// light: uniform in the solid angle of the clip, `0` outside it.
    #[inline(never)]
    pub(super) fn clip_pdf(&self, from: Vec3A, p: Vec3A) -> f32 {
        let Some(cone) = self.cone.as_ref() else {
            return 0.0;
        };
        match self.clip(cone, from) {
            Clip::Part(rect) if self.clip_holds(rect, p) => rect.pdf().get(),
            _ => 0.0,
        }
    }

    /// Whether `p`, a point on the light, lies within a [`ClippedRect`]'s
    /// bounds, give or take the f32 rounding of a point placed through the
    /// light's edges.
    pub(super) fn clip_holds(&self, rect: ClippedRect, p: Vec3A) -> bool {
        let frame = self.frame.as_ref().expect("a clipped light is a rectangle");
        let rel = DVec3::from(Vec3::from(p)) - frame.origin;
        let (x, y) = (
            rect.full.x + rel.dot(frame.x),
            rect.full.y + rel.dot(frame.y),
        );
        let tol = 8.0
            * f64::from(f32::EPSILON)
            * (frame.origin.abs().max_element() + frame.width + frame.height);
        (rect.x0 - tol..=rect.x1 + tol).contains(&x) && (rect.y0 - tol..=rect.y1 + tol).contains(&y)
    }
}

/// What the cone clip makes of a rect light from one shading point.
pub(super) enum Clip {
    /// The cutoff square holds the light (or the clip does not apply from
    /// here): the unclipped strategy.
    Whole,
    /// The cutoff square misses the light: nothing to sample.
    Nothing,
    /// The part inside the square, by its spherical rectangle.
    Part(ClippedRect),
    /// A part too large for the map: the whole light by area.
    Area,
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

/// The part of a light inside a cone's cutoff square, as a [`SphericalRect`]
/// sees it, with the whole light's origin corner `full` in the same frame,
/// which a sample's `(s, t)` on the light is measured from.
///
/// The same size as a [`SphericalRect`], so the sampler does not grow: it
/// keeps `full` where that keeps `b0` and `b1`, and recomputes those two per
/// sample. A larger `SphericalRect` (with `full` in it) cost every sphere and
/// rect sample on `veach_mis`, which has no shaped light, 13% of `sample_li`'s
/// instructions (callgrind): the sampler is copied whole for each sample.
#[derive(Clone, Copy, Debug)]
pub(super) struct ClippedRect {
    pub(super) x0: f64,
    pub(super) x1: f64,
    pub(super) y0: f64,
    pub(super) y1: f64,
    h: f64,
    g23: DVec2,
    solid_angle: f64,
    pub(super) full: DVec2,
}

impl ClippedRect {
    pub(super) fn new(rect: SphericalRect, full: DVec2) -> Self {
        Self {
            x0: rect.x0,
            x1: rect.x1,
            y0: rect.y0,
            y1: rect.y1,
            h: rect.h,
            g23: rect.g23,
            solid_angle: rect.solid_angle,
            full,
        }
    }

    /// Uniform over the solid angle: finite and positive, since a clip is only
    /// sampled this way with a solid angle in `(0, MAX_SPHERICAL_RECT_SR]`.
    pub(super) fn pdf(&self) -> PdfSolidAngle {
        PdfSolidAngle::from_measure((1.0 / self.solid_angle) as f32)
    }

    /// A point uniform in the solid angle, as `(x, y)` in this frame.
    pub(super) fn sample_xy(&self, u: f64, v: f64) -> (f64, f64) {
        let (h2, y0, y1) = (self.h * self.h, self.y0, self.y1);
        let b0 = -y0 / (h2 + y0 * y0).sqrt();
        let b1 = y1 / (h2 + y1 * y1).sqrt();
        rect_xy(
            [self.x0, self.x1, y0, y1, self.h, b0, b1],
            self.g23,
            self.solid_angle,
            u,
            v,
        )
    }
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
        Self::from_bounds(x0, x0 + frame.width, y0, y0 + frame.height, h)
    }

    /// The rectangle `[x0, x1] × [y0, y1]` in the plane `z = −h`, `h > 0`,
    /// seen from the origin.
    pub(super) fn from_bounds(x0: f64, x1: f64, y0: f64, y1: f64, h: f64) -> Option<Self> {
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
        let (xu, yv) = rect_xy(
            [self.x0, self.x1, self.y0, self.y1, self.h, self.b0, self.b1],
            self.g23,
            self.solid_angle,
            u,
            v,
        );
        let s = (xu - self.x0) / (self.x1 - self.x0);
        let t = (yv - self.y0) / (self.y1 - self.y0);
        (s.clamp(0.0, 1.0), t.clamp(0.0, 1.0))
    }
}

/// The spherical-rectangle map: the point uniform in the solid angle `omega`
/// of `[x0, x1] × [y0, y1]` at depth `h` (with `b0`, `b1` and `g23` as in
/// [`SphericalRect`]), as `(x, y)` in the rectangle's frame.
#[inline(always)]
fn rect_xy(
    [x0, x1, y0, y1, h, b0, b1]: [f64; 7],
    g23: DVec2,
    omega: f64,
    u: f64,
    v: f64,
) -> (f64, f64) {
    // The azimuthal slice holding a fraction `u` of the solid angle (the
    // paper's eq. 7-9, whose `u (g0 + g1 − 2π) + (u − 1)(g2 + g3)` is
    // `u Ω − (g2 + g3)`), by the angle-difference identities.
    let (sin_uo, cos_uo) = (u * omega).sin_cos();
    let cos_au = cos_uo * g23.x + sin_uo * g23.y;
    let sin_au = sin_uo * g23.x - cos_uo * g23.y;
    let fu = (cos_au * b0 - b1) / sin_au;
    // `fu` is ±∞ where `sin(a_u)` is zero, which lands `cu` on 0 — the
    // right limit. A 0/0 there is the only NaN; it has the same limit.
    let mut cu = (1.0 / (fu * fu + b0 * b0).sqrt()).copysign(fu);
    if cu.is_nan() {
        cu = 0.0;
    }
    let cu = cu.clamp(-1.0 + f64::EPSILON, 1.0 - f64::EPSILON);
    let xu = (cu * h / (1.0 - cu * cu).sqrt()).max(x0).min(x1);
    // Then the elevation within it, uniform in `h = cos` of the angle to
    // the y axis (eq. 10-11).
    let dd2 = xu * xu + h * h;
    let h0 = y0 / (dd2 + y0 * y0).sqrt();
    let h1 = y1 / (dd2 + y1 * y1).sqrt();
    let hv = h0 + v * (h1 - h0);
    let hv2 = hv * hv;
    let yv = if hv2 < 1.0 - 1e-12 {
        hv * (dd2 / (1.0 - hv2)).sqrt()
    } else {
        y1
    };
    let yv = yv.max(y0).min(y1);
    (xu, yv)
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
        if self.cone_clip
            && let Some(cone) = &self.cone
        {
            match self.clip_kind(cone, from) {
                Kind::Whole => {}
                Kind::Nothing => return Some(SolidAngleSampler::nothing()),
                Kind::Part => return Some(SolidAngleSampler::clipped_rect(self, from)),
                Kind::Area => return None,
            }
        }
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
