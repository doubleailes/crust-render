//! Custom nodes replaced by their node-graph implementations, before
//! compilation.
//!
//! A node whose category the compiler has no operator for, and whose nodedef
//! ([`crate::library`]) is implemented as a `<nodegraph nodedef=…>`, is
//! replaced in the document by a copy of that graph, so the compiler only
//! ever meets standard nodes. MaterialX's own code generators do the same
//! (`ShaderGraph::flattenSubgraphs`). One pass over a [`Doc`] serves both
//! ways MaterialX reaches crust, a `.mtlx` and an inline USD network, since
//! both end as a document.
//!
//! For an instance `n` of nodedef `D` implemented by graph `G`:
//!
//! - each node `m` of `G` becomes `n/m`, in the instance's scope, so two
//!   instances never share a node and the names stay readable in warnings;
//! - an input of `m` bound to `G`'s interface (`interfacename="x"`) takes the
//!   instance's input `x` as authored — connection, literal, text and colour
//!   space alike — else `D`'s default for `x`, else stays unauthored, so `m`'s
//!   operator default applies, as MaterialX's does;
//! - every reference to `n` — a node input, a nodegraph output, a document
//!   output — is redirected to the graph output it selects (`output="X"`, or
//!   the only or `out` output when it names none), keeping the selection that
//!   graph output makes on its own node (`nodename="split" output="outr"`).
//!
//! The expansion repeats until no custom node is left, so an implementation
//! that instantiates another custom node (a layering node built from several
//! instances of a blending node) expands in turn. Each copy remembers the
//! nodedefs it was expanded through, so a definition that reaches itself is a
//! cycle: it is refused at its first re-entry and reported, and the node left
//! behind is the compiler's unsupported category. A chain deeper than
//! [`MAX_DEPTH`], or an expansion past [`MAX_EXPANDED_NODES`] nodes in all,
//! is cut the same way: a graph holding k instances of the next definition
//! grows as k to the depth, so the depth alone does not bound the work.
//!
//! An instance names its nodedef by category, output type and, when it
//! authors one, `version` ([`Library::for_node`]). A category a library
//! defines, but not for that type or version, is reported rather than
//! expanded with another signature.
//!
//! The compiler's own operators always win over a definition of the same
//! category ([`compiles`]): a library that ships a node-graph `place2d` must
//! not replace the operator the OSL oracle checks.

use super::library::{Implementation, Library, NodeDef};
use super::parse::{Doc, Input, Node, Source};
use super::value::Val;
use std::collections::{HashMap, HashSet};

/// How many levels of custom nodes inside custom nodes are expanded.
pub const MAX_DEPTH: usize = 16;

/// How many nodes one document's expansion may create in all.
pub const MAX_EXPANDED_NODES: usize = 262_144;

/// The categories the compiler has an operator for: `compile_node`'s pattern
/// nodes, the closure nodes `bsdf.rs` reads, and the surface builders. A
/// custom definition of one of these is never expanded. Kept in step with
/// `compile_node` by `builtins_cover_every_pattern_operator`.
pub const BUILTIN_CATEGORIES: &[&str] = &[
    // Pattern nodes (`eval::compile::compile_node`).
    "constant",
    "image",
    "tiledimage",
    "texcoord",
    "normal",
    "viewdirection",
    "position",
    "add",
    "subtract",
    "multiply",
    "divide",
    "power",
    "min",
    "max",
    "modulo",
    "absval",
    "ln",
    "exp",
    "sin",
    "cos",
    "asin",
    "acos",
    "sqrt",
    "sign",
    "floor",
    "ceil",
    "normalize",
    "luminance",
    "dotproduct",
    "magnitude",
    "mix",
    "clamp",
    "contrast",
    "remap",
    "invert",
    "smoothstep",
    "convert",
    "extract",
    "combine2",
    "combine3",
    "normalmap",
    "artistic_ior",
    "chiang_hair_roughness",
    "chiang_hair_absorption_from_color",
    "deon_hair_absorption_from_melanin",
    "colorcorrect",
    "rgbtohsv",
    "hsvtorgb",
    "geompropvalue",
    "ifgreater",
    "ifgreatereq",
    "ifequal",
    "cellnoise2d",
    "randomfloat",
    "randomcolor",
    "heighttonormal",
    "separate2",
    "separate3",
    "separate4",
    "switch",
    "over",
    "in",
    "out",
    "difference",
    "premult",
    "place2d",
    "transformmatrix",
    // Closures and shaders (`bsdf.rs`, `surface/`).
    "layer",
    "oren_nayar_diffuse_bsdf",
    "diffuse_bsdf",
    "burley_diffuse_bsdf",
    "dielectric_bsdf",
    "conductor_bsdf",
    "generalized_schlick_bsdf",
    "sheen_bsdf",
    "subsurface_bsdf",
    "translucent_bsdf",
    "chiang_hair_bsdf",
    "thin_film_bsdf",
    "uniform_edf",
    "generalized_schlick_edf",
    "anisotropic_vdf",
    "absorption_vdf",
    "surface",
    "surfacematerial",
    "volume",
    "volumematerial",
    "displacement",
    "open_pbr_surface",
    "standard_surface",
    "gltf_pbr",
    "UsdPreviewSurface",
];

