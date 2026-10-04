//! Analytic and curve geometry: `UsdGeomSphere` and `UsdGeomBasisCurves`.

use std::sync::Arc;

use crust_rt::{CubicCurveSegment, CurveSegment, Geometry, SceneBuilder as RtSceneBuilder};
use glam::{Affine3A, Mat4 as GMat4, Vec3, Vec3A};
use openusd::gf::Vec3f;
use openusd::sdf;
use openusd::usd::Prim;
use openusd_schemas::geom::{
    BasisCurves as UsdBasisCurves, Curves as UsdCurves, PointBased, Sphere as UsdSphere,
};
use tracing::{debug, warn};

use crate::material::Material;
use crate::rt_world::WorldBuilder;

use super::attrs::{custom_token, prim_motion_translate, prim_ray_mask};
use super::time::eval_time;

// -----------------------------------------------------------------------
// Sphere
// -----------------------------------------------------------------------

/// The authored `radius`, defaulting to USD's 1.0.
pub(super) fn sphere_radius(sphere: &UsdSphere) -> f32 {
    sphere
        .radius_attr()
        .get_at::<sdf::Value>(eval_time())
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::Double(d) => Some(d as f32),
            sdf::Value::Float(f) => Some(f),
            _ => None,
        })
        .unwrap_or(1.0)
}

pub(super) fn emit_sphere(
    world: &mut WorldBuilder,
    prim: &Prim,
    sphere: &UsdSphere,
    world_xf: GMat4,
    material: Arc<dyn Material>,
) {
    let radius = sphere_radius(sphere);
    let center_world = world_xf.transform_point3(Vec3::ZERO);
    let center = Vec3A::new(center_world.x, center_world.y, center_world.z);
    debug!(
        "Sphere at {} radius={} center={:?}",
        prim.path(),
        radius,
        center
    );
    let mask = prim_ray_mask(prim);
    match prim_motion_translate(prim) {
        // A moving sphere rides an identity-placed instance whose end
        // transform is the shutter translation.
        Some(v) => {
            let mut b = RtSceneBuilder::new();
            b.attach(Geometry::Sphere { center, radius });
            world.attach_masked(
                Geometry::Instance {
                    scene: Arc::new(b.commit_with(crate::commit_options())),
                    transform: Affine3A::IDENTITY,
                    transform_end: Some(Box::new(Affine3A::from_translation(v))),
                },
                material,
                mask,
            );
        }
        None => {
            world.attach_masked(Geometry::Sphere { center, radius }, material, mask);
        }
    }
}

// -----------------------------------------------------------------------
// Curves
// -----------------------------------------------------------------------

/// Cubic basis matrices (row-major, `[t³ t² t 1] · M · [P0 P1 P2 P3]ᵀ`).
const BEZIER_M: [[f32; 4]; 4] = [
    [-1.0, 3.0, -3.0, 1.0],
    [3.0, -6.0, 3.0, 0.0],
    [-3.0, 3.0, 0.0, 0.0],
    [1.0, 0.0, 0.0, 0.0],
];
const BSPLINE_M: [[f32; 4]; 4] = [
    [-1.0 / 6.0, 3.0 / 6.0, -3.0 / 6.0, 1.0 / 6.0],
    [3.0 / 6.0, -6.0 / 6.0, 3.0 / 6.0, 0.0],
    [-3.0 / 6.0, 0.0, 3.0 / 6.0, 0.0],
    [1.0 / 6.0, 4.0 / 6.0, 1.0 / 6.0, 0.0],
];
const CATMULL_ROM_M: [[f32; 4]; 4] = [
    [-0.5, 1.5, -1.5, 0.5],
    [1.0, -2.5, 2.0, -0.5],
    [-0.5, 0.0, 0.5, 0.0],
    [0.0, 1.0, 0.0, 0.0],
];

/// Only `basis_to_bezier`'s tests evaluate a basis matrix directly now —
/// production code always converts to Bézier form first.
#[cfg(test)]
fn eval_cubic(m: &[[f32; 4]; 4], cp: &[Vec3A; 4], t: f32) -> Vec3A {
    let pow = [t * t * t, t * t, t, 1.0];
    let mut p = Vec3A::ZERO;
    for (row, &w) in m.iter().zip(&pow) {
        for (c, &coeff) in row.iter().enumerate() {
            p += cp[c] * (coeff * w);
        }
    }
    p
}

