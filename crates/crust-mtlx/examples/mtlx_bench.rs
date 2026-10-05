//! Deterministic MaterialX-program throughput probe: min-of-N nanoseconds per
//! program run, per material, for the unoptimised and optimised programs.
//!
//! What a render spends on a MaterialX surface is dominated, after the
//! shade-once split, by one [`crust_mtlx::Program::eval`] per path vertex — so an
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

#[path = "bench_common/mod.rs"]
mod bench_common;

use bench_common::{for_each_material, time_ab};

fn main() {
    println!(
        "{:<40} {:>6} {:>6} {:>10} {:>10}",
        "material", "ops", "opt", "ns/run", "ns/opt"
    );
    for_each_material(|name, compile, pts| {
        let reference = compile();
        let mut optimized = compile();
        optimized.optimize();
        let (p, o) = (&reference.program, &optimized.program);
        let (t_ref, t_opt) = time_ab(pts, |c, s| p.eval(c, s), |c, s| o.eval(c, s));
        println!(
            "{:<40} {:>6} {:>6} {:>10.1} {:>10.1}",
            name,
            p.len(),
            o.ops.len(),
            t_ref,
            t_opt
        );
    });
}
