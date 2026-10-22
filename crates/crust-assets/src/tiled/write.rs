//! Writing a tiled, mip-mapped, Deflate-compressed TIFF — a `.tx` as OIIO's
//! `maketx` writes one for an 8- or 16-bit source (Arnold 7.5's auto-tx).
//!
//! **Why this is hand-rolled on top of `DirectoryEncoder`.** The `tiff` crate's
//! encoder is strips-only; tiled output has been an open request since 2023
//! (image-tiff#205) and is absent from the current release. But only the
//! *tiling* is missing, not the container: `DirectoryEncoder` still gives the
//! file header, the IFD chain and its back-patching, 12-byte entry
//! serialisation, the inline-versus-offset rule for values over four bytes, and
//! ascending tag order (its `Directory` is a `BTreeMap`, which TIFF6 requires).
//! So the writer here supplies three things and borrows the rest: the tile
//! grid, the per-tile zlib stream, and the tile tags.
//!
//! **The compression must be ours.** `tiff` applies its own only inside the
//! bulk strip writer, and `TiffWriter::set_compression` is private — driving
//! the encoder tile-wise with `with_compression(Deflate)` set would emit
//! *uncompressed* bytes under a tag claiming Deflate, which is a corrupt file
//! that reads as garbage. Deflating each tile here and leaving the encoder in
//! its default uncompressed mode is not a workaround, it is the only correct
//! arrangement.
//!
//! **The tags are OIIO's.** Arnold keeps a `.tx` only when it reads as one it
//! wrote, so the file carries what `maketx` writes and where it writes it
//! (measured on Arnold 7.5's own files, `tests/data/arnold_tx`): the OIIO
//! attributes in `ImageDescription`, the texture format, wrap modes and
//! `fovcot` in Pixar's private tags, an Exif directory saying sRGB, the
//! horizontal-differencing predictor, associated alpha, and no
//! `NewSubfileType` on the levels.

use super::maketx::{Image, SampleType, quantize_u8, quantize_u16};
use std::io::{self, Write};
use std::path::Path;
use tiff::tags::Tag;

/// Tile edge, in texels. 64 is OIIO's default and therefore what the `.tx`
/// files a studio already has are cut to.
///
/// TIFF6 requires tile dimensions to be a multiple of 16; 64 satisfies that and
/// keeps one RGB tile at 12 KiB, which is the granularity the cache evicts at.
pub const TILE_EDGE: usize = 64;

/// The OIIO metadata a `.tx` records, as Arnold's auto-tx writes it.
#[derive(Clone, Debug)]
pub struct TxMeta {
    /// `maketx`'s command line as Arnold runs it: `Software`, and on an EXR
    /// `Exif:ImageHistory` too.
    pub software: String,
    pub sha1: String,
    /// `oiio:AverageColor`, formatted.
    pub average: String,
    /// `oiio:ConstantColor`, formatted, for an image of one colour.
    pub constant: Option<String>,
    /// `DateTime` / `capDate`, `YYYY:MM:DD HH:MM:SS`.
    pub date: String,
}

/// Writes `levels` (level 0 first, each a level of the same image) as a
/// tiled TIFF of `depth` samples, 8- or 16-bit, with `meta`.
pub(crate) fn write_tiff(
    path: &Path,
    levels: &[Image],
    depth: SampleType,
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
    let file = std::fs::File::create(path)?;
    let mut enc = tiff::encoder::TiffEncoder::new(io::BufWriter::new(file))
        .map_err(|e| io::Error::other(format!("tiff header: {e}")))?;

    // The Exif directory first, so level 0's IFD can point at it: one entry,
    // `ColorSpace = 1`, which OIIO reads back as `oiio:ColorSpace "sRGB"`.
    let exif = {
        let mut exif = enc
            .extra_directory()
            .map_err(|e| io::Error::other(format!("tiff exif directory: {e}")))?;
        exif.write_tag(Tag::Unknown(40961), 1u16)
            .map_err(|e| io::Error::other(format!("tiff exif ColorSpace: {e}")))?;
        exif.finish_with_offsets()
            .map_err(|e| io::Error::other(format!("tiff exif directory: {e}")))?
    };
    let exif = tiff::encoder::Ifd(
        u32::try_from(exif.pointer.0)
            .map_err(|_| io::Error::other("tiff exif directory past 4 GiB"))?,
    );

    for (n, level) in levels.iter().enumerate() {
        let samples = match depth {
            SampleType::U8 => Samples::U8(level.pixels.iter().map(|&v| quantize_u8(v)).collect()),
            SampleType::U16 => {
                Samples::U16(level.pixels.iter().map(|&v| quantize_u16(v)).collect())
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("a TIFF .tx holds 8- or 16-bit samples, not {other:?}"),
                ));
            }
        };
        write_level(&mut enc, level, &samples, n == 0, meta, &exif)?;
    }
    Ok(())
}