/// Converts 4 control points from a cubic basis (`BEZIER_M`, `BSPLINE_M`,
/// `CATMULL_ROM_M`) to the equivalent standard (Bernstein) Bézier control
/// points tracing the *same* curve. Needed because the analytic curve
/// intersector (`crust_rt::curve::cubic_curve_intersect`) subdivides via
/// de Casteljau, which only has its usual convex-hull/subdivision
/// properties in Bézier form.
///
/// `eval_cubic`'s row `r` gives the curve's monomial coefficient of `t^(3-r)`
/// (row 0 → t³, …, row 3 → the constant term) as `Σ_c m[r][c]·cp[c]`.
/// Matching those same four coefficients against the expansion of the
/// Bernstein basis — `B(t) = P0(1-t)³ + 3P1·t(1-t)² + 3P2·t²(1-t) + P3·t³`
/// — inverts cleanly to: `P0 = d`, `P1 = d + c/3`, `P2 = d + 2c/3 + b/3`,
/// `P3 = a+b+c+d`, where `a,b,c,d` are those coefficients of
/// `t³,t²,t,1`. Passing `BEZIER_M` itself through this is the identity
/// (pinned by `bezier_basis_round_trips_unchanged`).
fn basis_to_bezier(m: &[[f32; 4]; 4], cp: &[Vec3A; 4]) -> [Vec3A; 4] {
    let coeff = |row: usize| -> Vec3A {
        let mut s = Vec3A::ZERO;
        for c in 0..4 {
            s += cp[c] * m[row][c];
        }
        s
    };
    let (a, b, c, d) = (coeff(0), coeff(1), coeff(2), coeff(3));
    let p0 = d;
    let p1 = d + c / 3.0;
    let p2 = d + c * (2.0 / 3.0) + b / 3.0;
    let p3 = a + b + c + d;
    [p0, p1, p2, p3]
}

/// The values a span's four per-vertex scalars take at its two ends, `t = 0`
/// and `t = 1`, under basis `m` — USD's `vertex` interpolation of a cubic
/// primvar runs through the curve's basis, like its points. A Bézier span
/// starts and ends on its first and last control points, so it reads `w[0]`
/// and `w[3]` exactly (the weights are `[1, 0, 0, 0]` and `[0, 0, 0, 1]`),
/// but a B-spline span starts at `(w0 + 4w1 + w2) / 6` and a Catmull-Rom one
/// at `w1`: neither passes through its end control points.
///
/// At `t = 0` only the constant row of `m` remains; at `t = 1` every row
/// contributes, so the weight of control point `c` is its column's sum.
fn span_end_values(m: &[[f32; 4]; 4], w: [f32; 4]) -> (f32, f32) {
    let at = |weights: [f32; 4]| -> f32 { (0..4).map(|c| weights[c] * w[c]).sum() };
    let start = m[3];
    let end = std::array::from_fn(|c| m[0][c] + m[1][c] + m[2][c] + m[3][c]);
    (at(start), at(end))
}

