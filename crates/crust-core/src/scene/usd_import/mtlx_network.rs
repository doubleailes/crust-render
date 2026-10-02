//! `UsdShade` MaterialX networks → [`crust_mtlx::Doc`]s.
//!
//! A `.mtlx` referenced from a `Material` is read by crust-mtlx directly
//! (`materials::load_mtlx_material`). This is the other way MaterialX reaches
//! a stage: authored *inline*, as `Shader` prims whose `info:id` is a
//! MaterialX nodedef (`ND_open_pbr_surface_surfaceshader`,
//! `ND_anisotropic_vdf`, `ND_volume`) wired to the material's `surface` and
//! `volume` terminals — which is how UsdMtlx composes a document into a
//! stage, how Houdini and Maya export one, and how NVIDIA's Typhoon (the
//! OpenUSD reference renderer's hdEmbree) receives every material it renders.
//!
//! The network is translated node for node into the document crust-mtlx's
//! parser would have produced from the equivalent XML — the category and type
//! from the nodedef name, each input's literal formatted as the text a `value`
//! attribute holds and read back through the same
//! [`crust_mtlx::value::parse_literal`] — so a network shades exactly as the
//! same network in a `.mtlx` does. Connections through `NodeGraph` outputs and
//! through `Material` / `NodeGraph` interface inputs are followed to the
//! shader output or the literal they forward.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crust_mtlx::{Doc, Input, Node, Source, Val};
use openusd::sdf;
use openusd::usd::{Attribute, Prim, Stage};

use super::materials::{attribute_asset_path, shader_info_id};
use super::prim_at;
use super::time::eval_time;

/// How many forwarding hops (graph outputs, interface inputs) one connection
/// may take before the translation gives up on it — a cycle, in practice.
const MAX_HOPS: usize = 32;

/// A translated network: the document, and the node each terminal names.
pub(super) struct Network {
    pub(super) doc: Doc,
    pub(super) surface: Option<String>,
    pub(super) volume: Option<String>,
    /// What could not be translated, for one warning per material.
    pub(super) reported: Vec<String>,
}

/// Whether a shader id names a MaterialX nodedef.
pub(super) fn is_mtlx_id(id: &str) -> bool {
    id.starts_with("ND_")
}

/// Translates the networks under the shader prims `surface` and `volume`
/// (either may be absent) into one document.
pub(super) fn translate(
    stage: &Stage,
    stage_path: &Path,
    surface: Option<&Prim>,
    volume: Option<&Prim>,
) -> Network {
    let mut b = Builder {
        stage,
        stage_path,
        nodes: Vec::new(),
        names: HashMap::new(),
        active: HashSet::new(),
        reported: Vec::new(),
    };
    let surface = surface.and_then(|p| b.node(p));
    let volume = volume.and_then(|p| b.node(p));
    Network {
        doc: Doc::from_nodes(b.nodes),
        surface,
        volume,
        reported: b.reported,
    }
}

struct Builder<'s> {
    stage: &'s Stage,
    stage_path: &'s Path,
    nodes: Vec<Node>,
    /// Shader prim path → its node's name (the path itself), once translated.
    names: HashMap<String, String>,
    /// Shader prims being translated, for cycle detection.
    active: HashSet<String>,
    reported: Vec<String>,
}

