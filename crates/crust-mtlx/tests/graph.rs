//! Integration tests for the standalone MaterialX reader: parsing inline
//! documents, compiling them to programs, evaluating every operator the
//! compiler implements, flattening BSDF trees and compiling the checked-in
//! sample document.

use crust_mtlx::{
    BinOp, Compiler, Doc, Flattened, LobeKind, MtlxError, Op, Program, ShadeCtx, Source, Texture,
    TextureRef, Val, compile, flatten, reflectivity_from_ior,
};
use glam::Vec3A;
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn decline(_: &str, _: Option<&str>) -> Option<TextureRef> {
    None
}

fn ctx() -> ShadeCtx {
    ShadeCtx {
        uv: (0.25, 0.75),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        view: -Vec3A::Z,
        position: Vec3A::new(1.0, 2.0, 3.0),
        uv_width: 0.0,
    }
}

/// Compiles the node `name` of `doc` and returns its value at the default
/// shading context.
fn run_with(doc: &str, name: &str, ctx: &ShadeCtx, loader: crust_mtlx::TextureLoader<'_>) -> Val {
    let d = Doc::parse(doc).expect("document parses");
    let mut c = Compiler::new(&d, loader);
    let slot = c.compile_named("", name, None);
    let mut slots = Vec::new();
    c.program.eval(ctx, &mut slots);
    slots[slot as usize]
}

fn run(doc: &str, name: &str) -> Val {
    run_with(doc, name, &ctx(), &decline)
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

/// A one-node document of `category` with scalar/vector inputs.
fn unary(category: &str, ty: &str, value: &str) -> String {
    format!(
        r#"<materialx>
             <{category} name="n" type="{ty}">
               <input name="in" type="{ty}" value="{value}" />
             </{category}>
           </materialx>"#
    )
}

fn binary(category: &str, ty: &str, a: &str, b: &str) -> String {
    format!(
        r#"<materialx>
             <{category} name="n" type="{ty}">
               <input name="in1" type="{ty}" value="{a}" />
               <input name="in2" type="{ty}" value="{b}" />
             </{category}>
           </materialx>"#
    )
}

/// Lobe kinds and weights of a compiled material, evaluated at `ctx`.
fn lobes_of(doc: &str, root: &str) -> Vec<(LobeKind, f32, Vec3A, f32)> {
    let d = Doc::parse(doc).unwrap();
    let mut c = Compiler::new(&d, &decline);
    let one = c.constant(Val::ONE);
    let root = d.find("", root).expect("root node").clone();
    let mut flat = Flattened::default();
    flatten(&mut c, &root, one, 0, &mut flat);
    let mut slots = Vec::new();
    c.program.eval(&ctx(), &mut slots);
    flat.lobes
        .iter()
        .map(|l| {
            (
                l.kind,
                slots[l.weight as usize].x(),
                slots[l.color as usize].rgb(),
                slots[l.roughness as usize].x(),
            )
        })
        .collect()
}

/// Emission terms of a compiled material — `(weight, radiance)` at `ctx` —
/// alongside the lobe kinds, so a test can assert that reading the `edf` left
/// the BSDF side alone.
///
/// The weight is a `Vec3A` rather than an `f32` because the slot genuinely is
/// not a scalar: MaterialX's `ND_multiply_edfC` tints an EDF by a `color3`, and
/// `Mul` promotes arity. `Val::rgb` broadcasts an arity-1 value, so a `float`
/// weight still reads as three equal channels.
fn emission_of(doc: &str, root: &str) -> (Vec<(Vec3A, Vec3A)>, Vec<LobeKind>) {
    let d = Doc::parse(doc).unwrap();
    let mut c = Compiler::new(&d, &decline);
    let one = c.constant(Val::ONE);
    let root = d.find("", root).expect("root node").clone();
    let mut flat = Flattened::default();
    flatten(&mut c, &root, one, 0, &mut flat);
    let mut slots = Vec::new();
    c.program.eval(&ctx(), &mut slots);
    let terms = flat
        .emission
        .iter()
        .map(|e| {
            (
                slots[e.weight as usize].rgb(),
                slots[e.color as usize].rgb(),
            )
        })
        .collect();
    (terms, flat.lobes.iter().map(|l| l.kind).collect())
}

/// Node categories the compiler had nothing for, for the EDF tests.
fn unsupported_of(doc: &str, root: &str) -> Vec<String> {
    let d = Doc::parse(doc).unwrap();
    let mut c = Compiler::new(&d, &decline);
    let one = c.constant(Val::ONE);
    let root = d.find("", root).expect("root node").clone();
    let mut flat = Flattened::default();
    flatten(&mut c, &root, one, 0, &mut flat);
    c.unsupported.iter().cloned().collect()
}

fn sample_mtlx() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("samples")
        .join("materialx_basic.mtlx")
}

// ---------------------------------------------------------------------------
// Val
// ---------------------------------------------------------------------------

#[test]
fn val_constructors_set_arity() {
    assert_eq!(Val::float(2.0).arity, 1);
    assert_eq!(Val::vec2(1.0, 2.0).arity, 2);
    assert_eq!(Val::vec3(1.0, 2.0, 3.0).arity, 3);
    assert_eq!(Val::vec4(1.0, 2.0, 3.0, 4.0).arity, 4);
    assert_eq!(Val::ZERO, Val::float(0.0));
    assert_eq!(Val::ONE, Val::float(1.0));
    // A float fills every lane, so any lane read is the same number.
    assert_eq!(Val::float(2.0).v, [2.0; 4]);
}

#[test]
fn val_rgb_broadcasts_scalars_only() {
    assert_eq!(Val::float(0.4).rgb(), Vec3A::splat(0.4));
    assert_eq!(Val::vec3(0.1, 0.2, 0.3).rgb(), Vec3A::new(0.1, 0.2, 0.3));
    // A vector2 does not broadcast: its third lane is what it holds (zero).
    assert_eq!(Val::vec2(0.5, 0.6).rgb(), Vec3A::new(0.5, 0.6, 0.0));
    assert_eq!(Val::vec3(0.1, 0.2, 0.3).x(), 0.1);
}

#[test]
fn val_with_arity_clamps_and_keeps_lanes() {
    let v = Val::vec3(1.0, 2.0, 3.0);
    assert_eq!(v.with_arity(2).arity, 2);
    assert_eq!(v.with_arity(2).v, v.v);
    assert_eq!(v.with_arity(0).arity, 1);
    assert_eq!(v.with_arity(9).arity, 4);
}

#[test]
fn val_zip_takes_the_wider_arity_and_broadcasts_floats() {
    let s = Val::float(2.0);
    let v = Val::vec2(1.0, 3.0);
    let r = s.zip(v, |a, b| a * b);
    assert_eq!(r.arity, 2);
    assert_eq!(r.v[0..2], [2.0, 6.0]);
    let r = v.zip(s, |a, b| a - b);
    assert_eq!(r.v[0..2], [-1.0, 1.0]);
    // Two vectors of equal width stay lane-wise.
    let r = Val::vec3(1.0, 2.0, 3.0).zip(Val::vec3(3.0, 2.0, 1.0), f32::max);
    assert_eq!(r.v[0..3], [3.0, 2.0, 3.0]);
}

#[test]
fn val_map_touches_every_lane() {
    let r = Val::vec4(1.0, 4.0, 9.0, 16.0).map(f32::sqrt);
    assert_eq!(r.v, [1.0, 2.0, 3.0, 4.0]);
    assert_eq!(r.arity, 4);
}

#[test]
fn val_from_vec3a() {
    let v: Val = Vec3A::new(0.5, 0.25, 0.125).into();
    assert_eq!(v.arity, 3);
    assert_eq!(v.rgb(), Vec3A::new(0.5, 0.25, 0.125));
}

