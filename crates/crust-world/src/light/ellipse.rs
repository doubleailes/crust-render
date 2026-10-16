//! [`SphericalEllipse`]: the unit disk seen from a point in front of it, sampled
//! uniformly over the solid angle it subtends (Guillén, Ureña, King, Fajardo,
//! Georgiev, López-Moreno & Jarabo 2017, "Area-Preserving Parameterizations
//! for Spherical Ellipses"), by the paper's radial map with an exact Newton
//! inversion, in `f64`. And the elliptic integrals it needs, as Carlson's
//! symmetric forms.
//!
//! The design record (`openspec/specs/lighting/design.md`, "Disk lights")
//! has the derivation and the paper's traps.

use std::f64::consts::FRAC_PI_2;

use glam::{DVec3, Vec3, Vec3A};

use crate::pdf::PdfSolidAngle;

use super::rect::MAX_SPHERICAL_RECT_SR;

/// Below this solid angle (sr) a disk is area-sampled. The rect's floor,
/// `1e-4`, is where its map stops being worth computing; the ellipse's is
/// higher, because below it area sampling of a small disk was the better
/// sampler outright: on a disk of radius 0.1 at height 1 (~0.03 sr) the
/// ellipse had 1.8× area sampling's 16 spp relMSE on a diffuse floor (it
/// stratifies worse than the area map there), at 2× its cost, while at
/// radius 0.3 (~0.26 sr) it had a third of it.
pub(super) const MIN_ELLIPSE_SR: f64 = 0.1;

/// Below this minor-to-major semi-axis ratio — a disk seen nearly edge-on —
/// `n → 1` and eq. 19's `Π` grows while its prefactor shrinks, and the disk is
/// area-sampled, as it was before the ellipse.
const MIN_AXIS_RATIO: f64 = 1e-4;

/// Newton's iteration cap. The paper reports 1–4 iterations, and the tests
/// pin at most 6 over a dense grid; a bisection step keeps every iterate in
/// the bracket, so the cap is a guard, never the stopping rule.
const MAX_NEWTON: usize = 32;

/// The unit disk in the local XY plane, emitting along −Z, as seen from a
/// local point strictly in front of it: the solid angle of one quadrant of the
/// spherical ellipse it subtends, `Ω_r(π/2)`, which is all a sampler keeps.
///
/// That is the one elliptic integral the strategy has to decide with; the rest
/// of the map ([`EllipseMap`]) is a frame and a few constants, rebuilt from the
/// shading point where a sample is drawn. Kept this small because the
/// sampler's strategy enum is sized by its largest variant, and a 200-byte one
/// cost every sphere and rect light sample a copy (callgrind, `veach_mis`).
#[derive(Clone, Copy, Debug)]
pub(super) struct SphericalEllipse {
    quarter: f64,
}

/// The spherical ellipse's radial map from one shading point: its frame, its
/// semi-axes and eq. 19–20's constants.
#[derive(Clone, Copy, Debug)]
struct EllipseMap {
    /// The shading point, in the disk's local space.
    origin: DVec3,
    /// The ellipse's frame: `x` along the major semi-arc `α`, `y` along the
    /// minor `β`, `z` its centre direction.
    x: DVec3,
    y: DVec3,
    z: DVec3,
    /// `sin α` and `sin β`, eq. 18's `a ≥ b`.
    a: f64,
    b: f64,
    /// `tan α` and `tan β`.
    a_t: f64,
    b_t: f64,
    /// Eq. 20's characteristic and parameter, and their complements in
    /// closed form: `1 − n = b² cos² α / (a² cos² β)` and `1 − m = cos² α /
    /// cos² β`. A nearly edge-on ellipse has `n → 1`, where `1 − n sin² φ`
    /// formed by subtraction loses the digits `Π` is most sensitive to, and
    /// eq. 19's `φ − k Π` then cancels them into the solid angle.
    n: f64,
    m: f64,
    one_minus_n: f64,
    one_minus_m: f64,
    /// Eq. 19's prefactor `b (1 − a²) / (a √(1 − b²))`.
    k: f64,
    /// `Ω_r(π/2)`, one quadrant's solid angle.
    quarter: f64,
}

