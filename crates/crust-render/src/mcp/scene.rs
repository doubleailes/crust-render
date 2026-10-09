//! The session's scene: the saved override layer imported exactly as `crust
//! render <output>` imports it (design D1), kept as a renderer that every
//! session render retunes, beside the `crust-check/1` report of that
//! same import.

use crate::{CheckArgs, Checked, Cli, Command};
use clap::Parser;
use crust_assets::FileAssets;
use crust_core::check::CheckReport;
use crust_core::{AovRequest, RenderSettings, Renderer};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::debug;

/// One import of the override layer.
pub struct Imported {
    /// The scene, ready to render. Shared with a running render, so it is
    /// retuned only once that render has been joined.
    pub renderer: Arc<Renderer>,
    /// The scene's own render settings, which a session render overrides
    /// with its region and samples only.
    pub settings: RenderSettings,
    /// The products a render of the layer writes, as `crust render` selects
    /// them: a session render requests the same AOVs, so its beauty is the
    /// CLI's.
    pub aovs: Arc<AovRequest>,
    /// What `crust check -i <output> --json -` reports on this import.
    pub report: CheckReport,
    /// The loader the scene was imported with: held, never read, because it
    /// owns the texture caches the scene's textures stream through.
    pub _assets: Arc<FileAssets>,
    /// The scene-linear space the scene renders in, which its images are
    /// encoded from.
    pub working_space: crust_core::color::Space,
    /// How long the import took.
    pub took: Duration,
}

impl Imported {
    /// The warning codes the import raised, in report order.
    pub fn warning_codes(&self) -> Vec<&'static str> {
        self.report
            .warnings
            .iter()
            .map(|w| w.code.as_str())
            .collect()
    }
}

/// The arguments `crust <args>` would parse to for `check`: the session
/// imports with the CLI's own defaults, by construction.
fn check_args(layer: &Path) -> Result<CheckArgs, String> {
    let layer = layer.to_str().ok_or("the layer path is not UTF-8")?;
    match Cli::try_parse_from(["crust", "check", "-i", layer]) {
        Ok(Cli {
            command: Command::Check(args),
            ..
        }) => Ok(*args),
        Ok(_) => unreachable!("parsed as check"),
        Err(e) => Err(e.to_string()),
    }
}

/// Imports the saved layer at `layer`.
pub fn import(layer: &Path) -> Result<Imported, String> {
    let args = check_args(layer)?;
    let started = Instant::now();
    let Checked {
        scene,
        aovs,
        report,
        assets,
    } = crate::import_checked(&args)?;
    let settings = scene.settings;
    let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings)
        .with_volumes(scene.volumes);
    let took = started.elapsed();
    // One per edit batch: a count that grows with the session.
    debug!("Session import of {} in {took:.2?}", layer.display());
    Ok(Imported {
        renderer: Arc::new(renderer),
        settings,
        aovs: Arc::new(aovs),
        report,
        _assets: Arc::new(assets),
        working_space: scene.working_space,
        took,
    })
}
