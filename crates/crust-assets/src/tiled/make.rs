//! Converting an ordinary image into a `.tx` as Arnold 7.5's auto-tx does —
//! the library half of `maketx`.
//!
//! Lives here rather than in the example so the renderer can do it too
//! (`crust --auto-tx`, through [`crate::FileAssets::with_auto_tx`]).
//! There is one conversion in the workspace, which is what keeps a `.tx` the
//! CLI made on first use identical to one converted by hand.
//!
//! The file is the one Arnold would write for the same binding: under the
//! name Arnold gives it ([`tx_name`]), of the type and sample type Arnold
//! picks, converted and reduced by OIIO's `maketx` pipeline
//! ([`super::maketx`]), with OIIO's metadata, the `Software` string Arnold's
//! update check compares, and the source's modification time — so either
//! renderer uses the other's file (design decisions 1 to 4 of
//! `tx-arnold-convention`).

use super::TILE_EDGE;
use super::maketx::{self, Image, SampleType};
use super::write::{TxMeta, now_stamp};
use crust_core::color::{Space, Spelling};
use crust_core::{ColorSpace, ResolvedColorSpace};

use crate::error::AssetError;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What one conversion produced.
#[derive(Debug)]
pub struct MadeTx {
    pub dst: PathBuf,
    /// `"8-bit, tiff"`, `"half, exr, dwaa"`, `"float, exr"`, …
    pub kind: &'static str,
    /// The space the binding resolved to, `auto` decided against the source.
    pub space: ResolvedColorSpace,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// The `.tx` carries alpha: the source had one, and it cut something (an
    /// alpha opaque everywhere is not written).
    pub alpha: bool,
}

/// How Arnold stores a source's `.tx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Store {
    Tiff(SampleType),
    Exr { depth: SampleType, dwaa: bool },
}

/// The prefix of the `Software` string: Arnold 7.5's OIIO. Arnold's update
/// check compares the whole string, prefix included (measured, design
/// decision 4), so a file with any other is rebuilt — and on Windows that
/// rebuild aborts the render.
const SOFTWARE: &str = "OpenImageIO-Arnold 2.6.3.0dev";

/// The flags Arnold 7.5's auto-tx runs `maketx` with, before the per-binding
/// ones.
const MAKETX_FLAGS: &str = "--opaque-detect --constant-color-detect --monochrome-detect \
                            --fixnan box3 --oiio --attrib tiff:half 1 --unpremult -u";

/// Converts `src`, bound in `space`, into a `.tx` at `dst` converting to
/// `target`, as Arnold 7.5's auto-tx would write it there; the `Software`
/// string names `dst`. Most callers want [`make_tx_atomic`], which writes the
/// file Arnold names beside its source.
pub fn make_tx(
    src: &Path,
    dst: &Path,
    space: ColorSpace,
    target: Space,
) -> Result<MadeTx, AssetError> {
    convert(src, dst, dst, space, target)
}

