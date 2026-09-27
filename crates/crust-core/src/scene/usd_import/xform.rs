//! Prim transforms: the local `xformOp:*` composer and the conversions to glam.

use glam::{Mat4 as GMat4, Vec3};
use openusd::gf::Matrix4d;
use openusd::sdf;
use openusd::usd::{Prim, Stage};
use openusd_schemas::geom::{
    Camera as UsdCamera, Mesh as UsdMesh, Sphere as UsdSphere, Xform, Xformable,
};
use openusd_schemas::lux::{RectLight, SphereLight};
use tracing::warn;

use super::time::{eval_time, xform_time};

/// USD authors 4x4 matrices as row-vector row-major (translation in the
/// last row, indices 12..15). glam::Mat4 is column-major with the
/// column-vector convention, so USD's row-major layout is exactly the
/// column-major layout of the transposed matrix — which is what we want
/// for M * v evaluation.
fn usd_mat_to_glam(m: Matrix4d) -> GMat4 {
    let a = m.0;
    GMat4::from_cols_array(&[
        a[0] as f32,
        a[1] as f32,
        a[2] as f32,
        a[3] as f32,
        a[4] as f32,
        a[5] as f32,
        a[6] as f32,
        a[7] as f32,
        a[8] as f32,
        a[9] as f32,
        a[10] as f32,
        a[11] as f32,
        a[12] as f32,
        a[13] as f32,
        a[14] as f32,
        a[15] as f32,
    ])
}

/// Local-to-parent transform of `prim`, composed from its authored
/// `xformOp:*` attributes by [`compose_xform_ops`].
///
/// This deliberately does NOT use openusd's `local_to_parent_transform`:
/// openusd 0.5.0 composes multi-op `xformOpOrder` stacks in the wrong
/// order (the authored translate comes back multiplied by the scale — e.g.
/// cornellbox's `pCube1`, translate `(0,2,0)` · scale 4, yielded
/// translation `(0,8,0)`), which made `samples/cornellbox.usda` render as
/// floating objects against sky. Stacks with an op we cannot decode fall
/// back to openusd's composition with a warning, so unusual scenes behave
/// no worse than before.
pub(super) fn local_matrix_at(stage: &Stage, prim: &Prim) -> GMat4 {
    match compose_xform_ops(prim) {
        Some(m) => m,
        None => {
            warn!(
                "could not decode the xformOp stack at {} — falling back to openusd's \
                 composition (known to be wrong for multi-op stacks)",
                prim.path()
            );
            local_matrix_via_openusd(stage, prim)
        }
    }
}

/// Composes the prim's `xformOpOrder` stack into a local-to-parent matrix.
///
/// UsdGeomXformable semantics, in the column-vector convention used here:
/// for `xformOpOrder = [op1, op2, …, opN]` a point transforms as
/// `p' = M(op1)·M(op2)·…·M(opN)·p` — the last-listed op is applied to the
/// point first and the first-listed is outermost. (Maya's
/// `["xformOp:translate", "xformOp:scale"]` therefore scales points first
/// and translates last: the composed translation equals the authored
/// translate.)
///
/// Returns `None` if any op token or value cannot be decoded.
fn compose_xform_ops(prim: &Prim) -> Option<GMat4> {
    let order = match prim
        .attribute("xformOpOrder")
        .get_at::<sdf::Value>(eval_time())
    {
        Ok(Some(sdf::Value::TokenVec(order))) => order,
        Ok(Some(_)) => return None,
        // No order authored: authored xformOp attrs (if any) do not apply.
        _ => return Some(GMat4::IDENTITY),
    };

    let mut local = GMat4::IDENTITY;
    for token in &order {
        // Not a transform op: it truncates the inherited stack, which
        // `resets_xform_stack_at` reports separately.
        if token == "!resetXformStack!" {
            continue;
        }
        let (name, inverted) = match token.strip_prefix("!invert!") {
            Some(rest) => (rest, true),
            None => (token.as_str(), false),
        };
        let mut m = xform_op_matrix(prim, name)?;
        if inverted {
            m = m.inverse();
        }
        local *= m;
    }
    Some(local)
}

