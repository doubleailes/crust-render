//! Custom nodes expanded into their node-graph implementations
//! (`Doc::expand`), checked by value: each instance must compute exactly what
//! its graph, written out by hand, computes.

use crust_mtlx::{Compiler, Doc, Host, Library, ShadeCtx, TextureRef, Val};
use glam::Vec3A;

fn decline(_: &str, _: crust_mtlx::TexelSpace<'_>) -> Option<TextureRef> {
    None
}

fn ctx(u: f32, v: f32) -> ShadeCtx<'static> {
    ShadeCtx {
        uv: (u, v),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        view: -Vec3A::Z,
        position: Vec3A::new(1.0, 2.0, 3.0),
        uv_width: 0.0,
        curve: None,
        primvars: &[],
    }
}

/// The value of `name` (selecting `output`) in `doc` at `(u, v)`, and the
/// categories the compiler had no operator for.
fn eval_at(doc: &Doc, name: &str, output: Option<&str>, u: f32, v: f32) -> (Val, Vec<String>) {
    let host = Host::new(&decline);
    let mut c = Compiler::new(doc, &host);
    let slot = c.compile_named("", name, output);
    let mut slots = Vec::new();
    c.program.eval(&ctx(u, v), &mut slots);
    (
        slots[slot as usize],
        c.unsupported.iter().cloned().collect(),
    )
}

fn eval(doc: &Doc, name: &str) -> Val {
    let (v, unsupported) = eval_at(doc, name, None, 0.25, 0.75);
    assert!(unsupported.is_empty(), "{unsupported:?}");
    v
}

fn lanes(v: Val) -> [f32; 4] {
    v.v
}

fn library(text: &str) -> Library {
    let mut l = Library::default();
    l.merge_text(text).expect("library parses");
    l
}

fn expanded(doc: &str, lib: &Library) -> Doc {
    let mut d = Doc::parse(doc).expect("document parses");
    d.expand(lib);
    d
}

/// A two-colour blend as a custom node, on standard nodes only.
const BLEND: &str = r#"<materialx version="1.39">
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
    <extract name="r" type="float">
      <input name="in" type="color3" nodename="mix" />
      <input name="index" type="integer" value="0" />
    </extract>
    <output name="blended" type="color3" nodename="mix" />
    <output name="red" type="float" nodename="r" />
  </nodegraph>
</materialx>"#;

#[test]
fn an_unauthored_instance_input_takes_the_nodedef_default() {
    let d = expanded(
        r#"<materialx>
             <studio_mix name="b" type="multioutput">
               <input name="fg_color" type="color3" value="0.8, 0.4, 0.2" />
             </studio_mix>
             <convert name="o" type="color3"><input name="in" type="color3" nodename="b" output="blended" /></convert>
           </materialx>"#,
        &library(BLEND),
    );
    assert!(d.reported.is_empty(), "{:?}", d.reported);
    // mix(fg = fg_color, bg = 0, mix = 0.5).
    let o = lanes(eval(&d, "o"));
    assert_eq!(&o[..3], &[0.4, 0.2, 0.1]);
    assert!(d.find("", "b").is_none(), "the instance is gone");
    assert!(d.find("", "b/mix").is_some());
}

#[test]
fn an_interface_input_used_twice_reads_the_same_instance_input() {
    let lib = library(
        r#"<materialx>
             <nodedef name="ND_twice_float" node="twice">
               <input name="in" type="float" value="0" />
               <output name="out" type="float" />
             </nodedef>
             <nodegraph name="NG_twice_float" nodedef="ND_twice_float">
               <add name="sum" type="float">
                 <input name="in1" type="float" interfacename="in" />
                 <input name="in2" type="float" interfacename="in" />
               </add>
               <output name="out" type="float" nodename="sum" />
             </nodegraph>
           </materialx>"#,
    );
    let d = expanded(
        r#"<materialx>
             <texcoord name="uv" type="vector2" />
             <extract name="u" type="float"><input name="in" type="vector2" nodename="uv" /><input name="index" type="integer" value="0" /></extract>
             <twice name="t" type="float"><input name="in" type="float" nodename="u" /></twice>
             <convert name="o" type="float"><input name="in" type="float" nodename="t" /></convert>
           </materialx>"#,
        &lib,
    );
    let (v, _) = eval_at(&d, "o", None, 0.375, 0.0);
    assert_eq!(v.x(), 0.75, "u + u, from a connection");
    // A terminal naming the instance finds the node standing for it.
    assert_eq!(d.find_node("t").map(|n| n.name.as_str()), Some("t/sum"));
}

