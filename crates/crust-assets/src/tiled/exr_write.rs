//! Writing a tiled, mip-mapped OpenEXR — a `.tx` as OIIO's `maketx` writes one
//! for a half or float source, or for an 8-bit one bound in a curved space
//! (Arnold 7.5's auto-tx).
//!
//! Unlike the TIFF writer next door, almost none of this is hand-rolled: the
//! `exr` crate writes tiles and mip levels through its ordinary public API
//! (`Blocks::Tiles` plus `Levels::Mip`), so what is left here is the
//! de-interleaving and OIIO's attributes, as Arnold's own files carry them
//! (`tests/data/arnold_tx`): `Software` and `Exif:ImageHistory`, `autotx`,
//! `fovcot`, `textureformat`, `wrapmodes`, `oiio:AverageColor`, `oiio:SHA-1`,
//! `oiio:ConstantColor` for a constant image, and the levels rounded down.

use super::TILE_EDGE;
use super::maketx::{Image, SampleType};
use super::write::TxMeta;
use exr::math::RoundingMode;
use exr::meta::attribute::AttributeValue;
use exr::prelude::{
    AnyChannel, AnyChannels, Blocks, Compression, Encoding, FlatSamples, Image as ExrImage, Layer,
    LayerAttributes, LevelMaps, Levels, LineOrder, Text, Vec2, WritableImage,
};
use half::f16;
use std::io;
use std::path::Path;

/// The DWAA level `maketx` writes, `openexr:dwaCompressionLevel`.
const DWA_LEVEL: f32 = 45.0;

/// Writes `levels` (level 0 first) as a tiled EXR of `depth` samples, `half`
/// or `float`, DWAA-compressed when `dwaa` and zipped otherwise, with `meta`.
pub(crate) fn write_exr(
    path: &Path,
    levels: &[Image],
    depth: SampleType,
    dwaa: bool,
    meta: &TxMeta,
) -> io::Result<()> {
    let Some(first) = levels.first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no levels"));
    };
    if first.width == 0 || first.height == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to write a zero-sized texture",
        ));
    }
    let n = first.nchannels();
    // EXR is planar per channel, so the interleaved levels are split once
    // here rather than per tile at write time.
    let mut planes: Vec<LevelMaps<FlatSamples>> = vec![LevelMaps::default(); n];
    for level in levels {
        for (c, plane) in planes.iter_mut().enumerate() {
            let samples = level.pixels.iter().skip(c).step_by(n).copied();
            plane.push(match depth {
                SampleType::Half => FlatSamples::F16(samples.map(f16::from_f32).collect()),
                SampleType::Float => FlatSamples::F32(samples.collect()),
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("an EXR .tx holds half or float samples, not {other:?}"),
                    ));
                }
            });
        }
    }
    // Sorted, because EXR stores channels alphabetically and the reader finds
    // them by name — an unsorted list is a malformed file, not a reordered one.
    let channels = AnyChannels::sort(
        first
            .channels
            .iter()
            .zip(planes)
            .map(|(&name, level_data)| {
                AnyChannel::new(
                    name,
                    Levels::Mip {
                        // `maketx`'s: each level half the one above, rounded
                        // down.
                        rounding_mode: RoundingMode::Down,
                        level_data,
                    },
                )
            })
            .collect(),
    );

    let mut attributes = LayerAttributes {
        wrap_mode_name: Text::new_or_none("black,black"),
        capture_date: Text::new_or_none(meta.date.as_str()),
        ..LayerAttributes::default()
    };
    let text = |v: &str| Text::new_or_none(v).map(AttributeValue::Text);
    // `Software`, capitalised as OIIO writes it: the reserved `software` is
    // another attribute.
    let entries = [
        ("Software", text(&meta.software)),
        ("Exif:ImageHistory", text(&meta.software)),
        ("textureformat", text("Plain Texture")),
        ("oiio:AverageColor", text(&meta.average)),
        ("oiio:SHA-1", text(&meta.sha1)),
        (
            "oiio:ConstantColor",
            meta.constant.as_deref().and_then(text),
        ),
        ("autotx", Some(AttributeValue::I32(1))),
        (
            "fovcot",
            Some(AttributeValue::F32(
                first.width as f32 / first.height as f32,
            )),
        ),
    ];
    for (key, value) in entries {
        if let (Some(k), Some(v)) = (Text::new_or_none(key), value) {
            attributes.other.insert(k, v);
        }
    }

    let layer = Layer::new(
        (first.width, first.height),
        attributes,
        Encoding {
            compression: if dwaa {
                Compression::DWAA(Some(DWA_LEVEL))
            } else {
                // OpenEXR's `zip`, 16 scanlines a block.
                Compression::ZIP16
            },
            blocks: Blocks::Tiles(Vec2(TILE_EDGE, TILE_EDGE)),
            line_order: LineOrder::Increasing,
        },
        channels,
    );
    ExrImage::from_layer(layer)
        .write()
        .to_file(path)
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))
}
