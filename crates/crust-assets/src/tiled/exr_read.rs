//! The OpenEXR backing: one tile of one mip level, without decoding the rest.
//!
//! **Why EXR is here at all.** The TIFF path is 8-bit end to end, which is
//! right for the authored albedo it was built for and destroys anything that is
//! genuinely high dynamic range. OIIO's own answer to that is EXR — `maketx
//! --format exr` writes 64x64-tiled, full-MIPMAP, zipped `half` — V-Ray's
//! native streaming texture format is tiled mip EXR, and Karma recommends the
//! same. So this is the format's second backing rather than a crust invention.
//!
//! **Why the low-level block API and not `read()`.** The `exr` crate's readers
//! are built to materialise a whole image (or a whole level), and
//! `filter_chunks` — the one entry point that looks like random access —
//! *consumes* the reader and sorts the offsets, which is the opposite of what a
//! cache needs. Everything under it is public, though, and composes into exactly
//! one tile: read the metadata once, read the offset table once, and then per
//! tile seek → `Chunk::read` → `UncompressedBlock::decompress_chunk` →
//! `lines()`. That is the same shape as the TIFF backing's `read_chunk`, which
//! is why both fit behind one trait.
//!
//! **The one sharp edge is finding the offset table.** `MetaData` is read
//! through a `PeekRead`, which may hold one byte it has read but not handed
//! back, so the reader's position afterwards can be one byte past the end of
//! the header. The table is self-describing — the chunks start immediately
//! after it, so its smallest entry must equal its own end — and that identity
//! is *probed* rather than assumed, which turns a silent one-byte skew into
//! either the right answer or a clean refusal.

use super::cache::TileData;
use super::{Backend, LevelInfo, TileReader};
use exr::block::UncompressedBlock;
use exr::block::chunk::Chunk;
use exr::meta::BlockDescription;
use exr::meta::attribute::{AttributeValue, LevelMode, SampleType};
use exr::meta::header::Header;
use exr::meta::{MetaData, mip_map_levels};
use half::f16;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The immutable geometry of one tiled, mip-mapped EXR.
#[derive(Debug)]
pub struct ExrFile {
    path: PathBuf,
    levels: Vec<LevelInfo>,
    tile_edge: usize,
    mip_space: Option<String>,
    /// Read once; every tile read needs it to size and decompress its block.
    meta: MetaData,
    /// Chunk index → byte offset, as the file's own offset table gives it.
    offsets: Vec<u64>,
    /// `[level][tile index]` → chunk index. EXR does not require any particular
    /// chunk order, so this is built from the crate's own block enumeration
    /// rather than assumed to be row-major-by-level.
    chunk_of: Vec<Vec<u32>>,
    /// Which entries of the channel list are R, G and B. Resolved once, because
    /// EXR stores channels in alphabetical order (so an RGB file arrives as
    /// B, G, R) and re-deriving that per texel would be absurd.
    rgb: [usize; 3],
}

