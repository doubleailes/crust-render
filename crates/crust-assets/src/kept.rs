//! What a long-lived [`FileAssets`](crate::FileAssets) keeps between imports
//! ([`FileAssets::keep_decoded`](crate::FileAssets::keep_decoded)): every
//! texture, Ptex, light texture and IES profile it decoded, with the length
//! and modification time of each file the decode read.
//!
//! An import memoizes its own decodes by path (`usd_import/assets.rs`), so
//! within one import this adds nothing. It exists for a host that imports the
//! same stage again and again — the `crust mcp` session, once per edit batch —
//! where a fresh loader per import re-decoded every texture on every edit.
//! [`Kept::revalidate`] drops the entries whose files changed on disk; the
//! host calls it before each import, and [`Kept::sweep`] after a full one, to
//! let go of what that import no longer asked for.
//!
//! Only successes are kept. A file that failed to load is tried again by the
//! next import, which then raises the same warnings a fresh loader would.
//! Environment maps are not kept: they cross the seam by value, so a kept one
//! would be copied on every hit.

use crust_core::{ColorSpace, IesProfile, LightTexture, PtexTexture, Texture2D};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// What was asked for: the seam's method, the path as the importer resolved
/// it, and the colour space where the method takes one.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Key {
    Texture(PathBuf, ColorSpace),
    Ptex(PathBuf, ColorSpace),
    LightTexture(PathBuf, ColorSpace),
    Ies(PathBuf),
}

impl Key {
    fn path(&self) -> &Path {
        match self {
            Key::Texture(p, _) | Key::Ptex(p, _) | Key::LightTexture(p, _) | Key::Ies(p) => p,
        }
    }
}

/// What the seam handed back.
#[derive(Clone)]
pub(crate) enum Value {
    /// A UV texture, with its resident bytes when it took the preload path
    /// (`None`: it streams), which the loader's preload counters count again
    /// on every hit.
    Texture {
        texture: Arc<dyn Texture2D>,
        preloaded: Option<u64>,
    },
    Ptex(Arc<dyn PtexTexture>),
    LightTexture(Arc<LightTexture>),
    Ies(Arc<IesProfile>),
}

/// A file's length and modification time, `None` when it does not exist: a
/// `.tx` that appears beside a texture later changes the stamp too.
type Stamp = (PathBuf, Option<(u64, SystemTime)>);

fn stamp(path: PathBuf) -> Stamp {
    let meta = std::fs::metadata(&path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)));
    (path, meta)
}

struct Entry {
    value: Value,
    stamps: Vec<Stamp>,
    /// The [`Kept::generation`] that last asked for it.
    used: u64,
}

/// The kept decodes.
#[derive(Default)]
pub(crate) struct Kept {
    entries: Mutex<HashMap<Key, Entry>>,
    /// Bumped by every [`Kept::revalidate`]: the import (or update) about to
    /// run. An entry asked for since is the generation's.
    generation: AtomicU64,
    /// Decodes that went to a file, hits excluded: what
    /// [`FileAssets::decodes`](crate::FileAssets::decodes) reports.
    decodes: AtomicU64,
}

impl Kept {
    pub(crate) fn get(&self, key: &Key) -> Option<Value> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.get_mut(key)?;
        entry.used = self.generation.load(Ordering::Relaxed);
        Some(entry.value.clone())
    }

    /// Keeps `value` under `key`, stamped with `files` as they are now. The
    /// stamps are taken after the decode, so a file rewritten *during* it is
    /// not seen as changed; the next rewrite is.
    pub(crate) fn insert(&self, key: Key, value: Value, files: Vec<PathBuf>) {
        let stamps = files.into_iter().map(stamp).collect();
        let used = self.generation.load(Ordering::Relaxed);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.insert(
            key,
            Entry {
                value,
                stamps,
                used,
            },
        );
    }

    pub(crate) fn count_decode(&self) {
        self.decodes.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn decodes(&self) -> u64 {
        self.decodes.load(Ordering::Relaxed)
    }

    /// Starts a generation, and drops every entry one of whose files changed
    /// length or modification time, or appeared or vanished, since it was
    /// kept. Returns the dropped keys.
    pub(crate) fn revalidate(&self) -> Vec<Key> {
        self.generation.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let stale: Vec<Key> = entries
            .iter()
            .filter(|(_, e)| e.stamps.iter().any(|s| stamp(s.0.clone()) != *s))
            .map(|(k, _)| k.clone())
            .collect();
        for key in &stale {
            entries.remove(key);
            tracing::debug!(
                "{} changed on disk since it was decoded: dropped",
                key.path().display()
            );
        }
        stale
    }

    /// Drops every entry nothing asked for since the last
    /// [`Kept::revalidate`]: after a full import, what its scene does not use.
    /// Returns the dropped keys.
    pub(crate) fn sweep(&self) -> Vec<Key> {
        let current = self.generation.load(Ordering::Relaxed);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let unused: Vec<Key> = entries
            .iter()
            .filter(|(_, e)| e.used < current)
            .map(|(k, _)| k.clone())
            .collect();
        for key in &unused {
            entries.remove(key);
        }
        unused
    }
}

/// The files a UV texture's decode may have read: each tile (or the one
/// image), and the `.tx` the streaming path looks for beside each, which
/// `sibling` names (Arnold's name for the binding).
/// A `<UDIM>` / `<UVTILE>` set also stamps its directory, whose modification
/// time moves when a tile is added or removed: the set is the tiles on disk,
/// and one that appears later must be read.
pub(crate) fn texture_stamp_files(
    path: &Path,
    sibling: impl Fn(&Path) -> Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut files = crate::texture_files(path);
    if files.is_empty() {
        files.push(path.to_path_buf());
    }
    let name = path.to_string_lossy();
    if (name.contains("<UDIM>") || name.contains("<UVTILE>"))
        && let Some(dir) = path.parent()
    {
        files.push(dir.to_path_buf());
    }
    let siblings: Vec<PathBuf> = files.iter().filter_map(|f| sibling(f)).collect();
    files.extend(siblings);
    files.sort();
    files.dedup();
    files
}
