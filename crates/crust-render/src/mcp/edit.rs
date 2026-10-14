//! Editing through composition (design D3). Every edit tool reduces to a
//! USDA snippet merged into the override layer: `author_usda` takes one as
//! it is, and `set_attribute`, `set_variant`, `set_active` and
//! `bind_material` write theirs. openusd's own parser then types every
//! value, so an opinion means in the session what it means in any USD tool.
//!
//! The merge is per field, as a stronger layer's opinions sit over a weaker
//! one's: a field the snippet authors replaces the layer's, except that
//! - the namespace-children lists are unioned, so authoring one prim or
//!   property keeps its siblings;
//! - a variant selection or a dictionary merges key by key;
//! - a token or string list op (`apiSchemas`, `variantSetNames`) merges item
//!   by item, unless either side is explicit;
//! - an `over` never downgrades a `def` or a `class` already there.
//!
//! The other list ops (`references`, `payload`, relationship targets, …)
//! are replaced as a whole, as any other field: `bind_material` rebinds.

use openusd::sdf::{self, AbstractData, Specifier, Value};
use openusd::usd::Stage;
use std::path::Path;

/// Layer metadata a snippet may not author: it would change what the
/// session is an override *of*.
const REFUSED_LAYER_FIELDS: &[&str] = &["subLayers", "subLayerOffsets"];

/// The namespace-children fields, unioned rather than replaced.
const CHILDREN: &[&str] = &[
    "primChildren",
    "propertyChildren",
    "variantSetChildren",
    "variantChildren",
];

/// A snippet, parsed and checked against the layer it is merged into.
pub struct Snippet {
    data: sdf::Data,
}

impl Snippet {
    /// Parses `text` as a USDA layer (the `#usda 1.0` header may be left
    /// out), refusing layer metadata that changes the session's sublayers.
    pub fn parse(text: &str) -> Result<Snippet, String> {
        let text = if text.trim_start().starts_with("#usda") {
            text.to_owned()
        } else {
            format!("#usda 1.0\n{text}")
        };
        let data =
            openusd::usda::parse(&text).map_err(|e| format!("the USDA does not parse: {e}"))?;
        for field in data.list_fields(&sdf::Path::abs_root()).unwrap_or_default() {
            if REFUSED_LAYER_FIELDS.contains(&field.as_str()) {
                return Err(format!(
                    "the USDA authors `{field}`: a session's layer sublayers its input and nothing else"
                ));
            }
        }
        Ok(Snippet { data })
    }

    /// How many specs below the pseudo-root the snippet authors.
    pub fn specs(&self) -> usize {
        self.data
            .spec_paths()
            .iter()
            .filter(|p| **p != sdf::Path::abs_root())
            .count()
    }

    /// Refuses a snippet whose spec at some path is of another kind than the
    /// layer's there (a prim where the layer has a property).
    fn check_against(&self, dst: &dyn AbstractData) -> Result<(), String> {
        for path in self.data.spec_paths() {
            if let (Some(theirs), Some(ours)) = (self.data.spec_type(&path), dst.spec_type(&path))
                && theirs != ours
            {
                return Err(format!(
                    "{path} is a {ours:?} spec in the layer and a {theirs:?} spec in the USDA"
                ));
            }
        }
        Ok(())
    }

