//! A live session a host drives (`live-session-host`): layers on disk the
//! host writes, followed as `crust mcp` follows its override layer, and the
//! render of them that the host polls. The Hydra delegate (`crust-hydra`)
//! and Mantle's IPR delegate both run on it.
//!
//! Two threads. The session thread owns everything that cannot cross threads
//! — openusd's `Stage` is built on `Rc` — and runs the commands (`load`,
//! `apply`, `cancel`) one at a time, in the order they were sent: each
//! cancels and joins the running render before it touches the scene, and
//! starts a fresh one after. The render thread renders. What the host polls
//! (`snapshot`, `converged`, `aov`) reads the running render's shared state
//! directly, so a poll never waits behind an import, and neither does
//! [`LiveSession::cancel_render`].
//!
//! **The owned layers.** The host names the layers it writes: the root and
//! any layer the root reaches through `subLayers` alone, with no offset. An
//! [`apply`](LiveSession::apply) diffs each owned layer whose bytes moved
//! against its bytes at the last apply and classifies the union of their
//! changes, exactly as one override layer's are. A sublayer of the root
//! layer stack composes in the stage's own namespace, so a path in its diff
//! is the path `classify` reads. Any other layer the import composed is a
//! source: when one moved on disk since the last import, the change is
//! `full`.

use super::delta::{Applied, classify, layer_diff};
use super::scene::Imported;
use super::sources::Sources;
use super::{import_kept, loader, open_stage, update_or_import};
use crate::Assets;
use crust_assets::FileAssets;
use crust_core::{AovFilm, AovRequest, Buffer, RenderControl, RenderOutcome};
use openusd::sdf::{self, AbstractData, Value};
use openusd::usd::Stage;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use tracing::debug;

