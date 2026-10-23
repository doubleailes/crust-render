//! `Texture2D` over the tile cache — the streaming counterpart to
//! [`crate::UvTexture`].
//!
//! The two are meant to be interchangeable and are checked against each other:
//! a `.tx` converted from a source, as Arnold's auto-tx converts it, streams
//! what the source preloads within the tolerance of design decision 7 of
//! `tx-arnold-convention`. Level selection is the same arithmetic in the same
//! order — `log2(width · texels)`, the magnification short-circuit, floor and
//! lerp, the one [`trilinear`] — but the levels themselves are the file's,
//! reduced as `maketx` reduces them, so the two agree exactly only where the
//! file holds the source's own samples.
//!
//! What genuinely differs is the *ceiling*: preloading caps level 0 to keep it
//! resident, and streaming does not, so above the cap the streamed image is the
//! sharper and more correct one. That is the feature, not a discrepancy.
//!
//! Alpha follows the preload's rule too: a chart whose file carries alpha
//! reads it as the lookup's fourth component, through the same decode
//! ([`crate::ALPHA_U8`]) and the same blends, and every other chart reads 1.0.

use super::cache::{TileCache, TileId, with_tile};
use super::{LevelInfo, TiledFile};
use crate::TransferCurve;
use crate::mip_filter::{MipSource, Taps, lerp_rgba, lerp_rgba_alpha, trilinear};
use crate::uv_texture::udim_number;
use crust_core::{ColorSpace, ResolvedColorSpace, Texture2D};
use std::path::Path;
use std::sync::Arc;

/// One UDIM tile of a streaming texture: a file, and the cache index that
/// names it.
struct Chart {
    /// UDIM number, `1001 + u + 10·v`, as the preload path keys them.
    number: u32,
    file: TiledFile,
    id: u32,
    /// The file's own [`TiledFile::has_alpha`]: whether its tiles are RGBA.
    alpha: bool,
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
    ///
    /// Unused by an EXR-backed chart, whose texels were decoded once at
    /// conversion and stored linear — the table is still built and still
    /// handed to [`Tile::rgb`], which ignores it on that arm. Two samplers to
    /// keep in step would cost more than one dead array.
    to_linear: [f32; 256],
    /// Whether this texture's tiles arrive already linear. A property of the
    /// *file*, read once at open, which is what lets the texel fetch pick its
    /// decode from a loop-invariant field instead of from every tile.
    linear: bool,
    /// Whether any chart carries alpha. Settles which sampler a lookup runs,
    /// with `linear`: a texture with none never asks a chart.
    alpha: bool,
    /// The space the texels decode under, resolved against the file.
    space: ResolvedColorSpace,
    /// The change of primaries into the working space, applied once per
    /// lookup after filtering — both payloads are stored on the file's own
    /// primaries. `None` when they are the working space's.
    gamut: Option<crust_core::Mat3A>,
    tiled: bool,
    fallback: [f32; 4],
}

/// What `auto` means for one `.tx` bound directly: its own samples decide,
/// by the rule the preload path applies — an integer file is sRGB, a float one
/// is as stored.
fn resolve_auto_space(f: &TiledFile, working: crust_core::color::Space) -> ResolvedColorSpace {
    ColorSpace::AUTO
        .into_working(working)
        .resolve_auto(f.is_integer())
}

