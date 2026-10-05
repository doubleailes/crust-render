//! Diagnostic: what does a `.mtlx` closure tree resolve to at a given point
//! on the chart?
//!
//! A MaterialX surface can only be wrong in ways that still look like a
//! surface — a mis-decoded albedo is a plausible pastel, a mask read at the
//! wrong colour space is a plausible blend — so comparing renders by eye
//! settles nothing. This prints the numbers instead: every live BSDF leaf the
//! tree collapses to at whatever `(u, v)` you name — its MaterialX category,
//! the RGB weight the tree gives it there (mix factors, multiplies and the
//! throughput of every layer above it), its lobe parameters and normal — plus
//! the emitted radiance and the interior medium. The view is straight down
//! the normal unless `--theta <degrees>` tilts it, since layer throughput is a
//! function of the view angle.
//!
//! ```sh
//! cargo run --release -p crust-render --example mtlx_shade -- \
//!     Looks/teapot_ceramic_ldX.mtlx [material_node] [--theta deg] [u v]...
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
        eprintln!("usage: mtlx_shade <file.mtlx> [material_node] [--theta deg] [u v]...");
        std::process::exit(2);
    };
    let file = Path::new(file);

    // A material node name is optional; anything that parses as a float after
    // it is a coordinate, so the two are told apart by that rather than by
    // position.
    let mut node: Option<String> = None;
    let mut coords: Vec<f32> = Vec::new();
    let mut theta = 0.0f32;
    let mut rest = args[1..].iter();
    while let Some(a) = rest.next() {
        if a == "--theta" {
            theta = rest
                .next()
                .and_then(|t| t.parse::<f32>().ok())
                .unwrap_or(0.0)
                .to_radians();
            continue;
        }
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
    // In `lin_rec709`, the default working space, exactly as the importer
    // loads textures and converts literal colours.
    let working = crust_core::color::Space::LIN_REC709;
    let loader = |asset: &str, space: Option<&str>| -> Option<crust_core::TextureRef> {
        let tex = assets.load_texture(&dir.join(asset), ColorSpace::from_mtlx(space, working))?;
        Some(crust_core::TextureRef(tex))
    };
    let convert = |space: &str, rgb: [f32; 3]| -> [f32; 3] {
        ColorSpace::from_mtlx(Some(space), working)
            .resolved()
            .map_or(rgb, |s| {
                s.decode_rgb(crust_core::Vec3A::from_array(rgb)).to_array()
            })
    };
    let host = crust_core::mtlx::Host {
        load_texture: &loader,
        convert_color: &convert,
    };

    let loaded = match materialx::load(file, node.as_deref(), &host) {
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
    if !loaded.reported.is_empty() {
        println!("not represented: {}", loaded.reported.join("; "));
    }
    println!();

    for (u, v) in points {
        let rec = HitRecord {
            p: Vec3A::ZERO,
            normal: Vec3A::Z,
            t: 1.0,
            front_face: true,
            face: None,
            uv: Some((u, v)),
            tangent: Vec3A::X,
            // Point-sample: this probe reports what the graph evaluates to at a
            // named (u, v), not what a filtered render would show there.
            uv_width: 0.0,
            face_width: 0.0,
        };
        let eye = Vec3A::new(theta.sin(), 0.0, theta.cos());
        let r = Ray::new(eye, -eye);
        let p = loaded.material.probe(&r, &rec);
        // Radiance has no ceiling, unlike the albedos below: a value above 1.0
        // here is the point, not an error.
        println!(
            "(u, v) = ({u:.3}, {v:.3})  emission ({:.4} {:.4} {:.4}){}{}",
            p.emission.x,
            p.emission.y,
            p.emission.z,
            if p.opacity < 1.0 {
                format!("  opacity {:.4}", p.opacity)
            } else {
                String::new()
            },
            p.closure
                .medium()
                .map(|m| format!(
                    "  medium σa ({:.3} {:.3} {:.3}) σs ({:.3} {:.3} {:.3}) g {:.2}",
                    m.sigma_a.x,
                    m.sigma_a.y,
                    m.sigma_a.z,
                    m.sigma_s.x,
                    m.sigma_s.y,
                    m.sigma_s.z,
                    m.g
                ))
                .unwrap_or_default()
        );
        for l in p.closure.leaves() {
            println!(
                "  {:<26} weight ({:.4} {:.4} {:.4})  {}  n ({:.3} {:.3} {:.3})  t ({:.3} {:.3} {:.3})",
                l.category,
                l.weight.x,
                l.weight.y,
                l.weight.z,
                l.describe(),
                l.frame.n.x,
                l.frame.n.y,
                l.frame.n.z,
                l.frame.t.x,
                l.frame.t.y,
                l.frame.t.z
            );
        }
    }
}
