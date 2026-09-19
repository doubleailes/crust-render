//! `Texture2D` over the tile cache — the streaming counterpart to
//! [`crate::UvTexture`].
//!
//! The two are meant to be interchangeable and are checked against each other:
//! for any texture whose level 0 is the same in both (that is, anything at or
//! below the `CRUST_TEX_MAX` cap the preload path applies) they must agree
//! texel for texel. That is why the level selection here is the same arithmetic
//! in the same order — `log2(width · texels)`, the magnification
//! short-circuit, floor and lerp — and why both build their mip chains with the
//! same `reduce_half`. The only thing that differs is where the texels come
//! from.
//!
//! What genuinely differs is the *ceiling*: preloading caps level 0 to keep it
//! resident, and streaming does not, so above the cap the streamed image is the
//! sharper and more correct one. That is the feature, not a discrepancy.

use super::cache::{TileCache, TileId, with_microcache};
use super::read::{LevelInfo, TiledFile};
use crate::uv_texture::to_linear_table;
use crust_core::{ColorSpace, Texture2D};
use std::path::Path;
use std::sync::Arc;

/// One UDIM tile of a streaming texture: a file, and the cache index that
/// names it.
struct Chart {
    /// UDIM number, `1001 + u + 10·v`, as the preload path keys them.
    number: u32,
    file: TiledFile,
    id: u32,
}

/// A UV texture whose texels live on disk.
pub struct StreamingTexture {
    charts: Vec<Chart>,
    cache: Arc<TileCache>,
    /// Per-channel decode table, `u8` → linear `f32`, colour space baked in.
    ///
    /// The same table the preload path uses, and applied at the same point: on
    /// lookup, against stored bytes in the file's own encoding. Decoding to
    /// linear `f32` in the cache instead would quadruple every tile and work
    /// directly against the budget the cache exists to hold.
    to_linear: [f32; 256],
    tiled: bool,
    fallback: [f32; 4],
}

impl StreamingTexture {
    /// Opens a `.tx` (or a `<UDIM>` / `<UVTILE>` set of them) against `cache`.
    ///
    /// Returns `None` when nothing could be opened, so the caller can fall
    /// back to the preload path rather than render an untextured surface.
    pub fn open(
        path: &Path,
        space: ColorSpace,
        cache: Arc<TileCache>,
        expand: impl Fn(u32, u32) -> Option<std::path::PathBuf>,
    ) -> Option<StreamingTexture> {
        let name = path.to_string_lossy().into_owned();
        let tiled = name.contains("<UDIM>") || name.contains("<UVTILE>");
        let want_space = crate::tiled::space_name(space);

        let mut charts = Vec::new();
        if tiled {
            // The same 10x10 sweep the preload path does, and for the same
            // reason: only tiles present on disk cost anything, so a chart
            // with holes is free where it has none.
            for v in 0..10u32 {
                for u in 0..10u32 {
                    let Some(p) = expand(u, v) else { continue };
                    if !p.exists() {
                        continue;
                    }
                    match TiledFile::open(&p) {
                        Ok(f) if !f.mip_space_matches(want_space) => {
                            tracing::warn!(
                                "{}: mip chain was reduced in {:?}, not {want_space} — \
                                 falling back to the preloaded texture",
                                p.display(),
                                f.mip_space()
                            );
                            return None;
                        }
                        Ok(f) => {
                            if let Some(id) = cache.intern(f.clone()) {
                                charts.push(Chart {
                                    number: 1001 + u + 10 * v,
                                    file: f,
                                    id,
                                });
                            }
                        }
                        Err(e) => tracing::debug!("{}: {e}", p.display()),
                    }
                }
            }
        } else {
            match TiledFile::open(path) {
                Ok(f) if !f.mip_space_matches(want_space) => {
                    tracing::warn!(
                        "{}: mip chain was reduced in {:?}, not {want_space} — \
                         falling back to the preloaded texture",
                        path.display(),
                        f.mip_space()
                    );
                    return None;
                }
                Ok(f) => {
                    let id = cache.intern(f.clone())?;
                    charts.push(Chart {
                        number: 1001,
                        file: f,
                        id,
                    });
                }
                Err(e) => tracing::debug!("{}: {e}", path.display()),
            }
        }
        if charts.is_empty() {
            return None;
        }

        Some(StreamingTexture {
            charts,
            cache,
            to_linear: to_linear_table(space),
            tiled,
            // Mid-grey, not black: a tile that fails to page in should read as
            // an obviously wrong surface rather than as a shadow, which is
            // what black would be mistaken for.
            fallback: [0.5, 0.5, 0.5, 1.0],
        })
    }