impl ExrFile {
    pub fn open(path: &Path) -> io::Result<ExrFile> {
        let mut file = BufReader::new(File::open(path)?);
        let meta = MetaData::read_from_buffered(&mut file, false).map_err(exr_err)?;

        // One layer only. A texture is a single image, and allowing several
        // would mean carrying a layer index through every key in the cache for
        // a case no texture pipeline produces.
        if meta.headers.len() != 1 {
            return Err(invalid(format!(
                "{} layers — a texture must have exactly one",
                meta.headers.len()
            )));
        }
        let header = &meta.headers[0];
        if header.deep {
            return Err(invalid("deep data is not a texture"));
        }

        let BlockDescription::Tiles(tiles) = header.blocks else {
            return Err(invalid(
                "not a tiled EXR — run it through `maketx` or crust's own converter",
            ));
        };
        match tiles.level_mode {
            LevelMode::MipMap => {}
            LevelMode::Singular => return Err(invalid("tiled but not mip-mapped")),
            // A ripmap is anisotropic and would need a two-dimensional level
            // index; the sampler above is isotropic, so serving one would mean
            // silently reading its square levels only.
            LevelMode::RipMap => return Err(invalid("ripmap levels are not supported")),
        }
        let (tw, th) = (tiles.tile_size.width(), tiles.tile_size.height());
        if tw == 0 || tw != th {
            return Err(invalid(format!(
                "unexpected tile shape {tw}x{th} — square tiles only"
            )));
        }
        let tile_edge = tw;
        let rgb = resolve_rgb(header)?;

        let levels: Vec<LevelInfo> = mip_map_levels(tiles.rounding_mode, header.layer_size)
            .map(|(_, size)| LevelInfo {
                width: size.width(),
                height: size.height(),
                across: size.width().div_ceil(tile_edge),
                down: size.height().div_ceil(tile_edge),
            })
            .collect();
        if levels.is_empty() {
            return Err(invalid("no readable levels"));
        }

        let offsets = read_offset_table(&mut file, header.chunk_count)?;
        let mut chunk_of: Vec<Vec<u32>> = levels
            .iter()
            .map(|l| vec![u32::MAX; l.across * l.down])
            .collect();
        for (chunk, block) in meta.enumerate_ordered_header_block_indices() {
            if block.layer != 0 {
                continue;
            }
            // A mip level's index is square by construction; a ripmap was
            // already refused above.
            let level = block.level.width();
            let Some(li) = levels.get(level) else {
                continue;
            };
            let (tx, ty) = (
                block.pixel_position.x() / tile_edge,
                block.pixel_position.y() / tile_edge,
            );
            if let Some(slot) = chunk_of[level].get_mut(ty * li.across + tx) {
                *slot = chunk as u32;
            }
        }
        if chunk_of
            .iter()
            .any(|l| l.iter().any(|&c| c as usize >= offsets.len()))
        {
            return Err(invalid("the tile grid has holes — truncated or malformed"));
        }

        let mip_space = header
            .own_attributes
            .other
            .iter()
            .chain(header.shared_attributes.other.iter())
            .find(|(k, _)| *k == MIP_SPACE_KEY)
            .and_then(|(_, v)| match v {
                AttributeValue::Text(t) => Some(t.to_string()),
                _ => None,
            });

        Ok(ExrFile {
            path: path.to_path_buf(),
            levels,
            tile_edge,
            mip_space,
            meta,
            offsets,
            chunk_of,
            rgb,
        })
    }

    fn read_one(
        &self,
        file: &mut BufReader<File>,
        level: usize,
        index: u32,
    ) -> io::Result<TileData> {
        let level = level.min(self.levels.len() - 1);
        let li = self.levels[level];
        let chunk_index = *self
            .chunk_of
            .get(level)
            .and_then(|l| l.get(index as usize))
            .ok_or_else(|| invalid(format!("tile {index} is outside level {level}")))?;
        let offset = self.offsets[chunk_index as usize];

        file.seek(SeekFrom::Start(offset))?;
        let chunk = Chunk::read(file, &self.meta).map_err(exr_err)?;
        let block =
            UncompressedBlock::decompress_chunk(chunk, &self.meta, false).map_err(exr_err)?;

        let (tw, th) = li.tile_size(index, self.tile_edge);
        if block.index.pixel_size.width() != tw || block.index.pixel_size.height() != th {
            return Err(invalid(format!(
                "tile {index} of level {level} is {}x{}, expected {tw}x{th}",
                block.index.pixel_size.width(),
                block.index.pixel_size.height()
            )));
        }
        let origin = block.index.pixel_position;

        let header = &self.meta.headers[0];
        let mut out = vec![f16::ZERO; tw * th * 3];
        for line in block.lines(&header.channels) {
            // A block holds every channel of every one of its rows, so most of
            // what it yields is a channel this texture does not use — an alpha,
            // a data pass, a depth. Skipping early is what keeps an RGBA file
            // from costing a third more than an RGB one.
            // Which output slots this channel feeds — usually exactly one, but
            // a single-channel file maps its one channel to all three, which is
            // how a mask bound as a colour comes back grey instead of red.
            let slots = self.rgb.map(|c| c == line.location.channel);
            if !slots.iter().any(|&b| b) {
                continue;
            }
            let sample_type = header.channels.list[line.location.channel].sample_type;
            let row = line.location.position.y().saturating_sub(origin.y());
            let start = line.location.position.x().saturating_sub(origin.x());
            if row >= th {
                continue;
            }
            let count = line.location.sample_count.min(tw.saturating_sub(start));
            // Native endian: `decompress_chunk` converts the file's
            // little-endian samples on the way out, so this is a reinterpret
            // rather than a byte swap.
            for i in 0..count {
                let v = match sample_type {
                    SampleType::F16 => {
                        let b = line.value.get(i * 2..i * 2 + 2).unwrap_or(&[0, 0]);
                        f16::from_bits(u16::from_ne_bytes([b[0], b[1]]))
                    }
                    SampleType::F32 => {
                        let b = line.value.get(i * 4..i * 4 + 4).unwrap_or(&[0; 4]);
                        f16::from_f32(f32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
                    }
                    SampleType::U32 => {
                        let b = line.value.get(i * 4..i * 4 + 4).unwrap_or(&[0; 4]);
                        f16::from_f32(u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as f32)
                    }
                };
                let o = (row * tw + start + i) * 3;
                for (k, &wanted) in slots.iter().enumerate() {
                    if wanted {
                        out[o + k] = v;
                    }
                }
            }
        }
        Ok(TileData::Half(out))
    }
}

impl Backend for ExrFile {
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
        true
    }

