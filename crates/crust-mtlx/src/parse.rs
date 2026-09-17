//! `.mtlx` document → a name-addressable node graph.
//!
//! MaterialX's on-disk form is flat: every node is a direct child of
//! `<materialx>` (or of a `<nodegraph>`), carries a unique `name`, and refers
//! to its upstream neighbours *by that name* through an input's `nodename`
//! attribute. So parsing is one pass into a `Vec<Node>` plus a name index;
//! nothing is resolved here, because a node may legally reference one declared
//! later in the file.
//!
//! Deliberately partial. This reads the subset a look-dev document actually
//! uses — nodes, their inputs, and nodegraph outputs — and ignores the parts
//! that describe *code generation* rather than appearance: `<nodedef>` and
//! `<implementation>` declare custom node signatures for a shader compiler,
//! and the two teapot/lion documents declare one (an OCIO colour transform)
//! that they never instantiate. Skipping them is not a gap in the shading.

use super::value::{Val, parse_literal};
use std::collections::HashMap;

/// Where one input's value comes from.
#[derive(Clone, Debug)]
pub enum Source {
    /// An authored literal.
    Value(Val),
    /// Another node's output. `output` names which one for a `multioutput`
    /// node (`artistic_ior` yields both `ior` and `extinction`), and is
    /// `None` for the single-output majority.
    Node { name: String, output: Option<String> },
    /// A nodegraph's output, reached as `nodegraph="G" output="out"`.
    Graph { graph: String, output: String },
}

/// One input of one node.
#[derive(Clone, Debug)]
pub struct Input {
    pub name: String,
    pub type_name: String,
    pub source: Source,
    /// The `colorspace` attribute, verbatim. Only meaningful on an `image`'s
    /// `file` input, where it decides whether the texels are display-encoded.
    /// Handed to the texture loader as a string: mapping the spellings onto a
    /// colour-space enum is the host's business (crust-core's `ColorSpace`).
    pub colorspace: Option<String>,
    /// The raw `value` attribute text, kept because not every MaterialX value
    /// is a number: `filename` and `string` inputs carry asset paths and
    /// tokens, which [`Source::Value`] has no way to hold.
    pub text: Option<String>,
}

/// One node: its category (the XML tag, which *is* the operator), its name,
/// its declared output type, and its inputs.
#[derive(Clone, Debug)]
pub struct Node {
    /// The XML tag — `multiply`, `image`, `dielectric_bsdf`, …
    pub category: String,
    pub name: String,
    pub type_name: String,
    pub inputs: Vec<Input>,
    /// Set for nodes parsed inside a `<nodegraph>`, so a name collision
    /// between two graphs cannot merge them.
    pub graph: Option<String>,
}

impl Node {
    /// The input called `name`, if authored. An unauthored input falls back to
    /// its nodedef default, which the evaluator supplies per operator.
    pub fn input(&self, name: &str) -> Option<&Input> {
        self.inputs.iter().find(|i| i.name == name)
    }
}

/// A parsed `.mtlx` document.
pub struct Doc {
    pub nodes: Vec<Node>,
    /// `(graph, name) → index`, where `graph` is empty at document scope.
    index: HashMap<(String, String), usize>,
    /// `(graph, output name) → the node name it forwards to`, from a
    /// `<nodegraph>`'s `<output nodename=…>` children.
    graph_outputs: HashMap<(String, String), String>,
}

/// Why a `.mtlx` could not be turned into a material.
#[derive(Debug)]
pub enum MtlxError {
    Io(std::io::Error),
    Xml(roxmltree::Error),
    /// The document parsed, but the requested material node is not in it.
    NoSuchMaterial(String),
}

impl std::fmt::Display for MtlxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MtlxError::Io(e) => write!(f, "cannot read: {e}"),
            MtlxError::Xml(e) => write!(f, "malformed XML: {e}"),
            MtlxError::NoSuchMaterial(n) => write!(f, "no material node named '{n}'"),
        }
    }
}

