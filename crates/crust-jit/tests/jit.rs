//! The JIT must be invisible: every slot it fills holds exactly the bits the
//! interpreter puts there, at every shading point.

use crust_jit::JitProgram;
use crust_mtlx::{
    BinOp, Compiled, Doc, Op, Program, ShadeCtx, Texture, TextureRef, UnOp, Val, compile,
};
use glam::Vec3A;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Varies in every lane, crosses zero (so divide guards take both arms) and
/// goes negative and above one (so clamps and remaps do too).
struct Procedural;
impl Texture for Procedural {
    fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        let s = (u * 7.3).sin();
        [
            s * 1.5,
            (v * 5.1).fract() - 0.25,
            if (u * 3.0).fract() < 0.2 {
                0.0
            } else {
                u * v * 3.0 - 1.0
            },
            0.5 + width - (v * 2.0).cos(),
        ]
    }
}

fn procedural(_: &str, _: Option<&str>) -> Option<TextureRef> {
    Some(TextureRef(Arc::new(Procedural)))
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn shading_points() -> Vec<ShadeCtx> {
    (0..96)
        .map(|i| {
            let t = i as f32 / 96.0;
            ShadeCtx {
                uv: (t * 2.0 - 0.3, (t * 13.0).fract()),
                normal: Vec3A::new((t * 7.0).sin(), (t * 5.0).cos(), 1.0).normalize(),
                tangent: if i % 5 == 0 { Vec3A::ZERO } else { Vec3A::X },
                view: -Vec3A::new(0.2, (t * 3.0).sin(), 1.0).normalize(),
                position: Vec3A::new(t, 1.0 - t, t * t),
                uv_width: if i % 3 == 0 { 0.0 } else { t * 0.01 },
            }
        })
        .collect()
}

fn bits(v: &Val) -> ([u32; 4], u8) {
    (v.v.map(f32::to_bits), v.arity)
}

/// Every slot, bitwise, at every point; returns (inline, host) op counts.
fn same(program: &Program, what: &str) -> (usize, usize) {
    let jit = JitProgram::new(program).expect("compiles for the host");
    let (mut r, mut j) = (Vec::new(), Vec::new());
    for ctx in shading_points() {
        program.eval(&ctx, &mut r);
        jit.eval(&ctx, &mut j);
        assert_eq!(r.len(), j.len(), "{what}: slot count");
        for (i, (a, b)) in r.iter().zip(&j).enumerate() {
            assert_eq!(bits(a), bits(b), "{what}: slot {i} at uv {:?}", ctx.uv);
        }
    }
    jit.split()
}

fn check_file(path: &Path) {
    let doc = Doc::open(path).unwrap();
    for node in doc.by_category("surfacematerial") {
        let c: Compiled = compile(path, Some(&node.name), &procedural).unwrap();
        same(&c.program, &format!("{} (compiled)", node.name));
        let mut o: Compiled = compile(path, Some(&node.name), &procedural).unwrap();
        o.optimize();
        let (inline, host) = same(&o.program, &format!("{} (optimised)", node.name));
        assert_eq!(inline + host, o.program.ops.len());
    }
}

#[test]
fn the_jit_matches_the_interpreter_on_the_fixtures() {
    check_file(&repo().join("samples/materialx_basic.mtlx"));
    check_file(&repo().join("samples/materialx_emissive.mtlx"));
}

#[test]
fn the_jit_matches_the_interpreter_on_the_dpel_assets() {
    // Gitignored downloads: checked when present, skipped otherwise.
    let looks = repo().join("samples/MaterialXTeapotLion-1.0");
    for f in [
        "Lion/Looks/lion_ldX.mtlx",
        "Teapot/Looks/teapot_ceramic_ldX.mtlx",
        "Teapot/Looks/teapot_metal_ldX.mtlx",
    ] {
        let p = looks.join(f);
        if p.exists() {
            check_file(&p);
        }
    }
}

#[test]
fn every_inlined_op_matches_across_widths_and_edge_values() {
    let tex = |arity: u8| Op::Texture {
        tex: procedural("", None),
        fallback: Val::float(0.5),
        scale: [1.0, 1.0],
        offset: [0.0, 0.0],
        arity,
    };
    let mut p = Program::default();
    let mut push = |op: Op| {
        p.ops.push(op);
        (p.ops.len() - 1) as u32
    };
    // Operands of every width, including a one-lane texture whose lanes
    // 1..3 differ from lane 0, and a zero for the divide guards.
    let f = push(tex(1));
    let c3 = push(tex(3));
    let c4 = push(tex(4));
    let v2 = push(tex(2));
    let k = push(Op::Const(Val::float(0.75)));
    let zero = push(Op::Const(Val::float(0.0)));
    let kc = push(Op::Const(Val::vec3(0.2, -1.0, 4.0)));
    let neg = push(Op::Const(Val::float(-0.0)));
    let operands = [f, c3, c4, v2, k, zero, kc, neg];
    for &a in &operands {
        push(Op::Unary { op: UnOp::Abs, a });
        push(Op::Convert { a, arity: 3 });
        push(Op::Convert { a, arity: 1 });
        for index in [0, 2, 7] {
            push(Op::Extract { a, index });
        }
        for &b in &operands {
            for op in [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div] {
                push(Op::Binary { op, a, b });
            }
            push(Op::Invert { a, amount: b });
            push(Op::Combine2 { a, b });
            push(Op::Mix { fg: a, bg: b, m: f });
            push(Op::Mix {
                fg: a,
                bg: b,
                m: c3,
            });
            push(Op::Contrast {
                a,
                amount: b,
                pivot: k,
            });
            push(Op::Smoothstep {
                a,
                low: b,
                high: kc,
            });
            push(Op::Smoothstep {
                a,
                low: zero,
                high: zero,
            });
            push(Op::Remap {
                a,
                in_low: b,
                in_high: f,
                out_low: kc,
                out_high: c3,
            });
            push(Op::Remap {
                a,
                in_low: zero,
                in_high: zero,
                out_low: k,
                out_high: b,
            });
            push(Op::Combine3 { a, b, c: f });
        }
    }
    // And ops that go back to the interpreter, feeding inlined ones.
    let n = push(Op::Unary {
        op: UnOp::Normalize,
        a: f,
    });
    push(Op::Binary {
        op: BinOp::Mul,
        a: n,
        b: c3,
    });
    let m = push(Op::Binary {
        op: BinOp::Max,
        a: f,
        b: kc,
    });
    push(Op::Remap {
        a: m,
        in_low: zero,
        in_high: c3,
        out_low: f,
        out_high: kc,
    });
    let (inline, host) = same(&p, "synthetic");
    assert!(inline > 900 && host >= 2, "inline {inline}, host {host}");
}

#[test]
fn a_malformed_program_is_refused_not_compiled() {
    // The generated code reads operands without bounds checks, so a program
    // whose operand points forward or out of range must never reach it.
    let mut p = Program::default();
    p.ops.push(Op::Const(Val::float(2.0)));
    p.ops.push(Op::Binary {
        op: BinOp::Mul,
        a: 0,
        b: 1_000_000,
    });
    assert!(JitProgram::new(&p).is_err());
    p.ops[1] = Op::Binary {
        op: BinOp::Mul,
        a: 0,
        b: 1,
    };
    assert!(JitProgram::new(&p).is_err(), "an op reading its own slot");
}

#[test]
fn programs_can_be_built_and_dropped_repeatedly() {
    // Each program owns and frees its code; a survivor must keep working
    // while others are compiled and freed around it.
    let path = repo().join("samples/materialx_basic.mtlx");
    let mut c: Compiled = compile(&path, None, &procedural).unwrap();
    c.optimize();
    let survivor = JitProgram::new(&c.program).unwrap();
    let ctx = shading_points()[11];
    let (mut want, mut got) = (Vec::new(), Vec::new());
    c.program.eval(&ctx, &mut want);
    for _ in 0..500 {
        let j = JitProgram::new(&c.program).unwrap();
        j.eval(&ctx, &mut got);
        drop(j);
        survivor.eval(&ctx, &mut got);
        assert_eq!(
            want.iter().map(bits).collect::<Vec<_>>(),
            got.iter().map(bits).collect::<Vec<_>>()
        );
    }
}
