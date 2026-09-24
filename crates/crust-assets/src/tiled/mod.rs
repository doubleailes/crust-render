//! Tiled, mip-mapped textures — the `.tx` files a streaming cache reads from.
//!
//! **Why a separate format at all.** Everything else in this crate decodes a
//! whole image at load and holds it resident, which makes memory scale with
//! the scene's total texture footprint and forces the `CRUST_TEX_MAX` cap to
//! keep that survivable. A cap is a poor substitute for a residency policy: it
//! discards authored detail permanently, and it still cannot help a scene that
//! binds more texture than fits. The production answer — Arnold, Guerilla,
//! RenderMan, all through OpenImageIO — is to convert once, offline, to a
//! tiled and mip-mapped file, then stream individual tiles on demand behind a
//! bounded cache. Memory then scales with the *cache*, not with the scene.
//!
//! **`.tx` is not a new container.** `maketx`/`oiiotool -otex` guess the format
//! from the extension and fall back to TIFF, so a `.tx` is a plain TIFF that
//! happens to be tiled (64x64 by convention), carries each mip level as a
//! further IFD, and compresses with Deflate. Files crust writes are readable by
//! OIIO and files `maketx` writes are readable here; nothing is proprietary.
//!
//! **Two backings, one seam.** TIFF cannot be the only one, because the whole
//! 8-bit path clips: a texture that is genuinely HDR — an emissive map, a
//! gobo, an environment bound as a texture — loses everything above 1.0 and
//! bands the bottom end. OIIO's own answer is EXR (`maketx --format exr` emits
//! 64x64-tiled, full-MIPMAP, zipped `half`), V-Ray's native streaming format
//! *is* tiled mip EXR, and Karma recommends the same. So [`TiledFile`] is a
//! facade over two [`Backend`]s and picks between them by **magic number**
//! rather than by extension — which is what makes a `maketx --format exr -o
//! foo.tx`, an EXR inside a file named `.tx`, simply work.
//!
//! The split is by *sample type*, not by container: an 8-bit source keeps the
//! TIFF path and its `u8` tiles byte for byte, and a float source takes EXR and
//! `half` tiles that are already linear. See [`cache::TileData`].
//!
//! **The one thing crust does not delegate is the mip filter.** `maketx` has
//! its own, and `docs/color_management.md` records why this codebase reduces in
//! linear light: averaging display-encoded bytes is not averaging light, and a
//! chain built that way drifts darker at every level. So the writers here
//! reduce exactly as the in-memory pyramid does, which is what lets a streamed
//! render and a preloaded one agree texel for texel.

mod cache;
mod exr_read;
mod exr_write;
mod make;
mod read;
mod stream;
mod write;

pub use cache::{
    CacheCounters, DEFAULT_BUDGET_BYTES, Tile, TileCache, TileData, TileId, with_tile,
};
pub use exr_write::write_tx_exr;
pub use make::{MadeTx, TxFormat, is_ptex, make_tx, make_tx_atomic, tx_is_stale, tx_sibling};
pub use stream::StreamingTexture;
pub(crate) use write::space_name;
pub use write::{TILE_EDGE, write_tx};

use std::fmt::Debug;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::sync::Arc;

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
    /// the level's bounds, which is how both backends hand them back.
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

/// A cursor onto one file — the mutable half of reading it.
///
/// Both backends need `&mut` to read (the `tiff` decoder keeps its state there,
/// and an EXR tile read is a seek), so neither can be shared between Rayon
/// workers. The cache keeps a per-file pool of these; a miss takes one and puts
/// it back. The enum is matched once per **miss**, never per texel.
#[derive(Debug)]
pub enum TileReader {
    Tiff(Box<read::TiffReader>),
    Exr(BufReader<File>),
}