#[test]
fn parse_literal_handles_edge_cases() {
    use crust_mtlx::value::{arity_of, parse_literal};
    assert!(parse_literal("", "float").is_none());
    assert!(parse_literal("abc", "float").is_none());
    assert!(parse_literal("1, x", "vector2").is_none());
    // Trailing comma and whitespace are tolerated.
    let v = parse_literal(" 1 ,2, ", "vector2").unwrap();
    assert_eq!(v.arity, 2);
    assert_eq!(v.v[0..2], [1.0, 2.0]);
    // More than four numbers are truncated to four lanes.
    let v = parse_literal("1,2,3,4,5,6", "color4").unwrap();
    assert_eq!(v.arity, 4);
    assert_eq!(v.v, [1.0, 2.0, 3.0, 4.0]);
    assert_eq!(arity_of("color4"), 4);
    assert_eq!(arity_of("vector2"), 2);
    assert_eq!(arity_of("BSDF"), 1);
    assert_eq!(arity_of("float"), 1);
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[test]
fn malformed_xml_is_an_xml_error() {
    let err = match Doc::parse("<materialx><foo></materialx>") {
        Ok(_) => panic!("unbalanced markup must not parse"),
        Err(e) => e,
    };
    assert!(matches!(err, MtlxError::Xml(_)), "{err}");
    assert!(err.to_string().contains("malformed XML"));
}

#[test]
fn a_missing_file_is_an_io_error() {
    let err = match Doc::open(std::path::Path::new("/nonexistent/dir/none.mtlx")) {
        Ok(_) => panic!("a missing file must not open"),
        Err(e) => e,
    };
    assert!(matches!(err, MtlxError::Io(_)), "{err}");
    assert!(err.to_string().contains("cannot read"));
}

#[test]
fn compile_reports_a_missing_material_node() {
    let dir = std::env::temp_dir().join("crust_mtlx_missing");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("empty.mtlx");
    std::fs::write(&path, "<materialx></materialx>").unwrap();
    let err = compile(&path, None, &decline).err().expect("no material");
    assert!(matches!(err, MtlxError::NoSuchMaterial(_)), "{err}");
    let err = compile(&path, Some("nope"), &decline).err().unwrap();
    match err {
        MtlxError::NoSuchMaterial(n) => assert_eq!(n, "nope"),
        other => panic!("{other}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn nodes_carry_their_category_type_and_inputs() {
    let d = Doc::parse(
        r#"<materialx>
             <multiply name="m" type="color3">
               <input name="in1" type="color3" value="1, 0.5, 0.25" />
               <input name="in2" type="float" value="2" />
             </multiply>
           </materialx>"#,
    )
    .unwrap();
    assert_eq!(d.nodes.len(), 1);
    let m = d.find("", "m").unwrap();
    assert_eq!(m.category, "multiply");
    assert_eq!(m.type_name, "color3");
    assert!(m.graph.is_none());
    assert_eq!(m.inputs.len(), 2);
    let in1 = m.input("in1").unwrap();
    assert_eq!(in1.type_name, "color3");
    match in1.source {
        Source::Value(v) => assert_eq!(v.rgb(), Vec3A::new(1.0, 0.5, 0.25)),
        _ => panic!("literal expected"),
    }
    assert!(m.input("missing").is_none());
    assert!(d.find("", "missing").is_none());
}

#[test]
fn nodegraph_nodes_are_scoped_and_outputs_resolve() {
    let d = Doc::parse(
        r#"<materialx>
             <nodegraph name="g">
               <constant name="c" type="float"><input name="value" type="float" value="3" /></constant>
               <output name="out" type="float" nodename="c" />
             </nodegraph>
             <constant name="c" type="float"><input name="value" type="float" value="7" /></constant>
           </materialx>"#,
    )
    .unwrap();
    let inner = d.find("g", "c").unwrap();
    assert_eq!(inner.graph.as_deref(), Some("g"));
    let outer = d.find("", "c").unwrap();
    assert!(outer.graph.is_none());
    let conn = d.graph_output("g", "out").unwrap();
    assert_eq!(conn.node.graph.as_deref(), Some("g"));
    assert!(conn.output.is_none());
    assert!(d.graph_output("g", "nope").is_none());
    // A node inside a graph can also see document-scope names.
    assert!(d.find("g", "nothing").is_none());
    assert_eq!(d.by_category("constant").count(), 2);
}

#[test]
fn graph_output_references_evaluate_through_the_graph() {
    let v = run(
        r#"<materialx>
             <nodegraph name="g">
               <constant name="c" type="float"><input name="value" type="float" value="3" /></constant>
               <output name="out" type="float" nodename="c" />
             </nodegraph>
             <multiply name="m" type="float">
               <input name="in1" type="float" nodegraph="g" output="out" />
               <input name="in2" type="float" value="2" />
             </multiply>
           </materialx>"#,
        "m",
    );
    assert!(approx(v.x(), 6.0));
}

#[test]
fn a_dangling_graph_reference_is_zero_not_a_panic() {
    let v = run(
        r#"<materialx>
             <add name="a" type="float">
               <input name="in1" type="float" nodegraph="nope" output="out" />
               <input name="in2" type="float" value="1" />
             </add>
           </materialx>"#,
        "a",
    );
    assert!(approx(v.x(), 1.0));
}

#[test]
fn a_dangling_node_reference_is_zero() {
    let v = run(
        r#"<materialx>
             <add name="a" type="float">
               <input name="in1" type="float" nodename="ghost" />
               <input name="in2" type="float" value="4" />
             </add>
           </materialx>"#,
        "a",
    );
    assert!(approx(v.x(), 4.0));
}

#[test]
fn filename_inputs_keep_text_and_colorspace() {
    let d = Doc::parse(
        r#"<materialx>
             <image name="t" type="color3">
               <input name="file" type="filename" value="tex/a.<UVTILE>.png" colorspace="g22_rec709" />
             </image>
           </materialx>"#,
    )
    .unwrap();
    let f = d.find("", "t").unwrap().input("file").unwrap();
    assert_eq!(f.text.as_deref(), Some("tex/a.<UVTILE>.png"));
    assert_eq!(f.colorspace.as_deref(), Some("g22_rec709"));
    assert!(matches!(f.source, Source::Value(_)) || f.text.is_some());
}

// ---------------------------------------------------------------------------
// Operators
// ---------------------------------------------------------------------------

#[test]
fn arithmetic_binaries() {
    assert!(approx(
        run(&binary("add", "float", "1.5", "2"), "n").x(),
        3.5
    ));
    assert!(approx(
        run(&binary("subtract", "float", "1.5", "2"), "n").x(),
        -0.5
    ));
    assert!(approx(
        run(&binary("multiply", "float", "1.5", "2"), "n").x(),
        3.0
    ));
    assert!(approx(
        run(&binary("divide", "float", "1.5", "2"), "n").x(),
        0.75
    ));
    assert!(approx(
        run(&binary("power", "float", "2", "3"), "n").x(),
        8.0
    ));
    assert!(approx(run(&binary("min", "float", "2", "3"), "n").x(), 2.0));
    assert!(approx(run(&binary("max", "float", "2", "3"), "n").x(), 3.0));
    assert!(approx(
        run(&binary("modulo", "float", "7", "3"), "n").x(),
        1.0
    ));
}

#[test]
fn binaries_apply_lane_wise_over_vectors() {
    let v = run(&binary("multiply", "color3", "1, 2, 3", "2, 3, 4"), "n");
    assert_eq!(v.arity, 3);
    assert_eq!(v.v[0..3], [2.0, 6.0, 12.0]);
    let v = run(&binary("add", "vector2", "1, 2", "0.5, 0.25"), "n");
    assert_eq!(v.arity, 2);
    assert_eq!(v.v[0..2], [1.5, 2.25]);
}

#[test]
fn a_float_operand_broadcasts_into_a_colour() {
    let v = run(
        r#"<materialx>
             <multiply name="n" type="color3">
               <input name="in1" type="color3" value="0.2, 0.4, 0.8" />
               <input name="in2" type="float" value="0.5" />
             </multiply>
           </materialx>"#,
        "n",
    );
    assert_eq!(v.arity, 3);
    assert!(approx(v.v[0], 0.1) && approx(v.v[1], 0.2) && approx(v.v[2], 0.4));
}

#[test]
fn unauthored_binary_inputs_take_their_defaults() {
    // multiply defaults both inputs to 1, add to 0.
    let v = run(
        r#"<materialx><multiply name="n" type="float" /></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 1.0));
    let v = run(
        r#"<materialx><add name="n" type="float" /></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 0.0));
    let v = run(
        r#"<materialx><multiply name="n" type="float"><input name="in1" type="float" value="5" /></multiply></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 5.0));
}

#[test]
fn unary_math_nodes() {
    assert!(approx(run(&unary("absval", "float", "-2.5"), "n").x(), 2.5));
    assert!(approx(run(&unary("sqrt", "float", "16"), "n").x(), 4.0));
    assert!(approx(run(&unary("floor", "float", "2.7"), "n").x(), 2.0));
    assert!(approx(run(&unary("ceil", "float", "2.2"), "n").x(), 3.0));
    assert!(approx(run(&unary("sign", "float", "-0.3"), "n").x(), -1.0));
    assert!(approx(run(&unary("exp", "float", "0"), "n").x(), 1.0));
    assert!(approx(run(&unary("ln", "float", "1"), "n").x(), 0.0));
    assert!((run(&unary("ln", "float", "2.718281828"), "n").x() - 1.0).abs() < 1e-4);
    assert!(approx(run(&unary("sin", "float", "0"), "n").x(), 0.0));
    assert!(approx(run(&unary("cos", "float", "0"), "n").x(), 1.0));
    assert!(
        (run(&unary("asin", "float", "1"), "n").x() - std::f32::consts::FRAC_PI_2).abs() < 1e-5
    );
    assert!(approx(run(&unary("acos", "float", "1"), "n").x(), 0.0));
}

#[test]
fn normalize_makes_a_unit_vector() {
    let v = run(&unary("normalize", "vector3", "0, 3, 4"), "n");
    assert_eq!(v.arity, 3);
    assert!(approx(v.v[0], 0.0) && approx(v.v[1], 0.6) && approx(v.v[2], 0.8));
}

#[test]
fn clamp_defaults_to_the_unit_interval() {
    let doc = |x: &str| {
        format!(
            r#"<materialx><clamp name="n" type="float"><input name="in" type="float" value="{x}" /></clamp></materialx>"#
        )
    };
    assert!(approx(run(&doc("1.7"), "n").x(), 1.0));
    assert!(approx(run(&doc("-0.2"), "n").x(), 0.0));
    assert!(approx(run(&doc("0.4"), "n").x(), 0.4));
    let v = run(
        r#"<materialx><clamp name="n" type="float">
             <input name="in" type="float" value="7" />
             <input name="low" type="float" value="2" />
             <input name="high" type="float" value="5" />
           </clamp></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 5.0));
}

#[test]
fn remap_rescales_between_ranges() {
    let v = run(
        r#"<materialx><remap name="n" type="float">
             <input name="in" type="float" value="0.25" />
             <input name="inlow" type="float" value="0" />
             <input name="inhigh" type="float" value="1" />
             <input name="outlow" type="float" value="10" />
             <input name="outhigh" type="float" value="20" />
           </remap></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 12.5));
    // Defaults are the identity mapping.
    let v = run(
        r#"<materialx><remap name="n" type="float"><input name="in" type="float" value="0.3" /></remap></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 0.3));
}

#[test]
fn contrast_pivots_around_its_pivot() {
    let v = run(
        r#"<materialx><contrast name="n" type="float">
             <input name="in" type="float" value="0.75" />
             <input name="amount" type="float" value="2" />
             <input name="pivot" type="float" value="0.5" />
           </contrast></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 1.0));
    // The pivot itself is a fixed point whatever the amount.
    let v = run(
        r#"<materialx><contrast name="n" type="float">
             <input name="in" type="float" value="0.5" />
             <input name="amount" type="float" value="9" />
             <input name="pivot" type="float" value="0.5" />
           </contrast></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 0.5));
}

#[test]
fn invert_subtracts_from_its_amount() {
    let v = run(&unary("invert", "float", "0.3"), "n");
    assert!(approx(v.x(), 0.7));
    let v = run(
        r#"<materialx><invert name="n" type="float">
             <input name="in" type="float" value="0.3" />
             <input name="amount" type="float" value="2" />
           </invert></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 1.7));
    let v = run(&unary("invert", "color3", "0.25, 0.5, 1"), "n");
    assert!(approx(v.v[0], 0.75) && approx(v.v[1], 0.5) && approx(v.v[2], 0.0));
}

#[test]
fn smoothstep_is_hermite_between_its_edges() {
    let doc = |x: &str| {
        format!(
            r#"<materialx><smoothstep name="n" type="float">
                 <input name="in" type="float" value="{x}" />
                 <input name="low" type="float" value="0" />
                 <input name="high" type="float" value="1" />
               </smoothstep></materialx>"#
        )
    };
    assert!(approx(run(&doc("-1"), "n").x(), 0.0));
    assert!(approx(run(&doc("0.5"), "n").x(), 0.5));
    assert!(approx(run(&doc("2"), "n").x(), 1.0));
    // t = 0.25: 3t² − 2t³ = 0.15625.
    assert!(approx(run(&doc("0.25"), "n").x(), 0.15625));
}

#[test]
fn mix_defaults_to_its_background() {
    let v = run(
        r#"<materialx><mix name="n" type="float">
             <input name="bg" type="float" value="2" />
             <input name="fg" type="float" value="8" />
           </mix></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 2.0));
    let v = run(
        r#"<materialx><mix name="n" type="color3">
             <input name="bg" type="color3" value="0, 0, 0" />
             <input name="fg" type="color3" value="1, 1, 1" />
             <input name="mix" type="float" value="0.75" />
           </mix></materialx>"#,
        "n",
    );
    assert!(approx(v.v[0], 0.75) && approx(v.v[2], 0.75));
}

#[test]
fn convert_changes_arity_and_broadcasts_scalars() {
    let v = run(&unary("convert", "vector2", "0.4"), "n");
    // A float on a vector2 input is already a broadcast.
    assert_eq!(v.rgb().x, 0.4);
    let v = run(
        r#"<materialx><convert name="n" type="vector2">
             <input name="in" type="float" nodename="c" />
           </convert>
           <constant name="c" type="float"><input name="value" type="float" value="0.4" /></constant>
           </materialx>"#,
        "n",
    );
    assert_eq!(v.arity, 2);
    assert!(approx(v.v[0], 0.4) && approx(v.v[1], 0.4));
    // color3 → float takes the first lane.
    let v = run(
        r#"<materialx><convert name="n" type="float">
             <input name="in" type="color3" nodename="c" />
           </convert>
           <constant name="c" type="color3"><input name="value" type="color3" value="0.2, 0.5, 0.9" /></constant>
           </materialx>"#,
        "n",
    );
    assert_eq!(v.arity, 1);
    assert!(approx(v.x(), 0.2));
}

#[test]
fn extract_selects_one_lane() {
    let v = run(
        r#"<materialx><extract name="n" type="float">
             <input name="in" type="color3" value="0.2, 0.5, 0.9" />
             <input name="index" type="integer" value="2" />
           </extract></materialx>"#,
        "n",
    );
    assert_eq!(v.arity, 1);
    assert!(approx(v.x(), 0.9));
    let v = run(
        r#"<materialx><extract name="n" type="float">
             <input name="in" type="color3" value="0.2, 0.5, 0.9" />
           </extract></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 0.2), "index defaults to 0");
}

