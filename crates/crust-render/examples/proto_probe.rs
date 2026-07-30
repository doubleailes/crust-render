//! Diagnostic: what does openusd expose inside a native instance's prototype,
//! and do schema lookups work on prototype-internal paths?
//!
//! The island places nearly all of its geometry through `instanceable = true`
//! prims whose contents arrive via a payload. When such an element imports as
//! nothing at all, the question is *which* step lost it: the prototype's
//! children, or the schema `get()` the importer does on each child's path.
//!
//! For every prim beneath the prototype this reports how it was reached and
//! whether the same prim is still a Mesh when looked up by path — which is how
//! the importer asks.
//!
//! ```sh
//! cargo run --release -p crust-render --example proto_probe -- stage.usda [/prim/path]
//! ```

use openusd::{sdf, usd};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(stage_path) = args.first() else {
        eprintln!("usage: proto_probe <stage.usd[a]> [/root/prim]");
        std::process::exit(2);
    };
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

    println!("stage {stage_path}\n");

    // Find the first instanceable prim and walk its prototype exhaustively.
    let start = match args.get(1) {
        Some(p) => stage.prim(sdf::Path::new(p).expect("bad path")),
        None => stage.prim(sdf::Path::abs_root()),
    };
    let Some(inst) = find_instance(&start, 0) else {
        println!("no instanceable prim found");
        return;
    };
    println!("instance {}", inst.path());
    let Ok(Some(proto_path)) = inst.prototype() else {
        println!("  no prototype");
        return;
    };
    println!("prototype {proto_path}\n");

    let proto = stage.prim(proto_path.clone());
    let mut counts = Counts::default();
    walk(&stage, &proto, 0, &mut counts);

    println!("\n--- totals ---");
    println!("prims visited (via children())      {}", counts.prims);
    println!("Mesh by type_name                   {}", counts.mesh_by_type);
    println!("Mesh by UsdMesh::get(path)          {}", counts.mesh_by_schema);
    println!("prims whose prim_at(path) is valid  {}", counts.path_valid);
    println!("prims with readable 'points'        {}", counts.has_points);
    println!("PointInstancers                     {}", counts.instancers);
    println!("nested instanceable prims           {}", counts.nested_instances);
    if counts.mesh_by_type > 0 && counts.mesh_by_schema == 0 {
        println!(
            "\n=> The prototype HAS meshes, but a schema get() on their paths finds none.\n   \
             The importer asks by path, so it sees an empty prototype."
        );
    }
}

#[derive(Default)]
struct Counts {
    prims: usize,
    mesh_by_type: usize,
    mesh_by_schema: usize,
    path_valid: usize,
    has_points: usize,
    instancers: usize,
    nested_instances: usize,
}

fn find_instance(prim: &usd::Prim, depth: usize) -> Option<usd::Prim> {
    if depth > 6 {
        return None;
    }
    if prim.is_instance().unwrap_or(false) {
        return Some(prim.clone());
    }
    for k in prim.children().ok()?.iter() {
        if let Some(f) = find_instance(k, depth + 1) {
            return Some(f);
        }
    }
    None
}

fn walk(stage: &usd::Stage, prim: &usd::Prim, depth: usize, c: &mut Counts) {
    if depth > 10 {
        return;
    }
    c.prims += 1;
    let path = prim.path().clone();
    let ty = prim.type_name().ok().flatten().unwrap_or_default();

    // The two ways of asking the same question.
    let by_path = stage.prim(path.clone());
    let path_ty = by_path.type_name().ok().flatten().unwrap_or_default();
    let path_ok = !path_ty.is_empty();
    if path_ok {
        c.path_valid += 1;
    }
    let schema_mesh = matches!(
        openusd::schemas::geom::Mesh::get(stage, path.clone()),
        Ok(Some(_))
    );
    let points_ok = matches!(
        prim.attribute("points").get::<sdf::Value>(),
        Ok(Some(sdf::Value::Vec3fVec(_)))
    );
    if points_ok {
        c.has_points += 1;
    }
    if ty == "Mesh" {
        c.mesh_by_type += 1;
    }
    if schema_mesh {
        c.mesh_by_schema += 1;
    }
    if ty == "PointInstancer" {
        c.instancers += 1;
    }
    if depth > 0 && prim.is_instance().unwrap_or(false) {
        c.nested_instances += 1;
    }

    // Only print the interesting rows; a production prototype has thousands.
    if ty == "Mesh" || ty == "PointInstancer" || depth <= 3 {
        println!(
            "{}{path} [{ty}]  prim_at:{}  UsdMesh::get:{}  points:{}",
            "  ".repeat(depth),
            if path_ok { &path_ty } else { "INVALID" },
            if schema_mesh { "yes" } else { "no" },
            if points_ok { "yes" } else { "no" },
        );
    }

    let Ok(kids) = prim.children() else { return };
    for k in kids.iter() {
        walk(stage, k, depth + 1, c);
    }
}
