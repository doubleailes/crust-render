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
        })
    }

    /// Decodes one tile, clipped to the level's bounds, in whichever payload
    /// the file's samples call for.
    ///
    /// The returned buffer holds `tile_size(index).0 * .1 * 3` components —
    /// **not** `tile_edge²·3`. Edge tiles are stored padded and come back cut,
    /// so the row stride is the tile's own width.
    ///
    /// Greyscale and RGBA sources are normalised to RGB here rather than in the
    /// sampler: the alternative is a per-texel branch on a hot path for a fact
    /// that is fixed at open time.
    fn read_one(&self, dec: &mut TiffReader, level: usize, index: u32) -> io::Result<TileData> {
        let level = level.min(self.levels.len() - 1);
        dec.seek_to_image(level)
            .map_err(|e| io::Error::other(format!("seek to level {level}: {e}")))?;
        let raw = dec
            .read_chunk(index)
            .map_err(|e| io::Error::other(format!("tile {index} of level {level}: {e}")))?;

        let (tw, th) = self.levels[level].tile_size(index, self.tile_edge);
        let texels = tw * th;
        match raw {
            DecodingResult::U8(v) => Ok(TileData::U8(to_rgb(&v, texels, index)?)),
            // 16-bit integer samples are narrowed to 8, matching what the
            // preload path does with them: the renderer converts `u8` through a
            // 256-entry table, and widening the payload to keep two more bits
            // of an LDR texture would halve how much texture the cache's byte
            // budget holds. Float samples are a different matter — they carry
            // values a `u8` cannot represent at all, which is the whole reason
            // `TileData` has a second variant.
            DecodingResult::U16(v) => {
                let narrowed: Vec<u8> = v.iter().map(|&s| (s >> 8) as u8).collect();
                Ok(TileData::U8(to_rgb(&narrowed, texels, index)?))
            }
            DecodingResult::F16(v) => Ok(TileData::Half(to_rgb(&v, texels, index)?)),
            DecodingResult::F32(v) => {
                let halved: Vec<f16> = v.iter().map(|&s| f16::from_f32(s)).collect();
                Ok(TileData::Half(to_rgb(&halved, texels, index)?))
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

/// Normalises an interleaved tile to exactly three components a texel.
///
/// Greyscale replicates and anything wider (RGBA, RGB + extra samples) keeps
/// its first three channels, which is the convention the preload path already
/// follows.
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
        // RGBA drops alpha, as the preload path does.
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
}
