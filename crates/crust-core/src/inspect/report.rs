//! The report, `crust-light-paths/1` (design D7): what [`super::run`]
//! returns, the JSON an agent parses, and what the Markdown is rendered
//! from, so the two cannot disagree.
//!
//! Field order is key order: serde writes a struct's fields as declared,
//! so each struct below is its JSON object's layout — the conclusion first
//! (`summary`, then `groups` before `records`), every cut stated
//! (`truncated`, `ambiguous_luma_share`, `prims_note`, `reproduces_frame`
//! with its reason). No timing: the same command writes the same bytes.

use std::fmt::Write;

use serde::Serialize;

use crate::RenderSettings;
use crate::report::finite_or_null;

/// The `format` of every report this version writes.
pub const FORMAT: &str = "crust-light-paths/1";

/// The whole report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub format: String,
    pub crust_version: String,
    /// One sentence built from the first group (or the brightest record):
    /// the dominant expression, its light, its shares and how rare it is.
    pub summary: String,
    /// The command that reproduces this inspection, in CLI syntax.
    pub command: Option<String>,
    /// `X0, Y0, X1, Y1` in image space, `X1` and `Y1` excluded: as
    /// `--region` takes it.
    pub region: [usize; 4],
    pub resolution: [usize; 2],
    /// The render camera's prim path.
    pub camera: Option<String>,
    pub settings: Settings,
    /// Whether the inspected samples are the frame's, bit for bit.
    pub reproduces_frame: bool,
    /// Why not, when they are not.
    pub reproduces_frame_reason: Option<String>,
    /// Camera samples taken over the region.
    pub samples: u64,
    pub consistency: Consistency,
    /// The share of the region's luminance the indirect clamp removed.
    #[serde(serialize_with = "finite_or_null")]
    pub clamp_removed_share: f64,
    /// The share of the region's luminance whose records are
    /// `label_ambiguous`: how much grouping by expression approximates.
    #[serde(serialize_with = "finite_or_null")]
    pub ambiguous_luma_share: f64,
    pub group_by: String,
    /// Best first: by `second_moment_share`, then `luma_share`.
    pub groups: Vec<Group>,
    /// The brightest records, brightest first.
    pub records: Vec<Record>,
    pub truncated: Truncated,
    /// Every inspected pixel, top-down, left to right.
    pub pixels: Vec<Pixel>,
    /// Why `prims` is null; absent once prims are resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prims_note: Option<String>,
    pub hints: Vec<Hint>,
}

impl Report {
    /// The JSON report, pretty-printed, with a final newline.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("a report always serializes");
        s.push('\n');
        s
    }
}

/// The settings the region was rendered with.
#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub spp: u32,
    pub max_depth: u32,
    /// `null` when off.
    pub indirect_clamp: Option<f32>,
    pub filter: String,
    pub filter_radius: f32,
    pub strategy: String,
    pub light_selection: String,
    pub light_samples: u32,
    pub light_samples_indirect: u32,
    pub variance_threshold: f32,
    pub min_samples_per_pixel: u32,
    pub adaptive_neighbour_tolerance: f32,
    pub path_guiding: bool,
    pub exposure_scale: f32,
    pub frame: isize,
}

impl Settings {
    pub(crate) fn of(s: &RenderSettings) -> Settings {
        Settings {
            spp: s.samples_per_pixel(),
            max_depth: s.max_depth(),
            indirect_clamp: s.indirect_clamp(),
            filter: s.pixel_filter().name().into(),
            filter_radius: s.pixel_filter().radius(),
            strategy: s.sampling_strategy().to_string(),
            light_selection: s.light_selection().to_string(),
            light_samples: s.light_samples(),
            light_samples_indirect: s.light_samples_indirect(),
            variance_threshold: s.variance_threshold(),
            min_samples_per_pixel: s.min_samples_per_pixel(),
            adaptive_neighbour_tolerance: s.adaptive_neighbour_tolerance(),
            path_guiding: s.guiding(),
            exposure_scale: s.exposure_scale(),
            frame: s.frame(),
        }
    }
}

/// How exactly the records rebuild the beauty: the largest relative
/// difference, per pixel (records weighted by their samples' filter
/// weights, against the pass's pixel) and per sample. Rounding is about
/// 1e-7; anything near 1e-3 or above means a term of the beauty has no
/// record.
#[derive(Debug, Clone, Serialize)]
pub struct Consistency {
    #[serde(serialize_with = "finite_or_null")]
    pub max_relative_error: f32,
    #[serde(serialize_with = "finite_or_null")]
    pub max_sample_relative_error: f32,
}

