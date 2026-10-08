//! Reading what a stage holds without importing it (`crust ls`): its cameras,
//! lights or materials.
//!
//! The walk is the render's own — the same streaming chunks, the same
//! [`prune_reason`], the same prims it does not enter — so what is listed for
//! a kind is what the import would use of that kind, and nothing it would
//! never meet. Nothing but the listed schema is read: no geometry is built,
//! no material resolved, no texture or light emission decoded — so a light
//! the import refuses for its evaluated values (a zero radius, a transform
//! that collapses it) is still listed; see [`ListKind::Light`].

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use openusd::sdf;
use openusd::usd::{InitialLoadSet, Prim, Stage};
use openusd_schemas::geom::{
    BasisCurves as UsdBasisCurves, Camera as UsdCamera, Mesh as UsdMesh, PointInstancer,
    Sphere as UsdSphere,
};
use openusd_schemas::lux::{
    CylinderLight, DiskLight, DistantLight, DomeLight, Light as UsdLight, RectLight, SphereLight,
};
use openusd_schemas::shade::Material as UsdMaterial;
use tracing::debug;

use super::camera::camera_lens;
use super::lights::{LightInputs, light_inputs};
use super::materials::{bound_material, surface_shader_id};
use super::products::import_render_products;
use super::settings::{CameraPick, import_render_settings, pick_camera, wanted_camera};
use super::time::EvalTimeScope;
use super::{Prune, WalkScope, open_stage, prim_at, prune_reason, release_stage, stream_roots};
use crate::scene::{CameraRecord, LightRecord, ListKind, ListRecord, MaterialRecord};
use crate::tracer::RenderSettings;

/// Every `kind` prim on the stage at `path`, in namespace order (a parent's
/// children in their authored order), as absolute prim paths.
pub(crate) fn list_prims(path: &Path, kind: ListKind) -> Result<Vec<String>, crate::Error> {
    let started = Instant::now();
    let path_str = utf8(path)?;
    let chunks = stream_roots(&open_index(path, path_str)?);
    let mut found = Found::default();
    for_each_chunk(path, path_str, &chunks, |stage| {
        collect(stage, kind, Order::Authored, |prim, _| {
            found.push(prim.path().to_string(), ());
        });
    })?;
    let prims = found.items.into_iter().map(|(p, ())| p).collect::<Vec<_>>();
    debug!(
        "Found {} {kind:?} prim(s) on {} in {:?}",
        prims.len(),
        path.display(),
        started.elapsed()
    );
    Ok(prims)
}

/// [`list_prims`]' prims with the values a render reads for each, at time
/// code `frame` — see [`crate::Scene::list_usd_records`].
pub(crate) fn list_records(
    path: &Path,
    kind: ListKind,
    frame: Option<f64>,
) -> Result<Vec<ListRecord>, crate::Error> {
    if let Some(t) = frame
        && !t.is_finite()
    {
        return Err(crate::Error::InvalidFrame(t));
    }
    let _time_scope = EvalTimeScope::enter(frame);
    let started = Instant::now();
    let path_str = utf8(path)?;
    let index = open_index(path, path_str)?;
    let chunks = stream_roots(&index);
    // What the import reads before it meets a camera: the settings (the
    // image's aspect, which the vertical aperture defaults through) and the
    // camera it was told to use.
    let products = import_render_products(&index);
    let mut settings = import_render_settings(&index);
    if let Some((w, h)) = products.resolution {
        settings = settings.with_resolution(w, h);
    }
    let wanted = wanted_camera(None, products.camera, &index);
    drop(index);

    let mut found: Found<ListRecord> = Found::default();
    // The first camera met in the import's own order, and the materials the
    // import's binding resolution reaches.
    let mut first_camera: Option<String> = None;
    let mut bound: HashSet<String> = HashSet::new();
    for_each_chunk(path, path_str, &chunks, |stage| {
        collect(stage, kind, Order::Authored, |prim, hidden| {
            if let Some(r) = record(stage, prim, kind, hidden, &settings) {
                found.push(prim.path().to_string(), r);
            }
        });
        match kind {
            ListKind::Camera if first_camera.is_none() => {
                collect(stage, kind, Order::Import, |prim, _| {
                    if first_camera.is_none() {
                        first_camera = Some(prim.path().to_string());
                    }
                });
            }
            ListKind::Material => bound_materials(stage, &mut bound),
            _ => {}
        }
    })?;
    let mut records: Vec<ListRecord> = found.items.into_iter().map(|(_, r)| r).collect();
    match kind {
        ListKind::Camera => {
            let wanted_path = wanted.as_ref().map(|c| c.path().to_string());
            let wanted_met = wanted_path
                .as_ref()
                .is_some_and(|w| records.iter().any(|r| r.path() == w));
            let render_camera = match pick_camera(wanted.as_ref(), wanted_met, !records.is_empty())
            {
                CameraPick::Wanted => wanted_path,
                CameraPick::First => first_camera,
                CameraPick::Procedural | CameraPick::Missing(_) => None,
            };
            for r in &mut records {
                if let ListRecord::Camera(c) = r {
                    c.is_render_camera = render_camera.as_deref() == Some(c.path.as_str());
                }
            }
        }
        ListKind::Material => {
            for r in &mut records {
                if let ListRecord::Material(m) = r {
                    m.bound = bound.contains(&m.path);
                }
            }
        }
        ListKind::Light => {}
    }
    debug!(
        "Read {} {kind:?} record(s) on {} in {:?}",
        records.len(),
        path.display(),
        started.elapsed()
    );
    Ok(records)
}