#[test]
fn combine_nodes_assemble_lanes() {
    let v = run(
        r#"<materialx><combine3 name="n" type="color3">
             <input name="in1" type="float" value="0.1" />
             <input name="in2" type="float" value="0.2" />
             <input name="in3" type="float" value="0.3" />
           </combine3></materialx>"#,
        "n",
    );
    assert_eq!(v.arity, 3);
    assert!(approx(v.v[0], 0.1) && approx(v.v[1], 0.2) && approx(v.v[2], 0.3));
    let v = run(
        r#"<materialx><combine2 name="n" type="vector2">
             <input name="in1" type="float" value="0.7" />
             <input name="in2" type="float" value="0.9" />
           </combine2></materialx>"#,
        "n",
    );
    assert_eq!(v.arity, 2);
    assert!(approx(v.v[0], 0.7) && approx(v.v[1], 0.9));
}

#[test]
fn dotproduct_and_luminance() {
    let v = run(&binary("dotproduct", "vector3", "1, 2, 3", "4, 5, 6"), "n");
    assert!(approx(v.x(), 32.0));
    let v = run(&unary("luminance", "color3", "1, 1, 1"), "n");
    assert!(
        (v.x() - 1.0).abs() < 1e-4,
        "white has unit luminance: {}",
        v.x()
    );
    let v = run(&unary("luminance", "color3", "0, 1, 0"), "n");
    assert!(
        v.x() > 0.6 && v.x() < 0.8,
        "green carries most of the luminance: {}",
        v.x()
    );
}

#[test]
fn geometric_inputs_read_the_shading_context() {
    let c = ctx();
    let uv = run_with(
        r#"<materialx><texcoord name="n" type="vector2" /></materialx>"#,
        "n",
        &c,
        &decline,
    );
    assert_eq!(uv.arity, 2);
    assert!(approx(uv.v[0], 0.25) && approx(uv.v[1], 0.75));
    let n = run_with(
        r#"<materialx><normal name="n" type="vector3" /></materialx>"#,
        "n",
        &c,
        &decline,
    );
    assert_eq!(n.rgb(), Vec3A::Z);
    let p = run_with(
        r#"<materialx><position name="n" type="vector3" /></materialx>"#,
        "n",
        &c,
        &decline,
    );
    assert_eq!(p.rgb(), Vec3A::new(1.0, 2.0, 3.0));
    let v = run_with(
        r#"<materialx><viewdirection name="n" type="vector3" /></materialx>"#,
        "n",
        &c,
        &decline,
    );
    assert_eq!(v.rgb(), -Vec3A::Z);
}

