//! Reading one tile off disk: LDR images through `image`, EXR at `f32`, and
//! the `CRUST_TEX_MAX` box reduction.

use std::path::Path;

use crust_core::ColorSpace;

use crate::error::AssetError;

use super::tile::Tile;

/// Decodes one image file, box-filtering it down so neither edge exceeds
/// `max_edge`.
///
/// The reduction factor is an integer chosen so the result is the largest
/// power-of-two-ish fit under the cap, and every source texel contributes
/// exactly once — a straight box average, not a subsample. Point-decimating
/// a 4K albedo to 1024 would alias its high-frequency detail into the render
/// as fixed-pattern noise that no amount of spp removes.
///
/// Also returns the source's pixel format as `(eight_bit, channels)` — what
/// [`ColorSpace::resolve_auto`] asks about, and gone once `to_rgb8` has run.
pub(super) fn decode_tile(
    path: &Path,
    number: u32,
    max_edge: usize,
) -> Result<(Tile<u8>, (bool, u8)), AssetError> {
    let mut reader = image::ImageReader::open(path)
        .map_err(AssetError::io(path))?
        .with_guessed_format()
        .map_err(AssetError::io(path))?;
    // Same reasoning as the environment decoder: these are trusted, locally
    // authored assets and an 8K texture exceeds the default allocation limit.
    reader.no_limits();
    let decoded = reader.decode().map_err(AssetError::image(path))?;
    let color = decoded.color();
    let format = (
        color.bytes_per_pixel() == color.channel_count(),
        color.channel_count(),
    );
    let img = decoded.to_rgb8();
    let (sw, sh) = (img.width() as usize, img.height() as usize);
    if sw == 0 || sh == 0 {
        return Err(AssetError::unusable(path, "zero-sized image"));
    }
    let factor = (sw.div_ceil(max_edge)).max(sh.div_ceil(max_edge)).max(1);
    if factor == 1 {
        return Ok((Tile::unmipped(number, img.into_raw(), sw, sh), format));
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
    Ok((Tile::unmipped(number, pixels, w, h), format))
}

/// [`decode_tile`] for an EXR: linear `f32` RGB, box-filtered under the same
/// `max_edge` cap by the same integer factor.
///
/// The reduction averages in the file's own encoding exactly as the `u8` one
/// does — which for a float file *is* linear light, so here the resize and the
/// mip chain agree. An explicit non-raw `space` is applied once, before the
/// reduction, leaving every stored value linear.
pub(super) fn decode_exr_tile(
    path: &Path,
    number: u32,
    max_edge: usize,
    space: ColorSpace,
) -> Result<Tile<f32>, AssetError> {
    let (mut src, sw, sh) = crate::environment::try_read_exr_rgb(path)?;
    if sw == 0 || sh == 0 {
        return Err(AssetError::unusable(path, "zero-sized image"));
    }
    if space != ColorSpace::Raw {
        for c in &mut src {
            *c = crate::to_linear(space, *c);
        }
    }
    let factor = (sw.div_ceil(max_edge)).max(sh.div_ceil(max_edge)).max(1);
    if factor == 1 {
        return Ok(Tile::unmipped(number, src, sw, sh));
    }
    let (w, h) = ((sw / factor).max(1), (sh / factor).max(1));
    let mut pixels = vec![0.0f32; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
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
                    for k in 0..3 {
                        acc[k] += src[o + k];
                    }
                    n += 1;
                }
            }
            let o = (y * w + x) * 3;
            for k in 0..3 {
                pixels[o + k] = acc[k] / n.max(1) as f32;
            }
        }
    }
    Ok(Tile::unmipped(number, pixels, w, h))
}