/// What a tiled-file backing has to be able to do.
///
/// Deliberately small: geometry, provenance, a cursor, and one tile. Everything
/// above — level selection, bilinear taps, UDIM addressing, the cache itself —
/// is written once against this and not per format.
pub(crate) trait Backend: Debug + Send + Sync {
    fn levels(&self) -> &[LevelInfo];
    fn tile_edge(&self) -> usize;
    fn path(&self) -> &Path;
    /// The colour space this file is meant to be bound with, if it says.
    fn mip_space(&self) -> Option<&str>;
    /// Whether this backing's texels arrive already linear.
    fn linear(&self) -> bool;
    fn reader(&self) -> io::Result<TileReader>;
    fn read_tile(&self, r: &mut TileReader, level: usize, index: u32) -> io::Result<TileData>;
}

/// The immutable geometry of one tiled, mip-mapped file, whatever backs it.
///
/// Cheap to clone — the cache holds one per file and the sampler holds one per
/// UDIM chart — because everything behind it is immutable and shared.
///
/// **The geometry is copied out of the backing, not asked of it.** `levels`,
/// `tile_edge` and the rest are answered from fields here while `inner` is
/// touched only to open a cursor or read a tile. That is not redundancy: a
/// texel fetch asks for the tile edge and its level's layout, and routing those
/// through `dyn Backend` put an unelidable indirect call in the hottest loop in
/// a textured render — worth ~110 M instructions, 3% of a whole frame, on a
/// path where the answer cannot change after `open`.
#[derive(Clone, Debug)]
pub struct TiledFile {
    inner: Arc<dyn Backend>,
    /// `Arc` rather than `Vec` so a clone per UDIM chart and per cache entry
    /// copies a pointer instead of a dozen `LevelInfo`s.
    levels: Arc<[LevelInfo]>,
    tile_edge: usize,
    mip_space: Option<Arc<str>>,
    linear: bool,
    path: Arc<Path>,
}

