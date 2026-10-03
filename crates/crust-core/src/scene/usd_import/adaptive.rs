//! Adaptive subdivision: the level a subdivision mesh is refined to, chosen from
//! how large its cage edges are on screen.
//!
//! Every quantity here errs toward *more* detail: the stretch of a transform is
//! its exact spectral norm (the largest factor it can lengthen an edge by), the
//! distance is to the nearest point of the placement's bounds, and levels and
//! edge rates round up. See "Adaptive level" in
//! `openspec/specs/usd-scene-import/design.md`.

use glam::{DMat3, Mat3, Mat4 as GMat4, Vec3};
use openusd::gf::Vec3f;

/// Below this a distance, a stretch or an edge length is treated as zero.
const EPS: f32 = 1e-6;

/// The render camera's projection, the target and the ceiling: everything the
/// level of a placement depends on besides the placement itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ScreenRate {
    /// The camera position at shutter open.
    pub(super) eye: Vec3,
    /// Pixels per world unit at unit distance: `image height / (2 tan(vfov / 2))`.
    pub(super) f_px: f32,
    /// The target projected cage-edge length, in pixels. Positive and finite.
    pub(super) target: f32,
    /// The ceiling on the level.
    pub(super) max: u32,
    /// The view pyramid: geometry wholly outside it is not refined.
    /// `None` under `CRUST_ADAPTIVE_FRUSTUM=0`.
    pub(super) frustum: Option<Frustum>,
}

/// The render camera's view pyramid, as planes through the eye with inward
/// normals: the four sides, and the eye plane facing forward. The sides alone
/// are not enough for a box test — they all meet at the eye, so a box behind
/// it can pass each side with a different corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Frustum {
    eye: Vec3,
    normals: [Vec3; 5],
}

impl Frustum {
    /// The pyramid of a camera at `eye` looking along `forward` with `up`,
    /// whose half fields of view have tangents `tan_v` and `tan_h`.
    pub(super) fn new(eye: Vec3, forward: Vec3, up: Vec3, tan_v: f32, tan_h: f32) -> Frustum {
        let right = forward.cross(up).normalize();
        let up = right.cross(forward).normalize();
        // Inside is `|d·right| ≤ tan_h · d·forward` and the same with `up`.
        Frustum {
            eye,
            normals: [
                tan_h * forward - right,
                tan_h * forward + right,
                tan_v * forward - up,
                tan_v * forward + up,
                forward,
            ],
        }
    }

    /// Whether `b` can overlap the pyramid. Conservative: a box that crosses a
    /// plane's corner region may be reported in view, never the reverse.
    pub(super) fn overlaps(&self, b: &Aabb) -> bool {
        self.normals.iter().all(|n| {
            // The box corner furthest along the inward normal.
            let p = Vec3::new(
                if n.x >= 0.0 { b.max.x } else { b.min.x },
                if n.y >= 0.0 { b.max.y } else { b.min.y },
                if n.z >= 0.0 { b.max.z } else { b.min.z },
            );
            n.dot(p - self.eye) >= 0.0
        })
    }
}

/// How a mesh's culling boxes account for displacement.
///
/// A displaced point lies within its undisplaced local box grown by the
/// displacement bound on every axis, so growing the **local** box before it is
/// carried to world bounds the displaced geometry exactly, whatever the
/// placement's scale or rotation — no separate `max_axis_scale` factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Cull {
    /// Grow local boxes by this many local units (0: undisplaced, untouched).
    Pad(f32),
    /// A displaced mesh with no known bound: never treated as out of view.
    Off,
}

impl Cull {
    /// `bounds`, grown as this says — or `None` when the frustum test is off.
    fn padded(self, bounds: &Aabb) -> Option<Aabb> {
        match self {
            // Bit for bit the undisplaced box: adding 0.0 would turn a -0.0
            // corner into +0.0.
            Cull::Pad(0.0) => Some(*bounds),
            Cull::Pad(p) => Some(Aabb {
                min: bounds.min - Vec3::splat(p),
                max: bounds.max + Vec3::splat(p),
            }),
            Cull::Off => None,
        }
    }
}

