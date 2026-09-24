//! Converting an ordinary image into a `.tx` — the library half of `maketx`.
//!
//! Lives here rather than in the example so the renderer can do it too
//! (`crust-render --auto-tx`, through [`crate::FileAssets::with_auto_tx`]).
//! There is one conversion in the workspace, which is what keeps a `.tx` the
//! CLI made on first use identical to one converted by hand.
//!
//! The mip filter is the one thing that cannot be delegated to OIIO's
//! `maketx`: levels are reduced by the same `reduce_half` / `reduce_half_linear`
//! as the in-memory pyramid, so a streamed render and a preloaded one agree
//! texel for texel (see "Streaming textures" in `CLAUDE.md`).

use crust_core::ColorSpace;
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
    /// The space recorded in the file (`crust:mipspace`), with
    /// [`ColorSpace::Auto`] resolved against the source.
    pub space: ColorSpace,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// The source holds values above 1.0 that a TIFF backing clipped.
    pub clipped: bool,
}

/// The source as it was authored: 8-bit samples in its own encoding, or
/// floats that are already light.
enum Source {
    Bytes(Vec<u8>),
    Floats(Vec<f32>),
}

/// Decodes `src`, reporting its pixel format as `(eight_bit, channels)` — what
/// [`ColorSpace::resolve_auto`] asks about.
fn decode(src: &Path) -> Result<(Source, usize, usize, (bool, u8)), String> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // EXR goes through crust's own reader: the workspace's `image` has no
    // `exr` feature, and it is the one reader that handles single-channel and
    // layer-prefixed channels.
    if ext == "exr" {
        let (pixels, w, h) =
            crate::read_exr_rgb(src).ok_or_else(|| "could not decode".to_string())?;
        return Ok((Source::Floats(pixels), w, h, (false, 3)));
    }
    let mut reader = image::ImageReader::open(src)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    // Trusted, locally authored assets; an 8K texture exceeds the default
    // allocation limit, which is exactly the size this exists for.
    reader.no_limits();
    let img = reader.decode().map_err(|e| e.to_string())?;
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
    if float {
        Ok((Source::Floats(img.to_rgb32f().into_raw()), w, h, (false, 3)))
    } else {
        Ok((Source::Bytes(img.to_rgb8().into_raw()), w, h, format))
    }
}

/// Converts `src` into a `.tx` at `dst`, recording `space` — `Auto` resolved
/// against the source's format — as the space it is to be bound with.
pub fn make_tx(
    src: &Path,
    dst: &Path,
    space: ColorSpace,
    format: TxFormat,
) -> Result<MadeTx, String> {
    if src == dst {
        return Err("input is already a .tx".into());
    }
    if is_ptex(src) {
        return Err(
            "a Ptex file is already a tiled per-face mip pyramid and is streamed as is \
                    (CRUST_PTEX_STREAM) — it is never converted to .tx"
                .into(),
        );
    }
    let (source, w, h, (eight_bit, channels)) = decode(src)?;
    if w == 0 || h == 0 {
        return Err("zero-sized image".into());
    }
    let space = space.resolve_auto(eight_bit, channels);

    let hdr = match &source {
        Source::Bytes(_) => false,
        Source::Floats(v) => v.iter().any(|&s| s > 1.0),
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
        let linear: Vec<f32> = match &source {
            Source::Floats(v) => v.iter().map(|&s| crate::to_linear(space, s)).collect(),
            Source::Bytes(v) => v
                .iter()
                .map(|&b| crate::to_linear(space, b as f32 / 255.0))
                .collect(),
        };
        super::write_tx_exr(dst, &linear, w, h, space).map_err(|e| e.to_string())?;
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
        super::write_tx(dst, &bytes, w, h, space).map_err(|e| e.to_string())?;
        "8-bit, tiff"
    };

    Ok(MadeTx {
        dst: dst.to_path_buf(),
        kind,
        space,
        bytes_in: std::fs::metadata(src).map(|m| m.len()).unwrap_or(0),
        bytes_out: std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0),
        clipped: !exr && hdr,
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

/// Whether `dst` is missing or older than `src` — i.e. whether a conversion
/// would change what the renderer reads. An unreadable modification time
/// counts as current rather than stale: re-converting a file whose age cannot
/// be told would happen on every render.
pub fn tx_is_stale(src: &Path, dst: &Path) -> bool {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mtime(src), mtime(dst)) {
        (_, None) if !dst.exists() => true,
        (Some(s), Some(d)) => d < s,
        _ => false,
    }
}

/// [`make_tx`] into a temporary beside `dst`, renamed over it on success.
///
/// A conversion killed halfway — the render interrupted, the disk full —
/// would otherwise leave a truncated `.tx` that the next render trusts
/// because it exists and is newer than its source. The rename is atomic on
/// one filesystem, and the temporary is in `dst`'s own directory to keep it
/// on one filesystem.
pub fn make_tx_atomic(src: &Path, space: ColorSpace, format: TxFormat) -> Result<MadeTx, String> {
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
                format!("could not move the converted file into place: {e}")
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
            make_tx_atomic(&src, ColorSpace::Auto, TxFormat::FromSampleType).expect("converts");
        assert_eq!(made.dst, dir.join("a.tx"));
        assert_eq!(made.kind, "8-bit, tiff");
        assert_eq!(made.space, ColorSpace::Srgb, "auto on 8-bit RGB is sRGB");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "no temporary left over: {names:?}");
        assert!(!tx_is_stale(&src, &made.dst));
    }

    #[test]
    fn a_failed_conversion_leaves_nothing_behind() {
        let dir = scratch("failed");
        let src = dir.join("broken.png");
        std::fs::write(&src, b"not a png").unwrap();
        assert!(make_tx_atomic(&src, ColorSpace::Raw, TxFormat::FromSampleType).is_err());
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(names.len(), 1, "only the source remains");
    }

    #[test]
    fn a_linear_exr_in_range_still_takes_the_exr_backing() {
        let dir = scratch("exr_backing");
        let src = dir.join("rough.exr");
        exr::prelude::write_rgb_file(&src, 4, 4, |_, _| (0.02f32, 0.5f32, 0.9f32)).expect("exr");
        let by_type = make_tx_atomic(&src, ColorSpace::Auto, TxFormat::FromSampleType).expect("ok");
        assert_eq!(by_type.kind, "half, exr");
        assert_eq!(by_type.space, ColorSpace::Raw);
        // `maketx`'s own default would have narrowed it to 8 bits.
        let by_range = make_tx(
            &src,
            &dir.join("r.tx"),
            ColorSpace::Raw,
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
            let err = make_tx_atomic(&src, ColorSpace::Raw, TxFormat::FromSampleType)
                .expect_err("ptex must not convert");
            assert!(err.contains("Ptex"), "{err}");
            assert!(!tx_sibling(&src).exists());
        }
        assert!(!is_ptex(&dir.join("a.png")));
    }

    #[test]
    fn staleness_follows_modification_time() {
        let dir = scratch("stale");
        let src = dir.join("s.png");
        let dst = dir.join("s.tx");
        assert!(tx_is_stale(&src, &dst), "missing is stale");
        std::fs::write(&dst, b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&src, b"y").unwrap();
        assert!(tx_is_stale(&src, &dst), "older than its source");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&dst, b"z").unwrap();
        assert!(!tx_is_stale(&src, &dst));
    }
}
