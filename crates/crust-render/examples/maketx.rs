//! Converts an ordinary image into the tiled, mip-mapped `.tx` the streaming
//! texture cache reads, as Arnold 7.5's auto-tx converts it.
//!
//! The same conversion `crust --auto-tx` runs (`crust_assets::make_tx_atomic`):
//! the file is written beside its source under the name Arnold gives it —
//! `<stem>_raw.<ext>.tx`, or `<stem>_<source space>_<target>.<ext>.tx` — of
//! the type Arnold picks (a `half` EXR for an 8-bit source in a curved space,
//! otherwise the source's own sample type), converted to the target space and
//! reduced as OIIO's `maketx` reduces, with OIIO's metadata and the source's
//! modification time. Arnold uses the file as its own, and crust uses
//! Arnold's.
//!
//! The colour space matters and is not guessable from the file: an 8-bit PNG
//! holding an albedo is display-encoded while the same encoding holding a
//! roughness, a mask or a normal is raw data. Pass what the binding says —
//! a MaterialX `colorspace`, `raw`, or `auto` (UsdUVTexture's default: an
//! integer image is sRGB, a float one as stored), which is also the default.
//!
//! ```text
//! cargo run --release -p crust-render --example maketx -- albedo.png srgb_texture
//! cargo run --release -p crust-render --example maketx -- roughness.png raw
//! cargo run --release -p crust-render --example maketx -- sky.exr
//! cargo run --release -p crust-render --example maketx -- 'tex.<UDIM>.png' srgb_texture
//! cargo run --release -p crust-render --example maketx -- albedo.png srgb_texture --tx-color-space=lin_rec709
//! ```
//!
//! A `<UDIM>` / `<UVTILE>` token converts the whole set, one `.tx` per tile,
//! which is how the renderer expects to find them.

use crust_core::ColorSpace;
use crust_core::color::Space;
use std::path::{Path, PathBuf};

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut args: Vec<String> = Vec::new();
    let mut target = crust_assets::default_tx_color_space();
    for a in &argv {
        match a.split_once('=') {
            Some(("--tx-color-space", name)) => match Space::named(name) {
                Some(space) => target = space,
                None => {
                    eprintln!(
                        "--tx-color-space {name}: the OCIO config defines no such colour space"
                    );
                    std::process::exit(2);
                }
            },
            _ if a.starts_with("--") => {
                eprintln!("unknown flag {a}");
                std::process::exit(2);
            }
            _ => args.push(a.clone()),
        }
    }
    if args.is_empty() {
        eprintln!(
            "usage: maketx <image> [colorspace] [--tx-color-space=<space>]\n\
             \n\
             colorspace: the binding's — srgb_texture, lin_rec709, raw, any name or\n\
             alias of the OCIO config — or auto (the default): an integer image is\n\
             sRGB, a float one as stored.\n\
             \n\
             Anything crust reads is accepted (PNG, JPEG, TIFF, HDR, EXR). A <UDIM> or\n\
             <UVTILE> token converts every tile present on disk.\n\
             \n\
             The .tx is written beside its source, named and converted as Arnold 7.5's\n\
             auto-tx names and converts it, to --tx-color-space (default ACEScg)."
        );
        std::process::exit(2);
    }
    let input = args[0].clone();
    let working = Space::LIN_REC709;
    let space = match args.get(1).map(String::as_str) {
        None | Some("auto") => ColorSpace::AUTO,
        // Nothing authored: the values as stored, as Arnold reads `""`.
        Some("") => ColorSpace::RAW,
        Some(name) => {
            if Space::named(name).is_none() && !matches!(name, "srgb" | "raw") {
                // `from_mtlx` maps a name the OCIO config does not know to
                // raw, which is the right default for an absent attribute but
                // a poor one for a typo.
                eprintln!(
                    "warning: '{name}' is not a colour space of the OCIO config — treating it \
                     as raw. Expected srgb_texture, lin_rec709, raw, auto, or any other name \
                     or alias the config defines"
                );
            }
            ColorSpace::from_mtlx(Some(name), working)
        }
    };

    let mut jobs: Vec<PathBuf> = Vec::new();
    if input.contains("<UDIM>") || input.contains("<UVTILE>") {
        jobs = crust_assets::texture_files(Path::new(&input));
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
        match crust_assets::make_tx_atomic(src, space, target) {
            Ok(made) => {
                total_in += made.bytes_in;
                total_out += made.bytes_out;
                println!(
                    "{} -> {} [{}{}]  ({:.2} MiB -> {:.2} MiB)",
                    src.display(),
                    made.dst.display(),
                    made.kind,
                    if made.alpha { ", alpha" } else { "" },
                    made.bytes_in as f64 / (1024.0 * 1024.0),
                    made.bytes_out as f64 / (1024.0 * 1024.0),
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