/// Why a call did not do what it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not yet: no image has been published, or the render has not
    /// completed, so there is nothing to read.
    Busy(String),
    /// The layers could not be opened or imported. The last image stays.
    ImportFailed(String),
    /// crust panicked. The scene is dropped; the next `apply` imports again.
    Panicked(String),
    /// The call itself was wrong: a missing AOV, nothing loaded yet, an
    /// owned layer outside the root layer stack.
    InvalidArgument(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Busy(m) | Error::ImportFailed(m) | Error::InvalidArgument(m) => f.write_str(m),
            Error::Panicked(m) => write!(f, "crust panicked: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// One image of the render as it refines.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Grows with every new image, across renders: a render that restarts
    /// is a new image too. A host that keeps the last one it copied knows
    /// whether to copy again.
    pub generation: u64,
    /// Which render of the session the image belongs to, from 1: every
    /// `load` and `apply` starts the next one.
    pub render: u64,
    pub width: usize,
    pub height: usize,
    /// `width` × `height` RGBA, rows top-down (image space), alpha 1.
    pub rgba: Vec<f32>,
    /// The samples every pixel has taken, as the render's first sweep
    /// reaches them (1, 2, 4, …): a lower bound once adaptive sampling runs.
    pub spp: u32,
    /// Whether this is the final image of a render that completed.
    pub completed: bool,
    /// The scene's working space (`renderingColorSpace`): what a host
    /// encodes `rgba` from, as `crust render` encodes its PNG preview. That
    /// holds for a displayed precomp output too, which the preview encodes
    /// from the same space.
    pub working_space: crust_core::color::Space,
}

/// One AOV of a completed render.
#[derive(Debug, Clone)]
pub struct Aov {
    pub width: usize,
    pub height: usize,
    /// Components per pixel: 1 for a scalar such as `depth`, 3 or 4 for
    /// colour, as the RenderVar declares.
    pub channels: usize,
    /// `width` × `height` × `channels`, interleaved, rows top-down.
    pub data: Vec<f32>,
}

/// A host's hook for an import's warnings.
pub type WarningsHook = Arc<dyn Fn(&[crust_core::Warning]) + Send + Sync>;

/// What a host hooks into the session's threads.
#[derive(Clone, Default)]
pub struct Hooks {
    /// Runs first on each thread the session starts (the session thread and
    /// every render thread): where a host routes that thread's log.
    pub on_thread: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Runs after each import or in-place update with the scene's warnings.
    pub on_warnings: Option<WarningsHook>,
}

/// What a render left when it returned.
struct Finished {
    buffer: Buffer,
    film: Option<AovFilm>,
    outcome: RenderOutcome,
}

/// A render the host polls.
struct Running {
    /// Which render of the session this is, from 1.
    id: u64,
    control: RenderControl,
    /// The products the scene asks for: the AOVs a completed render holds.
    aovs: Arc<AovRequest>,
    /// The space the render's images are in.
    working_space: crust_core::color::Space,
    finished: Mutex<Option<Arc<Finished>>>,
    /// The message of a panic that ended the render.
    panicked: Mutex<Option<String>>,
    /// Whether a poll returned the panic already.
    reported: AtomicBool,
}

impl Running {
    /// `Panicked` when a panic ended the render, to the first poll only: a
    /// host may raise an error per failed poll, and poll on every redraw.
    /// Later ones are `Busy` until the next change renders again.
    fn check(&self) -> Result<(), Error> {
        match lock(&self.panicked).clone() {
            Some(_) if self.reported.swap(true, Ordering::SeqCst) => Err(Error::Busy(
                "the render panicked; the next change renders again".into(),
            )),
            Some(message) => Err(Error::Panicked(message)),
            None => Ok(()),
        }
    }
}

/// What the session thread and the polls share.
#[derive(Default)]
struct Shared {
    /// The latest render started; kept after it is cancelled or the scene
    /// fails to import, so the host keeps its last image.
    running: Mutex<Option<Arc<Running>>>,
    /// How the last change reached the scene.
    applied: Mutex<Option<Applied>>,
}

/// Locks `m`, through a poisoned lock: what it guards is replaced whole, never
/// left half-written.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The message of a caught panic.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// The layers a session follows: the root it imports, and every layer it
/// owns (the root among them), each with its bytes when the scene last
/// caught up with it.
struct Owned {
    root: PathBuf,
    layers: Vec<(PathBuf, Vec<u8>)>,
}

/// What the session thread owns.
struct State {
    shared: Arc<Shared>,
    hooks: Hooks,
    /// The layers `load` named.
    owned: Option<Owned>,
    /// The authoring stage the classifier reads, opened on the saved root.
    stage: Option<Stage>,
    /// The last import (or in-place update) of the root.
    scene: Option<Imported>,
    /// The layers the last full import composed that the session does not
    /// own, as stamped before that import.
    sources: Sources,
    /// The loader every import goes through, keeping its decodes; `None`
    /// under `CRUST_SCENE_DELTAS=0`, where each import gets a fresh one and
    /// every change is a full import.
    assets: Option<Arc<dyn Assets>>,
    render: Option<JoinHandle<()>>,
    renders: u64,
}

type Command = Box<dyn FnOnce(&mut State) + Send>;

/// A session on layers a host writes. Dropping it cancels the render and
/// joins both threads.
pub struct LiveSession {
    shared: Arc<Shared>,
    commands: Option<mpsc::Sender<Command>>,
    thread: Option<JoinHandle<()>>,
}

impl LiveSession {
    /// A session whose imports keep their decodes (unless
    /// `CRUST_SCENE_DELTAS=0`). Nothing is imported until
    /// [`LiveSession::load`].
    pub fn new(hooks: Hooks) -> Result<LiveSession, Error> {
        let assets = crust_core::config()
            .scene_deltas
            .then(|| Arc::new(FileAssets::new().keep_decoded()) as Arc<dyn Assets>);
        LiveSession::start(hooks, assets)
    }

    /// A session whose every import goes through `assets`, kept across
    /// imports: how a test brings a loader of its own.
    pub fn with_assets(hooks: Hooks, assets: Arc<dyn Assets>) -> Result<LiveSession, Error> {
        LiveSession::start(hooks, Some(assets))
    }

    fn start(hooks: Hooks, assets: Option<Arc<dyn Assets>>) -> Result<LiveSession, Error> {
        let shared = Arc::new(Shared::default());
        let (commands, queue) = mpsc::channel::<Command>();
        let ours = shared.clone();
        let thread = std::thread::Builder::new()
            .name("crust-live-session".into())
            .spawn(move || {
                if let Some(enter) = &hooks.on_thread {
                    enter();
                }
                // Built here: the stage it will hold never leaves this thread.
                let mut state = State {
                    shared: ours,
                    hooks,
                    owned: None,
                    stage: None,
                    scene: None,
                    sources: Sources::default(),
                    assets,
                    render: None,
                    renders: 0,
                };
                for command in queue {
                    command(&mut state);
                }
                state.stop();
            })
            .map_err(|e| Error::InvalidArgument(format!("cannot start the session thread: {e}")))?;
        Ok(LiveSession {
            shared,
            commands: Some(commands),
            thread: Some(thread),
        })
    }

    /// Runs `command` on the session thread and waits for its answer.
    fn call<R: Send + 'static>(
        &self,
        command: impl FnOnce(&mut State) -> Result<R, Error> + Send + 'static,
    ) -> Result<R, Error> {
        let (reply, answer) = mpsc::channel();
        let sent = self.commands.as_ref().is_some_and(|c| {
            c.send(Box::new(move |state: &mut State| {
                // A panic drops the scene: the next `apply` imports again.
                let result =
                    catch_unwind(AssertUnwindSafe(|| command(state))).unwrap_or_else(|payload| {
                        state.forget();
                        Err(Error::Panicked(panic_message(&*payload)))
                    });
                let _ = reply.send(result);
            }))
            .is_ok()
        });
        if !sent {
            return Err(Error::Panicked("the session thread has stopped".into()));
        }
        answer
            .recv()
            .unwrap_or_else(|_| Err(Error::Panicked("the session thread has stopped".into())))
    }

    /// Imports `root` whole, as `crust render -i <root>` would, and starts
    /// rendering it; the session owns `root` alone. What a first population
    /// and a time change call.
    pub fn load(&self, root: &Path) -> Result<(), Error> {
        self.load_owned(root, &[])
    }

    /// [`LiveSession::load`], the session also owning `sublayers`: layers the
    /// root reaches through `subLayers` alone, with no offset, whose changes
    /// [`LiveSession::apply`] classifies as the root's. Refused when one is
    /// not in the root layer stack that way.
    pub fn load_owned(&self, root: &Path, sublayers: &[PathBuf]) -> Result<(), Error> {
        let root = root.to_owned();
        let sublayers = sublayers.to_vec();
        self.call(move |state| state.load(&root, &sublayers))
    }

    /// The owned layers changed: applies the change in place when the
    /// session's classes allow it, else imports the root, and restarts the
    /// render. With nothing loaded, or after a failure dropped the scene, it
    /// imports the root `load` named.
    pub fn apply(&self) -> Result<(), Error> {
        self.call(State::apply)
    }

    /// Cancels the running render and waits for it to stop. The last image
    /// stays.
    pub fn cancel(&self) {
        let _ = self.call(|state| {
            state.stop();
            Ok(())
        });
    }

    /// Asks the running render to stop, from any thread, without waiting:
    /// one atomic store, never queued behind an import. The render returns
    /// once the per-pixel advances already in flight finish.
    pub fn cancel_render(&self) {
        if let Some(running) = self.running() {
            running.control.cancel();
        }
    }

    /// How the last `load` or `apply` reached the scene: `full` for an
    /// import, else the class it was applied in place as.
    pub fn applied(&self) -> Option<Applied> {
        *lock(&self.shared.applied)
    }

    fn running(&self) -> Option<Arc<Running>> {
        lock(&self.shared.running).clone()
    }

    /// Which render the session last started, from 1; 0 before the first.
    pub fn render(&self) -> u64 {
        self.running().map_or(0, |r| r.id)
    }

    /// The generation [`LiveSession::snapshot`] would return now, without
    /// copying the image: what a poll compares before it copies.
    pub fn generation(&self) -> Result<u64, Error> {
        let running = self
            .running()
            .ok_or_else(|| Error::Busy("nothing is rendering".into()))?;
        running.check()?;
        let finished = lock(&running.finished).is_some();
        let published = running.control.generation() + u64::from(finished);
        if published == 0 {
            return Err(Error::Busy("the render has published no image yet".into()));
        }
        Ok(generation(running.id, published))
    }

    /// The latest image of the render: its final image once it returned,
    /// else the latest it published. `Busy` before the first.
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        let running = self
            .running()
            .ok_or_else(|| Error::Busy("nothing is rendering".into()))?;
        running.check()?;
        // Read before the image: a stage is reached only once every unit of
        // it published, so an image cloned after a count of N holds N
        // samples everywhere. Read after, a stage completing between the two
        // paired "1 spp" with an image still missing tiles (black blocks in
        // usdview's playback, which shows the first full pass).
        let spp = running.control.max_samples_reached();
        let finished = lock(&running.finished).clone();
        let (published, buffer, completed) = match finished {
            // One past the last publish: the final image, which is the one
            // `crust render` writes.
            Some(f) => (
                running.control.generation() + 1,
                f.buffer.clone(),
                f.outcome == RenderOutcome::Completed,
            ),
            None => {
                let (published, buffer) = running
                    .control
                    .snapshot()
                    .ok_or_else(|| Error::Busy("the render has published no image yet".into()))?;
                (published, buffer, false)
            }
        };
        let (width, height) = buffer.size();
        let mut rgba = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let (r, g, b) = buffer.get_rgb(x, y);
                rgba.extend_from_slice(&[r, g, b, 1.0]);
            }
        }
        Ok(Snapshot {
            generation: generation(running.id, published),
            render: running.id,
            width,
            height,
            rgba,
            spp,
            completed,
            working_space: running.working_space,
        })
    }

    /// Whether the render returned complete. A cancelled render never
    /// converges. One a panic ended has stopped for good, and counts as
    /// converged so a host stops redrawing for it; it keeps its last image.
    pub fn converged(&self) -> bool {
        self.running().is_some_and(|r| {
            lock(&r.panicked).is_some()
                || lock(&r.finished)
                    .as_ref()
                    .is_some_and(|f| f.outcome == RenderOutcome::Completed)
        })
    }

    /// How the latest render returned, `None` while it runs (or before the
    /// first).
    pub fn outcome(&self) -> Option<RenderOutcome> {
        let running = self.running()?;
        lock(&running.finished).as_ref().map(|f| f.outcome)
    }

    /// The AOV named `name` (a RenderVar's layer name, as the products
    /// write it) of the completed render. `Busy` until the render completes.
    pub fn aov(&self, name: &str) -> Result<Aov, Error> {
        let running = self
            .running()
            .ok_or_else(|| Error::Busy("nothing is rendering".into()))?;
        running.check()?;
        let finished = lock(&running.finished)
            .clone()
            .filter(|f| f.outcome == RenderOutcome::Completed)
            .ok_or_else(|| Error::Busy("the render has not completed".into()))?;
        let var = running
            .aovs
            .products
            .iter()
            .flat_map(|p| &p.vars)
            .find(|v| v.name == name)
            .ok_or_else(|| {
                Error::InvalidArgument(format!("the scene's products write no AOV {name:?}"))
            })?;
        let film = finished.film.as_ref().ok_or_else(|| {
            Error::InvalidArgument(format!("the render gathered no AOVs for {name:?}"))
        })?;
        let planes = film.var_channels(&finished.buffer, var);
        let (width, height) = film.dimensions();
        let channels = planes.len();
        let mut data = Vec::with_capacity(width * height * channels);
        for i in 0..width * height {
            data.extend(planes.iter().map(|p| p[i]));
        }
        Ok(Aov {
            width,
            height,
            channels,
            data,
        })
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        // The thread ends once the queue closes, after stopping the render.
        self.commands.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl State {
    /// Cancels the running render and joins its thread, so the scene is no
    /// longer shared with it.
    fn stop(&mut self) {
        if let Some(running) = lock(&self.shared.running).as_ref() {
            running.control.cancel();
        }
        if let Some(thread) = self.render.take() {
            let _ = thread.join();
        }
    }

    /// Drops the scene and the stage after a panic: what they hold may be
    /// half-updated, and the next `apply` imports the root again.
    fn forget(&mut self) {
        self.stop();
        self.scene = None;
        self.stage = None;
    }

    /// Whether a panic ended the last render.
    fn render_panicked(&self) -> bool {
        lock(&self.shared.running)
            .as_ref()
            .is_some_and(|r| lock(&r.panicked).is_some())
    }

    fn load(&mut self, root: &Path, sublayers: &[PathBuf]) -> Result<(), Error> {
        self.stop();
        // Whatever happens next, the old scene is not these layers'.
        self.scene = None;
        self.stage = None;
        let root = super::layer::absolute(root).map_err(Error::InvalidArgument)?;
        let mut layers = vec![root.clone()];
        for sub in sublayers {
            let sub = super::layer::absolute(sub).map_err(Error::InvalidArgument)?;
            if !layers.iter().any(|l| super::layer::same_file(l, &sub)) {
                layers.push(sub);
            }
        }
        let stack = root_layer_stack(&root);
        if let Some(outside) = layers[1..]
            .iter()
            .find(|l| !stack.iter().any(|s| super::layer::same_file(s, l)))
        {
            return Err(Error::InvalidArgument(format!(
                "{} is not a sublayer of {} with no offset",
                outside.display(),
                root.display()
            )));
        }
        // Named even when the import below fails: `apply` imports them once
        // they do. The import reads their bytes.
        let layers = layers.into_iter().map(|path| (path, Vec::new())).collect();
        self.owned = Some(Owned {
            root: root.clone(),
            layers,
        });
        self.import()?;
        self.start()
    }

    /// Imports the root whole: the authoring stage reopened, the owned
    /// layers' bytes and the sources re-stamped. The owned layers' bytes are
    /// read before the import, so a write during it shows at the next apply.
    fn import(&mut self) -> Result<(), Error> {
        let owned = self
            .owned
            .as_mut()
            .ok_or_else(|| Error::InvalidArgument("no layer is loaded".into()))?;
        let root = owned.root.clone();
        for (path, bytes) in &mut owned.layers {
            *bytes = read(path)?;
        }
        let stage = open_stage(&root).map_err(Error::ImportFailed)?;
        let before = self.sources.now();
        let (assets, _) = loader(self.assets.as_ref());
        let scene = import_kept(&root, None, assets).map_err(Error::ImportFailed)?;
        self.sources = sources_of(&scene.layers, owned, &before);
        self.stage = Some(stage);
        self.scene = Some(scene);
        *lock(&self.shared.applied) = Some(Applied::Full);
        self.report();
        Ok(())
    }

    fn apply(&mut self) -> Result<(), Error> {
        if self.owned.is_none() {
            return Err(Error::InvalidArgument("no layer is loaded".into()));
        }
        if self.render_panicked() {
            debug!("The last render panicked: the layers are imported again");
            self.forget();
        }
        self.stop();
        if self.scene.is_none() {
            self.import()?;
            return self.start();
        }
        let owned = self.owned.as_ref().expect("checked above");
        let root = owned.root.clone();
        // Read before classifying: the bytes classified are the bytes the
        // next apply diffs against.
        let after = owned
            .layers
            .iter()
            .map(|(path, _)| read(path))
            .collect::<Result<Vec<_>, _>>()?;
        // Probed on a copy: the stamps advance only once the change they
        // report has been imported, so a failed import is retried by the
        // next apply rather than forgotten.
        let mut sources = self.sources.clone();
        let moved = sources.changed();
        // The stage the change is classified against is the root as saved.
        let stage = open_stage(&root).map_err(Error::ImportFailed)?;
        let (assets, changed_on_disk) = loader(self.assets.as_ref());
        let scene = self.scene.as_mut().expect("checked above");
        let change = if changed_on_disk > 0 || self.assets.is_none() {
            debug!("Change applied as full: decodes changed on disk, or scene deltas are off");
            None
        } else if let Some(source) = moved.first() {
            debug!(
                "Change applied as full: {} changed on disk",
                source.display()
            );
            None
        } else {
            let pairs: Vec<(&[u8], &[u8])> = owned
                .layers
                .iter()
                .zip(&after)
                .map(|((_, before), after)| (before.as_slice(), after.as_slice()))
                .collect();
            scene
                .provenance()
                .and_then(|p| classify_layers(&pairs, &stage, p))
        };
        let before = self.sources.now();
        let imported =
            update_or_import(scene, &root, None, change, assets).map_err(Error::ImportFailed)?;
        let owned = self.owned.as_mut().expect("checked above");
        for ((_, bytes), now) in owned.layers.iter_mut().zip(after) {
            *bytes = now;
        }
        self.sources = if imported {
            sources_of(&scene.layers, owned, &before)
        } else {
            sources
        };
        *lock(&self.shared.applied) = Some(scene.applied);
        self.stage = Some(stage);
        self.report();
        self.start()
    }

    /// Hands the scene's import warnings to the host, once per import.
    fn report(&self) {
        if let (Some(scene), Some(hook)) = (&self.scene, &self.hooks.on_warnings) {
            hook(&scene.report.warnings);
        }
    }

    /// Starts rendering the scene with its own settings, on a fresh control.
    fn start(&mut self) -> Result<(), Error> {
        let scene = self
            .scene
            .as_ref()
            .ok_or_else(|| Error::InvalidArgument("no scene to render".into()))?;
        self.renders += 1;
        let running = Arc::new(Running {
            id: self.renders,
            control: RenderControl::new(),
            aovs: scene.aovs.clone(),
            working_space: scene.working_space,
            finished: Mutex::new(None),
            panicked: Mutex::new(None),
            reported: AtomicBool::new(false),
        });
        *lock(&self.shared.running) = Some(running.clone());
        let renderer = scene.renderer.clone();
        let on_thread = self.hooks.on_thread.clone();
        let thread = std::thread::Builder::new()
            .name(format!("crust-live-render-{}", running.id))
            .spawn(move || {
                if let Some(enter) = &on_thread {
                    enter();
                }
                // The AOVs `crust render` requests for the stage: none
                // without products, so the beauty is the CLI's either way.
                let request = (!running.aovs.products.is_empty()).then_some(&*running.aovs);
                // A panic on a rayon worker (a material's, a texture's)
                // reaches this thread through rayon's join, and stops here.
                let rendered = catch_unwind(AssertUnwindSafe(|| {
                    renderer.render_with_control(true, None, request, &running.control)
                }));
                match rendered {
                    Ok(rendered) => {
                        *lock(&running.finished) = Some(Arc::new(Finished {
                            buffer: rendered.buffer,
                            film: rendered.film,
                            outcome: rendered.outcome,
                        }))
                    }
                    Err(payload) => *lock(&running.panicked) = Some(panic_message(&*payload)),
                }
            })
            .map_err(|e| Error::InvalidArgument(format!("cannot start the render thread: {e}")))?;
        self.render = Some(thread);
        Ok(())
    }
}

/// The layers an import composed, less the owned ones: the session's sources.
fn sources_of(layers: &[PathBuf], owned: &Owned, before: &Sources) -> Sources {
    let foreign: Vec<PathBuf> = layers
        .iter()
        .filter(|l| {
            !owned
                .layers
                .iter()
                .any(|(o, _)| super::layer::same_file(o, l))
        })
        .cloned()
        .collect();
    Sources::of(&foreign, &owned.root, before)
}

/// The root and every layer it reaches through `subLayers` with no offset,
/// read as they are on disk now. A layer that does not parse ends its branch.
fn root_layer_stack(root: &Path) -> Vec<PathBuf> {
    let mut stack = vec![root.to_owned()];
    let mut i = 0;
    while i < stack.len() {
        let layer = stack[i].clone();
        i += 1;
        let Some(data) = std::fs::read(&layer).ok().and_then(|bytes| parse(&bytes)) else {
            continue;
        };
        let pseudo_root = sdf::Path::abs_root();
        let field = |name: &str| {
            data.try_field(&pseudo_root, name)
                .ok()
                .flatten()
                .map(|v| v.into_owned())
        };
        let names: Vec<String> = match field("subLayers") {
            Some(Value::StringVec(v)) => v,
            _ => Vec::new(),
        };
        let offsets = match field("subLayerOffsets") {
            Some(Value::LayerOffsetVec(v)) => v,
            _ => Vec::new(),
        };
        let dir = layer.parent().unwrap_or(Path::new("."));
        for (n, name) in names.iter().enumerate() {
            let identity = offsets
                .get(n)
                .is_none_or(|o| *o == sdf::LayerOffset::IDENTITY);
            let path = dir.join(name);
            if identity && !stack.iter().any(|s| super::layer::same_file(s, &path)) {
                stack.push(path);
            }
        }
    }
    stack
}

/// A USDA layer's bytes as data; `None` for any other format, which ends
/// [`root_layer_stack`]'s branch there (a binary layer is never owned).
fn parse(bytes: &[u8]) -> Option<sdf::Data> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| openusd::usda::parse(text).ok())
}

