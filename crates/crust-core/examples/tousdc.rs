//! Convert a `.usda` scene to the binary `.usdc` crate format, so importer
//! benchmarks can be run against both encodings of the same scene — the
//! two have very different read profiles (text values are decoded up front
//! at open, crate values are decompressed per attribute read).
//!
//! ```text
//! cargo run --release -p crust-core --example tousdc -- in.usda out.usdc
//! ```
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let stage = openusd::usd::Stage::open(&args[1]).expect("open stage");
    stage
        .root_layer()
        .save_as(&args[2], openusd::sdf::LayerFormat::Usdc)
        .expect("save");
    println!("wrote {}", args[2]);
}