fn utf8(path: &Path) -> Result<&str, crate::Error> {
    path.to_str()
        .ok_or_else(|| crate::Error::NonUtf8Path(path.to_path_buf()))
}

/// The payload-free index: as the import does, it decides the chunks.
fn open_index(path: &Path, path_str: &str) -> Result<Stage, crate::Error> {
    Stage::builder()
        .load(InitialLoadSet::LoadNone)
        .open(path_str)
        .map_err(|e| crate::Error::UsdOpen {
            path: path.to_path_buf(),
            source: e.into(),
        })
}

/// `visit` on each chunk's stage, opened with its payloads — a shot camera
/// or a set's lights under a payload is the production case — or on the
/// whole stage when it does not stream.
fn for_each_chunk(
    path: &Path,
    path_str: &str,
    chunks: &[sdf::Path],
    mut visit: impl FnMut(&Stage),
) -> Result<(), crate::Error> {
    if chunks.is_empty() {
        let stage = open_stage(path, path_str, None)?;
        visit(&stage);
        release_stage(stage, false);
    } else {
        for chunk in chunks {
            let stage = open_stage(path, path_str, Some(chunk.clone()))?;
            visit(&stage);
            release_stage(stage, false);
        }
    }
    Ok(())
}

/// One listed prim's record, its derived fields (`is_render_camera`,
/// `bound`) left for the caller, who has seen every chunk.
fn record(
    stage: &Stage,
    prim: &Prim,
    kind: ListKind,
    hidden: bool,
    settings: &RenderSettings,
) -> Option<ListRecord> {
    let path = prim.path().to_string();
    Some(match kind {
        ListKind::Camera => {
            let lens = camera_lens(stage, prim, settings)?;
            ListRecord::Camera(CameraRecord {
                path,
                focal_length_mm: lens.focal_length,
                aperture_mm: lens.aperture,
                f_stop: lens.f_stop,
                focus_distance: lens.focus_distance,
                is_render_camera: false,
                hidden,
            })
        }
        ListKind::Light => {
            let p = || prim.path().clone();
            let (kind, inputs) = if let Ok(Some(l)) = SphereLight::get(stage, p()) {
                read("sphere", prim, &l)
            } else if let Ok(Some(l)) = RectLight::get(stage, p()) {
                read("rect", prim, &l)
            } else if let Ok(Some(l)) = DiskLight::get(stage, p()) {
                read("disk", prim, &l)
            } else if let Ok(Some(l)) = CylinderLight::get(stage, p()) {
                read("cylinder", prim, &l)
            } else if let Ok(Some(l)) = DistantLight::get(stage, p()) {
                read("distant", prim, &l)
            } else if let Ok(Some(l)) = DomeLight::get(stage, p()) {
                read("dome", prim, &l)
            } else {
                return None;
            };
            ListRecord::Light(LightRecord {
                path,
                kind,
                intensity: inputs.intensity,
                exposure: inputs.exposure,
                color: inputs.color.to_array(),
                normalize: inputs.normalize,
            })
        }
        ListKind::Material => ListRecord::Material(MaterialRecord {
            surface: surface_shader_id(stage, prim.path()),
            path,
            bound: false,
        }),
    })
}

