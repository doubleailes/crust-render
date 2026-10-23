//! The delegate's session (design D5): the override layer `hdCrust` writes,
//! followed as `crust mcp` follows its own (`crust_render::session`), and the
//! render of it that the viewport polls.
//!
//! Two threads. The session thread owns everything that cannot cross threads
//! — openusd's `Stage` is built on `Rc` — and runs the commands (`load`,
//! `apply`, `cancel`) one at a time, in the order they were sent: each
//! cancels and joins the running render before it touches the scene, and
//! starts a fresh one after. The render thread renders. What the viewport
//! polls (`snapshot`, `converged`, `aov`) reads the running render's shared
//! state directly, so a poll never waits behind an import.

use crust_assets::FileAssets;
use crust_core::{AovFilm, AovRequest, Buffer, RenderControl, RenderOutcome};
use crust_render::Assets;
use crust_render::session::delta::Applied;
use crust_render::session::scene::Imported;
use crust_render::session::{classify_saved, import_kept, loader, open_stage, update_or_import};
use openusd::usd::Stage;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use tracing::debug;

/// Why a call did not do what it was asked; each is one C ABI status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not yet: no image has been published, or the render has not
    /// completed, so there is nothing to read.
    Busy(String),
    /// The layer could not be opened or imported. The last image stays.
    ImportFailed(String),
    /// crust panicked. The scene is dropped; the next `apply` imports again.
    Panicked(String),
    /// The call itself was wrong: a missing AOV, nothing loaded yet.
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
    pub width: usize,
    pub height: usize,
    /// `width` × `height` RGBA, rows top-down (image space), alpha 1.
    pub rgba: Vec<f32>,
    /// The samples every pixel has taken, as the render's first sweep
    /// reaches them (1, 2, 4, …): a lower bound once adaptive sampling runs.
    pub spp: u32,
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

/// What a render left when it returned.
struct Finished {
    buffer: Buffer,
    film: Option<AovFilm>,
    outcome: RenderOutcome,
}

/// A render the viewport polls.
struct Running {
    /// Which render of the session this is, from 1.
    id: u64,
    control: RenderControl,
    /// The products the scene asks for: the AOVs a completed render holds.
    aovs: Arc<AovRequest>,
    finished: Mutex<Option<Arc<Finished>>>,
    /// The message of a panic that ended the render.
    panicked: Mutex<Option<String>>,
}

impl Running {
    /// `Panicked` when a panic ended the render.
    fn check(&self) -> Result<(), Error> {
        match lock(&self.panicked).clone() {
            Some(message) => Err(Error::Panicked(message)),
            None => Ok(()),
        }
    }
}

/// What the session thread and the polls share.
#[derive(Default)]
struct Shared {
    /// The latest render started; kept after it is cancelled or the scene
    /// fails to import, so the viewport keeps its last image.
    running: Mutex<Option<Arc<Running>>>,
    /// How the last change reached the scene.
    applied: Mutex<Option<Applied>>,
}

/// Locks `m`, through a poisoned lock: what it guards is replaced whole, never
/// left half-written.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the session thread owns.
struct State {
    shared: Arc<Shared>,
    /// The session's id for the log ([`crate::log`]).
    id: u64,
    /// The override layer, once `load` named it.
    layer: Option<PathBuf>,
    /// The authoring stage the classifier reads, opened on the saved layer.
    stage: Option<Stage>,
    /// The last import (or in-place update) of the layer.
    scene: Option<Imported>,
    /// The layer's bytes when the scene last caught up with it.
    before: Vec<u8>,
    /// The loader every import goes through, keeping its decodes; `None`
    /// under `CRUST_SCENE_DELTAS=0`, where each import gets a fresh one and
    /// every change is a full import.
    assets: Option<Arc<dyn Assets>>,
    render: Option<JoinHandle<()>>,
    renders: u64,
}

type Command = Box<dyn FnOnce(&mut State) + Send>;

/// A session on the layers in one directory. Dropping it cancels the render
/// and joins both threads.
pub struct Session {
    /// The id the session's threads log under ([`crate::log`]).
    id: u64,
    dir: PathBuf,
    shared: Arc<Shared>,
    commands: Option<mpsc::Sender<Command>>,
    thread: Option<JoinHandle<()>>,
}

impl Session {
    /// A session whose layers live in `dir`, created if missing. Nothing is
    /// imported until [`Session::load`].
    pub fn new(dir: impl Into<PathBuf>) -> Result<Session, Error> {
        let assets = crust_core::config()
            .scene_deltas
            .then(|| Arc::new(FileAssets::new().keep_decoded()) as Arc<dyn Assets>);
        Session::start(dir.into(), assets)
    }

    /// A session whose every import goes through `assets`, kept across
    /// imports: how the tests bring a loader of their own.
    #[cfg(test)]
    pub(crate) fn with_assets(
        dir: impl Into<PathBuf>,
        assets: Arc<dyn Assets>,
    ) -> Result<Session, Error> {
        Session::start(dir.into(), Some(assets))
    }

