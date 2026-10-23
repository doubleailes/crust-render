//! Value graphs: pure pattern nodes over values a host supplies per
//! evaluation — no shading point, no texture, no closure.
//!
//! A host that recombines numbers it already has (a renderer's sample
//! filters, which remap and mix a camera sample's AOVs) writes the graph's
//! interface inputs as [`Source::Interface`] and compiles it here. Each
//! interface input becomes a [`Program::primvars`] channel, exactly what a
//! `geompropvalue` reads, so the program is an ordinary [`Program`]: it folds
//! and optimises as a surface's does, and the JIT (crust-jit) runs it bit for
//! bit as the interpreter does.

use std::collections::BTreeSet;

use crate::eval::{Compiler, Op, Program};
use crate::parse::{Doc, Node, Source};
use crate::value::arity_of;

/// Categories a value graph may not hold: what reads the shading point, a
/// texture or a host-side primvar of its own. A camera sample has no
/// shading point; what the surface knew is what the host's inputs carry.
const REFUSED_CATEGORIES: &[&str] = &[
    "image",
    "tiledimage",
    "geompropvalue",
    "texcoord",
    "normal",
    "tangent",
    "bitangent",
    "position",
    "viewdirection",
    "normalmap",
    "heighttonormal",
];

/// Output types that are closures or shaders, never values.
const CLOSURE_TYPES: &[&str] = &[
    "BSDF",
    "EDF",
    "VDF",
    "surfaceshader",
    "volumeshader",
    "displacementshader",
    "lightshader",
    "material",
];

/// A compiled value graph.
#[derive(Clone, Default)]
pub struct ValueGraph {
    /// The program, optimised to the outputs. Its
    /// [`primvars`](Program::primvars) are the interface inputs it reads, by
    /// name, in first-use order: what [`ShadeCtx::primvars`](crate::ShadeCtx)
    /// holds per evaluation.
    pub program: Program,
    /// Each requested output's slot in an evaluation of `program`, and its
    /// width (1 to 4), in the order requested.
    pub outputs: Vec<(u32, u8)>,
    /// Why the graph cannot run as a value graph, one entry per cause
    /// (a refused node, an unknown category), sorted. Empty when it can.
    pub refused: Vec<String>,
}

impl ValueGraph {
    /// Whether the graph compiled to something a host may run.
    pub fn is_accepted(&self) -> bool {
        self.refused.is_empty()
    }
}

/// Compiles the document-scope nodes `outputs` names — `(node, output)`,
/// `output` naming one output of a multioutput node — into one program over
/// the document's interface inputs.
///
/// A node that reads the shading point or a texture (an `image`, a
/// `geompropvalue`, `position`, `normal`, …) or that is a closure or shader
/// is refused, as is a category the compiler has no operator for: the
/// result then lists why in [`ValueGraph::refused`], and a host should not
/// run it. Textures are never loaded: the host's loader is not called.
pub fn compile_value_graph(doc: &Doc, outputs: &[(&str, Option<&str>)]) -> ValueGraph {
    let mut refused = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut stack: Vec<(String, String)> = outputs
        .iter()
        .map(|(n, _)| (String::new(), (*n).to_string()))
        .collect();
    while let Some((scope, name)) = stack.pop() {
        let Some(node) = doc.find(&scope, &name) else {
            refused.insert(format!("{name}: no such node"));
            continue;
        };
        if !seen.insert((node.graph.clone().unwrap_or_default(), node.name.clone())) {
            continue;
        }
        if let Some(why) = refusal(node) {
            refused.insert(why);
        }
        let scope = node.graph.clone().unwrap_or_default();
        for input in &node.inputs {
            match &input.source {
                Source::Node { name, .. } => stack.push((scope.clone(), name.clone())),
                Source::Graph { graph, output } => match doc.graph_output(graph, output) {
                    Some(c) => stack.push((graph.clone(), c.node.name.clone())),
                    // The compiler would read it as zero: refused, never run
                    // on a value nobody authored.
                    None => {
                        refused.insert(format!(
                            "{}.{}: nodegraph \"{graph}\" has no output \"{output}\"",
                            node.name, input.name
                        ));
                    }
                },
                Source::Value(_) | Source::Interface { .. } => {}
            }
        }
    }

    // Textures never reach the loader: a refused `image` is not compiled
    // into anything a host runs.
    let no_textures = |_: &str, _: crate::TexelSpace<'_>| None;
    let host = crate::Host::new(&no_textures);
    let mut c = Compiler::new(doc, &host);
    let mut roots = Vec::with_capacity(outputs.len());
    // A node's declared type is its output's width, except a multioutput
    // node's: its selected output's width is the one its op computes.
    let mut widths: Vec<Option<u8>> = Vec::with_capacity(outputs.len());
    for (name, output) in outputs {
        roots.push(c.compile_named("", name, *output));
        widths.push(
            doc.find("", name)
                .filter(|n| n.type_name != "multioutput")
                .map(|n| arity_of(&n.type_name).clamp(1, 4)),
        );
    }
    refused.extend(c.unsupported.iter().map(|u| format!("{u}: not supported")));
    // Whatever the categories said, a program that reads anything but its
    // own channels is not a value graph.
    if c.program
        .ops
        .iter()
        .any(|op| !op.is_pure() && !matches!(op, Op::Primvar { .. }))
    {
        refused.insert("reads the shading point or a texture".into());
    }
    let (program, remap) = c.program.optimize(&roots);
    let slots: Vec<u32> = roots
        .iter()
        .map(|r| remap[*r as usize].unwrap_or(0))
        .collect();
    // The width of an op's value does not depend on its operands' values,
    // so one evaluation over the defaults gives the multioutput widths.
    let computed = widths.iter().any(Option::is_none).then(|| {
        let mut values = Vec::new();
        program.eval(&default_ctx(), &mut values);
        values
    });
    let outputs = slots
        .iter()
        .zip(widths)
        .map(|(s, w)| {
            let w = w.unwrap_or_else(|| {
                computed
                    .as_ref()
                    .and_then(|v| v.get(*s as usize))
                    .map_or(1, |v| v.arity.clamp(1, 4))
            });
            (*s, w)
        })
        .collect();
    ValueGraph {
        program,
        outputs,
        refused: refused.into_iter().collect(),
    }
}