/// One group of records (design D6).
#[derive(Debug, Clone, Serialize)]
pub struct Group {
    /// The group's key as text: the expression, the light, or both
    /// (`<expression> via <light>`).
    pub key: String,
    /// `null` when grouping by light.
    pub expression: Option<String>,
    /// The light's prim path; `null` for emission that is no light's, a
    /// backdrop, or records ending on several lights.
    pub light: Option<String>,
    /// Its share of the region's luminance (`Σ wᵢ lumaᵢ / Σ w` per pixel,
    /// summed over the region), before the clamp; and its clamped
    /// luminance over the same total.
    #[serde(serialize_with = "finite_or_null")]
    pub luma_share: f64,
    #[serde(serialize_with = "finite_or_null")]
    pub luma_share_clamped: f64,
    /// Its share of the region's `Σ x²` over samples: what finds
    /// fireflies.
    #[serde(serialize_with = "finite_or_null")]
    pub second_moment_share: f64,
    /// Samples with at least one record in it, over samples.
    #[serde(serialize_with = "finite_or_null")]
    pub hit_rate: f64,
    /// Its brightest record's luminance.
    #[serde(serialize_with = "finite_or_null")]
    pub max_luma: f32,
    pub records: u64,
    /// The ids of its records among `records`.
    pub top_records: Vec<usize>,
}

/// One light path: one emission-ending term of one camera sample.
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub id: usize,
    #[serde(serialize_with = "finite_or_null")]
    pub luma: f32,
    /// RGB, before and after the indirect clamp, in the beauty's units.
    #[serde(serialize_with = "finite_or_null_rgb")]
    pub contribution: [f32; 3],
    #[serde(serialize_with = "finite_or_null_rgb")]
    pub contribution_clamped: [f32; 3],
    /// In crust's LPE grammar, the light by its tag: authored as an `lpe`
    /// RenderVar's `sourceName`, it routes this path.
    pub expression: String,
    /// Per scattering vertex, in path order, each event's share of the
    /// vertex's luminance toward the path's direction.
    pub lobe_split: Vec<Vec<LobeShare>>,
    /// A vertex's named event carries less than 0.6 of it.
    pub label_ambiguous: bool,
    pub light: Option<String>,
    /// Each vertex's instance prim path: `null` until prims are resolvable.
    pub prims: Option<Vec<String>>,
    /// Image space, as `--region` counts.
    pub pixel: [usize; 2],
    /// Where the sample was taken: pixel `(x, y)` covers `[x, x+1] × [y, y+1]`.
    pub coordinates: [f32; 2],
    pub sample: u32,
    /// Its scattering vertices.
    pub depth: usize,
    pub camera: Option<String>,
    /// Its group's index in `groups`.
    pub group: Option<usize>,
}

/// One event of a vertex's lobe split.
#[derive(Debug, Clone, Serialize)]
pub struct LobeShare {
    pub event: String,
    #[serde(serialize_with = "finite_or_null")]
    pub share: f32,
}

/// The records kept, of all.
#[derive(Debug, Clone, Serialize)]
pub struct Truncated {
    pub kept: u64,
    pub of: u64,
}

/// One inspected pixel.
#[derive(Debug, Clone, Serialize)]
pub struct Pixel {
    pub pixel: [usize; 2],
    /// The beauty, as the render writes it.
    #[serde(serialize_with = "finite_or_null_rgb")]
    pub beauty: [f32; 3],
    #[serde(serialize_with = "finite_or_null")]
    pub luma: f32,
    pub samples: u32,
}

/// What to do next, as data: a code, the group it is about, the thresholds
/// that fired it.
#[derive(Debug, Clone, Serialize)]
pub struct Hint {
    pub code: &'static str,
    /// The group's index in `groups`; `null` for the region.
    pub group: Option<usize>,
    pub key: Option<String>,
    pub expression: Option<String>,
    pub when: &'static str,
    pub next: &'static str,
}

fn finite_or_null_rgb<S: serde::Serializer>(v: &[f32; 3], s: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(3))?;
    for x in v {
        seq.serialize_element(&x.is_finite().then_some(*x))?;
    }
    seq.end()
}

