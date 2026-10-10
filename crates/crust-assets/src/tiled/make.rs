//! Converting an ordinary image into a `.tx` — the library half of `maketx`.
//!
//! Lives here rather than in the example so the renderer can do it too
//! (`crust --auto-tx`, through [`crate::FileAssets::with_auto_tx`]).
//! There is one conversion in the workspace, which is what keeps a `.tx` the
//! CLI made on first use identical to one converted by hand.
//!
//! The mip filter is the one thing that cannot be delegated to OIIO's
//! `maketx`: levels are reduced by the same `reduce_half` / `reduce_half_linear`
//! as the in-memory pyramid, so a streamed render and a preloaded one agree
//! texel for texel (see "Streaming textures" in
//! `openspec/specs/textures/design.md`).

use crust_core::{ColorSpace, ResolvedColorSpace};

use crate::error::AssetError;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Which backing a conversion writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxFormat {
    /// Tiled TIFF, `u8` tiles.
    Tiff,
    /// Tiled mip EXR, `half` tiles.
    Exr,
    /// EXR only when the source carries values above 1.0 — `maketx`'s
    /// default, which keeps an 8-bit-range `.hdr` from paying double.
    FromRange,
    /// EXR for any float source, TIFF for an 8-bit one — what `--auto-tx`
    /// uses. A float source is float for a reason even when its values fit in
    /// `[0, 1]`: ALab's EXRs are linear albedo and roughness, and quantising
    /// linear data to 8 bits bands in the shadows. It also matches the
    /// preload path, which keeps an EXR at `f32`.
    FromSampleType,
}

/// What one conversion produced.
#[derive(Debug)]
pub struct MadeTx {
    pub dst: PathBuf,
    /// `"half, exr"` or `"8-bit, tiff"`.
    pub kind: &'static str,
    /// The space recorded in the file (`crust:mipspace`), resolved against
    /// the source.
    pub space: ResolvedColorSpace,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// The source holds values above 1.0 that a TIFF backing clipped.
    pub clipped: bool,
    /// The `.tx` carries the source's alpha: it had one, and it cut
    /// something (an alpha opaque everywhere is not written).
    pub alpha: bool,
}

/// The source as it was authored: 8-bit samples in its own encoding, or
/// floats that are already light — RGB, or RGBA when [`decode`] says so.
enum Source {
    Bytes(Vec<u8>),
    Floats(Vec<f32>),
}

/// A decoded source: its texels, size, whether they carry alpha, and its
/// pixel format as `(eight_bit, channels)`.
type Decoded = (Source, usize, usize, bool, (bool, u8));

/// Decodes `src`, reporting whether its texels carry alpha and its pixel
/// format as `(eight_bit, channels)` — what [`ColorSpace::resolve_auto`] asks
/// about.
///
/// Alpha is kept exactly when the preloaded texture keeps it — authored, and
/// not opaque everywhere (see [`crate::drop_opaque_alpha`]) — so a `.tx` and
/// its source read the same alpha.
fn decode(src: &Path) -> Result<Decoded, AssetError> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // EXR goes through crust's own reader: the workspace's `image` has no
    // `exr` feature, and it is the one reader that handles single-channel and
    // layer-prefixed channels.
    if ext == "exr" {
        let (pixels, w, h, alpha) = crate::environment::try_read_exr_texels(src)?;
        return Ok((Source::Floats(pixels), w, h, alpha, (false, 3)));
    }
    let (img, authored_alpha) = crate::image_file::decode_with_alpha(src)?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let color = img.color();
    let format = (
        color.bytes_per_pixel() == color.channel_count(),
        color.channel_count(),
    );
    // A Radiance `.hdr` (and any float source `image` hands back) keeps its
    // range; everything else is narrowed to 8 bits exactly as the preload
    // path narrows it.
    let float =
        ext == "hdr" || matches!(color, image::ColorType::Rgb32F | image::ColorType::Rgba32F);
    match (float, authored_alpha) {
        (true, false) => Ok((
            Source::Floats(img.to_rgb32f().into_raw()),
            w,
            h,
            false,
            (false, 3),
        )),
        (true, true) => {
            let (v, alpha) = crate::drop_opaque_alpha(img.to_rgba32f().into_raw(), 1.0);
            Ok((Source::Floats(v), w, h, alpha, (false, 3)))
        }
        (false, false) => Ok((Source::Bytes(img.to_rgb8().into_raw()), w, h, false, format)),
        (false, true) => {
            let (v, alpha) = crate::drop_opaque_alpha(img.to_rgba8().into_raw(), u8::MAX);
            Ok((Source::Bytes(v), w, h, alpha, format))
        }
    }
}

