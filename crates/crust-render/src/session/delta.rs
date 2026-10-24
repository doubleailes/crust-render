//! How an edit batch is applied (design D1 of `live-stage-scene-deltas`):
//! the override layer before and after the batch (or an undo) compared spec
//! by spec and field by field ([`layer_diff`]), and the changes put into the
//! cheapest class that can follow all of them ([`classify`]).
//!
//! The classes are an allow-list. A change is `settings`, `lights` or
//! `materials` only when it is an attribute value (`default` or
//! `timeSamples`) the table names; anything else — a spec defined or
//! removed, a relationship or connection, metadata, a transform, a variant —
//! is `full`, a re-import. Namespace bookkeeping is not a change: the `over`
//! prim specs an edit authors to reach an attribute, and the children lists
//! that name them, compose to nothing on their own.

use crust_core::live::{Change, Provenance};
use openusd::sdf::{self, AbstractData, SpecType, Specifier, Value};
use openusd::usd::Stage;
use std::fmt;

/// How an edit batch was applied, cheapest first: a batch is applied as the
/// most expensive class any of its changes needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Applied {
    /// The settings and the camera re-read; world and lights kept.
    Settings,
    /// Materials rebuilt into their slots; the BVH and light list kept.
    Materials,
    /// Lights rebuilt and the light selection re-run; the BVH kept.
    Lights,
    /// Geometry placed or read again, and the top-level BVH committed again
    /// from it; everything else kept.
    Geometry,
    /// The saved layer imported again.
    Full,
}

impl Applied {
    pub fn as_str(self) -> &'static str {
        match self {
            Applied::Settings => "settings",
            Applied::Materials => "materials",
            Applied::Lights => "lights",
            Applied::Geometry => "geometry",
            Applied::Full => "full",
        }
    }
}

impl fmt::Display for Applied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One change between two versions of a layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerChange {
    /// The value (`default` or `timeSamples`) of the attribute at this
    /// property path.
    Value(sdf::Path),
    /// Anything else, described.
    Other(String),
}

/// The fields an attribute spec may carry and still be only a value: the
/// value, and the declaration that comes with authoring it.
const VALUE_FIELDS: &[&str] = &["default", "timeSamples"];
const DECLARATION_FIELDS: &[&str] = &["typeName", "variability", "custom"];
/// The prim fields that only name children.
const CHILDREN: &[&str] = &["primChildren", "propertyChildren"];

fn field(data: &dyn AbstractData, path: &sdf::Path, name: &str) -> Option<Value> {
    data.try_field(path, name)
        .ok()
        .flatten()
        .map(|v| v.into_owned())
}

/// The changes a spec present on one side only makes: an `over` that only
/// names children makes none, an attribute that only declares and sets its
/// value is a value change, anything else is a change of its own.
fn lone_spec(data: &dyn AbstractData, path: &sdf::Path, ty: SpecType, out: &mut Vec<LayerChange>) {
    let fields = data.list_fields(path).unwrap_or_default();
    match ty {
        SpecType::Prim => {
            let over = matches!(
                field(data, path, "specifier"),
                None | Some(Value::Specifier(Specifier::Over))
            );
            let only_children = fields
                .iter()
                .all(|f| f == "specifier" || CHILDREN.contains(&f.as_str()));
            if !(over && only_children) {
                out.push(LayerChange::Other(format!("the prim spec {path}")));
            }
        }
        SpecType::Attribute => {
            let declares = fields.iter().all(|f| {
                VALUE_FIELDS.contains(&f.as_str()) || DECLARATION_FIELDS.contains(&f.as_str())
            });
            if !declares {
                out.push(LayerChange::Other(format!("the attribute spec {path}")));
            } else if fields.iter().any(|f| VALUE_FIELDS.contains(&f.as_str())) {
                out.push(LayerChange::Value(path.clone()));
            }
        }
        _ => out.push(LayerChange::Other(format!("the {ty:?} spec {path}"))),
    }
}

/// Every change from `before` to `after`, in spec path order.
pub fn layer_diff(before: &dyn AbstractData, after: &dyn AbstractData) -> Vec<LayerChange> {
    let mut paths = before.spec_paths();
    paths.extend(after.spec_paths());
    paths.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    paths.dedup();
    let mut out = Vec::new();
    for path in paths {
        match (before.spec_type(&path), after.spec_type(&path)) {
            (None, None) => {}
            (Some(ty), None) => lone_spec(before, &path, ty, &mut out),
            (None, Some(ty)) => lone_spec(after, &path, ty, &mut out),
            (Some(a), Some(b)) if a != b => {
                out.push(LayerChange::Other(format!(
                    "{path} changed from {a:?} to {b:?}"
                )));
            }
            (Some(ty), Some(_)) => {
                let mut fields = before.list_fields(&path).unwrap_or_default();
                fields.extend(after.list_fields(&path).unwrap_or_default());
                fields.sort();
                fields.dedup();
                let mut value = false;
                for f in fields {
                    if field(before, &path, &f) == field(after, &path, &f) {
                        continue;
                    }
                    match ty {
                        SpecType::Prim | SpecType::PseudoRoot if CHILDREN.contains(&f.as_str()) => {
                        }
                        SpecType::Attribute if VALUE_FIELDS.contains(&f.as_str()) => value = true,
                        _ => out.push(LayerChange::Other(format!("{f} on {path}"))),
                    }
                }
                if value {
                    out.push(LayerChange::Value(path.clone()));
                }
            }
        }
    }
    out
}

