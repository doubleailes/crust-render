//! Checks a Ptex file's face indexing against the USD mesh it is bound to,
//! in exact integers — no rendering, no eyeballing.
//!
//! # Why this exists
//!
//! A Ptex face id is an index into the mesh's original `faceVertexCounts`, and
//! the `(u, v)` parameterisation of a quad face is defined *relative to that
//! face's vertex order*. Both of those are assumptions the renderer makes and
//! cannot check for itself. If either is wrong the render does not break — it
//! produces a differently-textured surface that still looks like a plausible
//! rock, which is the worst possible failure mode.
//!
//! Disney's island `.ptx` files happen to make the check possible: each one
//! embeds the base cage it was baked against as metadata (`PtexFaceVertCounts`,
//! `PtexFaceVertIndices`, `PtexVertPositions`). So the texture can be asked,
//! independently of the USD, "which vertices does your face N have?" and the
//! answer compared against the mesh's face N.
//!
//! Matching **vertex index sequences, in order** is the strong result: it pins
//! the face correspondence *and* the corner ordering that fixes which way round
//! the texture sits.
//!
//! ```sh
//! cargo run --release -p crust-render --example ptex_verify -- \
//!     model.usd /path/to/mesh_prim color.ptx
//! ```

use openusd::{sdf, usd};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: ptex_verify <stage.usd> <mesh prim path> <texture.ptx>");
        std::process::exit(2);
    }
    let (stage_path, prim_path, ptx_path) = (&args[0], &args[1], &args[2]);

    let mesh = match read_mesh(stage_path, prim_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("USD: {e}");
            std::process::exit(1);
        }
    };
    let mut tx = match ptex::PtexReader::open(ptx_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Ptex: {e}");
            std::process::exit(1);
        }
    };

    println!("mesh    {prim_path}");
    println!("texture {ptx_path}\n");

    let mut failures = 0usize;
    let mut check = |label: &str, ok: bool, detail: String| {
        println!("{:<44} {}  {detail}", label, if ok { "PASS" } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    // 1. Face count. Ptex face ids index the mesh's faces, so the counts must
    //    be equal — not merely compatible.
    let n_ptex = tx.num_faces();
    let n_mesh = mesh.counts.len();
    check(
        "face count: numFaces == faceVertexCounts",
        n_ptex == n_mesh,
        format!("{n_ptex} vs {n_mesh}"),
    );

    // 2. Subfaces. A subface means Ptex split a non-quad into quadrants and its
    //    ids no longer line up 1:1 with mesh faces at all.
    let subfaces = tx.face_infos().iter().filter(|f| f.is_subface()).count();
    check(
        "no subfaces (would break 1:1 ids)",
        subfaces == 0,
        format!("{subfaces} of {n_ptex}"),
    );

    let meta = match tx.metadata() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("\ncannot read metadata ({e}) — the cage checks need it");
            std::process::exit(1);
        }
    };
    let cage_counts = meta.get_i32("PtexFaceVertCounts");
    let cage_indices = meta.get_i32("PtexFaceVertIndices");
    let cage_points = meta.get_f32("PtexVertPositions");

    let Some(cage_counts) = cage_counts else {
        println!("\nno PtexFaceVertCounts metadata — cannot cross-check the cage");
        std::process::exit(if failures == 0 { 0 } else { 1 });
    };

    // 3. Per-face vertex counts, elementwise.
    let counts_match = cage_counts.len() == mesh.counts.len() && cage_counts == mesh.counts;
    let first_count_diff = cage_counts
        .iter()
        .zip(&mesh.counts)
        .position(|(a, b)| a != b);
    check(
        "per-face vertex counts, elementwise",
        counts_match,
        match first_count_diff {
            Some(i) => format!("first differs at face {i}"),
            None => format!("{} faces", cage_counts.len()),
        },
    );

    // 4. The decisive one: the vertex index sequence of every face, in order.
    //    Same set in a rotated order would still tile the mesh correctly but
    //    would rotate every texture by 90 degrees.
    if let Some(cage_indices) = cage_indices {
        let same = cage_indices.len() == mesh.indices.len() && cage_indices == mesh.indices;
        let first = cage_indices
            .iter()
            .zip(&mesh.indices)
            .position(|(a, b)| a != b);
        check(
            "face vertex indices, in order",
            same,
            match first {
                Some(i) => format!("first differs at slot {i}"),
                None => format!("{} indices", cage_indices.len()),
            },
        );
    } else {
        check(
            "face vertex indices, in order",
            false,
            "no PtexFaceVertIndices metadata".into(),
        );
    }

    // 5. Positions, to confirm the two cages are the same geometry and not
    //    merely the same topology. Compared with a tolerance: the island's
    //    bakes differ from the USD by up to an ULP or so.
    if let Some(cage_points) = cage_points {
        let n = cage_points.len() / 3;
        let mut worst = 0.0f32;
        let mut worst_at = 0usize;
        for i in 0..n.min(mesh.points.len()) {
            for c in 0..3 {
                let d = (cage_points[i * 3 + c] - mesh.points[i][c]).abs();
                if d > worst {
                    worst = d;
                    worst_at = i;
                }
            }
        }
        let scale = mesh
            .points
            .iter()
            .flat_map(|p| p.iter().map(|c| c.abs()))
            .fold(0.0f32, f32::max)
            .max(1.0);
        let rel = worst / scale;
        check(
            "vertex positions agree",
            n == mesh.points.len() && rel < 1e-5,
            format!("{n} vs {} verts, worst |d| {worst:.6} at {worst_at} (rel {rel:.2e})",
                mesh.points.len()),
        );
    }

    println!();
    if failures == 0 {
        println!(
            "OK — Ptex face N is mesh face N, with the same corner order, so \
             face ids and quad (u,v) both resolve correctly."
        );
    } else {
        println!("{failures} check(s) failed — Ptex lookups on this mesh are not trustworthy.");
        std::process::exit(1);
    }
}

struct Mesh {
    counts: Vec<i32>,
    indices: Vec<i32>,
    points: Vec<[f32; 3]>,
}

fn read_mesh(stage_path: &str, prim_path: &str) -> Result<Mesh, String> {
    let stage = usd::Stage::open(stage_path)
        .map_err(|e| format!("cannot open {stage_path}: {e}"))?;
    let path = sdf::Path::new(prim_path).map_err(|e| format!("bad prim path: {e}"))?;
    let prim = stage.prim(path);

    let int_vec = |name: &str| -> Result<Vec<i32>, String> {
        match prim.attribute(name).get::<sdf::Value>() {
            Ok(Some(sdf::Value::IntVec(v))) => Ok(v),
            other => Err(format!("{prim_path}.{name}: expected int[], got {other:?}")),
        }
    };
    let counts = int_vec("faceVertexCounts")?;
    let indices = int_vec("faceVertexIndices")?;
    let points = match prim.attribute("points").get::<sdf::Value>() {
        Ok(Some(sdf::Value::Vec3fVec(v))) => v.iter().map(|p| [p.x, p.y, p.z]).collect(),
        other => return Err(format!("{prim_path}.points: expected point3f[], got {other:?}")),
    };
    Ok(Mesh {
        counts,
        indices,
        points,
    })
}
