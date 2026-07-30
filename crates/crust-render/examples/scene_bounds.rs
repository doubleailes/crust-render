//! Prints the world-space bounds of a USD scene, plus a camera distance that
//! would frame it. Placing a camera in a downloaded production asset otherwise
//! means guessing at the unit scale.
//!
//! ```sh
//! cargo run --release -p crust-render --example scene_bounds -- scene.usda
//! ```

use std::path::PathBuf;

fn main() {
    let Some(path) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: scene_bounds <scene.usd[a]>");
        std::process::exit(2);
    };

    let scene = match crust_core::Scene::from_usd(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load {}: {e}", path.display());
            std::process::exit(1);
        }
    };

    match scene.world.bounds() {
        Some(b) => {
            let (min, max) = (b.minimum, b.maximum);
            let size = max - min;
            let center = (min + max) * 0.5;
            let radius = size.length() * 0.5;
            println!("min    = ({:.3}, {:.3}, {:.3})", min.x, min.y, min.z);
            println!("max    = ({:.3}, {:.3}, {:.3})", max.x, max.y, max.z);
            println!("size   = ({:.3}, {:.3}, {:.3})", size.x, size.y, size.z);
            println!("center = ({:.3}, {:.3}, {:.3})", center.x, center.y, center.z);
            println!("radius = {radius:.3}");
            // Distance at which a sphere of `radius` fills a 35mm/36mm frame.
            println!("suggested camera distance (35mm on 36mm aperture) = {:.3}", radius / 0.5);
        }
        None => println!("scene has no bounded geometry"),
    }
    println!("lights = {}", scene.lights.lights.len());
}