/// Converts `src` into a `.tx` at `dst`, recording `space` — `Auto` resolved
/// against the source's format — as the space it is to be bound with.
pub fn make_tx(
    src: &Path,
    dst: &Path,
    space: ColorSpace,
    format: TxFormat,
) -> Result<MadeTx, AssetError> {
    if src == dst {
        return Err(AssetError::unusable(src, "input is already a .tx"));
    }
    if is_ptex(src) {
        return Err(AssetError::unusable(
            src,
            "a Ptex file is already a tiled per-face mip pyramid and is streamed as is \
             (CRUST_PTEX_STREAM) — it is never converted to .tx",
        ));
    }
    let (source, w, h, alpha, (eight_bit, channels)) = decode(src)?;
    if w == 0 || h == 0 {
        return Err(AssetError::unusable(src, "zero-sized image"));
    }
    let space = space.resolve_auto(eight_bit, channels);
    let n = crate::uv_texture::channels(alpha);

    // The colour's range, not the alpha's: coverage above 1 is not light a
    // TIFF backing would clip.
    let hdr = match &source {
        Source::Bytes(_) => false,
        Source::Floats(v) => v.chunks_exact(n).any(|t| t[..3].iter().any(|&s| s > 1.0)),
    };
    let exr = match format {
        TxFormat::Tiff => false,
        TxFormat::Exr => true,
        TxFormat::FromRange => hdr,
        TxFormat::FromSampleType => matches!(source, Source::Floats(_)),
    };

    let kind = if exr {
        // EXR has no transfer curve, so the decode happens once, here, and
        // the space is recorded as the one the file is to be bound with.
        let mut linear: Vec<f32> = match &source {
            Source::Floats(v) => v.clone(),
            Source::Bytes(v) => v.iter().map(|&b| b as f32 / 255.0).collect(),
        };
        // The curve only: the samples stay on their own primaries, like a
        // TIFF-backed `.tx`'s, and a lookup applies the change of primaries
        // into whichever working space the file is bound in. Never on alpha.
        if alpha {
            crate::decode_rgb_of_rgba(&mut linear, |rgb| space.decode_curve_slice(rgb));
            super::write_tx_exr_rgba(dst, &linear, w, h, space)
        } else {
            space.decode_curve_slice(&mut linear);
            super::write_tx_exr(dst, &linear, w, h, space)
        }
        .map_err(AssetError::io(dst))?;
        "half, exr"
    } else {
        let bytes: Vec<u8> = match source {
            Source::Bytes(v) => v,
            // Clamping is the honest report of what a TIFF backing can hold.
            Source::Floats(v) => v
                .iter()
                .map(|&s| (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
                .collect(),
        };
        if alpha {
            super::write_tx_rgba(dst, &bytes, w, h, space)
        } else {
            super::write_tx(dst, &bytes, w, h, space)
        }
        .map_err(AssetError::io(dst))?;
        "8-bit, tiff"
    };

    Ok(MadeTx {
        dst: dst.to_path_buf(),
        kind,
        space,
        bytes_in: std::fs::metadata(src).map(|m| m.len()).unwrap_or(0),
        bytes_out: std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0),
        clipped: !exr && hdr,
        alpha,
    })
}

/// Whether `path` is a Ptex file (`.ptx` / `.ptex`).
///
/// Ptex is excluded from `.tx` conversion entirely, not merely unsupported: a
/// `.ptx` is already a tiled, per-face mip pyramid — the thing a `.tx` exists
/// to provide — and it streams as it is through `ptex::SharedReader`
/// (`CRUST_PTEX_STREAM`). Its per-face parameterisation also has no UV chart
/// to flatten into one image, so any `foo.tx` beside a `foo.ptx` is not its
/// converted form and must never be read in its place.
pub fn is_ptex(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ptx") || e.eq_ignore_ascii_case("ptex"))
}

/// The `.tx` beside a source texture: the same path with the extension
/// swapped, `foo.1001.exr` → `foo.1001.tx`. This is the one naming rule, used
/// by both the lookup and the conversion.
pub fn tx_sibling(src: &Path) -> PathBuf {
    src.with_extension("tx")
}

/// Why the `.tx` at `dst` is not what converting `src` now would write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxStaleness {
    /// There is no `.tx`.
    Missing,
    /// The `.tx` is older than its source.
    Older,
    /// A crust conversion wrote the `.tx` before texture alpha was carried
    /// ([`super::TX_VERSION`] 1), and its source declares alpha: the `.tx`
    /// holds RGB, so a cutout reading it would be opaque. It is newer than
    /// its source, so its age alone would never retire it.
    PredatesAlpha,
}

