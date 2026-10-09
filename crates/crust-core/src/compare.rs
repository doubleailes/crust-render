//! "Did this image change, and by how much?" — what `crust diff` answers
//! (the `image-comparison` spec), as arithmetic over decoded planes.
//!
//! No I/O: the host decodes both files (crust-assets' `read_exr_planes`)
//! and hands the planes here, so crust-core still decodes nothing, and a
//! caller holding buffers in memory gets the very metrics `crust diff`
//! prints.
//!
//! Identity is bitwise over every channel of every layer, so a NaN or a
//! signed zero that moved counts, and two equal infinities (a depth's clear
//! value) do not. On the beauty (`R`, `G`, `B` at the image's size, in both
//! files) it adds the error metrics, against `a` as the reference. From the
//! two files' sampling stamps ([`crate::stamp`]) it judges whether the pixels
//! can be compared at all — which never changes the verdict on the pixels.

use crate::report::{Report, finite_or_null};
use crate::stamp::StampValue;
use serde::{Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The `format` of `crust diff --json`.
pub const DIFF_FORMAT: &str = "crust-diff/1";

/// At most this many differing beauty pixels are listed by value.
pub const MAX_LISTED_PIXELS: usize = 8;

/// One channel: its layer's size, and its samples as f32 in row order.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub size: (usize, usize),
    pub values: Vec<f32>,
}

/// What a file records about how it was rendered: its `crust:*` attributes
/// (empty for a file another renderer wrote) and its `colorInteropID`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stamp {
    pub attributes: BTreeMap<String, StampValue>,
    pub color_interop_id: Option<String>,
}

impl Stamp {
    /// Whether the file carries crust's stamp at all.
    pub fn is_present(&self) -> bool {
        !self.attributes.is_empty()
    }

    fn get(&self, name: &str) -> Option<&StampValue> {
        self.attributes.get(name)
    }

    fn int(&self, name: &str) -> Option<i64> {
        match self.get(name)? {
            StampValue::Int(v) => Some(i64::from(*v)),
            _ => None,
        }
    }

    fn float(&self, name: &str) -> Option<f32> {
        match self.get(name)? {
            StampValue::Float(v) => Some(*v),
            _ => None,
        }
    }
}

/// A decoded image: every named channel of every layer, by full name
/// (`layer.channel`, or the bare channel name in an unnamed layer), with the
/// first layer's size as the image's — a multi-part file's other layers may
/// differ, so each channel keeps its own.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Planes {
    /// Where the planes came from, as the report names them (a path).
    pub source: String,
    pub width: usize,
    pub height: usize,
    pub channels: BTreeMap<String, Channel>,
    pub stamp: Stamp,
}

/// One side of the comparison, as the report describes it.
#[derive(Debug, Clone, Serialize)]
pub struct Side {
    pub path: String,
    pub width: usize,
    pub height: usize,
    /// The `crust:*` attributes, by name; `null` when the file has none.
    pub stamp: Option<BTreeMap<String, StampJson>>,
}

/// A stamp value as JSON: a number, a pair or a string.
#[derive(Debug, Clone)]
pub struct StampJson(pub StampValue);

impl Serialize for StampJson {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match &self.0 {
            StampValue::Int(v) => s.serialize_i32(*v),
            StampValue::Int2(a, b) => [*a, *b].serialize(s),
            StampValue::Float(v) => finite_or_null(v, s),
            StampValue::Double(v) => finite_or_null(v, s),
            StampValue::Text(v) => s.serialize_str(v),
        }
    }
}

/// How one channel compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelStatus {
    Identical,
    /// Differs at `differing_pixels` pixels.
    Differs,
    /// A layer of its own size, the same in both files, that differs: no
    /// pixel of it lines up with the image's, so all of them count.
    LayerDiffers,
    /// The channel's layer has another size in each file.
    SizesDiffer,
    OnlyInA,
    OnlyInB,
}

/// One channel's line of the report.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelDiff {
    pub name: String,
    pub status: ChannelStatus,
    /// Pixels where this channel differs; `null` where the layer does not
    /// line up with the image (another size, or only in one file).
    pub differing_pixels: Option<u64>,
    /// The largest finite absolute difference, over the differing samples.
    #[serde(serialize_with = "finite_or_null")]
    pub max_abs: f32,
    /// The channel's layer size in `a` and in `b`, when it is not the image's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizes: Option<[(usize, usize); 2]>,
}

