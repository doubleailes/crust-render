//! A live session's core: an override layer followed on disk, imported as
//! `crust render` imports it, and each change to it classified and applied
//! in place or by a re-import. `crust mcp` authors that layer from an agent's
//! edits; the Hydra delegate (`crust-hydra`) writes it from the host's scene.
//! Both take the change through here, so an edit classifies the same in
//! either host and its scene is the one `crust render` imports.

pub mod delta;
pub mod layer;
pub mod live;
pub mod scene;
pub mod sources;

use crate::Assets;
use crust_assets::FileAssets;
use delta::{Applied, classify, layer_diff};
use openusd::usd::{InitialLoadSet, Stage};
use scene::{Imported, import};
use std::path::Path;
use std::sync::Arc;
use tracing::debug;

/// The loader for the session's next import or update: the session's own,
/// its decodes revalidated against the files on disk, or a fresh one under
/// `CRUST_SCENE_DELTAS=0`; and how many kept decodes changed on disk.
pub fn loader(kept: Option<&Arc<dyn Assets>>) -> (Arc<dyn Assets>, usize) {
    match kept {
        Some(assets) => {
            let dropped = assets.revalidate();
            if dropped > 0 {
                debug!("{dropped} decoded asset(s) changed on disk and will be read again");
            }
            (assets.clone(), dropped)
        }
        None => (Arc::new(FileAssets::new()) as Arc<dyn Assets>, 0),
    }
}

/// Imports the saved layer at `layer` through `assets`, rendering `pass`,
/// then lets go of the kept decodes the new scene does not use.
pub fn import_kept(
    layer: &Path,
    pass: Option<&str>,
    assets: Arc<dyn Assets>,
) -> Result<Imported, String> {
    let scene = import(layer, pass, assets.clone())?;
    let unused = assets.sweep();
    if unused > 0 {
        debug!("{unused} decoded asset(s) the scene no longer uses let go");
    }
    Ok(scene)
}

/// Opens `path`'s layer as the authoring stage, payloads unloaded: they load
/// only for the prims a query reaches.
pub fn open_stage(path: &Path) -> Result<Stage, String> {
    let id = path.to_str().ok_or("the layer path is not UTF-8")?;
    // The import's schema registry, not its resolver: a declared attribute
    // then has its schema type and fallback here, as in the render.
    Stage::builder()
        .schema_registry(crust_core::usd_schema_registry())
        .load(InitialLoadSet::LoadNone)
        .open(id)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))
}

/// Replaces the file at `path` with `bytes` by writing a sibling and renaming
/// it over `path`, as openusd's `Layer::save` does. A reader that still maps
/// the old file keeps the bytes it mapped: the rename gives `path` a new file,
/// where writing in place would change the mapped one under it.
pub fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let staged = path.with_file_name(format!(".{name}.crust-restore"));
    std::fs::write(&staged, bytes)?;
    std::fs::rename(&staged, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

/// The class of the change from `before` to the saved layer at `layer`, and
/// what it rebuilds, read against `stage` (the layer's stage, as saved) and
/// the scene's `provenance`; `None` when it needs a full import.
pub fn classify_saved(
    before: &[u8],
    layer: &Path,
    stage: &Stage,
    provenance: &crust_core::live::Provenance,
) -> Option<(Applied, crust_core::live::Change)> {
    let after = std::fs::read(layer).ok()?;
    let parse = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| openusd::usda::parse(text).ok())
    };
    let (before, after) = (parse(before)?, parse(&after)?);
    let changes = layer_diff(&before, &after);
    let (applied, change, full) = classify(&changes, stage, provenance);
    if let Some(why) = full {
        debug!("Edit applied as full: {why}");
        return None;
    }
    Some((applied, change))
}

/// Brings `scene` up to date with the saved layer at `layer`: in place with
/// `change` when there is one and the scene takes it, else by importing the
/// layer through `assets`. Whether it imported; `scene` is unchanged when
/// this fails.
pub fn update_or_import(
    scene: &mut Imported,
    layer: &Path,
    pass: Option<&str>,
    change: Option<(Applied, crust_core::live::Change)>,
    assets: Arc<dyn Assets>,
) -> Result<bool, String> {
    if let Some((applied, change)) = change {
        match scene.update(layer, pass, &change, applied) {
            Ok(()) => return Ok(false),
            // One per edit batch that falls back.
            Err(why) => debug!("{applied} edit applied as full instead: {why}"),
        }
    }
    *scene = import_kept(layer, pass, assets)?;
    Ok(true)
}

#[cfg(test)]
mod live_tests;