/// A reference to an instance's second output follows the graph output to
/// its node, keeping the graph output's own `output=` selection.
#[test]
fn a_multioutput_reference_keeps_the_graph_outputs_selection() {
    let lib = library(
        r#"<materialx>
             <nodedef name="ND_metal" node="metal">
               <input name="color" type="color3" value="0.9, 0.6, 0.2" />
               <output name="n" type="color3" />
               <output name="k" type="color3" />
             </nodedef>
             <nodegraph name="NG_metal" nodedef="ND_metal">
               <artistic_ior name="air" type="multioutput">
                 <input name="reflectivity" type="color3" interfacename="color" />
                 <input name="edge_color" type="color3" interfacename="color" />
               </artistic_ior>
               <output name="n" type="color3" nodename="air" output="ior" />
               <output name="k" type="color3" nodename="air" output="extinction" />
             </nodegraph>
           </materialx>"#,
    );
    let d = expanded(
        r#"<materialx>
             <metal name="m" type="multioutput" />
             <convert name="k" type="color3"><input name="in" type="color3" nodename="m" output="k" /></convert>
             <convert name="n" type="color3"><input name="in" type="color3" nodename="m" output="n" /></convert>
           </materialx>"#,
        &lib,
    );
    let by_hand = Doc::parse(
        r#"<materialx>
             <artistic_ior name="air" type="multioutput">
               <input name="reflectivity" type="color3" value="0.9, 0.6, 0.2" />
               <input name="edge_color" type="color3" value="0.9, 0.6, 0.2" />
             </artistic_ior>
             <convert name="k" type="color3"><input name="in" type="color3" nodename="air" output="extinction" /></convert>
             <convert name="n" type="color3"><input name="in" type="color3" nodename="air" output="ior" /></convert>
           </materialx>"#,
    )
    .unwrap();
    assert_eq!(lanes(eval(&d, "k")), lanes(eval(&by_hand, "k")));
    assert_eq!(lanes(eval(&d, "n")), lanes(eval(&by_hand, "n")));
    assert_ne!(
        lanes(eval(&d, "k")),
        lanes(eval(&d, "n")),
        "two different outputs"
    );
}

/// A layering node's shape: a custom node whose graph chains instances of
/// another custom node through its multioutput, with interface inputs passed
/// down a level.
const LAYERS: &str = r#"<materialx version="1.39">
  <nodedef name="ND_blend_layer" node="blend_layer">
    <input name="fgColor" type="color3" value="0, 0, 0" />
    <input name="fgAlpha" type="float" value="0" />
    <input name="bgColor" type="color3" value="0, 0, 0" />
    <input name="bgAlpha" type="float" value="0" />
    <output name="out" type="color3" />
    <output name="alpha" type="float" />
  </nodedef>
  <nodegraph name="NG_blend_layer" nodedef="ND_blend_layer">
    <mix name="color" type="color3">
      <input name="fg" type="color3" interfacename="fgColor" />
      <input name="bg" type="color3" interfacename="bgColor" />
      <input name="mix" type="float" interfacename="fgAlpha" />
    </mix>
    <mix name="a" type="float">
      <input name="fg" type="float" value="1" />
      <input name="bg" type="float" interfacename="bgAlpha" />
      <input name="mix" type="float" interfacename="fgAlpha" />
    </mix>
    <output name="out" type="color3" nodename="color" />
    <output name="alpha" type="float" nodename="a" />
  </nodegraph>
  <nodedef name="ND_layered" node="layered">
    <input name="layerColor0" type="color3" value="0, 0, 0" />
    <input name="layerAlpha0" type="float" value="0" />
    <input name="layerColor1" type="color3" value="0, 0, 0" />
    <input name="layerAlpha1" type="float" value="0" />
    <output name="outColor" type="color3" />
    <output name="outAlpha" type="float" />
  </nodedef>
  <nodegraph name="NG_layered" nodedef="ND_layered">
    <output name="outColor" type="color3" nodename="layer1" output="out" />
    <output name="outAlpha" type="float" nodename="layer1" output="alpha" />
    <blend_layer name="layer1" type="multioutput">
      <input name="fgColor" type="color3" interfacename="layerColor1" />
      <input name="fgAlpha" type="float" interfacename="layerAlpha1" />
      <input name="bgColor" type="color3" nodename="layer0" output="out" />
      <input name="bgAlpha" type="float" nodename="layer0" output="alpha" />
    </blend_layer>
    <blend_layer name="layer0" type="multioutput">
      <input name="fgColor" type="color3" interfacename="layerColor0" />
      <input name="fgAlpha" type="float" interfacename="layerAlpha0" />
    </blend_layer>
  </nodegraph>