/// Matrix of a single `xformOp:<kind>[:<suffix>]` attribute on `prim`, or
/// `None` for op kinds/value types we do not support.
fn xform_op_matrix(prim: &Prim, name: &str) -> Option<GMat4> {
    let kind = name.strip_prefix("xformOp:")?;
    // Suffixes name op instances (`xformOp:translate:pivot`); the kind is
    // the first segment.
    let kind = kind.split(':').next().unwrap_or(kind);
    let value = prim
        .attribute(name)
        .get_at::<sdf::Value>(eval_time())
        .ok()
        .flatten()?;

    match kind {
        "translate" => Some(GMat4::from_translation(value_as_vec3(&value)?)),
        "scale" => Some(GMat4::from_scale(value_as_vec3(&value)?)),
        "transform" => match value {
            sdf::Value::Matrix4d(m) => Some(usd_mat_to_glam(m)),
            _ => None,
        },
        "orient" => match value {
            sdf::Value::Quatf(q) => Some(GMat4::from_quat(
                glam::Quat::from_xyzw(q.x, q.y, q.z, q.w).normalize(),
            )),
            sdf::Value::Quatd(q) => Some(GMat4::from_quat(
                glam::Quat::from_xyzw(q.x as f32, q.y as f32, q.z as f32, q.w as f32).normalize(),
            )),
            _ => None,
        },
        "rotateX" => Some(GMat4::from_rotation_x(value_as_f32(&value)?.to_radians())),
        "rotateY" => Some(GMat4::from_rotation_y(value_as_f32(&value)?.to_radians())),
        "rotateZ" => Some(GMat4::from_rotation_z(value_as_f32(&value)?.to_radians())),
        // Euler triples: the vector components are always the X/Y/Z-axis
        // angles in degrees; the op name gives the application order, first
        // named axis applied to the point first (so it sits rightmost).
        "rotateXYZ" | "rotateXZY" | "rotateYXZ" | "rotateYZX" | "rotateZXY" | "rotateZYX" => {
            let v = value_as_vec3(&value)?;
            let rx = GMat4::from_rotation_x(v.x.to_radians());
            let ry = GMat4::from_rotation_y(v.y.to_radians());
            let rz = GMat4::from_rotation_z(v.z.to_radians());
            Some(match kind {
                "rotateXYZ" => rz * ry * rx,
                "rotateXZY" => ry * rz * rx,
                "rotateYXZ" => rz * rx * ry,
                "rotateYZX" => rx * rz * ry,
                "rotateZXY" => ry * rx * rz,
                _ => rx * ry * rz, // rotateZYX
            })
        }
        _ => None,
    }
}

fn value_as_vec3(value: &sdf::Value) -> Option<Vec3> {
    match value {
        sdf::Value::Vec3f(v) => Some(Vec3::new(v.x, v.y, v.z)),
        sdf::Value::Vec3d(v) => Some(Vec3::new(v.x as f32, v.y as f32, v.z as f32)),
        sdf::Value::Vec3h(v) => Some(Vec3::new(v.x.to_f32(), v.y.to_f32(), v.z.to_f32())),
        _ => None,
    }
}

fn value_as_f32(value: &sdf::Value) -> Option<f32> {
    match value {
        sdf::Value::Float(v) => Some(*v),
        sdf::Value::Double(v) => Some(*v as f32),
        sdf::Value::Half(v) => Some(v.to_f32()),
        _ => None,
    }
}

/// openusd's own composition, kept as the fallback for op stacks
/// `compose_xform_ops` cannot decode. Known to compose multi-op stacks in
/// the wrong order (see `local_matrix_at`).
fn local_matrix_via_openusd(stage: &Stage, prim: &Prim) -> GMat4 {
    if let Ok(Some(x)) = Xform::get(stage, prim.path().clone())
        && let Ok(m) = x.local_to_parent_transform(xform_time())
    {
        return usd_mat_to_glam(m);
    }
    if let Ok(Some(m)) = UsdMesh::get(stage, prim.path().clone())
        && let Ok(mat) = m.local_to_parent_transform(xform_time())
    {
        return usd_mat_to_glam(mat);
    }
    if let Ok(Some(s)) = UsdSphere::get(stage, prim.path().clone())
        && let Ok(mat) = s.local_to_parent_transform(xform_time())
    {
        return usd_mat_to_glam(mat);
    }
    if let Ok(Some(c)) = UsdCamera::get(stage, prim.path().clone())
        && let Ok(mat) = c.local_to_parent_transform(xform_time())
    {
        return usd_mat_to_glam(mat);
    }
    if let Ok(Some(l)) = SphereLight::get(stage, prim.path().clone())
        && let Ok(mat) = l.local_to_parent_transform(xform_time())
    {
        return usd_mat_to_glam(mat);
    }
    if let Ok(Some(l)) = RectLight::get(stage, prim.path().clone())
        && let Ok(mat) = l.local_to_parent_transform(xform_time())
    {
        return usd_mat_to_glam(mat);
    }
    GMat4::IDENTITY
}

pub(super) fn resets_xform_stack_at(stage: &Stage, prim: &Prim) -> bool {
    if let Ok(Some(x)) = Xform::get(stage, prim.path().clone()) {
        return x.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(m)) = UsdMesh::get(stage, prim.path().clone()) {
        return m.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(s)) = UsdSphere::get(stage, prim.path().clone()) {
        return s.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(c)) = UsdCamera::get(stage, prim.path().clone()) {
        return c.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(l)) = SphereLight::get(stage, prim.path().clone()) {
        return l.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(l)) = RectLight::get(stage, prim.path().clone()) {
        return l.resets_xform_stack().unwrap_or(false);
    }
    false
}
