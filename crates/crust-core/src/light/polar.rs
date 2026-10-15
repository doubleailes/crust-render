//! The polar disk strategy (`CRUST_DISK_SAMPLING = polar`): a disk round in
//! world space, sampled in polar coordinates about the shading point's foot
//! on its plane.
//!
//! The azimuth `ψ` is uniform over the wedge of directions in which the disk
//! lies as seen from the foot (all of them when the foot is inside it). Along
//! each, the distance `ρ` from the foot is drawn from
//! `g(ρ) ∝ ρ (h² + ρ²)^(−(k+3)/2)`, which with `k = 0` is the solid-angle
//! measure `cos θl / r²` itself and with `k` the shaping's focus also follows
//! its `cos^k` fall-off. Its integral `(h² + ρ²)^(−(k+1)/2)` inverts in
//! closed form. The tube's equiangular draw is the same idea along a line.
//! With the cone clip, `ρ` stops at the cutoff radius about the same foot,
//! and the wedge narrows to the directions that reach the disk within it
//! (the lighting design record, "Cone-aware sampling").
//!
//! Everything is in f64 and free of cancellation, which a thin slice seen
//! from far away, or a point right under a small disk, would otherwise hit:
//! `ρ` is drawn as `s = ρ²` through `ln1p` / `expm1` of the slice's relative
//! width, and the near intersection outside the disk as the roots' product.

use glam::{Affine3A, DVec3, Mat3A, Vec3, Vec3A};

use super::shape::{AffineShape, SolidAngleSampler};
use crate::pdf::PdfSolidAngle;

/// How far from round a placed disk may be, as relative differences of its
/// in-plane axes' lengths and their cosine, for the polar strategy to apply.
/// An f32 similarity leaves a few parts in 10⁷; an authored squash is far
/// above it.
const ROUNDNESS: f64 = 1e-6;

/// Below this height over the disk's plane, as a fraction of its radius, a
/// point is area-sampled: nearly in the plane, the density's `1/h` blows up.
const MIN_HEIGHT: f64 = 1e-4;

/// A disk light round in world space: what the polar strategy needs of its
/// placement.
#[derive(Clone, Copy, Debug)]
pub(super) struct RoundDisk {
    center: DVec3,
    /// The emitting side's unit normal.
    normal: DVec3,
    /// A unit direction in the plane, for the azimuth's origin when the foot
    /// is the centre.
    e1: DVec3,
    radius: f64,
}

impl RoundDisk {
    /// The unit disk (local XY, emitting along −Z) under `m`, if `m` keeps it
    /// round. `normal_mat` is `m`'s normal transform.
    pub(super) fn of(m: &Affine3A, normal_mat: Mat3A) -> Option<Self> {
        let d = |v: Vec3A| DVec3::from(Vec3::from(v));
        let (c0, c1) = (d(m.matrix3.x_axis), d(m.matrix3.y_axis));
        let (l0, l1) = (c0.length(), c1.length());
        let round = l0 > 0.0
            && l1 > 0.0
            && ((l0 - l1) / l0).abs() <= ROUNDNESS
            && c0.dot(c1).abs() <= ROUNDNESS * l0 * l1;
        let normal = d(normal_mat * -Vec3A::Z).normalize_or_zero();
        (round && normal != DVec3::ZERO && (l0 * l1).is_finite()).then(|| Self {
            center: d(m.translation),
            normal,
            e1: c0 / l0,
            radius: 0.5 * (l0 + l1),
        })
    }
}

/// The disk as seen from one shading point, for the polar strategy.
#[derive(Clone, Copy)]
pub(super) struct PolarView {
    from: Vec3A,
    /// Height over the plane, on the emitting side.
    h: f64,
    /// Distance from the foot to the centre.
    dist: f64,
    /// The wedge's half-angle about the direction to the centre.
    half: f64,
    /// The cone's cutoff radius about the foot, `∞` without the clip.
    cap: f64,
    /// The focus exponent the radial density follows.
    k: f32,
}

/// The foot's frame: the foot, the unit direction from it to the centre (or
/// `e1` when it is the centre), and its in-plane perpendicular.
struct Frame {
    foot: DVec3,
    a: DVec3,
    b: DVec3,
}

impl PolarView {
    fn frame(&self, disk: &RoundDisk) -> Frame {
        let from = DVec3::from(Vec3::from(self.from));
        let foot = from - self.h * disk.normal;
        let to_center = disk.center - foot;
        let a = if self.dist > 0.0 {
            to_center / self.dist
        } else {
            disk.e1
        };
        Frame {
            foot,
            a,
            b: disk.normal.cross(a),
        }
    }

    /// The slice at azimuth `psi` (off the direction to the centre): the
    /// squared distances `(s1, s2)` from the foot where it enters and leaves
    /// the disk (capped), `s1 = 0` from inside.
    fn slice(&self, radius: f64, psi: f64) -> (f64, f64, f64, f64) {
        let (sin, cos) = psi.sin_cos();
        let (dist, r) = (self.dist, radius);
        // `R² − D² sin² ψ`, factored.
        let disc = ((r - dist * sin) * (r + dist * sin)).max(0.0);
        let rho2 = dist * cos + disc.sqrt();
        let rho1 = if dist < r {
            0.0
        } else {
            // The roots' product, `D² − R²`, not `D cos ψ − √disc`.
            ((dist - r) * (dist + r) / rho2).max(0.0)
        };
        let rho2 = rho2.min(self.cap).max(rho1);
        (rho1, rho2, rho1 * rho1, rho2 * rho2)
    }
}