#[test]
fn constant_node_returns_its_value() {
    let v = run(
        r#"<materialx><constant name="n" type="color3"><input name="value" type="color3" value="0.3, 0.6, 0.9" /></constant></materialx>"#,
        "n",
    );
    assert_eq!(v.arity, 3);
    assert!(approx(v.v[1], 0.6));
    let v = run(
        r#"<materialx><constant name="n" type="float" /></materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 0.0));
}

#[test]
fn a_chain_of_nodes_is_evaluated_in_order() {
    // sqrt(max(in, 0) * 4) + 1 at in = 4 → 5
    let v = run(
        r#"<materialx>
             <constant name="c" type="float"><input name="value" type="float" value="4" /></constant>
             <max name="m" type="float">
               <input name="in1" type="float" nodename="c" />
               <input name="in2" type="float" value="0" />
             </max>
             <multiply name="mul" type="float">
               <input name="in1" type="float" nodename="m" />
               <input name="in2" type="float" value="4" />
             </multiply>
             <sqrt name="s" type="float"><input name="in" type="float" nodename="mul" /></sqrt>
             <add name="n" type="float">
               <input name="in1" type="float" nodename="s" />
               <input name="in2" type="float" value="1" />
             </add>
           </materialx>"#,
        "n",
    );
    assert!(approx(v.x(), 5.0));
}

#[test]
fn shared_upstream_nodes_are_compiled_once() {
    let d = Doc::parse(
        r#"<materialx>
             <constant name="c" type="float"><input name="value" type="float" value="2" /></constant>
             <add name="a" type="float">
               <input name="in1" type="float" nodename="c" />
               <input name="in2" type="float" nodename="c" />
             </add>
           </materialx>"#,
    )
    .unwrap();
    let mut c = Compiler::new(&d, &decline);
    let slot = c.compile_named("", "a", None);
    let consts = c
        .program
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Const(_)))
        .count();
    assert_eq!(consts, 1, "the shared constant is memoised");
    let mut slots = Vec::new();
    c.program.eval(&ctx(), &mut slots);
    assert!(approx(slots[slot as usize].x(), 4.0));
}

#[test]
fn unsupported_categories_degrade_to_a_constant_and_are_recorded() {
    let d = Doc::parse(
        r#"<materialx>
             <frobnicate name="f" type="float"><input name="in" type="float" value="3" /></frobnicate>
             <add name="a" type="float">
               <input name="in1" type="float" nodename="f" />
               <input name="in2" type="float" value="1" />
             </add>
           </materialx>"#,
    )
    .unwrap();
    let mut c = Compiler::new(&d, &decline);
    let slot = c.compile_named("", "a", None);
    assert!(c.unsupported.contains("frobnicate"));
    assert_eq!(c.unsupported.len(), 1);
    let mut slots = Vec::new();
    c.program.eval(&ctx(), &mut slots);
    assert!(slots[slot as usize].x().is_finite());
}

#[test]
fn program_eval_reuses_the_caller_buffer() {
    let mut p = Program::default();
    p.ops.push(Op::Const(Val::float(1.0)));
    p.ops.push(Op::Const(Val::float(2.0)));
    p.ops.push(Op::Binary {
        op: BinOp::Add,
        a: 0,
        b: 1,
    });
    let mut slots = vec![Val::float(99.0); 10];
    p.eval(&ctx(), &mut slots);
    assert_eq!(slots.len(), 3);
    assert!(approx(slots[2].x(), 3.0));
    // Evaluating again yields the same result on the same buffer.
    p.eval(&ctx(), &mut slots);
    assert_eq!(slots.len(), 3);
    assert!(approx(slots[2].x(), 3.0));
}

#[test]
fn hand_built_ops_evaluate() {
    let mut p = Program::default();
    let a = p.ops.len() as u32;
    p.ops.push(Op::Const(Val::vec3(0.5, 0.25, 0.125)));
    let s = p.ops.len() as u32;
    p.ops.push(Op::Const(Val::float(2.0)));
    p.ops.push(Op::Binary {
        op: BinOp::Mul,
        a,
        b: s,
    });
    p.ops.push(Op::Unary {
        op: crust_mtlx::UnOp::Sqrt,
        a,
    });
    p.ops.push(Op::Extract { a, index: 1 });
    p.ops.push(Op::Convert { a: s, arity: 3 });
    let mut slots = Vec::new();
    p.eval(&ctx(), &mut slots);
    assert_eq!(slots[2].v[0..3], [1.0, 0.5, 0.25]);
    assert!(approx(slots[3].v[0], 0.5f32.sqrt()));
    assert!(approx(slots[4].x(), 0.25));
    assert_eq!(slots[5].arity, 3);
    assert_eq!(slots[5].rgb(), Vec3A::splat(2.0));
}

// ---------------------------------------------------------------------------
// Textures
// ---------------------------------------------------------------------------

struct Flat([f32; 4]);

impl Texture for Flat {
    fn eval(&self, _u: f32, _v: f32, _width: f32) -> [f32; 4] {
        self.0
    }
}

/// Returns the UV it was asked for, so tiling and offsets are observable.
struct Echo;

impl Texture for Echo {
    fn eval(&self, u: f32, v: f32, _width: f32) -> [f32; 4] {
        [u, v, 0.0, 1.0]
    }
}

#[test]
fn a_declined_image_falls_back_to_its_default() {
    let v = run(
        r#"<materialx><image name="n" type="color3">
             <input name="file" type="filename" value="missing.png" />
             <input name="default" type="color3" value="0.1, 0.2, 0.3" />
           </image></materialx>"#,
        "n",
    );
    assert!(approx(v.v[0], 0.1) && approx(v.v[1], 0.2) && approx(v.v[2], 0.3));
    // Without a default: mid-grey rather than black.
    let v = run(
        r#"<materialx><image name="n" type="color3">
             <input name="file" type="filename" value="missing.png" />
           </image></materialx>"#,
        "n",
    );
    assert!(
        v.rgb().min_element() > 0.0 && v.rgb().max_element() < 1.0,
        "{v:?}"
    );
}

#[test]
fn a_loaded_image_is_sampled_and_counted() {
    let loader = |file: &str, cs: Option<&str>| -> Option<TextureRef> {
        assert_eq!(file, "albedo.png");
        assert_eq!(cs, Some("srgb_texture"));
        Some(TextureRef(Arc::new(Flat([0.6, 0.7, 0.8, 1.0]))))
    };
    let doc = r#"<materialx><image name="n" type="color3">
             <input name="file" type="filename" value="albedo.png" colorspace="srgb_texture" />
           </image></materialx>"#;
    let v = run_with(doc, "n", &ctx(), &loader);
    assert_eq!(v.arity, 3);
    assert!(approx(v.v[0], 0.6) && approx(v.v[1], 0.7) && approx(v.v[2], 0.8));

    let d = Doc::parse(doc).unwrap();
    let mut c = Compiler::new(&d, &loader);
    c.compile_named("", "n", None);
    let textured = c
        .program
        .ops
        .iter()
        .filter(|op| matches!(op, Op::Texture { tex: Some(_), .. }))
        .count();
    assert_eq!(textured, 1);
}

#[test]
fn a_float_image_reads_one_lane() {
    let loader = |_: &str, _: Option<&str>| -> Option<TextureRef> {
        Some(TextureRef(Arc::new(Flat([0.3, 0.9, 0.9, 1.0]))))
    };
    let v = run_with(
        r#"<materialx><image name="n" type="float">
             <input name="file" type="filename" value="mask.png" />
           </image></materialx>"#,
        "n",
        &ctx(),
        &loader,
    );
    assert_eq!(v.arity, 1);
    assert!(approx(v.x(), 0.3));
}

#[test]
fn image_lookups_use_the_context_uv() {
    let loader =
        |_: &str, _: Option<&str>| -> Option<TextureRef> { Some(TextureRef(Arc::new(Echo))) };
    let v = run_with(
        r#"<materialx><image name="n" type="vector2">
             <input name="file" type="filename" value="e.png" />
           </image></materialx>"#,
        "n",
        &ctx(),
        &loader,
    );
    assert!(approx(v.v[0], 0.25) && approx(v.v[1], 0.75), "{v:?}");
}

