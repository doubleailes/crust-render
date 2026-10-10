//! Reading one tile off disk: LDR images through `image`, EXR at `f32`, and
//! the `CRUST_TEX_MAX` box reduction.

use std::num::NonZeroUsize;
use std::path::Path;

use crust_core::ResolvedColorSpace;

use crate::error::AssetError;

use super::mip::channels;
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
/// The tile is RGBA when the file's alpha is authored and cuts something,
/// RGB otherwise (see [`crate::drop_opaque_alpha`]); its colour is the same
/// bytes either way.
///
/// Also returns the source's pixel format as `(eight_bit, channels)` — what
/// [`crust_core::ColorSpace::resolve_auto`] asks about, and gone once `to_rgb8` has run.
pub(super) fn decode_tile(
    path: &Path,
    number: u32,
    max_edge: NonZeroUsize,
) -> Result<(Tile<u8>, (bool, u8)), AssetError> {
    let (decoded, authored_alpha) = crate::image_file::decode_with_alpha(path)?;
    let color = decoded.color();
    let format = (
        color.bytes_per_pixel() == color.channel_count(),
        color.channel_count(),
    );
    let (sw, sh) = (decoded.width() as usize, decoded.height() as usize);
    if sw == 0 || sh == 0 {
        return Err(AssetError::unusable(path, "zero-sized image"));
    }
    // `to_rgba8` narrows the colour exactly as `to_rgb8` does, so the RGB of
    // a kept alpha's tile is the bytes an RGB decode of it would hold.
    let (src, alpha) = if authored_alpha {
        crate::drop_opaque_alpha(decoded.to_rgba8().into_raw(), u8::MAX)
    } else {
        (decoded.to_rgb8().into_raw(), false)
    };
    let max_edge = max_edge.get();
    let factor = (sw.div_ceil(max_edge)).max(sh.div_ceil(max_edge)).max(1);
    if factor == 1 {
        return Ok((Tile::unmipped(number, src, sw, sh, alpha), format));
    }
    let n = channels(alpha);
    let (w, h) = ((sw / factor).max(1), (sh / factor).max(1));
    let mut pixels = vec![0u8; w * h * n];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            let mut count = 0u32;
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
                    let o = (sy * sw + sx) * n;
                    for k in 0..n {
                        acc[k] += src[o + k] as u32;
                    }
                    count += 1;
                }
            }
            let o = (y * w + x) * n;
            // Averaged in the file's own encoding, not in linear light. That
            // is what a mip pyramid in an 8-bit pipeline does, and matching it
            // keeps a downsampled albedo the same brightness as the DCC's
            // preview of the same file. (Alpha has no encoding, so for it the
            // two are the same average.)
            for k in 0..n {
                pixels[o + k] = (acc[k] / count.max(1)) as u8;
            }
        }
    }
    Ok((Tile::unmipped(number, pixels, w, h, alpha), format))
}

/// [`decode_tile`] for an EXR: linear `f32` RGB, or RGBA when the file's `A`
/// channel cuts something, box-filtered under the same `max_edge` cap by the
/// same integer factor.
///
/// The reduction averages in the file's own encoding exactly as the `u8` one
/// does — which for a float file *is* linear light, so here the resize and the
/// mip chain agree. An explicit non-raw `space` is applied once, before the
/// reduction and to the colour alone, leaving every stored value linear.
pub(super) fn decode_exr_tile(
    path: &Path,
    number: u32,
    max_edge: NonZeroUsize,
    space: ResolvedColorSpace,
) -> Result<Tile<f32>, AssetError> {
    let (mut src, sw, sh, alpha) = crate::environment::try_read_exr_texels(path)?;
    if sw == 0 || sh == 0 {
        return Err(AssetError::unusable(path, "zero-sized image"));
    }
    // Curve and primaries both, once: a preloaded float tile is stored in
    // the working space, so its lookup needs no matrix.
    if alpha {
        crate::decode_rgb_of_rgba(&mut src, |rgb| space.decode_rgb_slice(rgb));
    } else {
        space.decode_rgb_slice(&mut src);
    }
    let max_edge = max_edge.get();
    let factor = (sw.div_ceil(max_edge)).max(sh.div_ceil(max_edge)).max(1);
    if factor == 1 {
        return Ok(Tile::unmipped(number, src, sw, sh, alpha));
    }
    let n = channels(alpha);
    let (w, h) = ((sw / factor).max(1), (sh / factor).max(1));
    let mut pixels = vec![0.0f32; w * h * n];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 4];
            let mut count = 0u32;
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
                    let o = (sy * sw + sx) * n;
                    for k in 0..n {
                        acc[k] += src[o + k];
                    }
                    count += 1;
                }
            }
            let o = (y * w + x) * n;
            for k in 0..n {
                pixels[o + k] = acc[k] / count.max(1) as f32;
            }
        }
    }
    Ok(Tile::unmipped(number, pixels, w, h, alpha))
}
