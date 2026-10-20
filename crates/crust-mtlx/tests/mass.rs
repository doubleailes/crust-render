//! The curve mass field's nodes in the reader: the four lookups read the
//! host's [`MassCtx`] and fall back to their `default` without one, the
//! program reports which it reads, and `crust_mass_diffuse_bsdf` flattens to
//! a leaf of its own.

use crust_mtlx::{
    Bsdf, Closure, Closures, Compiler, Doc, Host, MassCtx, MassProp, MassReads, Op, ShadeCtx,
    TextureRef, Val, flatten,
};
use glam::Vec3A;

fn decline(_: &str, _: Option<&str>) -> Option<TextureRef> {
    None
}

fn ctx(mass: Option<&MassCtx>) -> ShadeCtx<'_> {
    ShadeCtx {
        uv: (0.25, 0.75),
        normal: Vec3A::Z,
        tangent: Vec3A::X,
        view: -Vec3A::Z,
        position: Vec3A::new(1.0, 2.0, 3.0),
        uv_width: 0.0,
        curve: None,
        mass,
    }
}

const LOOKUPS: &str = r#"<materialx version="1.39">
  <crust_mass_color name="color" type="color3">
    <input name="default" type="color3" value="0.1, 0.2, 0.3" />
  </crust_mass_color>
  <crust_mass_normal name="normal" type="vector3">
    <input name="default" type="vector3" value="0, 0, 1" />
  </crust_mass_normal>
  <crust_mass_depth name="depth" type="float">
    <input name="default" type="float" value="-1" />
  </crust_mass_depth>
  <crust_mass_density name="density" type="float" />
  <crust_mass_diffuse_bsdf name="mass" type="BSDF">
    <input name="color" type="color3" nodename="color" />
    <input name="normal" type="vector3" nodename="normal" />
    <input name="weight" type="float" value="0.5" />
  </crust_mass_diffuse_bsdf>
</materialx>"#;

fn compiled() -> (Compiler<'static>, [u32; 4]) {
    let doc: &'static Doc = Box::leak(Box::new(Doc::parse(LOOKUPS).expect("parses")));
    let host: &'static Host<'static> = Box::leak(Box::new(Host::new(&decline)));
    let mut c = Compiler::new(doc, host);
    let slots = ["color", "normal", "depth", "density"].map(|n| c.compile_named("", n, None));
    assert!(c.unsupported.is_empty(), "{:?}", c.unsupported);
    (c, slots)
}

#[test]
fn every_lookup_reads_its_default_off_a_field() {
    let (c, [color, normal, depth, density]) = compiled();
    let mut slots = Vec::new();
    c.program.eval(&ctx(None), &mut slots);
    assert_eq!(slots[color as usize].rgb(), Vec3A::new(0.1, 0.2, 0.3));
    assert_eq!(slots[normal as usize].rgb(), Vec3A::Z);
    assert_eq!(slots[depth as usize].x(), -1.0);
    assert_eq!(slots[density as usize].x(), 0.0);
    // Without a colour or a normal the field still answers depth and density.
    let bare = MassCtx {
        color: None,
        normal: None,
        depth: 0.25,
        density: 1.5,
    };
    c.program.eval(&ctx(Some(&bare)), &mut slots);
    assert_eq!(slots[color as usize].rgb(), Vec3A::new(0.1, 0.2, 0.3));
    assert_eq!(slots[normal as usize].rgb(), Vec3A::Z);
    assert_eq!(slots[depth as usize].x(), 0.25);
    assert_eq!(slots[density as usize].x(), 1.5);
}

#[test]
fn every_lookup_reads_the_field_the_host_filled() {
    let (c, [color, normal, depth, density]) = compiled();
    let full = MassCtx {
        color: Some(Vec3A::new(0.6, 0.4, 0.2)),
        normal: Some(Vec3A::new(0.6, 0.0, 0.8)),
        depth: 0.75,
        density: 0.9,
    };
    let mut slots = Vec::new();
    c.program.eval(&ctx(Some(&full)), &mut slots);
    assert_eq!(slots[color as usize].rgb(), Vec3A::new(0.6, 0.4, 0.2));
    assert_eq!(slots[color as usize].arity, 3);
    assert_eq!(slots[normal as usize].rgb(), Vec3A::new(0.6, 0.0, 0.8));
    assert_eq!(slots[depth as usize].x(), 0.75);
    assert_eq!(slots[depth as usize].arity, 1);
    assert_eq!(slots[density as usize].x(), 0.9);
}

