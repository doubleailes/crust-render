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
//! **Two backings, chosen by the source's range.** An 8-bit source becomes a
//! tiled TIFF with `u8` tiles, exactly as before. Anything that carries values
//! above 1.0 — an EXR, a Radiance `.hdr` — becomes a tiled, mip-mapped EXR with
//! `half` tiles instead, because a `u8` tile would clip precisely the data the
//! file exists to carry. `--format` overrides the choice in either direction.
//! Both are written with the `.tx` extension: it names a purpose, not a
//! container, and the renderer picks the backing by magic number (which is also
//! what makes `maketx --format exr` output readable here).
//!
//! The colour space matters and is not guessable from the file: an 8-bit PNG
//! holding an albedo is display-encoded while the same encoding holding a
//! roughness, a mask or a normal is raw data. MaterialX states it per input,
//! so pass whatever the `colorspace` attribute on the binding says. It is
//! recorded in the file, and the renderer refuses a `.tx` that is not the one
//! to bind in the space it is about to decode with.
//!
//! ```text
//! cargo run --release -p crust-render --example maketx -- albedo.png srgb_texture
//! cargo run --release -p crust-render --example maketx -- roughness.png raw
//! cargo run --release -p crust-render --example maketx -- sky.exr raw
//! cargo run --release -p crust-render --example maketx -- albedo.png srgb_texture --format=exr
//! cargo run --release -p crust-render --example maketx -- 'tex.<UDIM>.png' srgb_texture
//! ```
//!
//! A `<UDIM>` / `<UVTILE>` token converts the whole set, one `.tx` per tile,
//! which is how the renderer expects to find them.

use crust_assets::tiled::TxFormat;
use crust_core::ColorSpace;
use std::path::{Path, PathBuf};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut args: Vec<String> = Vec::new();
    let mut format = TxFormat::FromRange;
    for a in &argv {
        match a.split_once('=') {
            Some(("--format", "tiff" | "tif" | "tx")) => format = TxFormat::Tiff,
            Some(("--format", "exr")) => format = TxFormat::Exr,
            Some(("--format", other)) => {
                eprintln!("unknown --format {other} — expected tiff or exr");
                std::process::exit(2);
            }
            _ if a.starts_with("--") => {
                eprintln!("unknown flag {a}");
                std::process::exit(2);
            }
            _ => args.push(a.clone()),
        }
    }
    if args.is_empty() {
        eprintln!(
            "usage: maketx <image> [colorspace] [--format=tiff|exr]\n\
             \n\
             colorspace: srgb_texture | g22_rec709 | g18_rec709 | raw   (default: raw)\n\
             \n\
             Anything crust reads is accepted (PNG, JPEG, HDR, EXR). A <UDIM> or\n\
             <UVTILE> token converts every tile present on disk.\n\
             \n\
             Pass the SAME colorspace the material binds the texture with: it is\n\
             recorded in the file and the renderer refuses a mismatch. An 8-bit\n\
             source keeps its encoding and is decoded per lookup; a float one is\n\
             decoded once, here, since EXR has no transfer curve of its own.\n\
             \n\
             --format picks the backing. By default a source carrying values\n\
             above 1.0 becomes a tiled mip EXR and everything else a tiled mip\n\
             TIFF; both are written as <name>.tx."
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
        match convert(src, space, format) {
            Ok((dst, kind, bytes_in, bytes_out)) => {
                total_in += bytes_in;
                total_out += bytes_out;
                println!(
                    "{} -> {} [{kind}]  ({:.2} MiB -> {:.2} MiB)",
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

/// One tile, through the same conversion `crust-render --auto-tx` runs
/// (`crust_assets::tiled::make_tx`), written beside the source.
fn convert(
    src: &Path,
    space: ColorSpace,
    format: TxFormat,
) -> Result<(PathBuf, &'static str, u64, u64), String> {
    let made = crust_assets::tiled::make_tx_atomic(src, space, format)?;
    if made.clipped {
        eprintln!(
            "warning: {} holds values above 1.0 that a TIFF backing clips — \
             drop --format=tiff to keep them",
            src.display()
        );
    }
    Ok((made.dst, made.kind, made.bytes_in, made.bytes_out))
}
