//! The session thread (design D2): one thread owns everything a session
//! holds that cannot cross threads — openusd's `Stage` is built on `Rc` and
//! `RefCell`, neither `Send` nor `Sync` — and the protocol's async handlers
//! reach it through a channel.
//!
//! A command is a closure run on that thread against the [`State`]. The
//! closure must be `Send` to get there; what it touches never leaves. The
//! thread runs commands one at a time in the order they were sent, so one
//! caller's commands run in the order it sent them, however many callers
//! interleave theirs.

use super::delta::{Applied, classify, layer_diff};
use super::edit::{
    Snippet, active_snippet, attribute_snippet, binding_snippet, reanchor, variant_snippet,
};
use super::layer::{absolute, new_layer_text, same_file};
use super::query::{prim_at, variant_sets};
use super::render::{Job, Renders};
use super::scene::{Imported, import};
use super::sources::Sources;
use crate::{Cli, Command, RenderRun, render_and_write};
use clap::Parser;
use crust_assets::FileAssets;
use crust_core::inspect::{self, GroupBy};
use crust_core::{PixelRect, RenderControl, RenderOutcome};
use openusd::sdf;
use openusd::usd::{InitialLoadSet, Stage};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use tracing::{debug, info};

/// What the session thread owns.
#[derive(Default)]
pub struct State {
    /// The open session; `None` until `open_session`.
    pub open: Option<Open>,
}

/// An open session: the override layer, the stage authored through it, and
/// the scene imported from it.
pub struct Open {
    /// The stage the layer is an override of, absolute.
    pub input: PathBuf,
    /// The override layer, absolute: the session itself.
    pub output: PathBuf,
    /// The authoring stage, opened on the override layer (its root layer, so
    /// the edit target) with payloads unloaded.
    pub stage: Stage,
    /// The last import of the saved layer.
    pub scene: Imported,
    /// The layer's bytes before each edit batch of this process, the latest
    /// last (design D4). Not kept across a resume.
    pub undo: Vec<Vec<u8>>,
    /// The session's renders.
    pub renders: Renders,
    /// The asset loader every import of the session goes through, keeping
    /// its decodes; `None` under `CRUST_SCENE_DELTAS=0`, where each import
    /// gets a fresh one.
    pub(super) assets: Option<Arc<FileAssets>>,
    /// The layers the last import composed, other than the override layer,
    /// stamped (`live-session-source-reload` D2).
    pub(super) sources: Sources,
    /// `Some` when the scene no longer shows the sources on disk: the layers
    /// that changed since the last import (empty for a `reload`, which
    /// imports whether or not one did). The next import is full; `None`
    /// once one succeeds (D3).
    pub(super) stale: Option<Vec<PathBuf>>,
    /// Whether the authoring stage must be composed again: a source changed
    /// and reopening the stage has not succeeded since.
    pub(super) restage: bool,
}

/// The loader for the session's next import or update: the session's own,
/// its decodes revalidated against the files on disk, or a fresh one under
/// `CRUST_SCENE_DELTAS=0`; and how many kept decodes changed on disk.
fn loader(kept: Option<&Arc<FileAssets>>) -> (Arc<FileAssets>, usize) {
    match kept {
        Some(assets) => {
            let dropped = assets.revalidate();
            if dropped > 0 {
                debug!("{dropped} decoded asset(s) changed on disk and will be read again");
            }
            (assets.clone(), dropped)
        }
        None => (Arc::new(FileAssets::new()), 0),
    }
}

/// Imports the saved layer at `layer` through `assets`, then lets go of the
/// kept decodes the new scene does not use.
fn import_kept(layer: &Path, assets: Arc<FileAssets>) -> Result<Imported, String> {
    let scene = import(layer, assets.clone())?;
    let unused = assets.sweep();
    if unused > 0 {
        debug!("{unused} decoded asset(s) the scene no longer uses let go");
    }
    Ok(scene)
}

