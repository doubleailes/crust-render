//! What `mtlx_bench` (crust-mtlx) and `jit_bench` (crust-jit) share, included
//! by each with `#[path]`: the procedural texture, the shading points, the
//! interleaved A/B timer and the walk over every `surfacematerial` of the files
//! given — so the two benches time the same work and differ only in what they
//! compare.

use crust_mtlx::{Compiled, Doc, Host, ShadeCtx, Texture, TextureRef, Val, compile};
use glam::Vec3A;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

pub struct Procedural;
impl Texture for Procedural {
    fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        [u.fract().abs(), v.fract().abs(), 0.5, 0.5 + width]
    }
}

pub fn procedural(_: &str, _: Option<&str>) -> Option<TextureRef> {
    Some(TextureRef(Arc::new(Procedural)))
}

const POINTS: usize = 4096;
const REPEATS: usize = 15;

pub fn points() -> Vec<ShadeCtx> {
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
pub fn time_ab(
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

/// For every `surfacematerial` in every file named on the command line, hands
/// `bench` its name, a compiler for it (against the procedural texture; call
/// it once per variant wanted) and the shading points.
pub fn for_each_material(mut bench: impl FnMut(&str, &dyn Fn() -> Compiled, &[ShadeCtx])) {
    let pts = points();
    for path in std::env::args().skip(1) {
        let path = std::path::Path::new(&path);
        let doc = Doc::open(path).expect("parse");
        for node in doc.by_category("surfacematerial") {
            let compile = || compile(path, Some(&node.name), &Host::new(&procedural)).unwrap();
            bench(&node.name, &compile, &pts);
        }
    }
}
