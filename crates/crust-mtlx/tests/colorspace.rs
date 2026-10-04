//! MaterialX colour-space handling: which `colorspace` applies to an input
//! (input ?? node ?? nodegraph ?? document), what is converted (authored
//! `color3` / `color4` literals and image defaults, never other types, never
//! nodedef defaults), and what the texture loader is told (a colour image's
//! effective space, `None` for a data image).

use crust_mtlx::{
    Closures, Compiler, Doc, Host, Op, ShadeCtx, Texture, TextureRef, Val, compile, flatten,
};
use glam::Vec3A;
use std::cell::RefCell;
use std::sync::Arc;

/// A stand-in for the host's OCIO conversion, chosen so every space leaves a
/// distinct fingerprint: each scales by its own factor, and `log` offsets,
/// which is the one that does not keep black at black.
fn convert(space: &str, rgb: [f32; 3]) -> [f32; 3] {
    let scale = |k: f32| rgb.map(|c| c * k);
    match space {
        "srgb_texture" => scale(2.0),
        "lin_rec709" => scale(3.0),
        "log" => rgb.map(|c| c + 1.0),
        other => panic!("converted from an unexpected space '{other}'"),
    }
}

fn decline(_: &str, _: Option<&str>) -> Option<TextureRef> {
    None
}

struct Flat;

impl Texture for Flat {
    fn eval(&self, _: f32, _: f32, _: f32) -> [f32; 4] {
        [0.5, 0.5, 0.5, 1.0]
    }
}

fn ctx() -> ShadeCtx {
    ShadeCtx {
        uv: (0.25, 0.75),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        view: -Vec3A::Z,
        position: Vec3A::ZERO,
        uv_width: 0.0,
    }
}

/// `name` in `graph` (empty for document scope) compiled under the test
/// converter, and its value.
fn run_in(doc: &str, graph: &str, name: &str) -> Val {
    let d = Doc::parse(doc).expect("document parses");
    let host = Host {
        load_texture: &decline,
        convert_color: &convert,
    };
    let mut c = Compiler::new(&d, &host);
    let slot = c.compile_named(graph, name, None);
    let mut slots = Vec::new();
    c.program.eval(&ctx(), &mut slots);
    slots[slot as usize]
}

fn run(doc: &str, name: &str) -> Val {
    run_in(doc, "", name)
}

/// Every `(file, colorspace)` the loader was asked for while compiling the
/// nodes `names` of `doc`, in order.
fn loads(doc: &str, names: &[(&str, &str)]) -> Vec<(String, Option<String>)> {
    let d = Doc::parse(doc).expect("document parses");
    let asked = RefCell::new(Vec::new());
    let loader = |f: &str, cs: Option<&str>| -> Option<TextureRef> {
        asked
            .borrow_mut()
            .push((f.to_string(), cs.map(str::to_string)));
        Some(TextureRef(Arc::new(Flat)))
    };
    let host = Host {
        load_texture: &loader,
        convert_color: &convert,
    };
    let mut c = Compiler::new(&d, &host);
    for (graph, name) in names {
        c.compile_named(graph, name, None);
    }
    drop(c);
    asked.into_inner()
}

fn rgb(v: Val) -> [f32; 3] {
    v.rgb().to_array()
}

fn space(s: &str) -> Option<String> {
    Some(s.to_string())
}

#[test]
fn a_document_colorspace_reaches_a_literal_colour_and_an_image_file() {
    let doc = r#"<materialx colorspace="srgb_texture">
        <constant name="c" type="color3">
          <input name="value" type="color3" value="0.1, 0.2, 0.3" />
        </constant>
        <image name="tex" type="color3">
          <input name="file" type="filename" value="albedo.png" />
        </image>
      </materialx>"#;
    assert_eq!(rgb(run(doc, "c")), [0.2, 0.4, 0.6]);
    assert_eq!(
        loads(doc, &[("", "tex")]),
        [("albedo.png".to_string(), space("srgb_texture"))]
    );
}