    /// Level-0 size of the representative chart, for the load report.
    pub fn size(&self) -> (usize, usize) {
        let l = self.charts[0].file.level(0);
        (l.width, l.height)
    }

    pub fn chart_count(&self) -> usize {
        self.charts.len()
    }

    pub fn level_count(&self) -> usize {
        self.charts[0].file.level_count()
    }

    /// One texel of one level, through the cache.
    ///
    /// `x`/`y` are level coordinates, already clamped by the caller. The tile
    /// they fall in is located, paged in if absent, and indexed by **its own**
    /// width — an edge tile is narrower than the nominal tile edge, and using
    /// the nominal stride shears the right-hand column of every texture whose
    /// size is not a multiple of it.
    #[inline]
    fn texel(&self, chart: &Chart, level: usize, li: &LevelInfo, x: usize, y: usize) -> [f32; 3] {
        let edge = chart.file.tile_edge();
        let (index, lx, ly) = li.locate(x, y, edge);
        let Some(tile) = with_microcache(
            &self.cache,
            TileId {
                file: chart.id,
                level: level as u8,
                tile: index,
            },
        ) else {
            return [self.fallback[0], self.fallback[1], self.fallback[2]];
        };
        if lx >= tile.width || ly >= tile.height {
            return [self.fallback[0], self.fallback[1], self.fallback[2]];
        }
        let o = (ly * tile.width + lx) * 3;
        [
            self.to_linear[tile.pixels[o] as usize],
            self.to_linear[tile.pixels[o + 1] as usize],
            self.to_linear[tile.pixels[o + 2] as usize],
        ]
    }

    /// Bilinear lookup within one level, at coordinates already reduced to
    /// `[0, 1)`.
    ///
    /// Mirrors `UvTexture::sample_level` exactly, including the `v` flip and
    /// the clamp-to-edge: the two are supposed to produce identical images, and
    /// a half-texel difference here would show up as a scene-wide shift that
    /// no test of either alone would catch.
    fn sample_level(&self, chart: &Chart, level: usize, u: f32, v: f32) -> [f32; 4] {
        let li = chart.file.level(level);
        let (w, h) = (li.width, li.height);
        let x = u * w as f32 - 0.5;
        let y = (1.0 - v) * h as f32 - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let (fx, fy) = (x - x0, y - y0);
        let clampi = |i: f32, n: usize| (i.max(0.0) as usize).min(n.saturating_sub(1));
        let (x0i, y0i) = (clampi(x0, w), clampi(y0, h));
        let (x1i, y1i) = (clampi(x0 + 1.0, w), clampi(y0 + 1.0, h));
        let a = self.texel(chart, level, &li, x0i, y0i);
        let b = self.texel(chart, level, &li, x1i, y0i);
        let c = self.texel(chart, level, &li, x0i, y1i);
        let d = self.texel(chart, level, &li, x1i, y1i);
        let mut out = [0.0f32; 4];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            out[k] = top + (bot - top) * fy;
        }
        out[3] = 1.0;
        out
    }

    /// Trilinear lookup, level chosen from the footprint.
    ///
    /// Kept line-for-line equivalent to `UvTexture::sample_tile` — same widest-
    /// axis measure, same magnification short-circuit before the `log2` (which
    /// was worth ~9% of render when it was missing), same floor and lerp.
    fn sample_chart(&self, chart: &Chart, u: f32, v: f32, width: f32) -> [f32; 4] {
        let levels = chart.file.level_count();
        if levels == 1 || width <= 0.0 {
            return self.sample_level(chart, 0, u, v);
        }
        let l0 = chart.file.level(0);
        let across = l0.width.max(l0.height) as f32;
        let texels = width * across;
        if texels <= 1.0 {
            return self.sample_level(chart, 0, u, v);
        }
        let lod = texels.log2().clamp(0.0, (levels - 1) as f32);
        let lo = lod.floor();
        let frac = lod - lo;
        let a = self.sample_level(chart, lo as usize, u, v);
        if frac <= 0.0 {
            return a;
        }
        let b = self.sample_level(chart, (lo as usize + 1).min(levels - 1), u, v);
        let mut out = [0.0f32; 4];
        for k in 0..3 {
            out[k] = a[k] + (b[k] - a[k]) * frac;
        }
        out[3] = 1.0;
        out
    }
}