impl ScreenRate {
    /// Pixels per local unit of a placement: its stretch over its distance to
    /// the camera. Infinite when the camera is inside `bounds` (in the
    /// placement's local frame, carried to world by `xf`).
    #[cfg(test)]
    pub(super) fn sigma(&self, xf: &GMat4, bounds: &Aabb) -> f32 {
        self.sigma_culled(xf, bounds, Cull::Pad(0.0))
    }

    /// [`ScreenRate::sigma`] for a mesh displaced under `cull`: the frustum
    /// test and the nearest-point distance both use the grown box.
    pub(super) fn sigma_culled(&self, xf: &GMat4, bounds: &Aabb, cull: Cull) -> f32 {
        let Some(bounds) = cull.padded(bounds) else {
            return self.sigma_unculled(xf, bounds);
        };
        let world = bounds.transformed(xf);
        if self.frustum.is_some_and(|f| !f.overlaps(&world)) {
            return 0.0;
        }
        let d = world.distance_to(self.eye);
        if d < EPS {
            return f32::INFINITY;
        }
        stretch(&Mat3::from_mat4(*xf)) * self.f_px / d
    }

    /// The smallest level at which a mean cage edge of `edge` local units,
    /// seen at `sigma` pixels per unit, projects to at most the target,
    /// clamped to `0..=max`.
    pub(super) fn level(&self, edge: f32, sigma: f32) -> u32 {
        let ratio = edge * sigma / self.target;
        if ratio.is_nan() {
            return 0;
        }
        if ratio == f32::INFINITY {
            return self.max;
        }
        if ratio <= 1.0 {
            return 0;
        }
        // Each level halves the edge, so `2^L >= ratio`. A float cast
        // saturates, so a huge ratio lands on the clamp rather than wrapping.
        (ratio.log2().ceil() as u32).min(self.max)
    }

    /// A cage segment's projected length over the target, for per-face
    /// tessellation of a direct mesh at `xf`: its chord (the first two of
    /// `points`, local units) stretched by `xf`, seen at the nearest point of
    /// the box around all of `points` carried to world. Infinite when the
    /// camera is inside that box.
    #[cfg(test)]
    pub(super) fn segment_at(&self, xf: &GMat4, points: &[[f32; 3]]) -> f32 {
        self.segment_culled(xf, points, Cull::Pad(0.0))
    }

    /// [`ScreenRate::segment_at`] for a mesh displaced under `cull`: the
    /// segment's box is grown by the displacement bound before the frustum
    /// test, and a mesh with no bound is never culled.
    pub(super) fn segment_culled(&self, xf: &GMat4, points: &[[f32; 3]], cull: Cull) -> f32 {
        let Some(bounds) = Aabb::of_arrays(points) else {
            return 0.0;
        };
        // A segment wholly out of view is split once. Its box is padded by its
        // own diagonal first, as MoonRay pads a face's, so the limit curve —
        // which strays off its chord — is not culled on the frustum's edge.
        if let Some(f) = self.frustum
            && let Some(displaced) = cull.padded(&bounds)
        {
            let world = displaced.transformed(xf);
            let pad = Vec3::splat((world.max - world.min).length());
            let padded = Aabb {
                min: world.min - pad,
                max: world.max + pad,
            };
            if !f.overlaps(&padded) {
                return 0.0;
            }
        }
        self.segment_with_sigma(self.sigma_unculled(xf, &bounds), points)
    }

    /// [`ScreenRate::sigma`] without the frustum test, for a caller that has
    /// made its own.
    fn sigma_unculled(&self, xf: &GMat4, bounds: &Aabb) -> f32 {
        let d = bounds.transformed(xf).distance_to(self.eye);
        if d < EPS {
            return f32::INFINITY;
        }
        stretch(&Mat3::from_mat4(*xf)) * self.f_px / d
    }

