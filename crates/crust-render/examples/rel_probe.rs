//! Diagnostic: can a relationship's targets be read on a prototype-internal
//! prim?
//!
//! The importer skips a `PointInstancer` whose `prototypes` relationship has no
//! targets. On the Moana island that fires for instancers *inside* a native
//! instance's prototype, which silently drops the xgen vegetation they place.
//! This asks the same question two ways — off the prim reached by traversal and
//! off the same path composed fresh — and also reports whether the targets, if
//! any, resolve to prims that exist.
//!
//! ```sh
//! cargo run --release -p crust-render --example rel_probe -- stage.usd[a] [relName]
//! ```

use openusd::{sdf, usd};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(stage_path) = args.first() else {
        eprintln!("usage: rel_probe <stage.usd[a]> [relationship name]");
        std::process::exit(2);
    };
    let rel_name = args.get(1).map(String::as_str).unwrap_or("prototypes");

    let stage = match usd::Stage::builder()
        .load(usd::InitialLoadSet::LoadAll)
        .open(stage_path)
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open: {e}");
            std::process::exit(1);
        }
    };
    println!("stage {stage_path}, relationship '{rel_name}'\n");

    // Walk the whole composed stage, descending into prototypes when an
    // instance is met, and report every PointInstancer found either way.
    let root = stage.prim(sdf::Path::abs_root());
    let mut seen = Vec::new();
    walk(&stage, &root, 0, rel_name, &mut seen, false);

    println!("\n--- summary ---");
    let (mut ok, mut empty) = (0, 0);
    for (path, n, in_proto) in &seen {
        if *n > 0 { ok += 1 } else { empty += 1 }
        println!(
            "{:<5} {:>3} target(s)  {}{}",
            if *n > 0 { "OK" } else { "EMPTY" },
            n,
            path,
            if *in_proto { "   (inside a prototype)" } else { "" }
        );
    }
    println!("\n{ok} instancer(s) with targets, {empty} without");
}

fn walk(
    stage: &usd::Stage,
    prim: &usd::Prim,
    depth: usize,
    rel_name: &str,
    seen: &mut Vec<(String, usize, bool)>,
    in_proto: bool,
) {
    if depth > 12 {
        return;
    }
    let ty = prim.type_name().ok().flatten().unwrap_or_default();
    if ty == "PointInstancer" {
        let n = prim
            .relationship(rel_name)
            .targets()
            .map(|t| t.len())
            .unwrap_or(0);
        seen.push((prim.path().to_string(), n, in_proto));
        // What does this instancer actually carry? A stub with no properties
        // is a different problem from a populated one whose rel will not read.
        if seen.len() <= 2 {
            println!("instancer {}", prim.path());
            match prim.property_names() {
                Ok(names) => println!("  properties ({}): {names:?}", names.len()),
                Err(e) => println!("  property_names failed: {e}"),
            }
            match prim.relationships() {
                Ok(rels) => {
                    println!("  relationships: {}", rels.len());
                    for r in &rels {
                        match r.targets() {
                            Ok(t) => {
                                println!("    {:?} -> {} target(s)", r.path(), t.len());
                                for tp in t.iter().take(4) {
                                    println!("        {tp}");
                                }
                            }
                            Err(e) => println!("    {:?} -> Err({e})", r.path()),
                        }
                    }
                }
                Err(e) => println!("  relationships failed: {e}"),
            }
        }
    }

    // Descend into the prototype rather than the instance's proxy subtree,
    // matching what the importer does.
    if prim.is_instance().unwrap_or(false) {
        if let Ok(Some(proto_path)) = prim.prototype() {
            let proto = stage.prim(proto_path);
            if let Ok(kids) = proto.children() {
                for k in kids.iter() {
                    walk(stage, k, depth + 1, rel_name, seen, true);
                }
            }
        }
        return;
    }

    let Ok(kids) = prim.children() else { return };
    for k in kids.iter() {
        walk(stage, k, depth + 1, rel_name, seen, in_proto);
    }
}