/// Opens `path`'s layer as the authoring stage, payloads unloaded: they load
/// only for the prims a query reaches (design, Risks).
fn open_stage(path: &Path) -> Result<Stage, String> {
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
fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let staged = path.with_file_name(format!(".{name}.crust-restore"));
    std::fs::write(&staged, bytes)?;
    std::fs::rename(&staged, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

/// `stage`'s composition errors, as messages.
fn composition_errors(stage: &Stage) -> Vec<String> {
    stage
        .composition_errors()
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// `paths`, comma-separated, for a message.
fn names(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The sublayers `stage`'s root layer authors, as written.
fn authored_sublayers(stage: &Stage) -> Vec<String> {
    let root = stage.root_layer();
    match root.data().try_field(&sdf::Path::abs_root(), "subLayers") {
        Ok(Some(value)) => match value.as_ref() {
            sdf::Value::StringVec(paths) => paths.clone(),
            sdf::Value::AssetPathVec(paths) => paths.iter().map(|p| p.to_string()).collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

impl Open {
    /// Authors `snippet` as one edit batch (spec: "Each edit batch is saved
    /// and imported"): merges it into the layer, saves the layer and imports
    /// the saved file. When the save or the import fails, the layer goes back
    /// to its bytes before the batch, and the batch is refused.
    pub fn edit(&mut self, snippet: &Snippet) -> Result<serde_json::Value, String> {
        self.refresh()?;
        // Before the import: a render must not outlive the scene it renders.
        let cancelled = self.renders.stop();
        let before =
            std::fs::read(&self.output).map_err(|e| format!("{}: {e}", self.output.display()))?;
        snippet.author(&self.stage)?;
        let root = self.stage.root_layer().identifier().to_owned();
        let saved = self
            .stage
            .layer_mut(&root)
            .ok_or_else(|| format!("cannot save {}: the stage has no root layer", root))
            .and_then(|mut layer| {
                layer
                    .save()
                    .map_err(|e| format!("cannot save {}: {e}", self.output.display()))
            });
        let synced = saved.and_then(|()| self.sync(&before));
        match synced {
            Ok(mut result) => {
                self.undo.push(before);
                result["undo_depth"] = self.undo.len().into();
                result["authored_specs"] = snippet.specs().into();
                result["cancelled_render"] = cancelled.into();
                Ok(result)
            }
            Err(e) => {
                self.restore(&before)?;
                Err(format!(
                    "{e}; the edit was not kept, and the layer is as it was"
                ))
            }
        }
    }

    /// `undo` (design D4): the layer as it was before the last edit batch of
    /// this process, saved and imported. The step leaves the history only
    /// once the restored layer has imported: when its restore or import
    /// fails, the layer goes back to its current bytes, the scene stays the
    /// one it renders, and the same `undo` can be tried again.
    pub fn undo(&mut self) -> Result<serde_json::Value, String> {
        let Some(before) = self.undo.last().cloned() else {
            return Err(
                "nothing to undo: no edit batch in this session (undo history does \
                        not survive a resume)"
                    .to_owned(),
            );
        };
        self.refresh()?;
        let cancelled = self.renders.stop();
        let current =
            std::fs::read(&self.output).map_err(|e| format!("{}: {e}", self.output.display()))?;
        let synced = self.restore(&before).and_then(|()| self.sync(&current));
        let mut result = match synced {
            Ok(result) => result,
            Err(e) => {
                let back = match self.restore(&current) {
                    Ok(()) => "the layer is as it was".to_owned(),
                    Err(r) => format!("putting the layer back failed too ({r})"),
                };
                return Err(format!(
                    "{e}; the undo was not applied: {back}, and the step is still there \
                     to undo"
                ));
            }
        };
        self.undo.pop();
        result["undo_depth"] = self.undo.len().into();
        result["cancelled_render"] = cancelled.into();
        Ok(result)
    }

    /// `render_final` (design D8): the saved layer rendered by `crust render
    /// <output>`'s own body, after stopping the session's render. Product
    /// paths resolve against the layer's directory; a stage without products
    /// writes `<output stem>.exr` and its PNG beside the layer.
    pub fn render_final(&mut self) -> Result<serde_json::Value, String> {
        // It imports the saved layer from disk itself; this keeps the
        // authoring stage up to date with what it renders.
        self.refresh()?;
        let cancelled = self.renders.stop();
        let layer = self.output.to_str().ok_or("the layer path is not UTF-8")?;
        let args = match Cli::try_parse_from(["crust", "render", "-i", layer]) {
            Ok(Cli {
                command: Command::Render(args),
                ..
            }) => args,
            Ok(_) => unreachable!("parsed as render"),
            Err(e) => return Err(e.to_string()),
        };
        let beauty = self.output.with_extension("exr");
        let beauty = beauty.to_str().ok_or("the layer path is not UTF-8")?;
        let dir = self.layer_dir();
        let control = RenderControl::without_snapshots();
        let run = RenderRun {
            control: &control,
            progress_bar: false,
            rendering: &|| {},
            rendered: &|| false,
            anchor: Some(&dir),
            beauty: Some(beauty),
        };
        let started = std::time::Instant::now();
        let written = render_and_write(&args, &run).map_err(|_| {
            "the final render failed; the server's log (stderr) says why".to_owned()
        })?;
        Ok(serde_json::json!({
            "files": written.files,
            "interrupted": written.interrupted,
            "seconds": started.elapsed().as_secs_f64(),
            "cancelled_render": cancelled,
        }))
    }

    /// `render`: starts a render of the scene, its own settings with the
    /// call's `region` (`[x0, y0, x1, y1]`, as `--region`) and samples, after
    /// stopping the one running.
    pub fn start_render(
        &mut self,
        region: Option<[usize; 4]>,
        spp: Option<u32>,
    ) -> Result<(Arc<Job>, Vec<u64>), String> {
        self.bring_current()?;
        let mut settings = self.scene.settings;
        if let Some(spp) = spp {
            if spp == 0 {
                return Err("spp must be at least 1".to_owned());
            }
            settings = settings.with_samples_per_pixel(spp);
        }
        if let Some([x0, y0, x1, y1]) = region {
            if x1 <= x0 || y1 <= y0 {
                return Err(format!(
                    "the region {region:?} is empty: x1 must be greater than x0, and y1 than y0"
                ));
            }
            settings = settings
                .with_region(PixelRect::new(x0, y0, x1, y1))
                .map_err(|e| format!("region: {e}"))?;
        }
        let aovs = self.scene.aovs.clone();
        let space = self.scene.working_space;
        let geometry = self.scene.geometry;
        let layer = self.layer_hash()?;
        self.renders.start(
            &mut self.scene.renderer,
            settings,
            aovs,
            space,
            geometry,
            layer,
        )
    }

    /// A hash of the saved layer's bytes and of the sources' stamps as they
    /// are on disk now: which layers a render rendered (design D5).
    fn layer_hash(&self) -> Result<u64, String> {
        use std::hash::{Hash, Hasher};
        let bytes =
            std::fs::read(&self.output).map_err(|e| format!("{}: {e}", self.output.display()))?;
        let mut hasher = std::hash::DefaultHasher::new();
        bytes.hash(&mut hasher);
        self.sources.hash(&mut hasher);
        Ok(hasher.finish())
    }

    /// `light_paths` (`light-path-inspection`): the `crust-light-paths/1`
    /// report of `region` (`[x0, y0, x1, y1]`, image space) of completed
    /// render `render_id`, re-rendered at that render's settings and samples
    /// on the session's scene — which must be the scene it rendered: the
    /// layer it rendered, saved again by an undo or never edited since.
    pub fn light_paths(
        &mut self,
        render_id: u64,
        region: [usize; 4],
        group_by: GroupBy,
        top: usize,
    ) -> Result<serde_json::Value, String> {
        self.refresh()?;
        let job = self.renders.get(render_id)?;
        if job.finished().map(|f| f.outcome) != Some(RenderOutcome::Completed) {
            return Err(format!(
                "render {render_id} is {}: light_paths inspects a completed render (wait for \
                 it with snapshot's budget_s, or render again)",
                job.status()
            ));
        }
        if self.layer_hash()? != job.layer {
            return Err(format!(
                "the session changed since render {render_id}: its layer, or a source layer on \
                 disk, is not the one that render used. Render again and inspect the new render \
                 (or undo back to it)"
            ));
        }
        let [x0, y0, x1, y1] = region;
        let rect = PixelRect::new(x0, y0, x1, y1);
        inspect::check_request(rect, group_by, top).map_err(|e| e.to_string())?;
        let rendered = job.settings.region();
        if !(rendered.contains(x0, y0) && x1 <= rendered.x1 && y1 <= rendered.y1) {
            return Err(format!(
                "the region {region:?} is not inside render {render_id}'s region {:?}",
                [rendered.x0, rendered.y0, rendered.x1, rendered.y1]
            ));
        }
        if let Some(running) = self.renders.running() {
            return Err(format!(
                "render {running} is still running and holds the scene: wait for it or cancel \
                 it, then call light_paths again"
            ));
        }
        let renderer =
            Arc::get_mut(&mut self.scene.renderer).ok_or("the scene is still held by a render")?;
        let options = inspect::Options {
            top,
            group_by,
            command: Some(format!(
                "crust light-paths -i {} -s {} --region {x0},{y0},{x1},{y1} --top {top} \
                 --group-by {group_by}",
                self.output.display(),
                job.settings.samples_per_pixel()
            )),
            camera: self.scene.report.scene.camera.clone(),
        };
        let own = renderer.settings;
        renderer.retune(job.settings);
        let report = inspect::run(renderer, rect, &options);
        renderer.retune(own);
        let report = report.map_err(|e| match e {
            crust_core::Error::EmptyRegion { .. } => format!("region: {e}"),
            e => e.to_string(),
        })?;
        serde_json::to_value(report).map_err(|e| e.to_string())
    }

    /// Looks at the sources on disk (`live-session-source-reload` D3): when a
    /// layer the last import composed changed, reopens the authoring stage,
    /// so queries and edits see the new composition, and marks the scene
    /// stale, so the next import is full. Imports nothing itself.
    ///
    /// The change is recorded before the stage is reopened: when the new
    /// sources do not compose, the call fails naming them, and the next one
    /// tries again rather than reading the old stage or scene.
    pub fn refresh(&mut self) -> Result<(), String> {
        let changed = self.sources.changed();
        if !changed.is_empty() {
            debug!(
                "{} source layer(s) changed on disk; the stage is composed again",
                changed.len()
            );
            self.mark_stale(changed);
            self.restage = true;
        }
        if self.restage {
            let changed = names(self.stale.as_deref().unwrap_or_default());
            let stage = open_stage(&self.output)
                .map_err(|e| format!("the sources changed on disk ({changed}): {e}"))?;
            // A layer that no longer parses composes as if it were absent; a
            // writer caught mid-write looks the same. Refused while the
            // change brings a composition error the stage did not have.
            let had: Vec<String> = composition_errors(&self.stage);
            let new: Vec<String> = composition_errors(&stage)
                .into_iter()
                .filter(|e| !had.contains(e))
                .collect();
            if !new.is_empty() {
                return Err(format!(
                    "the sources changed on disk ({changed}) and no longer compose: {}",
                    new.join("; ")
                ));
            }
            self.stage = stage;
            self.restage = false;
        }
        Ok(())
    }

    /// Adds `changed` to the layers the scene no longer shows.
    fn mark_stale(&mut self, changed: Vec<PathBuf>) {
        let stale = self.stale.get_or_insert_with(Vec::new);
        for path in changed {
            if !stale.contains(&path) {
                stale.push(path);
            }
        }
    }

    /// [`Open::refresh`], then, when the scene is stale, the saved layer
    /// imported over the sources as they are now (after stopping the
    /// running render): what a call that renders or reports the scene needs.
    /// The layers that changed; empty when the scene was current. A failed
    /// import keeps the scene stale, so the next call tries again.
    pub fn bring_current(&mut self) -> Result<Vec<PathBuf>, String> {
        self.refresh()?;
        let Some(changed) = self.stale.clone() else {
            return Ok(Vec::new());
        };
        self.renders.stop();
        let current =
            std::fs::read(&self.output).map_err(|e| format!("{}: {e}", self.output.display()))?;
        self.sync(&current).map_err(|e| {
            format!(
                "the sources changed on disk ({}) and the stage no longer imports: {e}",
                names(&changed)
            )
        })?;
        Ok(changed)
    }

    /// `reload` (D4): the stage composed again and the saved layer imported
    /// whole, whether or not a source changed, after stopping the running
    /// render. The undo history stays: it holds the override layer's bytes,
    /// which compose over the sources as they are now.
    pub fn reload(&mut self) -> Result<serde_json::Value, String> {
        let cancelled = self.renders.stop();
        let changed = self.sources.changed();
        self.mark_stale(changed);
        self.restage = true;
        self.refresh()?;
        let current =
            std::fs::read(&self.output).map_err(|e| format!("{}: {e}", self.output.display()))?;
        let mut result = self.sync(&current)?;
        result["undo_depth"] = self.undo.len().into();
        result["cancelled_render"] = cancelled.into();
        Ok(result)
    }

    /// Writes `bytes` as the layer and reopens the authoring stage on it.
    fn restore(&mut self, bytes: &[u8]) -> Result<(), String> {
        replace_file(&self.output, bytes).map_err(|e| format!("{}: {e}", self.output.display()))?;
        self.stage = open_stage(&self.output)?;
        Ok(())
    }

    /// Brings the scene up to date with the saved layer, whose bytes were
    /// `before` until the change just made (spec: "Edits apply in place when
    /// they are confined"), and says what that changed: the warning codes it
    /// gained and lost, the time taken, and how the change was applied. The
    /// scene is unchanged when this fails.
    fn sync(&mut self, before: &[u8]) -> Result<serde_json::Value, String> {
        let old: Vec<&'static str> = self.scene.warning_codes();
        let sources_changed = self.stale.clone().unwrap_or_default();
        self.apply_change(before)?;
        let new = self.scene.warning_codes();
        let gained: Vec<&str> = new.iter().filter(|c| !old.contains(c)).copied().collect();
        let lost: Vec<&str> = old.iter().filter(|c| !new.contains(c)).copied().collect();
        Ok(serde_json::json!({
            "saved": self.output,
            "applied": self.scene.applied.as_str(),
            "import_s": self.scene.took.as_secs_f64(),
            "warnings": self.scene.report.warnings.len(),
            "warnings_gained": gained,
            "warnings_lost": lost,
            "sources_changed": sources_changed,
        }))
    }

    /// The change from `before` to the saved layer, in place when it is
    /// confined to settings, light inputs or material inputs and the scene
    /// takes it, else by importing the saved layer.
    ///
    /// The kept decodes are revalidated first, whichever way the change goes:
    /// when a file the scene read changed on disk, the scene's other parts
    /// still hold its old decode, so only a full import gives what a fresh one
    /// would.
    fn apply_change(&mut self, before: &[u8]) -> Result<(), String> {
        let (assets, changed_on_disk) = loader(self.assets.as_ref());
        if changed_on_disk > 0 {
            debug!("Edit applied as full: {changed_on_disk} asset file(s) changed on disk");
        } else if let Some(changed) = &self.stale {
            debug!(
                "Applied as full: the sources changed on disk ({} layer(s)), or a reload",
                changed.len()
            );
        } else if self.assets.is_some()
            && let Some((applied, change)) = self.classify(before)
        {
            match self.scene.update(&self.output, &change, applied) {
                Ok(()) => return Ok(()),
                // One per edit batch that falls back.
                Err(why) => debug!("{applied} edit applied as full instead: {why}"),
            }
        }
        let before = self.sources.now();
        self.scene = import_kept(&self.output, assets)?;
        self.sources = Sources::of(&self.scene.layers, &self.output, &before);
        self.stale = None;
        Ok(())
    }

    /// The class of the change from `before` to the saved layer and what it
    /// rebuilds, `None` when it needs a full import.
    fn classify(&self, before: &[u8]) -> Option<(Applied, crust_core::live::Change)> {
        let provenance = self.scene.provenance()?;
        let after = std::fs::read(&self.output).ok()?;
        let parse = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .ok()
                .and_then(|text| openusd::usda::parse(text).ok())
        };
        let (before, after) = (parse(before)?, parse(&after)?);
        let changes = layer_diff(&before, &after);
        let (applied, change, full) = classify(&changes, &self.stage, provenance);
        if let Some(why) = full {
            debug!("Edit applied as full: {why}");
            return None;
        }
        Some((applied, change))
    }

    /// `path`, an existing prim of the stage, which an edit may target: not
    /// an instance proxy, whose opinions USD reads from its prototype only.
    pub(super) fn editable_prim(&self, path: &str) -> Result<sdf::Path, String> {
        let path = sdf::Path::new(path).map_err(|e| format!("{path:?} is not a USD path: {e}"))?;
        if path.is_property() {
            return Err(format!(
                "{path} is a property path; a prim path is expected"
            ));
        }
        let prim = prim_at(&self.stage, &path)?;
        if prim.is_instance_proxy().unwrap_or(false) {
            return Err(format!(
                "{path} is inside an instance: author on the instance's prototype source, or \
                 make the instance not instanceable first"
            ));
        }
        Ok(path)
    }

    /// `set_attribute`: `value` as the attribute's declared type, or `ty`
    /// for an attribute no layer declares.
    pub fn set_attribute(
        &mut self,
        path: &str,
        value: &serde_json::Value,
        ty: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        self.refresh()?;
        let attr_path =
            sdf::Path::new(path).map_err(|e| format!("{path:?} is not a USD path: {e}"))?;
        if !attr_path.is_property() {
            return Err(format!("{path} is not an attribute path (`/prim.name`)"));
        }
        self.editable_prim(&attr_path.prim_path().to_string())?;
        let attr = self
            .stage
            .attribute(attr_path.clone())
            .map_err(|e| e.to_string())?;
        let declared = attr.type_name().ok().flatten().map(|t| t.to_string());
        let type_name = match (declared, ty) {
            (Some(declared), Some(given)) if declared != given => {
                return Err(format!(
                    "{path} is declared as {declared}, not {given}; leave `type` out to use \
                     the declared type"
                ));
            }
            (Some(declared), _) => declared,
            (None, Some(given)) => given.to_owned(),
            (None, None) => {
                return Err(format!(
                    "no layer declares {path}: pass its USD `type` (`float`, `color3f`, \
                     `asset`, ...)"
                ));
            }
        };
        let uniform = attr.variability().ok().flatten() == Some(sdf::Variability::Uniform);
        let (input_dir, layer_dir) = (self.input_dir(), self.layer_dir());
        let asset = |p: &str| reanchor(p, &input_dir, &layer_dir);
        let text = attribute_snippet(&attr_path, &type_name, uniform, value, &asset)
            .map_err(|e| format!("{path}: {e}"))?;
        self.edit(&Snippet::parse(&text)?)
    }

    /// `set_variant`: a variant selection on `prim`, refused when the prim
    /// composes no such set or the set no such variant.
    pub fn set_variant(
        &mut self,
        prim: &str,
        set: &str,
        variant: &str,
    ) -> Result<serde_json::Value, String> {
        self.refresh()?;
        let path = self.editable_prim(prim)?;
        let sets = variant_sets(&self.stage, &prim_at(&self.stage, &path)?);
        let Some(known) = sets.get(set) else {
            return Err(format!(
                "{prim} has no variant set {set:?}; it has {:?}",
                sets.keys().collect::<Vec<_>>()
            ));
        };
        if !known.variants.is_empty() && !known.variants.iter().any(|v| v == variant) {
            return Err(format!(
                "{prim}'s variant set {set:?} has no variant {variant:?}; it has {:?}",
                known.variants
            ));
        }
        self.edit(&Snippet::parse(&variant_snippet(&path, set, variant)?)?)
    }

    /// `set_active`: `active` metadata on `prim`.
    pub fn set_active(&mut self, prim: &str, active: bool) -> Result<serde_json::Value, String> {
        self.refresh()?;
        let path = self.editable_prim(prim)?;
        self.edit(&Snippet::parse(&active_snippet(&path, active)?)?)
    }

    /// `bind_material`: `material:binding` on `prim` to `material`, which
    /// must be a Material prim of the stage.
    pub fn bind_material(
        &mut self,
        prim: &str,
        material: &str,
    ) -> Result<serde_json::Value, String> {
        self.refresh()?;
        let path = self.editable_prim(prim)?;
        let mat =
            sdf::Path::new(material).map_err(|e| format!("{material:?} is not a USD path: {e}"))?;
        let mat_prim = prim_at(&self.stage, &mat)?;
        let ty = mat_prim.type_name().ok().flatten().map(|t| t.to_string());
        if ty.as_deref() != Some("Material") {
            return Err(format!("{material} is a {ty:?} prim, not a Material"));
        }
        self.edit(&Snippet::parse(&binding_snippet(&path, &mat)?)?)
    }

    /// `author_usda`: the snippet as it is, one batch.
    pub fn author_usda(&mut self, text: &str) -> Result<serde_json::Value, String> {
        self.edit(&Snippet::parse(text)?)
    }

    pub(super) fn input_dir(&self) -> PathBuf {
        self.input
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    pub(super) fn layer_dir(&self) -> PathBuf {
        self.output
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }
}

impl State {
    /// `open_session` (spec: "Opening a session creates an override layer",
    /// "Resuming a session", "One session at a time"): cancels the open
    /// session's render, creates or resumes `output` on `input` and imports
    /// it, and only then closes the session that was open. Refused before
    /// anything is written when `output` is the input, is not a `.usda`, or
    /// exists without being an override layer of `input`; a refused or
    /// failed call leaves the open session as it was, its history included.
    pub fn open_session(&mut self, input: &str, output: &str) -> Result<serde_json::Value, String> {
        let cancelled = self.open.as_mut().and_then(|open| open.renders.stop());
        let input = absolute(Path::new(input))?;
        let output = absolute(Path::new(output))?;
        if !input.is_file() {
            return Err(format!("the input {} is not a file", input.display()));
        }
        if output.extension().and_then(|e| e.to_str()) != Some("usda") {
            return Err(format!(
                "the output {} must be a .usda layer: the session is a text file of opinions",
                output.display()
            ));
        }
        if same_file(&input, &output) {
            return Err(format!(
                "the output {} is the input: a session never writes a source layer",
                output.display()
            ));
        }
        let dir = output
            .parent()
            .ok_or("the output has no directory")?
            .to_owned();
        let resumed = output.exists();
        let stage = if resumed {
            let stage = open_stage(&output)?;
            let sublayers = authored_sublayers(&stage);
            let of_input = matches!(&sublayers[..], [one] if same_file(&dir.join(one), &input));
            if !of_input {
                return Err(format!(
                    "{} exists and is not an override layer of {}: its subLayers are {sublayers:?}; \
                     choose another output (a session never writes a source layer)",
                    output.display(),
                    input.display()
                ));
            }
            stage
        } else {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            std::fs::write(&output, new_layer_text(&input, &dir))
                .map_err(|e| format!("{}: {e}", output.display()))?;
            match open_stage(&output) {
                Ok(stage) => stage,
                Err(e) => {
                    let _ = std::fs::remove_file(&output);
                    return Err(e);
                }
            }
        };
        let assets = crust_core::config()
            .scene_deltas
            .then(|| Arc::new(FileAssets::new().keep_decoded()));
        let scene = match import_kept(&output, loader(assets.as_ref()).0) {
            Ok(scene) => scene,
            Err(e) => {
                // A new layer the import refuses is no session: leave nothing.
                if !resumed {
                    let _ = std::fs::remove_file(&output);
                }
                return Err(e);
            }
        };
        let report = &scene.report;
        let result = serde_json::json!({
            "input": input,
            "output": output,
            "resumed": resumed,
            "camera": report.scene.camera,
            "resolution": report.scene.resolution,
            "warnings": report.warnings.len(),
            "warning_codes": scene.warning_codes(),
            "import_s": scene.took.as_secs_f64(),
            "cancelled_render": cancelled,
        });
        info!(
            "Session {}: {} on {}",
            if resumed { "resumed" } else { "opened" },
            output.display(),
            input.display()
        );
        self.close();
        let sources = Sources::of(&scene.layers, &output, &Sources::default());
        self.open = Some(Open {
            input,
            output,
            stage,
            scene,
            undo: Vec::new(),
            renders: Renders::default(),
            assets,
            sources,
            stale: None,
            restage: false,
        });
        Ok(result)
    }

    /// Closes the open session, if any, cancelling its render: the id of the
    /// render it cancelled. Its layer is already saved.
    pub fn close(&mut self) -> Option<u64> {
        let mut open = self.open.take()?;
        let cancelled = open.renders.stop();
        debug!("Session on {} closed", open.output.display());
        cancelled
    }

    /// The open session, or the error a tool answers without one.
    pub fn session(&mut self) -> Result<&mut Open, String> {
        self.open
            .as_mut()
            .ok_or_else(|| "no session is open: call open_session first".to_owned())
    }
}

type Queued = Box<dyn FnOnce(&mut State) + Send>;

/// The session thread is gone, so no command reaches it any more. Under the
/// release profile's `panic = "abort"` a panic there ends the process, so in
/// practice this is a command that unwound out of it in a debug build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionGone;

impl std::fmt::Display for SessionGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the session thread has stopped")
    }
}

/// A handle on the session thread; clones share it. The thread stops once
/// the last handle is dropped and the commands already sent have run.
#[derive(Clone)]
pub struct Session {
    jobs: mpsc::Sender<Queued>,
}

impl Session {
    /// Starts the session thread with an empty [`State`].
    pub fn spawn() -> std::io::Result<(Self, JoinHandle<()>)> {
        let (jobs, queue) = mpsc::channel::<Queued>();
        let thread = std::thread::Builder::new()
            .name("crust-session".into())
            .spawn(move || {
                let mut state = State::default();
                for job in queue {
                    job(&mut state);
                }
            })?;
        Ok((Session { jobs }, thread))
    }

    /// Sends `command` to the session thread now, and returns its reply as a
    /// future. Sending happens here, not when the future is first polled, so
    /// commands are queued in the order `call` is called.
    pub fn call<R, F>(&self, command: F) -> impl Future<Output = Result<R, SessionGone>> + use<R, F>
    where
        R: Send + 'static,
        F: FnOnce(&mut State) -> R + Send + 'static,
    {
        let (reply, receive) = tokio::sync::oneshot::channel();
        let sent = self.jobs.send(Box::new(move |state: &mut State| {
            // The caller may have stopped waiting; the command still ran.
            let _ = reply.send(command(state));
        }));
        async move {
            sent.map_err(|_| SessionGone)?;
            receive.await.map_err(|_| SessionGone)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// `replace_file` gives the path a new file rather than rewriting the one
    /// there, so an import that still maps the old layer keeps the bytes it
    /// mapped; the staging sibling does not survive.
    #[cfg(unix)]
    #[test]
    fn restore_replaces_the_layer_file() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join("crust_mcp_replace_file");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let layer = dir.join("scene.usda");
        let old: &[u8] = b"#usda 1.0\n";
        std::fs::write(&layer, old).expect("write the layer");
        let before = std::fs::metadata(&layer).expect("stat").ino();
        let held = std::fs::File::open(&layer).expect("open the old layer");

        replace_file(&layer, b"#usda 1.0\n# restored\n").expect("replace");

        assert_eq!(
            std::fs::read(&layer).expect("read"),
            b"#usda 1.0\n# restored\n"
        );
        assert_ne!(std::fs::metadata(&layer).expect("stat").ino(), before);
        assert_eq!(
            held.metadata().expect("stat the held file").len(),
            old.len() as u64
        );
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("list")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".crust-restore"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    thread_local! {
        /// How many commands this thread has run: on the session thread, the
        /// order the commands ran in.
        static RAN: Cell<u64> = const { Cell::new(0) };
    }

    /// 4 callers on their own threads each queue 25 commands before awaiting
    /// any: all 100 run, on the one session thread, each caller's in the
    /// order it sent them, and each reply goes back to its own command.
    #[test]
    fn interleaved_commands_reply_in_order_per_caller() {
        const CALLERS: usize = 4;
        const EACH: usize = 25;
        let (session, thread) = Session::spawn().expect("spawn the session thread");
        let session_thread = thread.thread().id();
        let callers: Vec<_> = (0..CALLERS)
            .map(|caller| {
                let session = session.clone();
                std::thread::spawn(move || {
                    let pending: Vec<_> = (0..EACH)
                        .map(|seq| {
                            session.call(move |state: &mut State| {
                                assert!(state.open.is_none());
                                let order = RAN.with(|n| {
                                    n.set(n.get() + 1);
                                    n.get()
                                });
                                (caller, seq, order, std::thread::current().id())
                            })
                        })
                        .collect();
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .expect("a runtime");
                    pending
                        .into_iter()
                        .map(|reply| runtime.block_on(reply).expect("the session replies"))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut orders = Vec::new();
        for (caller, handle) in callers.into_iter().enumerate() {
            let replies = handle.join().expect("a caller");
            assert_eq!(replies.len(), EACH);
            let mut last = 0;
            for (seq, &(c, s, order, ran_on)) in replies.iter().enumerate() {
                assert_eq!((c, s), (caller, seq), "the reply of another command");
                assert_eq!(ran_on, session_thread, "ran off the session thread");
                assert!(
                    order > last,
                    "caller {caller}'s command {seq} ran out of order"
                );
                last = order;
                orders.push(order);
            }
        }
        orders.sort_unstable();
        assert_eq!(orders, (1..=(CALLERS * EACH) as u64).collect::<Vec<_>>());

        drop(session);
        thread
            .join()
            .expect("the session thread stops with its last handle");
    }
}