/// `rare_bright_path`: hit rate below this…
pub const RARE_HIT_RATE: f64 = 1e-3;
/// …and second moment share above this.
pub const RARE_SECOND_MOMENT_SHARE: f64 = 0.5;
/// `single_path_dominates`: luminance share above this.
pub const DOMINANT_LUMA_SHARE: f64 = 0.8;
/// `clamp_hides_path`: clamped share under this fraction of the unclamped.
pub const CLAMP_HIDES_RATIO: f64 = 0.5;
/// `labels_ambiguous`: ambiguous luminance share above this.
pub const AMBIGUOUS_LUMA_SHARE: f64 = 0.2;

/// A hint's rule: when it fires, and what it suggests. Each names a next
/// step that shows the path, never a setting that changes the picture
/// without saying so.
pub struct HintRule {
    pub code: &'static str,
    pub when: &'static str,
    /// Over a group; `None` for a region-wide rule.
    pub holds: Option<fn(&Group) -> bool>,
    pub next: &'static str,
}

/// The hint rules, in the order the hints are listed. A group rule names
/// the first group (in report order) it holds for.
pub const HINTS: &[HintRule] = &[
    HintRule {
        code: "rare_bright_path",
        when: "hit_rate < 1e-3 and second_moment_share > 0.5",
        holds: Some(|g| {
            g.hit_rate < RARE_HIT_RATE && g.second_moment_share > RARE_SECOND_MOMENT_SHARE
        }),
        next: "author the expression as an lpe RenderVar to see where else it lands in the frame",
    },
    HintRule {
        code: "single_path_dominates",
        when: "luma_share > 0.8",
        holds: Some(|g| g.luma_share > DOMINANT_LUMA_SHARE),
        next: "the region's light is this one path: author it as an lpe RenderVar to see its \
               share across the frame",
    },
    HintRule {
        code: "clamp_hides_path",
        when: "luma_share_clamped < 0.5 * luma_share",
        holds: Some(|g| {
            g.luma_share > 0.0 && g.luma_share_clamped < CLAMP_HIDES_RATIO * g.luma_share
        }),
        next: "the indirect clamp removes most of this path's light: inspect with \
               --indirect-clamp 0 to see what it carries (the picture changes with the clamp)",
    },
    HintRule {
        code: "labels_ambiguous",
        when: "ambiguous_luma_share > 0.2",
        holds: None,
        next: "expressions name each vertex's dominant lobe: read lobe_split, or author one \
               expression per lobe as lpe RenderVars for the exact split",
    },
];

/// The hints `groups` and the region's ambiguous share fire.
pub(super) fn hints(groups: &[Group], ambiguous_luma_share: f64) -> Vec<Hint> {
    let mut out = Vec::new();
    for rule in HINTS {
        match rule.holds {
            Some(holds) => {
                if let Some((i, g)) = groups.iter().enumerate().find(|(_, g)| holds(g)) {
                    out.push(Hint {
                        code: rule.code,
                        group: Some(i),
                        key: Some(g.key.clone()),
                        expression: g.expression.clone(),
                        when: rule.when,
                        next: rule.next,
                    });
                }
            }
            None => {
                if ambiguous_luma_share > AMBIGUOUS_LUMA_SHARE {
                    out.push(Hint {
                        code: rule.code,
                        group: None,
                        key: None,
                        expression: None,
                        when: rule.when,
                        next: rule.next,
                    });
                }
            }
        }
    }
    out
}

/// A share as a percentage.
fn percent(x: f64) -> String {
    if !x.is_finite() {
        "n/a".into()
    } else if x >= 0.995 || x < 0.0005 {
        format!("{:.0}%", x * 100.0)
    } else if x < 0.01 {
        format!("{:.1}%", x * 100.0)
    } else {
        format!("{:.0}%", x * 100.0)
    }
}

/// The report's one sentence, from the first group or the brightest record.
pub(super) fn summary(
    groups: &[Group],
    records: &[Record],
    samples: u64,
    total_records: u64,
    not_reproduced: Option<&str>,
) -> String {
    let mut s = if let Some(g) = groups.first() {
        let what = match (&g.expression, &g.light) {
            (Some(e), Some(l)) => format!("{e} via {l}"),
            (Some(e), None) => e.clone(),
            (None, _) => g.key.clone(),
        };
        let hits = (g.hit_rate * samples as f64).round() as u64;
        format!(
            "The top group carries {} of the region's luminance and {} of its second moment: \
             {what}, in {hits} of {samples} samples ({total_records} records in {} groups).",
            percent(g.luma_share),
            percent(g.second_moment_share),
            groups.len()
        )
    } else if let Some(r) = records.first() {
        format!(
            "The brightest record is {}{}, luminance {}, at pixel ({}, {}) sample {} \
             ({total_records} records in {samples} samples).",
            r.expression,
            r.light
                .as_ref()
                .map_or_else(String::new, |l| format!(" via {l}")),
            r.luma,
            r.pixel[0],
            r.pixel[1],
            r.sample
        )
    } else if total_records > 0 {
        format!("{total_records} records in {samples} samples; none kept (--top 0).")
    } else {
        format!("No light path reached the region: {samples} samples, all black.")
    };
    if not_reproduced.is_some() {
        s.push_str(" These are not the frame's samples (see reproduces_frame_reason).");
    }
    s
}

