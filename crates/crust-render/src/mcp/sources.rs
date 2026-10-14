//! The source layers on disk (`live-session-source-reload` D2): the layers
//! the session's last import composed, other than the override layer, each
//! stamped with its length and modification time as the kept decodes are
//! (`crust-assets/src/kept.rs`). A stamp that moved is a source another
//! program changed, which the session's scene and authoring stage no longer
//! show.

use super::layer::same_file;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A file's length and modification time, `None` when it does not exist.
type Meta = Option<(u64, SystemTime)>;

fn meta(path: &Path) -> Meta {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// The session's source layers, each with its stamp as last seen.
#[derive(Debug, Default)]
pub struct Sources {
    stamps: Vec<(PathBuf, Meta)>,
}

impl Sources {
    /// `layers` (an import's [`crust_core::Scene::layers`]) without the
    /// override layer `output`, each stamped as `before` saw it when it is
    /// there, else as it is now. `before` is [`Sources::now`] taken before
    /// the import: a layer written while the import read it then still
    /// differs from its stamp, and the next call sees the change.
    pub fn of(layers: &[PathBuf], output: &Path, before: &Sources) -> Sources {
        let stamps = layers
            .iter()
            .filter(|layer| !same_file(layer, output))
            .map(|layer| {
                let seen = before
                    .stamps
                    .iter()
                    .find(|(p, _)| p == layer)
                    .map_or_else(|| meta(layer), |(_, m)| *m);
                (layer.clone(), seen)
            })
            .collect();
        Sources { stamps }
    }

    /// The same layers, stamped as they are on disk now.
    pub fn now(&self) -> Sources {
        let stamps = self
            .stamps
            .iter()
            .map(|(path, _)| (path.clone(), meta(path)))
            .collect();
        Sources { stamps }
    }

    /// The layers whose stamp moved since they were last seen — rewritten,
    /// removed or created — re-stamped, so the same change is reported once.
    pub fn changed(&mut self) -> Vec<PathBuf> {
        let mut moved = Vec::new();
        for (path, seen) in &mut self.stamps {
            let now = meta(path);
            if now != *seen {
                *seen = now;
                moved.push(path.clone());
            }
        }
        moved
    }

    /// Feeds the layers' stamps as last seen into `hasher`: beside the
    /// override layer's bytes, which sources a render rendered (design D5).
    /// [`Sources::now`]'s hash is what is on disk now.
    pub fn hash(&self, hasher: &mut impl Hasher) {
        self.stamps.hash(hasher);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("crust_mcp_sources").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// Rewrites `path` with `text` and moves its modification time a
    /// second on, so the change is seen on a filesystem whose clock is coarse.
    fn rewrite(path: &Path, text: &str) {
        let before = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        std::fs::write(path, text).unwrap();
        if let Some(before) = before {
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(before + std::time::Duration::from_secs(1))
                .unwrap();
        }
    }

    #[test]
    fn a_rewritten_layer_is_reported_once_and_an_untouched_one_never() {
        let dir = work_dir("rewritten");
        let (output, sub, other) = (dir.join("out.usda"), dir.join("a.usda"), dir.join("b.usda"));
        for f in [&output, &sub, &other] {
            std::fs::write(f, "#usda 1.0\n").unwrap();
        }
        let mut sources = Sources::of(
            &[output.clone(), sub.clone(), other.clone()],
            &output,
            &Sources::default(),
        );
        assert!(sources.changed().is_empty());
        rewrite(&sub, "#usda 1.0\n");
        assert_eq!(sources.changed(), vec![sub.clone()]);
        assert!(sources.changed().is_empty(), "reported once");
        // The override layer is the session's own: never a source.
        rewrite(&output, "#usda 1.0\n# edited\n");
        assert!(sources.changed().is_empty());
        std::fs::remove_file(&other).unwrap();
        assert_eq!(sources.changed(), vec![other]);
    }

    #[test]
    fn the_hash_follows_the_files_on_disk() {
        let dir = work_dir("hash");
        let (output, sub) = (dir.join("out.usda"), dir.join("a.usda"));
        for f in [&output, &sub] {
            std::fs::write(f, "#usda 1.0\n").unwrap();
        }
        let sources = Sources::of(&[output.clone(), sub.clone()], &output, &Sources::default());
        let hash = |s: &Sources| {
            let mut h = std::hash::DefaultHasher::new();
            s.hash(&mut h);
            h.finish()
        };
        let seen = hash(&sources);
        assert_eq!(hash(&sources.now()), seen);
        rewrite(&sub, "#usda 1.0\n# changed\n");
        assert_ne!(hash(&sources.now()), seen);
        assert_eq!(hash(&sources), seen, "as seen, not as on disk");
    }

    /// A layer rewritten while an import read it keeps the stamp taken
    /// before the import, so the next look reports it.
    #[test]
    fn a_write_during_an_import_is_seen_afterwards() {
        let dir = work_dir("during");
        let (output, sub) = (dir.join("out.usda"), dir.join("a.usda"));
        for f in [&output, &sub] {
            std::fs::write(f, "#usda 1.0\n").unwrap();
        }
        let layers = [output.clone(), sub.clone()];
        let old = Sources::of(&layers, &output, &Sources::default());
        let before = old.now();
        rewrite(&sub, "#usda 1.0\n# written mid-import\n");
        let mut after = Sources::of(&layers, &output, &before);
        assert_eq!(after.changed(), vec![sub]);
    }
}