impl SphericalEllipse {
    /// The ellipse the unit disk subtends from `from` (local space), or `None`
    /// where the disk is area-sampled instead: on or behind the emitting
    /// side, nearly edge-on (`b < MIN_AXIS_RATIO · a`), or with a solid angle
    /// outside `[MIN_ELLIPSE_SR, MAX_SPHERICAL_RECT_SR]` — above it, as for the
    /// rect, the shading point is nearly on the disk's plane.
    #[inline(never)]
    pub(super) fn new(from: Vec3A) -> Option<Self> {
        Self::unbanded(from)
            .filter(|e| (MIN_ELLIPSE_SR..=MAX_SPHERICAL_RECT_SR).contains(&e.solid_angle()))
    }

    /// [`SphericalEllipse::new`] without the solid-angle band: what the
    /// tests check against brute force, at any distance.
    pub(super) fn unbanded(from: Vec3A) -> Option<Self> {
        EllipseMap::new(from).map(|map| Self {
            quarter: map.quarter,
        })
    }

    /// The solid angle the disk subtends, in steradians.
    pub(super) fn solid_angle(&self) -> f64 {
        4.0 * self.quarter
    }

    /// Uniform over the solid angle: `1 / Ω_D`, finite and positive for
    /// every ellipse [`SphericalEllipse::new`] returns.
    pub(super) fn pdf(&self) -> PdfSolidAngle {
        PdfSolidAngle::from_measure((1.0 / self.solid_angle()) as f32)
    }

    /// A point on the unit disk (local space, `z = 0`) uniform in the solid
    /// angle the disk subtends from `from`, the point this ellipse was built
    /// from (see [`EllipseMap::sample_counting`]).
    pub(super) fn sample(&self, from: Vec3A, u: f64, v: f64) -> Option<DVec3> {
        let map = EllipseMap::frame(from)?.with_quarter(self.quarter);
        Some(map.sample_counting(u, v).0)
    }
}

impl EllipseMap {
    /// The map from `from`, `None` where [`SphericalEllipse::unbanded`] is.
    fn new(from: Vec3A) -> Option<Self> {
        let mut map = Self::frame(from)?;
        map.quarter = map.omega_r(FRAC_PI_2).2;
        (map.quarter.is_finite() && map.quarter > 0.0).then_some(map)
    }

    /// The map's frame and constants from `from`, all but `Ω_r(π/2)`: the
    /// geometry, with no elliptic integral.
    fn frame(from: Vec3A) -> Option<Self> {
        let o = DVec3::from(Vec3::from(from));
        // Strictly in front: the disk emits along −Z, so the shading point is
        // at negative z and the disk lies along +Z from it.
        let h = -o.z;
        if h.is_nan() || h <= 0.0 {
            return None;
        }
        // The symmetry plane holds `o`, the centre and the normal. Paper eq. 3
        // as printed puts the centre in the plane of its `x̂_d`, then measures
        // the semi-major axis along `x̂_d` — both cannot hold. The major axis
        // is the one perpendicular to the symmetry plane; the foreshortened
        // one is in it (eq. 3's trap in the lighting design record, "Disk and tube lights").
        let d = (o.x * o.x + o.y * o.y).sqrt();
        let toward = if d > 0.0 {
            DVec3::new(-o.x / d, -o.y / d, 0.0)
        } else {
            // On the axis the ellipse is a circle and any frame will do.
            DVec3::X
        };
        // In the symmetry plane, as (along `toward`, along +Z): the two rim
        // points' directions, the minor semi-arc between them, and the centre
        // direction bisecting it.
        let unit2 = |x: f64, y: f64| {
            let l = (x * x + y * y).sqrt();
            (x / l, y / l)
        };
        let q0 = unit2(d - 1.0, h);
        let q1 = unit2(d + 1.0, h);
        let half = ((q1.0 - q0.0) * 0.5, (q1.1 - q0.1) * 0.5);
        let b_in = (half.0 * half.0 + half.1 * half.1).sqrt();
        let mid = ((q0.0 + q1.0) * 0.5, (q0.1 + q1.1) * 0.5);
        let cos_b = (mid.0 * mid.0 + mid.1 * mid.1).sqrt();
        let c = (mid.0 / cos_b, mid.1 / cos_b);
        // The centre direction meets the disk's plane at `t` from `o`, on the
        // symmetry plane `off` from the disk's centre; the chord through it
        // perpendicular to the plane is the major axis' rim, `±chord` along
        // the perpendicular.
        let t = h / c.1;
        let off = t * c.0 - d;
        let chord2 = 1.0 - off * off;
        if chord2.is_nan() || chord2 <= 0.0 {
            return None;
        }
        let chord = chord2.sqrt();
        let rim = (t * t + chord2).sqrt();
        let a_in = chord / rim;
        let cos_a = t / rim;

        let z = c.0 * toward + c.1 * DVec3::Z;
        let perpendicular = toward.cross(DVec3::Z);
        let foreshortened = z.cross(perpendicular);
        // Eq. 20 needs `a ≥ b`; if the foreshortened arc is ever the longer,
        // the axes swap (the map is the same up to that rotation).
        let (x, y, a, b, cos_a, cos_b) = if a_in >= b_in {
            (perpendicular, foreshortened, a_in, b_in, cos_a, cos_b)
        } else {
            (foreshortened, -perpendicular, b_in, a_in, cos_b, cos_a)
        };
        // Written to fail on a NaN too.
        let conditioned = b >= MIN_AXIS_RATIO * a && a < 1.0 && cos_a > 0.0 && cos_b > 0.0;
        if !conditioned {
            return None;
        }
        let (a2, b2) = (a * a, b * b);
        let diff = a2 - b2;
        let (cos_a2, cos_b2) = (cos_a * cos_a, cos_b * cos_b);
        Some(Self {
            origin: o,
            x,
            y,
            z,
            a,
            b,
            a_t: a / cos_a,
            b_t: b / cos_b,
            n: diff / (a2 * cos_b2),
            m: diff / cos_b2,
            one_minus_n: b2 * cos_a2 / (a2 * cos_b2),
            one_minus_m: cos_a2 / cos_b2,
            k: b * cos_a2 / (a * cos_b),
            quarter: 0.0,
        })
    }

