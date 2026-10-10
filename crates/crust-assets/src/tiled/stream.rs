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

/// What UsdUVTexture's `sourceColorSpace = "auto"` means for one `.tx`.
///
/// A recorded `crust:mipspace` marker wins: it names the space the file was
/// converted under, which is the decision `auto` would have made about the
/// *source* — the `.tx` itself is always 8-bit RGB(A) or half, so its own
/// format no longer says whether the source was a greyscale mask. Without a
/// marker (an OIIO `maketx` file) the format decides, by the same rule the
/// preload path applies: half is linear, 8-bit RGB or RGBA is sRGB.
fn resolve_auto_space(f: &TiledFile, working: crust_core::color::Space) -> ResolvedColorSpace {
    let named = f
        .mip_space()
        .and_then(crust_core::color::Space::named)
        .map(|s| ResolvedColorSpace::new(s, working));
    named.unwrap_or_else(|| {
        ColorSpace::AUTO
            .into_working(working)
            .resolve_auto(!f.is_linear(), 3)
    })
}

impl StreamingTexture {
    /// Opens a `.tx` (or a `<UDIM>` / `<UVTILE>` set of them) against `cache`.
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
            let opened = TiledFile::open(&p).map(|f| {
                let want = crate::tiled::space_name(settle(&f));
                (f, want)
            });
            match opened {
                Ok((f, want_space)) if !f.mip_space_matches(want_space) => {
                    crust_core::warning!(
                        TextureStreamFallback,
                        "{}: mip chain was reduced in {:?}, not {want_space} — \
                         falling back to the preloaded texture",
                        p.display(),
                        f.mip_space()
                    );
                    return None;
                }
                Ok((f, _)) => {
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
    use crate::tiled::write_tx;

    /// Writes a `.tx` and the equivalent PNG, so the two paths can be pointed
    /// at the same image.
    fn pair(
        name: &str,
        w: usize,
        h: usize,
        space: crust_core::ResolvedColorSpace,
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
        let (png, tx) = pair("agree", 256, 192, crust_core::ResolvedColorSpace::SRGB);
        let pre = UvTexture::open_with(&png, crust_core::ColorSpace::SRGB, true).expect("preload");
        let cache = Arc::new(TileCache::new(
            64 * 1024 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));
        let stream =
            StreamingTexture::open(&tx, crust_core::ColorSpace::SRGB, cache).expect("stream");

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
        let (png, tx) = pair("clipped", 150, 100, crust_core::ResolvedColorSpace::RAW);
        let pre = UvTexture::open_with(&png, crust_core::ColorSpace::RAW, true).expect("preload");
        let cache = Arc::new(TileCache::new(
            16 * 1024 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));
        let stream =
            StreamingTexture::open(&tx, crust_core::ColorSpace::RAW, cache).expect("stream");

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
        let (png, tx) = pair("thrash", 1024, 1024, crust_core::ResolvedColorSpace::SRGB);
        let pre = UvTexture::open_with(&png, crust_core::ColorSpace::SRGB, true).expect("preload");
        let cache = Arc::new(TileCache::new(1, crust_core::DEFAULT_TEX_MAX_OPEN_FILES));
        let stream = StreamingTexture::open(&tx, crust_core::ColorSpace::SRGB, cache.clone())
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
        crate::tiled::write_tx_exr(&tx, &decoded, w, h, crust_core::ResolvedColorSpace::RAW)
            .expect("write tx");

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

    /// The colour-space marker discriminates on the EXR backing too, where it
    /// means something slightly different — "the space this file's linear
    /// samples were decoded from" rather than "the space its levels were
    /// averaged in". Binding it under another space would put a transfer curve
    /// on data that has already had one removed.
    #[test]
    fn an_exr_backing_also_refuses_the_wrong_colour_space() {
        let dir = std::env::temp_dir().join("crust_stream_exrspace");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let tx = dir.join("s.tx");
        let (w, h) = (32usize, 32usize);
        let src: Vec<f32> = (0..w * h * 3).map(|i| (i % 17) as f32 / 16.0).collect();
        crate::tiled::write_tx_exr(&tx, &src, w, h, crust_core::ResolvedColorSpace::SRGB)
            .expect("write");

        let cache = Arc::new(TileCache::new(
            4 * 1024 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));
        assert_eq!(
            TiledFile::open(&tx).expect("open").mip_space(),
            Some("srgb_texture")
        );
        assert!(StreamingTexture::open(&tx, crust_core::ColorSpace::SRGB, cache.clone()).is_some());
        assert!(StreamingTexture::open(&tx, crust_core::ColorSpace::RAW, cache).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A marked tiled EXR binds the same preloaded as streamed: under `auto`
    /// both read its `crust:mipspace` marker, take its values as already
    /// linear on that space's primaries, and apply the change into the working
    /// space after filtering. Off Rec.709 a preload that ignored the marker
    /// would leave the values on the wrong primaries.
    #[test]
    fn a_marked_exr_binds_the_same_preloaded_and_streamed() {
        let dir = std::env::temp_dir().join("crust_stream_exr_marked");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let exr = dir.join("marked.exr");
        let (w, h) = (32usize, 32usize);
        let src: Vec<f32> = (0..w * h)
            .flat_map(|i| [(i % 7) as f32 / 6.0, (i % 5) as f32 / 4.0, 0.25])
            .collect();
        crate::tiled::write_tx_exr(&exr, &src, w, h, crust_core::ResolvedColorSpace::SRGB)
            .expect("write");

        let acescg = crust_core::color::working_space("acescg").expect("acescg");
        let auto = crust_core::ColorSpace::AUTO.into_working(acescg);
        let pre = UvTexture::open_with(&exr, auto, false).expect("preload");
        let cache = Arc::new(TileCache::new(
            4 * 1024 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));
        let stream = StreamingTexture::open(&exr, auto, cache).expect("stream");
        let gamut =
            crust_core::ResolvedColorSpace::new(crust_core::color::Space::SRGB_TEXTURE, acescg)
                .gamut()
                .expect("Rec.709 to ACEScg is a change of primaries");

        for &(u, v) in &[(0.1, 0.2), (0.5, 0.5), (0.73, 0.31), (0.9, 0.95)] {
            let p = pre.eval(u, v, 0.0);
            let s = stream.eval(u, v, 0.0);
            for k in 0..3 {
                assert!(
                    (p[k] - s[k]).abs() <= 1e-5,
                    "({u}, {v}) channel {k}: preloaded {p:?} against streamed {s:?}"
                );
            }
            // And the matrix really was applied: the stored value is on
            // Rec.709 primaries.
            let raw = UvTexture::open_with(&exr, crust_core::ColorSpace::RAW, false)
                .expect("raw")
                .eval(u, v, 0.0);
            let want = crust_core::color::apply_gamut(
                Some(&gamut),
                crust_core::Vec3A::new(raw[0], raw[1], raw[2]),
            );
            assert!(
                (crust_core::Vec3A::new(p[0], p[1], p[2]) - want)
                    .abs()
                    .max_element()
                    <= 1e-5,
                "({u}, {v}): {p:?} against {want}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
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
        let (png, tx) = pair("space", 128, 128, crust_core::ResolvedColorSpace::SRGB);
        let cache = Arc::new(TileCache::new(
            4 * 1024 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));

        // Same space: opens.
        assert!(StreamingTexture::open(&tx, crust_core::ColorSpace::SRGB, cache.clone()).is_some());
        // Different space: declines rather than serving a chain reduced for
        // the other one.
        assert!(StreamingTexture::open(&tx, crust_core::ColorSpace::RAW, cache.clone()).is_none());
        assert!(StreamingTexture::open(&tx, crust_core::ColorSpace::GAMMA22, cache).is_none());

        // And the guard really is load-bearing: had it not fired, the levels
        // would have differed from what a raw-decoded preload builds.
        let pre_raw = UvTexture::open_with(&png, crust_core::ColorSpace::RAW, true).expect("pre");
        let pre_srgb = UvTexture::open_with(&png, crust_core::ColorSpace::SRGB, true).expect("pre");
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
        let (png, tx) = pair("unmarked", 96, 96, crust_core::ResolvedColorSpace::SRGB);
        let tf = TiledFile::open(&tx).expect("open");
        assert_eq!(tf.mip_space(), Some("srgb_texture"));
        assert!(tf.mip_space_matches("srgb_texture"));
        assert!(!tf.mip_space_matches("raw"));

        let _ = std::fs::remove_dir_all(png.parent().unwrap());
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

    /// **The invariant, with alpha.** A `.tx` converted from an RGBA source
    /// — what `--auto-tx` and `maketx` write — streams exactly what the
    /// source preloads, alpha included, at every level: the same decode
    /// (`ALPHA_U8`), the same reduction, the same blends. Odd sizes, so the
    /// clipped edge tiles and the area-weighted odd levels are covered.
    #[test]
    fn streamed_alpha_agrees_bit_for_bit_with_preloaded() {
        let dir = std::env::temp_dir().join("crust_stream_alpha");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        for (w, h) in [(256usize, 192usize), (150, 100)] {
            let png = rgba_png(&dir, &format!("leaf_{w}.png"), w, h);
            let tx = dir.join(format!("leaf_{w}.tx"));
            let made = crate::tiled::make_tx(
                &png,
                &tx,
                crust_core::ColorSpace::AUTO,
                crate::tiled::TxFormat::Tiff,
            )
            .expect("convert");
            assert!(made.alpha, "an RGBA source keeps its alpha");
            let space = crust_core::ColorSpace::SRGB;
            let pre = UvTexture::open_with(&png, space, true).expect("preload");
            let cache = Arc::new(TileCache::new(
                64 * 1024 * 1024,
                crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
            ));
            let stream = StreamingTexture::open(&tx, space, cache).expect("stream");
            assert!(stream.has_alpha() && pre.has_alpha());
            assert_eq!(stream.level_count(), pre.level_count());
            for &width in &[0.0f32, 0.001, 0.004, 0.02, 0.09, 0.3, 0.7, 2.0, 9.0] {
                for i in 0..23 {
                    for j in 0..17 {
                        let (u, v) = (i as f32 / 22.0, j as f32 / 16.0);
                        assert_eq!(
                            stream.eval(u, v, width),
                            pre.eval(u, v, width),
                            "{w}x{h} at ({u}, {v}) width {width}"
                        );
                    }
                }
                for k in 0..40 {
                    let t = k as f32 / 39.0;
                    for &(u, v) in &[(t, 0.999), (0.999, t)] {
                        assert_eq!(stream.eval(u, v, width), pre.eval(u, v, width));
                    }
                }
            }
            // Not vacuous: the alpha really varies across the image.
            assert_eq!(pre.eval(0.01, 0.99, 0.0)[3], 0.0);
            assert!(pre.eval(0.99, 0.01, 0.0)[3] > 0.95);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The EXR backing carries the alpha as an `A` channel: a `half` of
    /// `a / 255`, so it agrees with the preloaded byte to `half` precision,
    /// and is never put through the colour space's curve.
    #[test]
    fn an_exr_backing_streams_the_alpha_too() {
        let dir = std::env::temp_dir().join("crust_stream_alpha_exr");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let png = rgba_png(&dir, "leaf.png", 64, 64);
        let tx = dir.join("leaf.tx");
        let made = crate::tiled::make_tx(
            &png,
            &tx,
            crust_core::ColorSpace::SRGB,
            crate::tiled::TxFormat::Exr,
        )
        .expect("convert");
        assert_eq!(made.kind, "half, exr");
        assert!(made.alpha);
        let space = crust_core::ColorSpace::SRGB;
        let pre = UvTexture::open_with(&png, space, true).expect("preload");
        let cache = Arc::new(TileCache::new(
            8 * 1024 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));
        let stream = StreamingTexture::open(&tx, space, cache).expect("stream");
        assert!(stream.is_linear() && stream.has_alpha());
        for (x, y) in [(1usize, 62usize), (40, 40), (63, 0), (20, 30)] {
            let (u, v) = ((x as f32 + 0.5) / 64.0, 1.0 - (y as f32 + 0.5) / 64.0);
            let (s, p) = (stream.eval(u, v, 0.0), pre.eval(u, v, 0.0));
            for k in 0..4 {
                assert!(
                    (s[k] - p[k]).abs() <= 1e-3,
                    "texel ({x}, {y}) channel {k}: streamed {s:?} against preloaded {p:?}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A UDIM set converted tile by tile, where one tile's alpha cuts and
    /// another's is opaque: the opaque one is written RGB, so the charts
    /// differ, and the streamed set still agrees with the preloaded one.
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
            let made = crate::tiled::make_tx_atomic(
                &src,
                crust_core::ColorSpace::RAW,
                crate::tiled::TxFormat::FromSampleType,
            )
            .expect("convert");
            kept.push(made.alpha);
        }
        assert_eq!(kept, [false, true], "an opaque alpha is not written");
        let set = dir.join("t.<UDIM>.png");
        let pre = UvTexture::open_with(&set, crust_core::ColorSpace::RAW, true).expect("preload");
        let cache = Arc::new(TileCache::new(
            8 * 1024 * 1024,
            crust_core::DEFAULT_TEX_MAX_OPEN_FILES,
        ));
        let stream =
            StreamingTexture::open(&dir.join("t.<UDIM>.tx"), crust_core::ColorSpace::RAW, cache)
                .expect("stream");
        assert_eq!(stream.chart_count(), 2);
        for &width in &[0.0f32, 0.05, 0.5] {
            for i in 0..20 {
                let u = i as f32 / 10.0 + 0.01;
                for &v in &[0.1f32, 0.5, 0.9] {
                    assert_eq!(
                        stream.eval(u, v, width),
                        pre.eval(u, v, width),
                        "({u}, {v})"
                    );
                }
            }
        }
        assert_eq!(stream.eval(0.5, 0.5, 0.0)[3], 1.0);
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