/// A differing beauty pixel, listed by value.
#[derive(Debug, Clone, Serialize)]
pub struct PixelDiff {
    pub x: usize,
    pub y: usize,
    pub a: [f32; 3],
    pub b: [f32; 3],
}

/// The beauty's error metrics, against `a`.
#[derive(Debug, Clone, Serialize)]
pub struct BeautyMetrics {
    /// The largest absolute and relative channel difference.
    #[serde(serialize_with = "finite_or_null")]
    pub max_abs: f32,
    #[serde(serialize_with = "finite_or_null")]
    pub max_rel: f32,
    #[serde(serialize_with = "finite_or_null")]
    pub mean_abs: f64,
    #[serde(serialize_with = "finite_or_null")]
    pub rmse: f64,
    /// `(a − b)² / (a² + 0.01)`, the mean over pixels and channels.
    #[serde(serialize_with = "finite_or_null")]
    pub relmse: f64,
    /// The same with the worst 0.1% of pixels discarded.
    #[serde(serialize_with = "finite_or_null")]
    pub relmse_trimmed: f64,
    /// The first differing pixels in row order, at most
    /// [`MAX_LISTED_PIXELS`].
    pub differing: Vec<PixelDiff>,
}

/// Whether the two files' pixels can be compared, from their stamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparabilityStatus {
    /// The stamps match on every value that changes what a pixel holds.
    Ok,
    /// They show a condition that makes pixel differences unreliable.
    Warn,
    /// A file carries no stamp.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comparability {
    pub status: ComparabilityStatus,
    pub notes: Vec<String>,
}

/// `crust diff`'s answer: the body of `crust-diff/1`, and what its text
/// report is rendered from.
#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    /// Same resolution, and every channel bitwise identical.
    pub identical: bool,
    pub a: Side,
    pub b: Side,
    pub resolution_match: bool,
    /// Pixels where any channel differs; `null` when the resolutions differ.
    pub differing_pixels: Option<u64>,
    /// `a`'s pixel count.
    pub total_pixels: u64,
    /// Every channel of either file, by name.
    pub channels: Vec<ChannelDiff>,
    /// Absent unless both files have a beauty at the image's size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beauty: Option<BeautyMetrics>,
    pub comparability: Comparability,
}

impl DiffReport {
    /// The `crust-diff/1` JSON report.
    pub fn to_json(&self) -> String {
        Report::new(DIFF_FORMAT, self).to_json()
    }

    /// The text report: the resolution and the pixels that differ, one line
    /// per differing channel, then the beauty's listed pixels and metrics.
    /// Comparability is not in it: the host prints its notes on stderr, so
    /// stdout reads as it always did.
    pub fn to_text(&self) -> String {
        let mut o = String::new();
        if !self.resolution_match {
            let _ = writeln!(
                o,
                "resolutions differ: {}x{} vs {}x{}",
                self.a.width, self.a.height, self.b.width, self.b.height
            );
            return o;
        }
        let total = self.total_pixels;
        let n = self.differing_pixels.unwrap_or(0);
        let _ = writeln!(
            o,
            "{}x{}  differing pixels: {n}/{total} ({:.4}%)",
            self.a.width,
            self.a.height,
            100.0 * n as f64 / total as f64
        );
        for c in &self.channels {
            let name = &c.name;
            let _ = match c.status {
                ChannelStatus::Identical => continue,
                ChannelStatus::Differs => writeln!(
                    o,
                    "  channel {name}: {} pixels differ, max abs diff {:e}",
                    c.differing_pixels.unwrap_or(0),
                    c.max_abs
                ),
                ChannelStatus::LayerDiffers => {
                    let (w, h) = c.sizes.map_or((0, 0), |s| s[0]);
                    writeln!(o, "  channel {name}: differs (a {w}x{h} layer)")
                }
                ChannelStatus::SizesDiffer => {
                    let [(aw, ah), (bw, bh)] = c.sizes.unwrap_or_default();
                    writeln!(o, "  channel {name}: sizes differ, {aw}x{ah} vs {bw}x{bh}")
                }
                ChannelStatus::OnlyInA => {
                    writeln!(o, "  channel {name}: only in {}", self.a.path)
                }
                ChannelStatus::OnlyInB => {
                    writeln!(o, "  channel {name}: only in {}", self.b.path)
                }
            };
        }
        let Some(m) = &self.beauty else {
            let _ = writeln!(o, "(no R, G, B in both files: no beauty metrics)");
            return o;
        };
        for p in &m.differing {
            let _ = writeln!(o, "  differs at ({}, {}): {:?} vs {:?}", p.x, p.y, p.a, p.b);
        }
        let _ = writeln!(
            o,
            "max abs diff: {:e}   max rel diff: {:e}",
            m.max_abs, m.max_rel
        );
        let _ = writeln!(o, "mean abs diff: {:e}", m.mean_abs);
        let _ = writeln!(o, "rmse: {:e}", m.rmse);
        let _ = writeln!(o, "relmse: {:e}", m.relmse);
        let _ = writeln!(o, "relmse (trimmed 0.1%): {:e}", m.relmse_trimmed);
        o
    }
}