</materialx>"#;

/// The instance, driven by the shading point so nothing folds away.
const LAYERED_DOC: &str = r#"<materialx>
  <texcoord name="uv" type="vector2" />
  <extract name="u" type="float"><input name="in" type="vector2" nodename="uv" /><input name="index" type="integer" value="0" /></extract>
  <extract name="v" type="float"><input name="in" type="vector2" nodename="uv" /><input name="index" type="integer" value="1" /></extract>
  <combine3 name="c0" type="color3"><input name="in1" type="float" nodename="u" /><input name="in2" type="float" value="0.3" /><input name="in3" type="float" nodename="v" /></combine3>
  <layered name="L" type="multioutput">
    <input name="layerColor0" type="color3" nodename="c0" />
    <input name="layerAlpha0" type="float" nodename="v" />
    <input name="layerColor1" type="color3" value="0.1, 0.7, 0.2" />
    <input name="layerAlpha1" type="float" nodename="u" />
  </layered>
  <convert name="color" type="color3"><input name="in" type="color3" nodename="L" output="outColor" /></convert>
  <convert name="alpha" type="float"><input name="in" type="float" nodename="L" output="outAlpha" /></convert>
</materialx>"#;

/// [`LAYERED_DOC`] with both levels expanded by hand.
const LAYERED_BY_HAND: &str = r#"<materialx>
  <texcoord name="uv" type="vector2" />
  <extract name="u" type="float"><input name="in" type="vector2" nodename="uv" /><input name="index" type="integer" value="0" /></extract>
  <extract name="v" type="float"><input name="in" type="vector2" nodename="uv" /><input name="index" type="integer" value="1" /></extract>
  <combine3 name="c0" type="color3"><input name="in1" type="float" nodename="u" /><input name="in2" type="float" value="0.3" /><input name="in3" type="float" nodename="v" /></combine3>
  <mix name="l0_color" type="color3">
    <input name="fg" type="color3" nodename="c0" />
    <input name="bg" type="color3" value="0, 0, 0" />
    <input name="mix" type="float" nodename="v" />
  </mix>
  <mix name="l0_a" type="float">
    <input name="fg" type="float" value="1" />
    <input name="bg" type="float" value="0" />
    <input name="mix" type="float" nodename="v" />
  </mix>
  <mix name="l1_color" type="color3">
    <input name="fg" type="color3" value="0.1, 0.7, 0.2" />
    <input name="bg" type="color3" nodename="l0_color" />
    <input name="mix" type="float" nodename="u" />
  </mix>
  <mix name="l1_a" type="float">
    <input name="fg" type="float" value="1" />
    <input name="bg" type="float" nodename="l0_a" />
    <input name="mix" type="float" nodename="u" />
  </mix>
  <convert name="color" type="color3"><input name="in" type="color3" nodename="l1_color" /></convert>
  <convert name="alpha" type="float"><input name="in" type="float" nodename="l1_a" /></convert>
</materialx>"#;

