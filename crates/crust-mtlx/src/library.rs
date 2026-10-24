//! Custom node definitions: `<nodedef>`s and the `<nodegraph nodedef=…>`
//! graphs that implement them.
//!
//! A studio's node library (a DCC's own nodes, a show's helpers) ships
//! as `.mtlx` files that declare each node's signature in a `<nodedef>` and
//! implement it as a node graph over standard nodes. USD's `usdMtlx` plugin
//! finds those files through `PXR_MTLX_PLUGIN_SEARCH_PATHS`, and a document
//! may carry its own definitions too. This module reads both into one
//! [`Library`]; it resolves nothing and instantiates nothing.
//!
//! A definition is found by nodedef name ([`Library::nodedef`]), which is what
//! a USD shader's `info:id` names, or by node category and output type
//! ([`Library::for_node`]), which is what a node in a document names. The two
//! differ for libraries whose nodedef names break MaterialX's
//! `ND_<category>_<type>` convention: a nodedef named `studio_texture` may
//! name category `studio_texture`, and `ND_studio_layer_color3` category
//! `studio_layer`.
//!
//! Only node-graph implementations are kept. An `<implementation>` element
//! names source code for a shader generator, which crust has no use for, so a
//! nodedef implemented that way has no [`Implementation`] here.

use super::MtlxError;
use super::parse::{Node, parse_node};
use super::value::{Val, parse_literal};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One input a nodedef declares: its type and default.
#[derive(Clone, Debug)]
pub struct DefInput {
    pub name: String,
    pub type_name: String,
    /// The `value` attribute as a number, when it reads as one for the type.
    pub value: Option<Val>,
    /// The `value` attribute verbatim, for `filename`, `string` and the
    /// other inputs a number cannot hold.
    pub text: Option<String>,
}

/// One output a nodedef declares.
#[derive(Clone, Debug)]
pub struct DefOutput {
    pub name: String,
    pub type_name: String,
}

/// A `<nodedef>`: the signature of one node category.
#[derive(Clone, Debug)]
pub struct NodeDef {
    /// The nodedef's own name, `ND_…` by convention only.
    pub name: String,
    /// The category its instances are written as (the `node` attribute).
    pub node: String,
    /// Its inputs in declaration order, before `inherit` is applied (see
    /// [`Library::inputs`]).
    pub inputs: Vec<DefInput>,
    pub outputs: Vec<DefOutput>,
    /// The nodedef it inherits inputs from.
    pub inherit: Option<String>,
    pub version: Option<String>,
    /// `isdefaultversion="true"`: the one a category lookup prefers.
    pub default_version: bool,
}

impl NodeDef {
    /// The type an instance of this nodedef is written with: its one
    /// output's type, or `multioutput` for several.
    pub fn instance_type(&self) -> &str {
        match self.outputs.as_slice() {
            [one] => &one.type_name,
            [] => "",
            _ => "multioutput",
        }
    }

    /// The declared output called `name`.
    pub fn output(&self, name: &str) -> Option<&DefOutput> {
        self.outputs.iter().find(|o| o.name == name)
    }
}

/// A graph output: the node inside the implementation it forwards to, and
/// which of that node's outputs it selects.
#[derive(Clone, Debug)]
pub struct ImplOutput {
    pub name: String,
    pub type_name: String,
    pub node: String,
    pub output: Option<String>,
}

/// A `<nodegraph nodedef=…>`: a nodedef's implementation as a graph.
#[derive(Clone, Debug)]
pub struct Implementation {
    /// The graph's own name (`NG_studio_texture`).
    pub name: String,
    /// The nodedef it implements.
    pub nodedef: String,
    /// Its nodes, scoped to the graph ([`Node::graph`] is [`Self::name`]).
    /// An input bound to the nodedef's interface carries the input's name in
    /// [`Input::interface`](super::Input::interface).
    pub nodes: Vec<Node>,
    pub outputs: Vec<ImplOutput>,
}

/// Node definitions and their graph implementations, from any number of
/// documents.
///
/// The first definition of a name wins, so a document's own definitions,
/// merged first, take precedence over a library's, and of two library files
/// the one read first does (files are read in path order).
#[derive(Clone, Debug, Default)]
pub struct Library {
    defs: Vec<NodeDef>,
    by_name: HashMap<String, usize>,
    impls: HashMap<String, Implementation>,
    /// The files [`Library::load`] read, in order.
    pub files: Vec<PathBuf>,
}

