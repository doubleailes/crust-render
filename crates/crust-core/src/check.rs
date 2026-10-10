//! The `crust-check/2` report: what `crust check` says about a stage after
//! importing it as a render would, without rendering it.
//!
//! No new vocabulary: each section is a shape another report already writes
//! — `scene` and `effective_settings` are `crust-diagnostic/1`'s, `import`
//! and `counts` are `crust-stats/1`'s `phases` and `scene`, `findings` are
//! the diagnostic's, `warnings` the import's [`Warning`] records. The host
//! (the CLI) resolves the products and decides `denied`; the engine writes
//! nothing.

use std::fmt::Write as _;

use serde::Serialize;

use crate::diagnostic::report::{Action, Finding, SceneInfo, Setting};
use crate::{Phase, SceneCounters, Warning, WarningCode, WarningKind};

/// The `crust-check/2` report's `format`.
pub const FORMAT: &str = "crust-check/2";

/// One file a render would write.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProductInfo {
    /// The `RenderProduct` prim; `null` for the default beauty image a stage
    /// without products renders to.
    pub prim: Option<String>,
    /// The path the render writes, as it would resolve it.
    pub file: String,
    /// The channels in the file, in the order their vars are listed.
    pub channels: Vec<String>,
}

/// The `crust-check/2` report, keys in the order the `scene-check` spec
/// fixes.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub scene: SceneInfo,
    pub products: Vec<ProductInfo>,
    pub effective_settings: Vec<Setting>,
    /// The import's phases, as `crust-stats/1` writes them.
    pub import: Vec<Phase>,
    /// The scene counts, as `crust-stats/1`'s `scene`.
    pub counts: SceneCounters,
    pub findings: Vec<Finding>,
    pub warnings: Vec<Warning>,
    /// The codes of the warnings a `--deny` matched, in record order.
    pub denied: Vec<WarningCode>,
}

impl CheckReport {
    /// The report as `crust-check/2` JSON.
    pub fn to_json(&self) -> String {
        crate::report::Report::new(FORMAT, self).to_json()
    }

    /// The text report, in the spec's order: the render described, effective
    /// settings, import costs and counts, findings, warnings, then one
    /// closing line of totals.
    pub fn to_text(&self) -> String {
        to_text(self)
    }
}

/// The codes of the records whose kind is in `kinds`, in record order — what
/// `--deny` matched.
pub fn denied(warnings: &[Warning], kinds: &[WarningKind]) -> Vec<WarningCode> {
    warnings
        .iter()
        .filter(|w| kinds.contains(&w.kind))
        .map(|w| w.code)
        .collect()
}