impl Texture2D for StreamingTexture {
    fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        if !u.is_finite() || !v.is_finite() {
            return [0.0, 0.0, 0.0, 1.0];
        }
        let width = if width.is_finite() { width } else { 0.0 };
        if self.tiled {
            let (tu, tv) = (u.floor(), v.floor());
            if !(0.0..10.0).contains(&tu) || !(0.0..10.0).contains(&tv) {
                return [0.0, 0.0, 0.0, 1.0];
            }
            let number = 1001 + tu as u32 + 10 * tv as u32;
            match self.charts.iter().find(|c| c.number == number) {
                Some(c) => self.sample_chart(c, u - tu, v - tv, width),
                None => [0.0, 0.0, 0.0, 1.0],
            }
        } else {
            let wrap = |x: f32| x - x.floor();
            self.sample_chart(&self.charts[0], wrap(u), wrap(v), width)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UvTexture;
    use crate::tiled::write_tx;

    /// Writes a `.tx` and the equivalent PNG, so the two paths can be pointed
    /// at the same image.
    fn pair(
        name: &str,
        w: usize,
        h: usize,
        space: crust_core::ColorSpace,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("crust_stream_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        // High-frequency on purpose: a smooth ramp would agree under almost
        // any filtering bug, while a checker with noise disagrees on a
        // half-texel shift or a wrong mip level.
        let src: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let c = if (x / 3 + y / 3) % 2 == 0 { 230 } else { 20 };
                [c, (x * 11 % 251) as u8, (y * 17 % 251) as u8]
            })
            .collect();
        let png = dir.join("src.png");
        image::RgbImage::from_raw(w as u32, h as u32, src.clone())
            .expect("image")
            .save(&png)
            .expect("write png");
        let tx = dir.join("src.tx");
        write_tx(&tx, &src, w, h, space).expect("write tx");
        (png, tx)
    }