#[test]
fn tiledimage_scales_and_offsets_the_uv() {
    let loader =
        |_: &str, _: Option<&str>| -> Option<TextureRef> { Some(TextureRef(Arc::new(Echo))) };
    let v = run_with(
        r#"<materialx><tiledimage name="n" type="vector2">
             <input name="file" type="filename" value="e.png" />
             <input name="uvtiling" type="vector2" value="4, 2" />
             <input name="uvoffset" type="vector2" value="0.5, 0.25" />
           </tiledimage></materialx>"#,
        "n",
        &ctx(),
        &loader,
    );
    // The lookup is at uv·tiling + offset (or an equivalent affine map):
    // it must differ from the raw uv, and scaling must be visible.
    assert!(
        !(approx(v.v[0], 0.25) && approx(v.v[1], 0.75)),
        "tiling ignored: {v:?}"
    );
    assert!(v.v[0].is_finite() && v.v[1].is_finite());
}

#[test]
fn a_flat_normal_map_returns_the_geometric_normal() {
    let loader = |_: &str, _: Option<&str>| -> Option<TextureRef> {
        Some(TextureRef(Arc::new(Flat([0.5, 0.5, 1.0, 1.0]))))
    };
    let c = ctx();
    let v = run_with(
        r#"<materialx>
             <image name="t" type="vector3"><input name="file" type="filename" value="n.png" /></image>
             <normalmap name="n" type="vector3"><input name="in" type="vector3" nodename="t" /></normalmap>
           </materialx>"#,
        "n",
        &c,
        &loader,
    );
    let n = v.rgb().normalize();
    assert!(
        n.abs_diff_eq(c.normal, 1e-3),
        "flat map bent the normal: {n}"
    );
}

#[test]
fn a_tilted_normal_map_leans_along_the_tangent() {
    // Red > 0.5 encodes a +tangent tilt.
    let loader = |_: &str, _: Option<&str>| -> Option<TextureRef> {
        Some(TextureRef(Arc::new(Flat([0.9, 0.5, 0.6, 1.0]))))
    };
    let c = ctx();
    let v = run_with(
        r#"<materialx>
             <image name="t" type="vector3"><input name="file" type="filename" value="n.png" /></image>
             <normalmap name="n" type="vector3"><input name="in" type="vector3" nodename="t" /></normalmap>
           </materialx>"#,
        "n",
        &c,
        &loader,
    );
    let n = v.rgb().normalize();
    assert!(
        n.dot(c.tangent) > 0.2,
        "normal did not lean toward the tangent: {n}"
    );
    assert!(
        n.dot(c.normal) > 0.0,
        "normal must stay on the surface's side: {n}"
    );
}

#[test]
fn a_normal_map_without_a_tangent_passes_the_normal_through() {
    let loader = |_: &str, _: Option<&str>| -> Option<TextureRef> {
        Some(TextureRef(Arc::new(Flat([0.9, 0.2, 0.6, 1.0]))))
    };
    let mut c = ctx();
    c.tangent = Vec3A::ZERO;
    let v = run_with(
        r#"<materialx>
             <image name="t" type="vector3"><input name="file" type="filename" value="n.png" /></image>
             <normalmap name="n" type="vector3"><input name="in" type="vector3" nodename="t" /></normalmap>
           </materialx>"#,
        "n",
        &c,
        &loader,
    );
    assert!(v.rgb().normalize().abs_diff_eq(c.normal, 1e-4), "{v:?}");
}

// ---------------------------------------------------------------------------
// Metals
// ---------------------------------------------------------------------------

#[test]
fn reflectivity_from_ior_matches_normal_incidence_fresnel() {
    // A dielectric-like (k = 0) n = 1.5 reflects ((n−1)/(n+1))² = 0.04.
    let r = reflectivity_from_ior(Vec3A::splat(1.5), Vec3A::ZERO);
    assert!((r.x - 0.04).abs() < 1e-5);
    // Vacuum reflects nothing.
    assert_eq!(reflectivity_from_ior(Vec3A::ONE, Vec3A::ZERO), Vec3A::ZERO);
    // Very absorbing metals approach one and never exceed it.
    let r = reflectivity_from_ior(Vec3A::splat(0.2), Vec3A::splat(50.0));
    assert!(r.x > 0.99 && r.x <= 1.0);
}

#[test]
fn artistic_ior_outputs_differ_and_invert() {
    let doc = r#"<materialx>
         <artistic_ior name="ai" type="multioutput">
           <input name="reflectivity" type="color3" value="0.9, 0.6, 0.3" />
           <input name="edge_color" type="color3" value="1, 1, 1" />
         </artistic_ior>
         <convert name="n" type="color3"><input name="in" type="color3" nodename="ai" output="ior" /></convert>
         <convert name="k" type="color3"><input name="in" type="color3" nodename="ai" output="extinction" /></convert>
       </materialx>"#;
    let n = run(doc, "n").rgb();
    let k = run(doc, "k").rgb();
    assert_ne!(n, k);
    let back = reflectivity_from_ior(n, k);
    assert!(back.abs_diff_eq(Vec3A::new(0.9, 0.6, 0.3), 1e-3), "{back}");
}

// ---------------------------------------------------------------------------
// BSDF flattening
// ---------------------------------------------------------------------------

const DIFFUSE: &str = r#"<oren_nayar_diffuse_bsdf name="d" type="BSDF">
                           <input name="color" type="color3" value="0.5, 0.4, 0.3" />
                           <input name="roughness" type="float" value="0.2" />
                         </oren_nayar_diffuse_bsdf>"#;
const METAL: &str = r#"<conductor_bsdf name="c" type="BSDF">
                         <input name="roughness" type="vector2" value="0.1, 0.1" />
                       </conductor_bsdf>"#;

#[test]
fn a_bare_diffuse_lobe_carries_its_parameters() {
    let l = lobes_of(&format!("<materialx>{DIFFUSE}</materialx>"), "d");
    assert_eq!(l.len(), 1);
    let (kind, w, color, rough) = l[0];
    assert_eq!(kind, LobeKind::Diffuse);
    assert!(approx(w, 1.0));
    assert!(color.abs_diff_eq(Vec3A::new(0.5, 0.4, 0.3), 1e-5));
    assert!(approx(rough, 0.2));
}