/// Whether the compiler has an operator for `category`.
pub fn compiles(category: &str) -> bool {
    BUILTIN_CATEGORIES.contains(&category)
}

/// One expanded instance, for redirecting the references to it.
struct Expanded {
    /// The prefix its graph's nodes were renamed under.
    prefix: String,
    imp: Implementation,
}

impl Doc {
    /// Replaces every custom node by its graph implementation (module
    /// documentation), looking definitions up in the document's own
    /// [`Doc::library`] first and in `extra` second. What could not be done
    /// is added to [`Doc::reported`].
    pub fn expand(&mut self, extra: &Library) {
        if self.library.is_empty() && extra.is_empty() {
            return;
        }
        let own = std::mem::take(&mut self.library);
        let libs = [&own, extra];
        let mut state = Expansion::default();
        loop {
            let expanded = self.expand_once(&libs, &mut state);
            if expanded == 0 {
                break;
            }
        }
        self.library = own;
    }

    /// Reports `message` about the node called `name`, once per name.
    fn note(&mut self, state: &mut Expansion, name: &str, message: String) {
        if state.noted.insert(name.to_string()) {
            self.reported.push(message);
        }
    }

    /// One pass: expands every custom node present now, then redirects the
    /// references to them. Returns how many were expanded.
    fn expand_once(&mut self, libs: &[&Library; 2], state: &mut Expansion) -> usize {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut done: HashMap<(String, String), Expanded> = HashMap::new();
        for node in std::mem::take(&mut self.nodes) {
            let scope = node.graph.clone().unwrap_or_default();
            let key = (scope, node.name.clone());
            let (def, imp, lib) = match definition(libs, &node) {
                Lookup::Found(def, imp, lib) => (def, imp, lib),
                Lookup::Builtin => {
                    out.push(node);
                    continue;
                }
                Lookup::Mismatch => {
                    let version = node
                        .version
                        .as_deref()
                        .map(|v| format!(" version {v:?}"))
                        .unwrap_or_default();
                    self.note(
                        state,
                        &node.name,
                        format!(
                            "{}: no {} nodedef of type {}{version} — not expanded",
                            node.name, node.category, node.type_name
                        ),
                    );
                    out.push(node);
                    continue;
                }
            };
            let chain = state.chains.get(&key).cloned().unwrap_or_default();
            if let Some(at) = chain.iter().position(|d| *d == def.name) {
                let through = chain[at..].join(" → ");
                // Once per definition: every copy that re-enters it is the
                // same defect.
                self.note(
                    state,
                    &format!("cycle {}", def.name),
                    format!(
                        "{}: {} instantiates itself ({through} → {}), a cycle — not expanded",
                        node.name, def.name, def.name
                    ),
                );
                out.push(node);
                continue;
            }
            if chain.len() >= MAX_DEPTH {
                self.note(
                    state,
                    &node.name,
                    format!(
                        "{}: custom nodes nested past {MAX_DEPTH} levels — {} not expanded",
                        node.name, node.category
                    ),
                );
                out.push(node);
                continue;
            }
            if state.created + imp.nodes.len() > MAX_EXPANDED_NODES {
                self.note(
                    state,
                    "",
                    format!(
                        "custom nodes expand past {MAX_EXPANDED_NODES} nodes — {} and the \
                         instances after it not expanded",
                        node.name
                    ),
                );
                out.push(node);
                continue;
            }
            state.created += imp.nodes.len();
            let mut inner = chain;
            inner.push(def.name.clone());
            let defaults = lib.inputs(def);
            let prefix = node.name.clone();
            for m in &imp.nodes {
                let copy = instantiate(&node, &prefix, m, imp, &defaults);
                let copy_key = (copy.graph.clone().unwrap_or_default(), copy.name.clone());
                state.chains.insert(copy_key, inner.clone());
                out.push(copy);
            }
            done.insert(
                key,
                Expanded {
                    prefix,
                    imp: imp.clone(),
                },
            );
        }
        let count = done.len();
        // Which names exist where, before the pass, decides what each
        // reference meant: a name in a graph resolves to that graph's node
        // first, to the document's second, as `Doc::find` does.
        let mut nodes = out;
        if count > 0 {
            let before: std::collections::HashSet<(String, String)> = nodes
                .iter()
                .map(|n| (n.graph.clone().unwrap_or_default(), n.name.clone()))
                .chain(done.keys().cloned())
                .collect();
            let resolve = |scope: &str, name: &str| -> Option<(String, String)> {
                let local = (scope.to_string(), name.to_string());
                if before.contains(&local) {
                    return Some(local);
                }
                let global = (String::new(), name.to_string());
                before.contains(&global).then_some(global)
            };
            let mut problems = Vec::new();
            for n in &mut nodes {
                let scope = n.graph.clone().unwrap_or_default();
                for input in &mut n.inputs {
                    let Source::Node { name, output } = &input.source else {
                        continue;
                    };
                    let Some(e) = resolve(&scope, name).and_then(|k| done.get(&k)) else {
                        continue;
                    };
                    match redirect(e, output.as_deref()) {
                        Some((node, output)) => input.source = Source::Node { name: node, output },
                        None => problems.push(format!(
                            "{}.{}: {} has no output {:?} — not connected",
                            n.name,
                            input.name,
                            e.imp.nodedef,
                            output.as_deref().unwrap_or("out")
                        )),
                    }
                }
            }
            for ((graph, _), target) in self.graph_outputs.iter_mut() {
                let Some(e) = resolve(graph, &target.node).and_then(|k| done.get(&k)) else {
                    continue;
                };
                if let Some((node, output)) = redirect(e, target.output.as_deref()) {
                    target.node = node;
                    target.output = output;
                }
            }
            self.reported.extend(problems);
            for (key, e) in &done {
                if key.0.is_empty()
                    && let Some((node, _)) = redirect(e, None)
                {
                    self.expanded.insert(key.1.clone(), node);
                }
            }
        }
        self.nodes = nodes;
        self.reindex();
        count
    }
}

