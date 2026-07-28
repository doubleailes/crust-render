//! Does USD traversal scale across threads?
//!
//! openusd's `Stage` is `Rc`-based, so it cannot be shared between
//! threads — but nothing stops each thread from opening its *own* stage
//! and composing a disjoint slice of the namespace. This probe measures
//! whether that actually buys wall-clock time (composition is CPU-bound
//! and per-prim, so it should) and what the per-thread stage costs.
//!
//! ```text
//! cargo run --release -p crust-core --example usd_parallel_probe -- scene.usda [max_threads]
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use openusd::sdf;
use openusd::usd::Stage;

/// Composes every prim under `root`, doing the two queries the importer
/// does on every prim it visits.
fn walk_subtree(stage: &Stage, root: sdf::Path) -> usize {
    let mut n = 0usize;
    let mut stack = vec![stage.prim_at(root)];
    while let Some(prim) = stack.pop() {
        n += 1;
        if prim.is_abstract().unwrap_or(false) {
            continue;
        }
        if let Ok(children) = prim.children() {
            stack.extend(children);
        }
    }
    n
}

/// Breadth-first expansion of the namespace until there are at least
/// `want` subtree roots — the unit of work handed to the threads.
fn split_roots(stage: &Stage, want: usize) -> Vec<sdf::Path> {
    let mut level = vec![sdf::Path::abs_root()];
    for _ in 0..8 {
        if level.len() >= want {
            break;
        }
        let mut next = Vec::new();
        for path in &level {
            match stage.prim_at(path.clone()).children() {
                Ok(children) if !children.is_empty() => {
                    next.extend(children.into_iter().map(|c| c.path().clone()))
                }
                // A leaf stays in the work list; it has no children to split.
                _ => next.push(path.clone()),
            }
        }
        // No progress: every item is a leaf, so there is nothing to split.
        if next == level {
            break;
        }
        level = next;
    }
    level
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: <scene.usda> [max_threads]");
    let max_threads: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()));

    let t = Instant::now();
    let stage = Stage::open(&path).expect("open stage");
    let open = t.elapsed().as_secs_f64();
    println!("open stage: {open:.3}s");

    let roots = split_roots(&stage, max_threads * 8);
    println!("work items: {}", roots.len());
    drop(stage);

    let mut baseline = 0.0;
    let mut threads = 1;
    while threads <= max_threads {
        let next = AtomicUsize::new(0);
        let counted = AtomicUsize::new(0);
        let t = Instant::now();
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    // Each worker composes on its own stage: same file,
                    // independent `Rc` graph and composition cache.
                    let t_open = Instant::now();
                    let stage = Stage::open(&path).expect("open stage");
                    let open = t_open.elapsed().as_secs_f64();
                    let t_walk = Instant::now();
                    let mut mine = 0usize;
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(root) = roots.get(i) else { break };
                        let n = walk_subtree(&stage, root.clone());
                        mine += n;
                        counted.fetch_add(n, Ordering::Relaxed);
                    }
                    println!(
                        "      worker: open {open:>6.3}s  walk {:>6.3}s  ({mine} prims)",
                        t_walk.elapsed().as_secs_f64()
                    );
                });
            }
        });
        let secs = t.elapsed().as_secs_f64();
        if threads == 1 {
            baseline = secs;
        }
        println!(
            "{threads:>3} thread(s): {secs:>8.3}s   speedup {:>5.2}x   ({} prims)",
            baseline / secs,
            counted.load(Ordering::Relaxed)
        );
        threads *= 2;
    }
}