/// How deep an `inherit` chain is followed before it is taken for a cycle.
const MAX_INHERIT: usize = 16;

impl Library {
    /// Every `.mtlx` file under `paths` (directories walked recursively, in
    /// sorted order; a path naming a file is read as one), each file's
    /// definitions merged in turn. A directory or file that cannot be read
    /// is returned beside the library, which keeps everything else.
    pub fn load(paths: &[PathBuf]) -> (Library, Vec<(PathBuf, MtlxError)>) {
        let mut lib = Library::default();
        let mut errors = Vec::new();
        let mut files = Vec::new();
        let mut walked = HashSet::new();
        for p in paths {
            collect_mtlx(p, &mut files, &mut walked, &mut errors);
        }
        for file in files {
            match std::fs::read_to_string(&file) {
                Ok(text) => match lib.merge_text(&text) {
                    Ok(()) => lib.files.push(file),
                    Err(e) => errors.push((file, e)),
                },
                Err(e) => errors.push((file, MtlxError::Io(e))),
            }
        }
        (lib, errors)
    }

    /// Merges the definitions of one document's text.
    pub fn merge_text(&mut self, text: &str) -> Result<(), MtlxError> {
        let text = super::parse::escape_udim_tokens(text);
        let dom = roxmltree::Document::parse(&text).map_err(MtlxError::Xml)?;
        self.merge_dom(dom.root_element());
        Ok(())
    }

    /// Merges every definition of `other` that names something not yet
    /// defined.
    pub fn merge(&mut self, other: &Library) {
        for def in &other.defs {
            self.add_def(def.clone());
        }
        for (name, imp) in &other.impls {
            self.impls
                .entry(name.clone())
                .or_insert_with(|| imp.clone());
        }
    }

    /// Reads the `<nodedef>` and `<nodegraph nodedef=…>` children of a
    /// `<materialx>` element.
    pub(crate) fn merge_dom(&mut self, root: roxmltree::Node<'_, '_>) {
        for child in root.children().filter(|n| n.is_element()) {
            match child.tag_name().name() {
                "nodedef" => self.add_def(parse_nodedef(child)),
                "nodegraph" => {
                    if let Some(imp) = parse_implementation(child) {
                        self.impls.entry(imp.nodedef.clone()).or_insert(imp);
                    }
                }
                _ => {}
            }
        }
    }

    fn add_def(&mut self, def: NodeDef) {
        if self.by_name.contains_key(&def.name) {
            return;
        }
        self.by_name.insert(def.name.clone(), self.defs.len());
        self.defs.push(def);
    }

    /// Whether the library defines nothing.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Every nodedef, in the order read.
    pub fn nodedefs(&self) -> &[NodeDef] {
        &self.defs
    }

    /// The nodedef called `name`.
    pub fn nodedef(&self, name: &str) -> Option<&NodeDef> {
        self.by_name.get(name).map(|&i| &self.defs[i])
    }

    /// The nodedef a node of `category` written with type `type_name`
    /// instantiates: among the nodedefs for that category whose
    /// [`NodeDef::instance_type`] is `type_name` (any, when `None`), the one
    /// whose `version` is `version` when the node authors one — none, if no
    /// nodedef has it — else the one marked the default version, else the
    /// first read.
    pub fn for_node(
        &self,
        category: &str,
        type_name: Option<&str>,
        version: Option<&str>,
    ) -> Option<&NodeDef> {
        let mut candidates = self
            .defs
            .iter()
            .filter(|d| d.node == category && type_name.is_none_or(|t| d.instance_type() == t));
        if let Some(v) = version {
            return candidates.find(|d| d.version.as_deref() == Some(v));
        }
        let first = candidates.next()?;
        if first.default_version {
            return Some(first);
        }
        Some(candidates.find(|d| d.default_version).unwrap_or(first))
    }

    /// Whether any nodedef is for `category`, whatever its type or version.
    pub fn defines(&self, category: &str) -> bool {
        self.defs.iter().any(|d| d.node == category)
    }

    /// How many nodedefs have a graph implementation.
    pub fn implemented(&self) -> usize {
        self.impls.len()
    }

