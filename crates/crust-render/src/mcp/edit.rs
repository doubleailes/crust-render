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
//! - an `over` never downgrades a `def` or a `class` already there.
//!
//! List ops (`apiSchemas`, `references`, …) are replaced as a whole, as any
//! other field.

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
}