    /// Merges the snippet into `dst` (see the module).
    fn merge_into(&self, dst: &mut dyn AbstractData) {
        let src = &self.data;
        for path in src.spec_paths() {
            let Some(ty) = src.spec_type(&path) else {
                continue;
            };
            if dst.spec_type(&path).is_none() {
                dst.create_spec(path.clone(), ty);
            }
            for field in src.list_fields(&path).unwrap_or_default() {
                let Ok(Some(value)) = src.try_field(&path, &field) else {
                    continue;
                };
                let old = dst
                    .try_field(&path, &field)
                    .ok()
                    .flatten()
                    .map(|v| v.into_owned());
                let merged = match (field.as_str(), value.as_ref(), old) {
                    ("specifier", Value::Specifier(Specifier::Over), Some(_)) => continue,
                    (f, Value::TokenVec(new), Some(Value::TokenVec(mut all)))
                        if CHILDREN.contains(&f) =>
                    {
                        for t in new {
                            if !all.contains(t) {
                                all.push(t.clone());
                            }
                        }
                        Value::TokenVec(all)
                    }
                    (
                        _,
                        Value::VariantSelectionMap(new),
                        Some(Value::VariantSelectionMap(mut all)),
                    ) => {
                        all.extend(new.iter().map(|(k, v)| (k.clone(), v.clone())));
                        Value::VariantSelectionMap(all)
                    }
                    (_, Value::Dictionary(new), Some(Value::Dictionary(mut all))) => {
                        all.extend(new.iter().map(|(k, v)| (k.clone(), v.clone())));
                        Value::Dictionary(all)
                    }
                    (_, Value::TokenListOp(new), Some(Value::TokenListOp(old))) => {
                        Value::TokenListOp(merge_list_op(old, new))
                    }
                    (_, Value::StringListOp(new), Some(Value::StringListOp(old))) => {
                        Value::StringListOp(merge_list_op(old, new))
                    }
                    _ => value.into_owned(),
                };
                dst.set_field(&path, &field, merged);
            }
        }
    }

    /// Merges the snippet into `stage`'s root layer as one transaction.
    pub fn author(&self, stage: &Stage) -> Result<(), String> {
        let root = stage.root_layer().identifier().to_owned();
        if let Some(layer) = stage.layer(&root) {
            self.check_against(layer.data())?;
        }
        stage
            .batch_edit(&[&root], |edits| {
                self.merge_into(edits[0].data_mut());
                Ok(())
            })
            .map(|_| ())
            .map_err(|e| format!("cannot author the USDA: {e}"))
    }
}

/// A token or string list op the snippet authors over the layer's
/// (`apiSchemas`, `variantSetNames`): item by item, so applying one schema
/// keeps the others the layer applies. An explicit list on either side
/// replaces, as it does in composition.
fn merge_list_op<T: Default + Clone + PartialEq>(
    old: sdf::ListOp<T>,
    new: &sdf::ListOp<T>,
) -> sdf::ListOp<T> {
    if new.explicit || old.explicit {
        return new.clone();
    }
    let union = |into: &mut Vec<T>, from: &[T]| {
        for item in from {
            if !into.contains(item) {
                into.push(item.clone());
            }
        }
    };
    let mut out = old;
    // An item the snippet adds is no longer deleted, and the reverse.
    for item in new
        .prepended_items
        .iter()
        .chain(&new.appended_items)
        .chain(&new.added_items)
    {
        out.deleted_items.retain(|d| d != item);
    }
    for item in &new.deleted_items {
        out.prepended_items.retain(|d| d != item);
        out.appended_items.retain(|d| d != item);
        out.added_items.retain(|d| d != item);
    }
    union(&mut out.prepended_items, &new.prepended_items);
    union(&mut out.appended_items, &new.appended_items);
    union(&mut out.added_items, &new.added_items);
    union(&mut out.deleted_items, &new.deleted_items);
    union(&mut out.ordered_items, &new.ordered_items);
    out
}

/// `prim`'s path as nested `over`s around `body` — the snippet that authors
/// `body`'s opinions at `prim` and nothing else.
fn overs(prim: &sdf::Path, metadata: &str, body: &str) -> Result<String, String> {
    let text = prim.to_string();
    if text.contains('{') || !prim.is_abs() {
        return Err(format!(
            "{prim} is not a prim path on the stage (no variant selections in it)"
        ));
    }
    let names: Vec<&str> = text.split('/').filter(|n| !n.is_empty()).collect();
    if names.is_empty() {
        return Err("the pseudo-root takes no opinion here".to_owned());
    }
    let mut out = String::new();
    for (depth, name) in names.iter().enumerate() {
        let pad = "    ".repeat(depth);
        let meta = if depth + 1 == names.len() && !metadata.is_empty() {
            format!(" (\n{pad}    {metadata}\n{pad})")
        } else {
            String::new()
        };
        out.push_str(&format!("{pad}over \"{name}\"{meta}\n{pad}{{\n"));
    }
    let pad = "    ".repeat(names.len());
    for line in body.lines() {
        out.push_str(&format!("{pad}{line}\n"));
    }
    for depth in (0..names.len()).rev() {
        out.push_str(&format!("{}}}\n", "    ".repeat(depth)));
    }
    Ok(out)
}

