//! Analytic and curve geometry: `UsdGeomSphere`, `UsdGeomBasisCurves` and
//! `UsdGeomNurbsCurves`.

use crate::warning;
use std::sync::Arc;

use crust_rt::{CubicCurveSegment, CurveSegment, Geometry, SceneBuilder as RtSceneBuilder};
use glam::{Affine3A, Mat4 as GMat4, Vec3, Vec3A};
use openusd::sdf;
use openusd::usd::Prim;
use openusd_schemas::geom::{
    BasisCurves as UsdBasisCurves, CurvesSchema, PointBasedSchema, Sphere as UsdSphere,
    SphereSchema,
};
use tracing::debug;

use crate::material::Material;
use crate::rt_world::WorldBuilder;

use super::attrs::{
    attr_f32, custom_token, decode_f32_array, decode_f64_array, decode_i32_array,
    decode_vec3f_array, prim_motion_translate, prim_ray_mask, prim_value, value_at,
};

// -----------------------------------------------------------------------
// Sphere
// -----------------------------------------------------------------------

/// The authored `radius`, defaulting to USD's 1.0.
pub(super) fn sphere_radius(sphere: &UsdSphere) -> f32 {
    attr_f32(&sphere.radius_attr()).unwrap_or(1.0)
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
pub(super) fn curve_segments(prim: &Prim, curves: &UsdBasisCurves) -> Option<CurveBatches> {
    let points = value_at(&curves.points_attr()).and_then(decode_vec3f_array);
    let counts = value_at(&curves.curve_vertex_counts_attr()).and_then(decode_i32_array);
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

    let widths: Vec<f32> = value_at(&curves.widths_attr())
        .and_then(decode_f32_array)
        .unwrap_or_else(|| vec![1.0]);

    // USD defaults: type = cubic, basis = bezier.
    let ty = custom_token(prim, "type").unwrap_or_else(|| "cubic".to_string());
    let basis_name = custom_token(prim, "basis").unwrap_or_else(|| "bezier".to_string());
    let (basis, vstep) = match basis_name.as_str() {
        "bezier" => (&BEZIER_M, 3usize),
        "bspline" => (&BSPLINE_M, 1),
        "catmullRom" => (&CATMULL_ROM_M, 1),
        other => {
            warning!(
                CurvesUnsupportedBasis,
                at = prim.path(),
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
            warning!(
                CurvesInvalidCounts,
                at = prim.path(),
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

/// The two primitive batches one curves prim imports as: linear segments
/// and cubic (Bézier-form) spans.
pub(super) type CurveBatches = (Vec<CurveSegment>, Vec<CubicCurveSegment>);

/// One kernel scene holding a curves prim's batches, in its local space.
pub(super) fn curves_scene(batches: CurveBatches) -> crust_rt::Scene {
    let (segments, cubic_segments) = batches;
    let mut b = RtSceneBuilder::new();
    if !segments.is_empty() {
        b.attach(Geometry::RoundCurves { segments });
    }
    if !cubic_segments.is_empty() {
        b.attach(Geometry::CubicCurves {
            segments: cubic_segments,
        });
    }
    b.commit_with(crate::commit_options())
}

/// Places a curves prim's batches ([`curve_segments`] or
/// [`nurbs_curve_segments`]) at the top level, under an `Instance` carrying
/// its world transform.
pub(super) fn emit_curves(
    world: &mut WorldBuilder,
    prim: &Prim,
    batches: Option<CurveBatches>,
    world_xf: GMat4,
    material: Arc<dyn Material>,
) {
    let Some(batches) = batches else {
        return;
    };
    if world_xf.determinant().abs() < 1e-12 {
        warning!(
            CurvesNonInvertibleTransform,
            at = prim.path(),
            "Curves at {} have a non-invertible transform — skipped",
            prim.path()
        );
        return;
    }
    world.attach_masked(
        Geometry::Instance {
            scene: Arc::new(curves_scene(batches)),
            transform: Affine3A::from_mat4(world_xf),
            transform_end: None,
        },
        material,
        prim_ray_mask(prim),
    );
}

// -----------------------------------------------------------------------
// NURBS curves
// -----------------------------------------------------------------------

/// Whether `prim` is a `UsdGeomNurbsCurves`. openusd-schemas has no typed
/// schema for it, so the prim is recognised by its type name and read
/// through raw attributes.
pub(super) fn is_nurbs_curves(prim: &Prim) -> bool {
    matches!(prim.type_name(), Ok(Some(t)) if t.as_str() == "NurbsCurves")
}

/// A NURBS control point carried through the blossom: the homogeneous
/// position `(w·x, w·y, w·z)`, the width (a primvar, so not weighted) and the
/// weight `w`. Every channel is affine in the control points, so one
/// recurrence evaluates them all.
type NurbsVertex = [f64; 5];

fn lerp5(a: &NurbsVertex, b: &NurbsVertex, t: f64) -> NurbsVertex {
    std::array::from_fn(|c| a[c] + (b[c] - a[c]) * t)
}

/// The blossom (polar form) of span `span` of a degree-`p` B-spline,
/// evaluated at the `p` parameters `ts`: de Boor's recurrence with a
/// different parameter at each level. `ctrl` is the span's `p + 1` control
/// points `P[span-p ..= span]`, `knots` the curve's own knot vector.
///
/// With `ts = [s; p-j] ++ [e; j]` it is the `j`th Bézier control point of the
/// curve restricted to `[s, e]` (any sub-interval of the span), which is how
/// [`nurbs_curve_segments`] extracts spans: no knot insertion, clamped or
/// not, and a `ranges` trim is just a narrower `[s, e]`.
fn nurbs_blossom(
    ctrl: &[NurbsVertex],
    knots: &[f64],
    span: usize,
    p: usize,
    ts: &[f64],
) -> NurbsVertex {
    let mut d: Vec<NurbsVertex> = ctrl.to_vec();
    for r in 1..=p {
        let t = ts[r - 1];
        for k in (r..=p).rev() {
            let lo = knots[span + k - p];
            let hi = knots[span + 1 + k - r];
            let a = if hi > lo { (t - lo) / (hi - lo) } else { 0.0 };
            d[k] = lerp5(&d[k - 1], &d[k], a);
        }
    }
    d[p]
}

/// The Bézier control points (degree `p`) of a B-spline span restricted to
/// `[s, e]`.
fn nurbs_span_bezier(
    ctrl: &[NurbsVertex],
    knots: &[f64],
    span: usize,
    p: usize,
    s: f64,
    e: f64,
) -> Vec<NurbsVertex> {
    (0..=p)
        .map(|j| {
            let ts: Vec<f64> = (0..p).map(|r| if r < p - j { s } else { e }).collect();
            nurbs_blossom(ctrl, knots, span, p, &ts)
        })
        .collect()
}

/// A `double2[]` (`ranges`) as `(min, max)` pairs.
fn decode_ranges(v: sdf::Value) -> Option<Vec<(f64, f64)>> {
    match v {
        sdf::Value::Vec2dVec(v) => Some(v.into_iter().map(|r| (r.x, r.y)).collect()),
        sdf::Value::Vec2fVec(v) => Some(
            v.into_iter()
                .map(|r| (f64::from(r.x), f64::from(r.y)))
                .collect(),
        ),
        _ => None,
    }
}

/// Import a `UsdGeomNurbsCurves` batch as round curves, into the same two
/// batches [`curve_segments`] builds: each span between distinct knots
/// becomes one Bézier span ([`nurbs_span_bezier`]) — order 4 directly as a
/// [`CubicCurveSegment`], order 3 degree-elevated to one, order 2 as a
/// linear [`CurveSegment`]. Higher orders are refused with
/// `curves.unsupported_order`.
///
/// Read: `points`, `curveVertexCounts`, `order`, `knots` (one vector of
/// `count + order` values per curve, concatenated), `ranges` (the parameter
/// interval rendered per curve; the full domain when absent), `widths` (as
/// for `BasisCurves`, `vertex` widths following the basis) and
/// `pointWeights`. A rational curve with unequal weights is approximated:
/// each span's Bézier control points are projected out of homogeneous
/// space, which keeps the span's ends and end tangents' directions but not
/// its exact shape (`curves.rational_approximated`).
pub(super) fn nurbs_curve_segments(prim: &Prim) -> Option<CurveBatches> {
    let points = prim_value(prim, "points").and_then(decode_vec3f_array);
    let counts = prim_value(prim, "curveVertexCounts").and_then(decode_i32_array);
    let (points, counts) = match (points, counts) {
        (Some(p), Some(c)) => (p, c),
        _ => {
            debug!(
                "NurbsCurves at {} missing points / curveVertexCounts — skipped",
                prim.path()
            );
            return None;
        }
    };
    let n_points = points.len();
    let n_curves = counts.len();
    let orders = prim_value(prim, "order")
        .and_then(decode_i32_array)
        .unwrap_or_default();
    let knots = prim_value(prim, "knots")
        .and_then(decode_f64_array)
        .unwrap_or_default();
    let expected_knots: i64 = counts
        .iter()
        .zip(&orders)
        .map(|(&c, &o)| i64::from(c) + i64::from(o))
        .sum();
    if orders.len() != n_curves || knots.len() as i64 != expected_knots {
        warning!(
            CurvesInvalidKnots,
            at = prim.path(),
            "NurbsCurves at {}: {} curves need one order each and count + order knots each \
             ({} orders, {} knots authored, {} expected) — skipped",
            prim.path(),
            n_curves,
            orders.len(),
            knots.len(),
            expected_knots
        );
        return None;
    }
    let widths: Vec<f32> = prim_value(prim, "widths")
        .and_then(decode_f32_array)
        .unwrap_or_else(|| vec![1.0]);
    let weights = prim_value(prim, "pointWeights")
        .and_then(decode_f64_array)
        .filter(|w| w.len() == n_points);
    let ranges = prim_value(prim, "ranges")
        .and_then(decode_ranges)
        .filter(|r| r.len() == n_curves);
    let width_of = |global_idx: usize, curve_idx: usize| -> f64 {
        f64::from(if widths.len() == n_points {
            widths[global_idx]
        } else if widths.len() == n_curves {
            widths[curve_idx]
        } else {
            widths[0]
        })
    };

    let mut segments: Vec<CurveSegment> = Vec::new();
    let mut cubic_segments: Vec<CubicCurveSegment> = Vec::new();
    let mut rational = false;
    let mut unsupported_order: Option<i32> = None;
    let mut bad_knots = 0usize;
    let mut offset = 0usize;
    let mut knot_offset = 0usize;
    for (curve_idx, (&cnt, &order)) in counts.iter().zip(&orders).enumerate() {
        let cnt = cnt.max(0) as usize;
        let n_knots = cnt + order.max(0) as usize;
        if offset + cnt > n_points {
            warning!(
                CurvesInvalidCounts,
                at = prim.path(),
                "NurbsCurves at {}: curveVertexCounts overruns points — remaining curves skipped",
                prim.path()
            );
            break;
        }
        let first = offset;
        let u = &knots[knot_offset..knot_offset + n_knots];
        offset += cnt;
        knot_offset += n_knots;
        if !(2..=4).contains(&order) {
            unsupported_order.get_or_insert(order);
            continue;
        }
        let p = (order - 1) as usize;
        if cnt < order as usize
            || u.iter().any(|k| !k.is_finite())
            || u.windows(2).any(|k| k[1] < k[0])
        {
            bad_knots += 1;
            continue;
        }
        let ctrl: Vec<NurbsVertex> = (0..cnt)
            .map(|k| {
                let g = first + k;
                let pt = points[g];
                let w = weights.as_ref().map_or(1.0, |w| w[g]);
                [
                    f64::from(pt.x) * w,
                    f64::from(pt.y) * w,
                    f64::from(pt.z) * w,
                    width_of(g, curve_idx),
                    w,
                ]
            })
            .collect();
        if ctrl.windows(2).any(|c| c[0][4] != c[1][4]) {
            rational = true;
        }
        // The rendered interval: the curve's domain [u_p, u_n+1], narrowed
        // by an authored range.
        let (mut lo, mut hi) = (u[p], u[cnt]);
        if let Some(&(r0, r1)) = ranges.as_ref().map(|r| &r[curve_idx])
            && r0 < r1
        {
            lo = lo.max(r0);
            hi = hi.min(r1);
        }
        for span in p..cnt {
            let s = u[span].max(lo);
            let e = u[span + 1].min(hi);
            // Finite: the knots are, and a NaN range is never applied.
            if e <= s {
                continue;
            }
            let bez = nurbs_span_bezier(&ctrl[span - p..=span], u, span, p, s, e);
            let pos = |v: &NurbsVertex| -> Vec3A {
                let w = if v[4] != 0.0 { v[4] } else { 1.0 };
                Vec3A::new((v[0] / w) as f32, (v[1] / w) as f32, (v[2] / w) as f32)
            };
            let radius = |v: &NurbsVertex| 0.5 * (v[3] as f32).max(1e-6);
            let (first_v, last_v) = (&bez[0], &bez[p]);
            match p {
                1 => segments.push(CurveSegment {
                    p0: pos(first_v),
                    p1: pos(last_v),
                    r0: radius(first_v),
                    r1: radius(last_v),
                }),
                2 => {
                    // Degree elevation: the cubic tracing the same quadratic.
                    let (q0, q1, q2) = (pos(&bez[0]), pos(&bez[1]), pos(&bez[2]));
                    cubic_segments.push(CubicCurveSegment {
                        cp: [q0, (q0 + q1 * 2.0) / 3.0, (q1 * 2.0 + q2) / 3.0, q2],
                        r0: radius(first_v),
                        r1: radius(last_v),
                    });
                }
                _ => cubic_segments.push(CubicCurveSegment {
                    cp: std::array::from_fn(|j| pos(&bez[j])),
                    r0: radius(first_v),
                    r1: radius(last_v),
                }),
            }
        }
    }

    if let Some(order) = unsupported_order {
        warning!(
            CurvesUnsupportedOrder,
            at = prim.path(),
            "NurbsCurves at {}: order {} is not supported (2 | 3 | 4) — those curves skipped",
            prim.path(),
            order
        );
    }
    if bad_knots > 0 {
        warning!(
            CurvesInvalidKnots,
            at = prim.path(),
            "NurbsCurves at {}: {} curves have fewer points than their order, or decreasing \
             or non-finite knots — skipped",
            prim.path(),
            bad_knots
        );
    }
    if rational {
        warning!(
            CurvesRationalApproximated,
            at = prim.path(),
            "NurbsCurves at {}: unequal pointWeights — the rational curves are approximated \
             by polynomial spans",
            prim.path()
        );
    }
    if segments.is_empty() && cubic_segments.is_empty() {
        debug!("NurbsCurves at {} produced no segments", prim.path());
        return None;
    }
    debug!(
        "Imported NurbsCurves at {} ({} curves, {} segments, {} cubic spans)",
        prim.path(),
        n_curves,
        segments.len(),
        cubic_segments.len()
    );
    Some((segments, cubic_segments))
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

    /// A point on a degree-`p` B-spline by de Boor's algorithm — the blossom
    /// on the diagonal.
    fn de_boor(ctrl: &[NurbsVertex], knots: &[f64], p: usize, t: f64) -> NurbsVertex {
        let span = (p..ctrl.len())
            .rfind(|&i| knots[i] <= t && knots[i] < knots[i + 1])
            .unwrap_or(p);
        nurbs_blossom(&ctrl[span - p..=span], knots, span, p, &vec![t; p])
    }

    fn vertex(x: f64, y: f64, z: f64) -> NurbsVertex {
        [x, y, z, 0.1, 1.0]
    }

    fn eval_bezier(b: &[NurbsVertex], t: f64) -> NurbsVertex {
        let mut d = b.to_vec();
        for r in 1..d.len() {
            for k in 0..d.len() - r {
                d[k] = lerp5(&d[k], &d[k + 1], t);
            }
        }
        d[0]
    }

    fn near(a: &NurbsVertex, b: &NurbsVertex) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    /// A clamped single-span cubic NURBS curve is its own Bézier curve.
    #[test]
    fn clamped_cubic_nurbs_is_its_bezier() {
        let ctrl = [
            vertex(0.0, 0.0, 0.0),
            vertex(1.0, 2.0, 0.0),
            vertex(2.0, -1.0, 1.0),
            vertex(3.0, 0.0, 0.0),
        ];
        let knots = [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
        let bez = nurbs_span_bezier(&ctrl, &knots, 3, 3, 0.0, 1.0);
        for (b, c) in bez.iter().zip(&ctrl) {
            assert!(near(b, c), "{bez:?}");
        }
    }

    /// A uniform-knot cubic span is the `bspline` basis's span.
    #[test]
    fn uniform_cubic_nurbs_matches_the_bspline_basis() {
        let pts = [
            Vec3A::new(0.0, 0.0, 0.0),
            Vec3A::new(1.0, 2.0, 0.5),
            Vec3A::new(2.0, -1.0, 1.0),
            Vec3A::new(3.5, 1.0, -0.5),
        ];
        let ctrl = pts.map(|p| vertex(f64::from(p.x), f64::from(p.y), f64::from(p.z)));
        let knots: Vec<f64> = (0..8).map(f64::from).collect();
        let bez = nurbs_span_bezier(&ctrl, &knots, 3, 3, 3.0, 4.0);
        let want = basis_to_bezier(&BSPLINE_M, &pts);
        for (b, w) in bez.iter().zip(&want) {
            let got = Vec3A::new(b[0] as f32, b[1] as f32, b[2] as f32);
            assert!(got.abs_diff_eq(*w, 1e-5), "{got:?} vs {w:?}");
        }
    }

    /// Every extracted span, trimmed or not, traces the B-spline itself —
    /// on a non-uniform, multi-span knot vector, densely over each span.
    #[test]
    fn extracted_spans_trace_the_nurbs_curve() {
        let ctrl = [
            vertex(0.0, 0.0, 0.0),
            [1.0, 2.0, 0.0, 0.3, 1.0],
            vertex(2.0, -1.0, 1.0),
            [3.0, 0.5, -1.0, 0.05, 1.0],
            vertex(4.0, 1.0, 0.0),
            vertex(5.0, 0.0, 2.0),
        ];
        for p in 1..=3 {
            let n = ctrl.len();
            let mut knots = vec![0.0; p + 1];
            knots.extend([0.2, 0.25, 0.7, 0.9, 1.3][..n - p - 1].iter().copied());
            knots.extend(vec![2.0; p + 1]);
            assert_eq!(knots.len(), n + p + 1);
            for span in p..n {
                let (lo, hi) = (knots[span], knots[span + 1]);
                // The whole span, then its middle half (a `ranges` trim).
                for (s, e) in [(lo, hi), (lo + 0.25 * (hi - lo), lo + 0.75 * (hi - lo))] {
                    let ctrl_span = &ctrl[span - p..=span];
                    let bez = nurbs_span_bezier(ctrl_span, &knots, span, p, s, e);
                    for i in 0..=8 {
                        let f = f64::from(i) / 8.0;
                        let direct = de_boor(&ctrl, &knots, p, s + (e - s) * f);
                        let via = eval_bezier(&bez, f);
                        assert!(near(&direct, &via), "p={p} span={span} f={f}");
                    }
                }
            }
        }
    }
}
