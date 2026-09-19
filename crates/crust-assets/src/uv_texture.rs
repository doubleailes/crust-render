//!! UV-addressed textures, single image or UDIM set, decoded at load.
//!!
//!! The host's implementation of `crust_core::Texture2D`. It answers the same
//!! shape of problem as the Ptex decoder — a path tracer asks for texels from
//!! every worker in an unpredictable order, so everything is decoded once at
//!! load into an immutable buffer — but the addressing is different, and so is
//!! the memory arithmetic.
//!!
//! **UDIM.** MaterialX addresses a tile set as `Albedo.<UDIM>.png`, where the
//! tile number is `1001 + u + 10·v` over integer chart coordinates. The teapot's
//! ceramic body spans 14 such tiles at 4K. Expanding the token and deciding how
//! many tiles to hold is the host's job, which is why `Texture2D::eval` takes
//! *unwrapped* coordinates: `u = 3.4` has to still know it is the fourth tile.
//!
//! **And `<UVTILE>`.** MaterialX defines a second spelling of the same grid:
//! `Albedo.<UVTILE>.png`, expanding to `u1_v1` for the tile `<UDIM>` calls
//! 1001 (both indices 1-based, the Mari/Mudbox convention). It addresses the
//! same tiles by the same coordinates, so only the name on disk differs and
//! [`TileToken`] is the whole of the difference — tiles are keyed by UDIM
//! number internally whichever token named them. The document parser already
//! carries both tokens through intact (`crust-mtlx`'s `escape_udim_tokens`),
//! so failing to expand one here meant a set that loaded *no* tiles at all
//! rather than one that loaded them wrongly.
//!
//! **Why a resolution cap is not an optimisation.** Fourteen 4096² tiles is
//! 235 M texels; at 3 bytes each that is 674 MiB for one map, and the ceramic
//! alone binds four maps across two materials — before the metal's 8K handle
//! textures. Decoding them at full resolution is several gigabytes for a 640×360
//! render that resolves nothing near it. `DEFAULT_MAX_EDGE` caps each tile's
//! edge length, box-filtering down by an integer factor at load, which brings
//! the same set to tens of MiB. `CRUST_TEX_MAX` overrides it.
//!
//! **Why 8-bit storage.** These are PNGs: the file has 8 bits a channel and
//! nothing recovers precision that was never there. Keeping them as `u8` and
//! converting on lookup through a 256-entry table costs one indexed load per
//! channel and is 4x smaller than `f32`, which at this scale is the difference
//! between fitting in cache and not.

use crust_core::{ColorSpace, Texture2D};
use std::path::Path;
use tracing::error;

/// One decoded UDIM tile.
struct Tile {
    /// UDIM number, `1001 + u + 10·v`.
    number: u32,
    /// Row-major RGB, 3 bytes a texel.
    pixels: Vec<u8>,
    width: usize,
    height: usize,
}

/// The filename token that addresses a tile set, and how it spells a tile.
///
/// Two spellings, one grid: a document may use either, and both index the
/// same `(u, v)` chart coordinates. Keeping the distinction in one place is
/// what lets everything downstream — the tile key, the sampler, the 10x10
/// bound — stay written in UDIM numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TileToken {
    /// `<UDIM>` → `1001 + u + 10·v`, e.g. `Albedo.1012.png`.
    Udim,
    /// `<UVTILE>` → `u<u+1>_v<v+1>`, e.g. `Albedo.u2_v2.png`. Both indices
    /// are 1-based, so `u1_v1` is the same tile as UDIM 1001.
    UvTile,
}

impl TileToken {
    /// The token this file name carries, or `None` for a single image.
    ///
    /// `<UDIM>` is tested first only to be deterministic about a name that
    /// carries both, which no conformant document writes.
    fn detect(name: &str) -> Option<TileToken> {
        if name.contains("<UDIM>") {
            Some(TileToken::Udim)
        } else if name.contains("<UVTILE>") {
            Some(TileToken::UvTile)
        } else {
            None
        }
    }

    /// The file name of the tile at zero-based chart coordinates `(u, v)`.
    fn expand(self, name: &str, u: u32, v: u32) -> String {
        match self {
            TileToken::Udim => name.replace("<UDIM>", &udim_number(u, v).to_string()),
            TileToken::UvTile => name.replace("<UVTILE>", &format!("u{}_v{}", u + 1, v + 1)),
        }
    }

    /// The token as authored, for the "nothing found" message.
    fn as_str(self) -> &'static str {
        match self {
            TileToken::Udim => "<UDIM>",
            TileToken::UvTile => "<UVTILE>",
        }
    }
}

