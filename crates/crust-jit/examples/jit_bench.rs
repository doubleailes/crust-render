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

#[path = "../../crust-mtlx/examples/bench_common/mod.rs"]
mod bench_common;

use bench_common::{for_each_material, time_ab};
use crust_jit::JitProgram;
use std::time::Instant;

fn main() {
    println!(
        "{:<36} {:>5} {:>12} {:>10} {:>10} {:>9}",
        "material", "ops", "inline/host", "ns/interp", "ns/jit", "compile"
    );
    for_each_material(|name, compile, pts| {
        let mut c = compile();
        c.optimize();
        let p = &c.program;
        let t0 = Instant::now();
        let jit = JitProgram::new(p).expect("jit");
        let compile_ms = t0.elapsed().as_secs_f64() * 1e3;
        let (inl, host) = jit.split();
        let (t_i, t_j) = time_ab(pts, |c, s| p.eval(c, s), |c, s| jit.eval(c, s));
        println!(
            "{:<36} {:>5} {:>12} {:>10.1} {:>10.1} {:>7.2}ms",
            name,
            p.ops.len(),
            format!("{inl}/{host}"),
            t_i,
            t_j,
            compile_ms
        );
    });
}
