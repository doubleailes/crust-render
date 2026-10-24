//! The delegate's session (design D5): the override layer `hdCrust` writes,
//! followed as `crust mcp` follows its own, and the render of it that the
//! viewport polls. The threads, the classification and the render are
//! `crust_render::session::live`'s, shared with other hosts; this wraps it
//! with the delegate's log routing and its session directory.

use crust_render::session::delta::Applied;
use crust_render::session::live::{Hooks, LiveSession};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use crust_render::session::live::{Aov, Error, Snapshot};

/// A session on the layers in one directory. Dropping it cancels the render
/// and joins both threads.
pub struct Session {
    /// The id the session's threads log under ([`crate::log`]).
    id: u64,
    dir: PathBuf,
    inner: LiveSession,
}

impl Session {
    /// A session whose layers live in `dir`, created if missing. Nothing is
    /// imported until [`Session::load`].
    pub fn new(dir: impl Into<PathBuf>) -> Result<Session, Error> {
        Session::start(dir.into(), LiveSession::new)
    }

    /// A session whose every import goes through `assets`, kept across
    /// imports: how the tests bring a loader of their own.
    #[cfg(test)]
    pub(crate) fn with_assets(
        dir: impl Into<PathBuf>,
        assets: Arc<dyn crust_render::Assets>,
    ) -> Result<Session, Error> {
        Session::start(dir.into(), |hooks| LiveSession::with_assets(hooks, assets))
    }

    fn start(
        dir: PathBuf,
        session: impl FnOnce(Hooks) -> Result<LiveSession, Error>,
    ) -> Result<Session, Error> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::InvalidArgument(format!("{}: {e}", dir.display())))?;
        let id = crate::log::next_session();
        let hooks = Hooks {
            on_thread: Some(Arc::new(move || crate::log::enter(id))),
            on_warnings: Some(Arc::new(crate::log::forward_warnings)),
        };
        Ok(Session {
            id,
            dir,
            inner: session(hooks)?,
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

    /// Imports the override layer at `layer` whole, as `crust render -i
    /// <layer>` would, and starts rendering it. What a first population and
    /// a time change call.
    pub fn load(&self, layer: &Path) -> Result<(), Error> {
        self.inner.load(layer)
    }

    /// The saved layer changed: applies the change in place when the
    /// session's classes allow it, else imports the layer, and restarts the
    /// render. With nothing loaded, or after a failure dropped the scene, it
    /// imports the layer `load` named.
    pub fn apply(&self) -> Result<(), Error> {
        self.inner.apply()
    }

    /// Cancels the running render and waits for it to stop. The last image
    /// stays.
    pub fn cancel(&self) {
        self.inner.cancel()
    }

    /// How the last `load` or `apply` reached the scene: `full` for an
    /// import, else the class it was applied in place as.
    pub fn applied(&self) -> Option<Applied> {
        self.inner.applied()
    }

    /// The generation [`Session::snapshot`] would return now, without
    /// copying the image: what a poll compares before it copies.
    pub fn generation(&self) -> Result<u64, Error> {
        self.inner.generation()
    }

    /// The latest image of the render: its final image once it returned,
    /// else the latest it published. `Busy` before the first.
    pub fn snapshot(&self) -> Result<Snapshot, Error> {
        self.inner.snapshot()
    }

    /// Whether the render returned complete: what Hydra's `IsConverged`
    /// reports. A cancelled render never converges. One a panic ended has
    /// stopped for good, and counts as converged so the host stops redrawing
    /// for it; the viewport keeps its last image.
    pub fn converged(&self) -> bool {
        self.inner.converged()
    }

    /// The AOV named `name` (a RenderVar's layer name, as the products
    /// write it) of the completed render. `Busy` until the render completes.
    pub fn aov(&self, name: &str) -> Result<Aov, Error> {
        self.inner.aov(name)
    }
}
