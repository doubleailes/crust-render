//! Writing a `deepRaster` product as an OpenEXRId matte
//! (<https://github.com/MercenariesEngineering/openexrid>, file version 3),
//! the layout its own `Builder` writes and its Nuke plug-in reads:
//!
//! - one part, deep scanline, ZIPS (`exr`'s `ZIP1`) compression;
//! - per pixel, one sample per name its camera samples hit first, front to
//!   back: `Id` (UINT, the name's index in the file's table), `Z` (FLOAT, its
//!   mean camera depth), `A` (HALF, its coverage) and, when the product has
//!   a beauty var, `R`, `G`, `B` (HALF, its share of the beauty);
//! - the samples in the deep "over" form `Builder::finish` gives them — a
//!   sample's values divided by the transparency of the samples in front of
//!   it — which a deep compositor (and OpenEXRId's readers) undo;
//! - `EXRIdVersion = 3` and `EXRIdNames`: the names, joined by newlines,
//!   zlib-compressed and base64-encoded.
//!
//! The names are the ones a sample uses, sorted, so a file holds no name it
//! does not show and the same scene writes the same table.
//!
//! The newline is the Nuke plug-in's separator for version 3; OpenEXRId's
//! own `Mask::read` (and so its OpenFX plug-in) splits on NUL instead, and
//! reads the table of such a file as one name (`openspec/specs/aovs/
//! design.md`, "OpenEXRId as built").

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use base64::Engine;
use crust_core::stamp::SamplingStamp;
use crust_core::{AovFilm, AovProduct, Buffer, MatteView};
use exr::image::deep::DeepChannels;
use exr::meta::attribute::AttributeValue;
use exr::prelude::{
    AnyChannel, Blocks, Compression, Encoding, FlatSamples, Image, LineOrder, Text, WritableImage,
    f16,
};

use super::products::{exr_windows, product_attributes};

/// The OpenEXRId file version written: names base64-encoded after zlib.
pub const EXRID_VERSION: i32 = 3;

/// The header attribute holding the version.
pub const VERSION_ATTRIBUTE: &str = "EXRIdVersion";

/// The header attribute holding the name table.
pub const NAMES_ATTRIBUTE: &str = "EXRIdNames";

/// The channels a matte writes, as the file stores them (alphabetically).
pub fn channel_names(color: bool) -> Vec<String> {
    let names: &[&str] = if color {
        &["A", "B", "G", "Id", "R", "Z"]
    } else {
        &["A", "Id", "Z"]
    };
    names.iter().map(|n| (*n).to_owned()).collect()
}

/// The `EXRIdNames` value of `names`: joined by newlines, zlib-compressed,
/// base64-encoded.
pub fn encode_names(names: &[&str]) -> io::Result<String> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(names.join("\n").as_bytes())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(z.finish()?))
}

/// One pixel's samples in OpenEXRId's deep "over" form, from front-to-back
/// plain shares `(r, g, b, a)`: each divided by the transparency the samples
/// in front of it leave — `openexrid::Builder::finish`, which a reader undoes
/// by multiplying each sample by that transparency.
///
/// The form holds coverage in `[0, 1]` that sums to at most one; a filter
/// with negative lobes can give a name a share outside it. So each coverage
/// is first clipped to `[0, 1 − c]`, `c` the clipped coverage in front of
/// it, and a sample with no transparency left in front of it keeps no colour
/// either: a reader decodes exactly what was encoded, never through a
/// stand-in denominator it would not use.
pub fn over_form(plain: &[[f32; 4]]) -> Vec<[f32; 4]> {
    let mut covered = 0.0f32;
    plain
        .iter()
        .map(|v| {
            let left = 1.0 - covered;
            if left <= 0.0 {
                return [0.0; 4];
            }
            let a = v[3].clamp(0.0, left);
            covered += a;
            [v[0] / left, v[1] / left, v[2] / left, a / left]
        })
        .collect()
}

/// Writes `product`'s matte to `path`, creating its parent directories, and
/// returns the channel names written. The header carries what every product
/// does — `software`, the working space's `colorInteropID` (the `R/G/B`
/// slices are colour), the product's `driver:parameters`, the sampling
/// stamp and an interrupted render's status — besides OpenEXRId's two.
pub fn write_matte(
    path: &Path,
    product: &AovProduct,
    beauty: &Buffer,
    film: &AovFilm,
    color: &super::OutputColor,
    sampling: &SamplingStamp,
    interrupted: bool,
) -> io::Result<Vec<String>> {
    let matte = film.matte(product).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: the render gathered no matte for it", product.prim_path),
        )
    })?;
    let with_color = matte.key().color;
    let (deep, names) = matte_channels(&matte)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let (position, display_window) = exr_windows(beauty);
    let mut attributes = product_attributes(product, color, sampling, interrupted, position);
    attributes.other.insert(
        Text::from(VERSION_ATTRIBUTE),
        AttributeValue::I32(EXRID_VERSION),
    );
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let packed = Text::new_or_none(encode_names(&names)?).expect("base64 is ASCII");
    attributes
        .other
        .insert(Text::from(NAMES_ATTRIBUTE), AttributeValue::Text(packed));
    let mut image = Image::from_encoded_channels(
        matte.dimensions(),
        Encoding {
            compression: Compression::ZIP1,
            blocks: Blocks::ScanLines,
            line_order: LineOrder::Increasing,
        },
        deep,
    );
    image.layer_data.attributes = attributes;
    image.attributes.display_window = display_window;
    image.attributes.chromaticities = color.exr_chromaticities();
    image
        .write()
        .to_file(path)
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;
    Ok(channel_names(with_color))
}