#[test]
fn every_bsdf_leaf_category_maps_to_a_pool() {
    for (cat, kind) in [
        ("oren_nayar_diffuse_bsdf", LobeKind::Diffuse),
        ("diffuse_bsdf", LobeKind::Diffuse),
        ("burley_diffuse_bsdf", LobeKind::Diffuse),
        ("dielectric_bsdf", LobeKind::Dielectric),
        ("generalized_schlick_bsdf", LobeKind::Dielectric),
        ("conductor_bsdf", LobeKind::Conductor),
        ("sheen_bsdf", LobeKind::Sheen),
        ("subsurface_bsdf", LobeKind::Subsurface),
        ("translucent_bsdf", LobeKind::Subsurface),
    ] {
        let l = lobes_of(
            &format!(r#"<materialx><{cat} name="x" type="BSDF" /></materialx>"#),
            "x",
        );
        assert_eq!(l.len(), 1, "{cat}");
        assert_eq!(l[0].0, kind, "{cat}");
    }
}

#[test]
fn layer_keeps_full_weight_on_both_branches() {
    let l = lobes_of(
        &format!(
            r#"<materialx>{DIFFUSE}
                 <dielectric_bsdf name="g" type="BSDF"><input name="ior" type="float" value="1.5" /></dielectric_bsdf>
                 <layer name="L" type="BSDF">
                   <input name="base" type="BSDF" nodename="d" />
                   <input name="top" type="BSDF" nodename="g" />
                 </layer>
               </materialx>"#
        ),
        "L",
    );
    assert_eq!(l.len(), 2);
    for (kind, w, _, _) in &l {
        assert!(approx(*w, 1.0), "{kind:?} weight {w}");
    }
    assert!(l.iter().any(|l| l.0 == LobeKind::Diffuse));
    assert!(l.iter().any(|l| l.0 == LobeKind::Dielectric));
}

#[test]
fn nested_mixes_multiply_their_masks() {
    let l = lobes_of(
        &format!(
            r#"<materialx>{DIFFUSE}{METAL}
                 <sheen_bsdf name="s" type="BSDF" />
                 <mix name="inner" type="BSDF">
                   <input name="bg" type="BSDF" nodename="d" />
                   <input name="fg" type="BSDF" nodename="c" />
                   <input name="mix" type="float" value="0.5" />
                 </mix>
                 <mix name="outer" type="BSDF">
                   <input name="bg" type="BSDF" nodename="inner" />
                   <input name="fg" type="BSDF" nodename="s" />
                   <input name="mix" type="float" value="0.2" />
                 </mix>
               </materialx>"#
        ),
        "outer",
    );
    let w = |k: LobeKind| l.iter().filter(|l| l.0 == k).map(|l| l.1).sum::<f32>();
    assert!(approx(w(LobeKind::Sheen), 0.2));
    assert!(approx(w(LobeKind::Diffuse), 0.4));
    assert!(approx(w(LobeKind::Conductor), 0.4));
    let total: f32 = l.iter().map(|l| l.1).sum();
    assert!(approx(total, 1.0));
}

#[test]
fn a_mix_driven_by_a_pattern_evaluates_the_pattern() {
    // mix = clamp(0.3 * 2) = 0.6
    let l = lobes_of(
        &format!(
            r#"<materialx>{DIFFUSE}{METAL}
                 <multiply name="m" type="float">
                   <input name="in1" type="float" value="0.3" />
                   <input name="in2" type="float" value="2" />
                 </multiply>
                 <clamp name="cl" type="float"><input name="in" type="float" nodename="m" /></clamp>
                 <mix name="x" type="BSDF">
                   <input name="bg" type="BSDF" nodename="d" />
                   <input name="fg" type="BSDF" nodename="c" />
                   <input name="mix" type="float" nodename="cl" />
                 </mix>
               </materialx>"#
        ),
        "x",
    );
    let metal = l.iter().find(|l| l.0 == LobeKind::Conductor).unwrap().1;
    let diffuse = l.iter().find(|l| l.0 == LobeKind::Diffuse).unwrap().1;
    assert!(approx(metal, 0.6));
    assert!(approx(diffuse, 0.4));
}

#[test]
fn a_leaf_weight_input_scales_its_lobe() {
    let l = lobes_of(
        r#"<materialx>
             <dielectric_bsdf name="t" type="BSDF">
               <input name="weight" type="float" value="0.25" />
             </dielectric_bsdf>
           </materialx>"#,
        "t",
    );
    assert_eq!(l.len(), 1);
    assert!(approx(l[0].1, 0.25), "own weight not applied: {}", l[0].1);
}

#[test]
fn a_literal_zero_weight_leaf_is_pruned_at_compile_time() {
    // The transmission dummy both DPEL assets use as a mix's null branch. It
    // contributes nothing to any pool, so dropping it changes no parameter —
    // but left in place it would count as a specular interface and promote a
    // glaze layered above it to a coat over nothing.
    let l = lobes_of(
        r#"<materialx>
             <dielectric_bsdf name="t" type="BSDF">
               <input name="weight" type="float" value="0" />
             </dielectric_bsdf>
           </materialx>"#,
        "t",
    );
    assert!(
        l.is_empty(),
        "a literal weight-0 dummy reached the pools: {l:?}"
    );
}

#[test]
fn a_connected_zero_weight_leaf_still_reaches_the_pool() {
    // Pruning is for literals only: a connected weight is a runtime value,
    // even when the node it comes from is a constant zero.
    let l = lobes_of(
        r#"<materialx>
             <constant name="k" type="float">
               <input name="value" type="float" value="0" />
             </constant>
             <dielectric_bsdf name="t" type="BSDF">
               <input name="weight" type="float" nodename="k" />
             </dielectric_bsdf>
           </materialx>"#,
        "t",
    );
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(approx(l[0].1, 0.0), "weight {}", l[0].1);
}

#[test]
fn a_literal_zero_multiply_is_pruned_at_compile_time() {
    // `multiply(BSDF, 0)` is the other way to author a null branch, and it has
    // to prune for the same reason a literal `weight = 0` leaf does: the lobe
    // under it can never contribute, but it would still count as a specular
    // interface and promote a dielectric layered above it to a coat.
    let l = lobes_of(
        r#"<materialx>
             <dielectric_bsdf name="t" type="BSDF" />
             <multiply name="m" type="BSDF">
               <input name="in1" type="BSDF" nodename="t" />
               <input name="in2" type="float" value="0" />
             </multiply>
           </materialx>"#,
        "m",
    );
    assert!(l.is_empty(), "a literal x0 branch reached the pools: {l:?}");
}

#[test]
fn a_zero_multiplied_dielectric_does_not_promote_the_glaze_above_it() {
    // The shape this regression is about: a diffuse base with a
    // zero-multiplied dielectric over it, all under a clear glaze. The glaze
    // must stay the *base specular* — the only specular interface the surface
    // actually has — rather than becoming a coat over a base that carries none.
    let l = lobes_of(
        r#"<materialx>
             <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
             <dielectric_bsdf name="dummy" type="BSDF" />
             <multiply name="off" type="BSDF">
               <input name="in1" type="BSDF" nodename="dummy" />
               <input name="in2" type="float" value="0" />
             </multiply>
             <layer name="inner" type="BSDF">
               <input name="top" type="BSDF" nodename="off" />
               <input name="base" type="BSDF" nodename="d" />
             </layer>
             <dielectric_bsdf name="clear" type="BSDF">
               <input name="roughness" type="vector2" value="0.02, 0.02" />
             </dielectric_bsdf>
             <layer name="L" type="BSDF">
               <input name="top" type="BSDF" nodename="clear" />
               <input name="base" type="BSDF" nodename="inner" />
             </layer>
           </materialx>"#,
        "L",
    );
    let kinds: Vec<LobeKind> = l.iter().map(|x| x.0).collect();
    assert_eq!(
        kinds,
        vec![LobeKind::Diffuse, LobeKind::Dielectric],
        "the zero-multiplied dummy promoted the glaze: {l:?}"
    );
}

#[test]
fn a_connected_zero_multiply_still_reaches_the_pool() {
    // Literals only, same as the leaf weight: a connected scalar is a runtime
    // value, and pruning on it would make the lobe set — and with it the
    // coat-vs-base-specular decision — depend on the shading point.
    let l = lobes_of(
        r#"<materialx>
             <constant name="k" type="float">
               <input name="value" type="float" value="0" />
             </constant>
             <dielectric_bsdf name="t" type="BSDF" />
             <multiply name="m" type="BSDF">
               <input name="in1" type="BSDF" nodename="t" />
               <input name="in2" type="float" nodename="k" />
             </multiply>
           </materialx>"#,
        "m",
    );
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(approx(l[0].1, 0.0), "weight {}", l[0].1);
}

#[test]
fn a_partly_zero_colour_multiply_is_not_pruned() {
    // A `color3` scalar is pruned only when every lane is zero. Testing lane 0
    // alone would drop a branch that still transmits green and blue.
    let l = lobes_of(
        r#"<materialx>
             <dielectric_bsdf name="t" type="BSDF" />
             <multiply name="m" type="BSDF">
               <input name="in1" type="BSDF" nodename="t" />
               <input name="in2" type="color3" value="0, 0.4, 0.4" />
             </multiply>
           </materialx>"#,
        "m",
    );
    assert_eq!(l.len(), 1, "a partly-zero colour pruned the branch: {l:?}");
}

#[test]
fn add_sums_both_branches_at_full_weight() {
    let l = lobes_of(
        &format!(
            r#"<materialx>{DIFFUSE}{METAL}
                 <add name="a" type="BSDF">
                   <input name="in1" type="BSDF" nodename="d" />
                   <input name="in2" type="BSDF" nodename="c" />
                 </add>
               </materialx>"#
        ),
        "a",
    );
    assert_eq!(l.len(), 2);
    assert!(l.iter().all(|l| approx(l.1, 1.0)));
}

#[test]
fn surfacematerial_surface_and_bsdf_chain_reaches_the_leaves() {
    let l = lobes_of(
        &format!(
            r#"<materialx>{DIFFUSE}
                 <surface name="s" type="surfaceshader"><input name="bsdf" type="BSDF" nodename="d" /></surface>
                 <surfacematerial name="m" type="material"><input name="surfaceshader" type="surfaceshader" nodename="s" /></surfacematerial>
               </materialx>"#
        ),
        "m",
    );
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].0, LobeKind::Diffuse);
}

#[test]
fn a_bsdf_cycle_terminates() {
    let l = lobes_of(
        r#"<materialx>
             <layer name="a" type="BSDF">
               <input name="base" type="BSDF" nodename="b" />
             </layer>
             <layer name="b" type="BSDF">
               <input name="base" type="BSDF" nodename="a" />
             </layer>
           </materialx>"#,
        "a",
    );
    assert!(l.is_empty(), "no leaves in a pure cycle: {}", l.len());
}

