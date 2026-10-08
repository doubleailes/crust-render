//! The report, `crust-diagnostic/1`: what [`super::run`] returns, the JSON
//! a machine reader parses, and what the Markdown is rendered from.
//!
//! Field order is key order: serde writes a struct's fields as declared, so
//! each struct below *is* its JSON object's layout, and the test in
//! `diagnostic/tests.rs` pins the top level. Keys are snake_case with their
//! units in the name (`time_s`, `mem_bytes`); every measured float is a
//! [`Num`], written with four significant digits. Arrays are in a fixed
//! order, so two runs differ only in times and the values derived from them.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The `format` of every report this version writes.
pub const FORMAT: &str = "crust-diagnostic/1";

/// The whole report, top-level keys in the order the spec fixes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub format: String,
    pub crust_version: String,
    pub scene: SceneInfo,
    pub effective_settings: Vec<Setting>,
    pub run: RunInfo,
    pub static_findings: Vec<Finding>,
    pub baseline: Baseline,
    pub noise_breakdown: NoiseBreakdown,
    pub crops: Vec<Crop>,
    pub trials: Vec<Trial>,
    pub sample_budget: Option<SampleBudget>,
    pub picture_changing: PictureChanging,
    pub not_tried: Vec<NotTried>,
    pub suggestions: Vec<Suggestion>,
    pub converged: bool,
    pub suggested_command: String,
    /// Only with `--baseline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deltas: Option<Deltas>,
}

impl Report {
    /// The JSON report, pretty-printed, with a final newline.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("a report always serializes");
        s.push('\n');
        s
    }
}

/// A measured number, written with four significant digits, and as `null`
/// when it is not finite (JSON has no NaN or infinity).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Num(pub f64);

impl Num {
    pub fn get(self) -> f64 {
        self.0
    }
}

impl From<f64> for Num {
    fn from(x: f64) -> Self {
        Num(x)
    }
}

/// `x` rounded to four significant digits. Scaling by an exact power of ten
/// and dividing back gives the double nearest the rounded decimal, which is
/// what `serde_json` then prints shortest.
pub fn sig4(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let mag = x.abs().log10().floor() as i32;
    if mag >= 3 {
        // An integer part of four digits or more: round to a multiple of
        // 10^(mag-3), exact while that is below 1e22.
        let unit = 10f64.powi(mag - 3);
        (x / unit).round() * unit
    } else {
        let scale = 10f64.powi(3 - mag);
        (x * scale).round() / scale
    }
}

impl Serialize for Num {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.0.is_finite() {
            s.serialize_f64(sig4(self.0))
        } else {
            s.serialize_none()
        }
    }
}

impl<'de> Deserialize<'de> for Num {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Num(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN)))
    }
}

/// Named numbers in a fixed order, written as one JSON object.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Evidence(pub Vec<(String, Num)>);

impl Evidence {
    pub fn new() -> Self {
        Evidence(Vec::new())
    }

    pub fn with(mut self, name: &str, value: f64) -> Self {
        self.0.push((name.to_owned(), Num(value)));
        self
    }

    pub fn get(&self, name: &str) -> Option<f64> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| v.0)
    }
}

impl Serialize for Evidence {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Evidence {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = Evidence;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object of numbers")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Evidence, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = a.next_entry::<String, Num>()? {
                    out.push((k, v));
                }
                Ok(Evidence(out))
            }
        }
        d.deserialize_map(Visit)
    }
}

/// What was diagnosed. Two reports are comparable only when this matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneInfo {
    pub path: String,
    /// `-f`, when given.
    pub frame: Option<f64>,
    /// `--camera`, when given; otherwise the stage's choice.
    pub camera: Option<String>,
    /// `[width, height]` in pixels.
    pub resolution: [usize; 2],
    /// `--region` (or the stage's data window) as `[x0, y0, x1, y1]`, image
    /// space, half-open; `null` for the whole frame.
    pub region: Option<[usize; 4]>,
}