    /// The same map, with the quadrant's solid angle the caller already has.
    fn with_quarter(mut self, quarter: f64) -> Self {
        self.quarter = quarter;
        self
    }

    /// `(sin α, sin β)`.
    #[cfg(test)]
    fn semi_axes(&self) -> (f64, f64) {
        (self.a, self.b)
    }

    /// `4 Ω_r(π/2)`.
    #[cfg(test)]
    fn solid_angle(&self) -> f64 {
        4.0 * self.quarter
    }

    /// Eq. 18's planar radius, squared: `a² b² / (a² sin² φ + b² cos² φ)`.
    fn r2(&self, sin: f64, cos: f64) -> f64 {
        let (a2, b2) = (self.a * self.a, self.b * self.b);
        a2 * b2 / (a2 * sin * sin + b2 * cos * cos)
    }

    /// Eq. 19 as a function of eq. 20's tangent-ellipse angle `ψ = φ_t`:
    /// the slice `[0, φ]`, `φ = atan((b_t / a_t) tan ψ)`, holds `φ − k Π(n; ψ
    /// | m)` of solid angle. Returns `(sin φ, cos φ, Ω_r)`.
    ///
    /// Parametrised by `ψ` rather than `φ` because `ψ` is `Π`'s own argument
    /// and the angle in which a small ellipse — the tangent ellipse, nearly —
    /// has `Ω_r` linear: Newton then converges in the same few steps on an
    /// eccentric ellipse as on a round one.
    fn omega_r(&self, psi: f64) -> (f64, f64, f64) {
        let (s, c) = psi.sin_cos();
        let (y, x) = (self.b_t * s, self.a_t * c);
        let l = (x * x + y * y).sqrt();
        let c2 = c * c;
        let omega = y.atan2(x)
            - self.k
                * carlson_pi(
                    self.n,
                    s,
                    c2,
                    self.one_minus_m + self.m * c2,
                    self.one_minus_n + self.n * c2,
                );
        (y / l, x / l, omega)
    }

