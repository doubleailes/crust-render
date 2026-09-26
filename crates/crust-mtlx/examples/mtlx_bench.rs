//! Deterministic MaterialX-program throughput probe: min-of-N nanoseconds per
//! program run, per material, for the unoptimised and optimised programs.
//!
//! What a render spends on a MaterialX surface is dominated, after the
//! shade-once split, by one [`Program::eval`] per path vertex — so an
//! interpreter change is measured here, in seconds, rather than through a
//! ten-minute callgrind of the lion. Textures are a cheap procedural stand-in,
//! which makes the interpreter's share larger than in a render: compare
//! variants against each other with this, then confirm on a scene.
//!
//! ```text
//! cargo run --release -p crust-mtlx --example mtlx_bench -- \
//!     samples/MaterialXTeapotLion-1.0/Lion/Looks/lion_ldX.mtlx
//! ```
//!
//! Every file given is benchmarked for every `surfacematerial` it holds.

use crust_mtlx::{Compiled, Doc, Program, ShadeCtx, Texture, TextureRef, Val, compile};
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

/// Min-of-N nanoseconds per run of `run` over every shading point.
fn time(pts: &[ShadeCtx], mut run: impl FnMut(&ShadeCtx, &mut Vec<Val>)) -> f64 {
    let mut slots = Vec::new();
    let mut best = f64::INFINITY;
    for _ in 0..REPEATS {
        let t = Instant::now();
        for p in pts {
            run(black_box(p), &mut slots);
            black_box(&slots);
        }
        best = best.min(t.elapsed().as_nanos() as f64 / pts.len() as f64);
    }
    best
}

fn main() {
    let pts = points();
    println!(
        "{:<40} {:>6} {:>6} {:>10} {:>10}",
        "material", "ops", "opt", "ns/run", "ns/opt"
    );
    for path in std::env::args().skip(1) {
        let path = std::path::Path::new(&path);
        let doc = Doc::open(path).expect("parse");
        for node in doc.by_category("surfacematerial") {
            let reference: Compiled = compile(path, Some(&node.name), &procedural).unwrap();
            let mut optimized: Compiled = compile(path, Some(&node.name), &procedural).unwrap();
            optimized.optimize();
            let (p, o): (&Program, &Program) = (&reference.program, &optimized.program);
            let t_ref = time(&pts, |c, s| p.eval(c, s));
            let t_opt = time(&pts, |c, s| o.eval(c, s));
            println!(
                "{:<40} {:>6} {:>6} {:>10.1} {:>10.1}",
                node.name,
                p.len(),
                o.ops.len(),
                t_ref,
                t_opt
            );
        }
    }
}