impl StreamingTexture {
    /// Opens a `.tx` (or a `<UDIM>` / `<UVTILE>` set of them) against `cache`,
    /// reading its values in `space`: its curve decoded (through the `u8`
    /// table, or once at page-in for a `half` tile) and its primaries changed
    /// to the working space's after filtering. That is the binding's space
    /// for a `.tx` bound directly, whatever the file holds, and the space the
    /// file's name says it was converted to for one found beside its source
    /// ([`super::TxName::read_as`]).
    ///
    /// Returns `None` when nothing could be opened, so the caller can fall
    /// back to the preload path rather than render an untextured surface.
    pub fn open(path: &Path, space: ColorSpace, cache: Arc<TileCache>) -> Option<StreamingTexture> {
        let name = path.to_string_lossy().into_owned();
        let tiled = name.contains("<UDIM>") || name.contains("<UVTILE>");
        // `Auto` is settled by the first file that opens (see
        // `resolve_auto_space`); every later tile must then match that answer,
        // exactly as it must match an explicit space.
        let mut resolved = space.resolved();
        let working = space.working();
        let mut settle =
            |f: &TiledFile| *resolved.get_or_insert_with(|| resolve_auto_space(f, working));

        // Every tile on disk for a set (the preload path's sweep, so the two
        // backends cover the same chart), or the one file as tile 1001.
        let files: Vec<(u32, std::path::PathBuf)> = if tiled {
            crate::uv_texture::existing_tiles(&name)
                .into_iter()
                .map(|t| (t.number, t.path))
                .collect()
        } else {
            vec![(udim_number(0, 0), path.to_path_buf())]
        };
        let mut charts = Vec::new();
        for (number, p) in files {
            match TiledFile::open(&p) {
                Ok(f) => {
                    let read_as = settle(&f);
                    let f = f.decoding(read_as);
                    if let Some(id) = cache.intern(f.clone()) {
                        charts.push(Chart {
                            number,
                            alpha: f.has_alpha(),
                            file: f,
                            id,
                        });
                    }
                }
                Err(e) => tracing::debug!("{}: {e}", p.display()),
            }
        }
        if charts.is_empty() {
            return None;
        }

        // Settled by the first chart, which is also the first file opened.
        let space = settle(&charts[0].file);
        let linear = charts[0].file.is_linear();
        // One sampler serves every chart, so every chart must hold the same
        // payload: a set whose 8-bit and 16-bit tiles became `u8` and `half`
        // `.tx` (both kept at their source depth) declines and preloads.
        if let Some(c) = charts.iter().find(|c| c.file.is_linear() != linear) {
            tracing::debug!(
                "{}: tile {} stores another sample type than tile {} — preloaded instead",
                path.display(),
                c.number,
                charts[0].number
            );
            return None;
        }
        let alpha = charts.iter().any(|c| c.alpha);
        Some(StreamingTexture {
            charts,
            cache,
            to_linear: space.to_linear_table(),
            linear,
            alpha,
            gamut: space.gamut(),
            space,
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

    /// Whether this texture's tiles arrive already linear — `half` payloads
    /// from an EXR backing rather than `u8` ones from a TIFF. Reported at load,
    /// since it is the difference between a tile costing 12 KiB and 24 KiB.
    pub fn is_linear(&self) -> bool {
        self.linear
    }

    /// The colour space the texels decode under.
    pub fn color_space(&self) -> ResolvedColorSpace {
        self.space
    }

    /// Whether any chart carries alpha, which a lookup returns as its fourth
    /// component. `false` reads 1.0 there.
    pub fn has_alpha(&self) -> bool {
        self.alpha
    }

    /// One texel of one level, through the cache.
    ///
    /// `x`/`y` are level coordinates, already clamped by the caller. The tile
    /// they fall in is located, paged in if absent, and indexed by **its own**
    /// width — an edge tile is narrower than the nominal tile edge, and using
    /// the nominal stride shears the right-hand column of every texture whose
    /// size is not a multiple of it.
    #[inline]
    fn texel<const HALF: bool>(
        &self,
        chart: &Chart,
        level: usize,
        li: &LevelInfo,
        x: usize,
        y: usize,
    ) -> [f32; 3] {
        let edge = chart.file.tile_edge();
        let (index, lx, ly) = li.locate(x, y, edge);
        let miss = [self.fallback[0], self.fallback[1], self.fallback[2]];
        let id = TileId {
            file: chart.id,
            level: level as u8,
            tile: index,
        };
        // The texel is read *inside* the cache's borrow rather than through a
        // returned handle: at ~8.7 M fetches a frame, cloning an `Arc` per
        // texel costs more than the lookup it is part of.
        //
        // **`HALF` is a const, not a field**, so this reads as a branch and
        // compiles to none: the whole sampler is monomorphised twice and
        // `eval` picks between them once per call, from the file's own kind.
        //
        // That is not micro-tuning, it is the entire cost of a second payload
        // existing, and callgrind priced every cheaper attempt at it. A single
        // closure matching a `TileData` enum grew past what LLVM would inline
        // into `with_tile`: `texel` went 414.9 M to 471.5 M instructions *and*
        // grew a 216.7 M-instruction out-of-line `texel::{closure#0}` that had
        // not existed, for +21% wall clock on an 8-bit render that gains
        // nothing from HDR. Two closures chosen by a field brought it to
        // +8.5%, a byte payload instead of an enum to +6%, and each time the
        // residue was the same shape: a per-texel decision that is per-texture
        // information. Const generics say that outright — at 4.3 M lookups a
        // frame, 19 instructions of "which payload is this" is 20% of the
        // whole fetch.
        with_tile(&self.cache, id, |tile| {
            if lx >= tile.width || ly >= tile.height {
                return miss;
            }
            if HALF {
                tile.rgb_half(lx, ly)
            } else {
                tile.rgb_u8(lx, ly, &self.to_linear)
            }
        })
        .unwrap_or(miss)
    }

    /// [`StreamingTexture::texel`] for a chart that carries alpha: the same
    /// fetch, four components out. Kept apart rather than folded in so the
    /// RGB fetch stays the closure it was (see `texel`).
    #[inline]
    fn texel_rgba<const HALF: bool>(
        &self,
        chart: &Chart,
        level: usize,
        li: &LevelInfo,
        x: usize,
        y: usize,
    ) -> [f32; 4] {
        let edge = chart.file.tile_edge();
        let (index, lx, ly) = li.locate(x, y, edge);
        let id = TileId {
            file: chart.id,
            level: level as u8,
            tile: index,
        };
        with_tile(&self.cache, id, |tile| {
            if lx >= tile.width || ly >= tile.height {
                return self.fallback;
            }
            // A tile whose alpha was opaque throughout is cached as RGB
            // (`TiledFile::read_tile`), and reads 1.0 there. Decided per
            // tile, in the alpha sampler only: the RGB fetch never asks.
            if !tile.data.alpha {
                let [r, g, b] = if HALF {
                    tile.rgb_half(lx, ly)
                } else {
                    tile.rgb_u8(lx, ly, &self.to_linear)
                };
                return [r, g, b, 1.0];
            }
            if HALF {
                tile.rgba_half(lx, ly)
            } else {
                tile.rgba_u8(lx, ly, &self.to_linear)
            }
        })
        .unwrap_or(self.fallback)
    }

    /// Trilinear lookup, level chosen from the footprint: the shared
    /// [`trilinear`] over this chart's levels, the same function the preloaded
    /// `UvTexture` runs, so the two select levels by one piece of code rather
    /// than by two kept in step.
    ///
    /// `ALPHA` is the chart's own alpha: whether its tiles are RGBA.
    #[inline(always)]
    fn sample_chart<const HALF: bool, const ALPHA: bool>(
        &self,
        chart: &Chart,
        u: f32,
        v: f32,
        width: f32,
    ) -> [f32; 4] {
        let source = ChartSource::<HALF, ALPHA> { tex: self, chart };
        trilinear(&source, u, v, width).expect("a streamed chart always answers")
    }

    /// [`StreamingTexture::sample_chart`] with the chart's alpha settled:
    /// compiled out for a texture with none (`ANY_ALPHA` false).
    #[inline(always)]
    fn sample_one<const HALF: bool, const ANY_ALPHA: bool>(
        &self,
        chart: &Chart,
        u: f32,
        v: f32,
        width: f32,
    ) -> [f32; 4] {
        if ANY_ALPHA && chart.alpha {
            self.sample_chart::<HALF, true>(chart, u, v, width)
        } else {
            self.sample_chart::<HALF, false>(chart, u, v, width)
        }
    }
}

/// One streamed chart as a [`MipSource`], its payload fixed by `HALF` and its
/// texel width by `ALPHA`.
struct ChartSource<'a, const HALF: bool, const ALPHA: bool> {
    tex: &'a StreamingTexture,
    chart: &'a Chart,
}

impl<const HALF: bool, const ALPHA: bool> MipSource for ChartSource<'_, HALF, ALPHA> {
    type Texel = [f32; 4];

    #[inline(always)]
    fn level_count(&self) -> usize {
        self.chart.file.level_count()
    }

    #[inline(always)]
    fn texels_across(&self) -> f32 {
        let l0 = self.chart.file.level(0);
        l0.width.max(l0.height) as f32
    }

    /// Bilinear lookup within one level, at coordinates already reduced to
    /// `[0, 1)`.
    ///
    /// The same taps and blend as `UvTexture`'s ([`Taps`]), including the `v`
    /// flip and the clamp-to-edge: the two are supposed to produce identical
    /// images, and a half-texel difference here would show up as a
    /// scene-wide shift that no test of either alone would catch.
    #[inline(always)]
    fn bilinear(&self, level: usize, u: f32, v: f32) -> Option<[f32; 4]> {
        let (tex, chart) = (self.tex, self.chart);
        let li = chart.file.level(level);
        let (w, h) = (li.width, li.height);
        let taps = Taps::new(u * w as f32 - 0.5, (1.0 - v) * h as f32 - 0.5, w, h);
        if ALPHA {
            let a = tex.texel_rgba::<HALF>(chart, level, &li, taps.x0, taps.y0);
            let b = tex.texel_rgba::<HALF>(chart, level, &li, taps.x1, taps.y0);
            let c = tex.texel_rgba::<HALF>(chart, level, &li, taps.x0, taps.y1);
            let d = tex.texel_rgba::<HALF>(chart, level, &li, taps.x1, taps.y1);
            return Some(taps.blend_rgba(a, b, c, d));
        }
        let a = tex.texel::<HALF>(chart, level, &li, taps.x0, taps.y0);
        let b = tex.texel::<HALF>(chart, level, &li, taps.x1, taps.y0);
        let c = tex.texel::<HALF>(chart, level, &li, taps.x0, taps.y1);
        let d = tex.texel::<HALF>(chart, level, &li, taps.x1, taps.y1);
        Some(taps.blend_rgb(a, b, c, d))
    }

    #[inline(always)]
    fn blend(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
        if ALPHA {
            lerp_rgba_alpha(a, b, t)
        } else {
            lerp_rgba(a, b, t)
        }
    }
}

impl Texture2D for StreamingTexture {
    /// The one place the payload is dispatched on: once per lookup rather than
    /// once per texel, and into a sampler monomorphised for that payload. See
    /// [`StreamingTexture::texel`] for what the alternatives cost. A texture
    /// with alpha leaves through one test into [`StreamingTexture::eval_alpha`].
    fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        let _p = crust_core::profile::scope(crust_core::profile::Section::Texture);
        let rgba = if self.alpha {
            self.eval_alpha(u, v, width)
        } else if self.linear {
            self.eval_as::<true, false>(u, v, width)
        } else {
            self.eval_as::<false, false>(u, v, width)
        };
        crate::to_working(self.gamut.as_ref(), rgba)
    }
}

