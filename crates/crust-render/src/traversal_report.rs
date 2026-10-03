//! The BVH traversal table `--stats` adds in a `traversal-stats` build.
//!
//! Kept apart from `RenderStats` because its counts come from the kernel and
//! only exist when the diagnostic feature is compiled in.

use crust_core::World;

/// The table, one string, for the caller to emit as a single `--stats`
/// event: a `println!` per row would leave these lines out of `--log-file`,
/// and one event per row would stamp each of them with a timestamp the table
/// has no column for. Counts are per camera ray.
pub(super) fn traversal_report(camera_rays: u64, world: &World) -> String {
    use crust_core::rt::traversal_stats as ts;
    use std::fmt::Write as _;
    let rays = camera_rays.max(1) as f64;
    let per = |n: u64| n as f64 / rays;
    let rule = "-".repeat(84);
    let mut out = String::new();
    // Infallible: `write!` into a String only fails if the formatter
    // does, and none of these arguments can.
    let _ = write!(out, "\n{rule}\nBVH Traversal (per camera ray)\n{rule}");
    for (level, name) in [(0usize, "top-level"), (1, "instanced")] {
        let (q, nodes, leaves, packets, scalars) = ts::read_level(level);
        if q == 0 {
            continue;
        }
        let _ = write!(
            out,
            "\n  {name:<12} queries {:>8.2}  nodes {:>9.2}  leaves {:>8.2}  packets {:>7.2}  scalar {:>8.2}",
            per(q),
            per(nodes),
            per(leaves),
            per(packets),
            per(scalars),
        );
    }
    // Which top-level instances the descents went into. A top level that
    // culls well spreads them thinly; one that does not concentrates them
    // on whatever geometry every ray's path overlaps. The importer's
    // DEBUG lines give each instancer's `geom ids a..b` range, which is
    // how an id here is traced back to a prim.
    let descents = ts::top_level_descents();
    let total: u64 = descents.iter().map(|d| d.1).sum();
    if total > 0 {
        let mut acc = 0u64;
        let mut marks = vec![];
        for (i, d) in descents.iter().enumerate() {
            acc += d.1;
            for f in [0.5, 0.9, 0.99] {
                if (acc as f64) >= f * total as f64 && !marks.iter().any(|&(g, _)| g == f) {
                    marks.push((f, i + 1));
                }
            }
        }
        let _ = write!(
            out,
            "\n  top-level instances entered: {} of them, {:.1} descents per camera ray \
                 (closest-hit and shadow rays; the rows above count closest-hit only)",
            descents.len(),
            per(total)
        );
        for (f, n) in marks {
            let _ = write!(
                out,
                "\n    {:.0}% of descents go to {n} instances",
                f * 100.0
            );
        }
        let top: Vec<_> = descents.iter().take(40).collect();
        let ids: std::collections::HashSet<u32> = top.iter().map(|d| d.0).collect();
        let info: std::collections::HashMap<u32, _> = world
            .describe_instances(&ids)
            .into_iter()
            .map(|(id, b, n, shared)| (id, (b, n, shared)))
            .collect();
        let _ = write!(
            out,
            "\n  {:>9} {:>7} {:>9} {:>8} {:>9}  bounds",
            "geom_id", "share", "per ray", "prims", "shared by"
        );
        for &&(id, n) in &top {
            let (b, prims, shared) = info[&id];
            let _ = write!(
                out,
                "\n  {id:>9} {:>6.2}% {:>9.2} {prims:>8} {shared:>9}  [{:.0} {:.0} {:.0}]..[{:.0} {:.0} {:.0}]",
                100.0 * n as f64 / total as f64,
                per(n),
                b.minimum.x,
                b.minimum.y,
                b.minimum.z,
                b.maximum.x,
                b.maximum.y,
                b.maximum.z,
            );
        }
    }
    out
}