#[test]
fn a_nodegraph_colorspace_overrides_the_document() {
    let doc = r#"<materialx colorspace="srgb_texture">
        <nodegraph name="g" colorspace="lin_rec709">
          <constant name="c" type="color3">
            <input name="value" type="color3" value="0.1, 0.2, 0.3" />
          </constant>
          <image name="tex" type="color3">
            <input name="file" type="filename" value="albedo.png" />
          </image>
          <output name="out" type="color3" nodename="c" />
        </nodegraph>
        <constant name="outside" type="color3">
          <input name="value" type="color3" value="0.1, 0.2, 0.3" />
        </constant>
      </materialx>"#;
    assert_eq!(rgb(run_in(doc, "g", "c")), [0.3, 0.6, 0.90000004]);
    assert_eq!(rgb(run(doc, "outside")), [0.2, 0.4, 0.6]);
    assert_eq!(
        loads(doc, &[("g", "tex")]),
        [("albedo.png".to_string(), space("lin_rec709"))]
    );
}

#[test]
fn an_input_colorspace_overrides_its_node_and_a_node_its_document() {
    let doc = r#"<materialx colorspace="log">
        <add name="sum" type="color3" colorspace="lin_rec709">
          <input name="in1" type="color3" value="0.1, 0.1, 0.1" colorspace="srgb_texture" />
          <input name="in2" type="color3" value="0.1, 0.1, 0.1" />
        </add>
        <image name="tex" type="color3" colorspace="lin_rec709">
          <input name="file" type="filename" value="albedo.png" colorspace="srgb_texture" />
        </image>
        <image name="tex2" type="color3" colorspace="lin_rec709">
          <input name="file" type="filename" value="albedo.png" />
        </image>
      </materialx>"#;
    // 0.1·2 from the input's own space, plus 0.1·3 from the node's.
    let v = run(doc, "sum");
    assert!(
        (v.rgb() - Vec3A::splat(0.5)).abs().max_element() < 1e-6,
        "{v:?}"
    );
    assert_eq!(
        loads(doc, &[("", "tex"), ("", "tex2")]),
        [
            ("albedo.png".to_string(), space("srgb_texture")),
            ("albedo.png".to_string(), space("lin_rec709")),
        ]
    );
}

#[test]
fn an_empty_colorspace_stops_the_inheritance() {
    let doc = r#"<materialx colorspace="srgb_texture">
        <constant name="c" type="color3">
          <input name="value" type="color3" value="0.1, 0.2, 0.3" colorspace="" />
        </constant>
        <image name="tex" type="color3" colorspace="">
          <input name="file" type="filename" value="albedo.png" />
        </image>
      </materialx>"#;
    assert_eq!(rgb(run(doc, "c")), [0.1, 0.2, 0.3]);
    assert_eq!(
        loads(doc, &[("", "tex")]),
        [("albedo.png".to_string(), None)]
    );
}

#[test]
fn a_data_image_gets_no_colorspace_whatever_the_document_says() {
    let doc = r#"<materialx colorspace="srgb_texture">
        <image name="mask" type="float">
          <input name="file" type="filename" value="mask.png" />
        </image>
        <image name="nrm" type="vector3">
          <input name="file" type="filename" value="normal.png" colorspace="srgb_texture" />
        </image>
        <tiledimage name="rgba" type="color4">
          <input name="file" type="filename" value="rgba.png" />
        </tiledimage>
      </materialx>"#;
    assert_eq!(
        loads(doc, &[("", "mask"), ("", "nrm"), ("", "rgba")]),
        [
            ("mask.png".to_string(), None),
            ("normal.png".to_string(), None),
            ("rgba.png".to_string(), space("srgb_texture")),
        ]
    );
}

