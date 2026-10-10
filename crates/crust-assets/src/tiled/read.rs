//! The TIFF backing: opening a `.tx` and pulling one tile out of it.
//!
//! The split of responsibility here is deliberate. [`TiffFile`] holds the
//! *geometry* of the file — how many levels, how big each is, how its tiles are
//! laid out — which is cheap, immutable and read once at open. The decoders
//! that actually touch the disk are handed in per call, because every `tiff`
//! read takes `&mut self` and a path tracer asks from every Rayon worker at
//! once: one shared decoder would serialise the whole render behind a single
//! lock. Keeping the geometry separate from the cursor is what lets the cache
//! pool cursors without duplicating the metadata.

use super::cache::TileData;
use super::{Backend, LevelInfo, TileReader};
use half::f16;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use tiff::decoder::{ChunkType, Decoder, DecodingResult};
use tiff::tags::Tag;

/// A `tiff` decoder positioned on some level of some file.
pub type TiffReader = Decoder<BufReader<File>>;

/// The immutable geometry of one tiled, mip-mapped TIFF.
#[derive(Clone, Debug)]
pub struct TiffFile {
    path: PathBuf,
    levels: Vec<LevelInfo>,
    tile_edge: usize,
    /// The colour space this file's mip chain was reduced in, as the writer
    /// recorded it, or `None` for a file that did not say (anything `maketx`
    /// produced).
    mip_space: Option<String>,
    /// Whether the samples are IEEE floats, and therefore already linear.
    /// Read once from `SampleFormat` rather than inferred per tile, because the
    /// load report wants it before any tile has been touched.
    float: bool,
    /// Which sample of a texel holds alpha — 1 after a grey sample, 3 after
    /// RGB — or `None` for a file without one. See [`alpha_sample`].
    alpha_at: Option<usize>,
}

impl TiffFile {
    /// Reads the header and every level's geometry, decoding no pixels.
    ///
    /// Everything this rejects, it rejects *here* rather than at the first
    /// sample, because a texture that declines to open falls back to a
    /// constant colour while one that fails mid-render takes down a worker —
    /// and `panic = "abort"` makes that the whole process.
    pub fn open(path: &Path) -> io::Result<TiffFile> {
        let mut dec = open_decoder(path)?;

        if dec.get_chunk_type() != ChunkType::Tile {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a tiled TIFF — run it through `maketx` or crust's own converter",
            ));
        }
        // `PlanarConfiguration = 2` panics inside `expand_chunk` on this
        // version of `tiff` (image-tiff#403). Refusing it is not caution about
        // an unsupported layout, it is refusing to hand a worker a panic.
        if let Ok(Some(planar)) = dec.find_tag(Tag::PlanarConfiguration)
            && planar.into_u16().ok() == Some(2)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "planar (PlanarConfiguration = 2) TIFFs are not supported",
            ));
        }

        // Read before seeking anywhere: this is level 0's IFD, which is where
        // the writer puts the provenance.
        let mip_space = dec
            .get_tag_ascii_string(Tag::ImageDescription)
            .ok()
            .and_then(|d| {
                d.split_whitespace()
                    .find_map(|f| f.strip_prefix("crust:mipspace=").map(str::to_owned))
            });

        // SampleFormat 3 is IEEE float. Absent means unsigned integer, which is
        // the TIFF6 default and what every 8-bit texture is.
        let float = dec
            .find_tag(Tag::SampleFormat)
            .ok()
            .flatten()
            .and_then(|v| v.into_u16_vec().ok())
            .is_some_and(|v| v.first() == Some(&3));

        let alpha_at = alpha_sample(&mut dec);

        let (tw, th) = dec.chunk_dimensions();
        if tw == 0 || th == 0 || tw != th {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected tile shape {tw}x{th} — square tiles only"),
            ));
        }
        let tile_edge = tw as usize;

        // Walk the IFD chain. Each level is an independent image as far as
        // `tiff` is concerned, so this is the only place the chain's *meaning*
        // — a mip pyramid — is imposed on it.
        let mut levels = Vec::new();
        let mut n = 0usize;
        loop {
            if dec.seek_to_image(n).is_err() {
                break;
            }
            let Ok((w, h)) = dec.dimensions() else { break };
            let (w, h) = (w as usize, h as usize);
            if w == 0 || h == 0 {
                break;
            }
            levels.push(LevelInfo {
                width: w,
                height: h,
                across: w.div_ceil(tile_edge),
                down: h.div_ceil(tile_edge),
            });
            // A pyramid ends at 1x1; anything further is a multi-page document
            // that happens to be tiled, and its extra pages are not mip levels.
            if w <= 1 && h <= 1 {
                break;
            }
            n += 1;
        }
        if levels.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no readable levels",
            ));
        }

        Ok(TiffFile {
            path: path.to_path_buf(),
            levels,
            tile_edge,
            mip_space,
            float,
            alpha_at,
        })
    }

    /// Decodes one tile, clipped to the level's bounds, in whichever payload
    /// the file's samples call for.
    ///
    /// The returned buffer holds `tile_size(index).0 * .1 * 3` components —
    /// **not** `tile_edge²·3`. Edge tiles are stored padded and come back cut,
    /// so the row stride is the tile's own width.
    ///
    /// Greyscale and wider sources are normalised to RGB — RGBA when the file
    /// carries alpha — here rather than in the sampler: the alternative is a
    /// per-texel branch on a hot path for a fact that is fixed at open time.
    fn read_one(&self, dec: &mut TiffReader, level: usize, index: u32) -> io::Result<TileData> {
        let level = level.min(self.levels.len() - 1);
        dec.seek_to_image(level)
            .map_err(|e| io::Error::other(format!("seek to level {level}: {e}")))?;
        let raw = dec
            .read_chunk(index)
            .map_err(|e| io::Error::other(format!("tile {index} of level {level}: {e}")))?;

        let (tw, th) = self.levels[level].tile_size(index, self.tile_edge);
        let texels = tw * th;
        let (alpha_at, alpha) = (self.alpha_at, self.alpha_at.is_some());
        match raw {
            DecodingResult::U8(v) => {
                Ok(TileData::u8(to_texels(&v, texels, index, alpha_at)?, alpha))
            }
            // 16-bit integer samples are narrowed to 8, matching what the
            // preload path does with them: the renderer converts `u8` through a
            // 256-entry table, and widening the payload to keep two more bits
            // of an LDR texture would halve how much texture the cache's byte
            // budget holds. Float samples are a different matter — they carry
            // values a `u8` cannot represent at all, which is the whole reason
            // `TileData` has a second variant.
            DecodingResult::U16(v) => {
                let narrowed: Vec<u8> = v.iter().map(|&s| (s >> 8) as u8).collect();
                Ok(TileData::u8(
                    to_texels(&narrowed, texels, index, alpha_at)?,
                    alpha,
                ))
            }
            DecodingResult::F16(v) => Ok(TileData::half(
                &to_texels(&v, texels, index, alpha_at)?,
                alpha,
            )),
            DecodingResult::F32(v) => {
                let halved: Vec<f16> = v.iter().map(|&s| f16::from_f32(s)).collect();
                Ok(TileData::half(
                    &to_texels(&halved, texels, index, alpha_at)?,
                    alpha,
                ))
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported sample layout: {other:?}"),
            )),
        }
    }
}