/// The UDIM number of the tile at zero-based chart coordinates.
///
/// The internal tile key, whichever token named the file.
fn udim_number(u: u32, v: u32) -> u32 {
    1001 + u + 10 * v
}

/// A UV-addressed texture: one image, or a tile set.
pub struct UvTexture {
    tiles: Vec<Tile>,
    /// Per-channel decode table, `u8` → linear `f32`. Holds the inverse sRGB
    /// EOTF for a display-encoded file and a plain `/255` otherwise, so the
    /// colour-space decision is made once at load and the lookup does not
    /// branch on it.
    to_linear: [f32; 256],
    /// Representative tile size, for the load message.
    width: usize,
    height: usize,
    /// True when the file name carried a tile token (`<UDIM>` or `<UVTILE>`).
    /// A tile set addresses tiles by the integer part of `(u, v)`; a single
    /// image wraps instead, which is MaterialX's default `periodic` address
    /// mode.
    tiled: bool,
}

/// Largest tile edge kept, unless `CRUST_TEX_MAX` says otherwise.
///
/// 1024 rather than full resolution for the reason in the module note above.
/// It is a *cap*, not a resize: a smaller tile is kept as authored.
pub const DEFAULT_MAX_EDGE: usize = 1024;

impl UvTexture {
    /// Opens `path` — a single image, or a tile set when the name carries a
    /// `<UDIM>` or `<UVTILE>` token — decoding every tile present on disk at
    /// or below the `CRUST_TEX_MAX` edge cap. `None` when nothing could be
    /// decoded.
    pub fn open(path: &Path, space: ColorSpace) -> Option<UvTexture> {
        let max_edge = std::env::var("CRUST_TEX_MAX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(DEFAULT_MAX_EDGE);

        let name = path.to_string_lossy().into_owned();
        let token = TileToken::detect(&name);
        let mut tiles = Vec::new();
        if let Some(token) = token {
            // Only tiles that exist on disk are opened, so a chart with holes
            // costs nothing for the tiles it does not use. 10x10 covers the
            // 1001..1100 range every DCC writes — and is what bounds the
            // `<UVTILE>` sweep too, since the two tokens name the same grid.
            for v in 0..10u32 {
                for u in 0..10u32 {
                    let candidate = token.expand(&name, u, v);
                    let p = Path::new(&candidate);
                    if !p.exists() {
                        continue;
                    }
                    if let Some(t) = decode_tile(p, udim_number(u, v), max_edge) {
                        tiles.push(t);
                    }
                }
            }
            if tiles.is_empty() {
                error!(
                    "No tiles found for {} ({} expanded over the 10x10 grid)",
                    path.display(),
                    token.as_str()
                );
                return None;
            }
        } else {
            tiles.push(decode_tile(path, udim_number(0, 0), max_edge)?);
        }

        let to_linear = to_linear_table(space);
        let (width, height) = (tiles[0].width, tiles[0].height);
        Some(UvTexture {
            tiles,
            to_linear,
            width,
            height,
            tiled: token.is_some(),
        })
    }

    /// Bytes held, for the load-time report.
    pub fn bytes(&self) -> usize {
        self.tiles.iter().map(|t| t.pixels.len()).sum()
    }

    /// Tiles decoded — 1 for a single image.
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Size of the first tile, representative of the set.
    pub fn tile_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Bilinear lookup inside one tile, at coordinates already reduced to
    /// `[0, 1)`.
    ///
    /// Bilinear rather than nearest because these charts are magnified: a 4K
    /// tile capped to 1024 covers a few hundred pixels of the framing, so the
    /// texel grid is plainly visible under point sampling — the artefact the
    /// dome light's own nearest-texel sampling is still criticised for in
    /// `CLAUDE.md`.
    fn sample_tile(&self, t: &Tile, u: f32, v: f32) -> [f32; 4] {
        // Image rows run top-down while `v` grows upward, the same flip the
        // rest of the graphics world applies between UV and raster space.
        let x = u * t.width as f32 - 0.5;
        let y = (1.0 - v) * t.height as f32 - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let (fx, fy) = (x - x0, y - y0);
        let clampi = |i: f32, n: usize| (i.max(0.0) as usize).min(n.saturating_sub(1));
        let (x0i, y0i) = (clampi(x0, t.width), clampi(y0, t.height));
        let (x1i, y1i) = (clampi(x0 + 1.0, t.width), clampi(y0 + 1.0, t.height));
        let texel = |xi: usize, yi: usize| {
            let o = (yi * t.width + xi) * 3;
            [
                self.to_linear[t.pixels[o] as usize],
                self.to_linear[t.pixels[o + 1] as usize],
                self.to_linear[t.pixels[o + 2] as usize],
            ]
        };
        let (a, b, c, d) = (
            texel(x0i, y0i),
            texel(x1i, y0i),
            texel(x0i, y1i),
            texel(x1i, y1i),
        );
        let mut out = [0.0f32; 4];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            out[k] = top + (bot - top) * fy;
        }
        out[3] = 1.0;
        out
    }
}

