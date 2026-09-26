//! Min-of-N nanoseconds per program run, interpreter against JIT, on the
//! optimised program of every `surfacematerial` in the given files.
//!
//! The companion of crust-mtlx's `mtlx_bench`, with the same procedural
//! texture and shading points; `inline/host` is how many ops the JIT emitted
//! as machine code and how many it handed back to the interpreter.
//!
//! ```text
//! cargo run --release -p crust-jit --example jit_bench -- \
//!     samples/MaterialXTeapotLion-1.0/Lion/Looks/lion_ldX.mtlx
//! ```

use crust_jit::JitProgram;
use crust_mtlx::{Compiled, Doc, ShadeCtx, Texture, TextureRef, Val, compile};
use glam::Vec3A;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

struct Procedural;
impl Texture for Procedural {
    fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        [u.fract().abs(), v.fract().abs(), 0.5, 0.5 + width]
    }
}

fn procedural(_: &str, _: Option<&str>) -> Option<TextureRef> {
    Some(TextureRef(Arc::new(Procedural)))
}

const POINTS: usize = 4096;
const REPEATS: usize = 15;

fn points() -> Vec<ShadeCtx> {
    (0..POINTS)
        .map(|i| {
            let t = i as f32 / POINTS as f32;
            ShadeCtx {
                uv: (t * 2.0, (t * 17.0).fract()),
                normal: Vec3A::new((t * 7.0).sin(), (t * 5.0).cos(), 1.0).normalize(),
                tangent: Vec3A::X,
                view: -Vec3A::new(0.2, (t * 3.0).sin(), 1.0).normalize(),
                position: Vec3A::new(t, 1.0 - t, t * t),
                uv_width: t * 0.01,
            }
        })
        .collect()
}

/// Min-of-N nanoseconds per run for two evaluators, **interleaved**: each
/// repeat times both, alternating which goes first, so load or a frequency
/// change lands on both rather than on whichever phase it happened to hit
/// (the in-process version of `scripts/bench_ab.sh`).
fn time_ab(
    pts: &[ShadeCtx],
    mut a: impl FnMut(&ShadeCtx, &mut Vec<Val>),
    mut b: impl FnMut(&ShadeCtx, &mut Vec<Val>),
) -> (f64, f64) {
    let mut slots = Vec::new();
    let once = |run: &mut dyn FnMut(&ShadeCtx, &mut Vec<Val>), slots: &mut Vec<Val>| {
        let t = Instant::now();
        for p in pts {
            run(black_box(p), slots);
            black_box(&*slots);
        }
        t.elapsed().as_nanos() as f64 / pts.len() as f64
    };
    let (mut best_a, mut best_b) = (f64::INFINITY, f64::INFINITY);
    for rep in 0..REPEATS {
        if rep % 2 == 0 {
            best_a = best_a.min(once(&mut a, &mut slots));
            best_b = best_b.min(once(&mut b, &mut slots));
        } else {
            best_b = best_b.min(once(&mut b, &mut slots));
            best_a = best_a.min(once(&mut a, &mut slots));
        }
    }
    (best_a, best_b)
}

fn main() {
    let pts = points();
    println!(
        "{:<36} {:>5} {:>12} {:>10} {:>10} {:>9}",
        "material", "ops", "inline/host", "ns/interp", "ns/jit", "compile"
    );
    for path in std::env::args().skip(1) {
        let path = std::path::Path::new(&path);
        let doc = Doc::open(path).expect("parse");
        for node in doc.by_category("surfacematerial") {
            let mut c: Compiled = compile(path, Some(&node.name), &procedural).unwrap();
            c.optimize();
            let p = &c.program;
            let t0 = Instant::now();
            let jit = JitProgram::new(p).expect("jit");
            let compile_ms = t0.elapsed().as_secs_f64() * 1e3;
            let (inl, host) = jit.split();
            let (t_i, t_j) = time_ab(&pts, |c, s| p.eval(c, s), |c, s| jit.eval(c, s));
            println!(
                "{:<36} {:>5} {:>12} {:>10.1} {:>10.1} {:>7.2}ms",
                node.name,
                p.ops.len(),
                format!("{inl}/{host}"),
                t_i,
                t_j,
                compile_ms
            );
        }
    }
}