    /// The graph implementing the nodedef called `nodedef`, if it has one.
    pub fn implementation(&self, nodedef: &str) -> Option<&Implementation> {
        self.impls.get(nodedef)
    }

    /// `def`'s inputs with `inherit` applied: the inherited nodedef's first,
    /// in its order, each overridden by an input of the same name declared
    /// below it, then the inputs only `def` declares. A chain longer than
    /// [`MAX_INHERIT`] (a cycle) stops there.
    pub fn inputs(&self, def: &NodeDef) -> Vec<DefInput> {
        let mut chain = vec![def];
        while let Some(parent) = chain
            .last()
            .and_then(|d| d.inherit.as_deref())
            .and_then(|p| self.nodedef(p))
            && chain.len() < MAX_INHERIT
        {
            chain.push(parent);
        }
        let mut out: Vec<DefInput> = Vec::new();
        for d in chain.iter().rev() {
            for i in &d.inputs {
                match out.iter_mut().find(|o| o.name == i.name) {
                    Some(o) => *o = i.clone(),
                    None => out.push(i.clone()),
                }
            }
        }
        out
    }
}

/// Appends the `.mtlx` files under `path` to `out`, sorted within each
/// directory so the read order — and so which duplicate wins — does not
/// depend on the file system. Each directory is walked once, by its
/// canonical path in `walked`, so a link back to an ancestor (or two search
/// paths naming one directory) reads nothing twice.
fn collect_mtlx(
    path: &Path,
    out: &mut Vec<PathBuf>,
    walked: &mut HashSet<PathBuf>,
    errors: &mut Vec<(PathBuf, MtlxError)>,
) {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            errors.push((path.to_path_buf(), MtlxError::Io(e)));
            return;
        }
    };
    if meta.is_file() {
        out.push(path.to_path_buf());
        return;
    }
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !walked.insert(canonical) {
        return;
    }
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            errors.push((path.to_path_buf(), MtlxError::Io(e)));
            return;
        }
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            collect_mtlx(&entry, out, walked, errors);
        } else if entry
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("mtlx"))
        {
            out.push(entry);
        }
    }
}

fn parse_nodedef(el: roxmltree::Node<'_, '_>) -> NodeDef {
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for c in el.children().filter(|n| n.is_element()) {
        let name = c.attribute("name").unwrap_or_default().to_string();
        let type_name = c.attribute("type").unwrap_or("float").to_string();
        match c.tag_name().name() {
            // 1.38's `<parameter>` is a uniform input.
            "input" | "parameter" | "token" => {
                let text = c.attribute("value").map(str::to_string);
                let value = text.as_deref().and_then(|v| parse_literal(v, &type_name));
                inputs.push(DefInput {
                    name,
                    type_name,
                    value,
                    text,
                });
            }
            "output" => outputs.push(DefOutput { name, type_name }),
            _ => {}
        }
    }
    // A nodedef that declares no output has MaterialX's implicit `out`, typed
    // by the nodedef's own `type` (1.37 style).
    if outputs.is_empty()
        && let Some(t) = el.attribute("type")
    {
        outputs.push(DefOutput {
            name: "out".to_string(),
            type_name: t.to_string(),
        });
    }
    NodeDef {
        name: el.attribute("name").unwrap_or_default().to_string(),
        node: el.attribute("node").unwrap_or_default().to_string(),
        inputs,
        outputs,
        inherit: el.attribute("inherit").map(str::to_string),
        version: el.attribute("version").map(str::to_string),
        default_version: el.attribute("isdefaultversion") == Some("true"),
    }
}

