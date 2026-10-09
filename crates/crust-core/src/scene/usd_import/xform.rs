//! Prim transforms: openusd's `UsdGeomXformable` composition, reached for any
//! prim type, and the conversion to glam.

use crate::warning;
use glam::Mat4 as GMat4;
use openusd::gf::Matrix4d;
use openusd::usd::{Prim, SchemaBase, SchemaKind};
use openusd_schemas::geom::{Imageable, Xformable};

use super::time::xform_time;

/// USD authors 4x4 matrices as row-vector row-major (translation in the
/// last row, indices 12..15). glam::Mat4 is column-major with the
/// column-vector convention, so USD's row-major layout is exactly the
/// column-major layout of the transposed matrix — which is what we want
/// for M * v evaluation.
fn usd_mat_to_glam(m: Matrix4d) -> GMat4 {
    GMat4::from_cols_array(&m.0.map(|v| v as f32))
}

/// Any prim, viewed as `UsdGeomXformable`.
///
/// openusd 0.7 reaches its transform composition only through the
/// `Xformable` trait, implemented per typed schema whose `get` checks the
/// prim's type. Every method this module calls is a default method over
/// `prim()`, so this one view composes the stack on every prim type — the
/// scope crust has always given `xformOp`s — instead of a list of types
/// that drifts (a type missing from it composed to identity).
struct AnyXformable<'a>(&'a Prim);

impl SchemaBase for AnyXformable<'_> {
    const KIND: SchemaKind = SchemaKind::AbstractTyped;

    fn prim(&self) -> &Prim {
        self.0
    }
}

impl Imageable for AnyXformable<'_> {}

impl Xformable for AnyXformable<'_> {}

/// Every op kind `UsdGeomXformOp` defines. openusd 0.7 composes any other
/// kind as identity without a word; C++ USD rejects it. This list decides a
/// warning only, never a matrix.
const XFORM_OP_KINDS: &[&str] = &[
    "translate",
    "translateX",
    "translateY",
    "translateZ",
    "scale",
    "scaleX",
    "scaleY",
    "scaleZ",
    "rotateX",
    "rotateY",
    "rotateZ",
    "rotateXYZ",
    "rotateXZY",
    "rotateYXZ",
    "rotateYZX",
    "rotateZXY",
    "rotateZYX",
    "orient",
    "transform",
];

/// The kind of an `xformOpOrder` entry (`!invert!xformOp:translate:pivot` →
/// `translate`), or `None` when the entry is not an `xformOp:` name at all.
fn op_kind(entry: &str) -> Option<&str> {
    let name = entry.strip_prefix("!invert!").unwrap_or(entry);
    name.strip_prefix("xformOp:")?.split(':').next()
}

/// Local-to-parent transform of `prim`, composed by openusd in `f64` and
/// cast once. An unknown op kind contributes identity (one warning naming
/// every such op); a stack openusd refuses — a `!resetXformStack!` after
/// the first entry, a value it cannot read — is identity, with a warning.
fn local_matrix(xf: &AnyXformable<'_>) -> GMat4 {
    if let Ok(Some(order)) = xf.xform_op_order() {
        let unknown: Vec<&str> = order
            .iter()
            .filter(|e| e.as_str() != "!resetXformStack!")
            .filter(|e| !op_kind(e).is_some_and(|k| XFORM_OP_KINDS.contains(&k)))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            warning!(
                XformUnknownOp,
                at = xf.0.path(),
                "{}: xformOpOrder lists {} — not a UsdGeomXformOp kind, read as identity",
                xf.0.path(),
                unknown.join(", ")
            );
        }
    }
    match xf.local_to_parent_transform(xform_time()) {
        Ok(m) => usd_mat_to_glam(m),
        Err(e) => {
            warning!(
                XformUncomposable,
                at = xf.0.path(),
                "{}: could not compose its xformOp stack ({e}) — its local transform is \
                 identity",
                xf.0.path()
            );
            GMat4::IDENTITY
        }
    }
}

