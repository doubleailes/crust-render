//! The `crust-check/1` report: what `crust check` says about a stage after
//! importing it as a render would, without rendering it.
//!
//! No new vocabulary: each section is a shape another report already writes
//! — `scene` and `effective_settings` are `crust-diagnostic/1`'s, `import`
//! and `counts` are `crust-stats/1`'s `phases` and `scene`, `findings` are
//! the diagnostic's, `warnings` the import's [`Warning`] records. The host
//! (the CLI) resolves the products and decides `denied`; the engine writes
//! nothing.

use serde::Serialize;

use crate::diagnostic::report::{Finding, SceneInfo, Setting};
use crate::{Phase, SceneCounters, Warning, WarningCode};

/// The `crust-check/1` report's `format`.
pub const FORMAT: &str = "crust-check/1";

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

/// The `crust-check/1` report, keys in the order the `scene-check` spec
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
    /// The report as `crust-check/1` JSON.
    pub fn to_json(&self) -> String {
        crate::report::Report::new(FORMAT, self).to_json()
    }
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

    /// The `crust-check/1` contract: the top-level keys in the spec's order,
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
        assert_eq!(value["format"], "crust-check/1");
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
}