impl Builder<'_> {
    /// The node for the shader prim `prim`, translating it (and everything
    /// upstream of it) on first use. `None` for a prim that is not a
    /// MaterialX shader.
    fn node(&mut self, prim: &Prim) -> Option<String> {
        let path = prim.path().as_str().to_string();
        if let Some(name) = self.names.get(&path) {
            return Some(name.clone());
        }
        if !self.active.insert(path.clone()) {
            self.reported
                .push(format!("{path}: connection cycle — cut"));
            return None;
        }
        let shader = openusd_schemas::shade::Shader::get(self.stage, prim.path().clone())
            .ok()
            .flatten();
        let id = shader.as_ref().and_then(shader_info_id);
        let Some(id) = id.filter(|id| is_mtlx_id(id)) else {
            self.reported.push(format!(
                "{path}: not a MaterialX shader ({}) — its input falls back to the default",
                shader
                    .as_ref()
                    .and_then(shader_info_id)
                    .unwrap_or_else(|| "no info:id".into())
            ));
            self.active.remove(&path);
            return None;
        };
        let (category, nd_type) = nodedef_category(&id);
        let type_name = node_type(&category, nd_type, prim);

        let mut inputs = Vec::new();
        for attr in prim.attributes().unwrap_or_default() {
            let full = attr.path().split_property().map(|(_, n)| n.to_string());
            let Some(name) = full.as_deref().and_then(|n| n.strip_prefix("inputs:")) else {
                continue;
            };
            if let Some(input) = self.input(name, &attr) {
                inputs.push(input);
            }
        }
        self.active.remove(&path);
        self.names.insert(path.clone(), path.clone());
        self.nodes.push(Node {
            category,
            name: path.clone(),
            type_name,
            inputs,
            graph: None,
            version: None,
        });
        Some(path)
    }

    /// One `inputs:<name>` attribute as a MaterialX input: a connection to
    /// the node it resolves to, or its literal.
    fn input(&mut self, name: &str, attr: &Attribute) -> Option<Input> {
        let usd_type = attr
            .type_name()
            .ok()
            .flatten()
            .map(|t| t.as_str().to_string())
            .unwrap_or_default();
        match self.resolve(attr, 0)? {
            Resolved::Node { name: node, output } => {
                let type_name = self
                    .nodes
                    .iter()
                    .find(|n| n.name == node)
                    .map(|n| n.type_name.clone())
                    .unwrap_or_else(|| mtlx_type(&usd_type).to_string());
                Some(Input {
                    name: name.to_string(),
                    type_name,
                    source: Source::Node { name: node, output },
                    colorspace: None,
                    text: None,
                })
            }
            Resolved::Value(owner) => {
                let ty = owner
                    .type_name()
                    .ok()
                    .flatten()
                    .map(|t| t.as_str().to_string())
                    .unwrap_or(usd_type);
                let mtype = mtlx_type(&ty);
                let text = self.literal_text(&owner, mtype)?;
                let source = Source::Value(
                    crust_mtlx::value::parse_literal(&text, mtype).unwrap_or(Val::ZERO),
                );
                let colorspace = owner
                    .get_metadata::<sdf::Value>("colorSpace")
                    .ok()
                    .flatten()
                    .and_then(|v| v.as_str().map(str::to_owned));
                Some(Input {
                    name: name.to_string(),
                    type_name: mtype.to_string(),
                    source,
                    colorspace,
                    text: Some(text),
                })
            }
        }
    }

    /// Follows `attr`'s connection — through graph outputs and interface
    /// inputs — to a shader output or to the attribute whose value it takes.
    /// `None` when it leads nowhere (an unconnected, unauthored interface
    /// input, a non-MaterialX shader, a cycle): the input stays unauthored
    /// and takes its nodedef default.
    fn resolve(&mut self, attr: &Attribute, hops: usize) -> Option<Resolved> {
        let targets = attr.connections().unwrap_or_default();
        let Some(target) = targets.first() else {
            // An `inputs:` / `outputs:` with no connection forwards its own
            // value, if it has one.
            return attr
                .get_at::<sdf::Value>(eval_time())
                .ok()
                .flatten()
                .map(|_| Resolved::Value(attr.clone()));
        };
        if hops >= MAX_HOPS {
            self.reported
                .push(format!("{}: connection chain too long — cut", attr.path()));
            return None;
        }
        let (prim_path, prop) = target.split_property()?;
        let prim = prim_at(self.stage, prim_path);
        let is_shader = prim
            .type_name()
            .ok()
            .flatten()
            .is_some_and(|t| t.as_str() == "Shader");
        match prop.strip_prefix("outputs:") {
            Some(output) if is_shader => {
                let name = self.node(&prim)?;
                Some(Resolved::Node {
                    name,
                    output: (output != "out").then(|| output.to_string()),
                })
            }
            // A container's output or interface input forwards whatever it
            // is connected to, or its own value.
            _ => {
                let next = prim.attribute(prop);
                self.resolve(&next, hops + 1)
            }
        }
    }

    /// An attribute's value as the text a MaterialX `value` attribute would
    /// hold. `filename`s are resolved against the layer that authored them,
    /// as every USD asset path is, and written absolute.
    fn literal_text(&mut self, attr: &Attribute, mtype: &str) -> Option<String> {
        let value = attr.get_at::<sdf::Value>(eval_time()).ok().flatten()?;
        let join = |v: &[f64]| {
            v.iter()
                .map(|x| format!("{}", *x as f32))
                .collect::<Vec<_>>()
                .join(", ")
        };
        Some(match &value {
            sdf::Value::Bool(b) => b.to_string(),
            sdf::Value::Int(i) => i.to_string(),
            sdf::Value::Float(x) => format!("{x}"),
            sdf::Value::Double(x) => format!("{}", *x as f32),
            sdf::Value::Vec2f(v) => join(&[v.x as f64, v.y as f64]),
            sdf::Value::Vec2d(v) => join(&[v.x, v.y]),
            sdf::Value::Vec3f(v) => join(&[v.x as f64, v.y as f64, v.z as f64]),
            sdf::Value::Vec3d(v) => join(&[v.x, v.y, v.z]),
            sdf::Value::Vec4f(v) => join(&[v.x as f64, v.y as f64, v.z as f64, v.w as f64]),
            sdf::Value::Vec4d(v) => join(&[v.x, v.y, v.z, v.w]),
            sdf::Value::String(s) => s.clone(),
            sdf::Value::Token(t) => t.as_str().to_string(),
            sdf::Value::AssetPath(_) if mtype == "filename" => {
                attribute_asset_path(attr, self.stage_path)?
                    .to_string_lossy()
                    .into_owned()
            }
            sdf::Value::AssetPath(p) => p.as_str().to_string(),
            other => {
                self.reported.push(format!(
                    "{}: value of type {:?} not translated",
                    attr.path(),
                    std::mem::discriminant(other)
                ));
                return None;
            }
        })
    }
}

