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

use super::edit::{
    Snippet, active_snippet, attribute_snippet, binding_snippet, reanchor, variant_snippet,
};
use super::layer::{absolute, new_layer_text, same_file};
use super::query::{prim_at, variant_sets};
use super::render::{Job, Renders};
use super::scene::{Imported, import};
use crate::{Cli, Command, RenderRun, render_and_write};
use clap::Parser;
use crust_core::{PixelRect, RenderControl};
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
}

/// Opens `path`'s layer as the authoring stage, payloads unloaded: they load
/// only for the prims a query reaches (design, Risks).
fn open_stage(path: &Path) -> Result<Stage, String> {
    let id = path.to_str().ok_or("the layer path is not UTF-8")?;
    Stage::builder()
        .load(InitialLoadSet::LoadNone)
        .open(id)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))
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
        let imported = saved.and_then(|()| import(&self.output));
        match imported {
            Ok(scene) => {
                self.undo.push(before);
                let mut result = self.swap(scene);
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
        let cancelled = self.renders.stop();
        let current =
            std::fs::read(&self.output).map_err(|e| format!("{}: {e}", self.output.display()))?;
        let imported = self.restore(&before).and_then(|()| import(&self.output));
        let scene = match imported {
            Ok(scene) => scene,
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
        let mut result = self.swap(scene);
        result["cancelled_render"] = cancelled.into();
        Ok(result)
    }

    /// `render_final` (design D8): the saved layer rendered by `crust render
    /// <output>`'s own body, after stopping the session's render. Product
    /// paths resolve against the layer's directory; a stage without products
    /// writes `<output stem>.exr` and its PNG beside the layer.
    pub fn render_final(&mut self) -> Result<serde_json::Value, String> {
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
        self.renders
            .start(&mut self.scene.renderer, settings, aovs, space)
    }

    /// Writes `bytes` as the layer and reopens the authoring stage on it.
    fn restore(&mut self, bytes: &[u8]) -> Result<(), String> {
        std::fs::write(&self.output, bytes)
            .map_err(|e| format!("{}: {e}", self.output.display()))?;
        self.stage = open_stage(&self.output)?;
        Ok(())
    }

    /// Replaces the scene with a new import of the layer, and says what that
    /// changed: the warning codes it gained and lost, and the import time.
    fn swap(&mut self, scene: Imported) -> serde_json::Value {
        let old = self.scene.warning_codes();
        let new = scene.warning_codes();
        let gained: Vec<&str> = new.iter().filter(|c| !old.contains(c)).copied().collect();
        let lost: Vec<&str> = old.iter().filter(|c| !new.contains(c)).copied().collect();
        let result = serde_json::json!({
            "saved": self.output,
            "import_s": scene.took.as_secs_f64(),
            "warnings": scene.report.warnings.len(),
            "warnings_gained": gained,
            "warnings_lost": lost,
            "undo_depth": self.undo.len(),
        });
        self.scene = scene;
        result
    }

    /// `path`, an existing prim of the stage, which an edit may target: not
    /// an instance proxy, whose opinions USD reads from its prototype only.
    fn editable_prim(&self, path: &str) -> Result<sdf::Path, String> {
        let path = sdf::Path::new(path).map_err(|e| format!("{path:?} is not a USD path: {e}"))?;
        if path.is_property_path() {
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
        let attr_path =
            sdf::Path::new(path).map_err(|e| format!("{path:?} is not a USD path: {e}"))?;
        if !attr_path.is_property_path() {
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

    fn input_dir(&self) -> PathBuf {
        self.input
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    fn layer_dir(&self) -> PathBuf {
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
        let scene = match import(&output) {
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
        self.open = Some(Open {
            input,
            output,
            stage,
            scene,
            undo: Vec::new(),
            renders: Renders::default(),
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
