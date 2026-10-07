//! Diagnostic: does openusd compose a multi-op `xformOpOrder` stack correctly?
//!
//! `scene/usd_import/xform.rs` used to compose `xformOp:*` attributes itself
//! because openusd 0.5.0 got the order wrong — the authored translate came back
//! multiplied by the scale, which rendered `samples/cornellbox.usda` as floating
//! objects. The importer now takes openusd's composition for every prim, so this
//! asks openusd directly and compares against the hand-computed answer, to catch
//! a regression on a version bump.
//!
//! ```sh
//! cargo run --release -p crust-render --example xform_probe -- stage.usda /prim/path
//! ```

use openusd::{sdf, usd};
use openusd_schemas::geom::{Xform, Xformable};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: xform_probe <stage.usda> <prim path>");
        std::process::exit(2);
    }
    let stage = usd::Stage::builder()
        .load(usd::InitialLoadSet::LoadAll)
        .open(&args[0])
        .expect("open");
    let path = sdf::Path::new(&args[1]).expect("path");

    let x = Xform::get(&stage, path.clone())
        .expect("Xform::get")
        .expect("prim is an Xform");
    match x.local_to_parent_transform(0.0) {
        Ok(m) => {
            println!("openusd local_transformation for {}:", args[1]);
            // Row-major as USD stores it; the translation is the last row.
            for r in 0..4 {
                let row: Vec<String> = (0..4)
                    .map(|c| format!("{:>12.4}", m.0[r * 4 + c]))
                    .collect();
                println!("  [{}]", row.join(" "));
            }
            println!(
                "translation row = [{:.4}, {:.4}, {:.4}]",
                m.0[12], m.0[13], m.0[14]
            );
        }
        Err(e) => println!("local_transformation failed: {e}"),
    }
}