fn side(p: &Planes) -> Side {
    Side {
        path: p.source.clone(),
        width: p.width,
        height: p.height,
        stamp: p.stamp.is_present().then(|| {
            p.stamp
                .attributes
                .iter()
                .map(|(k, v)| (k.clone(), StampJson(v.clone())))
                .collect()
        }),
    }
}

/// Compares `a` (the reference) with `b`.
pub fn compare(a: &Planes, b: &Planes) -> DiffReport {
    let (aw, ah) = (a.width, a.height);
    let total = aw * ah;
    let comparability = comparability(&a.stamp, &b.stamp);
    if (aw, ah) != (b.width, b.height) {
        return DiffReport {
            identical: false,
            a: side(a),
            b: side(b),
            resolution_match: false,
            differing_pixels: None,
            total_pixels: total as u64,
            channels: Vec::new(),
            beauty: None,
            comparability,
        };
    }

    // Any channel: which pixels differ, and which channels.
    let mut pixel_differs = vec![false; total];
    let mut channels = Vec::new();
    let mut names: Vec<&String> = a.channels.keys().chain(b.channels.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let mut line = ChannelDiff {
            name: name.clone(),
            status: ChannelStatus::Identical,
            differing_pixels: None,
            max_abs: 0.0,
            sizes: None,
        };
        match (a.channels.get(name), b.channels.get(name)) {
            (Some(x), Some(y)) if x.size != y.size || x.size != (aw, ah) => {
                line.sizes = Some([x.size, y.size]);
                if x.size != y.size {
                    line.status = ChannelStatus::SizesDiffer;
                    pixel_differs.iter_mut().for_each(|d| *d = true);
                } else if x
                    .values
                    .iter()
                    .zip(&y.values)
                    .any(|(p, q)| p.to_bits() != q.to_bits())
                {
                    line.status = ChannelStatus::LayerDiffers;
                    pixel_differs.iter_mut().for_each(|d| *d = true);
                }
            }
            (Some(x), Some(y)) => {
                let (x, y) = (&x.values, &y.values);
                let mut n = 0u64;
                let mut max_abs = 0.0f32;
                for p in 0..total {
                    if x[p].to_bits() != y[p].to_bits() {
                        n += 1;
                        pixel_differs[p] = true;
                        let d = (x[p] - y[p]).abs();
                        if d.is_finite() {
                            max_abs = max_abs.max(d);
                        }
                    }
                }
                line.differing_pixels = Some(n);
                line.max_abs = max_abs;
                if n > 0 {
                    line.status = ChannelStatus::Differs;
                }
            }
            (Some(_), None) => {
                line.status = ChannelStatus::OnlyInA;
                pixel_differs.iter_mut().for_each(|d| *d = true);
            }
            (None, Some(_)) => {
                line.status = ChannelStatus::OnlyInB;
                pixel_differs.iter_mut().for_each(|d| *d = true);
            }
            (None, None) => unreachable!("a name comes from one of the two"),
        }
        channels.push(line);
    }
    let differing = pixel_differs.iter().filter(|d| **d).count() as u64;
    DiffReport {
        identical: differing == 0,
        a: side(a),
        b: side(b),
        resolution_match: true,
        differing_pixels: Some(differing),
        total_pixels: total as u64,
        channels,
        beauty: beauty_metrics(a, b),
        comparability,
    }
}