impl AffineShape {
    /// The polar strategy from `from`: `None` where the disk is area-sampled
    /// instead (not round, `from` behind it or nearly in its plane), and the
    /// strategy that samples nothing where the cone's cutoff circle misses it.
    #[inline(never)]
    pub(super) fn polar_sampler(
        &self,
        disk: &RoundDisk,
        from: Vec3A,
    ) -> Option<SolidAngleSampler<'_>> {
        let f = DVec3::from(Vec3::from(from));
        let h = (f - disk.center).dot(disk.normal);
        if !(h > MIN_HEIGHT * disk.radius && h.is_finite()) {
            return None;
        }
        let foot = f - h * disk.normal;
        let dist = (disk.center - foot).length();
        let r = disk.radius;
        let cone = self.cone();
        let cap = match cone {
            Some(c) if self.cone_clip() => c.cutoff_radius(h).unwrap_or(f64::INFINITY),
            _ => f64::INFINITY,
        };
        if dist - r >= cap {
            return Some(SolidAngleSampler::nothing());
        }
        let half = if dist < r {
            std::f64::consts::PI
        } else {
            // The tangent length, and the wedge it bounds.
            let tangent = ((dist - r) * (dist + r)).sqrt();
            if cap < tangent {
                // Only the directions whose near point is within the cap:
                // where the circle of radius `cap` about the foot crosses the
                // disk's rim.
                ((dist * dist + cap * cap - r * r) / (2.0 * dist * cap))
                    .clamp(-1.0, 1.0)
                    .acos()
            } else {
                r.atan2(tangent)
            }
        };
        let k = cone.map_or(0.0, |c| c.focus());
        Some(SolidAngleSampler::polar(
            self,
            PolarView {
                from,
                h,
                dist,
                half,
                cap,
                k,
            },
        ))
    }

    /// A point of the polar strategy from two unit random numbers: the
    /// azimuth from `u`, the distance along it from `v`. Placed through the
    /// disk's local space, so it lies where an area sample would.
    #[inline(never)]
    pub(super) fn polar_point(&self, disk: &RoundDisk, view: PolarView, u: f32, v: f32) -> Vec3A {
        let frame = view.frame(disk);
        let psi = (2.0 * f64::from(u) - 1.0) * view.half;
        let (sin, cos) = psi.sin_cos();
        let w = cos * frame.a + sin * frame.b;
        let (_, _, s1, s2) = view.slice(disk.radius, psi);
        let h2 = view.h * view.h;
        // `(h² + s)^(−m) = (1 − v)(h² + s1)^(−m) + v (h² + s2)^(−m)`, solved
        // for `s` relative to `s1`.
        let m = 0.5 * (f64::from(view.k) + 1.0);
        let delta = (s2 - s1) / (h2 + s1);
        let y = f64::from(v) * (-m * delta.ln_1p()).exp_m1();
        let r_minus_1 = (-y.ln_1p() / m).exp_m1();
        let s = (s1 + (h2 + s1) * r_minus_1).clamp(s1, s2);
        let p = frame.foot + s.sqrt() * w;
        let local = self
            .world_to_light
            .transform_point3a(Vec3A::from(p.as_vec3()));
        self.light_to_world
            .transform_point3a(Vec3A::new(local.x, local.y, 0.0))
    }

    /// The polar strategy's solid-angle density at `p`, a point on the disk:
    /// `(k+1) d^−k / (h Δψ (G1 − G2))` with `G = d^−(k+1)` at the slice's ends,
    /// the slice through `p`. `None` beyond the cap, where the cone darkens
    /// the disk toward `from` and the strategy never draws.
    #[inline(never)]
    pub(super) fn polar_pdf(
        &self,
        disk: &RoundDisk,
        view: PolarView,
        p: Vec3A,
    ) -> Option<PdfSolidAngle> {
        let frame = view.frame(disk);
        let rel = DVec3::from(Vec3::from(p)) - frame.foot;
        let (x, y) = (rel.dot(frame.a), rel.dot(frame.b));
        let psi = y.atan2(x);
        // A point on the disk is in the wedge but for rounding at its edge;
        // past a capped wedge it is beyond the cap.
        let full_wedge = view.cap.is_infinite() || view.dist < disk.radius;
        if psi.abs() > view.half && !full_wedge {
            return None;
        }
        let psi = psi.clamp(-view.half, view.half);
        let (_, rho2, s1, s2) = view.slice(disk.radius, psi);
        let rho = x.hypot(y);
        if view.cap.is_finite() && rho2 >= view.cap && rho > view.cap * (1.0 + 1e-9) {
            return None;
        }
        let h2 = view.h * view.h;
        let k = f64::from(view.k);
        let delta = (s2 - s1) / (h2 + s1);
        // `G1 − G2 = d1^−(k+1) (1 − (d1/d2)^(k+1))`, the bracket by `expm1`.
        let d1 = (h2 + s1).sqrt();
        let g12 = d1.powf(-(k + 1.0)) * -(-0.5 * (k + 1.0) * delta.ln_1p()).exp_m1();
        let d = (h2 + rho * rho).sqrt();
        let pdf = (k + 1.0) * d.powf(-k) / (view.h * 2.0 * view.half * g12);
        PdfSolidAngle::new(pdf as f32)
    }
}