/// One setting the diagnosis ran with, by the names that change it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    pub name: String,
    pub value: String,
    /// The `crust render` flag that sets it, if there is one.
    pub flag: Option<String>,
    /// The `crust:*` render-settings attribute that authors it, if any.
    pub usd_attribute: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunInfo {
    pub budget_s: Num,
    /// Wall-clock seconds after import.
    pub used_s: Num,
    /// Not counted against the budget.
    pub import_s: Num,
    pub threads: usize,
    pub repeats: u32,
    /// The conditions every efficiency comparison is rendered under.
    pub probe_conditions: ProbeConditions,
    pub phases: Vec<PhaseRun>,
    /// The phase the budget ran out in, if it did.
    pub budget_exceeded_in: Option<String>,
    /// The process's exit status: 0 when tier 1 completed, 3 when the
    /// budget ran out first.
    pub exit: i32,
    /// The sampler seed (`crust:frame`) of each pair: pair 0 renders with
    /// the scene's own, so every run renders the same images.
    #[serde(default)]
    pub seeds: Vec<i64>,
    /// The seconds held back from tier 1 for tiers 2 and 3: their estimated
    /// render cost.
    #[serde(default)]
    pub tier2_reserve_s: Num,
    #[serde(default)]
    pub tier3_reserve_s: Num,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeConditions {
    /// Always `off`: the clamp is biased, and is measured under
    /// `picture_changing` instead.
    pub indirect_clamp: String,
    /// Always `off`, except where a phase says what it changed.
    pub adaptive_sampling: String,
    pub fixed_spp: bool,
    /// The scene's own `[width, height]`: a crop is a sub-image of the
    /// frame, bit for bit.
    pub resolution: [usize; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseRun {
    pub name: String,
    pub time_s: Num,
    pub completed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Time,
    Noise,
    Memory,
    Correctness,
}

impl FindingKind {
    pub fn name(self) -> &'static str {
        match self {
            FindingKind::Time => "time",
            FindingKind::Noise => "noise",
            FindingKind::Memory => "memory",
            FindingKind::Correctness => "correctness",
        }
    }
}

/// What to do about a finding: a setting crust has, or nothing, with why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Action {
    Set {
        flag: Option<String>,
        usd_attribute: Option<String>,
        value: String,
    },
    None {
        none: String,
    },
}

impl Action {
    pub fn is_actionable(&self) -> bool {
        matches!(self, Action::Set { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub kind: FindingKind,
    pub summary: String,
    pub evidence: Evidence,
    pub action: Action,
}

/// The full-frame baseline (P1).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Baseline {
    /// The 1 spp calibration render the baseline's sample count came from.
    pub calibration_time_s: Num,
    pub spp: u32,
    /// `setup_s + render_s`.
    pub time_s: Num,
    /// The `learned` pre-pass and guiding's training.
    pub setup_s: Num,
    pub render_s: Num,
    /// Mean over pixels of variance over squared luminance, against the
    /// baseline's own image.
    pub mrse: Num,
    pub rays_per_s: Num,
    pub mean_path_length: Num,
    pub rr_kill_rate: Num,
    pub ended_by_depth_share: Num,
    pub shadow_rays_per_vertex: Num,
    /// The largest profile sections, by share of thread time.
    pub profile_top: Vec<ProfileShare>,
    /// `null` when the render looked nothing up.
    pub texture_hit_rate: Option<Num>,
    pub ptex_hit_rate: Option<Num>,
    pub peak_mem_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileShare {
    pub section: String,
    pub share: Num,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoiseBreakdown {
    /// By light transport. Each row's error is its own; rows are never
    /// shares of the beauty's variance.
    pub components: Vec<NoiseRow>,
    /// By light group.
    pub light_groups: Vec<NoiseRow>,
    /// `lpe_tag` (authored `crust:light:lpeTag`), `light` (one per light,
    /// labelled by the diagnostic) or `none`.
    pub light_groups_by: String,
    /// The component with the largest relative error against the beauty.
    pub dominant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseRow {
    pub key: String,
    pub expression: String,
    pub mean_luminance: Num,
    /// Mean over pixels of `var / max(mean², ε)` — the component's own.
    pub relative_error: Num,
    /// Mean over pixels of `var / max(beauty², ε)`.
    pub relative_error_vs_beauty: Num,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub id: String,
    /// `[x0, y0, x1, y1]`, image space (top-left origin), half-open.
    pub rect: [usize; 4],
    /// `highest_relative_variance`, `highest_time`, `median` or `region`.
    pub reason: String,
    /// Mean relative variance over the crop in the baseline.
    pub relative_variance: Num,
    /// Thread-seconds the baseline spent in the crop's pixels: each tile's
    /// time on its worker, summed — a share of the work, not wall-clock.
    pub baseline_thread_s: Num,
    /// The estimated MRSE of the crop's reference image itself.
    pub reference_mrse: Option<Num>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Better,
    Worse,
    Inconclusive,
    Mixed,
    /// The trial changes the picture: never a gain, whatever its ΔEff.
    Biased,
    /// The probe's own error moves more between seeds than the smallest
    /// gain worth suggesting: a larger budget may decide it.
    InsufficientSamples,
}

impl Verdict {
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Better => "better",
            Verdict::Worse => "worse",
            Verdict::Inconclusive => "inconclusive",
            Verdict::Mixed => "mixed",
            Verdict::Biased => "biased",
            Verdict::InsufficientSamples => "insufficient_samples",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trial {
    /// `factor=value`, or `combined`.
    pub id: String,
    pub tier: u8,
    pub factor: String,
    pub value: String,
    pub flag: Option<String>,
    pub usd_attribute: Option<String>,
    pub per_crop: Vec<CropTrial>,
    /// Geometric mean of the per-crop medians.
    pub overall_delta_eff: Option<Num>,
    pub verdict: Verdict,
    /// The baseline's projected full-frame time to reach the target over
    /// the trial's, each side's setup included at full-frame scale: an
    /// estimate, which can only veto a suggestion.
    #[serde(default)]
    pub delta_eff_at_target: Option<Num>,
}

/// One trial on one crop. Values are medians over the pairs unless they are
/// lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CropTrial {
    pub crop: String,
    pub spp: u32,
    /// One per interleaved pair: `(render_B · MRSE_B) / (render_T ·
    /// MRSE_T)`, trimmed.
    pub delta_eff: Vec<Num>,
    pub median: Num,
    pub min: Num,
    pub max: Num,
    /// Trimmed: without the top 0.1% of either side's pixels.
    pub mrse_baseline: Num,
    pub mrse_trial: Num,
    /// Render time, setup excluded.
    #[serde(alias = "time_baseline_s")]
    pub render_baseline_s: Num,
    #[serde(alias = "time_trial_s")]
    pub render_trial_s: Num,
    /// The `learned` pre-pass and guiding's training, per render.
    pub setup_trial_s: Num,
    pub verdict: Verdict,
    /// Mean luminance of both sides over the pixels the picture check
    /// keeps (all but the 1% whose two values differ most).
    #[serde(default)]
    pub mean_luminance_baseline: Num,
    #[serde(default)]
    pub mean_luminance_trial: Num,
    /// `mean_luminance_trial / mean_luminance_baseline − 1`.
    #[serde(default)]
    pub luminance_shift: Num,
    /// The shift over its standard error.
    #[serde(default)]
    pub luminance_shift_z: Num,
    #[serde(default)]
    pub mrse_baseline_untrimmed: Num,
    #[serde(default)]
    pub mrse_trial_untrimmed: Num,
    /// One per pair, from the untrimmed MRSEs.
    #[serde(default)]
    pub delta_eff_untrimmed: Vec<Num>,
    /// The largest over the smallest of the baseline's MRSEs across seeds;
    /// `null` with one repeat.
    #[serde(default)]
    pub noise_floor: Option<Num>,
}

/// Tier 2. Projections are estimates and never produce suggestions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleBudget {
    pub target_mrse: Num,
    /// `variance_threshold` (its square) or `--target-mrse`.
    pub target_from: String,
    /// The tier-1 settings the projection assumes: a trial id, or
    /// `baseline`.
    pub settings: String,
    pub estimate: bool,
    pub spp_to_target: Option<Num>,
    /// Full frame, sampling only (setup excluded).
    pub projected_render_s: Option<Num>,
    pub adaptive: Vec<AdaptiveCrop>,
    /// The settings' setup at full-frame scale, apart from the render.
    #[serde(default)]
    pub projected_setup_s: Option<Num>,
}

/// One crop rendered with adaptive sampling on at the authored threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveCrop {
    pub crop: String,
    pub variance_threshold: Num,
    pub spp: u32,
    pub mean_spp: Num,
    pub early_stopped_share: Num,
    pub time_adaptive_s: Num,
    /// The same crop at `spp` fixed, from its measured per-sample time.
    pub time_fixed_estimate_s: Num,
    pub time_saved_share: Num,
}

/// Tier 3: settings that change the picture. Measured, never ranked.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PictureChanging {
    /// `null` when the clamp is off.
    pub clamp: Option<ClampResult>,
    pub max_depth: DepthResult,
    pub subdivision: SubdivisionResult,
    /// Per crop; `null` when it does not apply (no light-list entry, or a
    /// single-strategy baseline) or was not measured.
    #[serde(default)]
    pub light_sampling_reach: Option<Vec<Reach>>,
}

/// The light-sampling reach of one crop: a light-only render's mean
/// luminance over the baseline's, same seed and samples, every pixel.
/// Below 1, part of the energy arrives only on paths BSDF sampling finds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reach {
    pub crop: String,
    pub reach: Num,
    /// `(reach − 1)` over its standard error.
    pub z: Num,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClampResult {
    pub limit: Num,
    /// Of the baseline's mean luminance.
    pub removed_luminance_share: Num,
    pub mean_removed_luminance: Num,
    pub pixels_affected_share: Num,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DepthResult {
    pub max_depth: u32,
    /// Paths ended by `max_depth`, of every camera path, in the baseline.
    pub ended_by_depth_share: Num,
    pub half_depth: Option<HalfDepth>,
}

/// One crop at half the depth: adaptive off, clamp off, as the baseline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HalfDepth {
    pub crop: String,
    pub depth: u32,
    pub time_saved_share: Num,
    /// `(mean luminance at half depth / at full depth) − 1`.
    pub mean_luminance_change: Num,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubdivisionResult {
    /// Faces refined per level, level 0 first.
    pub levels: Vec<u64>,
    pub triangles: u64,
    pub mem_bytes: u64,
    pub build_s: Option<Num>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotTried {
    pub id: String,
    pub tier: u8,
    /// `budget`, or `not_applicable`.
    pub reason: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: String,
    pub flag: Option<String>,
    pub usd_attribute: Option<String>,
    pub value: String,
    pub expected_delta_eff: Num,
    /// Trial and finding ids.
    pub evidence: Vec<String>,
    #[serde(default)]
    pub expected_delta_eff_at_target: Num,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Deltas {
    pub comparable: bool,
    /// Why not, when not.
    pub note: Option<String>,
    /// Indicative only: two runs measured minutes apart, under whatever
    /// load each met. The evidence for a gain is a run's own interleaved
    /// trials, never this ratio.
    pub baseline_time_s: Option<Change>,
    /// The baselines' sample counts, which each run's calibration picks:
    /// MRSE scales as 1/spp, so read `baseline_mrse` against this.
    pub baseline_spp: Option<Change>,
    pub baseline_mrse: Option<Change>,
    pub settings_changed: Vec<SettingChange>,
    pub findings_resolved: Vec<String>,
    pub findings_new: Vec<String>,
    pub suggestions_gone: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub from: Num,
    pub to: Num,
    /// `to / from`.
    pub ratio: Num,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingChange {
    pub name: String,
    pub from: String,
    pub to: String,
}