/// Import a `UsdGeomBasisCurves` batch as round (sphere-swept) curves:
/// `linear` curves directly as [`CurveSegment`]s, `cubic` curves (bezier /
/// bspline / catmullRom) as one [`CubicCurveSegment`] per span — its
/// control points converted to Bézier form (`basis_to_bezier`) and
/// intersected analytically (`crust_rt::curve::cubic_curve_intersect`)
/// rather than flattened to a polyline. A dense xgen-style archive (grass,
/// needles) attaches tens of millions of these; one primitive per
/// authored span instead of several flattened straight segments is the
/// difference between that fitting in memory and not.
///
/// Widths (diameters, per USD) may be authored per point (`vertex`), per
/// curve, or constant; anything else falls back to the first value. Both
/// vectors live in local space under an `Instance`, like meshes. Shared by
/// the top-level emitter and the prototype collector; the caller supplies
/// the placement.
pub(super) fn curve_segments(
    prim: &Prim,
    curves: &UsdBasisCurves,
) -> Option<(Vec<CurveSegment>, Vec<CubicCurveSegment>)> {
    let points: Option<Vec<Vec3f>> = curves
        .points_attr()
        .get_at::<sdf::Value>(eval_time())
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::Vec3fVec(v) => Some(v),
            _ => None,
        });
    let counts: Option<Vec<i32>> = curves
        .curve_vertex_counts_attr()
        .get_at::<sdf::Value>(eval_time())
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::IntVec(v) => Some(v),
            _ => None,
        });
    let (points, counts) = match (points, counts) {
        (Some(p), Some(c)) => (p, c),
        _ => {
            debug!(
                "BasisCurves at {} missing points / curveVertexCounts — skipped",
                prim.path()
            );
            return None;
        }
    };
    let pts: Vec<Vec3A> = points.iter().map(|p| Vec3A::new(p.x, p.y, p.z)).collect();

    let widths: Vec<f32> = curves
        .widths_attr()
        .get_at::<sdf::Value>(eval_time())
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::FloatVec(v) => Some(v),
            _ => None,
        })
        .unwrap_or_else(|| vec![1.0]);

    // USD defaults: type = cubic, basis = bezier.
    let ty = custom_token(prim, "type").unwrap_or_else(|| "cubic".to_string());
    let basis_name = custom_token(prim, "basis").unwrap_or_else(|| "bezier".to_string());
    let (basis, vstep) = match basis_name.as_str() {
        "bezier" => (&BEZIER_M, 3usize),
        "bspline" => (&BSPLINE_M, 1),
        "catmullRom" => (&CATMULL_ROM_M, 1),
        other => {
            warn!(
                "BasisCurves at {}: basis \"{}\" is not supported (bezier | bspline | catmullRom) — skipped",
                prim.path(),
                other
            );
            return None;
        }
    };

    // Width (diameter) of control point `global_idx` under the authored
    // interpolation, resolved structurally from the array length.
    let n_points = pts.len();
    let n_curves = counts.len();
    let width_of = |global_idx: usize, curve_idx: usize| -> f32 {
        if widths.len() == n_points {
            widths[global_idx] // vertex
        } else if widths.len() == n_curves {
            widths[curve_idx] // uniform (per curve)
        } else {
            widths[0] // constant / fallback
        }
    };

    let mut segments: Vec<CurveSegment> = Vec::new();
    let mut cubic_segments: Vec<CubicCurveSegment> = Vec::new();
    let mut offset = 0usize;
    for (curve_idx, &cnt) in counts.iter().enumerate() {
        let cnt = cnt as usize;
        if offset + cnt > pts.len() {
            warn!(
                "BasisCurves at {}: curveVertexCounts overruns points — remaining curves skipped",
                prim.path()
            );
            break;
        }
        let cp = &pts[offset..offset + cnt];
        let radius = |k: usize| 0.5 * width_of(offset + k, curve_idx).max(1e-6);

        if ty == "linear" {
            for k in 0..cnt.saturating_sub(1) {
                segments.push(CurveSegment {
                    p0: cp[k],
                    p1: cp[k + 1],
                    r0: radius(k),
                    r1: radius(k + 1),
                });
            }
        } else {
            // Cubic: one CubicCurveSegment per span. Span k uses control
            // points [k·vstep .. k·vstep+3], converted to Bézier form;
            // widths interpolate linearly over the curve parameter
            // between the widths at the span's two ends. Per-vertex widths
            // are evaluated there through the basis (`span_end_values`):
            // only a Bézier span ends on its end control points.
            if cnt < 4 {
                offset += cnt;
                continue;
            }
            let n_spans = (cnt - 4) / vstep + 1;
            for s in 0..n_spans {
                let base = s * vstep;
                let ctrl = [cp[base], cp[base + 1], cp[base + 2], cp[base + 3]];
                let (r0, r1) = if widths.len() == n_points {
                    let w = std::array::from_fn(|c| width_of(offset + base + c, curve_idx));
                    let (w0, w1) = span_end_values(basis, w);
                    (0.5 * w0.max(1e-6), 0.5 * w1.max(1e-6))
                } else {
                    (radius(base), radius(base + 3))
                };
                cubic_segments.push(CubicCurveSegment {
                    cp: basis_to_bezier(basis, &ctrl),
                    r0,
                    r1,
                });
            }
        }
        offset += cnt;
    }

    if segments.is_empty() && cubic_segments.is_empty() {
        debug!("BasisCurves at {} produced no segments", prim.path());
        return None;
    }
    debug!(
        "Imported BasisCurves at {} ({} {} curves, {} segments, {} cubic spans)",
        prim.path(),
        counts.len(),
        ty,
        segments.len(),
        cubic_segments.len()
    );
    Some((segments, cubic_segments))
}