impl TiledFile {
    /// Reads the header and every level's geometry, decoding no pixels.
    ///
    /// The backing is chosen by the file's **magic number**, not its extension:
    /// `.tx` names a purpose rather than a container, and `maketx --format exr`
    /// writes an EXR under exactly that name.
    ///
    /// Everything this rejects, it rejects *here* rather than at the first
    /// sample, because a texture that declines to open falls back to a
    /// constant colour while one that fails mid-render takes down a worker —
    /// and `panic = "abort"` makes that the whole process.
    pub fn open(path: &Path) -> io::Result<TiledFile> {
        let mut magic = [0u8; 4];
        BufReader::new(File::open(path)?).read_exact(&mut magic)?;
        let inner: Arc<dyn Backend> = match magic {
            // Little- and big-endian TIFF.
            [b'I', b'I', 42, 0] | [b'M', b'M', 0, 42] => Arc::new(read::TiffFile::open(path)?),
            // OpenEXR, version-flagged second.
            [0x76, 0x2f, 0x31, 0x01] => Arc::new(exr_read::ExrFile::open(path)?),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{}: not a TIFF or an OpenEXR (magic {other:02x?})",
                        path.display()
                    ),
                ));
            }
        };
        Ok(TiledFile {
            levels: inner.levels().into(),
            tile_edge: inner.tile_edge(),
            mip_space: inner.mip_space().map(Arc::from),
            linear: inner.linear(),
            path: Arc::from(inner.path()),
            inner,
        })
    }

    pub fn levels(&self) -> &[LevelInfo] {
        &self.levels
    }

    #[inline]
    pub fn level(&self, n: usize) -> LevelInfo {
        self.levels[n.min(self.levels.len() - 1)]
    }

    #[inline]
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    #[inline]
    pub fn tile_edge(&self) -> usize {
        self.tile_edge
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this file is the one to bind in `space`.
    ///
    /// The marker means the same thing for both backings, but the two reach it
    /// differently. A `.tx` stores display-encoded texels and reduces its levels
    /// in linear light, so the space is baked into every level above 0 and
    /// cannot be reinterpreted afterwards: reading an sRGB-reduced chain as raw
    /// leaves level 0 perfectly correct and every coarser level wrong — a
    /// discrepancy that appears only under minification and reads exactly like a
    /// filtering bug. An EXR-backed file stores *linear* texels, decoded once at
    /// conversion, so the marker records which source encoding they came from
    /// and reading it under another space would apply a curve to data that has
    /// already had one removed. Either way it is checked rather than trusted.
    ///
    /// A file with no marker (anything `maketx` wrote) is accepted: its chain
    /// came from a different filter anyway, so there is nothing crust could
    /// usefully match against, and refusing it would rule out every
    /// pre-existing production asset.
    pub fn mip_space_matches(&self, space: &str) -> bool {
        match &self.mip_space {
            Some(recorded) => &**recorded == space,
            None => true,
        }
    }

    /// The recorded space, for reporting a mismatch.
    pub fn mip_space(&self) -> Option<&str> {
        self.mip_space.as_deref()
    }

    /// Whether this file's texels arrive already linear — reported at load, and
    /// the difference between a `u8` tile and a `half` one.
    pub fn is_linear(&self) -> bool {
        self.linear
    }

    /// A fresh cursor onto this file. The cache keeps a small pool of these;
    /// they are not shareable, so one per concurrent miss is the floor.
    pub fn reader(&self) -> io::Result<TileReader> {
        self.inner.reader()
    }

    /// Decodes one tile, clipped to the level's bounds.
    ///
    /// The returned payload holds `tile_size(index).0 * .1 * 3` components —
    /// **not** `tile_edge²·3`. Edge tiles are stored padded (TIFF) or already
    /// clipped (EXR) and come back cut either way, so the row stride is the
    /// tile's own width.
    pub fn read_tile(&self, r: &mut TileReader, level: usize, index: u32) -> io::Result<TileData> {
        self.inner.read_tile(r, level, index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    /// Round-trips a tiled, multi-level, Deflate TIFF through the writer and
    /// back out through `tiff`'s chunk API.
    ///
    /// This is the experiment the whole streaming design rests on, so it is
    /// deliberately end-to-end and deliberately exact. Three separate claims
    /// are being checked, each of which would sink the design on its own:
    ///
    /// 1. A tiled TIFF can be *written* at all. The `tiff` crate's encoder is
    ///    strips-only (upstream #205, open since 2023), so the tile tags and
    ///    the per-tile Deflate are ours; only the IFD machinery is borrowed.
    /// 2. Mip levels survive as chained IFDs, and `seek_to_image(n)` reaches
    ///    level `n` — the crate ignores `NewSubfileType` entirely and rebuilds
    ///    its state from whichever IFD it lands on, which is what makes this
    ///    work.
    /// 3. `read_chunk(i)` returns *one* tile without decoding the level around
    ///    it. That is the entire point: if it had to materialise the level,
    ///    streaming would cost more than preloading.
    ///
    /// It also pins the contract that decides the cache's layout: a tile is
    /// written full-size and zero-padded, but comes back **clipped** to
    /// `chunk_data_dimensions`. An edge tile is therefore narrower or shorter
    /// than `TILE_EDGE`, and the sampler must index it by its own width rather
    /// than by the nominal one. Assuming the padded size reads 64 texels of
    /// stride into a 22-texel row and shears the right-hand column of every
    /// non-multiple-sized texture.
    #[test]
    fn a_tiled_mipped_tx_round_trips_tile_by_tile() {
        let dir = std::env::temp_dir().join("crust_tx_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("spike.tx");

        // 150x100 is deliberately not a multiple of the 64-texel tile edge, so
        // the right and bottom tiles are partial and must be zero-padded out to
        // full size — TIFF6 requires that, and a reader assumes it.
        let (w, h) = (150usize, 100usize);
        let src: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                [(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8]
            })
            .collect();

        let levels = write_tx(&path, &src, w, h, crust_core::ColorSpace::Srgb).expect("write .tx");
        // 150x100 -> 75x50 -> 38x25 -> 19x13 -> 10x7 -> 5x4 -> 3x2 -> 2x1 -> 1x1
        assert_eq!(levels.len(), 9, "{levels:?}");
        assert_eq!(levels[0], (w, h));
        assert_eq!(*levels.last().unwrap(), (1, 1));

        let file = std::fs::File::open(&path).expect("open");
        let mut dec = tiff::decoder::Decoder::new(BufReader::new(file)).expect("decode");

        // Level 0: every tile, compared against the source it was cut from.
        assert_eq!(dec.get_chunk_type(), tiff::decoder::ChunkType::Tile);
        assert_eq!(dec.chunk_dimensions(), (TILE_EDGE as u32, TILE_EDGE as u32));
        let across = w.div_ceil(TILE_EDGE);
        let down = h.div_ceil(TILE_EDGE);
        assert_eq!(
            dec.tile_count().expect("tile count") as usize,
            across * down
        );

        for ty in 0..down {
            for tx in 0..across {
                let idx = (ty * across + tx) as u32;
                let (tw, th) = dec.chunk_data_dimensions(idx);
                let (tw, th) = (tw as usize, th as usize);
                // The edge tiles really are clipped, so the padding the writer
                // added never reaches a caller.
                assert_eq!(tw, TILE_EDGE.min(w - tx * TILE_EDGE), "tile {idx} width");
                assert_eq!(th, TILE_EDGE.min(h - ty * TILE_EDGE), "tile {idx} height");

                let got = match dec.read_chunk(idx).expect("read tile") {
                    tiff::decoder::DecodingResult::U8(v) => v,
                    other => panic!("unexpected sample type: {other:?}"),
                };
                assert_eq!(got.len(), tw * th * 3, "tile {idx} size");
                for ly in 0..th {
                    for lx in 0..tw {
                        let (sx, sy) = (tx * TILE_EDGE + lx, ty * TILE_EDGE + ly);
                        let o = (ly * tw + lx) * 3;
                        let s = (sy * w + sx) * 3;
                        assert_eq!(
                            [got[o], got[o + 1], got[o + 2]],
                            [src[s], src[s + 1], src[s + 2]],
                            "tile {idx} texel ({lx}, {ly})"
                        );
                    }
                }
            }
        }

        // Every coarser level is reachable and reports the size the writer
        // recorded for it. A level whose IFD did not chain would fail here.
        for (n, &(lw, lh)) in levels.iter().enumerate() {
            dec.seek_to_image(n)
                .unwrap_or_else(|e| panic!("seek to level {n}: {e}"));
            assert_eq!(dec.dimensions().expect("dims"), (lw as u32, lh as u32));
            assert_eq!(dec.get_chunk_type(), tiff::decoder::ChunkType::Tile);
            let want = lw.div_ceil(TILE_EDGE) * lh.div_ceil(TILE_EDGE);
            assert_eq!(dec.tile_count().expect("tiles") as usize, want, "level {n}");
            // The first tile of every level decodes — the levels are not just
            // headers with nothing behind them.
            assert!(dec.read_chunk(0).is_ok(), "level {n} tile 0");
        }

        // Seeking backwards works too, which the cache relies on: a render
        // asks for levels in whatever order the rays happen to need them.
        dec.seek_to_image(0).expect("back to level 0");
        assert_eq!(dec.dimensions().expect("dims"), (w as u32, h as u32));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `TiledFile` recovers, through its own API, exactly the image the writer
    /// was given — every level, every tile, every texel.
    ///
    /// The round-trip above proves the *format* works; this proves the reader's
    /// geometry does. The two are separable failures: `locate` and `tile_size`
    /// can each be wrong in ways that still decode successfully and simply
    /// return the wrong texels, which is the class of bug a texture makes
    /// impossible to see by eye.
    #[test]
    fn the_reader_recovers_every_level_texel_for_texel() {
        let dir = std::env::temp_dir().join("crust_tx_reader");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("read.tx");

        // Deliberately not a multiple of the tile edge in either axis, so the
        // right column and bottom row of tiles are both clipped.
        let (w, h) = (200usize, 70usize);
        let src: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                [
                    (x * 7 % 251) as u8,
                    (y * 13 % 251) as u8,
                    ((x + y) % 251) as u8,
                ]
            })
            .collect();
        let levels = write_tx(&path, &src, w, h, crust_core::ColorSpace::Raw).expect("write");

        let tf = TiledFile::open(&path).expect("open");
        assert_eq!(tf.tile_edge(), TILE_EDGE);
        assert_eq!(tf.level_count(), levels.len());
        let mut dec = tf.reader().expect("reader");

        // Level 0 must come back bit-exact: nothing has filtered it. Walked a
        // tile at a time and reconstructed into a full image, which checks
        // `locate` and `tile_size` against each other — a coordinate the two
        // disagree about lands in the wrong place and shows up as a gap.
        let l0 = tf.level(0);
        assert_eq!((l0.width, l0.height), (w, h));
        let mut rebuilt = vec![0u8; w * h * 3];
        for idx in 0..(l0.across * l0.down) as u32 {
            let (tw, th) = l0.tile_size(idx, TILE_EDGE);
            let tile = tf.read_tile(&mut dec, 0, idx).expect("tile");
            let tile = tile.expect_u8();
            let (tx, ty) = (idx as usize % l0.across, idx as usize / l0.across);
            for ly in 0..th {
                for lx in 0..tw {
                    let (x, y) = (tx * TILE_EDGE + lx, ty * TILE_EDGE + ly);
                    let d = (y * w + x) * 3;
                    rebuilt[d..d + 3].copy_from_slice(&tile[(ly * tw + lx) * 3..][..3]);
                }
            }
        }
        assert_eq!(rebuilt, src, "level 0 did not reconstruct");

        // And `locate` agrees with that reconstruction for a sample of texels,
        // including the ones in the clipped right column and bottom row.
        for &(x, y) in &[(0, 0), (63, 63), (64, 0), (199, 69), (128, 64), (199, 0)] {
            let (idx, lx, ly) = l0.locate(x, y, TILE_EDGE);
            let (tw, _) = l0.tile_size(idx, TILE_EDGE);
            let tile = tf.read_tile(&mut dec, 0, idx).expect("tile");
            let tile = tile.expect_u8();
            let s = (y * w + x) * 3;
            assert_eq!(
                &tile[(ly * tw + lx) * 3..][..3],
                &src[s..s + 3],
                "texel ({x}, {y}) via tile {idx}"
            );
        }

        // Every coarser level reads, reports the geometry the writer recorded,
        // and its tiles are the size the clipping rule says they are.
        for (n, &(lw, lh)) in levels.iter().enumerate() {
            let li = tf.level(n);
            assert_eq!((li.width, li.height), (lw, lh), "level {n} size");
            assert_eq!(li.across, lw.div_ceil(TILE_EDGE));
            assert_eq!(li.down, lh.div_ceil(TILE_EDGE));
            for idx in 0..(li.across * li.down) as u32 {
                let (tw, th) = li.tile_size(idx, TILE_EDGE);
                let tile = tf.read_tile(&mut dec, n, idx).expect("tile");
                assert_eq!(tile.len(), tw * th * 3, "level {n} tile {idx}");
            }
        }

        // Asking past the last level clamps rather than failing — the sampler
        // clamps too, but a cache that asked one level too far should degrade
        // to the coarsest rather than error a worker.
        let past = tf.read_tile(&mut dec, 99, 0).expect("clamped level");
        assert_eq!(past.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The EXR backing round-trips a tiled, mip-mapped file tile by tile —
    /// and keeps values a `u8` tile cannot hold.
    ///
    /// This is the EXR counterpart of the TIFF spike above and it is load-
    /// bearing in the same way: three separate claims, each of which would sink
    /// the second backing on its own.
    ///
    /// 1. `exr` can *write* tiled mip levels through its ordinary API, so
    ///    unlike TIFF there is no hand-rolled container here at all.
    /// 2. One tile of one level can be read **without** materialising the rest,
    ///    which is the entire point — the crate's own `filter_chunks` cannot do
    ///    it, so the offset table is walked by hand.
    /// 3. The chunk offset table is found correctly despite `PeekRead` being
    ///    allowed to sit one byte past the header.
    ///
    /// The comparison is exact rather than approximate: the writer quantises
    /// once, to `half`, and the reader returns those bits, so anything other
    /// than `f16::from_f32(source)` is a bug rather than precision.
    #[test]
    fn a_tiled_mipped_exr_round_trips_tile_by_tile() {
        let dir = std::env::temp_dir().join("crust_exr_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("spike.tx");

        // Not a multiple of the tile edge in either axis, so the right column
        // and bottom row of tiles are clipped. Values deliberately range well
        // past 1.0, which is the whole reason this backing exists.
        let (w, h) = (150usize, 100usize);
        let src: Vec<f32> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                [x * 0.25, y * 0.5, (x + y) * 0.125]
            })
            .collect();

        let levels =
            write_tx_exr(&path, &src, w, h, crust_core::ColorSpace::Raw).expect("write exr");
        // Same `div_ceil` chain as the TIFF writer: 150x100 down to 1x1.
        assert_eq!(levels.len(), 9, "{levels:?}");
        assert_eq!(levels[0], (w, h));
        assert_eq!(*levels.last().unwrap(), (1, 1));

        // Opened through the facade, so the magic number — not the `.tx`
        // extension — is what selected the EXR backing.
        let tf = TiledFile::open(&path).expect("open");
        assert!(tf.is_linear(), "an EXR-backed file is linear");
        assert_eq!(tf.level_count(), levels.len());
        assert_eq!(tf.tile_edge(), TILE_EDGE);
        let mut r = tf.reader().expect("reader");

        let l0 = tf.level(0);
        assert_eq!((l0.width, l0.height), (w, h));
        for idx in 0..(l0.across * l0.down) as u32 {
            let (tw, th) = l0.tile_size(idx, TILE_EDGE);
            let data = tf.read_tile(&mut r, 0, idx).expect("tile");
            let tile = data.expect_half();
            assert_eq!(tile.len(), tw * th * 3, "tile {idx} size");
            let (tx, ty) = (idx as usize % l0.across, idx as usize / l0.across);
            for ly in 0..th {
                for lx in 0..tw {
                    let (sx, sy) = (tx * TILE_EDGE + lx, ty * TILE_EDGE + ly);
                    for k in 0..3 {
                        let got = tile[(ly * tw + lx) * 3 + k];
                        let want = half::f16::from_f32(src[(sy * w + sx) * 3 + k]);
                        assert_eq!(got, want, "tile {idx} texel ({lx}, {ly}) channel {k}");
                    }
                }
            }
        }

        // Every coarser level reads, at the geometry the writer recorded. A
        // rounding-mode disagreement between writer and reader would land here.
        for (n, &(lw, lh)) in levels.iter().enumerate() {
            let li = tf.level(n);
            assert_eq!((li.width, li.height), (lw, lh), "level {n} size");
            for idx in 0..(li.across * li.down) as u32 {
                let (tw, th) = li.tile_size(idx, TILE_EDGE);
                let data = tf.read_tile(&mut r, n, idx).expect("tile");
                assert_eq!(
                    data.expect_half().len(),
                    tw * th * 3,
                    "level {n} tile {idx}"
                );
            }
        }

        // Levels are reachable in any order — a render asks for whichever the
        // rays happen to need — and past the end clamps rather than failing.
        let _ = tf.read_tile(&mut r, 0, 0).expect("back to level 0");
        assert_eq!(tf.read_tile(&mut r, 99, 0).expect("clamped").len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pyramid is averaged in linear light and the average is *exact*,
    /// because there is no encoding to average through.
    ///
    /// The `u8` path has to decode, average and re-encode, and
    /// `levels_average_in_linear_light_not_in_the_file_encoding` pins that it
    /// does. The float path has nothing to get wrong except the reduction
    /// itself, so this checks the reduction: a level of a constant image is
    /// that constant, and a 2x2 of known values is their mean — at a value a
    /// `u8` tile could not have represented at all.
    #[test]
    fn exr_levels_average_the_actual_values() {
        let dir = std::env::temp_dir().join("crust_exr_levels");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("levels.tx");

        // A 2x2 whose mean is 5.0 — well past anything 8 bits can hold.
        let (w, h) = (2usize, 2usize);
        let src: Vec<f32> = vec![
            2.0, 2.0, 2.0, //
            4.0, 4.0, 4.0, //
            6.0, 6.0, 6.0, //
            8.0, 8.0, 8.0,
        ];
        let levels = write_tx_exr(&path, &src, w, h, crust_core::ColorSpace::Raw).expect("write");
        assert_eq!(levels, vec![(2, 2), (1, 1)]);

        let tf = TiledFile::open(&path).expect("open");
        let mut r = tf.reader().expect("reader");
        let coarse = tf.read_tile(&mut r, 1, 0).expect("level 1");
        for (k, sample) in coarse.expect_half().iter().take(3).enumerate() {
            assert_eq!(sample.to_f32(), 5.0, "channel {k}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not tiled, or is planar, must decline at open.
    ///
    /// Planar especially: `PlanarConfiguration = 2` panics inside
    /// `expand_chunk` on `tiff` 0.11.3 (image-tiff#403), and with
    /// `panic = "abort"` that is the process, not the worker. Declining at open
    /// turns it into a texture that falls back to a constant colour.
    #[test]
    fn a_stripped_or_unreadable_file_declines_rather_than_panicking() {
        let dir = std::env::temp_dir().join("crust_tx_reject");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // Not a TIFF at all.
        let junk = dir.join("junk.tx");
        std::fs::write(&junk, b"this is not a tiff").expect("write");
        assert!(TiledFile::open(&junk).is_err());

        // A real, valid, *stripped* TIFF — the common mistake of pointing the
        // streaming path at an unconverted file.
        let stripped = dir.join("stripped.tx");
        {
            let f = std::fs::File::create(&stripped).expect("create");
            let mut enc =
                tiff::encoder::TiffEncoder::new(std::io::BufWriter::new(f)).expect("encoder");
            let img: Vec<u8> = vec![128; 32 * 32 * 3];
            enc.write_image::<tiff::encoder::colortype::RGB8>(32, 32, &img)
                .expect("write stripped");
        }
        let err = TiledFile::open(&stripped).expect_err("stripped must decline");
        assert!(
            err.to_string().contains("tiled"),
            "the message should say what is wrong: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The decoder must be `Send` for the cache to keep a pool of them and
    /// hand one to whichever Rayon worker takes a miss.
    ///
    /// Nothing in `tiff` documents this — it falls out of the field types — so
    /// it is asserted rather than assumed. A future version that stores a
    /// `Rc` or a raw pointer would break the pool design, and this is where
    /// that would surface.
    #[test]
    fn the_decoder_can_move_between_threads() {
        fn assert_send<T: Send>() {}
        assert_send::<tiff::decoder::Decoder<BufReader<std::fs::File>>>();
    }
}