    /// A cage segment's projected length over the target at `sigma` pixels
    /// per local unit: inside a prototype version, `2^q` times the part's
    /// stretch.
    pub(super) fn segment_with_sigma(&self, sigma: f32, points: &[[f32; 3]]) -> f32 {
        let chord = match points {
            [a, b, ..] => Vec3::from_array(*a).distance(Vec3::from_array(*b)),
            _ => 0.0,
        };
        let r = chord * sigma / self.target;
        if r.is_nan() { 0.0 } else { r }
    }
}

/// An axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Aabb {
    pub(super) min: Vec3,
    pub(super) max: Vec3,
}

impl Aabb {
    /// The box around `points`, `None` when there are none.
    pub(super) fn of_points(points: &[Vec3f]) -> Option<Aabb> {
        let mut it = points.iter().map(|p| Vec3::new(p.x, p.y, p.z));
        let first = it.next()?;
        Some(it.fold(
            Aabb {
                min: first,
                max: first,
            },
            |b, p| Aabb {
                min: b.min.min(p),
                max: b.max.max(p),
            },
        ))
    }

    /// The box around `points`, `None` when there are none.
    pub(super) fn of_arrays(points: &[[f32; 3]]) -> Option<Aabb> {
        let mut it = points.iter().map(|&p| Vec3::from_array(p));
        let first = it.next()?;
        Some(it.fold(
            Aabb {
                min: first,
                max: first,
            },
            |b, p| Aabb {
                min: b.min.min(p),
                max: b.max.max(p),
            },
        ))
    }

    /// The box around this one's eight corners carried by `xf`.
    pub(super) fn transformed(&self, xf: &GMat4) -> Aabb {
        let corner = |i: u32| {
            Vec3::new(
                if i & 1 == 0 { self.min.x } else { self.max.x },
                if i & 2 == 0 { self.min.y } else { self.max.y },
                if i & 4 == 0 { self.min.z } else { self.max.z },
            )
        };
        let first = xf.transform_point3(corner(0));
        (1..8).fold(
            Aabb {
                min: first,
                max: first,
            },
            |b, i| {
                let p = xf.transform_point3(corner(i));
                Aabb {
                    min: b.min.min(p),
                    max: b.max.max(p),
                }
            },
        )
    }

    /// The distance from `p` to the nearest point of the box, 0 inside.
    pub(super) fn distance_to(&self, p: Vec3) -> f32 {
        (self.min - p).max(p - self.max).max(Vec3::ZERO).length()
    }
}

/// The mean length of a cage's face edges, in its own units, floored at a tiny
/// positive length. An edge shared by two faces counts twice, which weights it
/// like the faces it borders. Entries that do not index a point are skipped.
pub(super) fn mean_edge_length(points: &[Vec3f], counts: &[i32], indices: &[i32]) -> f32 {
    let point = |i: i32| {
        usize::try_from(i)
            .ok()
            .and_then(|i| points.get(i))
            .map(|p| Vec3::new(p.x, p.y, p.z))
    };
    let (mut sum, mut n) = (0.0f64, 0u64);
    let mut start = 0usize;
    for &count in counts {
        let Ok(count) = usize::try_from(count) else {
            break;
        };
        let Some(face) = indices.get(start..start + count) else {
            break;
        };
        start += count;
        for (k, &a) in face.iter().enumerate() {
            let b = face[(k + 1) % count];
            if let (Some(a), Some(b)) = (point(a), point(b)) {
                sum += f64::from(a.distance(b));
                n += 1;
            }
        }
    }
    if n == 0 {
        return EPS;
    }
    ((sum / n as f64) as f32).max(EPS)
}