    /// A point on the unit disk (local space, `z = 0`) uniform in the solid
    /// angle the disk subtends, by the low-distortion radial map (eq. 23):
    /// `(u, v)` through Shirley–Chiu's concentric map and back through the
    /// polar map, then `u` picks the azimuth by solid angle (eq. 21) and `v`
    /// the altitude within it (eq. 22). Area-preserving from `(u, v)`, so the
    /// sampler's stratification carries onto the sphere of directions. With
    /// the Newton iterations it took.
    ///
    /// The pre-warp keeps the radial map's centre from being a point every
    /// `u` converges on. It had the lower 16 spp relMSE than the plain radial
    /// map on 7 of the 8 disks of the sweep and on `usdlux` (task 2.4 in the
    /// change that added it), for no elliptic integral.
    fn sample_counting(&self, u: f64, v: f64) -> (DVec3, usize) {
        let (a, b) = (2.0 * u - 1.0, 2.0 * v - 1.0);
        let (r, theta) = if a == 0.0 && b == 0.0 {
            (0.0, 0.0)
        } else if a.abs() > b.abs() {
            (a, std::f64::consts::FRAC_PI_4 * (b / a))
        } else {
            (b, FRAC_PI_2 - std::f64::consts::FRAC_PI_4 * (a / b))
        };
        let (x, y) = (r * theta.cos(), r * theta.sin());
        let az = y.atan2(x).rem_euclid(2.0 * std::f64::consts::PI);
        let (u, v) = (az / (2.0 * std::f64::consts::PI), 1.0 - (x * x + y * y));
        // Four quadrants, the odd ones walked backwards so the map is
        // continuous across their edges (as eq. 23 does).
        let e = (u * 4.0).clamp(0.0, 4.0);
        let quadrant = (e as usize).min(3);
        let mut t = e - quadrant as f64;
        if quadrant & 1 == 1 {
            t = 1.0 - t;
        }
        let ((sin, cos), iterations) = self.invert(t * self.quarter);
        // Eq. 22, uniform in the altitude `h` between the rim and the centre,
        // as `1 − h` so a small ellipse keeps its precision: `1 − h_r = r² /
        // (1 + h_r)`.
        let r2 = self.r2(sin, cos);
        let one_minus_hr = r2 / (1.0 + (1.0 - r2).max(0.0).sqrt());
        let one_minus_h = (1.0 - v) * one_minus_hr;
        let h = 1.0 - one_minus_h;
        let sin_theta = (one_minus_h * (1.0 + h)).max(0.0).sqrt();
        let (sx, sy) = match quadrant {
            0 => (1.0, 1.0),
            1 => (-1.0, 1.0),
            2 => (-1.0, -1.0),
            _ => (1.0, -1.0),
        };
        let dir = sx * sin_theta * cos * self.x + sy * sin_theta * sin * self.y + h * self.z;
        // Onto the disk's plane, then back inside the rim by what rounding
        // left outside it.
        let p = self.origin + dir * (-self.origin.z / dir.z);
        let rho2 = p.x * p.x + p.y * p.y;
        let p = if rho2 > 1.0 {
            let s = 1.0 / rho2.sqrt();
            DVec3::new(p.x * s, p.y * s, 0.0)
        } else {
            DVec3::new(p.x, p.y, 0.0)
        };
        (p, iterations)
    }

    /// Eq. 21: `(sin φ, cos φ)` of the `φ ∈ [0, π/2]` whose slice holds
    /// `target` of solid angle, by Newton in `ψ` (see
    /// [`EllipseMap::omega_r`]) with the closed-form derivative
    /// `dΩ_r/dφ = 1 − h_r(φ)` (eq. 16) times
    /// `dφ/dψ = a_t b_t / (a_t² cos² ψ + b_t² sin² ψ)`, bracketed so a step
    /// that leaves `[lo, hi]` bisects instead. Starts from
    /// `ψ = (target / Ω_r(π/2)) π/2`, the circle's exact answer and a flat
    /// ellipse's.
    fn invert(&self, target: f64) -> ((f64, f64), usize) {
        if target <= 0.0 {
            return ((0.0, 1.0), 0);
        }
        if target >= self.quarter {
            return ((1.0, 0.0), 0);
        }
        let (mut lo, mut hi) = (0.0, FRAC_PI_2);
        let mut psi = target / self.quarter * FRAC_PI_2;
        let mut last = (0.0, 1.0);
        for i in 1..=MAX_NEWTON {
            let (sin, cos, omega) = self.omega_r(psi);
            last = (sin, cos);
            let g = omega - target;
            // Converged once the residual is down to `Ω_r`'s own rounding,
            // which is absolute (eq. 19 cancels to it from `φ ≤ π/2`): at
            // most 7e-11 of a quadrant the band admits.
            if g.abs() <= 8.0 * f64::EPSILON {
                return (last, i);
            }
            if g > 0.0 {
                hi = psi;
            } else {
                lo = psi;
            }
            let r2 = self.r2(sin, cos);
            let (sp, cp) = psi.sin_cos();
            let dphi_dpsi = self.a_t * self.b_t
                / (self.a_t * self.a_t * cp * cp + self.b_t * self.b_t * sp * sp);
            let slope = r2 / (1.0 + (1.0 - r2).max(0.0).sqrt()) * dphi_dpsi;
            let mut next = psi - g / slope;
            if !(next >= lo && next <= hi) {
                next = 0.5 * (lo + hi);
            }
            let step = (next - psi).abs();
            psi = next;
            if step <= 1e-12 {
                let (sin, cos, _) = self.omega_r(psi);
                return ((sin, cos), i);
            }
        }
        (last, MAX_NEWTON)
    }
}