/// The beauty's metrics, when both files have `R`, `G`, `B` at the image's
/// size (the resolutions already match).
fn beauty_metrics(a: &Planes, b: &Planes) -> Option<BeautyMetrics> {
    let (aw, ah) = (a.width, a.height);
    let total = aw * ah;
    fn rgb(p: &Planes, size: (usize, usize)) -> Option<[&[f32]; 3]> {
        let plane = |n: &str| {
            p.channels
                .get(n)
                .filter(|c| c.size == size)
                .map(|c| c.values.as_slice())
        };
        Some([plane("R")?, plane("G")?, plane("B")?])
    }
    let (a, b) = (rgb(a, (aw, ah))?, rgb(b, (aw, ah))?);
    let mut max_abs = 0.0f32;
    let mut max_rel = 0.0f32;
    let mut sum_abs = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut sum_rel_sq = 0.0f64;
    let mut listed = Vec::new();
    // Per-pixel relative squared error (mean over channels), for the trimmed
    // relMSE below.
    let mut pixel_rel = Vec::with_capacity(total);
    for p in 0..total {
        let mut differs = false;
        let mut rel = 0.0f64;
        for c in 0..3 {
            let (x, y) = (a[c][p], b[c][p]);
            let d = (x - y).abs();
            sum_abs += d as f64;
            sum_sq += (d as f64) * (d as f64);
            let r = (d as f64) * (d as f64) / ((x as f64) * (x as f64) + 1e-2);
            sum_rel_sq += r;
            rel += r / 3.0;
            if d != 0.0 {
                differs = true;
                max_abs = max_abs.max(d);
                let scale = x.abs().max(y.abs());
                if scale > 0.0 {
                    max_rel = max_rel.max(d / scale);
                }
            }
        }
        pixel_rel.push(rel);
        if differs && listed.len() < MAX_LISTED_PIXELS {
            listed.push(PixelDiff {
                x: p % aw,
                y: p / aw,
                a: [a[0][p], a[1][p], a[2][p]],
                b: [b[0][p], b[1][p], b[2][p]],
            });
        }
    }
    let samples = (total * 3) as f64;
    // RMSE beside the mean, because they answer different questions: the
    // mean is dominated by how *much* of the image moved, while the square
    // weights the outliers — which is what aliasing is.
    //
    // Relative MSE against `a`, the noise metric of the sampling literature,
    // and the one to use when `a` is a high-spp reference: unlike the RMSE it
    // is not dominated by the few pixels that see a light directly.
    //
    // The trimmed one discards the worst 0.1% of pixels, the convention of
    // the path-guiding literature (Müller et al. 2017 and after): a handful
    // of fireflies — one can carry an error of 1e5 — otherwise decide the
    // mean on their own.
    pixel_rel.sort_by(f64::total_cmp);
    let keep = total - total / 1000;
    Some(BeautyMetrics {
        max_abs,
        max_rel,
        mean_abs: sum_abs / samples,
        rmse: (sum_sq / samples).sqrt(),
        relmse: sum_rel_sq / samples,
        relmse_trimmed: pixel_rel[..keep].iter().sum::<f64>() / keep.max(1) as f64,
        differing: listed,
    })
}

/// The stamped values whose difference makes two renders' pixels differ for
/// a reason other than the change under test. `crust:minSpp` and
/// `crust:sppTaken` are judged by the adaptive rule; `crust:version` never
/// warns, since comparing two builds is what `diff` is for.
const COMPARED: [&str; 11] = [
    "crust:frame",
    "crust:camera",
    "crust:spp",
    "crust:maxDepth",
    "crust:lightSamples",
    "crust:lightSamplesIndirect",
    "crust:varianceThreshold",
    "crust:pixelFilter",
    "crust:pixelFilterRadius",
    "crust:samplingStrategy",
    "crust:lightSelection",
];