/// A light's type name and its inputs.
fn read(kind: &'static str, prim: &Prim, light: &impl UsdLight) -> (&'static str, LightInputs) {
    (kind, light_inputs(prim, light))
}

/// The materials the import's binding resolution ([`bound_material`])
/// resolves the geometry prims it renders to — meshes, spheres and curves,
/// walked as the import walks them: pruned subtrees left out, an instance's
/// prototype walked once in its place, a `PointInstancer`'s prototypes
/// entered (they render through it).
fn bound_materials(stage: &Stage, out: &mut HashSet<String>) {
    let mut prototypes: HashSet<String> = HashSet::new();
    let mut stack: Vec<(Prim, WalkScope)> =
        vec![(prim_at(stage, sdf::Path::abs_root()), WalkScope::Stage)];
    while let Some((prim, scope)) = stack.pop() {
        if prune_reason(&prim, scope).is_some() {
            continue;
        }
        if prim.is_instance().unwrap_or(false)
            && let Ok(Some(proto)) = prim.prototype()
        {
            if prototypes.insert(proto.to_string()) {
                stack.push((prim_at(stage, proto), WalkScope::Prototype));
            }
            continue;
        }
        let p = || prim.path().clone();
        let geometry = matches!(UsdMesh::get(stage, p()), Ok(Some(_)))
            || matches!(UsdSphere::get(stage, p()), Ok(Some(_)))
            || matches!(UsdBasisCurves::get(stage, p()), Ok(Some(_)));
        if geometry && let Some(m) = bound_material(stage, &prim) {
            out.insert(m.to_string());
        }
        if let Ok(children) = prim.children() {
            stack.extend(children.into_iter().map(|c| (c, scope)));
        }
    }
}

/// In which order a walk visits a prim's children: as authored (the
/// listing's order), or as the import's traversal pops them, last first —
/// the order whose first camera is the import's fallback.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    Authored,
    Import,
}

/// What a walk does with one prim.
enum Visit {
    /// Neither the prim nor anything below it.
    Prune,
    /// The prim and its subtree; `hidden` is true under an invisible ancestor.
    Enter { hidden: bool },
}

/// The prims listed so far, in the order first met.
///
/// A path can be met more than once: a chunk's population mask keeps the
/// masked prim's ancestors, so a stage's sole top-level prim — the parent of
/// every streamed chunk — is walked once per chunk.
struct Found<T> {
    items: Vec<(String, T)>,
    seen: HashSet<String>,
}

impl<T> Default for Found<T> {
    fn default() -> Self {
        Found {
            items: Vec::new(),
            seen: HashSet::new(),
        }
    }
}

impl<T> Found<T> {
    fn push(&mut self, path: String, item: T) {
        if self.seen.insert(path.clone()) {
            self.items.push((path, item));
        }
    }
}

/// Calls `found` with each `kind` prim a walk of `stage` meets, in `order`,
/// and whether it is under an invisible ancestor.
fn collect(stage: &Stage, kind: ListKind, order: Order, mut found: impl FnMut(&Prim, bool)) {
    let mut stack: Vec<(Prim, bool)> = vec![(prim_at(stage, sdf::Path::abs_root()), false)];
    while let Some((prim, parent_hidden)) = stack.pop() {
        let Visit::Enter { hidden } = visit(stage, &prim, kind, parent_hidden) else {
            continue;
        };
        if is_kind(stage, &prim, kind) {
            found(&prim, hidden);
        }
        if let Ok(children) = prim.children() {
            match order {
                // Reversed onto the stack, so they pop in authored order.
                Order::Authored => stack.extend(children.into_iter().rev().map(|c| (c, hidden))),
                Order::Import => stack.extend(children.into_iter().map(|c| (c, hidden))),
            }
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