impl Texture2D for UvTexture {
    fn eval(&self, u: f32, v: f32, _width: f32) -> [f32; 4] {
        if !u.is_finite() || !v.is_finite() {
            return [0.0, 0.0, 0.0, 1.0];
        }
        if self.tiled {
            let (tu, tv) = (u.floor(), v.floor());
            // Outside the 10x10 tile grid there is no tile by definition;
            // black rather than a wrapped guess, so a mis-scaled chart looks
            // wrong instead of plausibly tiled.
            if !(0.0..10.0).contains(&tu) || !(0.0..10.0).contains(&tv) {
                return [0.0, 0.0, 0.0, 1.0];
            }
            let number = udim_number(tu as u32, tv as u32);
            match self.tiles.iter().find(|t| t.number == number) {
                Some(t) => self.sample_tile(t, u - tu, v - tv),
                None => [0.0, 0.0, 0.0, 1.0],
            }
        } else {
            // MaterialX's default address mode is `periodic`.
            let wrap = |x: f32| x - x.floor();
            self.sample_tile(&self.tiles[0], wrap(u), wrap(v))
        }
    }
}

/// Decodes one image file, box-filtering it down so neither edge exceeds
/// `max_edge`.
///
/// The reduction factor is an integer chosen so the result is the largest
/// power-of-two-ish fit under the cap, and every source texel contributes
/// exactly once — a straight box average, not a subsample. Point-decimating
/// a 4K albedo to 1024 would alias its high-frequency detail into the render
/// as fixed-pattern noise that no amount of spp removes.
fn decode_tile(path: &Path, number: u32, max_edge: usize) -> Option<Tile> {
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| error!("Texture decode failed for {}: {e}", path.display()))
        .ok()?
        .with_guessed_format()
        .map_err(|e| error!("Texture decode failed for {}: {e}", path.display()))
        .ok()?;
    // Same reasoning as the environment decoder: these are trusted, locally
    // authored assets and an 8K texture exceeds the default allocation limit.
    reader.no_limits();
    let img = reader
        .decode()
        .map_err(|e| error!("Texture decode failed for {}: {e}", path.display()))
        .ok()?
        .to_rgb8();
    let (sw, sh) = (img.width() as usize, img.height() as usize);
    if sw == 0 || sh == 0 {
        return None;
    }
    let factor = (sw.div_ceil(max_edge)).max(sh.div_ceil(max_edge)).max(1);
    if factor == 1 {
        return Some(Tile {
            number,
            pixels: img.into_raw(),
            width: sw,
            height: sh,
        });
    }
    let (w, h) = ((sw / factor).max(1), (sh / factor).max(1));
    let src = img.as_raw();
    let mut pixels = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 3];
            let mut n = 0u32;
            for dy in 0..factor {
                let sy = y * factor + dy;
                if sy >= sh {
                    break;
                }
                for dx in 0..factor {
                    let sx = x * factor + dx;
                    if sx >= sw {
                        break;
                    }
                    let o = (sy * sw + sx) * 3;
                    acc[0] += src[o] as u32;
                    acc[1] += src[o + 1] as u32;
                    acc[2] += src[o + 2] as u32;
                    n += 1;
                }
            }
            let o = (y * w + x) * 3;
            // Averaged in the file's own encoding, not in linear light. That
            // is what a mip pyramid in an 8-bit pipeline does, and matching it
            // keeps a downsampled albedo the same brightness as the DCC's
            // preview of the same file.
            for k in 0..3 {
                pixels[o + k] = (acc[k] / n.max(1)) as u8;
            }
        }
    }
    Some(Tile {
        number,
        pixels,
        width: w,
        height: h,
    })
}