/// A `<nodegraph>` that names the nodedef it implements; `None` for a plain
/// graph, which belongs to the document's material network instead.
fn parse_implementation(el: roxmltree::Node<'_, '_>) -> Option<Implementation> {
    let nodedef = el.attribute("nodedef")?.to_string();
    let name = el.attribute("name").unwrap_or_default().to_string();
    let mut nodes = Vec::new();
    let mut outputs = Vec::new();
    for inner in el.children().filter(|n| n.is_element()) {
        match inner.tag_name().name() {
            "output" => outputs.push(ImplOutput {
                name: inner.attribute("name").unwrap_or_default().to_string(),
                type_name: inner.attribute("type").unwrap_or_default().to_string(),
                node: inner.attribute("nodename").unwrap_or_default().to_string(),
                output: inner.attribute("output").map(str::to_string),
            }),
            // Interface declarations repeat the nodedef's; the nodedef is
            // the signature.
            "input" | "parameter" | "token" => {}
            _ => nodes.push(parse_node(inner, Some(name.clone()))),
        }
    }
    Some(Implementation {
        name,
        nodedef,
        nodes,
        outputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = r#"<?xml version="1.0"?>
<materialx version="1.39">
  <nodedef name="studio_mix" node="studio_mix">
    <input name="fg_color" type="color3" value="0, 0, 0" />
    <input name="bg_color" type="color3" value="0, 0, 0" />
    <input name="amount" type="float" value="0.5" />
    <output name="blended" type="color3" />
    <output name="red" type="float" />
  </nodedef>
  <nodegraph name="NG_studio_mix" nodedef="studio_mix">
    <mix name="mix" type="color3">
      <input name="bg" type="color3" interfacename="bg_color" />
      <input name="fg" type="color3" interfacename="fg_color" />
      <input name="mix" type="float" interfacename="amount" />
    </mix>
    <separate3 name="output_split" type="multioutput">
      <input name="in" type="color3" nodename="mix" />
    </separate3>
    <output name="blended" type="color3" nodename="mix" />
    <output name="red" type="float" nodename="output_split" output="outr" />
  </nodegraph>
  <nodedef name="ND_studio_layer_color3" node="studio_layer">
    <output name="out" type="color3" />
    <input name="mode" type="integer" value="1" />
    <output name="alpha" type="float" />
    <input name="color" type="color3" />
  </nodedef>
  <nodedef name="studio_texture" node="studio_texture" nodegroup="texture2d">
    <input name="file" type="filename" value="" />
    <input name="uaddressmode" type="string" value="periodic" />
    <output name="outColor" type="color3" />
  </nodedef>
  <implementation name="IM_studio_texture_osl" nodedef="studio_texture" file="file.osl" />
</materialx>"#;

    fn lib() -> Library {
        let mut l = Library::default();
        l.merge_text(LIB).expect("parses");
        l
    }

    #[test]
    fn a_nodedef_reads_its_inputs_defaults_and_outputs() {
        let l = lib();
        let d = l.nodedef("studio_mix").expect("defined");
        assert_eq!(d.node, "studio_mix");
        let names: Vec<_> = d.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["fg_color", "bg_color", "amount"]);
        assert_eq!(d.inputs[2].value.map(|v| v.x()), Some(0.5));
        assert_eq!(d.instance_type(), "multioutput");
        assert_eq!(d.output("red").map(|o| o.type_name.as_str()), Some("float"));
    }

    #[test]
    fn a_nodegraph_implementation_keeps_its_interface_and_output_selection() {
        let l = lib();
        let imp = l.implementation("studio_mix").expect("a graph");
        assert_eq!(imp.name, "NG_studio_mix");
        let mix = imp.nodes.iter().find(|n| n.name == "mix").expect("mix");
        assert_eq!(mix.graph.as_deref(), Some("NG_studio_mix"));
        assert_eq!(
            mix.input("fg").and_then(|i| i.interface.as_deref()),
            Some("fg_color")
        );
        let r = imp.outputs.iter().find(|o| o.name == "red").expect("red");
        assert_eq!(
            (r.node.as_str(), r.output.as_deref()),
            ("output_split", Some("outr"))
        );
    }

    #[test]
    fn a_nodedef_is_found_by_name_and_by_category() {
        let l = lib();
        // The name breaks `ND_<category>_<type>`; the category is the `node`.
        let by_cat = l.for_node("studio_layer", Some("multioutput"), None);
        assert_eq!(
            by_cat.map(|d| d.name.as_str()),
            Some("ND_studio_layer_color3")
        );
        assert!(l.for_node("studio_layer", Some("color3"), None).is_none());
        assert_eq!(
            l.for_node("studio_texture", None, None)
                .map(|d| d.name.as_str()),
            Some("studio_texture")
        );
        // A string default is kept as text; an input with no value has none.
        let file = l.nodedef("studio_texture").unwrap();
        assert_eq!(file.inputs[1].text.as_deref(), Some("periodic"));
        let bm = l.nodedef("ND_studio_layer_color3").unwrap();
        assert!(
            bm.inputs
                .iter()
                .find(|i| i.name == "color")
                .unwrap()
                .text
                .is_none()
        );
    }

    /// An authored `version` names its nodedef, and only that one: a version
    /// no nodedef has finds nothing rather than the default.
    #[test]
    fn a_version_selects_its_nodedef() {
        let mut l = Library::default();
        l.merge_text(
            r#"<materialx>
                 <nodedef name="ND_v1" node="n" version="1.0" isdefaultversion="true"><output name="out" type="float" /></nodedef>
                 <nodedef name="ND_v2" node="n" version="2.0"><output name="out" type="float" /></nodedef>
               </materialx>"#,
        )
        .unwrap();
        let name = |v: Option<&str>| l.for_node("n", Some("float"), v).map(|d| d.name.clone());
        assert_eq!(name(None).as_deref(), Some("ND_v1"));
        assert_eq!(name(Some("2.0")).as_deref(), Some("ND_v2"));
        assert_eq!(name(Some("1.0")).as_deref(), Some("ND_v1"));
        assert_eq!(name(Some("3.0")), None);
        assert!(l.defines("n") && !l.defines("m"));
    }

    #[test]
    fn a_code_implementation_is_not_a_graph() {
        assert!(lib().implementation("studio_texture").is_none());
    }

    #[test]
    fn the_first_definition_of_a_name_wins() {
        let mut l = Library::default();
        l.merge_text(
            r#"<materialx><nodedef name="studio_texture" node="mine"><output name="out" type="float" /></nodedef></materialx>"#,
        )
        .unwrap();
        l.merge(&lib());
        assert_eq!(l.nodedef("studio_texture").unwrap().node, "mine");
        assert!(l.nodedef("studio_mix").is_some(), "the rest is merged");
    }

    #[test]
    fn inherit_puts_the_parent_inputs_first_and_lets_the_child_override() {
        let mut l = Library::default();
        l.merge_text(
            r#"<materialx>
                 <nodedef name="ND_base" node="n"><input name="a" type="float" value="1" /><input name="b" type="float" value="2" /><output name="out" type="float" /></nodedef>
                 <nodedef name="ND_child" node="n" inherit="ND_base"><input name="b" type="float" value="3" /><input name="c" type="float" value="4" /></nodedef>
                 <nodedef name="ND_loop" node="m" inherit="ND_loop"><input name="x" type="float" /></nodedef>
               </materialx>"#,
        )
        .unwrap();
        let child = l.nodedef("ND_child").unwrap();
        let ins: Vec<_> = l
            .inputs(child)
            .iter()
            .map(|i| (i.name.clone(), i.value.map(|v| v.x())))
            .collect();
        let expected = [("a", 1.0), ("b", 3.0), ("c", 4.0)].map(|(n, v)| (n.to_string(), Some(v)));
        assert_eq!(ins, expected);
        // A self-inheriting nodedef stops at the guard.
        assert_eq!(l.inputs(l.nodedef("ND_loop").unwrap()).len(), 1);
    }

    #[test]
    fn load_walks_directories_and_reports_what_it_cannot_read() {
        let dir = std::env::temp_dir().join(format!("crust_mtlx_lib_{}", std::process::id()));
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("studio.mtlx"), LIB).unwrap();
        std::fs::write(dir.join("broken.mtlx"), "<materialx><nodedef").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a library").unwrap();
        let missing = dir.join("missing");
        let (l, errors) = Library::load(&[dir.clone(), missing.clone()]);
        assert!(l.nodedef("studio_mix").is_some());
        assert_eq!(l.files, [nested.join("studio.mtlx")]);
        // The walk's failures first, then the files that would not parse.
        let failed: Vec<_> = errors.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(failed, [missing, dir.join("broken.mtlx")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory link back to an ancestor is walked once: the files under
    /// it are read once, and the walk ends. Skipped where the link cannot be
    /// made (Windows without the symlink privilege).
    #[test]
    fn a_link_to_an_ancestor_reads_each_file_once() {
        let dir = std::env::temp_dir().join(format!("crust_mtlx_loop_{}", std::process::id()));
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("studio.mtlx"), LIB).unwrap();
        let link = nested.join("up");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&dir, &link);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&dir, &link);
        if made.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        // Both search paths name one tree, the second through the link.
        let (l, errors) = Library::load(&[dir.clone(), link.clone()]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(l.files, [nested.join("studio.mtlx")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