#[test]
fn the_program_says_which_properties_it_reads() {
    let (c, _) = compiled();
    let reads = c.program.mass_reads();
    assert!(reads.any());
    for p in [
        MassProp::Color,
        MassProp::Normal,
        MassProp::Depth,
        MassProp::Density,
    ] {
        assert!(reads.has(p), "{p:?}");
    }
    assert_eq!(reads, MassReads(15));
    // One lookup alone: its bit alone, and the lookups are impure, so the
    // optimiser keeps them.
    let doc = Doc::parse(
        r#"<materialx><crust_mass_density name="d" type="float">
             <input name="default" type="float" value="2" />
           </crust_mass_density></materialx>"#,
    )
    .unwrap();
    let mut c = Compiler::new(&doc, &Host::new(&decline));
    let root = c.compile_named("", "d", None);
    assert_eq!(c.program.mass_reads(), MassReads(MassProp::Density.bit()));
    let (optimised, remap) = c.program.optimize(&[root]);
    assert_eq!(optimised.mass_reads(), MassReads(MassProp::Density.bit()));
    assert!(matches!(
        optimised.ops[..],
        [Op::Mass {
            prop: MassProp::Density,
            arity: 1,
            ..
        }]
    ));
    let mut slots = Vec::new();
    optimised.eval(&ctx(None), &mut slots);
    assert_eq!(slots[remap[root as usize].unwrap() as usize].x(), 2.0);
    // A program with no lookup reads nothing.
    let plain = Doc::parse(r#"<materialx><constant name="k" type="float"><input name="value" type="float" value="1" /></constant></materialx>"#).unwrap();
    let mut c = Compiler::new(&plain, &Host::new(&decline));
    c.compile_named("", "k", None);
    assert!(!c.program.mass_reads().any());
}

#[test]
fn the_bake_refuses_what_needs_a_view_or_a_hit() {
    let doc = Doc::parse(
        r#"<materialx>
             <viewdirection name="v" type="vector3" />
             <normal name="n" type="vector3" />
             <constant name="k" type="float"><input name="value" type="float" value="1" /></constant>
             <add name="sum" type="vector3">
               <input name="in1" type="vector3" nodename="v" />
               <input name="in2" type="vector3" nodename="n" />
             </add>
           </materialx>"#,
    )
    .unwrap();
    let mut c = Compiler::new(&doc, &Host::new(&decline));
    c.compile_named("", "sum", None);
    assert_eq!(c.program.needs_view_or_hit(), ["viewdirection", "normal"]);
    let mut c = Compiler::new(&doc, &Host::new(&decline));
    c.compile_named("", "k", None);
    assert!(c.program.needs_view_or_hit().is_empty());
}

#[test]
fn the_mass_diffuse_leaf_flattens_with_its_normal() {
    let doc = Doc::parse(LOOKUPS).unwrap();
    let mut c = Compiler::new(&doc, &Host::new(&decline));
    let node = doc.find("", "mass").unwrap().clone();
    let mut out = Closures::default();
    flatten(&mut c, &node, &mut out);
    let leaves: Vec<_> = out
        .nodes
        .iter()
        .filter_map(|n| match n {
            Closure::Leaf(l) => Some(l),
            _ => None,
        })
        .collect();
    assert_eq!(leaves.len(), 1);
    let leaf = leaves[0];
    assert_eq!(leaf.bsdf.category(), "crust_mass_diffuse_bsdf");
    assert!(
        leaf.normal.is_some(),
        "the normal input is a slot of its own"
    );
    assert!(leaf.tangent.is_none());
    let Bsdf::MassDiffuse { color } = leaf.bsdf else {
        panic!("a mass diffuse leaf");
    };
    let mut slots = Vec::new();
    c.program.eval(&ctx(None), &mut slots);
    assert_eq!(slots[color as usize].rgb(), Vec3A::new(0.1, 0.2, 0.3));
    assert_eq!(slots[leaf.weight as usize].x(), 0.5);
    // Unauthored, the colour is the nodedef's default.
    let bare =
        Doc::parse(r#"<materialx><crust_mass_diffuse_bsdf name="m" type="BSDF" /></materialx>"#)
            .unwrap();
    let mut c = Compiler::new(&bare, &Host::new(&decline));
    let node = bare.find("", "m").unwrap().clone();
    let mut out = Closures::default();
    flatten(&mut c, &node, &mut out);
    let Some(Closure::Leaf(leaf)) = out.nodes.first() else {
        panic!("a leaf");
    };
    let Bsdf::MassDiffuse { color } = leaf.bsdf else {
        panic!("a mass diffuse leaf");
    };
    let mut slots = Vec::new();
    c.program.eval(&ctx(None), &mut slots);
    assert_eq!(slots[color as usize], Val::vec3(0.18, 0.18, 0.18));
    assert!(leaf.normal.is_none());
}