/// Carlson's `R_F(x, y, z)` by the duplication algorithm (Carlson 1995;
/// Numerical Recipes' `rf`), for non-negative arguments with at most one zero.
pub(super) fn carlson_rf(x: f64, y: f64, z: f64) -> f64 {
    const ERRTOL: f64 = 0.0008;
    const C1: f64 = 1.0 / 24.0;
    const C2: f64 = 0.1;
    const C3: f64 = 3.0 / 44.0;
    const C4: f64 = 1.0 / 14.0;
    let (mut x, mut y, mut z) = (x, y, z);
    loop {
        let (sx, sy, sz) = (x.sqrt(), y.sqrt(), z.sqrt());
        let lambda = sx * (sy + sz) + sy * sz;
        x = 0.25 * (x + lambda);
        y = 0.25 * (y + lambda);
        z = 0.25 * (z + lambda);
        let ave = (x + y + z) / 3.0;
        let (dx, dy, dz) = ((ave - x) / ave, (ave - y) / ave, (ave - z) / ave);
        if dx.abs().max(dy.abs()).max(dz.abs()) <= ERRTOL || !ave.is_finite() {
            let e2 = dx * dy - dz * dz;
            let e3 = dx * dy * dz;
            return (1.0 + (C1 * e2 - C2 - C3 * e3) * e2 + C4 * e3) / ave.sqrt();
        }
    }
}

/// Carlson's degenerate `R_C(x, y) = R_F(x, y, y)` for `y > 0`.
fn carlson_rc(x: f64, y: f64) -> f64 {
    const ERRTOL: f64 = 0.0005;
    const C1: f64 = 0.3;
    const C2: f64 = 1.0 / 7.0;
    const C3: f64 = 0.375;
    const C4: f64 = 9.0 / 22.0;
    let (mut x, mut y) = (x, y);
    loop {
        let lambda = 2.0 * x.sqrt() * y.sqrt() + y;
        x = 0.25 * (x + lambda);
        y = 0.25 * (y + lambda);
        let ave = (x + y + y) / 3.0;
        let s = (y - ave) / ave;
        if s.abs() <= ERRTOL || !ave.is_finite() {
            return (1.0 + s * s * (C1 + s * (C2 + s * (C3 + s * C4)))) / ave.sqrt();
        }
    }
}

/// Carlson's `R_J(x, y, z, p)` by the duplication algorithm (Carlson 1995;
/// Numerical Recipes' `rj`), for non-negative `x, y, z` with at most one zero
/// and `p > 0`.
pub(super) fn carlson_rj(x: f64, y: f64, z: f64, p: f64) -> f64 {
    const ERRTOL: f64 = 0.0005;
    const C1: f64 = 3.0 / 14.0;
    const C2: f64 = 1.0 / 3.0;
    const C3: f64 = 3.0 / 22.0;
    const C4: f64 = 3.0 / 26.0;
    const C5: f64 = 0.75 * C3;
    const C6: f64 = 1.5 * C4;
    const C7: f64 = 0.5 * C2;
    const C8: f64 = C3 + C3;
    let (mut x, mut y, mut z, mut p) = (x, y, z, p);
    let (mut sum, mut fac) = (0.0, 1.0);
    loop {
        let (sx, sy, sz) = (x.sqrt(), y.sqrt(), z.sqrt());
        let lambda = sx * (sy + sz) + sy * sz;
        let alpha = p * (sx + sy + sz) + sx * sy * sz;
        let beta = p * (p + lambda) * (p + lambda);
        sum += fac * carlson_rc(alpha * alpha, beta);
        fac *= 0.25;
        x = 0.25 * (x + lambda);
        y = 0.25 * (y + lambda);
        z = 0.25 * (z + lambda);
        p = 0.25 * (p + lambda);
        let ave = 0.2 * (x + y + z + p + p);
        let (dx, dy, dz, dp) = (
            (ave - x) / ave,
            (ave - y) / ave,
            (ave - z) / ave,
            (ave - p) / ave,
        );
        if dx.abs().max(dy.abs()).max(dz.abs()).max(dp.abs()) <= ERRTOL || !ave.is_finite() {
            let ea = dx * (dy + dz) + dy * dz;
            let eb = dx * dy * dz;
            let ec = dp * dp;
            let ed = ea - 3.0 * ec;
            let ee = eb + 2.0 * dp * (ea - ec);
            return 3.0 * sum
                + fac
                    * (1.0
                        + ed * (-C1 + C5 * ed - C6 * ee)
                        + eb * (C7 + dp * (-C8 + dp * C4))
                        + dp * ea * (C2 - dp * C3)
                        - C2 * dp * ec)
                    / (ave * ave.sqrt());
        }
    }
}