impl Doc {
    /// Parses a `.mtlx` file.
    pub fn open(path: &std::path::Path) -> Result<Doc, MtlxError> {
        let text = std::fs::read_to_string(path).map_err(MtlxError::Io)?;
        Doc::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Doc, MtlxError> {
        let text = escape_udim_tokens(text);
        let dom = roxmltree::Document::parse(&text).map_err(MtlxError::Xml)?;
        let mut doc = Doc {
            nodes: Vec::new(),
            index: HashMap::new(),
            graph_outputs: HashMap::new(),
        };
        for child in dom.root_element().children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                // Declarations for a shader compiler, not appearance.
                "nodedef" | "implementation" | "typedef" | "look" | "variantset" => {}
                "nodegraph" => {
                    let g = child.attribute("name").unwrap_or_default().to_string();
                    for inner in child.children().filter(|n| n.is_element()) {
                        if inner.tag_name().name() == "output" {
                            // A graph output is a rename, not a node: record
                            // where it points so a downstream `nodegraph=`
                            // reference can be followed in one step.
                            if let (Some(name), Some(target)) =
                                (inner.attribute("name"), inner.attribute("nodename"))
                            {
                                doc.graph_outputs
                                    .insert((g.clone(), name.to_string()), target.to_string());
                            }
                        } else {
                            doc.push(parse_node(inner, Some(g.clone())));
                        }
                    }
                }
                // A document-scope `<output>` is the same rename at the top
                // level; it keys on the empty graph name.
                "output" => {
                    if let (Some(name), Some(target)) =
                        (child.attribute("name"), child.attribute("nodename"))
                    {
                        doc.graph_outputs
                            .insert((String::new(), name.to_string()), target.to_string());
                    }
                }
                _ => doc.push(parse_node(child, None)),
            }
        }
        Ok(doc)
    }

    fn push(&mut self, node: Node) {
        let key = (
            node.graph.clone().unwrap_or_default(),
            node.name.clone(),
        );
        // First declaration wins. MaterialX requires names unique within a
        // scope, so a duplicate is a malformed document; keeping the first
        // keeps the graph acyclic-by-construction rather than half-rewired.
        if self.index.contains_key(&key) {
            return;
        }
        self.index.insert(key, self.nodes.len());
        self.nodes.push(node);
    }

    /// Looks up a node by name within a scope (`graph` empty for document
    /// scope), falling back to document scope so a node inside a graph can
    /// reference one outside it.
    pub fn find(&self, graph: &str, name: &str) -> Option<&Node> {
        self.index
            .get(&(graph.to_string(), name.to_string()))
            .or_else(|| self.index.get(&(String::new(), name.to_string())))
            .map(|&i| &self.nodes[i])
    }

    /// Resolves `nodegraph="G" output="o"` to the node the graph's output
    /// forwards to.
    pub fn graph_output(&self, graph: &str, output: &str) -> Option<&Node> {
        let target = self
            .graph_outputs
            .get(&(graph.to_string(), output.to_string()))?;
        self.find(graph, target)
    }

    /// Every node of a given category, in document order.
    pub fn by_category<'a>(&'a self, category: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.nodes.iter().filter(move |n| n.category == category)
    }
}