/// The matte's deep channels, rows top-down as an EXR stores them, and the
/// file's name table: each id a sample carries indexes it.
fn matte_channels(matte: &MatteView) -> io::Result<(DeepChannels, Vec<String>)> {
    let (w, h) = matte.dimensions();
    // The names the samples use, sorted: their index is the file's id.
    let mut table: BTreeMap<String, u32> = BTreeMap::new();
    for y in 0..h {
        for x in 0..w {
            for s in matte.pixel(x, y) {
                table.entry(matte.name(s.name).to_owned()).or_insert(0);
            }
        }
    }
    for (i, id) in table.values_mut().enumerate() {
        *id = i as u32;
    }
    let color = matte.key().color;
    let mut counts = Vec::with_capacity(w * h);
    let (mut ids, mut zs) = (Vec::new(), Vec::new());
    let mut slices: [Vec<f16>; 4] = Default::default();
    for y in 0..h {
        for x in 0..w {
            let samples = matte.pixel(x, y);
            counts.push(samples.len());
            let plain: Vec<[f32; 4]> = samples.iter().map(|s| s.value).collect();
            for (s, v) in samples.iter().zip(over_form(&plain)) {
                ids.push(table[matte.name(s.name)]);
                zs.push(s.z);
                for (slice, x) in slices.iter_mut().zip(v) {
                    slice.push(f16::from_f32(x));
                }
            }
        }
    }
    let [r, g, b, a] = slices;
    let mut channels = vec![
        AnyChannel::new("Id", FlatSamples::U32(ids)),
        AnyChannel::new("Z", FlatSamples::F32(zs)),
        AnyChannel::new("A", FlatSamples::F16(a)),
    ];
    if color {
        channels.push(AnyChannel::new("R", FlatSamples::F16(r)));
        channels.push(AnyChannel::new("G", FlatSamples::F16(g)));
        channels.push(AnyChannel::new("B", FlatSamples::F16(b)));
    }
    let deep = DeepChannels::new((w, h), counts, channels)
        .map_err(|e| io::Error::other(format!("a malformed matte: {e}")))?;
    Ok((deep, table.into_keys().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OpenEXRId's `Mask::read`, which undoes the over form: each sample
    /// times the transparency of the samples in front of it.
    fn plain_form(over: &[[f32; 4]]) -> Vec<[f32; 4]> {
        let mut prev_alpha = 0.0f32;
        over.iter()
            .map(|v| {
                let out = v.map(|x| (1.0 - prev_alpha) * x);
                prev_alpha += (1.0 - prev_alpha) * v[3];
                out
            })
            .collect()
    }

    #[test]
    fn the_over_form_round_trips_through_openexrids_reader() {
        let plain = [
            [0.1, 0.2, 0.0, 0.25],
            [0.3, 0.0, 0.1, 0.5],
            [0.0, 0.05, 0.2, 0.25],
        ];
        let over = over_form(&plain);
        // The front sample is as it is; the last one sees only the quarter
        // the first two leave.
        assert_eq!(over[0], plain[0]);
        assert!((over[2][3] - 1.0).abs() < 1e-6, "{:?}", over[2]);
        for (got, want) in plain_form(&over).iter().zip(&plain) {
            for c in 0..4 {
                assert!((got[c] - want[c]).abs() < 1e-6, "{got:?} vs {want:?}");
            }
        }
    }

    /// Signed filter weights can give one name more than the whole pixel
    /// and the next a negative share: coverage is clipped to what the over
    /// form holds, and the reader decodes exactly the clipped shares — a
    /// sample behind full coverage decodes to nothing, never to a value the
    /// film did not hold.
    #[test]
    fn coverage_outside_the_unit_interval_round_trips_clipped() {
        let plain = [
            [0.5, 0.0, 0.0, 1.2],
            [0.0, 0.4, 0.0, 0.3],
            [0.1, 0.1, 0.1, -0.5],
        ];
        let decoded = plain_form(&over_form(&plain));
        let want = [[0.5, 0.0, 0.0, 1.0], [0.0; 4], [0.0; 4]];
        for (got, want) in decoded.iter().zip(want) {
            for c in 0..4 {
                assert!((got[c] - want[c]).abs() < 1e-6, "{decoded:?}");
            }
        }
        let decoded = plain_form(&over_form(&[[0.2, 0.0, 0.0, -0.25], [0.3, 0.3, 0.3, 0.75]]));
        let want = [[0.2, 0.0, 0.0, 0.0], [0.3, 0.3, 0.3, 0.75]];
        for (got, want) in decoded.iter().zip(want) {
            for c in 0..4 {
                assert!((got[c] - want[c]).abs() < 1e-6, "{decoded:?}");
            }
        }
    }

    #[test]
    fn names_are_newline_separated_zlib_in_base64() {
        use std::io::Read;
        let packed = encode_names(&["/World/a", "/World/b"]).expect("encodes");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(packed)
            .expect("base64");
        let mut text = String::new();
        flate2::read::ZlibDecoder::new(&bytes[..])
            .read_to_string(&mut text)
            .expect("zlib");
        assert_eq!(text, "/World/a\n/World/b");
    }
}