/// The incomplete elliptic integral of the third kind,
/// `Π(n; φ | m) = ∫₀^φ dθ / ((1 − n sin² θ) √(1 − m sin² θ))`, for
/// `φ ∈ [0, π/2]`, `m < 1` and `n < 1`, from Carlson's forms (Guillén et al.
/// 2017, §4.2): `s R_F(c², 1 − m s², 1) + (n/3) s³ R_J(c², 1 − m s², 1,
/// 1 − n s²)`, `s = sin φ`, `c = cos φ`.
#[cfg(test)]
pub(super) fn ellint_pi(n: f64, phi: f64, m: f64) -> f64 {
    let (s, c) = phi.sin_cos();
    carlson_pi(n, s, c * c, 1.0 - m * s * s, 1.0 - n * s * s)
}

/// [`ellint_pi`] from `s = sin φ`, `c2 = cos² φ`, `y = 1 − m s²` and
/// `p = 1 − n s²`, which the caller forms without cancellation.
fn carlson_pi(n: f64, s: f64, c2: f64, y: f64, p: f64) -> f64 {
    let rf = carlson_rf(c2, y, 1.0);
    if n == 0.0 {
        return s * rf;
    }
    s * rf + n / 3.0 * s * s * s * carlson_rj(c2, y, 1.0, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative(got: f64, want: f64) -> f64 {
        if want == 0.0 {
            got.abs()
        } else {
            ((got - want) / want).abs()
        }
    }

    /// `R_F`, `R_J` and `Π` against mpmath at 50 digits
    /// (`scripts/ellipse_oracle.py`), over the `(n, m, φ)` eq. 20 produces.
    #[test]
    fn elliptic_integrals_match_mpmath() {
        let data = include_str!("../../tests/data/ellipse_oracle.txt");
        let (mut cases, mut worst) = (0, 0.0f64);
        for line in data
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
        {
            let mut it = line.split_whitespace();
            let kind = it.next().unwrap();
            let v: Vec<f64> = it.map(|t| t.parse().unwrap()).collect();
            let (got, want, tol) = match kind {
                "rf" => (carlson_rf(v[0], v[1], v[2]), v[3], 1e-12),
                "rj" => (carlson_rj(v[0], v[1], v[2], v[3]), v[4], 1e-12),
                // From `(n, φ, m)` as given, `Π` is ill-conditioned near
                // `n s² = 1` — `sin φ`'s last ulp alone moves it by
                // `n s² / (1 − n s²)` ulps — which the sampler avoids by
                // forming `1 − n s²` in closed form (`one_minus_n`). Its
                // `R_J` is pinned at 1e-12 by the `rj` lines, from exact
                // arguments.
                "pi" => {
                    let ns2 = v[0] * v[1].sin().powi(2);
                    let tol = 1e-12f64.max(4.0 * f64::EPSILON * ns2 / (1.0 - ns2));
                    (ellint_pi(v[0], v[1], v[2]), v[3], tol)
                }
                other => panic!("unknown case {other}"),
            };
            let err = relative(got, want);
            if tol == 1e-12 {
                worst = worst.max(err);
            }
            assert!(err <= tol, "{line}: got {got:e}, relative error {err:e}");
            cases += 1;
        }
        assert!(cases > 600, "{cases} cases");
        eprintln!("{cases} cases, worst relative error {worst:e}");
    }

    /// The disk's solid angle by brute force: a midpoint rule over the disk of
    /// `cos θ / r²`, in polar rings fine enough for 1e-6 relative.
    fn brute_force_solid_angle(from: DVec3) -> f64 {
        const NR: usize = 1500;
        const NP: usize = 1500;
        let h = -from.z;
        let mut sum = 0.0;
        for i in 0..NR {
            let r = (i as f64 + 0.5) / NR as f64;
            let mut ring = 0.0;
            for j in 0..NP {
                let phi = 2.0 * std::f64::consts::PI * (j as f64 + 0.5) / NP as f64;
                let p = DVec3::new(r * phi.cos(), r * phi.sin(), 0.0);
                let d2 = (p - from).length_squared();
                ring += h / (d2 * d2.sqrt());
            }
            sum += ring * r;
        }
        sum * (1.0 / NR as f64) * (2.0 * std::f64::consts::PI / NP as f64)
    }

    /// `(height, offset)` from the disk's centre, in disk radii: on axis, off
    /// axis, near the plane and near edge-on.
    fn grid() -> Vec<(f64, f64)> {
        let mut g = Vec::new();
        for &h in &[0.05, 0.2, 0.5, 1.0, 2.0, 5.0, 20.0] {
            for &d in &[0.0, 0.3, 0.9, 1.0, 1.5, 3.0, 10.0, 40.0] {
                g.push((h, d));
            }
        }
        g
    }

    fn local(h: f64, d: f64) -> Vec3A {
        // Off along a diagonal, so neither axis of the frame is a local one.
        let s = std::f64::consts::FRAC_1_SQRT_2;
        Vec3A::new((d * s) as f32, (-d * s) as f32, -h as f32)
    }

    #[test]
    fn solid_angle_matches_brute_force() {
        for (h, d) in grid() {
            let from = local(h, d);
            let Some(e) = SphericalEllipse::unbanded(from) else {
                // Only the near-edge-on guard may refuse.
                assert!(h / (d + 1.0) < 1e-3, "refused at h={h}, d={d}");
                continue;
            };
            let want = brute_force_solid_angle(DVec3::from(Vec3::from(from)));
            let err = relative(e.solid_angle(), want);
            // The midpoint rule's own error is `O(1/N²)` of the integrand's
            // curvature, which near the plane (h = 0.05 over the disk) is
            // what bounds this, not the ellipse.
            let tol = if h < 0.1 && d < 1.2 { 1e-4 } else { 1e-6 };
            assert!(
                err <= tol,
                "h={h} d={d}: ellipse {} vs brute force {want}, {err:e}",
                e.solid_angle()
            );
        }
    }

    #[test]
    fn on_axis_it_is_the_cone() {
        for h in [0.01f32, 0.3, 1.0, 7.0, 50.0] {
            let e = EllipseMap::new(Vec3A::new(0.0, 0.0, -h)).unwrap();
            let h = h as f64;
            // `1 − cos α`, cancellation-free.
            let l = (1.0 + h * h).sqrt();
            let cone = 2.0 * std::f64::consts::PI / (l * (l + h));
            // Eq. 19's `φ − k Π` cancels to the solid angle, so its rounding
            // is relative to `2π`, not to `Ω`.
            let tol = 8.0 * f64::EPSILON * 2.0 * std::f64::consts::PI / cone;
            let err = relative(e.solid_angle(), cone);
            assert!(err < tol, "h={h}: {err:e} > {tol:e}");
            let (a, b) = e.semi_axes();
            assert!(relative(a, b) < 1e-14, "a circle: {a} {b}");
        }
    }

    /// Eq. 20's `m ∈ [0, 1)` needs `a ≥ b`. Over a dense grid the
    /// perpendicular arc is the longer before any swap; the swap is there for
    /// what the grid misses, and tested by construction (`a ≥ b` after it).
    #[test]
    fn the_major_axis_is_the_perpendicular_one() {
        for i in 0..60 {
            for j in 0..60 {
                let h = 1e-3 * 1.2f64.powi(i);
                let d = 1e-3 * 1.2f64.powi(j);
                let Some(e) = EllipseMap::new(local(h, d)) else {
                    continue;
                };
                let (a, b) = e.semi_axes();
                assert!(a >= b, "h={h} d={d}: a={a} b={b}");
                assert!(
                    e.x.dot(DVec3::Z).abs() < 1e-12 || d < 1e-9,
                    "h={h} d={d}: the major axis left the disk's plane"
                );
            }
        }
    }

    #[test]
    fn behind_or_edge_on_is_refused() {
        assert!(SphericalEllipse::unbanded(Vec3A::new(0.0, 0.0, 1.0)).is_none());
        assert!(SphericalEllipse::unbanded(Vec3A::new(0.3, 0.0, 0.0)).is_none());
        assert!(SphericalEllipse::unbanded(Vec3A::new(1e4, 0.0, -1e-3)).is_none());
        // Far away: unbanded, it exists; banded, it is area-sampled.
        let far = Vec3A::new(0.0, 0.0, -1000.0);
        assert!(SphericalEllipse::unbanded(far).is_some());
        assert!(SphericalEllipse::new(far).is_none());
    }

    /// Every sample is on the disk, Newton converges within 6 iterations, and
    /// the samples are uniform in solid angle: a histogram over the
    /// ellipse's own `(azimuth, altitude)` cells of equal solid angle.
    #[test]
    fn samples_are_uniform_in_solid_angle() {
        let views = [
            ("centred", Vec3A::new(0.0, 0.0, -0.8)),
            ("off-axis", Vec3A::new(1.3, -0.7, -0.6)),
            ("near edge-on", Vec3A::new(3.0, 1.0, -0.05)),
        ];
        for (name, from) in views {
            check_histogram(name, &EllipseMap::new(from).unwrap(), from);
        }
    }

    /// Newton's iteration count over the `(height, offset)` grid and a dense
    /// sweep of `u`, for every ellipse of at least the rect's floor, `1e-4`
    /// sr — below the disk's own band too, where the map is most eccentric.
    #[test]
    fn newton_converges_within_six_iterations_over_the_grid() {
        let (mut worst, mut total, mut calls) = (0, 0, 0);
        for i in 0..40 {
            for j in 0..40 {
                let h = 1e-2 * 1.25f64.powi(i);
                let d = 1e-2 * 1.25f64.powi(j);
                let Some(e) = EllipseMap::new(local(h, d)) else {
                    continue;
                };
                if !(1e-4..=MAX_SPHERICAL_RECT_SR).contains(&e.solid_angle()) {
                    continue;
                }
                for k in 0..512 {
                    let (_, it) = e.sample_counting((k as f64 + 0.5) / 512.0, 0.5);
                    worst = worst.max(it);
                    total += it;
                    calls += 1;
                }
            }
        }
        eprintln!(
            "Newton: at most {worst} iterations, {:.2} on average over {calls} samples",
            total as f64 / calls as f64
        );
        assert!(calls > 100_000, "{calls}");
        assert!(worst <= 6, "{worst} Newton iterations");
    }

    fn check_histogram(name: &str, e: &EllipseMap, from: Vec3A) {
        const N: usize = 256;
        const AZIMUTHS: usize = 8;
        const BINS: usize = 2 * AZIMUTHS;
        let o = DVec3::from(Vec3::from(from));
        // An inner and an outer ring, split at a quarter of the minor arc's
        // `1 − cos`, by eight azimuths around the ellipse's centre.
        let inner = 0.25 * e.b * e.b / (1.0 + (1.0 - e.b * e.b).sqrt());
        // Each cell's expected share comes from brute force, binning each
        // disk point the same way as the samples, weighted by its solid angle.
        let bin_of = |p: DVec3| -> usize {
            let dir = (p - o).normalize();
            let (x, y) = (dir.dot(e.x), dir.dot(e.y));
            let az = y.atan2(x).rem_euclid(2.0 * std::f64::consts::PI);
            let i = (((az / (2.0 * std::f64::consts::PI)) * AZIMUTHS as f64) as usize)
                .min(AZIMUTHS - 1);
            let ring = usize::from(1.0 - dir.dot(e.z) > inner);
            2 * i + ring
        };
        let mut expected = [0.0f64; BINS];
        {
            const NR: usize = 600;
            const NP: usize = 1200;
            let h = -o.z;
            for i in 0..NR {
                let r = (i as f64 + 0.5) / NR as f64;
                for j in 0..NP {
                    let phi = 2.0 * std::f64::consts::PI * (j as f64 + 0.5) / NP as f64;
                    let p = DVec3::new(r * phi.cos(), r * phi.sin(), 0.0);
                    let d2 = (p - o).length_squared();
                    expected[bin_of(p)] += h / (d2 * d2.sqrt()) * r;
                }
            }
            let total: f64 = expected.iter().sum();
            for x in &mut expected {
                *x /= total;
            }
        }
        let mut counts = [0usize; BINS];
        let mut worst_newton = 0;
        for i in 0..N {
            for j in 0..N {
                let u = (i as f64 + 0.5) / N as f64;
                let v = (j as f64 + 0.5) / N as f64;
                let (p, it) = e.sample_counting(u, v);
                worst_newton = worst_newton.max(it);
                assert!(
                    p.x * p.x + p.y * p.y <= 1.0 + 1e-12 && p.z == 0.0,
                    "{name}: {p}"
                );
                counts[bin_of(p)] += 1;
            }
        }
        let total = (N * N) as f64;
        for b in 0..BINS {
            let got = counts[b] as f64 / total;
            assert!(
                (got - expected[b]).abs() < 2e-3,
                "{name}: bin {b} got {got}, expected {}",
                expected[b]
            );
        }
        eprintln!("{name}: Newton took at most {worst_newton} iterations");
        assert!(
            worst_newton <= 6,
            "{name}: {worst_newton} Newton iterations"
        );
    }
}