/// Whether, and why, a conversion would change what the renderer reads from
/// `dst` ([`TxStaleness`]), or `None` when the `.tx` is current.
///
/// An unreadable modification time counts as current rather than stale:
/// re-converting a file whose age cannot be told would happen on every
/// render. [`TxStaleness::PredatesAlpha`] reads two headers and no pixel: the
/// `.tx`'s version marker, then whether the source declares alpha. A
/// declared alpha that turns out opaque everywhere is reconverted once all the
/// same — telling it apart would mean decoding the source — and the new `.tx`
/// carries the current version, so it is not stale again.
pub fn tx_staleness(src: &Path, dst: &Path) -> Option<TxStaleness> {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mtime(src), mtime(dst)) {
        (_, None) if !dst.exists() => return Some(TxStaleness::Missing),
        (Some(s), Some(d)) if d < s => return Some(TxStaleness::Older),
        _ => {}
    }
    let predates_alpha = super::crust_tx_version(dst).is_some_and(|v| v < 2);
    (predates_alpha && source_declares_alpha(src)).then_some(TxStaleness::PredatesAlpha)
}

/// Whether a conversion source declares an alpha channel, from its header.
fn source_declares_alpha(src: &Path) -> bool {
    let exr = src
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exr"));
    if exr {
        super::exr_declares_alpha(src)
    } else {
        crate::image_file::declares_alpha(src)
    }
}