impl StreamingTexture {
    /// [`Texture2D::eval`]'s samplers for a texture with alpha, kept out of
    /// line. Inlined beside the RGB ones they cost a texture without alpha
    /// ~6 instructions a lookup (callgrind, `materialx_basic` streamed), where
    /// out of line it pays the one test that sends it here.
    #[inline(never)]
    fn eval_alpha(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        if self.linear {
            self.eval_as::<true, true>(u, v, width)
        } else {
            self.eval_as::<false, true>(u, v, width)
        }
    }

    fn eval_as<const HALF: bool, const ANY_ALPHA: bool>(
        &self,
        u: f32,
        v: f32,
        width: f32,
    ) -> [f32; 4] {
        if !u.is_finite() || !v.is_finite() {
            return [0.0, 0.0, 0.0, 1.0];
        }
        let width = if width.is_finite() { width } else { 0.0 };
        if self.tiled {
            let (tu, tv) = (u.floor(), v.floor());
            if !(0.0..10.0).contains(&tu) || !(0.0..10.0).contains(&tv) {
                return [0.0, 0.0, 0.0, 1.0];
            }
            let number = udim_number(tu as u32, tv as u32);
            match self.charts.iter().find(|c| c.number == number) {
                Some(c) => self.sample_one::<HALF, ANY_ALPHA>(c, u - tu, v - tv, width),
                None => [0.0, 0.0, 0.0, 1.0],
            }
        } else {
            let wrap = |x: f32| x - x.floor();
            self.sample_one::<HALF, ANY_ALPHA>(&self.charts[0], wrap(u), wrap(v), width)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UvTexture;
    use crate::tiled::{TILE_EDGE, write_tx};
    use crust_core::Texture2D;

    /// The space conversions go to, as `--auto-tx` writes by default.
    fn acescg() -> crust_core::color::Space {
        crust_core::color::Space::named("ACEScg").expect("ACEScg")
    }

    fn cache(mib: usize) -> Arc<TileCache> {
        Arc::new(TileCache::new(
            (mib * 1024 * 1024) as u64,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ))
    }

    /// A noisy checker as a PNG in a fresh directory named for the test.
    /// High-frequency on purpose: a smooth ramp would agree under almost any
    /// filtering bug, while a checker with noise disagrees on a half-texel
    /// shift or a wrong mip level.
    fn checker_png(name: &str, w: usize, h: usize) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("crust_stream_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let src: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let c = if (x / 3 + y / 3) % 2 == 0 { 230 } else { 20 };
                [c, (x * 11 % 251) as u8, (y * 17 % 251) as u8]
            })
            .collect();
        let png = dir.join("src.png");
        image::RgbImage::from_raw(w as u32, h as u32, src)
            .expect("image")
            .save(&png)
            .expect("write png");
        png
    }

