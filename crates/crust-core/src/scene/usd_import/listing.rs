//! Reading what a stage holds without importing it (`crust ls`): its cameras,
//! lights or materials.
//!
//! The walk is the render's own — the same streaming chunks, the same
//! [`prune_reason`], the same prims it does not enter — so what is listed for
//! a kind is what the import would use of that kind, and nothing it would
//! never meet. Nothing but the listed schema is read: no geometry is built,
//! no material resolved, no texture or light emission decoded.

use std::path::Path;
use std::time::Instant;

use openusd::sdf;
use openusd::usd::{InitialLoadSet, Prim, Stage};
use openusd_schemas::geom::{Camera as UsdCamera, PointInstancer};
use openusd_schemas::lux::{
    CylinderLight, DiskLight, DistantLight, DomeLight, RectLight, SphereLight,
};
use openusd_schemas::shade::Material as UsdMaterial;
use tracing::debug;

use super::{Prune, WalkScope, open_stage, prim_at, prune_reason, release_stage, stream_roots};
use crate::scene::ListKind;

/// Every `kind` prim on the stage at `path`, in namespace order (a parent's
/// children in their authored order), as absolute prim paths.
pub(crate) fn list_prims(path: &Path, kind: ListKind) -> Result<Vec<String>, crate::Error> {
    let started = Instant::now();
    let path_str = path
        .to_str()
        .ok_or_else(|| crate::Error::NonUtf8Path(path.to_path_buf()))?;
    // As the import does: the payload-free index decides the chunks, and each
    // chunk is opened with its payloads, since a shot camera or a set's
    // lights under a payload is the production case.
    let index = Stage::builder()
        .load(InitialLoadSet::LoadNone)
        .open(path_str)
        .map_err(|e| crate::Error::UsdOpen {
            path: path.to_path_buf(),
            source: e.into(),
        })?;
    let chunks = stream_roots(&index);
    drop(index);

    let mut prims = Vec::new();
    if chunks.is_empty() {
        let stage = open_stage(path, path_str, None)?;
        collect(&stage, kind, &mut prims);
        release_stage(stage, false);
    } else {
        for chunk in chunks {
            let stage = open_stage(path, path_str, Some(chunk))?;
            collect(&stage, kind, &mut prims);
            release_stage(stage, false);
        }
    }
    debug!(
        "Found {} {kind:?} prim(s) on {} in {:?}",
        prims.len(),
        path.display(),
        started.elapsed()
    );
    Ok(prims)
}

/// What a walk does with one prim.
enum Visit {
    /// Neither the prim nor anything below it.
    Prune,
    /// The prim and its subtree; `hidden` is true under an invisible ancestor.
    Enter { hidden: bool },
}

/// The `kind` prims a walk of `stage` meets, appended to `out`.
fn collect(stage: &Stage, kind: ListKind, out: &mut Vec<String>) {
    let mut stack: Vec<(Prim, bool)> = vec![(prim_at(stage, sdf::Path::abs_root()), false)];
    while let Some((prim, parent_hidden)) = stack.pop() {
        let Visit::Enter { hidden } = visit(stage, &prim, kind, parent_hidden) else {
            continue;
        };
        if is_kind(stage, &prim, kind) {
            out.push(prim.path().to_string());
        }
        if let Ok(children) = prim.children() {
            // Reversed onto the stack, so they pop in authored order.
            stack.extend(children.into_iter().rev().map(|c| (c, hidden)));
        }
    }
}

/// Whether the walk for `kind` enters `prim`, following the import:
///
/// - **cameras** — [`super::traverse_into`]'s rules: every [`prune_reason`]
///   but invisibility prunes; an invisible subtree is still walked, as the
///   render walks it for cameras; an instance with a prototype (and every
///   instance under an invisible ancestor) and a visible `PointInstancer`,
///   whose children are prototypes, are not entered.
/// - **lights** — the same, except that invisibility prunes too: an
///   invisible light lights nothing. A prototype's lights are never imported.
/// - **materials** — every Material prim a binding can reach: only an
///   inactive subtree, which USD removes from the stage, prunes, and an
///   instance's prototype is not entered (its materials are the prototype's,
///   bound only from inside it).
fn visit(stage: &Stage, prim: &Prim, kind: ListKind, parent_hidden: bool) -> Visit {
    let pruned = prune_reason(prim, WalkScope::Stage);
    let is_instance = prim.is_instance().unwrap_or(false);
    let has_prototype = || matches!(prim.prototype(), Ok(Some(_)));
    let is_instancer = || {
        PointInstancer::get(stage, prim.path().clone())
            .ok()
            .flatten()
            .is_some()
    };
    match kind {
        ListKind::Camera => {
            if pruned.is_some_and(|r| r != Prune::Invisible) {
                return Visit::Prune;
            }
            let hidden = parent_hidden || pruned == Some(Prune::Invisible);
            if is_instance && (hidden || has_prototype()) || !hidden && is_instancer() {
                return Visit::Prune;
            }
            Visit::Enter { hidden }
        }
        ListKind::Light => {
            if pruned.is_some() || is_instance && has_prototype() || is_instancer() {
                return Visit::Prune;
            }
            Visit::Enter { hidden: false }
        }
        ListKind::Material => {
            if pruned == Some(Prune::Inactive) || is_instance && has_prototype() {
                return Visit::Prune;
            }
            Visit::Enter { hidden: false }
        }
    }
}

/// Whether `prim` is of `kind`: a `UsdGeomCamera`; one of the six UsdLux
/// lights the import reads; a `UsdShadeMaterial`.
fn is_kind(stage: &Stage, prim: &Prim, kind: ListKind) -> bool {
    let path = || prim.path().clone();
    match kind {
        ListKind::Camera => matches!(UsdCamera::get(stage, path()), Ok(Some(_))),
        ListKind::Light => {
            matches!(SphereLight::get(stage, path()), Ok(Some(_)))
                || matches!(RectLight::get(stage, path()), Ok(Some(_)))
                || matches!(DiskLight::get(stage, path()), Ok(Some(_)))
                || matches!(CylinderLight::get(stage, path()), Ok(Some(_)))
                || matches!(DistantLight::get(stage, path()), Ok(Some(_)))
                || matches!(DomeLight::get(stage, path()), Ok(Some(_)))
        }
        ListKind::Material => matches!(UsdMaterial::get(stage, path()), Ok(Some(_))),
    }
}