/// What [`Doc::expand`] carries from one pass to the next.
#[derive(Default)]
struct Expansion {
    /// Each copy's key → the nodedefs it was expanded through, outermost
    /// first.
    chains: HashMap<(String, String), Vec<String>>,
    /// The names already reported, so a node left behind is reported once.
    noted: HashSet<String>,
    /// How many nodes the expansion has created so far.
    created: usize,
}

/// What [`definition`] found for a node.
enum Lookup<'l> {
    Found(&'l NodeDef, &'l Implementation, &'l Library),
    /// A category the compiler implements, or one no library defines: not
    /// this pass's business.
    Builtin,
    /// A category a library defines, but not for the node's type or version.
    Mismatch,
}

/// `node`'s nodedef and graph implementation, from the first library that
/// defines it for the node's type and version.
fn definition<'l>(libs: &[&'l Library; 2], node: &Node) -> Lookup<'l> {
    if compiles(&node.category) {
        return Lookup::Builtin;
    }
    let def_in = |lib: &'l Library| {
        lib.for_node(
            &node.category,
            Some(&node.type_name),
            node.version.as_deref(),
        )
    };
    let found = libs.iter().find_map(|lib| {
        let def = def_in(lib)?;
        Some(Lookup::Found(def, lib.implementation(&def.name)?, lib))
    });
    if let Some(found) = found {
        return found;
    }
    // A nodedef implemented by source code, not a graph, is left to the
    // compiler, which names it unsupported; so is a category nobody defines.
    if libs.iter().any(|lib| def_in(lib).is_some())
        || !libs.iter().any(|lib| lib.defines(&node.category))
    {
        return Lookup::Builtin;
    }
    Lookup::Mismatch
}