/// One level's quantised samples.
enum Samples {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

/// Horizontal differencing (`Predictor = 2`) of one row of `n`-sample
/// texels, in place: each sample less the one a texel to its left.
fn difference<T: Copy + std::ops::Sub<Output = T>>(row: &mut [T], n: usize, sub: fn(T, T) -> T) {
    for x in (1..row.len() / n).rev() {
        for c in 0..n {
            row[x * n + c] = sub(row[x * n + c], row[(x - 1) * n + c]);
        }
    }
}

/// One mip level as one IFD: every tile's compressed bytes, then the tags
/// that describe them. Level 0 also carries the file's metadata.
fn write_level<W: Write + io::Seek>(
    enc: &mut tiff::encoder::TiffEncoder<W>,
    level: &Image,
    samples: &Samples,
    first: bool,
    meta: &TxMeta,
    exif: &tiff::encoder::Ifd,
) -> io::Result<()> {
    let (w, h, n) = (level.width, level.height, level.nchannels());
    let across = w.div_ceil(TILE_EDGE);
    let down = h.div_ceil(TILE_EDGE);
    let bits: u16 = match samples {
        Samples::U8(_) => 8,
        Samples::U16(_) => 16,
    };

    let mut dir = enc
        .image_directory()
        .map_err(|e| io::Error::other(format!("tiff directory: {e}")))?;

    let mut offsets = Vec::with_capacity(across * down);
    let mut counts = Vec::with_capacity(across * down);
    for ty in 0..down {
        for tx in 0..across {
            let tile = match samples {
                Samples::U8(s) => tile_bytes(s, w, h, n, tx, ty, |row| {
                    difference(row, n, u8::wrapping_sub);
                    row.to_vec()
                }),
                Samples::U16(s) => tile_bytes(s, w, h, n, tx, ty, |row| {
                    difference(row, n, u16::wrapping_sub);
                    row.iter().flat_map(|v| v.to_le_bytes()).collect()
                }),
            };
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(&tile)?;
            let packed = z.finish()?;
            // `write_data` returns the offset it placed the bytes at, which is
            // the whole reason this can be done through the public API instead
            // of tracking file positions by hand.
            let at = dir
                .write_data(&packed[..])
                .map_err(|e| io::Error::other(format!("tile data: {e}")))?;
            offsets.push(at as u32);
            counts.push(packed.len() as u32);
        }
    }

    let err = |what: &'static str| {
        move |e: tiff::TiffError| io::Error::other(format!("tiff {what}: {e}"))
    };
    let mut tag = |t: Tag, v: &[u32]| -> io::Result<()> {
        if v.len() == 1 {
            dir.write_tag(t, v[0])
        } else {
            dir.write_tag(t, v)
        }
        .map_err(|e| io::Error::other(format!("tiff tag {t:?}: {e}")))
    };
    tag(Tag::ImageWidth, &[w as u32])?;
    tag(Tag::ImageLength, &[h as u32])?;
    // Emit *only* tile tags. `tiff` 0.11.3 matches on the whole
    // (StripByteCounts, StripOffsets, TileByteCounts, TileOffsets) tuple and
    // rejects any mix with `StripTileTagConflict`, so a stray RowsPerStrip
    // would make the file unreadable rather than merely redundant.
    tag(Tag::TileWidth, &[TILE_EDGE as u32])?;
    tag(Tag::TileLength, &[TILE_EDGE as u32])?;
    tag(Tag::TileOffsets, &offsets)?;
    tag(Tag::TileByteCounts, &counts)?;
    // BitsPerSample is per-channel and defaults to 1 (bilevel) when absent —
    // always written, never inferred.
    dir.write_tag(Tag::BitsPerSample, &[bits; 4][..n])
        .map_err(err("BitsPerSample"))?;
    dir.write_tag(Tag::SamplesPerPixel, n as u16)
        .map_err(err("SamplesPerPixel"))?;
    if level.alpha().is_some() {
        // Associated alpha: the colour is stored premultiplied, as `maketx`
        // stores it.
        dir.write_tag(Tag::ExtraSamples, 1u16)
            .map_err(err("ExtraSamples"))?;
    }
    // RGB, or BlackIsZero for a `Y` image.
    let photometric: u16 = if level.colour() == 1 { 1 } else { 2 };
    dir.write_tag(Tag::PhotometricInterpretation, photometric)
        .map_err(err("Photometric"))?;
    dir.write_tag(Tag::Compression, 8u16) // Adobe Deflate
        .map_err(err("Compression"))?;
    dir.write_tag(Tag::Predictor, 2u16) // horizontal differencing
        .map_err(err("Predictor"))?;
    dir.write_tag(Tag::PlanarConfiguration, 1u16) // chunky
        .map_err(err("PlanarConfiguration"))?;
    dir.write_tag(Tag::SampleFormat, &[1u16; 4][..n]) // unsigned integer
        .map_err(err("SampleFormat"))?;
    dir.write_tag(Tag::Orientation, 1u16)
        .map_err(err("Orientation"))?;
    dir.write_tag(Tag::DateTime, meta.date.as_str())
        .map_err(err("DateTime"))?;
    if first {
        let mut description = format!("oiio:SHA-1={}", meta.sha1);
        if let Some(c) = &meta.constant {
            description.push_str(&format!(" oiio:ConstantColor={c}"));
        }
        description.push_str(&format!(" oiio:AverageColor={}", meta.average));
        dir.write_tag(Tag::ImageDescription, description.as_str())
            .map_err(err("ImageDescription"))?;
        dir.write_tag(Tag::Software, meta.software.as_str())
            .map_err(err("Software"))?;
        // OIIO's texture attributes, in Pixar's private tags.
        dir.write_tag(Tag::Unknown(33302), "Plain Texture")
            .map_err(err("textureformat"))?;
        dir.write_tag(Tag::Unknown(33303), "black,black")
            .map_err(err("wrapmodes"))?;
        dir.write_tag(Tag::Unknown(33304), w as f32 / h as f32)
            .map_err(err("fovcot"))?;
        dir.write_tag(Tag::ExifDirectory, exif)
            .map_err(err("ExifDirectory"))?;
    }
    dir.finish()
        .map_err(|e| io::Error::other(format!("tiff directory finish: {e}")))?;
    Ok(())
}

/// Tile `(tx, ty)` of a `w x h` image of `n` samples a texel, padded with
/// zeros to the full tile edge — TIFF6 sizes every tile `TileWidth x
/// TileLength`, and readers size their buffers from the tags — each row run
/// through `row` (the predictor, then the bytes).
fn tile_bytes<T: Copy + Default>(
    samples: &[T],
    w: usize,
    h: usize,
    n: usize,
    tx: usize,
    ty: usize,
    mut row: impl FnMut(&mut [T]) -> Vec<u8>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut line = vec![T::default(); TILE_EDGE * n];
    for ly in 0..TILE_EDGE {
        line.fill(T::default());
        let sy = ty * TILE_EDGE + ly;
        if sy < h {
            let copy = TILE_EDGE.min(w - tx * TILE_EDGE);
            let s = (sy * w + tx * TILE_EDGE) * n;
            line[..copy * n].copy_from_slice(&samples[s..s + copy * n]);
        }
        out.extend(row(&mut line));
    }
    out
}

/// The current time as `YYYY:MM:DD HH:MM:SS`, UTC: what `DateTime` and
/// `capDate` hold.
pub(crate) fn now_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}:{month:02}:{day:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}