#[test]
fn a_custom_node_inside_a_custom_node_expands_in_turn() {
    let d = expanded(LAYERED_DOC, &library(LAYERS));
    assert!(d.reported.is_empty(), "{:?}", d.reported);
    for name in ["L/layer0/color", "L/layer1/a"] {
        assert!(d.find("", name).is_some(), "{name}");
    }
    assert!(
        d.nodes
            .iter()
            .all(|n| n.category != "blend_layer" && n.category != "layered")
    );
    // Through both levels: `L` → `L/layer1` → its `color` mix.
    assert_eq!(
        d.find_node("L").map(|n| n.name.as_str()),
        Some("L/layer1/color")
    );
}

/// The bitwise pin: an expanded custom node computes exactly — every lane,
/// every bit — what the same graph written out by hand computes, at points
/// spread over the surface.
#[test]
fn an_expanded_node_is_bit_identical_to_its_graph_by_hand() {
    let d = expanded(LAYERED_DOC, &library(LAYERS));
    let by_hand = Doc::parse(LAYERED_BY_HAND).unwrap();
    for (u, v) in [
        (0.0, 0.0),
        (0.25, 0.75),
        (0.6, 0.1),
        (1.0, 1.0),
        (0.123, 0.987),
    ] {
        for (name, out) in [("color", 3), ("alpha", 1)] {
            let (a, ua) = eval_at(&d, name, None, u, v);
            let (b, ub) = eval_at(&by_hand, name, None, u, v);
            assert!(ua.is_empty() && ub.is_empty(), "{ua:?} {ub:?}");
            let bits = |x: Val| x.v[..out].iter().map(|f| f.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(a), bits(b), "{name} at ({u}, {v}): {a:?} vs {b:?}");
        }
    }
}

#[test]
fn a_nodegraph_output_naming_an_instance_is_redirected() {
    let d = expanded(
        r#"<materialx>
             <nodegraph name="G">
               <studio_mix name="b" type="multioutput">
                 <input name="fg_color" type="color3" value="1, 1, 1" />
                 <input name="amount" type="float" value="0.25" />
               </studio_mix>
               <output name="red" type="float" nodename="b" output="red" />
             </nodegraph>
             <convert name="o" type="float"><input name="in" type="float" nodegraph="G" output="red" /></convert>
           </materialx>"#,
        &library(BLEND),
    );
    assert_eq!(eval(&d, "o").x(), 0.25);
}

#[test]
fn an_output_the_graph_does_not_have_is_reported() {
    let d = expanded(
        r#"<materialx>
             <studio_mix name="b" type="multioutput" />
             <convert name="o" type="float"><input name="in" type="float" nodename="b" output="outputQ" /></convert>
           </materialx>"#,
        &library(BLEND),
    );
    assert_eq!(d.reported.len(), 1, "{:?}", d.reported);
    assert!(d.reported[0].contains("outputQ"), "{:?}", d.reported);
}

#[test]
fn a_self_instantiating_definition_is_cut_and_reported() {
    let lib = library(
        r#"<materialx>
             <nodedef name="ND_loop_float" node="loop">
               <input name="in" type="float" value="1" />
               <output name="out" type="float" />
             </nodedef>
             <nodegraph name="NG_loop_float" nodedef="ND_loop_float">
               <loop name="again" type="float"><input name="in" type="float" interfacename="in" /></loop>
               <output name="out" type="float" nodename="again" />
             </nodegraph>
           </materialx>"#,
    );
    let d = expanded(
        r#"<materialx><loop name="l" type="float" /></materialx>"#,
        &lib,
    );
    assert_eq!(d.reported.len(), 1, "{:?}", d.reported);
    assert!(d.reported[0].contains("a cycle"), "{:?}", d.reported);
    // What is left is one unexpanded `loop`, refused at its first re-entry,
    // which the compiler names.
    let left: Vec<_> = d.nodes.iter().filter(|n| n.category == "loop").collect();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name, "l/again");
    let (_, unsupported) = eval_at(&d, &left[0].name, None, 0.0, 0.0);
    assert_eq!(unsupported, ["loop"]);
}

