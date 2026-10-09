//! `crust check`: import a stage as `crust render` would, render nothing,
//! and report what the render would use, what the import refused,
//! approximated or skipped, and which settings are worth changing — as text
//! on stdout, or as `crust-check/1` JSON.

use std::fmt::Write as _;
use std::process::ExitCode;

use clap::ValueEnum;
use crust_assets::FileAssets;
use crust_core::check::{CheckReport, ProductInfo};
use crust_core::diagnostic::checks::{self, Facts};
use crust_core::diagnostic::report::{Action, SceneInfo};
use crust_core::diagnostic::{SceneFlags, effective_settings};
use crust_core::{WarningCode, WarningKind};
use tracing::{error, info};

use super::{CheckArgs, apply_scene_overrides, is_stdout, load_scene, select_products, write_json};

/// `crust check`'s exit status when a denied warning was raised: the
/// diagnostic's "report written, condition not met".
const DENIED: u8 = 3;

/// A `--deny` entry: a warning kind, or every kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DenyKind {
    /// An invalid authored value was replaced by a fallback.
    Refused,
    /// A valid authored value is rendered differently from what it asks for.
    Approximated,
    /// Something authored contributes nothing.
    Skipped,
    /// Every kind.
    All,
}

impl DenyKind {
    fn matches(self, kind: WarningKind) -> bool {
        match self {
            DenyKind::Refused => kind == WarningKind::Refused,
            DenyKind::Approximated => kind == WarningKind::Approximated,
            DenyKind::Skipped => kind == WarningKind::Skipped,
            DenyKind::All => true,
        }
    }
}

/// The codes of the records whose kind `deny` names, in record order.
fn denied(warnings: &[crust_core::Warning], deny: &[DenyKind]) -> Vec<WarningCode> {
    warnings
        .iter()
        .filter(|w| deny.iter().any(|d| d.matches(w.kind)))
        .map(|w| w.code)
        .collect()
}

/// Runs `crust check`. 0 clean, 3 a denied warning was raised, 1 an error.
pub fn run(args: &CheckArgs) -> ExitCode {
    let Some(input) = &args.scene.input else {
        use clap::CommandFactory;
        super::Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "crust check needs a stage: -i <INPUT>",
            )
            .exit();
    };
    if let Some(ocio) = &crust_core::config().ocio
        && let Err(e) = crust_core::color::use_config(ocio)
    {
        error!("$OCIO: {e}");
        return ExitCode::FAILURE;
    }
    let assets = FileAssets::new().with_auto_tx(args.scene.auto_tx);
    let mut scene = match load_scene(&args.scene, None, &assets) {
        Ok(scene) => scene,
        Err(code) => return code,
    };
    let import_peak = crust_core::peak_memory_bytes();
    let mut settings = apply_scene_overrides(&args.scene, scene.settings);
    if let Some(region) = args.scene.region {
        settings = match settings.with_region(region) {
            Ok(s) => s,
            Err(e) => {
                error!("--region: {e}");
                return ExitCode::FAILURE;
            }
        };
    }
    scene.settings = settings;
    let (w, h) = settings.get_dimensions();

    // What the render would write, resolved as it resolves it.
    let mut aovs = std::mem::take(&mut scene.aovs);
    select_products(&mut aovs, None);
    let products = if aovs.products.is_empty() {
        vec![ProductInfo {
            prim: None,
            file: super::beauty_output(None).to_owned(),
            channels: ["R", "G", "B"].map(String::from).to_vec(),
        }]
    } else {
        aovs.products
            .iter()
            .map(|p| ProductInfo {
                prim: Some(p.prim_path.clone()),
                file: p.name.clone(),
                channels: crate::products::product_channels(p)
                    .into_iter()
                    .flat_map(|(_, names)| names)
                    .collect(),
            })
            .collect()
    };

    let flags = SceneFlags {
        subdivision_level: args.scene.subdiv_level,
        subdivision_edge_length: args.scene.subdiv_edge_length,
        auto_tx: args.scene.auto_tx,
    };
    let preloaded = assets.texture_cache_stats().preloaded;
    let facts = Facts::from_import(&scene, args.scene.auto_tx, preloaded, import_peak);
    let warnings = std::mem::take(&mut scene.warnings);
    let report = CheckReport {
        scene: SceneInfo {
            path: input.clone(),
            frame: args.scene.frame,
            camera: scene.camera_path.clone(),
            resolution: [w, h],
            region: (!settings.is_full_frame()).then(|| {
                let r = settings.region();
                [r.x0, r.y0, r.x1, r.y1]
            }),
        },
        products,
        effective_settings: effective_settings(&settings, &flags),
        import: scene.stats.phases.clone(),
        counts: scene.stats.scene,
        findings: checks::run(&facts),
        denied: denied(&warnings, &args.deny),
        warnings,
    };
    assets.release_texture_files();

    // The reports first, the status after: a report that cannot be written
    // is an error even when a warning was denied.
    match args.json.as_deref() {
        Some(path) if is_stdout(path) => {
            if let Err(e) = write_json(path, &report.to_json()) {
                error!("--json -: {e}");
                return ExitCode::FAILURE;
            }
        }
        json => {
            print!("{}", to_text(&report));
            if let Some(path) = json {
                if let Err(e) = write_json(path, &report.to_json()) {
                    error!("--json {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
                info!("Report written to {}", path.display());
            }
        }
    }
    if report.denied.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(DENIED)
    }
}

/// The text report, in the spec's order: the render described, effective
/// settings, import costs and counts, findings, warnings, then one closing
/// line of totals.
pub fn to_text(r: &CheckReport) -> String {
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
        let more = if w.count > w.prims.len() as u64 && !w.prims.is_empty() {
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