/// The class of the changes from each layer's bytes `before` to its bytes
/// `after`, and what they rebuild, read against `stage` (the root's stage, as
/// saved) and the scene's `provenance`; `None` when they need a full import.
/// Every layer must be in the root layer stack with no offset, so its paths
/// are the stage's. Only USDA layers are diffed: a changed layer in another
/// format is `full`.
pub fn classify_layers(
    layers: &[(&[u8], &[u8])],
    stage: &Stage,
    provenance: &crust_core::live::Provenance,
) -> Option<(Applied, crust_core::live::Change)> {
    let mut changes = Vec::new();
    for (before, after) in layers {
        if before == after {
            continue;
        }
        let (Some(before), Some(after)) = (parse(before), parse(after)) else {
            debug!("Edit applied as full: a changed layer is not USDA");
            return None;
        };
        changes.extend(layer_diff(&before, &after));
    }
    let (applied, change, full) = classify(&changes, stage, provenance);
    if let Some(why) = full {
        debug!("Edit applied as full: {why}");
        return None;
    }
    Some((applied, change))
}

/// The session-wide generation of a render's `published`-th image: a render
/// publishes fewer than 2^32 times, and its id keeps the count growing
/// across restarts.
fn generation(render: u64, published: u64) -> u64 {
    (render << 32) | published.min(u64::from(u32::MAX))
}

/// The bytes of the layer at `path`.
fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|e| Error::ImportFailed(format!("{}: {e}", path.display())))
}
