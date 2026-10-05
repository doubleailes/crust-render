//! Reading what a stage holds without importing it: the cameras a render
//! could go through (`crust ls camera`).
//!
//! The walk is the render's own — the same streaming chunks, the same
//! [`prune_reason`], the same refusal to take a camera from a prototype — so
//! a path listed here is one `UsdImportOptions::camera` accepts, and a camera
//! the render would never meet is not listed. Nothing but the camera schema is
//! read: no geometry, material or light is built.

use std::path::Path;
use std::time::Instant;

use openusd::sdf;
use openusd::usd::{InitialLoadSet, Prim, Stage};
use openusd_schemas::geom::{Camera as UsdCamera, PointInstancer};
use tracing::debug;

use super::{Prune, WalkScope, open_stage, prim_at, prune_reason, release_stage, stream_roots};

/// Every camera prim on the stage at `path`, in namespace order (a parent's
/// children in their authored order), as absolute prim paths.
pub(crate) fn list_cameras(path: &Path) -> Result<Vec<String>, crate::Error> {
    let started = Instant::now();
    let path_str = path
        .to_str()
        .ok_or_else(|| crate::Error::NonUtf8Path(path.to_path_buf()))?;
    // As the import does: the payload-free index decides the chunks, and each
    // chunk is opened with its payloads, since a shot camera under a payload
    // is the production case.
    let index = Stage::builder()
        .load(InitialLoadSet::LoadNone)
        .open(path_str)
        .map_err(|e| crate::Error::UsdOpen {
            path: path.to_path_buf(),
            source: e.into(),
        })?;
    let chunks = stream_roots(&index);
    drop(index);

    let mut cameras = Vec::new();
    if chunks.is_empty() {
        let stage = open_stage(path, path_str, None)?;
        collect_cameras(&stage, &mut cameras);
        release_stage(stage, false);
    } else {
        for chunk in chunks {
            let stage = open_stage(path, path_str, Some(chunk))?;
            collect_cameras(&stage, &mut cameras);
            release_stage(stage, false);
        }
    }
    debug!(
        "Found {} camera(s) on {} in {:?}",
        cameras.len(),
        path.display(),
        started.elapsed()
    );
    Ok(cameras)
}

/// The cameras [`super::traverse_into`] would meet on `stage`, appended to
/// `out`. It prunes the same subtrees and stops at the same prims: an
/// instance with a prototype (and every instance under an invisible
/// ancestor), and a `PointInstancer`, whose children are prototypes. An
/// invisible subtree is still walked, as the render walks it for cameras.
fn collect_cameras(stage: &Stage, out: &mut Vec<String>) {
    let mut stack: Vec<(Prim, bool)> = vec![(prim_at(stage, sdf::Path::abs_root()), false)];
    while let Some((prim, parent_hidden)) = stack.pop() {
        let pruned = prune_reason(&prim, WalkScope::Stage);
        if pruned.is_some_and(|r| r != Prune::Invisible) {
            continue;
        }
        let hidden = parent_hidden || pruned == Some(Prune::Invisible);
        if prim.is_instance().unwrap_or(false)
            && (hidden || matches!(prim.prototype(), Ok(Some(_))))
        {
            continue;
        }
        if !hidden
            && PointInstancer::get(stage, prim.path().clone())
                .ok()
                .flatten()
                .is_some()
        {
            continue;
        }
        if UsdCamera::get(stage, prim.path().clone())
            .ok()
            .flatten()
            .is_some()
        {
            out.push(prim.path().to_string());
        }
        if let Ok(children) = prim.children() {
            // Reversed onto the stack, so they pop in authored order.
            stack.extend(children.into_iter().rev().map(|c| (c, hidden)));
        }
    }
}