#[test]
fn a_lobe_authoring_a_normal_records_it() {
    let d = Doc::parse(
        r#"<materialx>
             <normal name="gn" type="vector3" />
             <oren_nayar_diffuse_bsdf name="d" type="BSDF">
               <input name="normal" type="vector3" nodename="gn" />
             </oren_nayar_diffuse_bsdf>
             <oren_nayar_diffuse_bsdf name="e" type="BSDF" />
           </materialx>"#,
    )
    .unwrap();
    let mut c = Compiler::new(&d, &decline);
    let one = c.constant(Val::ONE);
    let mut flat = Flattened::default();
    flatten(&mut c, &d.find("", "d").unwrap().clone(), one, 0, &mut flat);
    assert!(flat.lobes[0].normal.is_some());
    let mut flat2 = Flattened::default();
    flatten(
        &mut c,
        &d.find("", "e").unwrap().clone(),
        one,
        0,
        &mut flat2,
    );
    assert!(flat2.lobes[0].normal.is_none());
}

// ---------------------------------------------------------------------------
// The checked-in sample document
// ---------------------------------------------------------------------------

#[test]
fn sample_document_parses_with_its_bare_udim_tokens() {
    let d = Doc::open(&sample_mtlx()).expect("samples/materialx_basic.mtlx parses");
    assert_eq!(d.by_category("surfacematerial").count(), 3);
    let f = d.find("", "base_color_tex").unwrap().input("file").unwrap();
    assert_eq!(f.text.as_deref(), Some("textures/mtlx_base.<UDIM>.png"));
    assert_eq!(f.colorspace.as_deref(), Some("srgb_texture"));
}

#[test]
fn sample_ceramic_compiles_to_a_layered_diffuse_and_dielectric() {
    let c = compile(&sample_mtlx(), Some("mtlx_ceramic"), &decline).expect("compiles");
    assert_eq!(c.root_name, "mtlx_ceramic");
    assert!(c.unsupported.is_empty(), "{:?}", c.unsupported);
    assert_eq!(c.textures, 0, "every texture was declined");
    let kinds: Vec<LobeKind> = c.lobes.iter().map(|l| l.kind).collect();
    assert!(kinds.contains(&LobeKind::Diffuse));
    assert!(kinds.contains(&LobeKind::Dielectric));
    assert_eq!(kinds.len(), 2);
    assert!(!c.program.ops.is_empty());
}

#[test]
fn sample_lacquer_compiles_to_diffuse_dielectric_and_coat() {
    // Two specular lobes: the clear varnish sits over a base that already
    // carries the satin dielectric, so it is the coat — in tree order, base
    // before top, innermost first.
    let c = compile(&sample_mtlx(), Some("mtlx_lacquer"), &decline).expect("compiles");
    assert_eq!(c.root_name, "mtlx_lacquer");
    assert!(c.unsupported.is_empty(), "{:?}", c.unsupported);
    assert_eq!(c.textures, 0, "the lacquer is texture-free by design");
    let kinds: Vec<LobeKind> = c.lobes.iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        vec![LobeKind::Diffuse, LobeKind::Dielectric, LobeKind::Coat]
    );
}

#[test]
fn sample_metal_compiles_with_mask_driven_weights() {
    let c = compile(&sample_mtlx(), Some("mtlx_metal"), &decline).expect("compiles");
    assert_eq!(c.root_name, "mtlx_metal");
    assert!(c.unsupported.is_empty());
    let mut slots = Vec::new();
    c.program.eval(&ctx(), &mut slots);
    let w = |k: LobeKind| {
        c.lobes
            .iter()
            .filter(|l| l.kind == k)
            .map(|l| slots[l.weight as usize].x())
            .sum::<f32>()
    };
    let total = w(LobeKind::Conductor) + w(LobeKind::Diffuse);
    assert!(
        (total - 1.0).abs() < 1e-4,
        "a mix partitions its weight: {total}"
    );
    // The conductor's colour reduces from artistic_ior back to the authored
    // reflectivity.
    let metal = c
        .lobes
        .iter()
        .find(|l| l.kind == LobeKind::Conductor)
        .unwrap();
    let n = slots[metal.ior as usize].rgb();
    let k = slots[metal.extinction as usize].rgb();
    let r = reflectivity_from_ior(n, k);
    assert!(r.abs_diff_eq(Vec3A::new(0.94, 0.72, 0.36), 1e-2), "{r}");
}

#[test]
fn sample_first_material_is_used_when_none_is_named() {
    let c = compile(&sample_mtlx(), None, &decline).expect("compiles");
    assert_eq!(
        c.root_name, "mtlx_ceramic",
        "document order picks the first surfacematerial"
    );
}

#[test]
fn sample_textures_are_requested_with_their_colorspace() {
    let asked = std::sync::Mutex::new(Vec::new());
    let loader = |file: &str, cs: Option<&str>| -> Option<TextureRef> {
        asked
            .lock()
            .unwrap()
            .push((file.to_string(), cs.map(str::to_string)));
        Some(TextureRef(Arc::new(Flat([0.5, 0.5, 1.0, 1.0]))))
    };
    let c = compile(&sample_mtlx(), Some("mtlx_ceramic"), &loader).expect("compiles");
    assert_eq!(c.textures, 2, "albedo and normal map");
    let asked = asked.lock().unwrap();
    assert!(
        asked
            .iter()
            .any(|(f, cs)| f.contains("mtlx_base.<UDIM>.png")
                && cs.as_deref() == Some("srgb_texture"))
    );
    assert!(
        asked
            .iter()
            .any(|(f, cs)| f.contains("mtlx_normal.<UDIM>.png") && cs.is_none())
    );
}

// ---------------------------------------------------------------------------
// EDF: emission
// ---------------------------------------------------------------------------

/// A `<surface>` carries a `bsdf` *and* an `edf`, and the arm that reads it
/// used to follow only the first. The emission was not merely unsupported —
/// it was invisible, with nothing in `unsupported` to say so, because that set
/// is filled when an unmatched *category* is reached and the input was never
/// followed to its node. This is the regression that pins the fix.
#[test]
fn a_uniform_edf_under_a_surface_reaches_the_emission_list() {
    let doc = r#"<materialx>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF">
        <input name="color" type="color3" value="0.1, 0.1, 0.1" />
      </oren_nayar_diffuse_bsdf>
      <uniform_edf name="e" type="EDF">
        <input name="color" type="color3" value="2, 3, 4" />
      </uniform_edf>
      <surface name="s" type="surfaceshader">
        <input name="bsdf" type="BSDF" nodename="d" />
        <input name="edf" type="EDF" nodename="e" />
      </surface>
    </materialx>"#;
    let (terms, kinds) = emission_of(doc, "s");
    assert_eq!(kinds, vec![LobeKind::Diffuse], "the BSDF side is untouched");
    assert_eq!(terms.len(), 1);
    assert_eq!(terms[0].0.x, 1.0);
    assert_eq!(terms[0].1, Vec3A::new(2.0, 3.0, 4.0));
}

/// The domain flag is not plumbing. `closure_input` gates a branch on its
/// declared type, and an EDF-typed `mix` declares `type="EDF"` on `fg`/`bg` —
/// so walking the emission tree while still asking "is this a BSDF?" resolves
/// both branches to `None` and the emission silently vanishes.
#[test]
fn an_edf_typed_mix_is_not_mistaken_for_a_non_closure() {
    let doc = r#"<materialx>
      <uniform_edf name="a" type="EDF">
        <input name="color" type="color3" value="1, 0, 0" />
      </uniform_edf>
      <uniform_edf name="b" type="EDF">
        <input name="color" type="color3" value="0, 1, 0" />
      </uniform_edf>
      <mix name="m" type="EDF">
        <input name="fg" type="EDF" nodename="a" />
        <input name="bg" type="EDF" nodename="b" />
        <input name="mix" type="float" value="0.25" />
      </mix>
      <surface name="s" type="surfaceshader">
        <input name="edf" type="EDF" nodename="m" />
      </surface>
    </materialx>"#;
    let (terms, _) = emission_of(doc, "s");
    assert_eq!(terms.len(), 2, "both mix branches must survive");
    // `bg` is flattened first, as on the BSDF side.
    assert!(
        (terms[0].0.x - 0.75).abs() < 1e-6,
        "bg weight {}",
        terms[0].0.x
    );
    assert!(
        (terms[1].0.x - 0.25).abs() < 1e-6,
        "fg weight {}",
        terms[1].0.x
    );
}

/// `multiply(uniform_edf, 8)` is how MaterialX authors a bright emitter, and
/// the weight it produces must not be bounded by 1. This is the HDR claim
/// stated at the crate seam, upstream of any texture.
#[test]
fn a_multiply_scales_an_edf_above_one() {
    let doc = r#"<materialx>
      <uniform_edf name="e" type="EDF">
        <input name="color" type="color3" value="1, 1, 1" />
      </uniform_edf>
      <multiply name="m" type="EDF">
        <input name="in1" type="EDF" nodename="e" />
        <input name="in2" type="float" value="8" />
      </multiply>
      <surface name="s" type="surfaceshader">
        <input name="edf" type="EDF" nodename="m" />
      </surface>
    </materialx>"#;
    let (terms, _) = emission_of(doc, "s");
    assert_eq!(terms.len(), 1);
    assert!(
        terms[0].0.x > 1.0,
        "weight {} must not be clamped",
        terms[0].0.x
    );
    assert!((terms[0].0.x - 8.0).abs() < 1e-6);
}

