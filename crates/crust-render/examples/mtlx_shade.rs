//! Diagnostic: what OpenPBR parameters does a `.mtlx` graph reduce to at a
//! given point on the chart?
//!
//! A MaterialX surface can only be wrong in ways that still look like a
//! surface — a mis-decoded albedo is a plausible pastel, a mask read at the
//! wrong colour space is a plausible blend — so comparing renders by eye
//! settles nothing. This prints the numbers instead: the base colour,
//! metalness, roughness, lobe weights and the coat (the second specular lobe
//! a dielectric layered over another specular reduces to) the reduction
//! produces at whatever `(u, v)` you name, which can be checked against the
//! texture's own texels.
//!
//! ```sh
//! cargo run --release -p crust-render --example mtlx_shade -- \
//!     Looks/teapot_ceramic_ldX.mtlx [material_node] [u v]...
//! ```
//!
//! With no coordinates it sweeps a few points across the first UDIM tile.

use crust_assets::FileAssets;
use crust_core::AssetLoader;
use crust_core::ColorSpace;
use crust_core::materialx;
use crust_core::{HitRecord, Ray, Vec3A};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(file) = args.first() else {
        eprintln!("usage: mtlx_shade <file.mtlx> [material_node] [u v]...");
        std::process::exit(2);
    };
    let file = Path::new(file);

    // A material node name is optional; anything that parses as a float after
    // it is a coordinate, so the two are told apart by that rather than by
    // position.
    let mut node: Option<String> = None;
    let mut coords: Vec<f32> = Vec::new();
    for a in &args[1..] {
        match a.parse::<f32>() {
            Ok(v) => coords.push(v),
            Err(_) if node.is_none() => node = Some(a.clone()),
            Err(_) => eprintln!("ignoring '{a}'"),
        }
    }
    let points: Vec<(f32, f32)> = if coords.len() >= 2 {
        coords
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| (c[0], c[1]))
            .collect()
    } else {
        vec![(0.1, 0.1), (0.3, 0.5), (0.5, 0.5), (0.7, 0.5), (0.9, 0.9)]
    };

    // The renderer's own asset seam, not just its decoder, so the numbers
    // printed are the ones a render would shade with: bilinear, every UDIM
    // tile, `CRUST_TEX_MAX` honoured, the colour space resolved by the same
    // spelling table — and `CRUST_TEX_STREAM` obeyed.
    //
    // That last one is why this goes through `FileAssets` rather than calling
    // `UvTexture::open` directly, as it used to. The preloaded decoder narrows
    // to 8 bits at `to_rgb8()`, so an HDR emission texture probed through it
    // reads 1.0 whatever the file holds — and this probe is the tool the
    // project uses to settle a MaterialX question in numbers. A probe that
    // cannot see the range is worse than no probe, because it answers
    // confidently.
    let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
    let assets = FileAssets::new();
    let loader = |asset: &str, space: Option<&str>| -> Option<crust_core::TextureRef> {
        let tex = assets.load_texture(&dir.join(asset), ColorSpace::from_mtlx(space))?;
        Some(crust_core::TextureRef(tex))
    };

    let loaded = match materialx::load(file, node.as_deref(), &loader) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot load {}: {e}", file.display());
            std::process::exit(1);
        }
    };
    println!("{}  ({})", file.display(), loaded.summary);
    println!("{} texture(s) resolved", loaded.textures);
    if !loaded.unsupported.is_empty() {
        println!("unsupported nodes: {}", loaded.unsupported.join(", "));
    }
    println!();

    // A hit looking straight down at a flat, upward-facing patch: the view
    // direction matters (these graphs compute a view-dependent glaze path
    // length), so it is stated rather than left at a default.
    println!(
        "{:>6} {:>6}   {:>22} {:>6} {:>6} {:>6} {:>6} {:>10}   {:>22}",
        "u",
        "v",
        "base_color",
        "metal",
        "rough",
        "spec",
        "coat",
        "coat_rough",
        // The product, not the two fields: OpenPBR only ever multiplies them
        // back together, so the split is a presentation choice and either
        // field alone would mislead. A value above 1.0 here is the point --
        // radiance has no ceiling, unlike the albedo two columns left.
        "emission"
    );
    for (u, v) in points {
        let rec = HitRecord {
            p: Vec3A::ZERO,
            normal: Vec3A::Z,
            t: 1.0,
            front_face: true,
            face_id: HitRecord::NO_FACE,
            face_uv: (0.0, 0.0),
            uv: (u, v),
            tangent: Vec3A::X,
            has_uv: true,
            // Point-sample: this probe reports what the graph evaluates to at a
            // named (u, v), not what a filtered render would show there.
            uv_width: 0.0,
            face_width: 0.0,
        };
        let r = Ray::new(Vec3A::new(0.0, 0.0, 1.0), -Vec3A::Z);
        let m = loaded.material.probe(&r, &rec);
        let emission = m.emission_color * m.emission_luminance;
        println!(
            "{u:>6.3} {v:>6.3}   ({:>6.4} {:>6.4} {:>6.4}) {:>6.3} {:>6.4} {:>6.3} {:>6.3} {:>10.4}   \
             ({:>6.3} {:>6.3} {:>6.3})",
            m.base_color.x,
            m.base_color.y,
            m.base_color.z,
            m.base_metalness,
            m.specular_roughness,
            m.specular_weight,
            m.coat_weight,
            m.coat_roughness,
            emission.x,
            emission.y,
            emission.z
        );
    }
}