/// Mesh attributes whose change reads the mesh again: its points, topology,
/// subdivision tags and primvars.
const MESH_SHAPE: &[&str] = &[
    "points",
    "normals",
    "extent",
    "faceVertexCounts",
    "faceVertexIndices",
    "holeIndices",
    "cornerIndices",
    "cornerSharpnesses",
    "creaseIndices",
    "creaseLengths",
    "creaseSharpnesses",
    "subdivisionScheme",
    "interpolateBoundary",
    "faceVaryingLinearInterpolation",
    "triangleSubdivisionRule",
    "orientation",
];

/// A light input that shapes the light's geometry: the BVH holds it.
const SHAPE_INPUTS: &[&str] = &[
    "inputs:width",
    "inputs:height",
    "inputs:radius",
    "inputs:length",
    "inputs:treatAsPoint",
    "inputs:treatAsLine",
];

/// The attribute at `path`'s name, after its prim path.
fn property_name(path: &sdf::Path) -> Option<&str> {
    let prim = path.prim_path();
    path.as_str().strip_prefix(prim.as_str())?.strip_prefix('.')
}

/// The class each change needs, and the parts an in-place update rebuilds.
/// `stage` is the session's authoring stage, after the batch: a prim's type
/// is the composed one, so an `over` classifies by what the prim is.
pub fn classify(
    changes: &[LayerChange],
    stage: &Stage,
    provenance: &Provenance,
) -> (Applied, Change, Option<String>) {
    let mut applied = Applied::Settings;
    let mut change = Change::default();
    let full = |why: String| (Applied::Full, Change::default(), Some(why));
    // A transform on the render camera is a settings edit — the rebuild
    // re-reads the camera, and refuses when it moves what tessellation read —
    // unless an imported prim lies below the camera: the rebuild does not
    // move that.
    let carries = std::cell::OnceCell::new();
    let camera_carries = |camera: &str| *carries.get_or_init(|| provenance.any_below(camera));
    for c in changes {
        let attr = match c {
            LayerChange::Value(p) => p,
            LayerChange::Other(why) => return full(why.clone()),
        };
        let Some(name) = property_name(attr) else {
            return full(format!("{attr} is not an attribute"));
        };
        let prim = attr.prim_path();
        let prim_str = prim.as_str();
        let is_xform = name.starts_with("xformOp") || name == "xformOpOrder";
        let input = name.starts_with("inputs:");
        if provenance.render_settings() == Some(prim_str)
            || (provenance.camera() == Some(prim_str) && !(is_xform && camera_carries(prim_str)))
        {
            change.settings = true;
        } else if input && let Some(light) = provenance.light(prim_str) {
            push(&mut change.lights, prim_str);
            if SHAPE_INPUTS.contains(&name) && !light.geoms.is_empty() {
                // The emitter's geometry follows the input.
                push(&mut change.light_geometry, prim_str);
                applied = applied.max(Applied::Geometry);
            } else {
                applied = applied.max(Applied::Lights);
            }
        } else if is_xform && provenance.camera() != Some(prim_str) {
            if let Some(why) = provenance.movable(prim_str) {
                return full(format!("{attr}: {why}"));
            }
            let mut emits = false;
            for light in provenance.lights_at(prim_str) {
                push(&mut change.lights, light);
                if provenance.light(light).is_some_and(|l| !l.geoms.is_empty()) {
                    push(&mut change.light_geometry, light);
                    emits = true;
                }
            }
            let meshes = provenance
                .geometry_of(prim_str)
                .into_iter()
                .flatten()
                .any(|id| !provenance.lights().any(|(_, l)| l.geoms.contains(&id)));
            if meshes {
                push(&mut change.moved, prim_str);
            }
            // `movable` refused a subtree holding neither geometry nor a
            // light: what is left places geometry, emitters, or lights only.
            applied = applied.max(if meshes || emits {
                Applied::Geometry
            } else {
                Applied::Lights
            });
        } else if MESH_SHAPE.contains(&name) || name.starts_with("primvars:") {
            if let Some(why) = provenance.reshapable(prim_str) {
                return full(format!("{attr}: {why}"));
            }
            push(&mut change.reshaped, prim_str);
            applied = applied.max(Applied::Geometry);
        } else if let Some((material, typed)) = input
            .then(|| owning_material(stage, &prim, provenance))
            .flatten()
        {
            if let Some(reader) = provenance.other_readers(prim_str, &material).next() {
                return full(format!("{attr} is also read by {reader}"));
            }
            if !typed && !change.shaders.iter().any(|s| s == prim_str) {
                change.shaders.push(prim_str.to_owned());
            }
            if !change.materials.contains(&material) {
                change.materials.push(material);
            }
            applied = applied.max(Applied::Materials);
        } else {
            return full(format!("{attr}"));
        }
    }
    (applied, change, None)
}

