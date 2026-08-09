//! The prototype geometry cache — what makes Hydra edits cheap.
//!
//! A committed inner `rt::Scene` (the triangles and their BVH) is immutable
//! and shareable, so it can outlive any number of top-level scene rebuilds:
//! the host keys each mesh by an opaque `u64` (hdCrust uses the prim path's
//! hash) plus a content version it bumps when the geometry itself changes.
//! A rebuild whose meshes all hit the cache pays only the top-level BVH
//! over instance boxes — never a triangle re-upload or an inner build.

use crust_core::rt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct CachedProto {
    version: u32,
    scene: Arc<rt::Scene>,
}

/// The C `CrustGeoCache`. Mutex-guarded (like the stop token's atomic): the
/// cache outlives individual scene builds and may be shared, so unlike the
/// builder/renderer handles it is safe to touch from any thread.
pub struct GeoCacheHandle {
    entries: Mutex<HashMap<u64, CachedProto>>,
}

impl GeoCacheHandle {
    fn new() -> Self {
        GeoCacheHandle {
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn lookup(&self, key: u64, version: u32) -> Option<Arc<rt::Scene>> {
        let entries = self.entries.lock().expect("geo cache poisoned");
        entries
            .get(&key)
            .filter(|c| c.version == version)
            .map(|c| c.scene.clone())
    }

    /// Inserts (or replaces — a stale version is dead weight) an entry.
    pub(crate) fn insert(&self, key: u64, version: u32, scene: Arc<rt::Scene>) {
        let mut entries = self.entries.lock().expect("geo cache poisoned");
        entries.insert(key, CachedProto { version, scene });
    }
}

/// `CrustGeoCache* crust_geo_cache_create(void);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_geo_cache_create() -> *mut GeoCacheHandle {
    Box::into_raw(Box::new(GeoCacheHandle::new()))
}

/// `bool crust_geo_cache_contains(const CrustGeoCache*, uint64_t key,
///     uint32_t version);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_geo_cache_contains(
    cache: *const GeoCacheHandle,
    key: u64,
    version: u32,
) -> bool {
    // SAFETY: non-null checked; validity until destroy is the header
    // contract, and the cache is internally synchronized.
    unsafe { cache.as_ref() }.is_some_and(|c| c.lookup(key, version).is_some())
}

/// `void crust_geo_cache_remove(CrustGeoCache*, uint64_t key);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_geo_cache_remove(cache: *mut GeoCacheHandle, key: u64) {
    // SAFETY: as `crust_geo_cache_contains`.
    if let Some(cache) = unsafe { cache.as_ref() } {
        cache
            .entries
            .lock()
            .expect("geo cache poisoned")
            .remove(&key);
    }
}

/// `void crust_geo_cache_clear(CrustGeoCache*);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_geo_cache_clear(cache: *mut GeoCacheHandle) {
    // SAFETY: as `crust_geo_cache_contains`.
    if let Some(cache) = unsafe { cache.as_ref() } {
        cache
            .entries
            .lock()
            .expect("geo cache poisoned")
            .clear();
    }
}

/// `void crust_geo_cache_destroy(CrustGeoCache*);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_geo_cache_destroy(cache: *mut GeoCacheHandle) {
    if !cache.is_null() {
        // SAFETY: created by `crust_geo_cache_create`; use after destroy is
        // forbidden by the header contract. Prototypes still referenced by
        // live renderers survive — they are `Arc`s.
        drop(unsafe { Box::from_raw(cache) });
    }
}