/// Two definitions that instantiate each other three times over would grow
/// as 3 to the depth cut (43 million nodes at 16); the cycle is refused at
/// its first re-entry instead, after 3 + 9 copies.
#[test]
fn a_cycle_through_two_definitions_is_refused_before_it_grows() {
    let lib = library(
        r#"<materialx>
             <nodedef name="ND_ping" node="ping"><output name="out" type="float" /></nodedef>
             <nodegraph name="NG_ping" nodedef="ND_ping">
               <pong name="a" type="float" /><pong name="b" type="float" /><pong name="c" type="float" />
               <output name="out" type="float" nodename="a" />
             </nodegraph>
             <nodedef name="ND_pong" node="pong"><output name="out" type="float" /></nodedef>
             <nodegraph name="NG_pong" nodedef="ND_pong">
               <ping name="a" type="float" /><ping name="b" type="float" /><ping name="c" type="float" />
               <output name="out" type="float" nodename="a" />
             </nodegraph>
           </materialx>"#,
    );
    let d = expanded(
        r#"<materialx><ping name="p" type="float" /></materialx>"#,
        &lib,
    );
    assert_eq!(
        d.nodes.len(),
        9,
        "{:?}",
        d.nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert!(d.nodes.iter().all(|n| n.category == "ping"));
    assert_eq!(d.reported.len(), 1, "{:?}", d.reported);
    assert!(
        d.reported[0].contains("ND_ping → ND_pong → ND_ping"),
        "{:?}",
        d.reported
    );
}

/// Without a cycle, a graph holding k instances of the next definition still
/// grows as k to the depth; past `MAX_EXPANDED_NODES` the expansion stops
/// and says so once.
#[test]
fn an_expansion_past_the_node_budget_stops() {
    // Eight levels of eight instances each: 8^8 = 16.7 million nodes.
    let mut text = String::from("<materialx>");
    for level in 0..8 {
        text += &format!(
            r#"<nodedef name="ND_l{level}" node="l{level}"><output name="out" type="float" /></nodedef>
               <nodegraph name="NG_l{level}" nodedef="ND_l{level}">"#
        );
        for i in 0..8 {
            if level == 7 {
                text += &format!(
                    r#"<constant name="k{i}" type="float"><input name="value" type="float" value="1" /></constant>"#
                );
            } else {
                text += &format!(r#"<l{} name="n{i}" type="float" />"#, level + 1);
            }
        }
        let first = if level == 7 { "k0" } else { "n0" };
        text += &format!(r#"<output name="out" type="float" nodename="{first}" /></nodegraph>"#);
    }
    text += "</materialx>";
    let d = expanded(
        r#"<materialx><l0 name="root" type="float" /></materialx>"#,
        &library(&text),
    );
    assert!(
        d.nodes.len() <= crust_mtlx::MAX_EXPANDED_NODES,
        "{}",
        d.nodes.len()
    );
    let budget: Vec<_> = d
        .reported
        .iter()
        .filter(|r| r.contains("expand past"))
        .collect();
    assert_eq!(
        budget.len(),
        1,
        "{:?}",
        &d.reported[..d.reported.len().min(4)]
    );
}

/// An instance's `version` picks its nodedef's graph; a version no nodedef
/// has is reported, not expanded with the default version's graph.
#[test]
fn a_versioned_instance_expands_its_own_version() {
    let lib = library(
        r#"<materialx>
             <nodedef name="ND_k_v1" node="k" version="1" isdefaultversion="true"><output name="out" type="float" /></nodedef>
             <nodegraph name="NG_k_v1" nodedef="ND_k_v1">
               <constant name="c" type="float"><input name="value" type="float" value="1" /></constant>
               <output name="out" type="float" nodename="c" />
             </nodegraph>
             <nodedef name="ND_k_v2" node="k" version="2"><output name="out" type="float" /></nodedef>
             <nodegraph name="NG_k_v2" nodedef="ND_k_v2">
               <constant name="c" type="float"><input name="value" type="float" value="2" /></constant>
               <output name="out" type="float" nodename="c" />
             </nodegraph>
           </materialx>"#,
    );
    let d = expanded(
        r#"<materialx>
             <k name="plain" type="float" />
             <k name="two" type="float" version="2" />
             <k name="three" type="float" version="3" />
             <convert name="o1" type="float"><input name="in" type="float" nodename="plain" /></convert>
             <convert name="o2" type="float"><input name="in" type="float" nodename="two" /></convert>
           </materialx>"#,
        &lib,
    );
    assert_eq!(eval(&d, "o1").x(), 1.0);
    assert_eq!(eval(&d, "o2").x(), 2.0);
    assert_eq!(d.reported.len(), 1, "{:?}", d.reported);
    assert!(d.reported[0].contains("three") && d.reported[0].contains("\"3\""));
    assert!(d.find("", "three").is_some(), "left in place");
}

/// A category the library defines only for another output type is not
/// expanded with that type's graph: it is reported and left unsupported.
#[test]
fn an_instance_of_another_type_is_reported_not_expanded() {
    let d = expanded(
        r#"<materialx><studio_mix name="b" type="float" /></materialx>"#,
        &library(BLEND),
    );
    assert_eq!(d.reported.len(), 1, "{:?}", d.reported);
    assert!(d.reported[0].contains("no studio_mix nodedef of type float"));
    assert!(d.find("", "b").is_some(), "left in place");
    let (_, unsupported) = eval_at(&d, "b", None, 0.0, 0.0);
    assert_eq!(unsupported, ["studio_mix"]);
}

/// The compiler's operators always win: a library's node-graph `mix` does
/// not replace the operator.
#[test]
fn a_definition_never_replaces_a_builtin_operator() {
    let lib = library(
        r#"<materialx>
             <nodedef name="ND_mix_float" node="mix">
               <input name="fg" type="float" value="0" />
               <input name="bg" type="float" value="0" />
               <input name="mix" type="float" value="0" />
               <output name="out" type="float" />
             </nodedef>
             <nodegraph name="NG_mix_float" nodedef="ND_mix_float">
               <constant name="seven" type="float"><input name="value" type="float" value="7" /></constant>
               <output name="out" type="float" nodename="seven" />
             </nodegraph>
           </materialx>"#,
    );
    let d = expanded(
        r#"<materialx>
             <mix name="m" type="float">
               <input name="fg" type="float" value="1" />
               <input name="bg" type="float" value="0" />
               <input name="mix" type="float" value="0.5" />
             </mix>
           </materialx>"#,
        &lib,
    );
    assert_eq!(eval(&d, "m").x(), 0.5);
}

/// A document's own definitions come before the library's.
#[test]
fn the_documents_own_definition_wins() {
    let d = expanded(
        r#"<materialx>
             <nodedef name="studio_mix" node="studio_mix">
               <output name="blended" type="color3" />
             </nodedef>
             <nodegraph name="NG_mine" nodedef="studio_mix">
               <constant name="k" type="color3"><input name="value" type="color3" value="0.5, 0.5, 0.5" /></constant>
               <output name="blended" type="color3" nodename="k" />
             </nodegraph>
             <studio_mix name="b" type="color3">
               <input name="fg_color" type="color3" value="1, 1, 1" />
             </studio_mix>
             <convert name="o" type="color3"><input name="in" type="color3" nodename="b" /></convert>
           </materialx>"#,
        &library(BLEND),
    );
    assert_eq!(&lanes(eval(&d, "o"))[..3], &[0.5, 0.5, 0.5]);
}

/// An unsupported node hides nothing: every category without an operator
/// upstream of it is named too, though none of it is compiled.
#[test]
fn unsupported_nodes_behind_an_unsupported_node_are_named() {
    let d = Doc::parse(
        r#"<materialx>
             <wobble name="w" type="color3" />
             <multiply name="m" type="color3"><input name="in1" type="color3" nodename="w" /></multiply>
             <frobnicate name="f" type="color3"><input name="in" type="color3" nodename="m" /></frobnicate>
             <convert name="o" type="color3"><input name="in" type="color3" nodename="f" /></convert>
           </materialx>"#,
    )
    .unwrap();
    let (v, unsupported) = eval_at(&d, "o", None, 0.0, 0.0);
    assert_eq!(unsupported, ["frobnicate", "wobble"]);
    assert_eq!(v.x(), 0.5, "the unsupported node's mid-grey, as before");
}