    fn reader(&self) -> io::Result<TileReader> {
        Ok(TileReader::Exr(BufReader::new(File::open(&self.path)?)))
    }

    fn read_tile(&self, r: &mut TileReader, level: usize, index: u32) -> io::Result<TileData> {
        match r {
            TileReader::Exr(file) => self.read_one(file, level, index),
            TileReader::Tiff(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a TIFF cursor was handed to the EXR backing",
            )),
        }
    }
}

/// The attribute naming the colour space a file is meant to be bound with.
///
/// EXR takes arbitrary typed header attributes, so this is a first-class field
/// rather than a string smuggled through `ImageDescription` the way the TIFF
/// backing has to do it.
pub(crate) const MIP_SPACE_KEY: &str = "crust:mipspace";

/// Which entries of the channel list carry R, G and B.
///
/// A single-channel file (a mask, a displacement) replicates its one channel,
/// matching what the TIFF backing does with greyscale.
fn resolve_rgb(header: &Header) -> io::Result<[usize; 3]> {
    let find = |want: &str| {
        header
            .channels
            .list
            .iter()
            .position(|c| c.name.to_string().eq_ignore_ascii_case(want))
    };
    match (find("R"), find("G"), find("B")) {
        (Some(r), Some(g), Some(b)) => Ok([r, g, b]),
        _ => {
            if header.channels.list.len() == 1 {
                Ok([0, 0, 0])
            } else {
                let names: Vec<String> = header
                    .channels
                    .list
                    .iter()
                    .map(|c| c.name.to_string())
                    .collect();
                Err(invalid(format!("no R/G/B channels, found {names:?}")))
            }
        }
    }
}