    /// `src` converted as `--auto-tx` converts it for `space`, and opened
    /// streamed as the lookup opens it: in the space its name says.
    fn converted(
        src: &std::path::Path,
        space: ColorSpace,
        cache: Arc<TileCache>,
    ) -> StreamingTexture {
        let made = crate::tiled::make_tx_atomic(src, space, acescg()).expect("convert");
        let name = crate::tiled::tx_name(src, space, acescg()).expect("named");
        assert_eq!(made.dst, name.path);
        StreamingTexture::open(&made.dst, name.read_as, cache).expect("stream")
    }

    /// Level 0 at every texel centre, all four components, within design
    /// decision 7's bounds: per channel a mean absolute difference of at most
    /// 0.005 and a 99th percentile of at most 0.02.
    fn assert_level0_agrees(a: &dyn Texture2D, b: &dyn Texture2D, w: usize, h: usize) {
        for k in 0..4 {
            let mut diffs: Vec<f32> = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let (u, v) = (
                        (x as f32 + 0.5) / w as f32,
                        1.0 - (y as f32 + 0.5) / h as f32,
                    );
                    (a.eval(u, v, 0.0)[k] - b.eval(u, v, 0.0)[k]).abs()
                })
                .collect();
            diffs.sort_by(f32::total_cmp);
            let mean = diffs.iter().sum::<f32>() / diffs.len() as f32;
            let p99 = diffs[diffs.len() * 99 / 100];
            assert!(
                mean <= 0.005 && p99 <= 0.02,
                "channel {k}: mean {mean}, p99 {p99}"
            );
        }
    }

    /// **The A/B the streaming path rests on.** A source converted as
    /// `--auto-tx` converts it streams what the same source preloads, within
    /// tolerance: an sRGB albedo through a `half` EXR of linear ACEScg, a raw
    /// one through an 8-bit TIFF of its own bytes.
    #[test]
    fn a_converted_tx_streams_what_its_source_preloads() {
        for (name, space, (w, h)) in [
            ("agree_srgb", crust_core::ColorSpace::SRGB, (256, 192)),
            ("agree_raw", crust_core::ColorSpace::RAW, (150, 100)),
        ] {
            let png = checker_png(name, w, h);
            let pre = UvTexture::open_with(&png, space, true).expect("preload");
            let stream = converted(&png, space, cache(64));
            assert_eq!(stream.size(), pre.tile_size());
            assert_level0_agrees(&stream, &pre, w, h);
            let _ = std::fs::remove_dir_all(png.parent().unwrap());
        }
    }

    /// Where the file holds the source's own bytes — a raw 8-bit source —
    /// level 0 agrees exactly, hard against the right and bottom edges too,
    /// where the last tile column is 22 texels wide against a nominal 64 and
    /// a wrong stride would shear it.
    #[test]
    fn a_raw_tx_reads_its_source_s_texels_across_clipped_edge_tiles() {
        let png = checker_png("clipped", 150, 100);
        let pre = UvTexture::open_with(&png, crust_core::ColorSpace::RAW, true).expect("preload");
        let stream = converted(&png, crust_core::ColorSpace::RAW, cache(16));
        for k in 0..40 {
            let t = k as f32 / 39.0;
            for &(u, v) in &[(t, 0.999), (0.999, t), (t, 0.001), (0.001, t)] {
                assert_eq!(
                    stream.eval(u, v, 0.0),
                    pre.eval(u, v, 0.0),
                    "edge ({u}, {v})"
                );
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
        let png = checker_png("thrash", 1024, 1024);
        let roomy = converted(&png, crust_core::ColorSpace::RAW, cache(64));
        let tiny = Arc::new(TileCache::new(1, crust_core::DEFAULT_TEX_MAX_OPEN_FILES));
        let tx = crate::tiled::tx_name(&png, crust_core::ColorSpace::RAW, acescg())
            .expect("named")
            .path;
        let stream =
            StreamingTexture::open(&tx, crust_core::ColorSpace::RAW, tiny.clone()).expect("stream");
        for i in 0..31 {
            for j in 0..31 {
                let (u, v) = (i as f32 / 30.0, j as f32 / 30.0);
                assert_eq!(stream.eval(u, v, 0.0), roomy.eval(u, v, 0.0), "({u}, {v})");
            }
        }
        let c = tiny.counters();
        assert!(c.evictions > 0, "the budget should have forced evictions");
        assert!(
            c.redundant > 0,
            "a budget this small should have re-read tiles"
        );
        assert_eq!(c.errors, 0);
        let _ = std::fs::remove_dir_all(png.parent().unwrap());
    }

    /// **The claim the EXR backing exists for.**
    ///
    /// A texture with highlights at 8.0 keeps them when streamed and loses
    /// them when preloaded, because the preload path decodes every source
    /// through `to_rgb8()` and an 8-bit tile has nowhere to put a value above
    /// 1.0. Below 1.0 the two still agree to 8-bit quantisation, which is what
    /// makes this a statement about *range* rather than about two unrelated
    /// images.
    #[test]
    fn hdr_survives_streaming_and_does_not_survive_preloading() {
        let dir = std::env::temp_dir().join("crust_stream_hdr");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // Radiance RGBE, because that is a float format the *preload* path can
        // read — it goes through `image`, which has no EXR decoder here. Its
        // shared exponent is lossy, so the comparison below is against what the
        // file actually holds rather than against what was written to it.
        let (w, h) = (64usize, 64usize);
        let authored: Vec<f32> = (0..w * h)
            .flat_map(|i| {
                let bright = (i % 8) < 4;
                if bright {
                    [8.0f32, 8.0, 8.0]
                } else {
                    [0.25, 0.5, 0.75]
                }
            })
            .collect();
        let hdr = dir.join("src.hdr");
        {
            let file = std::fs::File::create(&hdr).expect("create");
            let pixels: Vec<image::Rgb<f32>> = authored
                .as_chunks::<3>()
                .0
                .iter()
                .copied()
                .map(image::Rgb)
                .collect();
            image::codecs::hdr::HdrEncoder::new(std::io::BufWriter::new(file))
                .encode(&pixels, w, h)
                .expect("write hdr");
        }
        // What the file holds, which is what both paths are given.
        let decoded = image::ImageReader::open(&hdr)
            .expect("open")
            .with_guessed_format()
            .expect("format")
            .decode()
            .expect("decode")
            .to_rgb32f()
            .into_raw();

        let tx = dir.join("src.tx");
        crate::tiled::write_tx_exr(&tx, &decoded, w, h).expect("write tx");

        let pre = UvTexture::open_with(&hdr, crust_core::ColorSpace::RAW, true).expect("preload");
        let cache = Arc::new(TileCache::new(
            8 * 1024 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));
        let stream =
            StreamingTexture::open(&tx, crust_core::ColorSpace::RAW, cache).expect("stream");
        assert!(stream.is_linear(), "an EXR backing pages in half tiles");

        // Point-sampled at texel centres, so no interpolation blurs the two
        // populations into each other.
        let at = |x: usize, y: usize| {
            let (u, v) = (
                (x as f32 + 0.5) / w as f32,
                1.0 - (y as f32 + 0.5) / h as f32,
            );
            (stream.eval(u, v, 0.0), pre.eval(u, v, 0.0), {
                let o = (y * w + x) * 3;
                [decoded[o], decoded[o + 1], decoded[o + 2]]
            })
        };

        let (bright_s, bright_p, bright_src) = at(1, 3);
        assert!(bright_src[0] > 4.0, "the fixture must actually be HDR");
        for k in 0..3 {
            assert!(
                (bright_s[k] - bright_src[k]).abs() < 0.01 * bright_src[k],
                "streamed channel {k}: {} against {}",
                bright_s[k],
                bright_src[k]
            );
            assert_eq!(bright_p[k], 1.0, "preloaded channel {k} must clip to 1.0");
        }

        let (dim_s, dim_p, dim_src) = at(5, 3);
        for k in 0..3 {
            assert!(
                (dim_s[k] - dim_src[k]).abs() < 0.002,
                "streamed channel {k}: {} against {}",
                dim_s[k],
                dim_src[k]
            );
            // Below 1.0 the two are the same image to within the 8 bits the
            // preload path keeps — so the divergence above really is the range
            // and not a different lookup.
            assert!(
                (dim_s[k] - dim_p[k]).abs() <= 1.0 / 255.0,
                "channel {k}: streamed {} against preloaded {}",
                dim_s[k],
                dim_p[k]
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An RGBA PNG whose colour is a noisy checker and whose alpha is a
    /// diagonal cut with a soft band, so both the colour and every level of
    /// the alpha disagree under a filtering bug.
    fn rgba_png(dir: &std::path::Path, name: &str, w: usize, h: usize) -> std::path::PathBuf {
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let c = if (x / 3 + y / 3) % 2 == 0 { 230 } else { 20 };
                let a = ((x + y) * 255 / (w + h)).min(255) as u8;
                let a = if a < 100 { 0 } else { a };
                [c, (x * 11 % 251) as u8, (y * 17 % 251) as u8, a]
            })
            .collect();
        let png = dir.join(name);
        image::RgbaImage::from_raw(w as u32, h as u32, rgba)
            .expect("image")
            .save(&png)
            .expect("write png");
        png
    }

    /// **The A/B, with alpha.** An RGBA source whose alpha cuts, converted
    /// as `--auto-tx` converts it — premultiplied, in a `half` EXR — streams
    /// what it preloads within tolerance, alpha included. Odd sizes, so the
    /// clipped edge tiles are covered.
    #[test]
    fn a_converted_cutout_streams_what_its_source_preloads() {
        let dir = std::env::temp_dir().join("crust_stream_alpha");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        for (w, h) in [(256usize, 192usize), (150, 100)] {
            let png = rgba_png(&dir, &format!("leaf_{w}.png"), w, h);
            let space = crust_core::ColorSpace::SRGB;
            let pre = UvTexture::open_with(&png, space, true).expect("preload");
            let stream = converted(&png, space, cache(64));
            assert!(stream.has_alpha() && pre.has_alpha());
            assert_level0_agrees(&stream, &pre, w, h);
            // Not vacuous: the alpha really varies across the image.
            assert_eq!(pre.eval(0.01, 0.99, 0.0)[3], 0.0);
            assert!(pre.eval(0.99, 0.01, 0.0)[3] > 0.95);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A UDIM set converted tile by tile, where one tile's alpha cuts and
    /// another's is opaque: the opaque one is written RGB, so the charts
    /// differ, and the streamed set reads what the preloaded one does.
    /// A UDIM set of an 8-bit and a 16-bit tile, bound raw, converts to a `u8`
    /// and a `half` `.tx` (each kept at its source depth). One sampler cannot
    /// read both, so the set declines to stream and preloads.
    #[test]
    fn a_udim_set_of_mixed_depths_preloads() {
        let dir = std::env::temp_dir().join("crust_stream_mixed_depth_udim");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        image::RgbImage::from_pixel(64, 64, image::Rgb([200, 100, 50]))
            .save(dir.join("t.1001.png"))
            .expect("8-bit png");
        image::ImageBuffer::<image::Rgb<u16>, _>::from_pixel(
            64,
            64,
            image::Rgb([9000, 30000, 60000]),
        )
        .save(dir.join("t.1002.png"))
        .expect("16-bit png");
        for tile in ["t.1001.png", "t.1002.png"] {
            crate::tiled::make_tx_atomic(&dir.join(tile), crust_core::ColorSpace::RAW, acescg())
                .expect("convert");
        }
        let depth = |tile: &str| TiledFile::open(&dir.join(tile)).expect("open").is_linear();
        assert_eq!(
            [depth("t.1001_raw.png.tx"), depth("t.1002_raw.png.tx")],
            [false, true],
            "the fixture is one u8 and one half .tx"
        );
        assert!(
            StreamingTexture::open(
                &dir.join("t.<UDIM>_raw.png.tx"),
                crust_core::ColorSpace::RAW,
                cache(8),
            )
            .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_udim_set_streams_alpha_per_chart() {
        let dir = std::env::temp_dir().join("crust_stream_alpha_udim");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cut = rgba_png(&dir, "t.1002.png", 64, 64);
        image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 100, 50, 255]))
            .save(dir.join("t.1001.png"))
            .expect("png");
        let mut kept = Vec::new();
        for src in [dir.join("t.1001.png"), cut] {
            let made = crate::tiled::make_tx_atomic(&src, crust_core::ColorSpace::RAW, acescg())
                .expect("convert");
            kept.push(made.alpha);
        }
        assert_eq!(kept, [false, true], "an opaque alpha is not written");
        let set = dir.join("t.<UDIM>.png");
        let pre = UvTexture::open_with(&set, crust_core::ColorSpace::RAW, true).expect("preload");
        let stream = StreamingTexture::open(
            &dir.join("t.<UDIM>_raw.png.tx"),
            crust_core::ColorSpace::RAW,
            cache(8),
        )
        .expect("stream");
        assert_eq!(stream.chart_count(), 2);
        for i in 0..20 {
            let u = i as f32 / 10.0 + 0.01;
            for &v in &[0.1f32, 0.5, 0.9] {
                let (s, p) = (stream.eval(u, v, 0.0), pre.eval(u, v, 0.0));
                for k in 0..4 {
                    assert!(
                        (s[k] - p[k]).abs() <= 2.0 / 255.0,
                        "({u}, {v}): {s:?} against {p:?}"
                    );
                }
            }
        }
        assert_eq!(stream.eval(0.5, 0.5, 0.0)[3], 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that declares alpha but never cuts — an OIIO `.tx` of an RGBA
    /// image whose alpha is opaque — is cached as the RGB it is, tile by
    /// tile, and reads exactly what the same image without alpha reads. The
    /// cache would otherwise hold a third more bytes for nothing.
    #[test]
    fn a_tile_whose_alpha_is_opaque_is_cached_as_rgb() {
        let dir = std::env::temp_dir().join("crust_stream_opaque_tiles");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (w, h) = (150usize, 100usize);
        let rgb: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % 251) as u8, (i * 7 % 253) as u8, (i / 3 % 256) as u8])
            .collect();
        let rgba: Vec<u8> = rgb
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|t| [t[0], t[1], t[2], 255])
            .collect();
        let space = crust_core::ResolvedColorSpace::SRGB;
        let floats = |v: &[u8]| v.iter().map(|&b| b as f32 / 255.0).collect::<Vec<f32>>();
        // Written directly, as a tool that keeps an opaque alpha would.
        let pairs = [
            (dir.join("rgba.tx"), dir.join("rgb.tx"), false),
            (dir.join("rgba_exr.tx"), dir.join("rgb_exr.tx"), true),
        ];
        crate::tiled::write_tx_rgba(&pairs[0].0, &rgba, w, h).expect("rgba");
        write_tx(&pairs[0].1, &rgb, w, h).expect("rgb");
        crate::tiled::write_tx_exr_rgba(&pairs[1].0, &floats(&rgba), w, h).expect("rgba");
        crate::tiled::write_tx_exr(&pairs[1].1, &floats(&rgb), w, h).expect("rgb");

        for (with, without, exr) in &pairs {
            let file = TiledFile::open(with).expect("open");
            assert!(file.has_alpha(), "the file declares alpha");
            let mut r = file.reader().expect("reader");
            for level in 0..file.level_count() {
                let tile = file.read_tile(&mut r, level, 0).expect("tile");
                let (tw, th) = file.level(level).tile_size(0, TILE_EDGE);
                assert!(
                    !tile.alpha,
                    "level {level}: an opaque tile is cached as RGB"
                );
                assert_eq!(tile.len(), tw * th * 3);
            }
            let cache = || {
                Arc::new(TileCache::new(
                    16 * 1024 * 1024,
                    crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
                ))
            };
            let a = StreamingTexture::open(with, space.into(), cache()).expect("stream");
            let b = StreamingTexture::open(without, space.into(), cache()).expect("stream");
            assert!(a.has_alpha() && !b.has_alpha());
            for &width in &[0.0f32, 0.01, 0.05, 0.3, 2.0] {
                for i in 0..23 {
                    for j in 0..17 {
                        let (u, v) = (i as f32 / 22.0, j as f32 / 16.0);
                        assert_eq!(
                            a.eval(u, v, width),
                            b.eval(u, v, width),
                            "exr {exr} at ({u}, {v}) width {width}"
                        );
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cutting `.tx` whose opaque tiles are cached as RGB still streams
    /// what its source preloads: the RGB tiles read 1.0, as `ALPHA_U8[255]`
    /// does.
    #[test]
    fn a_cutout_with_opaque_tiles_streams_what_its_source_preloads() {
        let dir = std::env::temp_dir().join("crust_stream_mixed_tiles");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (w, h) = (256u32, 192u32);
        // Cut on the left 40 columns, opaque elsewhere: level 0's first tile
        // column cuts, its others are opaque, and the coarse levels mix.
        let png = dir.join("card.png");
        image::RgbaImage::from_fn(w, h, |x, y| {
            let c = if (x / 3 + y / 3) % 2 == 0 { 230 } else { 20 };
            image::Rgba([
                c,
                (x * 11 % 251) as u8,
                (y * 17 % 251) as u8,
                if x < 40 { 0 } else { 255 },
            ])
        })
        .save(&png)
        .expect("png");
        let space = crust_core::ColorSpace::SRGB;
        let made = crate::tiled::make_tx_atomic(&png, space, acescg()).expect("convert");
        assert!(made.alpha);
        let file = TiledFile::open(&made.dst).expect("open");
        let mut r = file.reader().expect("reader");
        assert!(
            file.read_tile(&mut r, 0, 0).expect("tile").alpha,
            "tile 0 cuts"
        );
        assert!(
            !file.read_tile(&mut r, 0, 1).expect("tile").alpha,
            "tile 1 is opaque"
        );

        let pre = UvTexture::open_with(&png, space, true).expect("preload");
        let stream = converted(&png, space, cache(16));
        assert_level0_agrees(&stream, &pre, w as usize, h as usize);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unopenable file declines rather than producing a black texture, so
    /// the caller can fall back to the preload path.
    #[test]
    fn a_missing_or_stripped_file_declines() {
        let cache = Arc::new(TileCache::new(
            1024 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));
        assert!(
            StreamingTexture::open(
                std::path::Path::new("/definitely/not/here.tx"),
                crust_core::ColorSpace::RAW,
                cache,
            )
            .is_none()
        );
    }
}