/// A string as a USDA string literal.
fn quoted(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What a value of a USD type is shaped like in JSON.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Bool,
    Integer,
    Number,
    Text,
    Asset,
    /// `n` numbers: `float3`, `color3f`, `quatf`, …
    Tuple(usize),
    /// `n` × `n` numbers: `matrix4d`.
    Matrix(usize),
}

fn shape(base: &str) -> Shape {
    let digit = base
        .chars()
        .find(|c| c.is_ascii_digit())
        .and_then(|c| c.to_digit(10));
    match base {
        "bool" => Shape::Bool,
        "int" | "int64" | "uint" | "uint64" | "uchar" => Shape::Integer,
        "float" | "double" | "half" | "timecode" => Shape::Number,
        "string" | "token" => Shape::Text,
        "asset" => Shape::Asset,
        b if b.starts_with("quat") => Shape::Tuple(4),
        b if b.starts_with("matrix") || b.starts_with("frame") => {
            Shape::Matrix(digit.unwrap_or(4) as usize)
        }
        _ => match digit {
            Some(n) => Shape::Tuple(n as usize),
            None => Shape::Number,
        },
    }
}

/// One element of `type_name`'s values as a USDA literal, or why `value` is
/// not one. `asset` rewrites an asset path for the layer (design D5).
fn literal(
    type_name: &str,
    shape: Shape,
    value: &serde_json::Value,
    asset: &dyn Fn(&str) -> String,
) -> Result<String, String> {
    use serde_json::Value as J;
    let wrong = || format!("{value} is not a {type_name}");
    let numbers = |items: &Vec<J>, n: usize| -> Result<String, String> {
        if items.len() != n || !items.iter().all(J::is_number) {
            return Err(format!(
                "{value} is not a {type_name}: it takes {n} numbers"
            ));
        }
        Ok(format!(
            "({})",
            items
                .iter()
                .map(J::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    };
    match (shape, value) {
        (Shape::Bool, J::Bool(b)) => Ok(if *b { "1" } else { "0" }.to_owned()),
        (Shape::Integer, J::Number(n)) if n.is_i64() || n.is_u64() => Ok(n.to_string()),
        (Shape::Number, J::Number(n)) => Ok(n.to_string()),
        (Shape::Text, J::String(s)) => Ok(quoted(s)),
        (Shape::Asset, J::String(s)) if !s.contains('@') => Ok(format!("@{}@", asset(s))),
        (Shape::Tuple(n), J::Array(items)) => numbers(items, n),
        (Shape::Matrix(n), J::Array(rows)) if rows.len() == n => {
            let rows: Result<Vec<String>, String> = rows
                .iter()
                .map(|r| match r {
                    J::Array(items) => numbers(items, n),
                    _ => Err(wrong()),
                })
                .collect();
            Ok(format!("({})", rows?.join(", ")))
        }
        _ => Err(wrong()),
    }
}

/// `value` as a USDA value of `type_name` (`float`, `color3f[]`, …).
pub fn value_literal(
    type_name: &str,
    value: &serde_json::Value,
    asset: &dyn Fn(&str) -> String,
) -> Result<String, String> {
    match type_name.strip_suffix("[]") {
        Some(base) => {
            let serde_json::Value::Array(items) = value else {
                return Err(format!("{value} is not a {type_name}: it takes an array"));
            };
            let items: Result<Vec<String>, String> = items
                .iter()
                .map(|v| literal(type_name, shape(base), v, asset))
                .collect();
            Ok(format!("[{}]", items?.join(", ")))
        }
        None => literal(type_name, shape(type_name), value, asset),
    }
}

/// The snippet `set_attribute` authors: `type_name` (declared or given)
/// and `uniform` as the composed attribute has them.
pub fn attribute_snippet(
    attribute: &sdf::Path,
    type_name: &str,
    uniform: bool,
    value: &serde_json::Value,
    asset: &dyn Fn(&str) -> String,
) -> Result<String, String> {
    let name = attribute.property_suffix().trim_start_matches('.');
    if name.is_empty() {
        return Err(format!("{attribute} is not an attribute path"));
    }
    let literal = value_literal(type_name, value, asset)?;
    let uniform = if uniform { "uniform " } else { "" };
    overs(
        &attribute.prim_path(),
        "",
        &format!("{uniform}{type_name} {name} = {literal}"),
    )
}

pub fn variant_snippet(prim: &sdf::Path, set: &str, variant: &str) -> Result<String, String> {
    overs(
        prim,
        &format!(
            "variants = {{\n        string {set} = {}\n    }}",
            quoted(variant)
        ),
        "",
    )
}

pub fn active_snippet(prim: &sdf::Path, active: bool) -> Result<String, String> {
    overs(prim, &format!("active = {active}"), "")
}

pub fn binding_snippet(prim: &sdf::Path, material: &sdf::Path) -> Result<String, String> {
    overs(
        prim,
        "prepend apiSchemas = [\"MaterialBindingAPI\"]",
        &format!("rel material:binding = <{material}>"),
    )
}

/// The op placement authors on a prim (`drop_to_surface`): one `matrix4d`,
/// first in the prim's `xformOpOrder` after any `!resetXformStack!`.
pub const PLACE_OP: &str = "xformOp:transform:crustPlace";

/// Opinions on several prims as one snippet: each `(prim, body)`'s body
/// inside nested `over`s down to its prim, the prims' common ancestors
/// written once. A body at the pseudo-root (`/`) is written at the top.
pub fn overs_tree(entries: &[(sdf::Path, String)]) -> Result<String, String> {
    #[derive(Default)]
    struct Node {
        body: String,
        children: Vec<(String, Node)>,
    }
    let mut root = Node::default();
    for (prim, body) in entries {
        let text = prim.to_string();
        if text.contains('{') || !prim.is_abs() || prim.is_property_path() {
            return Err(format!("{prim} is not a prim path on the stage"));
        }
        let mut node = &mut root;
        for name in text.split('/').filter(|n| !n.is_empty()) {
            let i = match node.children.iter().position(|(n, _)| n == name) {
                Some(i) => i,
                None => {
                    node.children.push((name.to_owned(), Node::default()));
                    node.children.len() - 1
                }
            };
            node = &mut node.children[i].1;
        }
        node.body.push_str(body);
        if !body.ends_with('\n') {
            node.body.push('\n');
        }
    }
    fn emit(node: &Node, depth: usize, out: &mut String) {
        let pad = "    ".repeat(depth);
        for line in node.body.lines() {
            out.push_str(&format!("{pad}{line}\n"));
        }
        for (name, child) in &node.children {
            out.push_str(&format!("{pad}over \"{name}\"\n{pad}{{\n"));
            emit(child, depth + 1, out);
            out.push_str(&format!("{pad}}}\n"));
        }
    }
    let mut out = String::new();
    emit(&root, 0, &mut out);
    Ok(out)
}

/// A `matrix4d` literal: glam's column-major array is USD's row-major one
/// (row vectors, translation in the last row).
fn matrix_literal(m: &glam::DMat4) -> String {
    let a = m.to_cols_array();
    let rows: Vec<String> = a
        .chunks(4)
        .map(|r| format!("({}, {}, {}, {})", r[0], r[1], r[2], r[3]))
        .collect();
    format!("( {} )", rows.join(", "))
}

/// The opinions that move the prim of `frame` by `delta` in world space,
/// applied after its current world transform: the [`PLACE_OP`] it composes
/// (identity when it has none) folded with `delta` conjugated into its
/// parent's space, so the prim's world transform becomes `delta · world`.
/// The op is listed after the last `!resetXformStack!` (whose parent space
/// is the world), else first, and the prim's composed ops keep their order.
pub fn placement_body(frame: &crust_core::live::PrimFrame, delta: &glam::DMat4) -> String {
    let parent = if frame.resets_xform_stack {
        glam::DMat4::IDENTITY
    } else {
        frame.parent_to_world
    };
    let mut order = frame.xform_op_order.clone();
    let listed = order.iter().any(|e| e == PLACE_OP);
    let current = match frame.place_op {
        Some(m) if listed => m,
        _ => glam::DMat4::IDENTITY,
    };
    let op = parent.inverse() * *delta * parent * current;
    if !listed {
        let at = order
            .iter()
            .rposition(|e| e == "!resetXformStack!")
            .map_or(0, |i| i + 1);
        order.insert(at, PLACE_OP.to_owned());
    }
    let order: Vec<String> = order.iter().map(|e| quoted(e)).collect();
    format!(
        "matrix4d {PLACE_OP} = {}\nuniform token[] xformOpOrder = [{}]",
        matrix_literal(&op),
        order.join(", ")
    )
}

/// Where a scattered prototype comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum ProtoSource {
    /// A prim of the stage, referenced internally.
    Prim(sdf::Path),
    /// An asset, its path already written for the layer ([`reanchor`]).
    Asset(String),
}

/// One prototype of a scatter: its child's name under `Prototypes`.
#[derive(Clone, Debug, PartialEq)]
pub struct Prototype {
    pub name: String,
    pub source: ProtoSource,
}

/// The `def PointInstancer` a scatter authors, named `name`, whose parent's
/// local-to-world is `parent`: each instance's world placement (its
/// prototype's origin at `position`, turned `angle` about `up`, scaled
/// uniformly) as `positions`, `orientations`, `scales` and `protoIndices`
/// in the parent's space, and one typeless child per prototype under
/// `Prototypes` referencing its source with an empty `xformOpOrder`, so it is
/// instanced at its own origin. Refused when the parent's space would shear
/// an instance, which the arrays cannot express.
pub fn instancer_body(
    name: &str,
    path: &sdf::Path,
    parent: &glam::DMat4,
    up: glam::DVec3,
    instances: &[crust_core::place::Instance],
    prototypes: &[Prototype],
) -> Result<String, String> {
    let to_parent = parent.inverse();
    let mut positions = Vec::with_capacity(instances.len());
    let mut orientations = Vec::with_capacity(instances.len());
    let mut scales = Vec::with_capacity(instances.len());
    let mut indices = Vec::with_capacity(instances.len());
    for i in instances {
        let world = glam::DMat4::from_scale_rotation_translation(
            glam::DVec3::splat(i.scale),
            glam::DQuat::from_axis_angle(up, i.angle),
            i.position,
        );
        let local = to_parent * world;
        let (s, r, t) = local.to_scale_rotation_translation();
        let back = glam::DMat4::from_scale_rotation_translation(s, r, t);
        let size = local
            .to_cols_array()
            .iter()
            .fold(1f64, |m, v| m.max(v.abs()));
        let off = (local - back)
            .to_cols_array()
            .iter()
            .fold(0f64, |m, v| m.max(v.abs()));
        if off > 1e-5 * size {
            return Err(format!(
                "{path}'s parent space shears an instance turned about the up axis (a rotation \
                 with a non-uniform scale): put the instancer under a prim without one"
            ));
        }
        let (s, r, t) = (s.as_vec3(), r.as_quat(), t.as_vec3());
        positions.push(format!("({}, {}, {})", t.x, t.y, t.z));
        orientations.push(format!("({}, {}, {}, {})", r.w, r.x, r.y, r.z));
        scales.push(format!("({}, {}, {})", s.x, s.y, s.z));
        indices.push(i.prototype.to_string());
    }
    let targets: Vec<String> = prototypes
        .iter()
        .map(|p| format!("<{path}/Prototypes/{}>", p.name))
        .collect();
    let mut protos = String::new();
    for p in prototypes {
        let reference = match &p.source {
            ProtoSource::Prim(prim) => format!("<{prim}>"),
            ProtoSource::Asset(asset) => format!("@{asset}@"),
        };
        // Typeless: a `def Xform` would win over the referenced prim's type.
        protos.push_str(&format!(
            "    def {} (\n        prepend references = {reference}\n    )\n    {{\n        \
             uniform token[] xformOpOrder = []\n    }}\n",
            quoted(&p.name)
        ));
    }
    Ok(format!(
        "def PointInstancer {}\n{{\n    point3f[] positions = [{}]\n    quath[] orientations = \
         [{}]\n    float3[] scales = [{}]\n    int[] protoIndices = [{}]\n    rel prototypes = \
         [{}]\n\n    def Scope \"Prototypes\"\n    {{\n{protos}    }}\n}}\n",
        quoted(name),
        positions.join(", "),
        orientations.join(", "),
        scales.join(", "),
        indices.join(", "),
        targets.join(", ")
    ))
}

/// An asset path as given to an edit tool, written for the override layer
/// (design D5): a relative path is read against the input stage's directory
/// — how `query` shows the input's own paths — and rewritten against the
/// layer's, so it names the same file; an absolute path, an empty one and
/// a URI (`scheme://…`) are kept.
pub fn reanchor(path: &str, input_dir: &Path, layer_dir: &Path) -> String {
    if path.is_empty() || path.contains("://") || Path::new(path).is_absolute() {
        return path.to_owned();
    }
    let target = input_dir.join(path);
    let target = normalize(&target);
    super::layer::anchored(&target, layer_dir)
}

/// `path` with its `.` and `..` components folded, without touching the
/// file system.
fn normalize(path: &Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn keep(s: &str) -> String {
        s.to_owned()
    }

    #[test]
    fn values_take_their_declared_types_shape() {
        assert_eq!(value_literal("float", &json!(-0.5), &keep).unwrap(), "-0.5");
        assert_eq!(value_literal("int", &json!(3), &keep).unwrap(), "3");
        assert!(value_literal("int", &json!(3.5), &keep).is_err());
        assert_eq!(
            value_literal("color3f", &json!([1, 0.5, 0]), &keep).unwrap(),
            "(1, 0.5, 0)"
        );
        assert!(value_literal("color3f", &json!([1, 0.5]), &keep).is_err());
        assert_eq!(
            value_literal("float3[]", &json!([[0, 1, 2], [3, 4, 5]]), &keep).unwrap(),
            "[(0, 1, 2), (3, 4, 5)]"
        );
        assert_eq!(
            value_literal("token", &json!("a\"b"), &keep).unwrap(),
            "\"a\\\"b\""
        );
        assert_eq!(value_literal("bool", &json!(true), &keep).unwrap(), "1");
        assert_eq!(
            value_literal("asset", &json!("./t.png"), &keep).unwrap(),
            "@./t.png@"
        );
        let e = value_literal("float", &json!("bright"), &keep).unwrap_err();
        assert!(e.contains("float"), "{e}");
        assert_eq!(
            value_literal("matrix2d", &json!([[1, 0], [0, 1]]), &keep).unwrap(),
            "((1, 0), (0, 1))"
        );
    }

    #[test]
    fn snippets_nest_overs_down_to_the_prim() {
        let attr = sdf::Path::new("/lights/key.inputs:exposure").unwrap();
        let text = attribute_snippet(&attr, "float", false, &json!(-0.5), &keep).unwrap();
        assert_eq!(
            text,
            "over \"lights\"\n{\n    over \"key\"\n    {\n        float inputs:exposure = -0.5\n    }\n}\n"
        );
        Snippet::parse(&text).expect("parses");
        let prim = sdf::Path::new("/asset").unwrap();
        Snippet::parse(&variant_snippet(&prim, "lod", "high").unwrap()).expect("parses");
        Snippet::parse(&active_snippet(&prim, false).unwrap()).expect("parses");
        let mat = sdf::Path::new("/Looks/red").unwrap();
        Snippet::parse(&binding_snippet(&prim, &mat).unwrap()).expect("parses");
    }

    #[test]
    fn a_snippet_may_not_change_the_sublayers() {
        let e = Snippet::parse("#usda 1.0\n(\n    subLayers = [@./x.usda@]\n)\n")
            .err()
            .expect("refused");
        assert!(e.contains("subLayers"), "{e}");
        assert!(Snippet::parse("over \"a\" {").is_err());
    }

    #[test]
    fn relative_assets_move_from_the_inputs_directory_to_the_layers() {
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        let p = |s: &str| std::path::PathBuf::from(format!("{root}{s}"));
        assert_eq!(
            reanchor("./tex/wood.png", &p("shots/a"), &p("work")),
            "../shots/a/tex/wood.png"
        );
        assert_eq!(
            reanchor("tex/wood.png", &p("shots/a"), &p("work")),
            "../shots/a/tex/wood.png"
        );
        assert_eq!(reanchor("./t.png", &p("w"), &p("w")), "./t.png");
        let abs = p("x/t.png").to_string_lossy().into_owned();
        assert_eq!(reanchor(&abs, &p("shots"), &p("work")), abs);
    }

    /// A frame whose parent rotates and scales non-uniformly, listing ops of
    /// its own.
    fn frame(order: &[&str], place_op: Option<glam::DMat4>) -> crust_core::live::PrimFrame {
        let parent = glam::DMat4::from_scale_rotation_translation(
            glam::DVec3::new(2.0, 0.5, 3.0),
            glam::DQuat::from_euler(glam::EulerRot::XYZ, 0.3, -0.7, 1.1),
            glam::DVec3::new(10.0, -4.0, 2.5),
        );
        crust_core::live::PrimFrame {
            path: "/World/a/b".into(),
            parent_to_world: parent,
            local_to_world: parent,
            xform_op_order: order.iter().map(|s| s.to_string()).collect(),
            resets_xform_stack: order.contains(&"!resetXformStack!"),
            xformable: true,
            place_op,
        }
    }

    /// The op and the order `placement_body` authors on `/World/a/b`, read
    /// back through the USDA parser.
    fn authored(body: &str) -> (glam::DMat4, Vec<String>) {
        let prim = sdf::Path::new("/World/a/b").unwrap();
        let text = overs_tree(&[(prim, body.to_owned())]).unwrap();
        let snippet = Snippet::parse(&text).expect("parses");
        let field = |name: &str| {
            let path = sdf::Path::new(&format!("/World/a/b.{name}")).unwrap();
            snippet.data.try_field(&path, "default").unwrap().unwrap()
        };
        let op = match field(PLACE_OP).as_ref() {
            Value::Matrix4d(m) => glam::DMat4::from_cols_array(&m.0),
            v => panic!("{v:?}"),
        };
        let order = match field("xformOpOrder").as_ref() {
            Value::TokenVec(o) => o.iter().map(|t| t.as_str().to_owned()).collect(),
            v => panic!("{v:?}"),
        };
        (op, order)
    }

    /// The authored op moves the prim by exactly `delta` in world space,
    /// whatever its parent's rotation and scale; it goes first, after a
    /// reset, keeping the prim's ops; a second drop folds into it.
    #[test]
    fn a_placement_moves_the_prim_by_delta_in_world_space() {
        let delta = glam::DMat4::from_translation(glam::DVec3::new(0.0, -1.25, 0.0))
            * glam::DMat4::from_rotation_z(0.2);
        let f = frame(&["xformOp:translate", "xformOp:rotateY"], None);
        let (op, order) = authored(&placement_body(&f, &delta));
        assert_eq!(order, [PLACE_OP, "xformOp:translate", "xformOp:rotateY"]);
        // World' = P · op · L = Δ · P · L.
        let p = f.parent_to_world;
        assert!((p * op * p.inverse()).abs_diff_eq(delta, 1e-9));

        // Again: the op folds, listed once.
        let f2 = frame(
            &order.iter().map(String::as_str).collect::<Vec<_>>(),
            Some(op),
        );
        let (op2, order2) = authored(&placement_body(&f2, &delta));
        assert_eq!(order2, order);
        assert!((p * op2 * p.inverse()).abs_diff_eq(delta * delta, 1e-9));

        // After a reset, the parent space is the world's.
        let f3 = frame(
            &["xformOp:scale", "!resetXformStack!", "xformOp:translate"],
            None,
        );
        let (op3, order3) = authored(&placement_body(&f3, &delta));
        assert_eq!(
            order3,
            [
                "xformOp:scale",
                "!resetXformStack!",
                PLACE_OP,
                "xformOp:translate"
            ]
        );
        assert!(op3.abs_diff_eq(delta, 1e-12));
    }

    /// Instances land where they were placed in world space, under a parent
    /// that rotates and scales uniformly; a shearing parent is refused.
    #[test]
    fn an_instancer_places_its_instances_in_its_parents_space() {
        use crust_core::place::Instance;
        let up = glam::DVec3::Y;
        let instances = [
            Instance {
                position: glam::DVec3::new(1.0, 2.0, 3.0),
                angle: 0.7,
                scale: 1.5,
                prototype: 1,
            },
            Instance {
                position: glam::DVec3::new(-4.0, 0.0, 0.5),
                angle: 0.0,
                scale: 0.5,
                prototype: 0,
            },
        ];
        let protos = [
            Prototype {
                name: "rock".into(),
                source: ProtoSource::Prim(sdf::Path::new("/World/rock").unwrap()),
            },
            Prototype {
                name: "stone".into(),
                source: ProtoSource::Asset("./stone.usda".into()),
            },
        ];
        let parent = glam::DMat4::from_scale_rotation_translation(
            glam::DVec3::splat(2.0),
            glam::DQuat::from_rotation_x(0.4),
            glam::DVec3::new(5.0, 0.0, 0.0),
        );
        let path = sdf::Path::new("/World/scatter").unwrap();
        let body = instancer_body("scatter", &path, &parent, up, &instances, &protos).unwrap();
        let text = overs_tree(&[(sdf::Path::new("/World").unwrap(), body)]).unwrap();
        let snippet = Snippet::parse(&text).expect("parses");
        let field = |name: &str| {
            let p = sdf::Path::new(&format!("/World/scatter.{name}")).unwrap();
            snippet.data.try_field(&p, "default").unwrap().unwrap()
        };
        let Value::Vec3fVec(pos) = field("positions").as_ref().clone() else {
            panic!("positions")
        };
        let Value::QuathVec(rot) = field("orientations").as_ref().clone() else {
            panic!("orientations")
        };
        let Value::Vec3fVec(scl) = field("scales").as_ref().clone() else {
            panic!("scales")
        };
        let Value::IntVec(idx) = field("protoIndices").as_ref().clone() else {
            panic!("protoIndices")
        };
        assert_eq!(idx, [1, 0]);
        for (k, i) in instances.iter().enumerate() {
            let (q, s, p) = (rot[k], scl[k], pos[k]);
            let local = glam::DMat4::from_scale_rotation_translation(
                glam::DVec3::new(s.x as f64, s.y as f64, s.z as f64),
                glam::DQuat::from_xyzw(q.x.to_f64(), q.y.to_f64(), q.z.to_f64(), q.w.to_f64())
                    .normalize(),
                glam::DVec3::new(p.x as f64, p.y as f64, p.z as f64),
            );
            let world = glam::DMat4::from_scale_rotation_translation(
                glam::DVec3::splat(i.scale),
                glam::DQuat::from_axis_angle(up, i.angle),
                i.position,
            );
            // Half-precision orientations: a few thousandths.
            assert!((parent * local).abs_diff_eq(world, 5e-3), "instance {k}");
        }
        let proto =
            |name: &str| sdf::Path::new(&format!("/World/scatter/Prototypes/{name}")).unwrap();
        assert!(
            snippet
                .data
                .try_field(&proto("rock"), "typeName")
                .unwrap()
                .is_none(),
            "typeless"
        );
        assert!(
            snippet
                .data
                .try_field(&proto("stone"), "references")
                .unwrap()
                .is_some()
        );

        let shear = glam::DMat4::from_scale_rotation_translation(
            glam::DVec3::new(1.0, 3.0, 1.0),
            glam::DQuat::from_rotation_x(0.4),
            glam::DVec3::ZERO,
        );
        let e = instancer_body("scatter", &path, &shear, up, &instances, &protos).unwrap_err();
        assert!(e.contains("shears"), "{e}");
    }
}