    /// A session on `dir`, importing through `assets` (`None`: a fresh
    /// loader per import, every change a full one).
    fn start(dir: PathBuf, assets: Option<Arc<dyn Assets>>) -> Result<Session, Error> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::InvalidArgument(format!("{}: {e}", dir.display())))?;
        let shared = Arc::new(Shared::default());
        let (commands, queue) = mpsc::channel::<Command>();
        let ours = shared.clone();
        let id = crate::log::next_session();
        let thread = std::thread::Builder::new()
            .name("crust-hydra-session".into())
            .spawn(move || {
                crate::log::enter(id);
                // Built here: the stage it will hold never leaves this thread.
                let mut state = State {
                    shared: ours,
                    id,
                    layer: None,
                    stage: None,
                    scene: None,
                    before: Vec::new(),
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
        Ok(Session {
            id,
            dir,
            shared,
            commands: Some(commands),
            thread: Some(thread),
        })
    }

    /// The directory the layers live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The id the session's threads log under ([`crate::log`]).
    pub(crate) fn log_id(&self) -> u64 {
        self.id
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
                        Err(Error::Panicked(crate::ffi::panic_message(&*payload)))
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

    /// Imports the override layer at `layer` whole, as `crust render -i
    /// <layer>` would, and starts rendering it. What a first population and
    /// a time change call.
    pub fn load(&self, layer: &Path) -> Result<(), Error> {
        let layer = layer.to_owned();
        self.call(move |state| state.load(&layer))
    }

    /// The saved layer changed: applies the change in place when the
    /// session's classes allow it, else imports the layer, and restarts the
    /// render. With nothing loaded, or after a failure dropped the scene, it
    /// imports the layer `load` named.
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

    /// How the last `load` or `apply` reached the scene: `full` for an
    /// import, else the class it was applied in place as.
    pub fn applied(&self) -> Option<Applied> {
        *lock(&self.shared.applied)
    }

    fn running(&self) -> Option<Arc<Running>> {
        lock(&self.shared.running).clone()
    }

    /// The generation [`Session::snapshot`] would return now, without
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
        let (published, buffer) = match finished {
            // One past the last publish: the final image, which is the one
            // `crust render` writes.
            Some(f) => (running.control.generation() + 1, f.buffer.clone()),
            None => running
                .control
                .snapshot()
                .ok_or_else(|| Error::Busy("the render has published no image yet".into()))?,
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
            width,
            height,
            rgba,
            spp,
        })
    }

    /// Whether the render returned complete: what Hydra's `IsConverged`
    /// reports. A cancelled render never converges.
    pub fn converged(&self) -> bool {
        self.running()
            .and_then(|r| lock(&r.finished).clone())
            .is_some_and(|f| f.outcome == RenderOutcome::Completed)
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

impl Drop for Session {
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
    /// half-updated, and the next `apply` imports the layer again.
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

    fn load(&mut self, layer: &Path) -> Result<(), Error> {
        self.stop();
        self.layer = Some(layer.to_owned());
        // Whatever happens next, the old scene is not this layer's.
        self.scene = None;
        self.stage = None;
        let stage = open_stage(layer).map_err(Error::ImportFailed)?;
        let before = read(layer)?;
        let (assets, _) = loader(self.assets.as_ref());
        let scene = import_kept(layer, None, assets).map_err(Error::ImportFailed)?;
        self.stage = Some(stage);
        self.before = before;
        self.scene = Some(scene);
        *lock(&self.shared.applied) = Some(Applied::Full);
        self.report();
        self.start()
    }

    fn apply(&mut self) -> Result<(), Error> {
        let layer = self
            .layer
            .clone()
            .ok_or_else(|| Error::InvalidArgument("no layer is loaded".into()))?;
        if self.render_panicked() {
            debug!("The last render panicked: the layer is imported again");
            self.forget();
        }
        if self.scene.is_none() {
            return self.load(&layer);
        }
        self.stop();
        // The stage the change is classified against is the layer as saved.
        let stage = open_stage(&layer).map_err(Error::ImportFailed)?;
        let after = read(&layer)?;
        let (assets, changed_on_disk) = loader(self.assets.as_ref());
        let scene = self.scene.as_mut().expect("checked above");
        let change = if changed_on_disk > 0 || self.assets.is_none() {
            debug!("Change applied as full: decodes changed on disk, or scene deltas are off");
            None
        } else {
            scene
                .provenance()
                .and_then(|p| classify_saved(&self.before, &layer, &stage, p))
        };
        update_or_import(scene, &layer, None, change, assets).map_err(Error::ImportFailed)?;
        *lock(&self.shared.applied) = Some(scene.applied);
        self.stage = Some(stage);
        self.before = after;
        self.report();
        self.start()
    }

    /// Sends the scene's import warnings to the host, once per import.
    fn report(&self) {
        if let Some(scene) = &self.scene {
            crate::log::forward_warnings(&scene.report.warnings);
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
            finished: Mutex::new(None),
            panicked: Mutex::new(None),
        });
        *lock(&self.shared.running) = Some(running.clone());
        let renderer = scene.renderer.clone();
        let session = self.id;
        let thread = std::thread::Builder::new()
            .name(format!("crust-hydra-render-{}", running.id))
            .spawn(move || {
                crate::log::enter(session);
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
                    Err(payload) => {
                        *lock(&running.panicked) = Some(crate::ffi::panic_message(&*payload))
                    }
                }
            })
            .map_err(|e| Error::InvalidArgument(format!("cannot start the render thread: {e}")))?;
        self.render = Some(thread);
        Ok(())
    }
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