/// Adds `path` to `list` once.
fn push(list: &mut Vec<String>, path: &str) {
    if !list.iter().any(|p| p == path) {
        list.push(path.to_owned());
    }
}

/// The material an input at `prim` belongs to: `prim` itself when it is a
/// material the scene uses (an interface input), else the nearest ancestor
/// the scene uses when `prim` is a `Shader` in its network. The flag says
/// whether that type was read here: the authoring stage leaves payloads
/// unloaded, so a shader a payload brings in composes only as the edit's
/// typeless `over`, and the rebuild checks it on the material's own stage
/// (`Change::shaders`).
fn owning_material(
    stage: &Stage,
    prim: &sdf::Path,
    provenance: &Provenance,
) -> Option<(String, bool)> {
    if provenance.material(prim.as_str()).is_some() {
        return Some((prim.as_str().to_owned(), true));
    }
    let ty = stage
        .prim(prim.clone())
        .ok()
        .and_then(|p| p.type_name().ok().flatten())
        .map(|t| t.to_string());
    let typed = match ty.as_deref() {
        Some("Shader") => true,
        None => false,
        Some(_) => return None,
    };
    let mut cur = prim.parent();
    while let Some(p) = cur {
        if p.is_abs_root() {
            return None;
        }
        if provenance.material(p.as_str()).is_some() {
            return Some((p.as_str().to_owned(), typed));
        }
        cur = p.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(text: &str) -> sdf::Data {
        openusd::usda::parse(&format!("#usda 1.0\n{text}")).expect("parses")
    }

    fn diff(before: &str, after: &str) -> Vec<LayerChange> {
        layer_diff(&data(before), &data(after))
    }

    fn value(path: &str) -> LayerChange {
        LayerChange::Value(sdf::Path::new(path).unwrap())
    }

    fn others(changes: &[LayerChange]) -> usize {
        changes
            .iter()
            .filter(|c| matches!(c, LayerChange::Other(_)))
            .count()
    }

    /// Setting an attribute on a fresh layer authors `over`s down to it and
    /// the attribute: one value change, the namespace nothing.
    #[test]
    fn a_set_value_is_one_value_change() {
        let after = r#"over "W" { over "L" { float inputs:intensity = 2 } }"#;
        assert_eq!(diff("", after), [value("/W/L.inputs:intensity")]);
        let again = r#"over "W" { over "L" { float inputs:intensity = 3 } }"#;
        assert_eq!(diff(after, again), [value("/W/L.inputs:intensity")]);
        // Undo is the same change backwards.
        assert_eq!(diff(after, ""), [value("/W/L.inputs:intensity")]);
        assert!(diff(after, after).is_empty());
    }

    #[test]
    fn an_added_spec_is_a_change_of_its_own() {
        let after = r#"def Sphere "S" { }"#;
        assert_eq!(others(&diff("", after)), 1);
        let rel = r#"over "S" { rel material:binding = </M> }"#;
        assert_eq!(others(&diff("", rel)), 1);
    }

    #[test]
    fn a_removed_spec_is_a_change_of_its_own() {
        let before = r#"def Sphere "S" { double radius = 2 }"#;
        assert_eq!(others(&diff(before, "")), 1);
    }

    #[test]
    fn a_time_sample_is_a_value_change() {
        let before = r#"over "L" { float inputs:intensity.timeSamples = { 1: 2 } }"#;
        let after = r#"over "L" { float inputs:intensity.timeSamples = { 1: 3 } }"#;
        assert_eq!(diff(before, after), [value("/L.inputs:intensity")]);
    }

    #[test]
    fn a_relationship_or_connection_is_not_a_value() {
        let before = r#"over "S" { rel material:binding = </A> }"#;
        let after = r#"over "S" { rel material:binding = </B> }"#;
        assert_eq!(others(&diff(before, after)), 1);
        let conn = r#"over "S" { color3f inputs:c.connect = </T.outputs:rgb> }"#;
        assert_eq!(others(&diff("", conn)), 1);
        let meta = r#"over "S" ( active = false ) { }"#;
        assert_eq!(others(&diff("", meta)), 1);
    }
}
