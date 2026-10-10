//! Prim transforms: openusd's `UsdGeomXformable` composition, and the
//! conversion to glam.

use crate::warning;
use glam::Mat4 as GMat4;
use openusd::gf::Matrix4d;
use openusd::tf::Token;
use openusd::usd::Prim;
use openusd_schemas::geom::{XformQuery, Xformable, XformableSchema};

use super::time::eval_time;

/// USD authors 4x4 matrices as row-vector row-major (translation in the
/// last row, indices 12..15). glam::Mat4 is column-major with the
/// column-vector convention, so USD's row-major layout is exactly the
/// column-major layout of the transposed matrix — which is what we want
/// for M * v evaluation.
fn usd_mat_to_glam(m: Matrix4d) -> GMat4 {
    GMat4::from_cols_array(&m.0.map(|v| v as f32))
}

/// Every op kind `UsdGeomXformOp` defines. openusd composes any other kind as
/// identity without a word; C++ USD rejects it. This list decides a
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

/// Warns once, naming every `xformOpOrder` entry that is not a
/// `UsdGeomXformOp` kind: openusd reads each as identity.
fn warn_unknown_ops(xf: &Xformable) {
    let Ok(Some(order)) = xf.xform_op_order() else {
        return;
    };
    let unknown: Vec<&str> = order
        .iter()
        .filter(|e| e.as_str() != "!resetXformStack!")
        .filter(|e| !op_kind(e).is_some_and(|k| XFORM_OP_KINDS.contains(&k)))
        .map(|e| e.as_str())
        .collect();
    if !unknown.is_empty() {
        warning!(
            XformUnknownOp,
            at = xf.path(),
            "{}: xformOpOrder lists {} — not a UsdGeomXformOp kind, read as identity",
            xf.path(),
            unknown.join(", ")
        );
    }
}

/// Warns when a prim whose type the schema registry does not know (a plugin or
/// studio schema) authors an `xformOpOrder`: it reads as not `Xformable`, so
/// its ops and the placement of everything under it are dropped, as in C++
/// USD without that plugin. An untyped prim and a known type that is not
/// `Xformable` (a `Scope`, a `Material`) stay silent: C++ ignores their ops by
/// definition.
fn warn_unknown_type(prim: &Prim) {
    let Ok(Some(ty)) = prim.type_name() else {
        return;
    };
    if ty.as_str().is_empty() || openusd_schemas::schema_registry().is_concrete_type(&ty) {
        return;
    }
    let authors_ops = prim
        .attribute("xformOpOrder")
        .get::<Vec<Token>>()
        .ok()
        .flatten()
        .is_some_and(|order| !order.is_empty());
    if authors_ops {
        warning!(
            XformUnknownType,
            at = prim.path(),
            "{}: its type {ty} is not one the schema registry knows, so it is not Xformable and \
             its xformOps are ignored (further occurrences are counted in the import's \
             warnings)",
            prim.path()
        );
    }
}

/// `prim`'s transform given its parent's: `parent · local`, or `local` alone
/// when the prim's `xformOpOrder` lists `!resetXformStack!` — the one
/// composition rule every walk (the traversal, the placement count, the
/// prototype walk, a camera's ancestor chain) applies.
///
/// openusd composes the stack as C++ `UsdGeomXformable` does, in `f64`, cast
/// once: a reset drops the ops listed before it, and a prim that is not
/// `Xformable` (the pseudo-root, an untyped prim, a `Scope`) contributes
/// nothing, whatever ops it authors. A stack openusd cannot compose (a
/// singular `transform` to invert) keeps the parent, with a warning.
pub(super) fn compose_with_parent(prim: &Prim, parent: GMat4) -> GMat4 {
    let Some(xf) = Xformable::from_prim(prim.clone()).ok().flatten() else {
        warn_unknown_type(prim);
        return parent;
    };
    warn_unknown_ops(&xf);
    let query = XformQuery::new(&xf);
    let composed = query.as_ref().map_err(|e| e.to_string()).and_then(|q| {
        q.local_transformation(eval_time())
            .map_err(|e| e.to_string())
    });
    let local = match composed {
        Ok(m) => usd_mat_to_glam(m),
        Err(e) => {
            warning!(
                XformUncomposable,
                at = xf.path(),
                "{}: could not compose its xformOp stack ({e}) — its local transform is \
                 identity",
                xf.path()
            );
            GMat4::IDENTITY
        }
    };
    if query.is_ok_and(|q| q.resets_xform_stack()) {
        local
    } else {
        parent * local
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::usd_import::stage_builder;
    use glam::Vec3;
    use openusd::sdf;

    /// `/W/P` of a stage whose `Xform` `P` authors `ops` under a parent `W`
    /// translated by `(0, 0, 7)`, with its world transform.
    fn world_of(name: &str, ops: &str) -> GMat4 {
        world_of_type(name, "Xform", ops)
    }

    /// [`world_of`] with `P` of type `ty` (empty: an untyped `def`).
    fn world_of_type(name: &str, ty: &str, ops: &str) -> GMat4 {
        let dir = std::env::temp_dir().join("crust_xform_tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{name}.usda"));
        std::fs::write(
            &path,
            format!(
                "#usda 1.0\ndef Xform \"W\"\n{{\n    double3 xformOp:translate = (0, 0, 7)\n    \
                 uniform token[] xformOpOrder = [\"xformOp:translate\"]\n    \
                 def {ty} \"P\"\n    {{\n{ops}\n    }}\n}}\n"
            ),
        )
        .expect("write stage");
        let stage = stage_builder()
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

    /// A reset after the first entry keeps only the ops listed after it and
    /// drops the parent, as C++ `GetOrderedXformOps` does. (openusd 0.7
    /// refused such a stack: identity, with the parent kept.)
    #[test]
    fn a_mid_stack_reset_keeps_only_the_ops_after_it() {
        let m = world_of(
            "mid_stack_reset",
            r#"        double3 xformOp:translate = (1, 2, 3)
        double3 xformOp:translate:after = (0, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "!resetXformStack!", "xformOp:translate:after"]"#,
        );
        assert_near(m, GMat4::from_translation(Vec3::new(0.0, 2.0, 0.0)));
    }

    /// A prim that is not `Xformable` contributes nothing, whatever ops it
    /// authors, as in C++ USD: it passes its parent's transform through.
    #[test]
    fn ops_on_a_prim_that_is_not_xformable_are_ignored() {
        let ops = r#"        double3 xformOp:translate = (1, 2, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]"#;
        let parent = GMat4::from_translation(Vec3::new(0.0, 0.0, 7.0));
        assert_near(world_of_type("scope_ops", "Scope", ops), parent);
        assert_near(world_of_type("untyped_ops", "", ops), parent);
    }

    /// Every `Xformable` type C++ USD defines outside UsdGeom and UsdLux composes
    /// its ops: the schema registry knows these types only because crust-core
    /// compiles their families in, and an unknown type would read as not
    /// `Xformable`.
    #[test]
    fn xformable_prims_of_every_family_compose_their_ops() {
        let ops = r#"        double3 xformOp:translate = (1, 2, 3)
        uniform token[] xformOpOrder = ["xformOp:translate"]"#;
        let placed = GMat4::from_translation(Vec3::new(1.0, 2.0, 10.0));
        for ty in [
            "SkelRoot",
            "Skeleton",
            "Volume",
            "OpenVDBAsset",
            "Field3DAsset",
            "ParticleField",
            "ParticleField3DGaussianSplat",
            "GenerativeProcedural",
            "SpatialAudio",
        ] {
            assert_near(world_of_type(&format!("{ty}_ops"), ty, ops), placed);
        }
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