/// [`make_tx`] into a temporary beside `dst`, renamed over it on success.
///
/// A conversion killed halfway — the render interrupted, the disk full —
/// would otherwise leave a truncated `.tx` that the next render trusts
/// because it exists and is newer than its source. The rename is atomic on
/// one filesystem, and the temporary is in `dst`'s own directory to keep it
/// on one filesystem.
pub fn make_tx_atomic(
    src: &Path,
    space: ColorSpace,
    format: TxFormat,
) -> Result<MadeTx, AssetError> {
    let dst = tx_sibling(src);
    let tmp = dst.with_extension(format!(
        "tx.tmp{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let made = make_tx(src, &tmp, space, format);
    match made {
        Ok(mut m) => {
            std::fs::rename(&tmp, &dst).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                AssetError::Io {
                    path: dst.clone(),
                    source: e,
                }
            })?;
            m.dst = dst;
            Ok(m)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("crust_make_tx_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn an_atomic_conversion_leaves_only_the_tx_behind() {
        let dir = scratch("atomic");
        let src = dir.join("a.png");
        image::RgbImage::from_pixel(8, 8, image::Rgb([200, 100, 50]))
            .save(&src)
            .expect("png");
        let made =
            make_tx_atomic(&src, ColorSpace::AUTO, TxFormat::FromSampleType).expect("converts");
        assert_eq!(made.dst, dir.join("a.tx"));
        assert_eq!(made.kind, "8-bit, tiff");
        assert_eq!(
            made.space,
            ResolvedColorSpace::SRGB,
            "auto on 8-bit RGB is sRGB"
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "no temporary left over: {names:?}");
        assert_eq!(tx_staleness(&src, &made.dst), None);
    }

    #[test]
    fn a_failed_conversion_leaves_nothing_behind() {
        let dir = scratch("failed");
        let src = dir.join("broken.png");
        std::fs::write(&src, b"not a png").unwrap();
        assert!(make_tx_atomic(&src, ColorSpace::RAW, TxFormat::FromSampleType).is_err());
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(names.len(), 1, "only the source remains");
    }

    #[test]
    fn a_linear_exr_in_range_still_takes_the_exr_backing() {
        let dir = scratch("exr_backing");
        let src = dir.join("rough.exr");
        exr::prelude::write_rgb_file(&src, 4, 4, |_, _| (0.02f32, 0.5f32, 0.9f32)).expect("exr");
        let by_type = make_tx_atomic(&src, ColorSpace::AUTO, TxFormat::FromSampleType).expect("ok");
        assert_eq!(by_type.kind, "half, exr");
        assert_eq!(by_type.space, ResolvedColorSpace::RAW);
        // `maketx`'s own default would have narrowed it to 8 bits.
        let by_range = make_tx(
            &src,
            &dir.join("r.tx"),
            ColorSpace::RAW,
            TxFormat::FromRange,
        )
        .expect("ok");
        assert_eq!(by_range.kind, "8-bit, tiff");
    }

    #[test]
    fn ptex_is_refused_before_anything_is_written() {
        let dir = scratch("ptex");
        for name in ["face.ptx", "face.PTEX"] {
            let src = dir.join(name);
            std::fs::write(&src, b"Ptex").unwrap();
            let err = make_tx_atomic(&src, ColorSpace::RAW, TxFormat::FromSampleType)
                .expect_err("ptex must not convert");
            assert!(err.to_string().contains("Ptex"), "{err}");
            assert!(!tx_sibling(&src).exists());
        }
        assert!(!is_ptex(&dir.join("a.png")));
    }

    /// Rewrites the first `from` in a file to `to`, the same length, in place.
    fn patch(path: &Path, from: &[u8], to: &[u8]) {
        assert_eq!(from.len(), to.len());
        let mut bytes = std::fs::read(path).unwrap();
        let at = bytes
            .windows(from.len())
            .position(|w| w == from)
            .unwrap_or_else(|| panic!("{} holds no {:?}", path.display(), from));
        bytes[at..at + from.len()].copy_from_slice(to);
        std::fs::write(path, bytes).unwrap();
    }

    /// **A `.tx` an older crust wrote from a source with alpha is stale**,
    /// though it is newer than the source: it holds RGB (crust dropped alpha
    /// before `crust:txversion` existed), so its cutout would read opaque
    /// and `--auto-tx` must reconvert it. One from a source without alpha is
    /// current, and so is any `.tx` crust did not write (no `crust:mipspace`).
    #[test]
    fn a_tx_written_before_alpha_is_stale_for_a_source_that_has_one() {
        let dir = scratch("predates_alpha");
        let cut = dir.join("cut.png");
        image::RgbaImage::from_fn(8, 8, |x, _| image::Rgba([90, 160, 40, (x * 30) as u8]))
            .save(&cut)
            .expect("png");
        let solid = dir.join("solid.png");
        image::RgbImage::from_pixel(8, 8, image::Rgb([90, 160, 40]))
            .save(&solid)
            .expect("png");
        let cut_exr = dir.join("cut_src.exr");
        exr::prelude::write_rgba_file(&cut_exr, 4, 4, |x, _| {
            (0.5f32, 0.25f32, 0.1f32, x as f32 / 4.0)
        })
        .expect("exr");

        // What each conversion writes now: versioned, and current.
        let made = make_tx(&cut, &dir.join("cut.tx"), ColorSpace::AUTO, TxFormat::Tiff).unwrap();
        assert!(made.alpha);
        assert_eq!(super::super::crust_tx_version(&made.dst), Some(2));
        assert_eq!(tx_staleness(&cut, &made.dst), None);
        let made = make_tx(
            &cut_exr,
            &dir.join("cut_exr.tx"),
            ColorSpace::RAW,
            TxFormat::Exr,
        )
        .unwrap();
        assert_eq!(super::super::crust_tx_version(&made.dst), Some(2));
        assert_eq!(tx_staleness(&cut_exr, &made.dst), None);

        // What an older crust wrote beside each: RGB, with no version.
        let old_tiff = dir.join("old.tx");
        make_tx(&solid, &old_tiff, ColorSpace::AUTO, TxFormat::Tiff).unwrap();
        patch(&old_tiff, b" crust:txversion=2", b"                  ");
        assert_eq!(super::super::crust_tx_version(&old_tiff), Some(1));
        let old_exr = dir.join("old_exr.tx");
        make_tx(&solid, &old_exr, ColorSpace::RAW, TxFormat::Exr).unwrap();
        patch(&old_exr, b"crust:txversion", b"crust:txversioX");
        assert_eq!(super::super::crust_tx_version(&old_exr), Some(1));

        for old in [&old_tiff, &old_exr] {
            assert_eq!(
                tx_staleness(&cut, old),
                Some(TxStaleness::PredatesAlpha),
                "{}",
                old.display()
            );
            assert_eq!(
                tx_staleness(&cut_exr, old),
                Some(TxStaleness::PredatesAlpha)
            );
            assert_eq!(tx_staleness(&solid, old), None, "no alpha to have dropped");
        }

        // A `.tx` crust did not write is OIIO's business, whatever it holds.
        patch(&old_tiff, b"crust:mipspace", b"oiio_:mipspace");
        assert_eq!(super::super::crust_tx_version(&old_tiff), None);
        assert_eq!(tx_staleness(&cut, &old_tiff), None);
    }

    #[test]
    fn staleness_follows_modification_time() {
        let dir = scratch("stale");
        let src = dir.join("s.png");
        let dst = dir.join("s.tx");
        assert_eq!(
            tx_staleness(&src, &dst),
            Some(TxStaleness::Missing),
            "missing is stale"
        );
        std::fs::write(&dst, b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&src, b"y").unwrap();
        assert_eq!(
            tx_staleness(&src, &dst),
            Some(TxStaleness::Older),
            "older than its source"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&dst, b"z").unwrap();
        assert_eq!(tx_staleness(&src, &dst), None);
    }
}