/// Where a connection ends.
enum Resolved {
    /// A shader output: the node, and the output when not the default `out`.
    Node {
        name: String,
        output: Option<String>,
    },
    /// The attribute whose value the input takes.
    Value(Attribute),
}

/// The MaterialX type tokens a nodedef name spells its signature with.
const TYPES: [&str; 18] = [
    "float",
    "integer",
    "boolean",
    "string",
    "filename",
    "color3",
    "color4",
    "vector2",
    "vector3",
    "vector4",
    "matrix33",
    "matrix44",
    "surfaceshader",
    "volumeshader",
    "displacementshader",
    "lightshader",
    "material",
    "multioutput",
];

/// The closure types, which a nodedef name spells as a signature only on the
/// combinators (`ND_mix_vdf`, `ND_multiply_bsdfC`); on a leaf
/// (`ND_dielectric_bsdf`, `ND_anisotropic_vdf`) it is part of the category.
const CLOSURES: [&str; 3] = ["bsdf", "edf", "vdf"];
const COMBINATORS: [&str; 4] = ["mix", "add", "multiply", "layer"];

/// The signature token `tok` spells, without the variant letters MaterialX
/// appends (`vdfC`, `color3FA`, `floatB`), if it is one.
fn type_token(tok: &str) -> Option<&str> {
    let base = tok.trim_end_matches(|c: char| c.is_ascii_uppercase());
    (TYPES.contains(&base) || CLOSURES.contains(&base)).then_some(base)
}

/// Splits a nodedef name into the node's category and the first type its
/// signature names: `ND_open_pbr_surface_surfaceshader` →
/// (`open_pbr_surface`, `surfaceshader`), `ND_mix_vdf` → (`mix`, `vdf`),
/// `ND_anisotropic_vdf` → (`anisotropic_vdf`, none),
/// `ND_standard_surface_surfaceshader_100` → (`standard_surface`,
/// `surfaceshader`). MaterialX names every standard nodedef this way.
fn nodedef_category(id: &str) -> (String, Option<&str>) {
    let rest = id.strip_prefix("ND_").unwrap_or(id);
    let mut toks: Vec<&str> = rest.split('_').collect();
    // A versioned nodedef: `_100`.
    if toks.len() > 1
        && toks
            .last()
            .is_some_and(|t| t.chars().all(|c| c.is_ascii_digit()))
    {
        toks.pop();
    }
    let mut first = None;
    while toks.len() > 1 {
        let Some(t) = type_token(toks[toks.len() - 1]) else {
            break;
        };
        if CLOSURES.contains(&t)
            && !COMBINATORS.contains(&toks[..toks.len() - 1].join("_").as_str())
        {
            break;
        }
        toks.pop();
        first = Some(t);
    }
    (toks.join("_"), first)
}

