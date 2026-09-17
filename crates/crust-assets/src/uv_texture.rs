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

/// A UV-addressed texture: one image, or a UDIM set.
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
    /// True when the file name carried a `<UDIM>` token. A UDIM set addresses
    /// tiles by the integer part of `(u, v)`; a single image wraps instead,
    /// which is MaterialX's default `periodic` address mode.
    udim: bool,
}

/// Largest tile edge kept, unless `CRUST_TEX_MAX` says otherwise.
///
/// 1024 rather than full resolution for the reason in the module note above.
/// It is a *cap*, not a resize: a smaller tile is kept as authored.
pub const DEFAULT_MAX_EDGE: usize = 1024;

impl UvTexture {
    /// Opens `path` — a single image, or a UDIM set when the name carries a
    /// `<UDIM>` token — decoding every tile present on disk at or below the
    /// `CRUST_TEX_MAX` edge cap. `None` when nothing could be decoded.
    pub fn open(path: &Path, space: ColorSpace) -> Option<UvTexture> {
        let max_edge = std::env::var("CRUST_TEX_MAX")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v >= 1)
            .unwrap_or(DEFAULT_MAX_EDGE);

        let name = path.to_string_lossy().into_owned();
        let udim = name.contains("<UDIM>");
        let mut tiles = Vec::new();
        if udim {
            // Only tiles that exist on disk are opened, so a chart with holes
            // costs nothing for the tiles it does not use. 10x10 covers the
            // 1001..1100 range every DCC writes.
            for v in 0..10u32 {
                for u in 0..10u32 {
                    let number = 1001 + u + 10 * v;
                    let candidate = name.replace("<UDIM>", &number.to_string());
                    let p = Path::new(&candidate);
                    if !p.exists() {
                        continue;
                    }
                    if let Some(t) = decode_tile(p, number, max_edge) {
                        tiles.push(t);
                    }
                }
            }
            if tiles.is_empty() {
                error!("No UDIM tiles found for {}", path.display());
                return None;
            }
        } else {
            tiles.push(decode_tile(path, 1001, max_edge)?);
        }

        let to_linear = to_linear_table(space);
        let (width, height) = (tiles[0].width, tiles[0].height);
        Some(UvTexture {
            tiles,
            to_linear,
            width,
            height,
            udim,
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
    fn eval(&self, u: f32, v: f32) -> [f32; 4] {
        if !u.is_finite() || !v.is_finite() {
            return [0.0, 0.0, 0.0, 1.0];
        }
        if self.udim {
            let (tu, tv) = (u.floor(), v.floor());
            // Outside the 10x10 UDIM grid there is no tile by definition;
            // black rather than a wrapped guess, so a mis-scaled chart looks
            // wrong instead of plausibly tiled.
            if !(0.0..10.0).contains(&tu) || !(0.0..10.0).contains(&tv) {
                return [0.0, 0.0, 0.0, 1.0];
            }
            let number = 1001 + tu as u32 + 10 * tv as u32;
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
            let (g18, g22) = (at(ColorSpace::Gamma18, encoded), at(ColorSpace::Gamma22, encoded));
            assert!(g18 > g22, "at {encoded}: g18 {g18} !> g22 {g22}");
            assert!(g18 > at(ColorSpace::Srgb, encoded), "at {encoded}: g18 {g18}");
        }
    }

    #[test]
    fn raw_is_the_identity() {
        for encoded in [0u8, 1, 77, 255] {
            assert_eq!(at(ColorSpace::Raw, encoded), encoded as f32 / 255.0);
        }
    }
}