/// `prim`'s transform given its parent's: `parent · local`, or `local` alone
/// when the prim authors a leading `!resetXformStack!` — the one composition
/// rule every walk (the traversal, the placement count, the prototype walk,
/// a camera's ancestor chain) applies. The pseudo-root, where every walk
/// starts, owns no properties and passes `parent` through.
pub(super) fn compose_with_parent(prim: &Prim, parent: GMat4) -> GMat4 {
    if prim.path().as_str() == "/" {
        return parent;
    }
    let xf = AnyXformable(prim);
    let local = local_matrix(&xf);
    if xf.resets_xform_stack().unwrap_or(false) {
        local
    } else {
        parent * local
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use openusd::sdf;
    use openusd::usd::Stage;

    /// `/W/P` of a stage whose `P` authors `ops` under a parent `W`
    /// translated by `(0, 0, 7)`, with its world transform.
    fn world_of(name: &str, ops: &str) -> GMat4 {
        let dir = std::env::temp_dir().join("crust_xform_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(
            &path,
            format!(
                "#usda 1.0\ndef Xform \"W\"\n{{\n    double3 xformOp:translate = (0, 0, 7)\n    \
                 uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    \
                 def Xform \"P\"\n    {{\n{ops}\n    }}\n}}\n"
            ),
        )
        .expect("write stage");
        let stage = Stage::builder()
            .open(path.to_str().unwrap())
            .expect("stage opens");
        [
            sdf::Path::abs_root(),
            sdf::path("/W").unwrap(),
            sdf::path("/W/P").unwrap(),
        ]
        .into_iter()
        .map(|p| super::super::prim_at(&stage, p))
        .fold(GMat4::IDENTITY, |acc, p| compose_with_parent(&p, acc))
    }

    fn assert_near(a: GMat4, b: GMat4) {
        assert!(a.abs_diff_eq(b, 1e-5), "expected\n{b}\ngot\n{a}");
    }

    #[test]
    fn an_unknown_op_kind_is_identity_and_the_rest_composes() {
        let m = world_of(
            "unknown_kind",
            r#"        double3 xformOp:translate = (1, 2, 3)
        double3 xformOp:bogus = (5, 5, 5)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:bogus"]"#,
        );
        assert_near(m, GMat4::from_translation(Vec3::new(1.0, 2.0, 10.0)));
    }

    /// openusd 0.7 refuses a reset after the first entry; the prim's local
    /// transform is then identity and the parent's is still inherited.
    #[test]
    fn a_mid_stack_reset_is_identity_and_keeps_the_parent() {
        let m = world_of(
            "mid_stack_reset",
            r#"        double3 xformOp:translate = (1, 2, 3)
        uniform token[] xformOpOrder = ["xformOp:translate", "!resetXformStack!"]"#,
        );
        assert_near(m, GMat4::from_translation(Vec3::new(0.0, 0.0, 7.0)));
    }

    #[test]
    fn a_leading_reset_drops_the_parent() {
        let m = world_of(
            "leading_reset",
            r#"        double3 xformOp:translate = (1, 2, 3)
        uniform token[] xformOpOrder = ["!resetXformStack!", "xformOp:translate"]"#,
        );
        assert_near(m, GMat4::from_translation(Vec3::new(1.0, 2.0, 3.0)));
    }

    /// The pivot pair, `!invert!` and a three-axis rotation, against the
    /// matrix C++ USD computes for the same stack (row-vector:
    /// `0 2 0 0 / -2 0 0 0 / 0 0 2 0 / 5 2 0 1`).
    #[test]
    fn a_pivot_stack_matches_cpp_usd() {
        let m = world_of(
            "pivot",
            r#"        double3 xformOp:translate = (2, 3, 0)
        double3 xformOp:translate:pivot = (1, 1, 0)
        float3 xformOp:rotateXYZ = (0, 0, 90)
        float3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:translate:pivot", "xformOp:rotateXYZ", "xformOp:scale", "!invert!xformOp:translate:pivot"]"#,
        );
        let cpp = GMat4::from_cols_array(&[
            0.0, 2.0, 0.0, 0.0, -2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 5.0, 2.0, 0.0, 1.0,
        ]);
        assert_near(m, GMat4::from_translation(Vec3::new(0.0, 0.0, 7.0)) * cpp);
    }
}