/// Writes `src`'s `.tx` to `write_to`, its `Software` string naming `named`
/// (the final name, which `write_to` is a temporary beside).
fn convert(
    src: &Path,
    write_to: &Path,
    named: &Path,
    space: ColorSpace,
    target: Space,
) -> Result<MadeTx, AssetError> {
    if src == write_to {
        return Err(AssetError::unusable(src, "input is already a .tx"));
    }
    if is_ptex(src) {
        return Err(AssetError::unusable(
            src,
            "a Ptex file is already a tiled per-face mip pyramid and is streamed as is \
             (CRUST_PTEX_STREAM) — it is never converted to .tx",
        ));
    }
    let name = tx_name(src, space, target)
        .ok_or_else(|| AssetError::unusable(src, "cannot read the source's header"))?;
    let (mut image, sample) = decode_source(src)?;
    if image.width == 0 || image.height == 0 {
        return Err(AssetError::unusable(src, "zero-sized image"));
    }
    let resolved = space
        .resolved()
        .unwrap_or_else(|| space.resolve_auto(matches!(sample, SampleType::U8 | SampleType::U16)));

    // Arnold writes `half` for an 8-bit source in a space it does not take
    // as linear (a curve, or the keyword `raw`), and keeps every other
    // source's sample type.
    let linear = match name.conversion {
        None => space.spelling() != Spelling::RawKeyword,
        Some((c, _)) => c.is_data() || c.is_scene_linear(),
    };
    let to_half = sample == SampleType::U8 && !linear;

    // OIIO's order: fix NaNs, detect opaque alpha, a constant colour and a
    // grey image, then convert unpremultiplied.
    maketx::fix_nan_box3(&mut image);
    maketx::drop_opaque_alpha(&mut image);
    let mut constant = maketx::shrink_constant(&mut image, TILE_EDGE);
    maketx::to_monochrome(&mut image);
    let constant_channels: Vec<&'static str> = match &constant {
        Some(c) if c.len() == image.nchannels() => image.channels.clone(),
        Some(c) => ["R", "G", "B", "A"][..c.len()].to_vec(),
        None => Vec::new(),
    };
    let mut average = maketx::average(&image);
    if let Some((c, t)) = name.conversion.filter(|(c, _)| !c.is_data()) {
        let conversion = crust_core::color::conversion(c, t);
        maketx::unpremultiply(&mut image);
        maketx::convert(&mut image, &conversion);
        maketx::premultiply(&mut image);
        maketx::convert_one(&mut average, &image.channels, &conversion);
        if let Some(c) = &mut constant {
            maketx::convert_one(c, &constant_channels, &conversion);
        }
    }

    let store = match sample {
        SampleType::U8 if to_half => Store::Exr {
            depth: SampleType::Half,
            // DWAA only for colour: a `Y` image is zipped, as OIIO falls back.
            dwaa: image.colour() == 3,
        },
        SampleType::U8 | SampleType::U16 => Store::Tiff(sample),
        depth => Store::Exr { depth, dwaa: false },
    };
    let meta = TxMeta {
        software: command_line(src, named, to_half, name.conversion),
        sha1: level0_hash(&image, name.conversion),
        average: maketx::format_colour(&average),
        constant: constant.as_deref().map(maketx::format_colour),
        date: now_stamp(),
    };
    let alpha = image.alpha().is_some();
    let levels = maketx::mip_chain(image);
    let written = match store {
        Store::Tiff(depth) => super::write::write_tiff(write_to, &levels, depth, &meta),
        Store::Exr { depth, dwaa } => {
            super::exr_write::write_exr(write_to, &levels, depth, dwaa, &meta)
        }
    };
    written.map_err(AssetError::io(write_to))?;
    stamp_as_source(src, write_to).map_err(AssetError::io(write_to))?;

    Ok(MadeTx {
        dst: write_to.to_path_buf(),
        kind: match store {
            Store::Tiff(SampleType::U16) => "16-bit, tiff",
            Store::Tiff(_) => "8-bit, tiff",
            Store::Exr {
                depth: SampleType::Half,
                dwaa: true,
            } => "half, exr, dwaa",
            Store::Exr {
                depth: SampleType::Half,
                ..
            } => "half, exr",
            Store::Exr { .. } => "float, exr",
        },
        space: resolved,
        bytes_in: std::fs::metadata(src).map(|m| m.len()).unwrap_or(0),
        bytes_out: std::fs::metadata(write_to).map(|m| m.len()).unwrap_or(0),
        alpha,
    })
}