    /// **The invariant the whole change rests on.**
    ///
    /// For a texture at or below the preload path's resolution cap, streaming
    /// and preloading see the same level 0, build their mip chains with the
    /// same `reduce_half`, and select levels with the same arithmetic — so
    /// every lookup must return exactly the same bits. If this drifts, the
    /// golden-image check across the whole sample set drifts with it, and the
    /// two paths stop being comparable at all.
    #[test]
    fn streaming_and_preloading_agree_bit_for_bit() {
        let (png, tx) = pair("agree", 256, 192, crust_core::ColorSpace::Srgb);
        let pre = UvTexture::open_with(&png, crust_core::ColorSpace::Srgb, true).expect("preload");
        let cache = Arc::new(TileCache::new(64 * 1024 * 1024));
        let stream = StreamingTexture::open(&tx, crust_core::ColorSpace::Srgb, cache, |_, _| None)
            .expect("stream");

        assert_eq!(stream.level_count(), pre.level_count());
        assert_eq!(stream.size(), pre.tile_size());

        // Across the footprint range that selects every level, including the
        // magnification short-circuit (width 0) and past the coarsest.
        for &width in &[0.0f32, 0.001, 0.004, 0.02, 0.09, 0.3, 0.7, 2.0, 9.0] {
            for i in 0..23 {
                for j in 0..17 {
                    let (u, v) = (i as f32 / 22.0, j as f32 / 16.0);
                    assert_eq!(
                        stream.eval(u, v, width),
                        pre.eval(u, v, width),
                        "at ({u}, {v}) width {width}"
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(png.parent().unwrap());
    }

    /// The agreement must hold when the image is *not* a multiple of the tile
    /// edge, which is where the clipped-edge-tile stride would show up.
    #[test]
    fn agreement_holds_across_clipped_edge_tiles() {
        let (png, tx) = pair("clipped", 150, 100, crust_core::ColorSpace::Raw);
        let pre = UvTexture::open_with(&png, crust_core::ColorSpace::Raw, true).expect("preload");
        let cache = Arc::new(TileCache::new(16 * 1024 * 1024));
        let stream = StreamingTexture::open(&tx, crust_core::ColorSpace::Raw, cache, |_, _| None)
            .expect("stream");

        // Sampled hard against the right and bottom edges, where the last tile
        // column is 22 texels wide against a nominal 64.
        for &width in &[0.0f32, 0.01, 0.2] {
            for k in 0..40 {
                let t = k as f32 / 39.0;
                for &(u, v) in &[(t, 0.999), (0.999, t), (t, 0.001), (0.001, t)] {
                    assert_eq!(
                        stream.eval(u, v, width),
                        pre.eval(u, v, width),
                        "edge ({u}, {v}) width {width}"
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(png.parent().unwrap());
    }

    /// A tiny budget must change how often tiles are read, never what they
    /// contain — the cache is an optimisation, not a filter.
    #[test]
    fn a_thrashing_budget_changes_timing_not_pixels() {
        // Big enough that level 0 alone (3.1 MB of tiles) is several times
        // the cache's 1 MiB floor, so the sweep really runs.
        let (png, tx) = pair("thrash", 1024, 1024, crust_core::ColorSpace::Srgb);
        let pre = UvTexture::open_with(&png, crust_core::ColorSpace::Srgb, true).expect("preload");
        let cache = Arc::new(TileCache::new(1));
        let stream =
            StreamingTexture::open(&tx, crust_core::ColorSpace::Srgb, cache.clone(), |_, _| {
                None
            })
            .expect("stream");

        for i in 0..31 {
            for j in 0..31 {
                let (u, v) = (i as f32 / 30.0, j as f32 / 30.0);
                assert_eq!(stream.eval(u, v, 0.0), pre.eval(u, v, 0.0), "({u}, {v})");
            }
        }
        let c = cache.counters();
        assert!(c.evictions > 0, "the budget should have forced evictions");
        assert!(
            c.redundant > 0,
            "a budget this small should have re-read tiles"
        );
        assert_eq!(c.errors, 0);
        let _ = std::fs::remove_dir_all(png.parent().unwrap());
    }

    /// A `.tx` whose mip chain was reduced in one colour space must not be
    /// read as another.
    ///
    /// This is the bug the agreement test above found. A `.tx` stores
    /// display-encoded texels but averages its levels in linear light, so the
    /// space is baked into every level above 0. Read it as something else and
    /// level 0 stays perfectly correct while every coarser level is wrong —
    /// visible only under minification, and indistinguishable by eye from a
    /// filtering bug. Declining is better than being subtly wrong: the caller
    /// falls back to preloading, which is slower and right.
    #[test]
    fn a_mismatched_mip_space_declines_instead_of_reading_the_wrong_levels() {
        let (png, tx) = pair("space", 128, 128, crust_core::ColorSpace::Srgb);
        let cache = Arc::new(TileCache::new(4 * 1024 * 1024));

        // Same space: opens.
        assert!(
            StreamingTexture::open(&tx, crust_core::ColorSpace::Srgb, cache.clone(), |_, _| {
                None
            })
            .is_some()
        );
        // Different space: declines rather than serving a chain reduced for
        // the other one.
        assert!(
            StreamingTexture::open(&tx, crust_core::ColorSpace::Raw, cache.clone(), |_, _| None)
                .is_none()
        );
        assert!(
            StreamingTexture::open(&tx, crust_core::ColorSpace::Gamma22, cache, |_, _| None)
                .is_none()
        );

        // And the guard really is load-bearing: had it not fired, the levels
        // would have differed from what a raw-decoded preload builds.
        let pre_raw = UvTexture::open_with(&png, crust_core::ColorSpace::Raw, true).expect("pre");
        let pre_srgb = UvTexture::open_with(&png, crust_core::ColorSpace::Srgb, true).expect("pre");
        assert_ne!(
            pre_raw.eval(0.5, 0.5, 0.2),
            pre_srgb.eval(0.5, 0.5, 0.2),
            "the two spaces must actually produce different mip levels, or \
             this test proves nothing"
        );

        let _ = std::fs::remove_dir_all(png.parent().unwrap());
    }

    /// The marker crust writes is readable, and discriminates.
    ///
    /// A file with no marker at all — anything `maketx` wrote — is accepted
    /// instead: its chain came from OIIO's filter rather than crust's, so
    /// there is nothing to match against, and refusing it would rule out every
    /// pre-existing production asset, which is the interop this format was
    /// chosen for. That arm is `TiledFile::mip_space_matches`'s `None` case.
    #[test]
    fn the_mip_space_marker_round_trips_and_discriminates() {
        let (png, tx) = pair("unmarked", 96, 96, crust_core::ColorSpace::Srgb);
        let tf = TiledFile::open(&tx).expect("open");
        assert_eq!(tf.mip_space(), Some("srgb_texture"));
        assert!(tf.mip_space_matches("srgb_texture"));
        assert!(!tf.mip_space_matches("raw"));

        let _ = std::fs::remove_dir_all(png.parent().unwrap());
    }

    /// An unopenable file declines rather than producing a black texture, so
    /// the caller can fall back to the preload path.
    #[test]
    fn a_missing_or_stripped_file_declines() {
        let cache = Arc::new(TileCache::new(1024 * 1024));
        assert!(
            StreamingTexture::open(
                std::path::Path::new("/definitely/not/here.tx"),
                crust_core::ColorSpace::Raw,
                cache,
                |_, _| None,
            )
            .is_none()
        );
    }
}
