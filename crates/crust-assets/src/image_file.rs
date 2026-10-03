//! The one way an LDR / HDR image file is decoded through `image`.
//!
//! Every decoder here (UV textures, environment maps, `maketx`) reads its file
//! through [`decode`], so the decode limits and the TIFF workaround below
//! apply to all of them alike.

use std::io::{Cursor, Read};
use std::path::Path;

use crate::error::AssetError;

/// Decodes `path`, format guessed from its bytes, with no allocation limit.
///
/// The limit is lifted because these are trusted, locally authored assets,
/// and an 8K texture or a 16K panorama exceeds `image`'s default 512 MiB.
///
/// Only a TIFF is read into memory first, to be patched; every other format
/// streams from the file, so a large PNG or HDR does not hold its encoded
/// bytes alongside the decoded image.
pub(crate) fn decode(path: &Path) -> Result<image::DynamicImage, AssetError> {
    let mut magic = [0u8; 4];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut magic))
        .map_err(AssetError::io(path))?;
    let decoded = if tiff_byte_order(&magic[..n]).is_some() {
        let mut bytes = std::fs::read(path).map_err(AssetError::io(path))?;
        declare_unspecified_extra_sample_as_alpha(&mut bytes);
        let mut reader =
            image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Tiff);
        reader.no_limits();
        reader.decode()
    } else {
        let mut reader = image::ImageReader::open(path)
            .map_err(AssetError::io(path))?
            .with_guessed_format()
            .map_err(AssetError::io(path))?;
        reader.no_limits();
        reader.decode()
    };
    decoded.map_err(AssetError::image(path))
}

/// `Some(little_endian)` for a classic TIFF header, `None` for anything else.
fn tiff_byte_order(bytes: &[u8]) -> Option<bool> {
    match bytes.get(0..4)? {
        [b'I', b'I', 42, 0] => Some(true),
        [b'M', b'M', 0, 42] => Some(false),
        _ => None,
    }
}

