//! Tiled, mip-mapped TIFF — the `.tx` textures a streaming cache reads from.
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
//! **The one thing crust does not delegate is the mip filter.** `maketx` has
//! its own, and `docs/color_management.md` records why this codebase reduces in
//! linear light: averaging display-encoded bytes is not averaging light, and a
//! chain built that way drifts darker at every level. So the writer here
//! reduces exactly as the in-memory pyramid does, which is what lets a streamed
//! render and a preloaded one agree texel for texel.

mod cache;
mod read;
mod write;

pub use cache::{CacheCounters, DEFAULT_BUDGET_BYTES, Tile, TileCache, TileId, with_microcache};
pub use read::{LevelInfo, TileReader, TiledFile};
pub use write::{TILE_EDGE, write_tx};

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
