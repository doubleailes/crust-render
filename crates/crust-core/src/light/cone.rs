//! [`ConeCut`]: what a flat light's shape needs to know about its emitter's
//! `ShapingAPI` to sample only where the light emits.
//!
//! For a light whose shaping axis is its emitting normal, a point `p` on the
//! light emits toward a receiver `x` at height `h` in front of the light's
//! plane exactly when `p` is within `h · tan θc` of `x`'s foot on that plane:
//! the direction `x − p` makes the angle `atan(|p − foot| / h)` with the axis.
//! Everything the shaping does except IES depends on that ratio alone, so the
//! region a receiver sees lit is a circle about its foot, and the focus
//! fall-off is radial about the same point (the lighting design record,
//! "Cone-aware sampling").

use glam::Vec3A;

use crate::lux::Shaping;

/// How far below `cos θc` (in f32) a direction's cosine to the axis may be
/// rounded and still pass `Shaping::factor`'s cutoff: its `normalize`, its
/// dot product and its `acos` each round, by an ulp or so of a cosine near 1
/// (`6e-8`). The margin is taken in cosine, not as a relative inflation of
/// `tan θc`: an `acos` near a small angle `θ` errs by about `ε / sin θ`, which
/// relative to `θ` grows as `1 / θ²` and exceeds any fixed fraction for a
/// narrow enough cone.
const COS_SLACK: f64 = 1e-6;

/// How far from parallel the shaping axis and the light's normal may be, as
/// one minus the cosine between them, for the cutoff region to be a circle
/// about the receiver's foot. An unsheared light's axis and normal come out
/// of an f32 transform parallel to a few parts in 10⁷; a tilted axis is off
/// by orders of magnitude more.
const AXIS_ALIGNMENT: f64 = 1e-6;

/// The part of a light's shaping its sampling uses. Built only for a shaping
/// that restricts or weights where the light emits: a cone under 90°, or a
/// focus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConeCut {
    /// The shaping's axis, the direction the light emits along, unit length.
    axis: Vec3A,
    /// `tan` of the conservative cutoff angle, `None` for a cone of 90° or
    /// wider, which cuts nothing off in front of the light.
    tan_cutoff: Option<f64>,
    /// `inputs:shaping:focus`, `0` for none.
    focus: f32,
}

impl ConeCut {
    /// The cut of `shaping`, `None` where there is nothing to cut or weight.
    pub fn of(shaping: &Shaping) -> Option<Self> {
        // The same f32 angle `Shaping::factor` compares against.
        let theta = shaping.cone_angle_deg.clamp(0.0, 180.0).to_radians() as f64;
        let cos_limit = theta.cos() - COS_SLACK;
        let tan_cutoff = (cos_limit > 0.0).then(|| cos_limit.acos().tan());
        let focus = if shaping.focus > 0.0 {
            shaping.focus
        } else {
            0.0
        };
        (tan_cutoff.is_some() || focus > 0.0).then_some(Self {
            axis: shaping.axis(),
            tan_cutoff,
            focus,
        })
    }

    /// Whether the shaping's axis is the light's unit emitting `normal`: the
    /// condition for the cutoff region to be a circle about the foot.
    pub fn is_along(&self, normal: Vec3A) -> bool {
        let dot = f64::from(self.axis.x) * f64::from(normal.x)
            + f64::from(self.axis.y) * f64::from(normal.y)
            + f64::from(self.axis.z) * f64::from(normal.z);
        dot >= 1.0 - AXIS_ALIGNMENT
    }

    /// The radius, on the light's plane, of the circle about the foot of a
    /// receiver `h` in front of it outside which the light emits nothing
    /// toward it. Conservative: every point the shaping lights at all is
    /// inside. `None` for a cone of 90° or wider.
    pub fn cutoff_radius(&self, h: f64) -> Option<f64> {
        self.tan_cutoff.map(|t| h * t)
    }