/// `src` as `maketx` reads it: `f32` samples, its channels named, and the
/// sample type it stores. Alpha is **associated** as OIIO's readers associate
/// it: a straight 8-bit sample `c` becomes `round(c · a / 255)` (16-bit:
/// `/ 65535`) before anything else, and an EXR is associated already. An
/// alpha the file declares but that is not coverage (a TIFF's unspecified
/// extra sample) is dropped.
fn decode_source(src: &Path) -> Result<(Image, SampleType), AssetError> {
    let exr = src
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exr"));
    if exr {
        return decode_exr(src);
    }
    let (img, authored_alpha, associated) = crate::image_file::decode_with_alpha_kind(src)?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let hdr = src
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("hdr"));
    use image::DynamicImage as D;
    let (channels, pixels, sample): (Vec<&'static str>, Vec<f32>, SampleType) = match &img {
        D::ImageLuma8(b) => (vec!["Y"], scale_u8(b.as_raw()), SampleType::U8),
        D::ImageLumaA8(b) => (vec!["Y", "A"], scale_u8(b.as_raw()), SampleType::U8),
        D::ImageRgb8(b) => (vec!["R", "G", "B"], scale_u8(b.as_raw()), SampleType::U8),
        D::ImageRgba8(b) => (
            vec!["R", "G", "B", "A"],
            scale_u8(b.as_raw()),
            SampleType::U8,
        ),
        D::ImageLuma16(b) => (vec!["Y"], scale_u16(b.as_raw()), SampleType::U16),
        D::ImageLumaA16(b) => (vec!["Y", "A"], scale_u16(b.as_raw()), SampleType::U16),
        D::ImageRgb16(b) => (vec!["R", "G", "B"], scale_u16(b.as_raw()), SampleType::U16),
        D::ImageRgba16(b) => (
            vec!["R", "G", "B", "A"],
            scale_u16(b.as_raw()),
            SampleType::U16,
        ),
        D::ImageRgba32F(b) => (
            vec!["R", "G", "B", "A"],
            b.as_raw().clone(),
            SampleType::Float,
        ),
        other if hdr || matches!(other.color(), image::ColorType::Rgb32F) => (
            vec!["R", "G", "B"],
            other.to_rgb32f().into_raw(),
            SampleType::Float,
        ),
        other => (
            vec!["R", "G", "B", "A"],
            scale_u8(other.to_rgba8().as_raw()),
            SampleType::U8,
        ),
    };
    let mut image = Image {
        width: w,
        height: h,
        channels,
        pixels,
    };
    if image.alpha().is_some() && !authored_alpha {
        // Not coverage: read as the colour it carries.
        let a = image.alpha().expect("checked");
        let n = image.nchannels();
        image.pixels = image
            .pixels
            .chunks_exact(n)
            .flat_map(|t| {
                t.iter()
                    .enumerate()
                    .filter(|&(c, _)| c != a)
                    .map(|(_, &v)| v)
            })
            .collect();
        image.channels.remove(a);
    } else if image.alpha().is_some() && !associated {
        associate(&mut image, sample);
    }
    Ok((image, sample))
}

fn scale_u8(v: &[u8]) -> Vec<f32> {
    v.iter().map(|&b| f32::from(b) / 255.0).collect()
}

fn scale_u16(v: &[u16]) -> Vec<f32> {
    v.iter().map(|&s| f32::from(s) / 65535.0).collect()
}

/// Premultiplies straight integer samples the way OIIO's readers do, in the
/// integer domain: `round(c · a / max)` (measured on Arnold's `cut_empty`).
fn associate(image: &mut Image, sample: SampleType) {
    let Some(a) = image.alpha() else { return };
    let max: u64 = match sample {
        SampleType::U16 => 65535,
        _ => 255,
    };
    let int = |v: f32| (v * max as f32).round() as u64;
    let n = image.nchannels();
    for t in image.pixels.chunks_exact_mut(n) {
        let alpha = int(t[a]);
        for (c, v) in t.iter_mut().enumerate() {
            if c != a {
                *v = ((int(*v) * alpha + max / 2) / max) as f32 / max as f32;
            }
        }
    }
}

