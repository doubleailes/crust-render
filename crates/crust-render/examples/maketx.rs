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

use crust_core::ColorSpace;
use std::path::{Path, PathBuf};

/// Which backing to write, if the source's range is not to decide.
#[derive(Clone, Copy, PartialEq)]
enum Format {
    Tiff,
    Exr,
    /// 8-bit sources take TIFF, float ones take EXR.
    FromSource,
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut args: Vec<String> = Vec::new();
    let mut format = Format::FromSource;
    for a in &argv {
        match a.split_once('=') {
            Some(("--format", "tiff" | "tif" | "tx")) => format = Format::Tiff,
            Some(("--format", "exr")) => format = Format::Exr,
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

/// The source as it was authored: either 8-bit samples in its own encoding, or
/// floats that are already light.
enum Source {
    Bytes(Vec<u8>),
    Floats(Vec<f32>),
}

fn decode(src: &Path) -> Result<(Source, usize, usize), String> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // EXR goes through crust's own reader: the workspace's `image` has no `exr`
    // feature, and crust-assets is where every format decoder lives anyway.
    if ext == "exr" {
        let (pixels, w, h) =
            crust_assets::read_exr_rgb(src).ok_or_else(|| "could not decode".to_string())?;
        return Ok((Source::Floats(pixels), w, h));
    }

    let mut reader = image::ImageReader::open(src)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    // Same reasoning as the decoder: these are trusted, locally authored
    // assets, and an 8K texture exceeds the default allocation limit — which
    // is exactly the size this tool exists for.
    reader.no_limits();
    let img = reader.decode().map_err(|e| e.to_string())?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    // A Radiance `.hdr` (and any float or 16-bit source `image` hands back)
    // keeps its range; everything else is genuinely 8-bit and narrowing it
    // would be inventing precision to throw away.
    let float = ext == "hdr"
        || matches!(
            img.color(),
            image::ColorType::Rgb32F | image::ColorType::Rgba32F
        );
    if float {
        Ok((Source::Floats(img.to_rgb32f().into_raw()), w, h))
    } else {
        Ok((Source::Bytes(img.to_rgb8().into_raw()), w, h))
    }
}

fn convert(
    src: &Path,
    space: ColorSpace,
    format: Format,
) -> Result<(PathBuf, &'static str, u64, u64), String> {
    let (source, w, h) = decode(src)?;
    if w == 0 || h == 0 {
        return Err("zero-sized image".into());
    }

    let dst = src.with_extension("tx");
    if dst == src {
        return Err("input is already a .tx".into());
    }

    // The default: a source that carries light beyond what 8 bits can express
    // takes the backing that can hold it. Deciding on *content* rather than on
    // the extension is what keeps a `.hdr` of an overcast sky — nothing above
    // 1.0 in it — from paying double for a range it never uses.
    let hdr = match &source {
        Source::Bytes(_) => false,
        Source::Floats(v) => v.iter().any(|&s| s > 1.0),
    };
    let exr = match format {
        Format::Tiff => false,
        Format::Exr => true,
        Format::FromSource => hdr,
    };

    let (kind, levels) = if exr {
        // EXR has no transfer curve, so the decode happens once, here, and the
        // space is recorded as the one the file is to be bound with.
        let linear: Vec<f32> = match &source {
            Source::Floats(v) => v
                .iter()
                .map(|&s| crust_assets::to_linear(space, s))
                .collect(),
            Source::Bytes(v) => v
                .iter()
                .map(|&b| crust_assets::to_linear(space, b as f32 / 255.0))
                .collect(),
        };
        (
            "half, exr",
            crust_assets::tiled::write_tx_exr(&dst, &linear, w, h, space)
                .map_err(|e| e.to_string())?,
        )
    } else {
        let bytes: Vec<u8> = match &source {
            Source::Bytes(v) => v.clone(),
            // Clamping is the honest report of what the TIFF backing can hold;
            // the default only lands here for a float source whose range fits,
            // and `--format=tiff` on a real HDR is the caller's choice.
            Source::Floats(v) => v
                .iter()
                .map(|&s| (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
                .collect(),
        };
        (
            "8-bit, tiff",
            crust_assets::tiled::write_tx(&dst, &bytes, w, h, space).map_err(|e| e.to_string())?,
        )
    };
    if !exr && hdr {
        eprintln!(
            "warning: {} holds values above 1.0 that a TIFF backing clips — \
             drop --format=tiff to keep them",
            src.display()
        );
    }

    let bytes_in = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    let bytes_out = std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
    // Worth printing: a reader that sees fewer levels than it expects is
    // looking at a truncated file, and the count is the cheapest way to notice.
    debug_assert!(!levels.is_empty());
    Ok((dst, kind, bytes_in, bytes_out))
}
