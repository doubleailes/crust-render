//! `query`: what the authoring stage composes at a prim or an attribute, and
//! which layers say so. Values are read at the default time, as the
//! session's import (no frame) reads them.

use openusd::sdf;
use openusd::usd::{LoadPolicy, Prim, Stage};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Arrays longer than this are answered with their length and first
/// elements: a mesh's points would drown the answer.
const MAX_ELEMENTS: usize = 16;

/// An asset path as authored, and the file it resolves to (`null` when it
/// resolves to none).
fn asset_json(asset: &sdf::AssetPath) -> Value {
    json!({ "authored": asset.authored_path, "resolved": asset.resolved_path() })
}

/// A value as JSON, long arrays shortened.
fn value_json(value: &sdf::Value) -> Value {
    let full = match value {
        sdf::Value::AssetPath(a) => asset_json(a),
        sdf::Value::AssetPathVec(v) => Value::Array(v.iter().map(asset_json).collect()),
        other => serde_json::to_value(other).unwrap_or(Value::Null),
    };
    match full {
        Value::Array(items) if items.len() > MAX_ELEMENTS => json!({
            "length": items.len(),
            "first": items[..MAX_ELEMENTS],
        }),
        other => other,
    }
}

/// `path`'s prim, its payload loaded when it has one: the authoring stage
/// opens with payloads unloaded and loads them only where a query reaches.
pub(super) fn prim_at(stage: &Stage, path: &sdf::Path) -> Result<Prim, String> {
    let prim = stage.prim(path.clone()).map_err(|e| e.to_string())?;
    if !prim.is_valid().unwrap_or(false) {
        // Perhaps under a payload not loaded yet.
        stage
            .load(path.clone(), LoadPolicy::WithoutDescendants)
            .map_err(|e| e.to_string())?;
    }
    let prim = stage.prim(path.clone()).map_err(|e| e.to_string())?;
    if !prim.is_valid().unwrap_or(false) {
        return Err(format!("no prim at {path}"));
    }
    if !prim.is_loaded().unwrap_or(true) {
        prim.load(LoadPolicy::WithoutDescendants);
    }
    Ok(prim)
}

/// The layers that author `path`'s prim, strongest first.
fn prim_layers(prim: &Prim) -> Vec<String> {
    let mut layers: Vec<String> = Vec::new();
    for site in prim.prim_stack().unwrap_or_default() {
        if !layers.contains(&site.layer) {
            layers.push(site.layer);
        }
    }
    layers
}

/// A variant set as the stage composes it on a prim.
pub(super) struct VariantSet {
    /// The effective selection, if any.
    pub selection: Option<String>,
    /// The variants the prim's specs author, in first-seen order.
    pub variants: Vec<String>,
}

/// The variant sets the prim's specs author, each with its variants.
pub(super) fn variant_sets(stage: &Stage, prim: &Prim) -> BTreeMap<String, VariantSet> {
    let mut sets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for site in prim.prim_stack().unwrap_or_default() {
        let Some(layer) = stage.layer(&site.layer) else {
            continue;
        };
        let data = layer.data();
        let names = match data.try_field(&site.path, "variantSetChildren") {
            Ok(Some(v)) => match v.as_ref() {
                sdf::Value::TokenVec(t) => t.iter().map(|t| t.as_str().to_owned()).collect(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        for set in names {
            let variants = sets.entry(set.clone()).or_default();
            let Ok(set_path) = site.path.append_variant_selection(&set, "") else {
                continue;
            };
            if let Ok(Some(v)) = data.try_field(&set_path, "variantChildren")
                && let sdf::Value::TokenVec(t) = v.as_ref()
            {
                for name in t {
                    if !variants.iter().any(|n| n == name.as_str()) {
                        variants.push(name.as_str().to_owned());
                    }
                }
            }
        }
    }
    let selections: BTreeMap<String, String> = prim
        .variant_sets()
        .get_all_variant_selections()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut out = BTreeMap::new();
    for name in sets.keys().chain(selections.keys()) {
        out.insert(
            name.clone(),
            VariantSet {
                selection: selections.get(name).cloned(),
                variants: sets.get(name).cloned().unwrap_or_default(),
            },
        );
    }
    out
}

fn prim_json(stage: &Stage, prim: &Prim) -> Value {
    let attributes: Vec<Value> = prim
        .authored_attributes()
        .unwrap_or_default()
        .iter()
        .map(|a| {
            json!({
                "name": a.path().property_suffix().trim_start_matches('.'),
                "type": a.type_name().ok().flatten().map(|t| t.to_string()),
                "value": a.get::<sdf::Value>().ok().flatten().as_ref().map(value_json),
            })
        })
        .collect();
    let mut relationships = Map::new();
    for r in prim.authored_relationships().unwrap_or_default() {
        let targets: Vec<String> = r
            .targets()
            .unwrap_or_default()
            .iter()
            .map(|p| p.to_string())
            .collect();
        relationships.insert(
            r.path()
                .property_suffix()
                .trim_start_matches('.')
                .to_owned(),
            json!(targets),
        );
    }
    json!({
        "path": prim.path().to_string(),
        "type": prim.type_name().ok().flatten().map(|t| t.to_string()),
        "specifier": prim.specifier().ok().flatten().map(|s| format!("{s:?}").to_lowercase()),
        "active": prim.is_active().unwrap_or(true),
        "loaded": prim.is_loaded().unwrap_or(true),
        "children": prim
            .child_names()
            .unwrap_or_default()
            .iter()
            .map(|t| t.as_str().to_owned())
            .collect::<Vec<_>>(),
        "variant_sets": variant_sets(stage, prim)
            .into_iter()
            .map(|(name, set)| {
                (name, json!({ "selection": set.selection, "variants": set.variants }))
            })
            .collect::<Map<String, Value>>(),
        "attributes": attributes,
        "relationships": relationships,
        "layers": prim_layers(prim),
    })
}

fn attribute_json(stage: &Stage, path: &sdf::Path) -> Result<Value, String> {
    prim_at(stage, &path.prim_path())?;
    let attr = stage.attribute(path.clone()).map_err(|e| e.to_string())?;
    let Some(type_name) = attr.type_name().ok().flatten() else {
        return Err(format!(
            "no layer authors {path}. If a schema declares it, its type and fallback are not known \
             here: neither openusd 0.7 nor openusd-schemas 0.7 ships schema data, and the \
             session registers none. Set it with set_attribute and an explicit `type`"
        ));
    };
    let mut layers: Vec<String> = Vec::new();
    for site in attr.property_stack().unwrap_or_default() {
        if !layers.contains(&site.layer) {
            layers.push(site.layer);
        }
    }
    let value = attr.get::<sdf::Value>().ok().flatten();
    Ok(json!({
        "path": path.to_string(),
        "type": type_name.to_string(),
        "value": value.as_ref().map(value_json),
        // Where the value comes from: the strongest layer that authors it,
        // or the schema's fallback when none does.
        "source": layers.first().cloned().unwrap_or_else(|| "fallback".to_owned()),
        "layers": layers,
        "time_samples": attr.num_time_samples().unwrap_or(0),
    }))
}

/// `query`: a prim (`/a/b`) or an attribute (`/a/b.inputs:x`).
pub fn query(stage: &Stage, path: &str) -> Result<Value, String> {
    let path = sdf::Path::new(path).map_err(|e| format!("{path:?} is not a USD path: {e}"))?;
    if path.is_property_path() {
        attribute_json(stage, &path)
    } else {
        Ok(prim_json(stage, &prim_at(stage, &path)?))
    }
}