/// Groups and records the Markdown lists at most.
const MARKDOWN_ROWS: usize = 10;

impl Report {
    /// A short Markdown summary rendered from this report.
    pub fn to_markdown(&self) -> String {
        let mut o = String::new();
        // `write!` into a `String` cannot fail.
        let _ = self.write_markdown(&mut o);
        o
    }

    fn write_markdown(&self, o: &mut String) -> std::fmt::Result {
        let [x0, y0, x1, y1] = self.region;
        writeln!(o, "# crust light-paths: region {x0},{y0},{x1},{y1}")?;
        writeln!(o)?;
        writeln!(o, "{}", self.summary)?;
        writeln!(o)?;
        writeln!(
            o,
            "- {} pixels, {} samples at {} spp; reproduces the frame: {}",
            self.pixels.len(),
            self.samples,
            self.settings.spp,
            match &self.reproduces_frame_reason {
                None => "yes".to_owned(),
                Some(r) => format!("no — {r}"),
            }
        )?;
        writeln!(
            o,
            "- records rebuild the pixels to {:.1e} (per sample {:.1e})",
            self.consistency.max_relative_error, self.consistency.max_sample_relative_error
        )?;
        writeln!(
            o,
            "- the indirect clamp removed {} of the luminance; ambiguous labels carry {}",
            percent(self.clamp_removed_share),
            percent(self.ambiguous_luma_share)
        )?;
        if !self.groups.is_empty() {
            writeln!(o)?;
            writeln!(o, "## Groups by {}", self.group_by)?;
            writeln!(o)?;
            writeln!(
                o,
                "| # | key | luminance | clamped | 2nd moment | hit rate | max | records |"
            )?;
            writeln!(
                o,
                "|---|-----|-----------|---------|------------|----------|-----|---------|"
            )?;
            for (i, g) in self.groups.iter().take(MARKDOWN_ROWS).enumerate() {
                writeln!(
                    o,
                    "| {i} | `{}` | {} | {} | {} | {:.2e} | {:.3} | {} |",
                    g.key,
                    percent(g.luma_share),
                    percent(g.luma_share_clamped),
                    percent(g.second_moment_share),
                    g.hit_rate,
                    g.max_luma,
                    g.records
                )?;
            }
            if self.groups.len() > MARKDOWN_ROWS {
                writeln!(
                    o,
                    "\n…and {} more in the JSON.",
                    self.groups.len() - MARKDOWN_ROWS
                )?;
            }
        }
        if !self.records.is_empty() {
            writeln!(o)?;
            writeln!(
                o,
                "## Brightest records ({} kept of {})",
                self.truncated.kept, self.truncated.of
            )?;
            writeln!(o)?;
            writeln!(
                o,
                "| id | luminance | expression | light | pixel | sample | depth |"
            )?;
            writeln!(
                o,
                "|----|-----------|------------|-------|-------|--------|-------|"
            )?;
            for r in self.records.iter().take(MARKDOWN_ROWS) {
                writeln!(
                    o,
                    "| {} | {:.3} | `{}`{} | {} | {},{} | {} | {} |",
                    r.id,
                    r.luma,
                    r.expression,
                    if r.label_ambiguous {
                        " (ambiguous)"
                    } else {
                        ""
                    },
                    r.light.as_deref().unwrap_or("-"),
                    r.pixel[0],
                    r.pixel[1],
                    r.sample,
                    r.depth
                )?;
            }
        }
        if !self.hints.is_empty() {
            writeln!(o)?;
            writeln!(o, "## Hints")?;
            writeln!(o)?;
            for h in &self.hints {
                match h.group {
                    Some(g) => writeln!(o, "- `{}` (group {g}): {}", h.code, h.next)?,
                    None => writeln!(o, "- `{}`: {}", h.code, h.next)?,
                }
            }
        }
        if let Some(note) = &self.prims_note {
            writeln!(o)?;
            writeln!(o, "Note: {note}.")?;
        }
        Ok(())
    }
}
