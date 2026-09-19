//! Converts an ordinary image into the tiled, mip-mapped `.tx` the streaming
//! texture cache reads.
//!
//! The equivalent of OpenImageIO's `maketx`, except for the one thing crust
//! cannot delegate: **the mip filter**. `maketx` reduces with its own, and this
//! codebase reduces in linear light for the reason `docs/color_management.md`
//! records — averaging display-encoded bytes is not averaging light, and a
//! chain built that way drifts darker at every level. Using the same
//! `reduce_half` as the in-memory pyramid is what lets a streamed render and a
//! preloaded one agree texel for texel, which is the invariant the whole
//! streaming path is checked against.
//!
//! The colour space matters and is not guessable from the file: an 8-bit PNG
//! holding an albedo is display-encoded while the same encoding holding a
//! roughness, a mask or a normal is raw data. MaterialX states it per input,
//! so pass whatever the `colorspace` attribute on the binding says. It is
//! recorded in the file, and the renderer refuses a `.tx` whose chain was
//! reduced in a space other than the one it is about to decode with.
//!
//! ```text
//! cargo run --release -p crust-render --example maketx -- albedo.png srgb_texture
//! cargo run --release -p crust-render --example maketx -- roughness.png raw
//! cargo run --release -p crust-render --example maketx -- 'tex.<UDIM>.png' srgb_texture
//! ```
//!
//! A `<UDIM>` / `<UVTILE>` token converts the whole set, one `.tx` per tile,
//! which is how the renderer expects to find them.

use crust_core::ColorSpace;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "usage: maketx <image> [colorspace]\n\
             \n\
             colorspace: srgb_texture | g22_rec709 | g18_rec709 | raw   (default: raw)\n\
             \n\
             Anything crust reads is accepted (PNG, JPEG, HDR). A <UDIM> or\n\
             <UVTILE> token converts every tile present on disk.\n\
             \n\
             Pass the SAME colorspace the material binds the texture with: the\n\
             mip chain is reduced in linear light, so it is baked into every\n\
             level above 0 and the renderer will refuse a mismatch."
        );
        std::process::exit(2);
    }
    let input = args[0].clone();
    let space = ColorSpace::from_mtlx(args.get(1).map(String::as_str));
    if args.len() > 1 && matches!(space, ColorSpace::Raw) && args[1] != "raw" {
        // `from_mtlx` maps anything it does not know to `Raw`, which is the
        // right default for an absent attribute but a poor one for a typo.
        eprintln!(
            "warning: '{}' is not a colour space crust decodes — treating it as raw. \
             Expected one of: srgb_texture, g22_rec709, g18_rec709, raw",
            args[1]
        );
    }

    let mut jobs: Vec<PathBuf> = Vec::new();
    if input.contains("<UDIM>") || input.contains("<UVTILE>") {
        for v in 0..10u32 {
            for u in 0..10u32 {
                let name = input
                    .replace("<UDIM>", &(1001 + u + 10 * v).to_string())
                    .replace("<UVTILE>", &format!("u{}_v{}", u + 1, v + 1));
                let p = PathBuf::from(&name);
                if p.exists() {
                    jobs.push(p);
                }
            }
        }
        if jobs.is_empty() {
            eprintln!("no tiles of {input} found on disk");
            std::process::exit(1);
        }
    } else {
        jobs.push(PathBuf::from(&input));
    }

    let mut total_in = 0u64;
    let mut total_out = 0u64;
    let mut failed = 0usize;
    for src in &jobs {
        match convert(src, space) {
            Ok((dst, bytes_in, bytes_out)) => {
                total_in += bytes_in;
                total_out += bytes_out;
                println!(
                    "{} -> {}  ({:.2} MiB -> {:.2} MiB)",
                    src.display(),
                    dst.display(),
                    bytes_in as f64 / (1024.0 * 1024.0),
                    bytes_out as f64 / (1024.0 * 1024.0),
                );
            }
            Err(e) => {
                eprintln!("{}: {e}", src.display());
                failed += 1;
            }
        }
    }

    if jobs.len() > 1 {
        println!(
            "\n{} tile(s), {:.2} MiB -> {:.2} MiB ({:.0}% — the pyramid adds about a third, \
             compression takes more back)",
            jobs.len(),
            total_in as f64 / (1024.0 * 1024.0),
            total_out as f64 / (1024.0 * 1024.0),
            100.0 * total_out as f64 / total_in.max(1) as f64,
        );
    }
    if failed > 0 {
        std::process::exit(1);
    }
}

fn convert(src: &Path, space: ColorSpace) -> Result<(PathBuf, u64, u64), String> {
    let mut reader = image::ImageReader::open(src)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    // Same reasoning as the decoder: these are trusted, locally authored
    // assets, and an 8K texture exceeds the default allocation limit — which
    // is exactly the size this tool exists for.
    reader.no_limits();
    let img = reader.decode().map_err(|e| e.to_string())?.to_rgb8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 {
        return Err("zero-sized image".into());
    }

    let dst = src.with_extension("tx");
    if dst == src {
        return Err("input is already a .tx".into());
    }
    let levels = crust_assets::tiled::write_tx(&dst, img.as_raw(), w, h, space)
        .map_err(|e| e.to_string())?;

    let bytes_in = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    let bytes_out = std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
    // Worth printing: a reader that sees fewer levels than it expects is
    // looking at a truncated file, and the count is the cheapest way to notice.
    debug_assert!(!levels.is_empty());
    Ok((dst, bytes_in, bytes_out))
}
