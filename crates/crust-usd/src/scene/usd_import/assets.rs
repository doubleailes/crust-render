//! Asset paths and host decodes: the one rule that turns an `asset`-valued
//! attribute into a filesystem path ([`asset_path`]), and the one memoized,
//! timed route every decode takes through the host's
//! [`AssetLoader`](crate::scene::AssetLoader) ([`cached_asset`]).
//! crust-core decodes nothing itself.

use crate::record_warning;
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use openusd::sdf;
use openusd::usd::Attribute;
use tracing::debug;

use super::ImportCaches;
use super::attrs::value_at;

/// Runs one host decode, adding its wall time to `asset_time` — the "Load
/// assets" phase, which the traversal figure has subtracted out (see
/// `ImportCaches::asset_time`).
pub(super) fn timed_asset<V>(asset_time: &mut Duration, load: impl FnOnce() -> V) -> V {
    let started = Instant::now();
    let loaded = load();
    *asset_time += started.elapsed();
    loaded
}

/// Runs one host load, noting `path` in `failed` when it comes back `None`
/// with a cause the host explained ([`cause_warning!`](crate::cause_warning))
/// — a file it could not read. A `None` without one is the host declining on
/// purpose (`CRUST_TEX=0`, an asset it does not decode, which it reports
/// itself): the reference renders its fallback, but it is not an unreadable
/// file.
pub(super) fn explained<V>(
    failed: &mut HashSet<PathBuf>,
    path: &Path,
    load: impl FnOnce() -> Option<V>,
) -> Option<V> {
    let causes = crate::warnings::causes_raised();
    let loaded = load();
    if loaded.is_none() && crate::warnings::causes_raised() != causes {
        failed.insert(path.to_owned());
    }
    loaded
}

/// One host decode, memoized by `key` and timed by [`timed_asset`].
///
/// Negative results are cached too: a file that failed to open (a missing
/// texture, a 600 MB Ptex the host declined) is not retried per material or
/// light. `load` runs only on a miss, so whatever it reports is reported once
/// per key.
pub(super) fn cached_asset<K: Eq + Hash, V: Clone>(
    cache: &mut HashMap<K, Option<V>>,
    asset_time: &mut Duration,
    key: K,
    load: impl FnOnce(&K) -> Option<V>,
) -> Option<V> {
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let loaded = timed_asset(asset_time, || load(&key));
    cache.insert(key, loaded.clone());
    loaded
}

/// Opens a UV texture through the host, memoized by resolved path and colour
/// space. The caller maps its own vocabulary onto the space — MaterialX's
/// `colorspace` through [`crate::ColorSpace::from_mtlx`], UsdUVTexture's
/// `sourceColorSpace` through [`crate::ColorSpace::from_usd`] — since the two
/// disagree on what an absent attribute means.
///
/// `prim` is the material referencing the texture. Every reference to a file
/// the host failed to read ([`explained`]), a cache hit included, counts one
/// `texture.unreadable` on it; the host explained the cause once, when the
/// file failed.
pub(super) fn load_uv_texture(
    path: &Path,
    space: crate::ColorSpace,
    prim: &dyn Display,
    caches: &mut ImportCaches<'_>,
) -> Option<Arc<dyn crate::Texture2D>> {
    let key = (path.to_string_lossy().into_owned(), space);
    let assets = caches.assets;
    let loaded = cached_asset(
        &mut caches.materials.textures,
        &mut caches.asset_time,
        key,
        |_| {
            let loaded = explained(&mut caches.failed_assets, path, || {
                assets.load_texture(path, space)
            });
            if loaded.is_none() {
                debug!(
                    "Texture {} ({space:?}) not loadable by the host",
                    path.display()
                );
            }
            loaded
        },
    );
    if loaded.is_none() && caches.failed_assets.contains(path) {
        unreadable(prim, path);
    }
    loaded
}

/// Counts one reference to a texture the host could not load.
fn unreadable(prim: &dyn Display, path: &Path) {
    record_warning!(
        TextureUnreadable,
        at = prim,
        "{prim}: texture {} could not be loaded — the input reads its fallback",
        path.display()
    );
}