#[test]
fn one_file_in_one_effective_space_is_loaded_once() {
    // The memo is keyed on the space handed to the loader: the same file in
    // the same *effective* space is one sampler however that space was
    // spelled, and a second space is a second load.
    let doc = r#"<materialx colorspace="srgb_texture">
        <image name="a" type="color3">
          <input name="file" type="filename" value="albedo.png" colorspace="srgb_texture" />
        </image>
        <image name="b" type="color3">
          <input name="file" type="filename" value="albedo.png" />
        </image>
        <image name="c" type="color3">
          <input name="file" type="filename" value="albedo.png" colorspace="lin_rec709" />
        </image>
      </materialx>"#;
    assert_eq!(
        loads(doc, &[("", "a"), ("", "b"), ("", "c")]),
        [
            ("albedo.png".to_string(), space("srgb_texture")),
            ("albedo.png".to_string(), space("lin_rec709")),
        ]
    );
}

#[test]
fn non_colour_literals_are_never_converted() {
    let doc = r#"<materialx colorspace="srgb_texture">
        <constant name="f" type="float">
          <input name="value" type="float" value="0.3" colorspace="srgb_texture" />
        </constant>
        <constant name="v" type="vector3">
          <input name="value" type="vector3" value="0.1, 0.2, 0.3" />
        </constant>
      </materialx>"#;
    assert_eq!(run(doc, "f"), Val::float(0.3));
    assert_eq!(run(doc, "v"), Val::vec3(0.1, 0.2, 0.3));
}

#[test]
fn unauthored_defaults_are_not_converted() {
    // `in2` is the nodedef's 1: converted it would double the product too.
    // The authored broadcast `0.25` is converted, and widens to a colour.
    let doc = r#"<materialx colorspace="srgb_texture">
        <multiply name="m" type="color3">
          <input name="in1" type="color3" value="0.25" />
        </multiply>
      </materialx>"#;
    let v = run(doc, "m");
    assert_eq!(v.arity, 3);
    assert_eq!(rgb(v), [0.5, 0.5, 0.5]);
}

#[test]
fn a_colour4_converts_its_rgb_and_keeps_its_alpha() {
    let doc = r#"<materialx colorspace="srgb_texture">
        <constant name="c" type="color4">
          <input name="value" type="color4" value="0.1, 0.2, 0.3, 0.4" />
        </constant>
        <constant name="b" type="color4">
          <input name="value" type="color4" value="0.25" />
        </constant>
      </materialx>"#;
    assert_eq!(run(doc, "c"), Val::vec4(0.2, 0.4, 0.6, 0.4));
    // A broadcast's alpha is the scalar as authored.
    assert_eq!(run(doc, "b"), Val::vec4(0.5, 0.5, 0.5, 0.25));
}

#[test]
fn an_identity_host_leaves_every_literal_bit_for_bit() {
    let doc = r#"<materialx colorspace="srgb_texture">
        <constant name="c" type="color3">
          <input name="value" type="color3" value="0.25" />
        </constant>
      </materialx>"#;
    let d = Doc::parse(doc).unwrap();
    let mut c = Compiler::new(&d, &Host::new(&decline));
    c.compile_named("", "c", None);
    // Still the authored broadcast: same lanes, same width.
    assert!(
        matches!(c.program.ops[..], [Op::Const(v)] if v == Val::float(0.25)),
        "{:?}",
        c.program.ops
    );
}