    /// The shaping's focus exponent, `0` for none.
    pub fn focus(&self) -> f32 {
        self.focus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Mat3A;

    /// A light at the origin emitting along −Z (its local frame is the world).
    fn shaping(cone_deg: f32, softness: f32, focus: f32) -> Shaping {
        let mut s = Shaping::new(Mat3A::IDENTITY);
        s.cone_angle_deg = cone_deg;
        s.cone_softness = softness;
        s.focus = focus;
        s
    }

    /// The cutoff is conservative, in numbers: from receivers at heights
    /// over five orders of magnitude, every light point just outside
    /// `cutoff_radius` gets exactly zero from `Shaping::factor`, for cones
    /// from 0.5° to 85°, with and without softness and focus. And it is not
    /// loose: a point 0.1% inside the true radius is still lit by a hard cone.
    #[test]
    fn nothing_outside_the_cutoff_radius_is_lit() {
        for cone in [0.5_f32, 1.0, 5.0, 10.0, 18.0, 30.0, 45.0, 60.0, 80.0, 85.0] {
            for (softness, focus) in [(0.0, 0.0), (0.4, 0.0), (0.0, 2.0), (1.0, 8.0)] {
                let s = shaping(cone, softness, focus);
                let cut = ConeCut::of(&s).expect("a cone under 90° cuts");
                for h in [1e-3_f32, 0.01, 0.1, 1.0, 10.0, 100.0] {
                    let rc = cut.cutoff_radius(f64::from(h)).unwrap();
                    for k in 0..64 {
                        let phi = k as f32 * std::f32::consts::TAU / 64.0;
                        let (sin, cos) = phi.sin_cos();
                        // The receiver at (0, 0, −h); the light point on z = 0.
                        let x = Vec3A::new(0.0, 0.0, -h);
                        for step in [1.0 + 1e-6, 1.0 + 1e-4, 1.01, 1.5] {
                            let r = (rc * step) as f32;
                            let p = Vec3A::new(r * cos, r * sin, 0.0);
                            let f = s.factor((x - p).normalize());
                            assert_eq!(
                                f,
                                Vec3A::ZERO,
                                "cone {cone}°, softness {softness}, focus {focus}, h {h}, \
                                 {step}× the cutoff radius, azimuth {k}"
                            );
                        }
                        let true_radius = f64::from(h) * f64::from(cone).to_radians().tan();
                        if cone >= 1.0 {
                            assert!(
                                rc <= true_radius * 1.01,
                                "cone {cone}°: loose by {}",
                                rc / true_radius
                            );
                        }
                        // A hard cone 1% inside its edge is lit. Closer than
                        // that, f32's `acos` in `factor` decides a narrow cone
                        // either way, which is the factor's precision, not
                        // the cut's.
                        if softness == 0.0 && cone >= 5.0 {
                            let r = (true_radius * 0.99) as f32;
                            let p = Vec3A::new(r * cos, r * sin, 0.0);
                            assert!(s.factor((x - p).normalize()).max_element() > 0.0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_wide_cone_cuts_nothing_and_focus_alone_still_weights() {
        assert_eq!(ConeCut::of(&shaping(90.0, 0.0, 0.0)), None);
        assert_eq!(ConeCut::of(&shaping(180.0, 0.0, 0.0)), None);
        let focused = ConeCut::of(&shaping(180.0, 0.0, 3.0)).expect("focus weights");
        assert_eq!(focused.cutoff_radius(1.0), None);
        assert_eq!(focused.focus(), 3.0);
    }

    #[test]
    fn the_axis_must_be_the_normal() {
        let cut = ConeCut::of(&shaping(30.0, 0.0, 0.0)).unwrap();
        assert!(cut.is_along(-Vec3A::Z));
        assert!(!cut.is_along(Vec3A::Z), "the back of the light");
        let tilted = Vec3A::new(0.01, 0.0, -1.0).normalize();
        assert!(!cut.is_along(tilted), "a 0.6° tilt is not along");
    }
}
