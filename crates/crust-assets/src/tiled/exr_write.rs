//! Writing a tiled, mip-mapped OpenEXR — the HDR half of `.tx`.
//!
//! Unlike the TIFF writer next door, almost none of this is hand-rolled: the
//! `exr` crate writes tiles and mip levels through its ordinary public API
//! (`Blocks::Tiles` plus `Levels::Mip`), so what is left here is the pyramid,
//! the de-interleaving, and the provenance attributes.
//!
//! **The samples stored are linear.** An EXR has no 8-bit type and no transfer
//! curve, so a display-encoded source is decoded **once, at conversion**, and
//! the file records which encoding it came from. That is a different division
//! of labour from the TIFF path — which stores encoded texels and decodes per
//! lookup — and it is the reason [`super::TiledFile::mip_space_matches`] is
//! phrased as "the space this file is meant to be bound with" rather than "the
//! space its levels were averaged in". Both are true of a `.tx`; only the
//! former is true of both backings.
//!
//! **`half`, not `f32`.** It is what `maketx --format exr` writes, what every
//! other streaming texture format stores, and the reason a streamed HDR tile
//! costs twice a `u8` one rather than four times. A texture is shading input,
//! not a render target; `half`'s ~3 decimal digits are past what a BSDF
//! evaluation preserves.

use super::write::space_name;
use crate::uv_texture::reduce_half_linear;
use crust_core::ColorSpace;
use exr::math::RoundingMode;
use exr::prelude::{
    AnyChannel, AnyChannels, Blocks, Compression, Encoding, FlatSamples, Image, Layer,
    LayerAttributes, LevelMaps, Levels, LineOrder, Text, Vec2, WritableImage,
};
use half::f16;
use std::io;
use std::path::Path;

/// Writes `src` (row-major **linear** RGB, `f32`) as a tiled, mip-mapped EXR,
/// returning each level's `(width, height)` in order.
///
/// `space` does not change a single sample — the data is already linear. It is
/// recorded so the renderer can refuse a file whose source encoding is not the
/// one the material binds it with, which would mean applying a curve to values
/// that already had one removed.
pub fn write_tx_exr(
    path: &Path,
    src: &[f32],
    width: usize,
    height: usize,
    space: ColorSpace,
) -> io::Result<Vec<(usize, usize)>> {
    if width == 0 || height == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to write a zero-sized texture",
        ));
    }
    let want = width * height * 3;
    if src.len() < want {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("expected {want} floats of RGB, got {}", src.len()),
        ));
    }

    // The same chain the TIFF writer builds, reduced by the same rule — halving
    // with `div_ceil` so every level spans the whole `[0, 1]` domain — but with
    // no decode/encode around the average, because the samples are already
    // light.
    let mut levels: Vec<(Vec<f32>, usize, usize)> = vec![(src[..want].to_vec(), width, height)];
    while {
        let (_, w, h) = levels.last().expect("level 0 always exists");
        *w > 1 || *h > 1
    } {
        let (pixels, w, h) = {
            let (p, w, h) = levels.last().expect("level 0 always exists");
            reduce_half_linear(p, *w, *h)
        };
        levels.push((pixels, w, h));
    }

    // EXR is planar per channel, so the interleaved source is split once here
    // rather than per tile at write time.
    let mut planes: [LevelMaps<FlatSamples>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (pixels, w, h) in &levels {
        for (k, plane) in planes.iter_mut().enumerate() {
            let samples: Vec<f16> = (0..w * h)
                .map(|i| f16::from_f32(pixels[i * 3 + k]))
                .collect();
            plane.push(FlatSamples::F16(samples));
        }
    }
    let [r, g, b] = planes;
    let mip = |level_data: LevelMaps<FlatSamples>| Levels::Mip {
        // `div_ceil`, which is what `reduce_half_linear` does and what the
        // reader will recompute the level sizes with. `ROUND_DOWN` — the more
        // common choice, and what `maketx` writes — would describe a different
        // pyramid than the one actually stored.
        rounding_mode: RoundingMode::Up,
        level_data,
    };
    // Sorted, because EXR stores channels alphabetically and the reader finds
    // them by name — an unsorted list is a malformed file, not a reordered one.
    let channels = AnyChannels::sort(
        [
            AnyChannel::new("R", mip(r)),
            AnyChannel::new("G", mip(g)),
            AnyChannel::new("B", mip(b)),
        ]
        .into_iter()
        .collect(),
    );

    let mut attributes = LayerAttributes::default();
    let put = |attributes: &mut LayerAttributes, key: &str, value: &str| {
        if let (Some(k), Some(v)) = (Text::new_or_none(key), Text::new_or_none(value)) {
            attributes
                .other
                .insert(k, exr::meta::attribute::AttributeValue::Text(v));
        }
    };
    put(
        &mut attributes,
        super::exr_read::MIP_SPACE_KEY,
        space_name(space),
    );
    // OIIO's own "this is a texture, not a picture" marker, as a first-class
    // attribute rather than smuggled through a description string the way TIFF
    // forces. Its companion `wrapmodes` is deliberately absent: OpenEXR
    // reserves that name for a standard attribute, `exr` refuses to write a
    // reserved name as a custom one, and it exposes no typed field for it — so
    // the choice is to omit it or to hand-write the header, and nothing reads
    // it back here. The `black,black` crust would have written is the default a
    // reader assumes anyway.
    put(&mut attributes, "textureformat", "Plain Texture");
    attributes.software_name = Text::new_or_none("crust-render");

    let layer = Layer::new(
        (width, height),
        attributes,
        Encoding {
            // ZIP16 is what `maketx` defaults to for EXR, lossless, and the one
            // the tile reader is fastest at: a 64-row tile is four zip blocks.
            compression: Compression::ZIP16,
            blocks: Blocks::Tiles(Vec2(super::TILE_EDGE, super::TILE_EDGE)),
            line_order: LineOrder::Increasing,
        },
        channels,
    );

    Image::from_layer(layer)
        .write()
        .to_file(path)
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;

    Ok(levels.iter().map(|(_, w, h)| (*w, *h)).collect())
}