/// The 256-entry decode table for one colour space.
///
/// The files are 8-bit, so every possible stored value is one of 256 — the
/// transfer function is evaluated once per level at load rather than per
/// texel fetch, and nothing recovers precision that was never in the file.
///
/// The three curves are deliberately distinct. MaterialX's `g22_rec709` and
/// `g18_rec709` are pure power laws; sRGB's EOTF is piecewise, with a linear
/// toe that keeps near-black values well above the power law (up to 19x at
/// 0.01 — `docs/color_management.md` tabulates it). Collapsing them into one
/// curve is wrong in the shadows for 2.2 and wrong everywhere for 1.8.
fn to_linear_table(space: ColorSpace) -> [f32; 256] {
    let mut table = [0.0f32; 256];
    for (i, v) in table.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        *v = match space.gamma() {
            Some(g) => c.powf(g),
            None => match space {
                ColorSpace::Srgb => crate::srgb_to_linear(c),
                _ => c,
            },
        };
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a 1x1 PNG of a flat colour, creating parent directories.
    ///
    /// One texel is enough: these tests are about *which file* a coordinate
    /// reaches, so the colour is the tile's identity and bilinear filtering
    /// across a uniform tile returns it exactly.
    fn write_tile(path: &Path, rgb: [u8; 3]) {
        std::fs::create_dir_all(path.parent().unwrap()).expect("temp dir");
        image::RgbImage::from_pixel(1, 1, image::Rgb(rgb))
            .save(path)
            .expect("write png");
    }

    /// A scratch directory of its own per test, so the parallel test runner
    /// cannot have one test's tiles satisfy another's glob.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("crust_uv_texture_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// The channel values a tile is written with, recovered from a lookup.
    /// Raw decode, so the round trip is exact.
    fn sampled(tex: &UvTexture, u: f32, v: f32) -> [u8; 3] {
        let px = tex.eval(u, v, 0.0);
        [
            (px[0] * 255.0).round() as u8,
            (px[1] * 255.0).round() as u8,
            (px[2] * 255.0).round() as u8,
        ]
    }

    #[test]
    fn a_udim_set_loads_and_addresses_by_tile() {
        let dir = scratch("udim");
        // 1001 is (0,0), 1002 is (1,0), 1011 is (0,1) — and 1012 is left off
        // disk, so the set has a hole.
        write_tile(&dir.join("a.1001.png"), [10, 0, 0]);
        write_tile(&dir.join("a.1002.png"), [20, 0, 0]);
        write_tile(&dir.join("a.1011.png"), [30, 0, 0]);

        let tex = UvTexture::open(&dir.join("a.<UDIM>.png"), ColorSpace::Raw)
            .expect("a <UDIM> set must load the tiles that exist");
        assert_eq!(tex.tile_count(), 3, "only the tiles on disk are decoded");
        assert_eq!(sampled(&tex, 0.5, 0.5), [10, 0, 0]);
        assert_eq!(sampled(&tex, 1.5, 0.5), [20, 0, 0]);
        assert_eq!(sampled(&tex, 0.5, 1.5), [30, 0, 0]);
    }

    #[test]
    fn a_uvtile_set_loads_and_addresses_by_the_same_tile() {
        // The regression this pins: `<UVTILE>` reached `open` intact (the
        // document parser escapes and restores it) and was then never
        // expanded, so the literal name was opened as a single image, failed,
        // and the whole set loaded *nothing*.
        let dir = scratch("uvtile");
        // Both indices are 1-based: u1_v1 is the tile <UDIM> numbers 1001.
        write_tile(&dir.join("a.u1_v1.png"), [10, 0, 0]);
        write_tile(&dir.join("a.u2_v1.png"), [20, 0, 0]);
        write_tile(&dir.join("a.u1_v2.png"), [30, 0, 0]);

        let tex = UvTexture::open(&dir.join("a.<UVTILE>.png"), ColorSpace::Raw)
            .expect("a <UVTILE> set must load like a <UDIM> one");
        assert_eq!(tex.tile_count(), 3);
        // Identical coordinates to the <UDIM> test above: the two tokens
        // spell the same grid, so addressing must not depend on which named
        // the files.
        assert_eq!(sampled(&tex, 0.5, 0.5), [10, 0, 0]);
        assert_eq!(sampled(&tex, 1.5, 0.5), [20, 0, 0]);
        assert_eq!(sampled(&tex, 0.5, 1.5), [30, 0, 0]);
    }

    #[test]
    fn a_missing_tile_reads_black_rather_than_a_neighbour() {
        let dir = scratch("holes");
        write_tile(&dir.join("a.u1_v1.png"), [10, 20, 30]);
        let tex = UvTexture::open(&dir.join("a.<UVTILE>.png"), ColorSpace::Raw).expect("loads");

        // A hole inside the grid, and coordinates off the grid entirely.
        // Both are black, so a mis-scaled chart looks wrong instead of
        // plausibly tiled — the set must not wrap onto the tile it does have.
        assert_eq!(sampled(&tex, 3.5, 2.5), [0, 0, 0], "hole");
        assert_eq!(sampled(&tex, 10.5, 0.5), [0, 0, 0], "past the grid");
        assert_eq!(sampled(&tex, -0.5, 0.5), [0, 0, 0], "before the grid");
        assert_eq!(
            sampled(&tex, 0.5, 0.5),
            [10, 20, 30],
            "the tile that exists"
        );
    }

    #[test]
    fn an_empty_tile_set_declines_instead_of_loading_the_literal_name() {
        let dir = scratch("empty");
        assert!(UvTexture::open(&dir.join("gone.<UDIM>.png"), ColorSpace::Raw).is_none());
        assert!(UvTexture::open(&dir.join("gone.<UVTILE>.png"), ColorSpace::Raw).is_none());
    }

    #[test]
    fn a_single_image_wraps_instead_of_tiling() {
        let dir = scratch("single");
        write_tile(&dir.join("flat.png"), [7, 8, 9]);
        let tex = UvTexture::open(&dir.join("flat.png"), ColorSpace::Raw).expect("loads");
        // MaterialX's default address mode is `periodic`, so a coordinate
        // outside the unit square wraps back rather than reading black.
        assert_eq!(sampled(&tex, 0.5, 0.5), [7, 8, 9]);
        assert_eq!(sampled(&tex, 3.5, 2.5), [7, 8, 9]);
    }

    #[test]
    fn the_two_tokens_spell_the_same_grid() {
        for (u, v) in [(0, 0), (1, 0), (0, 1), (9, 9)] {
            let udim = TileToken::Udim.expand("a.<UDIM>.png", u, v);
            let uvtile = TileToken::UvTile.expand("a.<UVTILE>.png", u, v);
            assert_eq!(udim, format!("a.{}.png", udim_number(u, v)));
            assert_eq!(uvtile, format!("a.u{}_v{}.png", u + 1, v + 1));
        }
        assert_eq!(TileToken::detect("a.<UDIM>.png"), Some(TileToken::Udim));
        assert_eq!(TileToken::detect("a.<UVTILE>.png"), Some(TileToken::UvTile));
        assert_eq!(TileToken::detect("a.png"), None);
    }

    fn at(space: ColorSpace, encoded: u8) -> f32 {
        to_linear_table(space)[encoded as usize]
    }

    #[test]
    fn gamma_tables_are_pure_power_laws() {
        for encoded in [0u8, 3, 13, 26, 128, 255] {
            let c = encoded as f32 / 255.0;
            assert!((at(ColorSpace::Gamma22, encoded) - c.powf(2.2)).abs() < 1e-7);
            assert!((at(ColorSpace::Gamma18, encoded) - c.powf(1.8)).abs() < 1e-7);
        }
        // Both curves are anchored: black stays black, white stays white, so
        // a fully-lit albedo keeps its exposure whichever tag it carries.
        for space in [ColorSpace::Gamma22, ColorSpace::Gamma18, ColorSpace::Srgb] {
            assert_eq!(at(space, 0), 0.0);
            assert!((at(space, 255) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn gamma_22_is_not_the_srgb_curve_in_the_shadows() {
        // The regression this pins. `g22_rec709` used to decode through the
        // piecewise sRGB curve, whose linear toe lifts near-black by an order
        // of magnitude — an albedo of 0.01 encoded reads 19x too bright.
        let (srgb, g22) = (at(ColorSpace::Srgb, 3), at(ColorSpace::Gamma22, 3));
        assert!(srgb > g22 * 10.0, "srgb {srgb} vs gamma22 {g22}");
        // And converges in the midtones, which is why the bug is invisible
        // on a look-dev turntable and only shows up in dark albedo.
        let (srgb, g22) = (at(ColorSpace::Srgb, 128), at(ColorSpace::Gamma22, 128));
        assert!((srgb - g22).abs() < 0.005, "srgb {srgb} vs gamma22 {g22}");
    }

    #[test]
    fn gamma_18_is_brighter_than_both_across_the_range() {
        // 1.8 is the shallower exponent, so it decodes above 2.2 everywhere
        // strictly inside [0,1] — the error is not confined to the toe.
        for encoded in [13u8, 64, 128, 200] {
            let (g18, g22) = (
                at(ColorSpace::Gamma18, encoded),
                at(ColorSpace::Gamma22, encoded),
            );
            assert!(g18 > g22, "at {encoded}: g18 {g18} !> g22 {g22}");
            assert!(
                g18 > at(ColorSpace::Srgb, encoded),
                "at {encoded}: g18 {g18}"
            );
        }
    }

    #[test]
    fn raw_is_the_identity() {
        for encoded in [0u8, 1, 77, 255] {
            assert_eq!(at(ColorSpace::Raw, encoded), encoded as f32 / 255.0);
        }
    }
}