/// The largest factor by which `m` can lengthen a vector: its spectral norm,
/// the square root of the largest eigenvalue of `mᵀm`.
///
/// Not the largest column norm, which equals it only when the columns are
/// orthogonal (a scale applied before a rotation). A scale applied *after* a
/// rotation, or a shear, stretches some direction further than any column, and
/// a column-norm estimate would then refine too little. Unlike the column norm
/// this one is submultiplicative, so the stretch of a nested placement is
/// bounded by the product of its levels'.
pub(super) fn stretch(m: &Mat3) -> f32 {
    let a = DMat3::from_cols(
        m.x_axis.as_dvec3(),
        m.y_axis.as_dvec3(),
        m.z_axis.as_dvec3(),
    );
    let b = a.transpose() * a;
    // Closed-form eigenvalues of a symmetric 3×3 (Smith, 1961).
    let (b00, b11, b22) = (b.x_axis.x, b.y_axis.y, b.z_axis.z);
    let (b01, b02, b12) = (b.y_axis.x, b.z_axis.x, b.z_axis.y);
    let p1 = b01 * b01 + b02 * b02 + b12 * b12;
    let q = (b00 + b11 + b22) / 3.0;
    let p2 = (b00 - q).powi(2) + (b11 - q).powi(2) + (b22 - q).powi(2) + 2.0 * p1;
    let p = (p2 / 6.0).sqrt();
    let largest = if p <= q * 1e-12 {
        // A multiple of the identity: every direction is stretched alike.
        q
    } else {
        let c = (b - DMat3::from_diagonal(glam::DVec3::splat(q))) * (1.0 / p);
        let r = (c.determinant() / 2.0).clamp(-1.0, 1.0);
        q + 2.0 * p * (r.acos() / 3.0).cos()
    };
    largest.max(0.0).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    fn rate() -> ScreenRate {
        ScreenRate {
            eye: Vec3::ZERO,
            f_px: 1000.0,
            target: 2.0,
            max: 3,
            frustum: None,
        }
    }

    /// A camera at the origin looking down −Z, ±45° both ways.
    fn frustum() -> Frustum {
        Frustum::new(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y, 1.0, 1.0)
    }

    fn unit_box_at(c: Vec3) -> Aabb {
        Aabb {
            min: c - 0.5,
            max: c + 0.5,
        }
    }

    #[test]
    fn the_frustum_keeps_what_is_in_view() {
        let f = frustum();
        assert!(
            f.overlaps(&unit_box_at(Vec3::new(0.0, 0.0, -10.0))),
            "dead ahead"
        );
        assert!(
            f.overlaps(&unit_box_at(Vec3::new(9.8, 0.0, -10.0))),
            "at the edge"
        );
        assert!(
            !f.overlaps(&unit_box_at(Vec3::new(20.0, 0.0, -10.0))),
            "beside"
        );
        assert!(
            !f.overlaps(&unit_box_at(Vec3::new(0.0, -20.0, -10.0))),
            "below"
        );
        assert!(
            !f.overlaps(&unit_box_at(Vec3::new(0.0, 0.0, 10.0))),
            "behind"
        );
        assert!(f.overlaps(&unit_box_at(Vec3::ZERO)), "around the camera");
    }

    #[test]
    fn out_of_view_geometry_is_not_refined() {
        let r = ScreenRate {
            frustum: Some(frustum()),
            ..rate()
        };
        let behind = GMat4::from_translation(Vec3::new(0.0, 0.0, 3.0));
        let ahead = GMat4::from_translation(Vec3::new(0.0, 0.0, -3.0));
        let b = unit_box_at(Vec3::ZERO);
        assert_eq!(r.level(0.25, r.sigma(&behind, &b)), 0);
        assert!(r.level(0.25, r.sigma(&ahead, &b)) > 0);
        let seg = [[0.0, 0.0, 0.0], [0.25, 0.0, 0.0]];
        assert_eq!(r.segment_at(&behind, &seg), 0.0);
        assert!(r.segment_at(&ahead, &seg) > 1.0);
        // Without the frustum the one behind is as fine as the one ahead.
        assert!(rate().segment_at(&behind, &seg) > 1.0);
    }

    /// "Displaced into view": a segment just beside the view, displaced by
    /// up to its bound, is in view once its box grows by the bound — so it is
    /// diced at its screen rate rather than split once.
    #[test]
    fn a_displacement_bound_brings_geometry_into_view() {
        let r = ScreenRate {
            frustum: Some(frustum()),
            ..rate()
        };
        let id = GMat4::IDENTITY;
        // At z = −10 the view reaches x = 10; this segment starts at 11.
        let seg = [[11.0, 0.0, -10.0], [11.25, 0.0, -10.0]];
        assert_eq!(
            r.segment_culled(&id, &seg, Cull::Pad(0.0)),
            0.0,
            "out of view"
        );
        let rated = r.segment_culled(&id, &seg, Cull::Pad(2.0));
        assert_eq!(rated, rate().segment_at(&id, &seg), "diced as if in view");
        assert!(rated > 1.0);
        // The per-mesh path: the same box, padded, is in view.
        let b = Aabb {
            min: Vec3::new(11.0, -0.5, -10.5),
            max: Vec3::new(12.0, 0.5, -9.5),
        };
        assert_eq!(r.sigma_culled(&id, &b, Cull::Pad(0.0)), 0.0);
        assert!(r.sigma_culled(&id, &b, Cull::Pad(2.0)) > 0.0);
    }

    /// "No bound known": the frustum term is off, so nothing is out of view.
    #[test]
    fn no_bound_turns_the_frustum_off() {
        let r = ScreenRate {
            frustum: Some(frustum()),
            ..rate()
        };
        let behind = GMat4::from_translation(Vec3::new(0.0, 0.0, 3.0));
        let seg = [[0.0, 0.0, 0.0], [0.25, 0.0, 0.0]];
        assert_eq!(
            r.segment_culled(&behind, &seg, Cull::Off),
            rate().segment_at(&behind, &seg)
        );
        let b = unit_box_at(Vec3::ZERO);
        assert_eq!(
            r.sigma_culled(&behind, &b, Cull::Off),
            rate().sigma(&behind, &b)
        );
    }

    /// An undisplaced mesh (`Pad(0)`) is rated exactly as before, bit for bit.
    #[test]
    fn a_zero_pad_keeps_every_rate() {
        let r = ScreenRate {
            frustum: Some(frustum()),
            ..rate()
        };
        for k in 0..200 {
            let f = k as f32 * 0.731;
            let xf = GMat4::from_scale_rotation_translation(
                Vec3::splat(0.5 + f.fract()),
                Quat::from_rotation_y(f),
                Vec3::new(f.sin() * 12.0, f.cos() * 3.0, -f % 15.0),
            );
            let seg = [[-0.0, 0.0, -0.0], [0.3, f.fract(), -0.0]];
            assert_eq!(
                r.segment_culled(&xf, &seg, Cull::Pad(0.0)).to_bits(),
                r.segment_at(&xf, &seg).to_bits()
            );
        }
    }

    /// A unit cube centred `z` units in front of the camera.
    fn cube_at(z: f32) -> (GMat4, Aabb) {
        let b = Aabb {
            min: Vec3::splat(-0.5),
            max: Vec3::splat(0.5),
        };
        (GMat4::from_translation(Vec3::new(0.0, 0.0, -z)), b)
    }

    #[test]
    fn the_level_falls_with_distance() {
        let r = rate();
        let mut last = u32::MAX;
        for z in [1.0, 3.0, 10.0, 30.0, 100.0, 300.0, 1000.0] {
            let (xf, b) = cube_at(z);
            let level = r.level(0.25, r.sigma(&xf, &b));
            assert!(
                level <= last,
                "level rose from {last} to {level} at z = {z}"
            );
            last = level;
        }
        assert_eq!(last, 0, "far enough away a cage needs no refinement");
    }

    #[test]
    fn the_level_rises_with_scale() {
        let r = rate();
        let mut last = 0;
        for s in [0.01, 0.1, 1.0, 10.0] {
            let xf = GMat4::from_scale_rotation_translation(
                Vec3::splat(s),
                Quat::IDENTITY,
                Vec3::new(0.0, 0.0, -100.0),
            );
            let b = Aabb {
                min: Vec3::splat(-0.5),
                max: Vec3::splat(0.5),
            };
            let level = r.level(0.25, r.sigma(&xf, &b));
            assert!(
                level >= last,
                "level fell from {last} to {level} at scale {s}"
            );
            last = level;
        }
    }

    #[test]
    fn a_camera_inside_the_bounds_gives_the_maximum() {
        let r = rate();
        let (xf, b) = cube_at(0.0);
        let sigma = r.sigma(&xf, &b);
        assert_eq!(sigma, f32::INFINITY);
        assert_eq!(r.level(1e-6, sigma), r.max);
    }

    #[test]
    fn the_level_is_clamped_at_both_ends() {
        let r = rate();
        assert_eq!(r.level(1.0, 1.0), 0, "half the target");
        assert_eq!(r.level(2.0, 1.0), 0, "exactly the target");
        assert_eq!(r.level(2.0001, 1.0), 1, "just over rounds up");
        assert_eq!(r.level(8.0, 1.0), 2);
        assert_eq!(r.level(1e30, 1.0), r.max, "clamped above");
        assert_eq!(r.level(f32::NAN, 1.0), 0);
    }

    #[test]
    fn stretch_is_the_spectral_norm() {
        let close = |a: f32, b: f32| (a - b).abs() <= 1e-5 * b.max(1.0);
        assert!(close(stretch(&Mat3::IDENTITY), 1.0));
        assert!(close(
            stretch(&Mat3::from_diagonal(Vec3::new(2.0, 3.0, 0.5))),
            3.0
        ));
        let rot = Mat3::from_quat(Quat::from_rotation_z(0.7) * Quat::from_rotation_x(0.3));
        assert!(
            close(stretch(&(rot * 4.0)), 4.0),
            "rotation and uniform scale"
        );
        // A scale *after* a 45° rotation: no column is stretched by 2, but the
        // direction (1, 1)/√2 is. The largest column norm would say √2.5.
        let m = Mat3::from_diagonal(Vec3::new(2.0, 1.0, 1.0))
            * Mat3::from_rotation_z(std::f32::consts::FRAC_PI_4);
        let col = m
            .x_axis
            .length()
            .max(m.y_axis.length())
            .max(m.z_axis.length());
        assert!(col < 1.6);
        assert!(close(stretch(&m), 2.0));
        // A shear.
        let shear = Mat3::from_cols(Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Z);
        let golden = (1.0 + 5f32.sqrt()) / 2.0;
        assert!(close(stretch(&shear), golden));
    }

    #[test]
    fn the_mean_edge_of_a_unit_quad_is_one() {
        let p = |x, y| Vec3f { x, y, z: 0.0 };
        let points = [p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)];
        assert!((mean_edge_length(&points, &[4], &[0, 1, 2, 3]) - 1.0).abs() < 1e-6);
        assert!(
            mean_edge_length(&points, &[], &[]) > 0.0,
            "floored, never zero"
        );
        assert!(
            (mean_edge_length(&points, &[4], &[0, 1, 2, 9]) - 1.0).abs() < 1e-6,
            "an out-of-range index drops its two edges"
        );
    }

    #[test]
    fn the_box_distance_is_zero_inside_and_to_the_nearest_face_outside() {
        let b = Aabb {
            min: Vec3::ZERO,
            max: Vec3::ONE,
        };
        assert_eq!(b.distance_to(Vec3::splat(0.5)), 0.0);
        assert!((b.distance_to(Vec3::new(3.0, 0.5, 0.5)) - 2.0).abs() < 1e-6);
        assert!((b.distance_to(Vec3::new(2.0, 2.0, 0.5)) - 2f32.sqrt()).abs() < 1e-6);
    }
}