/// Escapes MaterialX's UDIM/UVTILE placeholders so the document parses.
///
/// This is not a nicety. A `.mtlx` addresses a UDIM set as
/// `value="../Texture/Albedo.<UDIM>.png"` — a bare `<` **inside an attribute
/// value**, which XML forbids outright. MaterialX's own reader is PugiXML,
/// which accepts it; every conformant parser (`roxmltree` included) rejects
/// the whole document with an `InvalidChar`. The shipped MaterialX Teapot and
/// Lion assets are written this way, so without this the documents do not
/// parse at all — not "the UDIM path is wrong", but no material.
///
/// The substitution is deliberately narrow: only the two literal tokens the
/// MaterialX specification defines, which no element in its schema is named,
/// so this cannot swallow real markup. `roxmltree` decodes the entities back,
/// leaving the attribute holding exactly `<UDIM>` again.
fn escape_udim_tokens(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("<UDIM>") && !text.contains("<UVTILE>") {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(
        text.replace("<UDIM>", "&lt;UDIM&gt;")
            .replace("<UVTILE>", "&lt;UVTILE&gt;"),
    )
}

fn parse_node(el: roxmltree::Node<'_, '_>, graph: Option<String>) -> Node {
    let type_name = el.attribute("type").unwrap_or("float").to_string();
    let mut inputs = Vec::new();
    for i in el.children().filter(|n| n.is_element()) {
        // 1.38 authored parameters as `<parameter>`; 1.39 folded them into
        // `<input>`. Both mean the same thing here.
        if i.tag_name().name() != "input" && i.tag_name().name() != "parameter" {
            continue;
        }
        let name = i.attribute("name").unwrap_or_default().to_string();
        let itype = i.attribute("type").unwrap_or("float").to_string();
        let text = i.attribute("value").map(str::to_string);
        let source = if let Some(n) = i.attribute("nodename") {
            Source::Node {
                name: n.to_string(),
                output: i.attribute("output").map(str::to_string),
            }
        } else if let Some(g) = i.attribute("nodegraph") {
            Source::Graph {
                graph: g.to_string(),
                output: i.attribute("output").unwrap_or("out").to_string(),
            }
        } else {
            // A `filename` or `string` value is not numeric; it rides in
            // `text` and the numeric source stays a harmless zero that no
            // operator consuming those types ever reads.
            Source::Value(
                text.as_deref()
                    .and_then(|v| parse_literal(v, &itype))
                    .unwrap_or(Val::ZERO),
            )
        };
        inputs.push(Input {
            name,
            type_name: itype,
            source,
            colorspace: i.attribute("colorspace").map(str::to_string),
            text,
        });
    }
    Node {
        category: el.tag_name().name().to_string(),
        name: el.attribute("name").unwrap_or_default().to_string(),
        type_name,
        inputs,
        graph,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<?xml version="1.0"?>
<materialx version="1.38">
  <constant name="c" type="color3">
    <input name="value" type="color3" value="0.2, 0.4, 0.8" />
  </constant>
  <image name="tex" type="color3">
    <input name="file" type="filename" value="../t/a.<UDIM>.png" colorspace="srgb_texture" />
  </image>
  <multiply name="m" type="color3">
    <input name="in1" type="color3" nodename="c" />
    <input name="in2" type="color3" nodename="tex" />
  </multiply>
  <nodedef name="ND_custom" node="custom" />
</materialx>"#;

    #[test]
    fn nodes_index_by_name_and_keep_connections() {
        let d = Doc::parse(DOC).unwrap();
        let m = d.find("", "m").unwrap();
        assert_eq!(m.category, "multiply");
        match &m.input("in1").unwrap().source {
            Source::Node { name, .. } => assert_eq!(name, "c"),
            other => panic!("expected a node connection, got {other:?}"),
        }
    }

    #[test]
    fn filenames_survive_as_text_not_as_numbers() {
        let d = Doc::parse(DOC).unwrap();
        let f = d.find("", "tex").unwrap().input("file").unwrap();
        assert_eq!(f.text.as_deref(), Some("../t/a.<UDIM>.png"));
        assert_eq!(f.colorspace.as_deref(), Some("srgb_texture"));
    }

    #[test]
    fn a_bare_udim_token_in_an_attribute_still_parses() {
        // The shipped DPEL assets author `value="...<UDIM>.png"`, which is
        // malformed XML. Rejecting it loses the whole material, so the reader
        // escapes the token first — and must hand it back unescaped.
        let d = Doc::parse(DOC).unwrap();
        let f = d.find("", "tex").unwrap().input("file").unwrap();
        assert!(f.text.as_deref().unwrap().contains("<UDIM>"));
    }

    #[test]
    fn nodedefs_are_not_nodes() {
        // They declare signatures for a shader compiler. Treating one as a
        // node would put an uninstantiated operator in the graph.
        let d = Doc::parse(DOC).unwrap();
        assert!(d.find("", "ND_custom").is_none());
    }
}
