//! The program optimiser must be invisible: at every shading point, every
//! slot a consumer reads holds exactly the value — bit for bit — that the
//! unoptimised program computes there.
//!
//! Checked over every material in the checked-in `.mtlx` fixtures, and over
//! the DPEL teapot and lion when they have been downloaded, with a
//! procedural texture standing in for every `image` node so the texture
//! paths are exercised rather than folded away as declined fallbacks.

use crust_mtlx::{Compiled, Doc, Program, ShadeCtx, Texture, TextureRef, Val, compile};
use glam::Vec3A;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Varies in every channel and with the footprint, so a lookup at the wrong
/// coordinates, or a folded-away texture, cannot go unnoticed.
struct Procedural;
impl Texture for Procedural {
    fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        [
            (u * 3.7).fract().abs(),
            (v * 5.3).fract().abs(),
            (u * v * 11.0).sin() * 0.5 + 0.5,
            0.5 + width,
        ]
    }
}

fn procedural(_: &str, _: Option<&str>) -> Option<TextureRef> {
    Some(TextureRef(Arc::new(Procedural)))
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn eval(p: &Program, ctx: &ShadeCtx) -> Vec<Val> {
    let mut slots = Vec::new();
    p.eval(ctx, &mut slots);
    slots
}

/// Shading points spread over two UDIM tiles, both faces, a spread of
/// normals, tangents and footprints.
fn shading_points() -> Vec<ShadeCtx> {
    let mut out = Vec::new();
    for i in 0..64 {
        let t = i as f32 / 64.0;
        let n = Vec3A::new((t * 7.0).sin(), (t * 5.0).cos(), 1.0).normalize();
        out.push(ShadeCtx {
            uv: (t * 2.0, (t * 13.0).fract()),
            normal: n,
            tangent: if i % 5 == 0 { Vec3A::ZERO } else { Vec3A::X },
            view: -Vec3A::new(0.2, (t * 3.0).sin(), 1.0).normalize(),
            position: Vec3A::new(t, 1.0 - t, t * t),
            uv_width: if i % 3 == 0 { 0.0 } else { t * 0.01 },
        });
    }
    out
}

fn bits(v: Val) -> ([u32; 4], u8) {
    (v.v.map(f32::to_bits), v.arity)
}

fn check(path: &Path) -> usize {
    let doc = Doc::open(path).unwrap();
    let names: Vec<String> = doc
        .by_category("surfacematerial")
        .map(|n| n.name.clone())
        .collect();
    assert!(
        !names.is_empty(),
        "{} has no surfacematerial",
        path.display()
    );
    for name in &names {
        let reference: Compiled = compile(path, Some(name), &procedural).unwrap();
        let mut optimized: Compiled = compile(path, Some(name), &procedural).unwrap();
        optimized.optimize();
        assert!(
            optimized.program.ops.len() <= reference.program.ops.len(),
            "{name}: the optimiser added work"
        );
        let (ra, oa) = (reference.roots(), optimized.roots());
        assert_eq!(ra.len(), oa.len());
        for ctx in shading_points() {
            let (r, o) = (
                eval(&reference.program, &ctx),
                eval(&optimized.program, &ctx),
            );
            for (&rs, &os) in ra.iter().zip(&oa) {
                assert_eq!(
                    bits(r[rs as usize]),
                    bits(o[os as usize]),
                    "{name}: slot {rs} (now {os}) differs at uv {:?}",
                    ctx.uv
                );
            }
        }
    }
    names.len()
}

#[test]
fn the_optimised_program_matches_the_reference_on_the_fixtures() {
    let n = check(&repo().join("samples/materialx_basic.mtlx"))
        + check(&repo().join("samples/materialx_emissive.mtlx"));
    assert!(n >= 5, "expected the fixtures' materials, found {n}");
}

#[test]
fn the_optimised_program_matches_the_reference_on_the_dpel_assets() {
    // Gitignored downloads: checked when present, skipped otherwise.
    let looks = repo().join("samples/MaterialXTeapotLion-1.0");
    for f in [
        "Lion/Looks/lion_ldX.mtlx",
        "Teapot/Looks/teapot_ceramic_ldX.mtlx",
        "Teapot/Looks/teapot_metal_ldX.mtlx",
    ] {
        let p = looks.join(f);
        if p.exists() {
            check(&p);
        }
    }
}

#[test]
fn constants_are_folded_hoisted_and_pruned() {
    // `(2 + 3) * uv.x` plus a node nothing reads: the sum folds, the two
    // literals it consumed disappear, and the orphan is dropped.
    let doc = r#"<materialx>
        <add name="five" type="float">
          <input name="in1" type="float" value="2" />
          <input name="in2" type="float" value="3" />
        </add>
        <texcoord name="tc" type="vector2" />
        <extract name="u" type="float">
          <input name="in" type="vector2" nodename="tc" />
          <input name="index" type="integer" value="0" />
        </extract>
        <multiply name="out" type="float">
          <input name="in1" type="float" nodename="five" />
          <input name="in2" type="float" nodename="u" />
        </multiply>
        <sin name="orphan" type="float">
          <input name="in" type="float" nodename="u" />
        </sin>
      </materialx>"#;
    let doc = Doc::parse(doc).unwrap();
    let loader = procedural;
    let mut c = crust_mtlx::Compiler::new(&doc, &loader);
    let out = c.compile_named("", "out", None);
    c.compile_named("", "orphan", None);
    let (opt, remap) = c.program.optimize(&[out]);
    assert_eq!(opt.consts, vec![Val::float(5.0)]);
    // texcoord, extract, multiply — the adds, literals and sine are gone.
    assert_eq!(opt.ops.len(), 3);
    let ctx = shading_points()[7];
    let r = eval(&c.program, &ctx)[out as usize];
    let o = eval(&opt, &ctx)[remap[out as usize].unwrap() as usize];
    assert_eq!(bits(r), bits(o));
}