fn to_text(r: &CheckReport) -> String {
    let mut o = String::new();
    let s = &r.scene;
    let _ = writeln!(o, "crust check: {}", s.path);
    let _ = writeln!(o);
    let _ = writeln!(o, "Render");
    let frame = s
        .frame
        .map_or_else(|| "default values".into(), |f| f.to_string());
    let _ = writeln!(o, "  frame       {frame}");
    let camera = s.camera.as_deref().unwrap_or("procedural default");
    let _ = writeln!(o, "  camera      {camera}");
    let [w, h] = s.resolution;
    let _ = writeln!(o, "  resolution  {w}x{h}");
    let region = s.region.map_or_else(
        || "full frame".into(),
        |[x0, y0, x1, y1]| format!("{x0},{y0},{x1},{y1}"),
    );
    let _ = writeln!(o, "  region      {region}");
    for p in &r.products {
        let from = p
            .prim
            .as_deref()
            .map_or(String::new(), |p| format!(" ({p})"));
        let _ = writeln!(
            o,
            "  writes      {}{from}: {}",
            p.file,
            p.channels.join(" ")
        );
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "Effective settings");
    let width = r
        .effective_settings
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(0);
    for e in &r.effective_settings {
        let by: Vec<&str> = [e.flag.as_deref(), e.usd_attribute.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let _ = writeln!(o, "  {:width$}  {}  ({})", e.name, e.value, by.join(", "));
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "Import");
    for p in &r.import {
        let indent = "  ".repeat(1 + p.depth as usize);
        let peak = p
            .peak_end
            .map_or_else(String::new, |b| format!(", peak {}", mib(b)));
        let _ = writeln!(
            o,
            "{indent}{}: {:.2} s{peak}",
            p.name,
            p.duration.as_secs_f64()
        );
    }
    let c = &r.counts;
    let _ = writeln!(
        o,
        "  {} geometries, {} light(s), {} volume region(s)",
        c.geometries, c.lights, c.volumes
    );
    let _ = writeln!(o);

    let _ = writeln!(o, "Findings");
    if r.findings.is_empty() {
        let _ = writeln!(o, "  no findings");
    }
    for f in &r.findings {
        let action = match &f.action {
            Action::Set {
                flag,
                usd_attribute,
                value,
            } => {
                let by: Vec<&str> = [flag.as_deref(), usd_attribute.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect();
                format!("set {} = {value}", by.join(" / "))
            }
            Action::None { none } => format!("none — {none}"),
        };
        let _ = writeln!(
            o,
            "  {} ({}): {}. Action: {action}.",
            f.id,
            f.kind.name(),
            f.summary
        );
    }
    let _ = writeln!(o);

    let _ = writeln!(o, "Warnings");
    if r.warnings.is_empty() {
        let _ = writeln!(o, "  no warnings");
    }
    for w in &r.warnings {
        // `prims` keeps distinct prims, so a count above its length can be
        // repeats on the listed ones: only a full list may have turned one
        // away.
        let more = if w.prims.len() == crate::warnings::MAX_PRIMS {
            ", …"
        } else {
            ""
        };
        let prims = if w.prims.is_empty() {
            String::new()
        } else {
            format!(": {}{more}", w.prims.join(", "))
        };
        let _ = writeln!(o, "  {} {} ×{}{prims}", w.kind, w.code, w.count);
    }
    if !r.denied.is_empty() {
        let codes: Vec<&str> = r.denied.iter().map(|c| c.as_str()).collect();
        let _ = writeln!(o, "  denied: {}", codes.join(", "));
    }
    let _ = writeln!(o);
    let _ = writeln!(o, "{}", totals(r));
    o
}

/// The closing line: the findings, and the warnings by kind.
fn totals(r: &CheckReport) -> String {
    let findings = match r.findings.len() {
        0 => "no findings".to_owned(),
        1 => "1 finding".to_owned(),
        n => format!("{n} findings"),
    };
    if r.warnings.is_empty() {
        return format!("{findings} and no warnings");
    }
    let by_kind: Vec<String> = WarningKind::ALL
        .iter()
        .map(|&k| {
            let n: u64 = r
                .warnings
                .iter()
                .filter(|w| w.kind == k)
                .map(|w| w.count)
                .sum();
            format!("{n} {k}")
        })
        .collect();
    let total: u64 = r.warnings.iter().map(|w| w.count).sum();
    let warnings = if total == 1 { "warning" } else { "warnings" };
    format!("{findings}; {total} {warnings} ({})", by_kind.join(", "))
}

fn mib(bytes: u64) -> String {
    format!("{:.0} MiB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::report::{Action, Evidence, FindingKind};
    use crate::{MemorySample, RenderStats, WarningKind};
    use std::time::Duration;

    fn report() -> CheckReport {
        let mut stats = RenderStats::new();
        stats.record_at(
            "Parse USD stage",
            0,
            Duration::from_millis(250),
            MemorySample::default(),
        );
        CheckReport {
            scene: SceneInfo {
                path: "scene.usda".into(),
                frame: None,
                camera: Some("/cam".into()),
                resolution: [64, 32],
                region: None,
            },
            products: vec![ProductInfo {
                prim: None,
                file: "output.exr".into(),
                channels: vec!["R".into(), "G".into(), "B".into()],
            }],
            effective_settings: vec![Setting {
                name: "light_selection".into(),
                value: "power".into(),
                flag: Some("--light-selection".into()),
                usd_attribute: Some("crust:lightSelection".into()),
            }],
            import: stats.phases.clone(),
            counts: SceneCounters::default(),
            findings: vec![Finding {
                id: "textures_without_tx".into(),
                kind: FindingKind::Time,
                summary: "s".into(),
                evidence: Evidence::new().with("textures", f64::NAN),
                action: Action::None { none: "n".into() },
            }],
            warnings: vec![Warning {
                code: WarningCode::LightDegenerateShape,
                kind: WarningKind::Skipped,
                count: 2,
                prims: vec!["/a".into(), "/b".into()],
                message: "m".into(),
            }],
            denied: vec![WarningCode::LightDegenerateShape],
        }
    }

    /// Every key path of a JSON value, `a.b`, an array's elements as `a[]`.
    fn key_paths(v: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, v) in m {
                    let p = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    out.push(p.clone());
                    key_paths(v, &p, out);
                }
            }
            serde_json::Value::Array(a) => {
                for v in a.iter().take(1) {
                    key_paths(v, &format!("{prefix}[]"), out);
                }
            }
            _ => {}
        }
    }

    /// The `crust-check/2` contract: the top-level keys in the spec's order,
    /// and the key paths of each section (`openspec/specs/cli/design.md`
    /// § Machine-readable reports lists the same).
    #[test]
    fn check_json_keys_are_pinned() {
        let json = report().to_json();
        let top: Vec<&str> = json
            .lines()
            .filter(|l| l.starts_with("  \"") && !l.starts_with("   "))
            .filter_map(|l| l.trim().strip_prefix('"')?.split('"').next())
            .collect();
        assert_eq!(
            top,
            [
                "format",
                "crust_version",
                "scene",
                "products",
                "effective_settings",
                "import",
                "counts",
                "findings",
                "warnings",
                "denied"
            ]
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["format"], "crust-check/2");
        let mut paths = Vec::new();
        key_paths(&value, "", &mut paths);
        for want in [
            "scene.path",
            "scene.frame",
            "scene.camera",
            "scene.resolution",
            "scene.region",
            "products[].prim",
            "products[].file",
            "products[].channels",
            "effective_settings[].name",
            "effective_settings[].value",
            "effective_settings[].flag",
            "effective_settings[].usd_attribute",
            "import[].name",
            "counts.geometries",
            "counts.lights",
            "counts.volumes",
            "findings[].id",
            "findings[].kind",
            "findings[].summary",
            "findings[].evidence",
            "findings[].action",
            "warnings[].code",
            "warnings[].kind",
            "warnings[].count",
            "warnings[].prims",
            "warnings[].message",
        ] {
            assert!(paths.iter().any(|p| p == want), "no {want} in {paths:#?}");
        }
        assert_eq!(
            value["denied"],
            serde_json::json!(["light.degenerate_shape"])
        );
        assert_eq!(value["warnings"][0]["kind"], "skipped");
    }

    /// A number that is not finite, or not measured, is `null`.
    #[test]
    fn unavailable_numbers_are_null() {
        let value: serde_json::Value = serde_json::from_str(&report().to_json()).unwrap();
        assert!(value["findings"][0]["evidence"]["textures"].is_null());
        assert!(value["scene"]["region"].is_null());
        assert!(value["scene"]["frame"].is_null());
    }

    /// `…` follows a warning's prims only when the list is full: a count
    /// above its length can be repeats on the prims listed.
    #[test]
    fn an_ellipsis_only_after_a_full_prim_list() {
        let line = |prims: Vec<String>| {
            let mut r = report();
            r.warnings[0].count = 40;
            r.warnings[0].prims = prims;
            let text = r.to_text();
            text.lines()
                .find(|l| l.contains(r.warnings[0].code.as_str()))
                .expect("the warning's line")
                .to_owned()
        };
        let complete = line(vec!["/a".into(), "/b".into()]);
        assert!(complete.ends_with(": /a, /b"), "{complete}");
        let full = line(
            (0..crate::warnings::MAX_PRIMS)
                .map(|i| format!("/p{i}"))
                .collect(),
        );
        assert!(full.ends_with(", …"), "{full}");
    }
}