pub(super) fn emit_curves(
    world: &mut WorldBuilder,
    prim: &Prim,
    curves: &UsdBasisCurves,
    world_xf: GMat4,
    material: Arc<dyn Material>,
) {
    let Some((segments, cubic_segments)) = curve_segments(prim, curves) else {
        return;
    };
    if world_xf.determinant().abs() < 1e-12 {
        warn!(
            "BasisCurves at {} has a non-invertible transform — skipped",
            prim.path()
        );
        return;
    }
    let mut b = RtSceneBuilder::new();
    if !segments.is_empty() {
        b.attach(Geometry::RoundCurves { segments });
    }
    if !cubic_segments.is_empty() {
        b.attach(Geometry::CubicCurves {
            segments: cubic_segments,
        });
    }
    world.attach_masked(
        Geometry::Instance {
            scene: Arc::new(b.commit_with(crate::commit_options())),
            transform: Affine3A::from_mat4(world_xf),
            transform_end: None,
        },
        material,
        prim_ray_mask(prim),
    );
}

#[cfg(test)]
mod curve_basis_tests {
    use super::*;

    #[test]
    fn bezier_basis_round_trips_unchanged() {
        let cp = [
            Vec3A::new(0.0, 0.0, 0.0),
            Vec3A::new(1.0, 2.0, 0.0),
            Vec3A::new(2.0, -1.0, 1.0),
            Vec3A::new(3.0, 0.0, 0.0),
        ];
        let out = basis_to_bezier(&BEZIER_M, &cp);
        for i in 0..4 {
            assert!(out[i].abs_diff_eq(cp[i], 1e-5), "index {i}: {out:?}");
        }
    }

    /// A span's end widths are the basis evaluated at its ends, not its end
    /// control points' widths, and a Bézier span's are those exactly — so
    /// every Bézier groom imports bit-identically to before.
    #[test]
    fn span_end_widths_follow_the_basis() {
        let w = [0.12, 0.10, 0.06, 0.03];
        assert_eq!(span_end_values(&BEZIER_M, w), (w[0], w[3]));

        // Evaluate the basis on the widths as a curve (x carries the width).
        let as_curve = w.map(|x| Vec3A::new(x, 0.0, 0.0));
        for basis in [&BSPLINE_M, &CATMULL_ROM_M] {
            let (w0, w1) = span_end_values(basis, w);
            assert!((w0 - eval_cubic(basis, &as_curve, 0.0).x).abs() < 1e-7);
            assert!((w1 - eval_cubic(basis, &as_curve, 1.0).x).abs() < 1e-7);
        }
        let (w0, w1) = span_end_values(&BSPLINE_M, w);
        assert!((w0 - (w[0] + 4.0 * w[1] + w[2]) / 6.0).abs() < 1e-7, "{w0}");
        assert!((w1 - (w[1] + 4.0 * w[2] + w[3]) / 6.0).abs() < 1e-7, "{w1}");
        assert_eq!(span_end_values(&CATMULL_ROM_M, w), (w[1], w[2]));
    }

    #[test]
    fn bspline_and_catmull_rom_convert_to_the_same_curve() {
        // The converted Bézier control points, evaluated via the standard
        // Bernstein formula, must trace exactly the curve the original
        // basis matrix evaluates directly — checked densely over the
        // span, not just at the endpoints.
        let cp = [
            Vec3A::new(0.0, 0.0, 0.0),
            Vec3A::new(1.0, 2.0, 0.5),
            Vec3A::new(2.0, -1.0, 1.0),
            Vec3A::new(3.5, 1.0, -0.5),
        ];
        for basis in [&BSPLINE_M, &CATMULL_ROM_M] {
            let bezier_cp = basis_to_bezier(basis, &cp);
            for i in 0..=10 {
                let t = i as f32 / 10.0;
                let direct = eval_cubic(basis, &cp, t);
                let via_bezier = eval_cubic(&BEZIER_M, &bezier_cp, t);
                assert!(
                    direct.abs_diff_eq(via_bezier, 1e-4),
                    "t={t}: direct={direct:?} via_bezier={via_bezier:?}"
                );
            }
        }
    }
}