/// An EXR source's first layer: `R G B (A)` found by base name (so a
/// layer-prefixed `rgba.R` is red), or a `Y` (or only) channel, with the
/// sample type of its first colour channel.
fn decode_exr(src: &Path) -> Result<(Image, SampleType), AssetError> {
    use exr::prelude::FlatSamples;
    let image = exr::prelude::read_first_flat_layer_from_file(src).map_err(AssetError::exr(src))?;
    let layer = &image.layer_data;
    let (w, h) = (layer.size.width(), layer.size.height());
    let list = &layer.channel_data.list;
    let find = |want: &str| {
        list.iter().position(|c| {
            let name = c.name.to_string();
            name.rsplit('.')
                .next()
                .is_some_and(|base| base.eq_ignore_ascii_case(want))
        })
    };
    let rgb = [find("R"), find("G"), find("B")];
    let picked: Vec<(&'static str, usize)> = match rgb {
        [Some(r), Some(g), Some(b)] => vec![("R", r), ("G", g), ("B", b)],
        _ => match find("Y").or_else(|| (list.len() == 1).then_some(0)) {
            Some(y) => vec![("Y", y)],
            None => return Err(AssetError::unusable(src, "no R/G/B or Y channel")),
        },
    };
    let picked: Vec<(&'static str, usize)> = picked
        .into_iter()
        .chain(find("A").map(|a| ("A", a)))
        .collect();
    let sample = match &list[picked[0].1].sample_data {
        FlatSamples::F16(_) => SampleType::Half,
        _ => SampleType::Float,
    };
    let plane = |i: usize| -> Vec<f32> {
        match &list[i].sample_data {
            FlatSamples::F16(v) => v.iter().map(|s| s.to_f32()).collect(),
            FlatSamples::F32(v) => v.clone(),
            FlatSamples::U32(v) => v.iter().map(|&s| s as f32).collect(),
        }
    };
    let planes: Vec<Vec<f32>> = picked.iter().map(|&(_, i)| plane(i)).collect();
    let pixels = (0..w * h)
        .flat_map(|t| planes.iter().map(move |p| p[t]))
        .collect();
    Ok((
        Image {
            width: w,
            height: h,
            channels: picked.iter().map(|&(n, _)| n).collect(),
            pixels,
        },
        sample,
    ))
}

/// The `Software` string Arnold 7.5 records for this conversion: its
/// `maketx` command line, paths absolute with forward slashes, and any
/// argument with a space in quotes, as OIIO rebuilds the command line.
fn command_line(
    src: &Path,
    dst: &Path,
    to_half: bool,
    conversion: Option<(Space, Space)>,
) -> String {
    let quote = |s: &str| {
        if s.contains(' ') {
            format!("\"{s}\"")
        } else {
            s.to_string()
        }
    };
    let mut line = format!(
        "{SOFTWARE} : maketx {} {MAKETX_FLAGS}",
        quote(&arnold_path(src))
    );
    if to_half {
        line.push_str(" --format exr -d half --compression dwaa");
    }
    if let Some((c, t)) = conversion {
        line.push_str(&format!(
            " --colorconvert {} {}",
            quote(c.name()),
            quote(t.name())
        ));
    }
    line.push_str(" --attrib autotx 1");
    // An EXR source gets `--format exr` last, as Arnold passes it.
    let exr = src
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exr"));
    if exr {
        line.push_str(" --format exr");
    }
    line.push_str(&format!(" -o {}", quote(&arnold_path(dst))));
    line
}

/// A path as Arnold hands it to `maketx`: absolute, with forward slashes.
fn arnold_path(p: &Path) -> String {
    let path = std::path::absolute(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned();
    // A Windows verbatim path (`\\?\D:\…`, as a canonicalised import
    // resolves an asset) is not the spelling Arnold writes: it has no prefix.
    let path = match path.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(share) => format!(r"\\{share}"),
            None => rest.to_string(),
        },
        None => path,
    };
    path.replace('\\', "/")
}

/// The `oiio:SHA-1` crust records: level 0's samples as converted, and the
/// conversion. OIIO hashes its own buffer with its own extra data, which this
/// does not reproduce (a recorded gap); OIIO uses the hash only to share
/// identical textures in its cache, so a different one costs that sharing
/// and nothing else.
fn level0_hash(image: &Image, conversion: Option<(Space, Space)>) -> String {
    let mut bytes: Vec<u8> = image.pixels.iter().flat_map(|v| v.to_le_bytes()).collect();
    if let Some((c, t)) = conversion {
        bytes.extend_from_slice(format!("{} {}", c.name(), t.name()).as_bytes());
    }
    super::sha1::sha1_hex(&bytes)
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

/// The `.tx` Arnold 7.5's auto-tx gives a source bound in a space, converting
/// to a target space: the one naming rule, used by both the lookup and the
/// conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxName {
    /// `<stem>_raw.<ext>.tx` or `<stem>_<C>_<T>.<ext>.tx`, beside the source.
    pub path: PathBuf,
    /// The conversion the name records, `(C, T)`: `None` for `_raw`, whose
    /// values are the source's.
    pub conversion: Option<(Space, Space)>,
    /// The space the file's values are read in (the effective space of
    /// design decision 2): the target for a converted file, nothing for one
    /// converted from a data space (`Raw` to `T` leaves data as it was), and
    /// the binding's own space, `auto` resolved, for `_raw`.
    pub read_as: ColorSpace,
}