/// The copy of `m`, a node of `imp`, for the instance `inst`.
fn instantiate(
    inst: &Node,
    prefix: &str,
    m: &Node,
    imp: &Implementation,
    defaults: &[super::library::DefInput],
) -> Node {
    let mut inputs = Vec::with_capacity(m.inputs.len());
    for i in &m.inputs {
        if let Some(iface) = &i.interface {
            if let Some(bound) = inst.input(iface) {
                inputs.push(Input {
                    name: i.name.clone(),
                    type_name: i.type_name.clone(),
                    source: bound.source.clone(),
                    colorspace: bound.colorspace.clone(),
                    text: bound.text.clone(),
                    interface: bound.interface.clone(),
                });
            } else if let Some(d) = defaults
                .iter()
                .find(|d| d.name == *iface)
                .filter(|d| d.value.is_some() || d.text.is_some())
            {
                inputs.push(Input {
                    name: i.name.clone(),
                    type_name: i.type_name.clone(),
                    source: Source::Value(d.value.unwrap_or(Val::ZERO)),
                    colorspace: None,
                    text: d.text.clone(),
                    interface: None,
                });
            }
            // Else unauthored: the operator's own default.
            continue;
        }
        let mut copy = i.clone();
        if let Source::Node { name, .. } = &mut copy.source
            && imp.nodes.iter().any(|n| n.name == *name)
        {
            *name = format!("{prefix}/{name}");
        }
        inputs.push(copy);
    }
    Node {
        category: m.category.clone(),
        name: format!("{prefix}/{}", m.name),
        type_name: m.type_name.clone(),
        inputs,
        graph: inst.graph.clone(),
        version: m.version.clone(),
        colorspace: m.colorspace.clone(),
    }
}

/// Where a reference to an expanded instance, selecting `output`, now
/// points: the renamed node its graph output forwards to, with that graph
/// output's own selection. `None` for an output the graph does not have.
fn redirect(e: &Expanded, output: Option<&str>) -> Option<(String, Option<String>)> {
    let outs = &e.imp.outputs;
    let target = match output {
        Some(o) => outs.iter().find(|x| x.name == o)?,
        None => match outs.as_slice() {
            [one] => one,
            many => many.iter().find(|x| x.name == "out").or(many.first())?,
        },
    };
    if target.node.is_empty() {
        return None;
    }
    Some((
        format!("{}/{}", e.prefix, target.node),
        target.output.clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pattern operator `compile_node` matches is in
    /// [`BUILTIN_CATEGORIES`], so a custom definition can never shadow one.
    #[test]
    fn builtins_cover_every_pattern_operator() {
        let src = include_str!("eval/compile.rs");
        let start = src.find("fn compile_node").expect("compile_node");
        let body = &src[start..];
        let end = body[1..].find("\n    fn ").map_or(body.len(), |e| e + 1);
        let mut missing = Vec::new();
        for line in body[..end].lines() {
            let t = line.trim_start();
            if !t.starts_with('"') || !t.contains("=>") {
                continue;
            }
            for arm in t.split("=>").next().unwrap().split('|') {
                let name = arm.trim().trim_matches('"');
                // `geompropvalue`'s property names share the syntax.
                if !name.contains(':') && name != "st" && !compiles(name) {
                    missing.push(name.to_string());
                }
            }
        }
        assert!(missing.is_empty(), "not in BUILTIN_CATEGORIES: {missing:?}");
    }
}