/// Whether a stamp shows adaptive sampling at work: a render the tracer's
/// own rule ([`crate::tracer::samples_adaptively`]) lets stop early — the
/// threshold on, and the budget past the first check point — or pixels that
/// took different counts.
fn adaptive(s: &Stamp) -> bool {
    let could_stop = match (
        s.int("crust:spp"),
        s.int("crust:minSpp"),
        s.float("crust:varianceThreshold"),
    ) {
        (Some(spp), Some(min), Some(threshold)) => {
            let count = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
            crate::tracer::samples_adaptively(count(spp), count(min), threshold)
        }
        _ => false,
    };
    let spread = matches!(s.get("crust:sppTaken"), Some(StampValue::Int2(lo, hi)) if lo != hi);
    could_stop || spread
}

/// The comparability verdict of the `image-comparison` spec, from the two
/// stamps alone.
pub fn comparability(a: &Stamp, b: &Stamp) -> Comparability {
    let mut notes = Vec::new();
    for (side, s) in [("a", a), ("b", b)] {
        if !s.is_present() {
            notes.push(format!("{side} has no crust:* sampling stamp"));
        }
    }
    if !notes.is_empty() {
        return Comparability {
            status: ComparabilityStatus::Unknown,
            notes,
        };
    }
    let show = |v: Option<&StampValue>| v.map_or_else(|| "(absent)".to_owned(), |v| v.to_string());
    let sides: Vec<&str> = [("a", a), ("b", b)]
        .into_iter()
        .filter(|(_, s)| adaptive(s))
        .map(|(side, _)| side)
        .collect();
    if !sides.is_empty() {
        notes.push(format!(
            "adaptive sampling was active in {}: a one-ulp difference can change a \
             pixel's sample budget and cascade; compare renders at a fixed budget \
             (-s at most crust:minSpp)",
            sides.join(" and ")
        ));
    }
    let (ca, cb) = (a.get("crust:indirectClamp"), b.get("crust:indirectClamp"));
    if ca != cb {
        notes.push(format!(
            "crust:indirectClamp differs ({} vs {}): the firefly clamp is biased, so \
             the metrics include its bias; render both with --indirect-clamp 0",
            show(ca),
            show(cb)
        ));
    }
    for name in COMPARED {
        let (va, vb) = (a.get(name), b.get(name));
        if va != vb {
            notes.push(format!("{name} differs: {} vs {}", show(va), show(vb)));
        }
    }
    if a.color_interop_id != b.color_interop_id {
        let id = |s: &Stamp| {
            s.color_interop_id
                .clone()
                .unwrap_or_else(|| "(absent)".into())
        };
        notes.push(format!("colorInteropID differs: {} vs {}", id(a), id(b)));
    }
    Comparability {
        status: if notes.is_empty() {
            ComparabilityStatus::Ok
        } else {
            ComparabilityStatus::Warn
        },
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`x`h` image with one layer of `names`, every sample `v`.
    fn planes(source: &str, (w, h): (usize, usize), names: &[&str], v: f32) -> Planes {
        Planes {
            source: source.into(),
            width: w,
            height: h,
            channels: names
                .iter()
                .map(|n| {
                    (
                        (*n).to_owned(),
                        Channel {
                            size: (w, h),
                            values: vec![v; w * h],
                        },
                    )
                })
                .collect(),
            stamp: Stamp::default(),
        }
    }

    fn set(p: &mut Planes, channel: &str, i: usize, v: f32) {
        p.channels.get_mut(channel).unwrap().values[i] = v;
    }

    fn json(r: &DiffReport) -> serde_json::Value {
        serde_json::from_str(&r.to_json()).unwrap()
    }

    #[test]
    fn identical_files_are_identical() {
        let a = planes("a.exr", (4, 2), &["R", "G", "B", "Z"], 0.5);
        let r = compare(&a, &a.clone());
        assert!(r.identical);
        assert_eq!(r.differing_pixels, Some(0));
        let m = r.beauty.as_ref().expect("a beauty");
        assert_eq!((m.rmse, m.relmse, m.relmse_trimmed), (0.0, 0.0, 0.0));
        assert!(
            r.to_text()
                .starts_with("4x2  differing pixels: 0/8 (0.0000%)\n")
        );
        let v = json(&r);
        assert_eq!(v["format"], DIFF_FORMAT);
        assert_eq!(v["identical"], true);
        assert_eq!(v["channels"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn a_moved_nan_counts_twice_and_equal_infinities_not_at_all() {
        let mut a = planes("a", (4, 1), &["R", "G", "B", "Z"], 1.0);
        let mut b = a.clone();
        set(&mut a, "R", 0, f32::NAN);
        set(&mut b, "R", 2, f32::NAN);
        for p in [&mut a, &mut b] {
            set(p, "Z", 1, f32::INFINITY);
        }
        let r = compare(&a, &b);
        assert!(!r.identical);
        assert_eq!(r.differing_pixels, Some(2));
        let z = r.channels.iter().find(|c| c.name == "Z").unwrap();
        assert_eq!(z.status, ChannelStatus::Identical);
        let red = r.channels.iter().find(|c| c.name == "R").unwrap();
        assert_eq!(red.differing_pixels, Some(2));
        // NaN differences are not finite: no max to report, and the metrics
        // that sum them are NaN — `null` in the JSON.
        let v = json(&r);
        assert!(v["beauty"]["rmse"].is_null());
    }

    #[test]
    fn a_channel_in_one_file_only_differs_everywhere() {
        let a = planes("a.exr", (3, 2), &["R", "G", "B", "albedo.R"], 0.0);
        let b = planes("b.exr", (3, 2), &["R", "G", "B"], 0.0);
        let r = compare(&a, &b);
        assert_eq!(r.differing_pixels, Some(6));
        let line = r.channels.iter().find(|c| c.name == "albedo.R").unwrap();
        assert_eq!(line.status, ChannelStatus::OnlyInA);
        assert!(r.to_text().contains("  channel albedo.R: only in a.exr\n"));
        assert_eq!(json(&r)["channels"][3]["status"], "only_in_a");
    }

    #[test]
    fn a_resolution_mismatch_states_both() {
        let a = planes("a", (640, 360), &["R"], 0.0);
        let b = planes("b", (320, 180), &["R"], 0.0);
        let r = compare(&a, &b);
        assert!(!r.identical && !r.resolution_match);
        assert_eq!(r.to_text(), "resolutions differ: 640x360 vs 320x180\n");
        let v = json(&r);
        assert!(v["differing_pixels"].is_null());
        assert_eq!(v["b"]["width"], 320);
    }

    #[test]
    fn without_a_beauty_there_are_no_metrics() {
        let a = planes("a", (2, 2), &["Z", "N.X"], 1.0);
        let mut b = a.clone();
        set(&mut b, "Z", 0, 2.0);
        let r = compare(&a, &b);
        assert!(r.beauty.is_none());
        assert!(json(&r).get("beauty").is_none());
        let text = r.to_text();
        assert!(text.contains("  channel Z: 1 pixels differ, max abs diff 1e0\n"));
        assert!(text.ends_with("(no R, G, B in both files: no beauty metrics)\n"));
    }

    #[test]
    fn an_infinite_relative_metric_is_null() {
        let a = planes("a", (2, 1), &["R", "G", "B"], 0.0);
        let mut b = a.clone();
        set(&mut b, "R", 0, f32::INFINITY);
        let r = compare(&a, &b);
        let m = r.beauty.as_ref().unwrap();
        assert!(m.relmse.is_infinite() && m.max_abs.is_infinite());
        let v = json(&r);
        assert!(v["beauty"]["relmse"].is_null());
        assert!(v["beauty"]["max_abs"].is_null());
    }

    #[test]
    fn at_most_eight_pixels_are_listed_in_row_order() {
        let a = planes("a", (100, 100), &["R", "G", "B"], 0.25);
        let mut b = a.clone();
        b.channels.get_mut("G").unwrap().values.fill(0.5);
        let r = compare(&a, &b);
        let m = r.beauty.as_ref().unwrap();
        assert_eq!(m.differing.len(), MAX_LISTED_PIXELS);
        assert_eq!((m.differing[1].x, m.differing[1].y), (1, 0));
        assert!(m.relmse_trimmed <= m.relmse);
    }

    fn stamp(pairs: &[(&str, StampValue)]) -> Stamp {
        Stamp {
            attributes: pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
            color_interop_id: None,
        }
    }

    /// A `-s 16` render's stamp, against a `minSpp` of 32.
    fn s16() -> Vec<(&'static str, StampValue)> {
        use StampValue::*;
        vec![
            ("crust:spp", Int(16)),
            ("crust:minSpp", Int(32)),
            ("crust:sppTaken", Int2(16, 16)),
            ("crust:indirectClamp", Float(0.0)),
            ("crust:varianceThreshold", Float(0.05)),
            ("crust:pixelFilter", Text("gaussian".into())),
            ("crust:pixelFilterRadius", Float(1.5)),
            ("crust:version", Text("0.6.0".into())),
        ]
    }

    fn with(mut s: Vec<(&'static str, StampValue)>, k: &'static str, v: StampValue) -> Stamp {
        s.retain(|(n, _)| *n != k);
        s.push((k, v));
        stamp(&s)
    }

    #[test]
    fn comparability_follows_the_stamps() {
        use StampValue::*;
        let base = stamp(&s16());
        let unknown = comparability(&Stamp::default(), &base);
        assert_eq!(unknown.status, ComparabilityStatus::Unknown);
        assert_eq!(comparability(&base, &base).status, ComparabilityStatus::Ok);

        let adaptive = with(s16(), "crust:spp", Int(64));
        let c = comparability(&base, &adaptive);
        assert_eq!(c.status, ComparabilityStatus::Warn);
        assert!(c.notes.iter().any(|n| n.contains("adaptive sampling")));
        assert!(c.notes.iter().any(|n| n.contains("crust:spp differs")));

        let clamped = with(s16(), "crust:indirectClamp", Float(10.0));
        let c = comparability(&clamped, &base);
        assert_eq!(c.status, ComparabilityStatus::Warn);
        assert!(
            c.notes
                .iter()
                .any(|n| n.contains("indirectClamp") && n.contains("bias"))
        );

        for (k, v) in [
            ("crust:pixelFilter", Text("box".into())),
            ("crust:pixelFilterRadius", Float(2.0)),
            ("crust:frame", Double(10.5)),
            ("crust:camera", Text("/cam".into())),
        ] {
            let c = comparability(&base, &with(s16(), k, v));
            assert_eq!(c.status, ComparabilityStatus::Warn, "{k}");
            assert!(
                c.notes.iter().any(|n| n.starts_with(k)),
                "{k}: {:?}",
                c.notes
            );
        }

        // The tracer's own rule, not `spp > minSpp`: with the threshold off,
        // a 1024 spp render takes its whole budget and nothing warns; at a
        // budget equal to the first check point (32) there is no round
        // after it either. A 1024 spp render with the threshold on can stop.
        let fixed = |spp: i32, threshold: f32| {
            let mut s = s16();
            s.retain(|(n, _)| !matches!(*n, "crust:spp" | "crust:varianceThreshold"));
            s.push(("crust:spp", Int(spp)));
            s.push(("crust:varianceThreshold", Float(threshold)));
            s.push(("crust:sppTaken", Int2(spp, spp)));
            stamp(&s)
        };
        let off = fixed(1024, 0.0);
        assert_eq!(comparability(&off, &off).status, ComparabilityStatus::Ok);
        assert_eq!(
            comparability(&fixed(32, 0.05), &fixed(32, 0.05)).status,
            ComparabilityStatus::Ok
        );
        let on = fixed(1024, 0.05);
        assert_eq!(comparability(&on, &on).status, ComparabilityStatus::Warn);

        let other_build = with(s16(), "crust:version", Text("0.7.0".into()));
        assert_eq!(
            comparability(&base, &other_build).status,
            ComparabilityStatus::Ok
        );
    }

    /// Comparability never changes the verdict on the pixels.
    #[test]
    fn comparability_never_decides_identity() {
        let mut a = planes("a", (2, 1), &["R", "G", "B"], 0.5);
        let mut b = a.clone();
        a.stamp = stamp(&s16());
        b.stamp = with(s16(), "crust:spp", StampValue::Int(64));
        let r = compare(&a, &b);
        assert!(r.identical);
        assert_eq!(r.comparability.status, ComparabilityStatus::Warn);
        b.stamp = Stamp::default();
        set(&mut b, "R", 0, 0.0);
        let r = compare(&a, &b);
        assert!(!r.identical);
        assert_eq!(r.comparability.status, ComparabilityStatus::Unknown);
    }
}