/// The `.tx` beside `src` for a binding in `space`, with `target` the space
/// conversions go to (`--tx-color-space`): Arnold's name, computed and never
/// parsed (design decision 1 of `tx-arnold-convention`).
///
/// - `_raw` when nothing is converted: no space authored, the keyword `raw`,
///   or the binding already in `target`.
/// - `_<C>_<T>` otherwise, both the OCIO config's own names: an alias such as
///   `srgb_tx` names the file as its canonical space does, and the config's
///   `Raw` space is `_Raw_<T>`, apart from the keyword's `_raw`.
///
/// `auto` is resolved first, from `src`'s header (an integer image is sRGB).
/// The stem keeps everything before the last extension, a UDIM token or
/// number included: `chair.1001.tif` gives `chair.1001_…_ACEScg.tif.tx`.
/// `None` when `auto` has to be resolved and `src` cannot be read.
pub fn tx_name(src: &Path, space: ColorSpace, target: Space) -> Option<TxName> {
    // An `auto` that resolves to "as stored" names no space, as an unset one.
    let (resolved, spelling) = match space.resolved() {
        Some(resolved) => (resolved, space.spelling()),
        None => (space.resolve_auto(source_is_integer(src)?), Spelling::Unset),
    };
    let conversion = if resolved.is_raw() {
        (spelling == Spelling::Named && target != Space::RAW).then_some((Space::RAW, target))
    } else {
        (resolved.source() != target).then_some((resolved.source(), target))
    };
    let file = src.file_name()?.to_string_lossy();
    let (stem, ext) = match file.rsplit_once('.') {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (&*file, String::new()),
    };
    let tag = match conversion {
        None => "raw".to_string(),
        Some((c, t)) => format!("{}_{}", c.name(), t.name()),
    };
    let read_as = match conversion {
        Some((c, _)) if c.is_data() => ColorSpace::RAW,
        Some((_, t)) => ColorSpace::new(t, space.working()),
        None => ColorSpace::from(resolved),
    };
    Some(TxName {
        path: src.with_file_name(format!("{stem}_{tag}{ext}.tx")),
        conversion,
        read_as,
    })
}

/// Whether `src`'s samples are integers, from its header: what `auto`
/// resolves on. An EXR is float; a `<UDIM>` set answers for its first tile.
fn source_is_integer(src: &Path) -> Option<bool> {
    let first = crate::texture_files(src).into_iter().next()?;
    let exr = first
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exr"));
    if exr {
        Some(false)
    } else {
        crate::image_file::header_is_integer(&first)
    }
}

/// Why the `.tx` at `dst` is not what converting `src` now would write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxStaleness {
    /// There is no `.tx`.
    Missing,
    /// The `.tx`'s modification time is not its source's: the source was
    /// saved after the conversion (or before, by a clock that moved).
    TimeDiffers,
}

/// Whether, and why, a conversion would change what the renderer reads from
/// `dst` ([`TxStaleness`]), or `None` when the `.tx` is current.
///
/// Arnold's rule (`maketx -u`): a `.tx` is current when its modification time
/// is its source's, which the conversion sets it to ([`stamp_as_source`]).
/// Compared in **whole seconds**, the resolution OIIO records it at, so a
/// `.tx` Arnold wrote beside a source saved at a fraction of a second is
/// current, not reconverted. An unreadable time counts as current rather
/// than stale: re-converting a file whose age cannot be told would happen on
/// every render.
pub fn tx_staleness(src: &Path, dst: &Path) -> Option<TxStaleness> {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mtime(src).and_then(whole_seconds), mtime(dst)) {
        (_, None) if !dst.exists() => Some(TxStaleness::Missing),
        (Some(s), Some(d)) if whole_seconds(d) != Some(s) => Some(TxStaleness::TimeDiffers),
        _ => None,
    }
}

/// `t` as whole seconds since the Unix epoch, truncated.
fn whole_seconds(t: SystemTime) -> Option<u64> {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Sets `dst`'s modification time to `src`'s, truncated to whole seconds, as
/// OIIO's `maketx` stamps the `.tx` it writes: what makes the `.tx` current
/// for crust's [`tx_staleness`] and for Arnold's own update check alike.
pub fn stamp_as_source(src: &Path, dst: &Path) -> std::io::Result<()> {
    let when = std::fs::metadata(src)?.modified()?;
    let when = whole_seconds(when)
        .map(|s| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s))
        .unwrap_or(when);
    std::fs::File::options()
        .write(true)
        .open(dst)?
        .set_modified(when)
}