/// Reads the chunk offset table, finding its start by the identity it has to
/// satisfy rather than by trusting the reader's position.
///
/// `MetaData::read_from_buffered` wraps the reader in a `PeekRead`, which holds
/// at most one byte it has consumed but not returned, so the position after the
/// header is either exactly the table's start or one byte past it. The chunks
/// begin immediately after the table, so the table's smallest entry equals its
/// own end — an identity that picks between the two candidates outright and
/// refuses a file that satisfies neither.
fn read_offset_table(file: &mut BufReader<File>, chunk_count: usize) -> io::Result<Vec<u64>> {
    if chunk_count == 0 {
        return Err(invalid("no chunks"));
    }
    let here = file.stream_position()?;
    let span = 8 * chunk_count as u64;
    let mut last = None;
    for candidate in [here, here.saturating_sub(1)] {
        file.seek(SeekFrom::Start(candidate))?;
        let mut bytes = vec![0u8; span as usize];
        match file.read_exact(&mut bytes) {
            Ok(()) => {}
            Err(e) => {
                last = Some(e);
                continue;
            }
        }
        let offsets: Vec<u64> = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .copied()
            .map(u64::from_le_bytes)
            .collect();
        if offsets.iter().copied().min() == Some(candidate + span) {
            return Ok(offsets);
        }
        if candidate == here {
            continue;
        }
    }
    Err(match last {
        Some(e) => e,
        None => invalid("the chunk offset table is not where the header says it is"),
    })
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn exr_err(e: exr::error::Error) -> io::Error {
    io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiled::write_tx_exr;

    fn fixture(name: &str, w: usize, h: usize) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("crust_exrread_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("t.tx");
        let src: Vec<f32> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                [x * 0.5, y * 0.25, 3.0]
            })
            .collect();
        write_tx_exr(&path, &src, w, h, crust_core::ColorSpace::Raw).expect("write");
        path
    }

    /// The offset table is found by its own identity, not by the reader's
    /// position — which is the one thing about this backing that could be
    /// silently, subtly wrong.
    ///
    /// `MetaData::read_from_buffered` wraps the reader in a `PeekRead` that may
    /// hold one byte it has consumed but not returned, so the position on
    /// return is either the table's start or one byte past it. As it happens
    /// the current `exr` consumes its peeked byte and lands exactly right — so
    /// the fallback candidate is never taken in practice and would rot
    /// untested. This forces it: the same file, read from a position one byte
    /// late, must produce the same table.
    #[test]
    fn the_offset_table_is_found_even_when_the_reader_is_a_byte_late() {
        let path = fixture("probe", 200, 130);
        let mut file = BufReader::new(File::open(&path).expect("open"));
        let meta = MetaData::read_from_buffered(&mut file, false).expect("meta");
        let count = meta.headers[0].chunk_count;

        let exact = file.stream_position().expect("pos");
        let want = read_offset_table(&mut file, count).expect("table at the exact position");
        assert_eq!(want.len(), count);

        // One byte late: the `here` candidate misses, `here - 1` hits.
        file.seek(SeekFrom::Start(exact + 1)).expect("seek");
        let late = read_offset_table(&mut file, count).expect("table one byte late");
        assert_eq!(late, want);

        // Far enough off and it declines rather than returning nonsense, which
        // is what matters: a wrong table reads whole tiles from the wrong
        // place and still decodes.
        file.seek(SeekFrom::Start(exact + 9)).expect("seek");
        assert!(read_offset_table(&mut file, count).is_err());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Everything this backing cannot serve is refused at `open`, not at the
    /// first sample: a texture that declines falls back to preloading, while
    /// one that fails mid-render takes a worker down — and `panic = "abort"`
    /// makes that the process.
    #[test]
    fn unusable_exr_files_decline_at_open() {
        let dir = std::env::temp_dir().join("crust_exrread_reject");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // A perfectly good scanline EXR — the common mistake of pointing the
        // streaming path at an unconverted asset.
        let flat = dir.join("flat.exr");
        exr::prelude::write_rgb_file(&flat, 32, 32, |x, y| {
            (x as f32 / 32.0, y as f32 / 32.0, 0.5)
        })
        .expect("write scanline");
        let err = ExrFile::open(&flat).expect_err("scanline must decline");
        assert!(err.to_string().contains("tiled"), "{err}");

        // Not an EXR at all.
        let junk = dir.join("junk.exr");
        std::fs::write(&junk, b"not an exr either").expect("write");
        assert!(ExrFile::open(&junk).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A single-channel file replicates, matching what the TIFF backing does
    /// with greyscale — a mask bound as a colour should be grey, not an error.
    #[test]
    fn a_single_channel_file_replicates_to_rgb() {
        let dir = std::env::temp_dir().join("crust_exrread_grey");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("grey.tx");

        // Written by hand rather than through `write_tx_exr`, which always
        // emits RGB — the point is a file crust did not write.
        use exr::prelude::{
            AnyChannel, AnyChannels, Blocks, Compression, Encoding, FlatSamples, Image, Layer,
            LayerAttributes, Levels, LineOrder, Vec2, WritableImage,
        };
        let (w, h) = (4usize, 4usize);
        let levels = vec![
            FlatSamples::F16((0..w * h).map(|i| f16::from_f32(i as f32)).collect()),
            FlatSamples::F16((0..4).map(|_| f16::from_f32(2.5)).collect()),
            FlatSamples::F16(vec![f16::from_f32(7.5)]),
        ];
        let channels = AnyChannels::sort(
            [AnyChannel::new(
                "Y",
                Levels::Mip {
                    rounding_mode: exr::math::RoundingMode::Up,
                    level_data: levels,
                },
            )]
            .into_iter()
            .collect(),
        );
        let layer = Layer::new(
            (w, h),
            LayerAttributes::default(),
            Encoding {
                compression: Compression::Uncompressed,
                blocks: Blocks::Tiles(Vec2(super::super::TILE_EDGE, super::super::TILE_EDGE)),
                line_order: LineOrder::Increasing,
            },
            channels,
        );
        Image::from_layer(layer)
            .write()
            .to_file(&path)
            .expect("write");

        let f = ExrFile::open(&path).expect("open");
        let mut r = f.reader().expect("reader");
        let tile = f.read_tile(&mut r, 0, 0).expect("tile");
        let got = tile.expect_half();
        assert_eq!(got.len(), w * h * 3);
        for i in 0..w * h {
            let v = f16::from_f32(i as f32);
            assert_eq!([got[i * 3], got[i * 3 + 1], got[i * 3 + 2]], [v, v, v]);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
