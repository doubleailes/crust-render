//! The JIT must be invisible: every slot it fills holds exactly the bits the
//! interpreter puts there, at every shading point.

use crust_jit::JitProgram;
use crust_mtlx::{
    BinOp, CmpOp, Compiled, Compiler, CurveCtx, Doc, Host, Op, Program, ShadeCtx, Texture,
    TextureRef, UnOp, Val, compile,
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

fn shading_points() -> Vec<ShadeCtx<'static>> {
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
                curve: None,
            }
        })
        .collect()
}

fn bits(v: &Val) -> ([u32; 4], u8) {
    (v.v.map(f32::to_bits), v.arity)
}

/// Every slot, bitwise, at every point; returns (inline, host) op counts.
fn same(program: &Program, what: &str) -> (usize, usize) {
    same_at(program, what, &shading_points())
}

/// [`same`] at the given points.
fn same_at(program: &Program, what: &str, points: &[ShadeCtx<'_>]) -> (usize, usize) {
    let jit = JitProgram::new(program).expect("compiles for the host");
    let (mut r, mut j) = (Vec::new(), Vec::new());
    for &ctx in points {
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
        let c: Compiled = compile(path, Some(&node.name), &Host::new(&procedural)).unwrap();
        same(&c.program, &format!("{} (compiled)", node.name));
        let mut o: Compiled = compile(path, Some(&node.name), &Host::new(&procedural)).unwrap();
        o.optimize();
        let (inline, host) = same(&o.program, &format!("{} (optimised)", node.name));
        assert_eq!(inline + host, o.program.ops.len());
    }
}

#[test]
fn the_jit_matches_the_interpreter_on_the_fixtures() {
    check_file(&repo().join("samples/materialx_basic.mtlx"));
    check_file(&repo().join("samples/materialx_emissive.mtlx"));
    // The surface-shader builders' programs: every derived closure parameter
    // (coat-broadened roughness, the modulated IOR, the darkening, the
    // `ifgreater` selects) is plain program ops, so it JITs like any graph.
    check_file(&repo().join("samples/materialx_surfaces.mtlx"));
    // The hair helpers are composed from existing ops, so their programs
    // JIT like any graph and must match bit for bit.
    check_file(&repo().join("samples/hair.mtlx"));
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
        shift: [0.0, 0.0],
        coord: None,
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
        // Every target width: narrowing, and widening's zero / one fill.
        for arity in 1..=4 {
            push(Op::Convert { a, arity });
        }
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
            // Inverted edges, where the `x < low` / `x >= high` tests decide.
            push(Op::Smoothstep {
                a,
                low: kc,
                high: b,
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
    // A `heighttonormal` tap: a shifted lookup is the interpreter's, and so
    // are the HSV and height-to-normal ops it and `colorcorrect` compile to.
    let shifted = push(Op::Texture {
        tex: procedural("", None),
        fallback: Val::float(0.5),
        scale: [2.0, 1.0],
        offset: [0.0, 0.0],
        arity: 1,
        shift: [0.5, -0.5],
        coord: None,
    });
    // And one at an authored coordinate, read from a slot.
    push(Op::Texture {
        tex: procedural("", None),
        fallback: Val::float(0.5),
        scale: [1.0, 2.0],
        offset: [0.25, 0.0],
        arity: 3,
        shift: [0.0, 0.0],
        coord: Some(v2),
    });
    let hsv = push(Op::HsvAdjust { a: c3, amount: kc });
    let h2n = push(Op::HeightToNormal {
        xp: shifted,
        xm: f,
        yp: c3,
        ym: zero,
        scale: k,
    });
    push(Op::Binary {
        op: BinOp::Mul,
        a: hsv,
        b: h2n,
    });
    let (inline, host) = same(&p, "synthetic");
    assert!(inline > 900 && host >= 6, "inline {inline}, host {host}");
}

#[test]
fn the_hsv_conditional_and_noise_ops_match_across_widths() {
    // All interpreter calls: what must hold is the width the generated code
    // assumes for each, which the inlined consumers below read.
    let tex = |arity: u8| Op::Texture {
        tex: procedural("", None),
        fallback: Val::float(0.5),
        scale: [3.0, 2.0],
        offset: [0.0, 0.0],
        arity,
        shift: [0.0, 0.0],
        coord: None,
    };
    let mut p = Program::default();
    let mut push = |op: Op| {
        p.ops.push(op);
        (p.ops.len() - 1) as u32
    };
    let f = push(tex(1));
    let v2 = push(tex(2));
    let c3 = push(tex(3));
    let c4 = push(tex(4));
    let half = push(Op::Const(Val::float(0.5)));
    let operands = [f, v2, c3, c4, half];
    for &a in &operands {
        for op in [Op::RgbToHsv { a }, Op::HsvToRgb { a }, Op::CellNoise2 { a }] {
            let r = push(op);
            push(Op::Binary {
                op: BinOp::Mul,
                a: r,
                b: c3,
            });
            push(Op::Combine2 { a: r, b: f });
        }
        for &b in &operands {
            for cmp in [CmpOp::Greater, CmpOp::GreaterEq, CmpOp::Equal] {
                let r = push(Op::Select {
                    cmp,
                    value1: f,
                    value2: half,
                    in1: a,
                    in2: b,
                });
                push(Op::Binary {
                    op: BinOp::Add,
                    a: r,
                    b: f,
                });
                push(Op::Convert { a: r, arity: 4 });
            }
        }
    }
    let (_, host) = same(&p, "hsv / select / cellnoise");
    assert!(host >= 5 * 3 + 5 * 5 * 3, "host {host}");
}

#[test]
fn the_random_nodes_jit_like_any_graph() {
    // `randomfloat` / `randomcolor` expand to their nodegraphs' ops; driven
    // by the chart so that every shading point lands in another cell.
    let doc = Doc::parse(
        r#"<materialx>
             <texcoord name="uv" type="vector2" />
             <extract name="u" type="float"><input name="in" type="vector2" nodename="uv" /></extract>
             <randomfloat name="rf" type="float">
               <input name="in" type="float" nodename="u" />
               <input name="min" type="float" value="-1" />
               <input name="max" type="float" value="3" />
               <input name="seed" type="integer" value="7" />
             </randomfloat>
             <randomcolor name="rc" type="color3">
               <input name="in" type="float" nodename="rf" />
               <input name="saturationlow" type="float" value="0.2" />
               <input name="brightnesslow" type="float" value="0.5" />
             </randomcolor>
             <ifgreatereq name="sel" type="color3">
               <input name="value1" type="float" nodename="rf" />
               <input name="value2" type="float" value="1" />
               <input name="in1" type="color3" nodename="rc" />
               <input name="in2" type="color3" value="0.1, 0.2, 0.3" />
             </ifgreatereq>
           </materialx>"#,
    )
    .unwrap();
    let mut c = Compiler::new(&doc, &Host::new(&procedural));
    let root = c.compile_named("", "sel", None);
    assert!(c.unsupported.is_empty(), "{:?}", c.unsupported);
    same(&c.program, "random nodes (compiled)");
    let (optimised, remap) = c.program.optimize(&[root]);
    assert!(remap[root as usize].is_some());
    same(&optimised, "random nodes (optimised)");
}

/// A curve's primvars at one hit: one of each width, and one the curve
/// does not have.
static PRIMVARS: [Option<Val>; 5] = [
    Some(Val::float(0.75)),
    Some(Val::vec2(0.2, -1.5)),
    Some(Val::vec3(0.9, 0.4, 0.1)),
    Some(Val::vec4(0.1, 0.2, 0.3, 0.5)),
    None,
];

#[test]
fn the_jit_matches_the_interpreter_on_curves() {
    let doc = Doc::parse(
        r#"<materialx>
             <geompropvalue name="v" type="float">
               <input name="geomprop" type="string" value="curve:v" />
               <input name="default" type="float" value="0.5" />
             </geompropvalue>
             <geompropvalue name="index" type="float">
               <input name="geomprop" type="string" value="curve:index" />
             </geompropvalue>
             <geompropvalue name="random" type="color3">
               <input name="geomprop" type="string" value="curve:random" />
               <input name="default" type="color3" value="0.1, 0.2, 0.3" />
             </geompropvalue>
             <geompropvalue name="p0" type="color3">
               <input name="geomprop" type="string" value="melanin" />
             </geompropvalue>
             <geompropvalue name="p1" type="vector2">
               <input name="geomprop" type="string" value="offset" />
             </geompropvalue>
             <geompropvalue name="p2" type="color3">
               <input name="geomprop" type="string" value="hairColor" />
             </geompropvalue>
             <geompropvalue name="p3" type="color4">
               <input name="geomprop" type="string" value="tint" />
             </geompropvalue>
             <geompropvalue name="p4" type="float">
               <input name="geomprop" type="string" value="missing" />
               <input name="default" type="float" value="2" />
             </geompropvalue>
             <mix name="ramp" type="color3">
               <input name="bg" type="color3" nodename="p2" />
               <input name="fg" type="color3" nodename="random" />
               <input name="mix" type="float" nodename="v" />
             </mix>
             <rgbtohsv name="hsv" type="color3"><input name="in" type="color3" nodename="ramp" /></rgbtohsv>
             <hsvtorgb name="rgb" type="color3"><input name="in" type="color3" nodename="hsv" /></hsvtorgb>
             <randomfloat name="rf" type="float">
               <input name="in" type="float" nodename="index" />
               <input name="seed" type="integer" nodename="p4" />
             </randomfloat>
             <randomcolor name="rc" type="color3"><input name="in" type="float" nodename="rf" /></randomcolor>
             <cellnoise2d name="cn" type="float"><input name="texcoord" type="vector2" nodename="p1" /></cellnoise2d>
             <ifequal name="pick" type="color3">
               <input name="value1" type="float" nodename="cn" />
               <input name="value2" type="float" nodename="v" />
               <input name="in1" type="color3" nodename="rc" />
               <input name="in2" type="color3" nodename="rgb" />
             </ifequal>
             <multiply name="m0" type="color3">
               <input name="in1" type="color3" nodename="pick" />
               <input name="in2" type="color3" nodename="p0" />
             </multiply>
             <convert name="c4" type="color4"><input name="in" type="color3" nodename="m0" /></convert>
             <add name="out" type="color4">
               <input name="in1" type="color4" nodename="c4" />
               <input name="in2" type="color4" nodename="p3" />
             </add>
           </materialx>"#,
    )
    .unwrap();
    let mut c = Compiler::new(&doc, &Host::new(&procedural));
    let root = c.compile_named("", "out", None);
    assert!(c.unsupported.is_empty(), "{:?}", c.unsupported);
    // Listed in the order compilation meets them.
    let mut names = c.program.primvars.clone();
    names.sort();
    assert_eq!(names, ["hairColor", "melanin", "missing", "offset", "tint"]);
    // Every point twice: on a strand (whose properties vary, and whose
    // primvar list is cut short at some), and off any curve.
    let points: Vec<ShadeCtx<'static>> = shading_points()
        .into_iter()
        .enumerate()
        .flat_map(|(i, ctx)| {
            let t = i as f32 / 96.0;
            let curve = CurveCtx {
                v: t,
                index: i as u32 * 37,
                random: (t * 7.0).fract(),
                primvars: &PRIMVARS[..5 - i % 4],
            };
            [
                ShadeCtx {
                    curve: Some(curve),
                    ..ctx
                },
                ctx,
            ]
        })
        .collect();
    same_at(&c.program, "curve graph (compiled)", &points);
    let (optimised, remap) = c.program.optimize(&[root]);
    assert!(remap[root as usize].is_some());
    assert_eq!(optimised.primvars, c.program.primvars);
    same_at(&optimised, "curve graph (optimised)", &points);
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
    let mut c: Compiled = compile(&path, None, &Host::new(&procedural)).unwrap();
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