/// Converts `src` into the `.tx` Arnold names beside it ([`tx_name`]),
/// through a temporary renamed over it on success.
///
/// A conversion killed halfway — the render interrupted, the disk full —
/// would otherwise leave a truncated `.tx` that the next render trusts
/// because it exists and carries its source's time. The rename is atomic on
/// one filesystem, and the temporary is in the `.tx`'s own directory to keep
/// it on one filesystem.
pub fn make_tx_atomic(src: &Path, space: ColorSpace, target: Space) -> Result<MadeTx, AssetError> {
    let dst = tx_name(src, space, target)
        .ok_or_else(|| AssetError::unusable(src, "cannot read the source's header"))?
        .path;
    let tmp = dst.with_extension(format!(
        "tx.tmp{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    match convert(src, &tmp, &dst, space, target) {
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

    fn acescg() -> Space {
        Space::named("ACEScg").expect("ACEScg")
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("crust_make_tx_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// Arnold's names, from the binding: an alias names its canonical space,
    /// the target itself and the keyword `raw` are `_raw`, the config's `Raw`
    /// space is not, and a UDIM number stays in the stem.
    #[test]
    fn a_tx_is_named_as_arnold_names_it() {
        let dir = scratch("names");
        let t = acescg();
        let w = Space::LIN_REC709;
        let name = |file: &str, space: ColorSpace| {
            let n = tx_name(&dir.join(file), space, t).expect("named");
            n.path.file_name().unwrap().to_string_lossy().into_owned()
        };
        let srgb = ColorSpace::from_mtlx(Some("srgb_tx"), w);
        assert_eq!(
            name("chair.1001.tif", srgb),
            "chair.1001_sRGB Encoded Rec.709 (sRGB)_ACEScg.tif.tx"
        );
        assert_eq!(name("wall.png", ColorSpace::new(t, w)), "wall_raw.png.tx");
        assert_eq!(name("m.png", ColorSpace::RAW), "m_raw.png.tx");
        assert_eq!(name("m.png", ColorSpace::RAW_KEYWORD), "m_raw.png.tx");
        let ocio_raw = ColorSpace::from_mtlx(Some("Raw"), w);
        assert_eq!(name("m.png", ocio_raw), "m_Raw_ACEScg.png.tx");
        let lin = ColorSpace::from_mtlx(Some("lin_rec709"), w);
        let n = tx_name(&dir.join("m.exr"), lin, t).expect("named");
        assert_eq!(n.conversion, Some((Space::LIN_REC709, t)));
        assert!(n.path.ends_with("m_Linear Rec.709 (sRGB)_ACEScg.exr.tx"));
        // Into another target, the target's own name.
        let n = tx_name(&dir.join("m.png"), srgb, Space::LIN_REC709).expect("named");
        assert!(
            n.path
                .ends_with("m_sRGB Encoded Rec.709 (sRGB)_Linear Rec.709 (sRGB).png.tx")
        );
        // `auto` needs the file, and declines without one.
        assert_eq!(tx_name(&dir.join("absent.png"), ColorSpace::AUTO, t), None);
    }

    /// The `Software` string is Arnold's command line, quoting what has a
    /// space, with the half-EXR flags only for an 8-bit source Arnold
    /// converts to half.
    #[test]
    fn the_software_string_is_arnolds_command_line() {
        let t = acescg();
        let line = command_line(
            Path::new("C:/tex dir/a.png"),
            Path::new("C:/tex dir/a_x.png.tx"),
            true,
            Some((Space::SRGB_TEXTURE, t)),
        );
        assert_eq!(
            line,
            "OpenImageIO-Arnold 2.6.3.0dev : maketx \"C:/tex dir/a.png\" --opaque-detect \
             --constant-color-detect --monochrome-detect --fixnan box3 --oiio --attrib \
             tiff:half 1 --unpremult -u --format exr -d half --compression dwaa --colorconvert \
             \"sRGB Encoded Rec.709 (sRGB)\" ACEScg --attrib autotx 1 -o \"C:/tex dir/a_x.png.tx\""
        );
        let line = command_line(Path::new("C:/t/a.png"), Path::new("C:/t/a.tx"), false, None);
        assert!(
            line.ends_with("--unpremult -u --attrib autotx 1 -o C:/t/a.tx"),
            "{line}"
        );
        let line = command_line(Path::new("C:/t/a.exr"), Path::new("C:/t/a.tx"), false, None);
        assert!(
            line.ends_with("--attrib autotx 1 --format exr -o C:/t/a.tx"),
            "{line}"
        );
    }

    /// A verbatim Windows path, as a canonicalised import resolves an asset,
    /// is written without its `\\?\` prefix, as Arnold spells the path.
    #[cfg(windows)]
    #[test]
    fn a_verbatim_path_is_written_as_arnold_spells_it() {
        assert_eq!(arnold_path(Path::new(r"\\?\D:\t\a.png")), "D:/t/a.png");
        assert_eq!(
            arnold_path(Path::new(r"\\?\UNC\server\share\a.png")),
            "//server/share/a.png"
        );
    }

    #[test]
    fn an_atomic_conversion_leaves_only_the_tx_behind() {
        let dir = scratch("atomic");
        let src = dir.join("a.png");
        image::RgbImage::from_fn(8, 8, |x, y| {
            image::Rgb([200, (x * 20) as u8, (y * 9) as u8])
        })
        .save(&src)
        .expect("png");
        let made = make_tx_atomic(&src, ColorSpace::AUTO, acescg()).expect("converts");
        assert_eq!(
            made.dst,
            dir.join("a_sRGB Encoded Rec.709 (sRGB)_ACEScg.png.tx")
        );
        assert_eq!(made.kind, "half, exr, dwaa");
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
        assert_eq!(tx_staleness(&src, &made.dst), None, "stamped as its source");
    }

    #[test]
    fn a_failed_conversion_leaves_nothing_behind() {
        let dir = scratch("failed");
        let src = dir.join("broken.png");
        std::fs::write(&src, b"not a png").unwrap();
        assert!(make_tx_atomic(&src, ColorSpace::RAW, acescg()).is_err());
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(names.len(), 1, "only the source remains");
    }

    /// A float source keeps its sample type, whatever its range.
    #[test]
    fn a_float_exr_stays_float() {
        let dir = scratch("exr_backing");
        let src = dir.join("rough.exr");
        exr::prelude::write_rgb_file(&src, 4, 4, |_, _| (0.02f32, 0.5f32, 0.9f32)).expect("exr");
        let made = make_tx_atomic(&src, ColorSpace::AUTO, acescg()).expect("ok");
        assert_eq!(made.kind, "float, exr");
        assert_eq!(made.space, ResolvedColorSpace::RAW);
    }

    #[test]
    fn ptex_is_refused_before_anything_is_written() {
        let dir = scratch("ptex");
        for name in ["face.ptx", "face.PTEX"] {
            let src = dir.join(name);
            std::fs::write(&src, b"Ptex").unwrap();
            let err =
                make_tx_atomic(&src, ColorSpace::RAW, acescg()).expect_err("ptex must not convert");
            assert!(err.to_string().contains("Ptex"), "{err}");
            let written = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .any(|n| n.ends_with(".tx"));
            assert!(!written, "nothing written");
        }
        assert!(!is_ptex(&dir.join("a.png")));
    }

    /// A `.tx` is current when its time is its source's in whole seconds —
    /// as Arnold stamps it, the source's fraction truncated — and stale a
    /// second either way, newer as much as older.
    #[test]
    fn staleness_follows_modification_time_in_whole_seconds() {
        let dir = scratch("stale");
        let src = dir.join("s.png");
        let dst = dir.join("s.tx");
        std::fs::write(&src, b"y").unwrap();
        assert_eq!(
            tx_staleness(&src, &dst),
            Some(TxStaleness::Missing),
            "missing is stale"
        );
        std::fs::write(&dst, b"x").unwrap();
        let at = |p: &Path, t: SystemTime| {
            std::fs::File::options()
                .write(true)
                .open(p)
                .and_then(|f| f.set_modified(t))
                .unwrap()
        };
        let second = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_790_000_000);
        at(&src, second + std::time::Duration::from_millis(700));
        at(&dst, second);
        assert_eq!(tx_staleness(&src, &dst), None, "Arnold's truncated stamp");
        for t in [
            second - std::time::Duration::from_secs(1),
            second + std::time::Duration::from_secs(1),
        ] {
            at(&dst, t);
            assert_eq!(tx_staleness(&src, &dst), Some(TxStaleness::TimeDiffers));
        }
        stamp_as_source(&src, &dst).unwrap();
        assert_eq!(tx_staleness(&src, &dst), None, "stamped as its source");
    }
}