/// Rewrites a TIFF's `ExtraSamples = [0]` (one unspecified extra sample) to
/// `[2]` (unassociated alpha), in place. Anything else is left untouched.
///
/// **Why.** `tiff` 0.11.3 reads an RGB image with an unspecified fourth sample
/// as 3-channel `RGB`. It strips the extra sample from a 4-sample-per-pixel
/// stream, and gets that wrong: the result is scrambled, streaky noise (with
/// the LZW + horizontal predictor combination Substance exports, at least).
/// The OpenPBR playground's `walls_*.tif` are written this way and rendered as
/// multicoloured speckle. Declared as alpha, the same bytes take the crate's
/// RGBA path, which decodes them correctly, and every caller here drops alpha
/// (`to_rgb8` / `to_rgb32f`), so the extra sample is ignored either way — which
/// is what "unspecified" means.
///
/// Only the first IFD of a classic (non-Big) TIFF is patched: that is the one
/// `image` decodes. The value must be inline (a count of 1 or 2 shorts fits in
/// the entry), which it always is for the one-extra-sample case this is for.
fn declare_unspecified_extra_sample_as_alpha(bytes: &mut [u8]) {
    const EXTRA_SAMPLES: u16 = 338;
    const SHORT: u16 = 3;
    let Some(le) = tiff_byte_order(bytes) else {
        return;
    };
    let u16_at = |b: &[u8], o: usize| -> Option<u16> {
        let v: [u8; 2] = b.get(o..o + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        })
    };
    let u32_at = |b: &[u8], o: usize| -> Option<u32> {
        let v: [u8; 4] = b.get(o..o + 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        })
    };
    let Some(ifd) = u32_at(bytes, 4).map(|o| o as usize) else {
        return;
    };
    let Some(entries) = u16_at(bytes, ifd) else {
        return;
    };
    for e in 0..usize::from(entries) {
        let entry = ifd + 2 + e * 12;
        if u16_at(bytes, entry) != Some(EXTRA_SAMPLES) {
            continue;
        }
        let inline = u16_at(bytes, entry + 2) == Some(SHORT)
            && matches!(u32_at(bytes, entry + 4), Some(1 | 2));
        if inline && u16_at(bytes, entry + 8) == Some(0) {
            let alpha = if le {
                2u16.to_le_bytes()
            } else {
                2u16.to_be_bytes()
            };
            bytes[entry + 8..entry + 10].copy_from_slice(&alpha);
        }
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-row, uncompressed, little-endian TIFF of RGB + one extra sample
    /// with `ExtraSamples = [extra]` and the horizontal predictor — the shape
    /// the playground's walls are written in, minus the LZW.
    fn rgbx_tiff(pixels: &[[u8; 4]], extra: u16) -> Vec<u8> {
        let w = pixels.len() as u32;
        // Predictor 2 stores each sample as the difference from the same
        // sample one pixel to the left.
        let mut data = Vec::new();
        for (i, px) in pixels.iter().enumerate() {
            for c in 0..4 {
                let left = if i == 0 { 0 } else { pixels[i - 1][c] };
                data.push(px[c].wrapping_sub(left));
            }
        }
        let data_at = 8u32;
        let bps_at = data_at + data.len() as u32;
        let ifd_at = bps_at + 8;
        let mut b = Vec::new();
        b.extend_from_slice(b"II");
        b.extend_from_slice(&42u16.to_le_bytes());
        b.extend_from_slice(&ifd_at.to_le_bytes());
        b.extend_from_slice(&data);
        for _ in 0..4 {
            b.extend_from_slice(&8u16.to_le_bytes());
        }
        // (tag, type, count, value): 3 = SHORT, 4 = LONG; sorted by tag.
        let entries: [(u16, u16, u32, u32); 12] = [
            (256, 3, 1, w),
            (257, 3, 1, 1),
            (258, 3, 4, bps_at),
            (259, 3, 1, 1),
            (262, 3, 1, 2),
            (273, 4, 1, data_at),
            (277, 3, 1, 4),
            (278, 3, 1, 1),
            (279, 4, 1, data.len() as u32),
            (284, 3, 1, 1),
            (317, 3, 1, 2),
            (338, 3, 1, u32::from(extra)),
        ];
        b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, ty, count, value) in entries {
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&ty.to_le_bytes());
            b.extend_from_slice(&count.to_le_bytes());
            b.extend_from_slice(&value.to_le_bytes());
        }
        b.extend_from_slice(&0u32.to_le_bytes());
        b
    }

    const PIXELS: [[u8; 4]; 3] = [[10, 20, 30, 255], [40, 50, 60, 255], [200, 5, 90, 255]];
    const RGB: [u8; 9] = [10, 20, 30, 40, 50, 60, 200, 5, 90];

    fn rgb8(bytes: &[u8]) -> Vec<u8> {
        image::load_from_memory(bytes).unwrap().to_rgb8().into_raw()
    }

    #[test]
    fn an_unspecified_extra_sample_decodes_as_the_rgb_it_carries() {
        let mut bytes = rgbx_tiff(&PIXELS, 0);
        declare_unspecified_extra_sample_as_alpha(&mut bytes);
        assert_eq!(rgb8(&bytes), RGB);
    }

    /// The upstream bug this module works around, as a canary: when it
    /// fails, `tiff` has fixed it and the workaround can go. `Cargo.lock` is
    /// committed, so this can only start failing in the change that moves
    /// `tiff` in the lock — the one that should remove the workaround.
    #[test]
    fn the_tiff_crate_still_needs_the_workaround() {
        assert_ne!(rgb8(&rgbx_tiff(&PIXELS, 0)), RGB);
    }

    /// A file under the temp directory, removed on drop.
    struct TempFile(std::path::PathBuf);
    impl TempFile {
        fn new(name: &str, bytes: &[u8]) -> TempFile {
            let p = std::env::temp_dir()
                .join(format!("crust-image-file-{}-{name}", std::process::id()));
            std::fs::write(&p, bytes).unwrap();
            TempFile(p)
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn decode_patches_a_tiff_and_streams_everything_else() {
        let tif = TempFile::new("rgbx.tif", &rgbx_tiff(&PIXELS, 0));
        assert_eq!(decode(&tif.0).unwrap().to_rgb8().into_raw(), RGB);

        let mut png = Vec::new();
        image::RgbImage::from_raw(3, 1, RGB.to_vec())
            .unwrap()
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        // No extension: the format comes from the bytes on both paths.
        let png = TempFile::new("rgb", &png);
        assert_eq!(decode(&png.0).unwrap().to_rgb8().into_raw(), RGB);

        let short = TempFile::new("short", b"II");
        assert!(decode(&short.0).is_err());
        assert!(decode(Path::new("does/not/exist.tif")).is_err());
    }

    #[test]
    fn declared_alpha_and_other_files_are_left_alone() {
        let mut bytes = rgbx_tiff(&PIXELS, 1);
        let before = bytes.clone();
        declare_unspecified_extra_sample_as_alpha(&mut bytes);
        assert_eq!(bytes, before, "associated alpha is not rewritten");
        assert_eq!(rgb8(&bytes), RGB);

        let mut png = b"\x89PNG\r\n\x1a\nnot really".to_vec();
        let before = png.clone();
        declare_unspecified_extra_sample_as_alpha(&mut png);
        assert_eq!(png, before);
        // A truncated or out-of-range header must not panic.
        for len in 0..bytes.len() {
            let mut short = bytes[..len].to_vec();
            declare_unspecified_extra_sample_as_alpha(&mut short);
        }
    }
}