impl Backend for TiffFile {
    fn levels(&self) -> &[LevelInfo] {
        &self.levels
    }

    fn tile_edge(&self) -> usize {
        self.tile_edge
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn mip_space(&self) -> Option<&str> {
        self.mip_space.as_deref()
    }

    fn linear(&self) -> bool {
        self.float
    }

    fn alpha(&self) -> bool {
        self.alpha_at.is_some()
    }

    fn reader(&self) -> io::Result<TileReader> {
        Ok(TileReader::Tiff(Box::new(open_decoder(&self.path)?)))
    }

    fn read_tile(&self, r: &mut TileReader, level: usize, index: u32) -> io::Result<TileData> {
        match r {
            TileReader::Tiff(dec) => self.read_one(dec, level, index),
            // Unreachable by construction — a file's reader pool only ever
            // holds cursors it made itself — but an error beats a panic, which
            // `panic = "abort"` would make fatal to the render.
            TileReader::Exr(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "an EXR cursor was handed to the TIFF backing",
            )),
        }
    }
}

/// Normalises an interleaved tile to exactly three components a texel, for a
/// file without alpha.
///
/// Greyscale replicates and anything wider (RGB + extra samples that are not
/// alpha) keeps its first three channels, which is the convention the preload
/// path already follows. A file with alpha goes through [`to_texels`].
fn to_rgb<T: Copy + Default>(src: &[T], texels: usize, index: u32) -> io::Result<Vec<T>> {
    if src.len() == texels * 3 {
        return Ok(src.to_vec());
    }
    let channels = src.len().checked_div(texels.max(1)).unwrap_or(0);
    let mut out = vec![T::default(); texels * 3];
    match channels {
        1 => {
            for i in 0..texels {
                out[i * 3] = src[i];
                out[i * 3 + 1] = src[i];
                out[i * 3 + 2] = src[i];
            }
        }
        n if n >= 3 => {
            for i in 0..texels {
                out[i * 3..i * 3 + 3].copy_from_slice(&src[i * n..i * n + 3]);
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("tile {index} has {channels} channel(s)"),
            ));
        }
    }
    Ok(out)
}

