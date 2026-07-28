//! Where does USD import time actually go?
//!
//! Walks a stage several times, each pass doing strictly more per prim, so
//! the cost of an individual composed query is the difference between two
//! adjacent passes. Run it on a prim-heavy scene (see
//! `scripts/gen_moana_like_scene.py`) when tuning the importer.
//!
//! ```text
//! cargo run --release -p crust-core --example usd_probe -- scene.usda
//! ```

use std::time::Instant;

use openusd::schemas::geom::{Camera, Mesh, PointBased, PointInstancer, Sphere, Xform};
use openusd::schemas::lux::{DistantLight, DomeLight, RectLight, SphereLight};
use openusd::sdf;
use openusd::usd::{Prim, Stage};

fn walk(stage: &Stage, mut per_prim: impl FnMut(&Prim)) -> usize {
    let mut n = 0usize;
    let mut stack = vec![stage.prim_at(sdf::Path::abs_root())];
    while let Some(prim) = stack.pop() {
        n += 1;
        per_prim(&prim);
        if let Ok(children) = prim.children() {
            stack.extend(children);
        }
    }
    n
}

fn timed(label: &str, stage: &Stage, per_prim: impl FnMut(&Prim)) -> f64 {
    let t = Instant::now();
    let n = walk(stage, per_prim);
    let secs = t.elapsed().as_secs_f64();
    println!(
        "{label:<44} {secs:>8.3}s   {:>8.2} us/prim   ({n} prims)",
        secs * 1e6 / n as f64
    );
    secs
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: usd_probe <scene.usda>");

    let t = Instant::now();
    let stage = Stage::open(&path).expect("open stage");
    println!("{:<44} {:>8.3}s", "open stage", t.elapsed().as_secs_f64());

    // Warm the composition cache so later passes measure query cost, not
    // the one-time prim-index build.
    timed("walk: children() only (cold cache)", &stage, |_| {});
    timed("walk: children() only (warm)", &stage, |_| {});
    timed("walk: + is_active + is_abstract", &stage, |p| {
        let _ = p.is_active();
        let _ = p.is_abstract();
    });
    timed("walk: + type_name x1", &stage, |p| {
        let _ = p.type_name();
    });
    timed("walk: + type_name x10", &stage, |p| {
        for _ in 0..10 {
            let _ = p.type_name();
        }
    });
    timed("walk: + xformOpOrder attr get", &stage, |p| {
        let _ = p.attribute("xformOpOrder").get::<sdf::Value>();
    });
    timed("walk: + is_instance", &stage, |p| {
        let _ = p.is_instance();
    });
    timed("walk: + schema get() x10 (importer dispatch)", &stage, |p| {
        let s = p.stage();
        let path = p.path();
        let _ = PointInstancer::get(s, path.clone());
        let _ = Mesh::get(s, path.clone());
        let _ = Sphere::get(s, path.clone());
        let _ = Camera::get(s, path.clone());
        let _ = SphereLight::get(s, path.clone());
        let _ = RectLight::get(s, path.clone());
        let _ = DistantLight::get(s, path.clone());
        let _ = DomeLight::get(s, path.clone());
        let _ = Xform::get(s, path.clone());
        let _ = Xform::get(s, path.clone());
    });
    timed("walk: + custom attr probe x1 (crust:rayMask)", &stage, |p| {
        let _ = p.attribute("crust:rayMask").get::<sdf::Value>();
    });

    // What a mesh-heavy scene pays just to get the arrays out of USD,
    // before the importer hashes, triangulates or builds anything.
    let mut points = 0usize;
    let mut faces = 0usize;
    timed("walk: + read mesh points/counts/indices", &stage, |p| {
        if let Ok(Some(mesh)) = Mesh::get(p.stage(), p.path().clone()) {
            if let Ok(Some(sdf::Value::Vec3fVec(v))) = mesh.points_attr().get::<sdf::Value>() {
                points += v.len();
            }
            if let Ok(Some(sdf::Value::IntVec(v))) =
                mesh.face_vertex_counts_attr().get::<sdf::Value>()
            {
                faces += v.len();
            }
            let _ = mesh.face_vertex_indices_attr().get::<sdf::Value>();
        }
    });
    println!("  ({points} points, {faces} faces read)");
}
