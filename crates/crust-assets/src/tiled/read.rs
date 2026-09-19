//! Opening a `.tx` and pulling one tile out of it.
//!
//! The split of responsibility here is deliberate. [`TiledFile`] holds the
//! *geometry* of the file — how many levels, how big each is, how its tiles are
//! laid out — which is cheap, immutable and read once at open. The decoders
//! that actually touch the disk are handed in per call, because every `tiff`
//! read takes `&mut self` and a path tracer asks from every Rayon worker at
//! once: one shared decoder would serialise the whole render behind a single
//! lock. Keeping the geometry separate from the cursor is what lets the cache
//! pool cursors without duplicating the metadata.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use tiff::decoder::{ChunkType, Decoder, DecodingResult};
use tiff::tags::Tag;

/// A decoder positioned on some level of some file — the mutable half.
pub type TileReader = Decoder<BufReader<File>>;

/// One mip level's geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelInfo {
    pub width: usize,
    pub height: usize,
    /// Tiles across and down. Stored rather than recomputed because the tile
    /// index a sampler asks for is `ty * across + tx`, and getting `across`
    /// wrong silently reads a tile from the wrong row.
    pub across: usize,
    pub down: usize,
}

impl LevelInfo {
    /// The tile index containing texel `(x, y)`, and the texel's offset within
    /// that tile.
    #[inline]
    pub fn locate(&self, x: usize, y: usize, tile_edge: usize) -> (u32, usize, usize) {
        let (tx, ty) = (x / tile_edge, y / tile_edge);
        (
            (ty * self.across + tx) as u32,
            x - tx * tile_edge,
            y - ty * tile_edge,
        )
    }

    /// The clipped size of tile `index` — its right and bottom edges are cut to
    /// the level's bounds, which is how `tiff` hands them back.
    #[inline]
    pub fn tile_size(&self, index: u32, tile_edge: usize) -> (usize, usize) {
        let i = index as usize;
        let (tx, ty) = (i % self.across.max(1), i / self.across.max(1));
        (
            tile_edge.min(self.width.saturating_sub(tx * tile_edge)),
            tile_edge.min(self.height.saturating_sub(ty * tile_edge)),
        )
    }
}

/// The immutable geometry of one tiled, mip-mapped file.
#[derive(Clone, Debug)]
pub struct TiledFile {
    path: PathBuf,
    levels: Vec<LevelInfo>,
    tile_edge: usize,
}

impl TiledFile {
    /// Reads the header and every level's geometry, decoding no pixels.
    ///
    /// Everything this rejects, it rejects *here* rather than at the first
    /// sample, because a texture that declines to open falls back to a
    /// constant colour while one that fails mid-render takes down a worker —
    /// and `panic = "abort"` makes that the whole process.
    pub fn open(path: &Path) -> io::Result<TiledFile> {
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

        Ok(TiledFile {
            path: path.to_path_buf(),
            levels,
            tile_edge,
        })
    }

    pub fn levels(&self) -> &[LevelInfo] {
        &self.levels
    }

    pub fn level(&self, n: usize) -> LevelInfo {
        self.levels[n.min(self.levels.len() - 1)]
    }

    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    pub fn tile_edge(&self) -> usize {
        self.tile_edge
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A fresh cursor onto this file. The cache keeps a small pool of these;
    /// they are not shareable, so one per concurrent miss is the floor.
    pub fn reader(&self) -> io::Result<TileReader> {
        open_decoder(&self.path)
    }

    /// Decodes one tile, as `u8` RGB, clipped to the level's bounds.
    ///
    /// The returned buffer is `tile_size(index).0 * .1 * 3` bytes — **not**
    /// `tile_edge²·3`. Edge tiles are stored padded and come back cut, so the
    /// row stride is the tile's own width.
    ///
    /// Greyscale and RGBA sources are normalised to RGB here rather than in the
    /// sampler: the alternative is a per-texel branch on a hot path for a fact
    /// that is fixed at open time.
    pub fn read_tile(&self, dec: &mut TileReader, level: usize, index: u32) -> io::Result<Vec<u8>> {
        let level = level.min(self.levels.len() - 1);
        dec.seek_to_image(level)
            .map_err(|e| io::Error::other(format!("seek to level {level}: {e}")))?;
        let raw = dec
            .read_chunk(index)
            .map_err(|e| io::Error::other(format!("tile {index} of level {level}: {e}")))?;

        let (tw, th) = self.levels[level].tile_size(index, self.tile_edge);
        let texels = tw * th;
        let bytes = match raw {
            DecodingResult::U8(v) => v,
            // 16-bit and float sources are accepted but flattened to 8 bits,
            // matching what the preload path does with them. The cache exists
            // to bound memory; keeping four bytes a channel for data the
            // renderer converts through a 256-entry table anyway would work
            // against that.
            DecodingResult::U16(v) => v.iter().map(|&s| (s >> 8) as u8).collect(),
            DecodingResult::F32(v) => v
                .iter()
                .map(|&s| (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
                .collect(),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported sample layout: {other:?}"),
                ));
            }
        };

        if bytes.len() == texels * 3 {
            return Ok(bytes);
        }
        let channels = bytes.len().checked_div(texels).unwrap_or(0);
        let mut out = vec![0u8; texels * 3];
        match channels {
            1 => {
                for i in 0..texels {
                    out[i * 3] = bytes[i];
                    out[i * 3 + 1] = bytes[i];
                    out[i * 3 + 2] = bytes[i];
                }
            }
            n if n >= 3 => {
                for i in 0..texels {
                    out[i * 3..i * 3 + 3].copy_from_slice(&bytes[i * n..i * n + 3]);
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
}

fn open_decoder(path: &Path) -> io::Result<TileReader> {
    let file = File::open(path)?;
    Decoder::new(BufReader::new(file))
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))
}