/// `add` over EDFs is two emitters, each at full weight — the reduction sums
/// them rather than sharing one surface between them.
#[test]
fn an_edf_add_keeps_both_emitters_at_full_weight() {
    let doc = r#"<materialx>
      <uniform_edf name="a" type="EDF">
        <input name="color" type="color3" value="1, 0, 0" />
      </uniform_edf>
      <uniform_edf name="b" type="EDF">
        <input name="color" type="color3" value="0, 0, 1" />
      </uniform_edf>
      <add name="s2" type="EDF">
        <input name="in1" type="EDF" nodename="a" />
        <input name="in2" type="EDF" nodename="b" />
      </add>
      <surface name="s" type="surfaceshader">
        <input name="edf" type="EDF" nodename="s2" />
      </surface>
    </materialx>"#;
    let (terms, _) = emission_of(doc, "s");
    assert_eq!(terms.len(), 2);
    assert_eq!(terms[0].0.x, 1.0);
    assert_eq!(terms[1].0.x, 1.0);
}

/// The same rule the BSDF side applies: an omitted `type` attribute parses as
/// `float`, so the *target node's* declaration has to count too.
#[test]
fn an_untyped_edge_to_an_edf_node_is_still_an_edf() {
    let doc = r#"<materialx>
      <uniform_edf name="a" type="EDF">
        <input name="color" type="color3" value="5, 5, 5" />
      </uniform_edf>
      <uniform_edf name="b" type="EDF">
        <input name="color" type="color3" value="1, 1, 1" />
      </uniform_edf>
      <mix name="m" type="EDF">
        <input name="fg" nodename="a" />
        <input name="bg" nodename="b" />
        <input name="mix" type="float" value="0.5" />
      </mix>
      <surface name="s" type="surfaceshader">
        <input name="edf" nodename="m" />
      </surface>
    </materialx>"#;
    let (terms, _) = emission_of(doc, "s");
    assert_eq!(terms.len(), 2);
}

/// Every EDF but `uniform_edf` is a *directional* distribution, and the
/// consumer's emitter is uniform — so a cone, an IES profile or a Schlick
/// falloff has nowhere to go. Pooling one onto a uniform emitter would be a
/// plausible glow at the wrong intensity, so they are refused; what matters is
/// that the refusal is *reported* rather than silent, which is precisely what
/// the unread `edf` input was not.
#[test]
fn a_directional_edf_is_reported_rather_than_dropped() {
    for category in ["conical_edf", "measured_edf", "generalized_schlick_edf"] {
        let doc = format!(
            r#"<materialx>
              <{category} name="e" type="EDF">
                <input name="color" type="color3" value="1, 1, 1" />
              </{category}>
              <surface name="s" type="surfaceshader">
                <input name="edf" type="EDF" nodename="e" />
              </surface>
            </materialx>"#
        );
        let (terms, _) = emission_of(&doc, "s");
        assert!(terms.is_empty(), "{category} must not emit");
        assert!(
            unsupported_of(&doc, "s").contains(&category.to_string()),
            "{category} must be reported"
        );
    }
}

/// A literal black EDF is a dummy branch exactly as a literal `weight = 0`
/// dielectric is, and dropping it keeps the emission list *empty* — which is
/// what the consumer's "do not evaluate the graph" fast path keys on.
#[test]
fn a_literal_black_edf_is_pruned() {
    let doc = r#"<materialx>
      <uniform_edf name="e" type="EDF">
        <input name="color" type="color3" value="0, 0, 0" />
      </uniform_edf>
      <surface name="s" type="surfaceshader">
        <input name="edf" type="EDF" nodename="e" />
      </surface>
    </materialx>"#;
    let (terms, _) = emission_of(doc, "s");
    assert!(terms.is_empty());
}

/// The coat promotion asks which lobes the `layer`'s base produced. Emission
/// lives in a list of its own, so that scan is structurally unable to see an
/// emitter — this is the test that would catch a future refactor merging the
/// two into one vector of kinds.
#[test]
fn an_edf_does_not_disturb_the_coat_promotion() {
    let doc = r#"<materialx>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
      <dielectric_bsdf name="satin" type="BSDF">
        <input name="roughness" type="float" value="0.4" />
      </dielectric_bsdf>
      <dielectric_bsdf name="clear" type="BSDF">
        <input name="roughness" type="float" value="0.01" />
      </dielectric_bsdf>
      <layer name="l1" type="BSDF">
        <input name="top" type="BSDF" nodename="satin" />
        <input name="base" type="BSDF" nodename="d" />
      </layer>
      <layer name="l2" type="BSDF">
        <input name="top" type="BSDF" nodename="clear" />
        <input name="base" type="BSDF" nodename="l1" />
      </layer>
      <uniform_edf name="e" type="EDF">
        <input name="color" type="color3" value="3, 3, 3" />
      </uniform_edf>
      <surface name="s" type="surfaceshader">
        <input name="bsdf" type="BSDF" nodename="l2" />
        <input name="edf" type="EDF" nodename="e" />
      </surface>
    </materialx>"#;
    let (terms, kinds) = emission_of(doc, "s");
    assert_eq!(
        kinds,
        vec![LobeKind::Diffuse, LobeKind::Dielectric, LobeKind::Coat],
        "the glaze over a base specular is still the coat"
    );
    assert_eq!(terms.len(), 1);
}

/// The negative control, and the one the fast path depends on: a document
/// that authors no `edf` must produce no emission terms at all.
#[test]
fn a_surface_with_no_edf_has_no_emission() {
    let doc = r#"<materialx>
      <oren_nayar_diffuse_bsdf name="d" type="BSDF" />
      <surface name="s" type="surfaceshader">
        <input name="bsdf" type="BSDF" nodename="d" />
      </surface>
    </materialx>"#;
    let (terms, _) = emission_of(doc, "s");
    assert!(terms.is_empty());
}

/// MaterialX declares `ND_multiply_edfC` — `multiply` on an EDF by a `color3`
/// — so tinting an emitter is ordinary authoring, and the tint must reach the
/// weight slot **per channel**. `Mul` promotes arity through `Val::zip`, so the
/// producer already does this; the test exists because a consumer reading lane
/// 0 alone would see `0.0` here and drop the emitter entirely.
#[test]
fn a_colour_multiply_on_an_edf_reaches_the_weight_per_channel() {
    let doc = r#"<materialx>
      <uniform_edf name="e" type="EDF">
        <input name="color" type="color3" value="1, 1, 1" />
      </uniform_edf>
      <multiply name="m" type="EDF">
        <input name="in1" type="EDF" nodename="e" />
        <input name="in2" type="color3" value="0, 0.6, 0.9" />
      </multiply>
      <surface name="s" type="surfaceshader">
        <input name="edf" type="EDF" nodename="m" />
      </surface>
    </materialx>"#;
    let (terms, _) = emission_of(doc, "s");
    assert_eq!(
        terms.len(),
        1,
        "a partly-zero colour is not a pruned branch"
    );
    assert!(
        (terms[0].0 - Vec3A::new(0.0, 0.6, 0.9)).length() < 1e-6,
        "the tint reached the weight as {:?}",
        terms[0].0
    );
}

/// The `float` case must keep costing nothing: `Val::rgb` broadcasts an arity-1
/// value, so a scalar weight still scales all three channels. This is what
/// would break if that broadcast were ever removed from `Val::rgb`.
#[test]
fn a_float_edf_weight_still_reads_as_three_equal_channels() {
    let doc = r#"<materialx>
      <uniform_edf name="e" type="EDF">
        <input name="color" type="color3" value="1, 1, 1" />
      </uniform_edf>
      <multiply name="m" type="EDF">
        <input name="in1" type="EDF" nodename="e" />
        <input name="in2" type="float" value="4" />
      </multiply>
      <surface name="s" type="surfaceshader">
        <input name="edf" type="EDF" nodename="m" />
      </surface>
    </materialx>"#;
    let (terms, _) = emission_of(doc, "s");
    assert_eq!(terms.len(), 1);
    assert!(
        (terms[0].0 - Vec3A::splat(4.0)).length() < 1e-6,
        "{:?}",
        terms[0].0
    );
}