/// The MaterialX type of a node: a closure or shader type when its category
/// or signature says so, else its `outputs:out`'s USD type, else the
/// signature's.
fn node_type(category: &str, signature: Option<&str>, prim: &Prim) -> String {
    let closure = |t: &str| match t {
        "bsdf" => Some("BSDF"),
        "edf" => Some("EDF"),
        "vdf" => Some("VDF"),
        _ => None,
    };
    if let Some(t) = CLOSURES
        .iter()
        .find(|c| category.ends_with(&format!("_{c}")))
        .and_then(|c| closure(c))
    {
        return t.to_string();
    }
    match category {
        "surface" | "surface_unlit" => return "surfaceshader".into(),
        "volume" => return "volumeshader".into(),
        "surfacematerial" | "volumematerial" => return "material".into(),
        _ => {}
    }
    if let Some(sig) = signature {
        if let Some(c) = closure(sig) {
            return c.to_string();
        }
        if sig.ends_with("shader") || sig == "material" {
            return sig.to_string();
        }
    }
    let out = prim
        .attribute("outputs:out")
        .type_name()
        .ok()
        .flatten()
        .map(|t| t.as_str().to_string());
    match out.as_deref().map(mtlx_type) {
        Some(t) if t != "string" => t.to_string(),
        _ => signature.unwrap_or("float").to_string(),
    }
}

/// The MaterialX spelling of a USD value type.
fn mtlx_type(usd: &str) -> &'static str {
    match usd {
        "float" | "double" | "half" => "float",
        "int" | "uint" | "int64" => "integer",
        "bool" => "boolean",
        "color3f" | "color3d" | "color3h" => "color3",
        "color4f" | "color4d" | "color4h" => "color4",
        "float2" | "double2" | "half2" | "texCoord2f" | "texCoord2d" => "vector2",
        "float3" | "double3" | "half3" | "vector3f" | "vector3d" | "normal3f" | "normal3d"
        | "point3f" | "point3d" => "vector3",
        "float4" | "double4" | "half4" => "vector4",
        "matrix3d" => "matrix33",
        "matrix4d" => "matrix44",
        "asset" => "filename",
        _ => "string",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodedef_names_split_into_category_and_signature() {
        let cases = [
            (
                "ND_open_pbr_surface_surfaceshader",
                "open_pbr_surface",
                Some("surfaceshader"),
            ),
            (
                "ND_standard_surface_surfaceshader_100",
                "standard_surface",
                Some("surfaceshader"),
            ),
            ("ND_anisotropic_vdf", "anisotropic_vdf", None),
            ("ND_dielectric_bsdf", "dielectric_bsdf", None),
            ("ND_mix_vdf", "mix", Some("vdf")),
            ("ND_multiply_vdfC", "multiply", Some("vdf")),
            ("ND_layer_vdf", "layer", Some("vdf")),
            ("ND_mix_volumeshader", "mix", Some("volumeshader")),
            ("ND_volume", "volume", None),
            ("ND_surface", "surface", None),
            ("ND_surface_unlit", "surface_unlit", None),
            ("ND_surfacematerial", "surfacematerial", None),
            ("ND_image_color3", "image", Some("color3")),
            ("ND_convert_float_color3", "convert", Some("float")),
            ("ND_multiply_color3FA", "multiply", Some("color3")),
            ("ND_artistic_ior", "artistic_ior", None),
            (
                "ND_UsdPreviewSurface_surfaceshader",
                "UsdPreviewSurface",
                Some("surfaceshader"),
            ),
        ];
        for (id, category, sig) in cases {
            let (c, s) = nodedef_category(id);
            assert_eq!((c.as_str(), s), (category, sig), "{id}");
        }
    }

    #[test]
    fn usd_types_map_to_materialx_spellings() {
        assert_eq!(mtlx_type("color3f"), "color3");
        assert_eq!(mtlx_type("normal3f"), "vector3");
        assert_eq!(mtlx_type("texCoord2f"), "vector2");
        assert_eq!(mtlx_type("asset"), "filename");
        assert_eq!(mtlx_type("token"), "string");
    }
}