/// Opens a Ptex file through the host, once per `(resolved path, space)`.
///
/// Keyed on the resolved filesystem path, which — unlike a prototype-scoped
/// scene path — is stable across the streaming importer's stages, so one
/// texture is opened once however many materials or chunks reference it.
/// The space is in the key because the decode happens at open: a file read
/// both as colour and as displacement is two textures, which is correct and
/// rare. `prim` is counted as [`load_uv_texture`] counts it.
pub(super) fn load_ptex(
    path: &Path,
    space: crate::ColorSpace,
    prim: &dyn Display,
    caches: &mut ImportCaches<'_>,
) -> Option<Arc<dyn crate::PtexTexture>> {
    let key = (path.to_string_lossy().into_owned(), space);
    let assets = caches.assets;
    let loaded = cached_asset(
        &mut caches.materials.ptex,
        &mut caches.asset_time,
        key,
        |_| {
            explained(&mut caches.failed_assets, path, || {
                assets.load_ptex(path, space)
            })
        },
    );
    if loaded.is_none() && caches.failed_assets.contains(path) {
        unreadable(prim, path);
    }
    loaded
}

/// An `asset`-valued attribute as a filesystem path — the one rule every
/// asset reference (textures, Ptex, IES profiles, light and dome maps) is
/// resolved by.
///
/// openusd anchors default-sourced asset paths against the layer that
/// authored them and reports the result in `resolved_path` — which is what
/// makes a production stage's `../../../textures/foo.ptx` work at all, since
/// the layer authoring it is nested several directories below the root (the
/// Moana island's lights author `../textures/islandsun.exr` relative to
/// `usd/island.usda`).
///
/// But openusd reports the anchored path only when it names a file that
/// exists — and a `<UDIM>`-tokened texture path never does, since it names a
/// set. So such a value arrives with no `resolved_path` at all, and an
/// **unresolved** relative path is anchored here against the layer that
/// authored it: the strongest spec in the attribute's property stack, the
/// layer whose opinion supplies the value, which is exactly what USD anchors
/// against. Anchoring it against the root layer was wrong for any texture
/// authored in a sublayer or reference: ALab's look layers sit five
/// directories below `entry.usda` and author `@../../texture/…<UDIM>.exr@`,
/// and 1 718 texture sets failed to load. The root layer (`stage_path`) is
/// only the last resort, when the authoring layer is unknown.
pub(super) fn asset_path(attr: &Attribute, stage_path: &Path) -> Option<PathBuf> {
    let value = value_at(attr)?;
    let resolved = matches!(&value, sdf::Value::AssetPath(p)
        if p.resolved_path().is_some_and(|r| !r.is_empty()));
    let authored = value.as_str().map(str::to_owned);
    if !resolved
        && let Some(authored) = authored.filter(|a| !a.is_empty() && Path::new(a).is_relative())
        && let Some(layer_dir) = attr
            .property_stack()
            .ok()
            .and_then(|stack| stack.into_iter().next())
            .and_then(|site| Path::new(&site.layer).parent().map(Path::to_path_buf))
            .filter(|d| !d.as_os_str().is_empty())
    {
        return Some(layer_dir.join(authored));
    }
    asset_value_path(&value, stage_path)
}

/// An asset value as a path: openusd's `resolved_path` when it has one, else
/// the authored string, a relative one anchored against the root layer.
fn asset_value_path(value: &sdf::Value, stage_path: &Path) -> Option<PathBuf> {
    let (authored, resolved) = match value {
        sdf::Value::AssetPath(p) => (p.as_str().to_string(), p.resolved_path()),
        sdf::Value::String(p) => (p.clone(), None),
        _ => return None,
    };
    if let Some(r) = resolved
        && !r.is_empty()
    {
        return Some(PathBuf::from(r));
    }
    if authored.is_empty() {
        return None;
    }
    let candidate = Path::new(&authored);
    if candidate.is_absolute() {
        return Some(candidate.to_path_buf());
    }
    Some(
        stage_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(candidate),
    )
}