/// A shading context with nothing in it: every channel reads its default.
fn default_ctx() -> crate::ShadeCtx<'static> {
    crate::ShadeCtx {
        uv: (0.0, 0.0),
        normal: glam::Vec3A::Z,
        tangent: glam::Vec3A::ZERO,
        view: -glam::Vec3A::Z,
        position: glam::Vec3A::ZERO,
        uv_width: 0.0,
        curve: None,
        primvars: &[],
    }
}

/// Why `node` cannot be part of a value graph, if it cannot.
fn refusal(node: &Node) -> Option<String> {
    if REFUSED_CATEGORIES.contains(&node.category.as_str()) {
        return Some(format!(
            "{} ({}): reads the shading point or a texture",
            node.name, node.category
        ));
    }
    if CLOSURE_TYPES.contains(&node.type_name.as_str()) {
        return Some(format!(
            "{} ({}): a {} is not a value",
            node.name, node.category, node.type_name
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Input, ShadeCtx, Val};
    use glam::Vec3A;

    fn node(category: &str, name: &str, type_name: &str, inputs: Vec<Input>) -> Node {
        Node {
            category: category.into(),
            name: name.into(),
            type_name: type_name.into(),
            inputs,
            graph: None,
            version: None,
            colorspace: None,
        }
    }

    fn interface(name: &str, type_name: &str, default: Option<Val>) -> Input {
        Input {
            name: name.into(),
            type_name: type_name.into(),
            source: Source::Interface {
                name: name.into(),
                default,
            },
            colorspace: None,
            text: None,
        }
    }

    fn port(input: &str, type_name: &str, interface_name: &str) -> Input {
        Input {
            name: input.into(),
            type_name: type_name.into(),
            source: Source::Interface {
                name: interface_name.into(),
                default: None,
            },
            colorspace: None,
            text: None,
        }
    }

    fn link(input: &str, type_name: &str, from: &str) -> Input {
        Input {
            name: input.into(),
            type_name: type_name.into(),
            source: Source::Node {
                name: from.into(),
                output: None,
            },
            colorspace: None,
            text: None,
        }
    }

    fn ctx(primvars: &[Option<Val>]) -> ShadeCtx<'_> {
        ShadeCtx {
            uv: (0.0, 0.0),
            normal: Vec3A::Z,
            tangent: Vec3A::ZERO,
            view: -Vec3A::Z,
            position: Vec3A::ZERO,
            uv_width: 0.0,
            curve: None,
            primvars,
        }
    }

    /// `matte > 0.5 ? key : fill`, the recombination the precomp design
    /// opens with.
    fn select_doc() -> Doc {
        Doc::from_nodes(vec![node(
            "ifgreater",
            "pick",
            "color3",
            vec![
                port("value1", "float", "matte"),
                Input {
                    name: "value2".into(),
                    type_name: "float".into(),
                    source: Source::Value(Val::float(0.5)),
                    colorspace: None,
                    text: None,
                },
                port("in1", "color3", "key"),
                port("in2", "color3", "fill"),
            ],
        )])
    }

    #[test]
    fn interface_inputs_become_channels_read_per_evaluation() {
        let g = compile_value_graph(&select_doc(), &[("pick", None)]);
        assert!(g.is_accepted(), "{:?}", g.refused);
        assert_eq!(g.outputs.len(), 1);
        assert_eq!(g.outputs[0].1, 3);
        let at = |matte: f32| {
            let mut values = vec![None; g.program.primvars.len()];
            for (i, name) in g.program.primvars.iter().enumerate() {
                values[i] = Some(match name.as_str() {
                    "matte" => Val::float(matte),
                    "key" => Val::vec3(1.0, 0.0, 0.0),
                    "fill" => Val::vec3(0.0, 0.0, 1.0),
                    other => panic!("unexpected channel {other}"),
                });
            }
            let mut slots = Vec::new();
            g.program.eval(&ctx(&values), &mut slots);
            slots[g.outputs[0].0 as usize].rgb()
        };
        assert_eq!(at(1.0), Vec3A::new(1.0, 0.0, 0.0));
        assert_eq!(at(0.0), Vec3A::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn an_unsupplied_interface_input_reads_its_default() {
        let doc = Doc::from_nodes(vec![node(
            "multiply",
            "m",
            "float",
            vec![
                interface("in1", "float", Some(Val::float(3.0))),
                interface("in2", "float", None),
            ],
        )]);
        let g = compile_value_graph(&doc, &[("m", None)]);
        assert!(g.is_accepted());
        let mut slots = Vec::new();
        // `in2` supplied, `in1` not: 3 × 2.
        let values: Vec<Option<Val>> = g
            .program
            .primvars
            .iter()
            .map(|n| (n == "in2").then_some(Val::float(2.0)))
            .collect();
        g.program.eval(&ctx(&values), &mut slots);
        assert_eq!(slots[g.outputs[0].0 as usize].x(), 6.0);
        // Nothing supplied: the default times a zero default.
        g.program.eval(&ctx(&[]), &mut slots);
        assert_eq!(slots[g.outputs[0].0 as usize].x(), 0.0);
    }

    #[test]
    fn textures_geometry_and_closures_are_refused() {
        for (category, type_name) in [
            ("image", "color3"),
            ("geompropvalue", "float"),
            ("position", "vector3"),
            ("normal", "vector3"),
            ("texcoord", "vector2"),
            ("oren_nayar_diffuse_bsdf", "BSDF"),
        ] {
            let doc = Doc::from_nodes(vec![
                node(category, "leaf", type_name, Vec::new()),
                node(
                    "add",
                    "out",
                    "color3",
                    vec![link("in1", type_name, "leaf"), port("in2", "color3", "key")],
                ),
            ]);
            let g = compile_value_graph(&doc, &[("out", None)]);
            assert!(!g.is_accepted(), "{category} accepted");
            assert!(
                g.refused.iter().any(|r| r.contains(category)),
                "{category}: {:?}",
                g.refused
            );
        }
    }

    /// `artistic_ior` is a multioutput node: the selected output's width,
    /// three lanes for `extinction`, not the node's generic type's.
    #[test]
    fn a_selected_multioutput_output_keeps_its_width() {
        let doc = Doc::from_nodes(vec![node("artistic_ior", "ior", "multioutput", Vec::new())]);
        let g = compile_value_graph(&doc, &[("ior", Some("extinction")), ("ior", Some("ior"))]);
        assert!(g.is_accepted(), "{:?}", g.refused);
        assert_eq!(g.outputs[0].1, 3);
        assert_eq!(g.outputs[1].1, 3);
    }

    /// An input wired to a nodegraph output that does not exist would read 0;
    /// the graph is refused instead.
    #[test]
    fn a_dangling_graph_output_is_refused() {
        let doc = Doc::from_nodes(vec![node(
            "add",
            "a",
            "float",
            vec![Input {
                name: "in1".into(),
                type_name: "float".into(),
                source: Source::Graph {
                    graph: "missing".into(),
                    output: "out".into(),
                },
                colorspace: None,
                text: None,
            }],
        )]);
        let g = compile_value_graph(&doc, &[("a", None)]);
        assert!(!g.is_accepted());
        assert!(
            g.refused.iter().any(|r| r.contains("missing")),
            "{:?}",
            g.refused
        );
    }

    #[test]
    fn an_unknown_category_is_refused() {
        let doc = Doc::from_nodes(vec![node("frobnicate", "f", "float", Vec::new())]);
        let g = compile_value_graph(&doc, &[("f", None)]);
        assert!(!g.is_accepted());
    }

    #[test]
    fn a_constant_graph_folds_to_constants() {
        let doc = Doc::from_nodes(vec![node(
            "add",
            "a",
            "float",
            vec![
                Input {
                    name: "in1".into(),
                    type_name: "float".into(),
                    source: Source::Value(Val::float(1.0)),
                    colorspace: None,
                    text: None,
                },
                Input {
                    name: "in2".into(),
                    type_name: "float".into(),
                    source: Source::Value(Val::float(2.0)),
                    colorspace: None,
                    text: None,
                },
            ],
        )]);
        let g = compile_value_graph(&doc, &[("a", None)]);
        assert!(g.is_accepted());
        assert!(g.program.ops.is_empty(), "folded");
        let mut slots = Vec::new();
        g.program.eval(&ctx(&[]), &mut slots);
        assert_eq!(slots[g.outputs[0].0 as usize].x(), 3.0);
    }
}