/// Normalises an interleaved tile to RGB, or to RGBA when `alpha_at` names
/// the sample holding the file's alpha: the colour as [`to_rgb`] reads it (a
/// grey sample replicated, else the first three), the alpha after it.
fn to_texels<T: Copy + Default>(
    src: &[T],
    texels: usize,
    index: u32,
    alpha_at: Option<usize>,
) -> io::Result<Vec<T>> {
    let Some(a) = alpha_at else {
        return to_rgb(src, texels, index);
    };
    let channels = src.len().checked_div(texels.max(1)).unwrap_or(0);
    if !matches!(a, 1 | 3) || a >= channels || src.len() < texels * channels {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("tile {index} has {channels} channel(s), its alpha at {a}"),
        ));
    }
    let mut out = vec![T::default(); texels * 4];
    for (texel, s) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.chunks_exact(channels))
    {
        *texel = if a == 1 {
            [s[0], s[0], s[0], s[1]]
        } else {
            [s[0], s[1], s[2], s[3]]
        };
    }
    Ok(out)
}

/// Where a TIFF's alpha is: the first extra sample, when `ExtraSamples` names
/// it alpha — associated (1) or unassociated (2) — right after the colour's
/// one (grey) or three (RGB) samples. `None` for anything else, including an
/// unspecified extra sample (0), which is not coverage.
///
/// Associated alpha is read as alpha with the colour as stored: an OIIO
/// `maketx` file premultiplies by default, and its colour is not divided back
/// out (see the textures design record).
fn alpha_sample(dec: &mut TiffReader) -> Option<usize> {
    let u16s = |dec: &mut TiffReader, tag: Tag| {
        dec.find_tag(tag)
            .ok()
            .flatten()
            .and_then(|v| v.into_u16_vec().ok())
    };
    let extra = u16s(dec, Tag::ExtraSamples)?;
    if !matches!(extra.first(), Some(1 | 2)) {
        return None;
    }
    let samples = usize::from(*u16s(dec, Tag::SamplesPerPixel)?.first()?);
    let colour = samples.checked_sub(extra.len())?;
    matches!(colour, 1 | 3).then_some(colour)
}

fn open_decoder(path: &Path) -> io::Result<TiffReader> {
    let file = File::open(path)?;
    Decoder::new(BufReader::new(file))
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Channel normalisation is the one part of a tile read that is not the
    /// `tiff` crate's, and it runs over every payload type.
    ///
    /// The float arms exist for files crust does not write — OIIO's `maketx -d
    /// float` produces them, and `.tx` is a format crust reads as well as
    /// writes — so they are checked here rather than through a fixture no tool
    /// in this repository can produce.
    #[test]
    fn channel_normalisation_handles_grey_rgb_and_rgba() {
        // Greyscale replicates.
        assert_eq!(
            to_rgb(&[1u8, 2, 3], 3, 0).expect("grey"),
            vec![1, 1, 1, 2, 2, 2, 3, 3, 3]
        );
        // RGB passes straight through.
        let rgb = [9u8, 8, 7, 6, 5, 4];
        assert_eq!(to_rgb(&rgb, 2, 0).expect("rgb"), rgb.to_vec());
        // A fourth sample that is not alpha is dropped.
        assert_eq!(
            to_rgb(&[1u8, 2, 3, 255, 4, 5, 6, 0], 2, 0).expect("rgba"),
            vec![1, 2, 3, 4, 5, 6]
        );
        // Same rules for `half` samples, which is what a float file lands on.
        let h = |v: f32| f16::from_f32(v);
        assert_eq!(
            to_rgb(&[h(4.0), h(0.5)], 2, 0).expect("grey half"),
            vec![h(4.0), h(4.0), h(4.0), h(0.5), h(0.5), h(0.5)]
        );
        // Two channels is not something to guess at.
        assert!(to_rgb(&[1u8, 2, 3, 4], 2, 0).is_err());
    }

    /// With an alpha, the colour normalises by the same rules and the alpha
    /// lands fourth — after a replicated grey as after RGB — and a sample
    /// beyond it is dropped.
    #[test]
    fn alpha_normalisation_puts_the_alpha_after_the_colour() {
        assert_eq!(
            to_texels(&[1u8, 2, 3, 4, 5, 6, 7, 8], 2, 0, Some(3)).expect("rgba"),
            vec![1, 2, 3, 4, 5, 6, 7, 8]
        );
        assert_eq!(
            to_texels(&[9u8, 200, 10, 0], 2, 0, Some(1)).expect("grey + alpha"),
            vec![9, 9, 9, 200, 10, 10, 10, 0]
        );
        assert_eq!(
            to_texels(&[1u8, 2, 3, 4, 99], 1, 0, Some(3)).expect("rgba + extra"),
            vec![1, 2, 3, 4]
        );
        let h = |v: f32| f16::from_f32(v);
        assert_eq!(
            to_texels(&[h(4.0), h(0.25)], 1, 0, Some(1)).expect("half grey + alpha"),
            vec![h(4.0), h(4.0), h(4.0), h(0.25)]
        );
        // No alpha: exactly the RGB rules.
        assert_eq!(
            to_texels(&[1u8, 2, 3, 4], 1, 0, None).expect("rgb"),
            vec![1, 2, 3]
        );
        // An alpha the tile does not reach is refused, not read past.
        assert!(to_texels(&[1u8, 2, 3], 1, 0, Some(3)).is_err());
    }
}