#[test]
fn an_image_default_is_converted_from_its_own_colorspace() {
    let doc = r#"<materialx colorspace="srgb_texture">
        <image name="tex" type="color3">
          <input name="file" type="filename" value="missing.png" />
          <input name="default" type="color3" value="0.1, 0.2, 0.3" colorspace="lin_rec709" />
        </image>
        <image name="inherits" type="color3">
          <input name="file" type="filename" value="missing.png" />
          <input name="default" type="color3" value="0.1, 0.2, 0.3" />
        </image>
        <image name="bare" type="color3">
          <input name="file" type="filename" value="missing.png" />
        </image>
        <image name="mask" type="float">
          <input name="file" type="filename" value="missing.png" />
          <input name="default" type="float" value="0.3" />
        </image>
      </materialx>"#;
    // Declined, so each evaluates to its fallback.
    assert_eq!(rgb(run(doc, "tex")), [0.3, 0.6, 0.90000004]);
    assert_eq!(rgb(run(doc, "inherits")), [0.2, 0.4, 0.6]);
    // No authored default: the mid-grey stand-in, unconverted.
    assert_eq!(run(doc, "bare"), Val::float(0.5));
    assert_eq!(run(doc, "mask"), Val::float(0.3));
}

/// The emission terms of the `surface` over a `uniform_edf` of `color`,
/// authored in `space`.
fn emission_of(space: &str, color: &str) -> Closures {
    let doc = format!(
        r#"<materialx colorspace="{space}">
             <uniform_edf name="e" type="EDF">
               <input name="color" type="color3" value="{color}" />
             </uniform_edf>
             <surface name="s" type="surfaceshader">
               <input name="edf" type="EDF" nodename="e" />
             </surface>
           </materialx>"#
    );
    let d = Doc::parse(&doc).unwrap();
    let host = Host {
        load_texture: &decline,
        convert_color: &convert,
    };
    let mut c = Compiler::new(&d, &host);
    let root = d.find("", "s").unwrap().clone();
    let mut out = Closures::default();
    flatten(&mut c, &root, &mut out);
    out
}

#[test]
fn literal_zero_pruning_tests_the_converted_colour() {
    // A matrix keeps black at black, so the dummy EDF is still dropped …
    assert!(emission_of("srgb_texture", "0, 0, 0").emission.is_empty());
    // … but a conversion that lifts black makes it a real emitter, and
    // pruning on the authored zero would have dropped a glow the program
    // computes.
    assert_eq!(emission_of("log", "0, 0, 0").emission.len(), 1);
}

#[test]
fn compile_threads_the_host_through() {
    let doc = r#"<?xml version="1.0"?>
<materialx version="1.39" colorspace="srgb_texture">
  <image name="tex" type="color3">
    <input name="file" type="filename" value="albedo.png" />
  </image>
  <oren_nayar_diffuse_bsdf name="diffuse" type="BSDF">
    <input name="color" type="color3" nodename="tex" />
    <input name="roughness" type="float" value="0.25" />
  </oren_nayar_diffuse_bsdf>
  <uniform_edf name="glow" type="EDF">
    <input name="color" type="color3" value="0.1, 0.2, 0.3" />
  </uniform_edf>
  <surface name="surf" type="surfaceshader">
    <input name="bsdf" type="BSDF" nodename="diffuse" />
    <input name="edf" type="EDF" nodename="glow" />
  </surface>
  <surfacematerial name="mat" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="surf" />
  </surfacematerial>
</materialx>"#;
    let dir = std::env::temp_dir().join("crust_mtlx_colorspace_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("host.mtlx");
    std::fs::write(&path, doc).unwrap();

    let asked = RefCell::new(Vec::new());
    let loader = |f: &str, cs: Option<&str>| -> Option<TextureRef> {
        asked
            .borrow_mut()
            .push((f.to_string(), cs.map(str::to_string)));
        Some(TextureRef(Arc::new(Flat)))
    };
    let host = Host {
        load_texture: &loader,
        convert_color: &convert,
    };
    let c = compile(&path, None, &host).expect("compiles");
    assert_eq!(
        asked.into_inner(),
        [("albedo.png".to_string(), space("srgb_texture"))]
    );
    assert_eq!(c.textures, 1);
    let mut slots = Vec::new();
    c.program.eval(&ctx(), &mut slots);
    let glow = slots[c.closures.emission[0].color as usize];
    assert_eq!(rgb(glow), [0.2, 0.4, 0.6]);
}
