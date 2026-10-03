//! USD scene import: opens a stage and produces a runtime `Scene`
//! (camera, world, lights, render settings). See `Scene::from_usd`.
//!
//! # Layout
//!
//! This module owns the import as a whole — [`load_scene`], the streaming
//! chunk loop, the prim traversal ([`traverse_into`]) and the state threaded
//! through it ([`ImportCtx`], [`ImportCaches`]). Each schema family is a
//! submodule that the traversal dispatches into:
//!
//! | module         | reads                                                        |
//! |----------------|--------------------------------------------------------------|
//! | [`time`]       | the time code every attribute read resolves at (`-f`)        |
//! | [`settings`]   | `UsdRenderSettings`, `crust:*` render attributes, camera choice |
//! | [`attrs`]      | typed attribute readers, `crust:rayMask` / motion / subdivision |
//! | [`xform`]      | `xformOp:*` stacks → world matrices                          |
//! | [`camera`]     | `UsdGeomCamera`                                              |
//! | [`mesh`]       | `UsdGeomMesh`: interning, deferred bake-vs-instance, side tables |
//! | [`adaptive`]   | adaptive subdivision: a mesh's level from its size on screen |
//! | [`shapes`]     | `UsdGeomSphere`, `UsdGeomBasisCurves`                        |
//! | [`instancing`] | `PointInstancer` and native `instanceable` prototypes        |
//! | [`lights`]     | every UsdLux light type, shaping and IES                     |
//! | [`light_links`] | `collection:lightLink`: lights that illuminate nothing      |
//! | [`materials`]  | binding resolution, the material cache, shader dispatch      |
//! | [`preview`]    | `UsdPreviewSurface` + `UsdUVTexture` networks                |
//! | [`volume`]     | `crust:volume:*` regions                                     |
//!
//! Submodules expose what their siblings need as `pub(super)` and import
//! each other explicitly, so a file's `use super::…` block is its real
//! dependency list. The traversal is single-threaded; see [`time`] for why
//! that matters.

use std::collections::HashMap;
use std::hash::Hash;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat4 as GMat4, Vec3};
use tracing::{debug, info, warn};

use crate::camera::Camera;
use crate::light::LightList;
use crate::rt_world::{World, WorldBuilder};
use crate::scene::AssetLoader;
use crate::scene::Scene;
use crate::stats::{ImageCounters, MemorySample, RenderStats, SceneCounters, SubdivisionCounters};
use crate::tracer::RenderSettings;
use crate::volume::VolumeRegion;

use openusd::sdf;
use openusd::usd::{InitialLoadSet, Prim, Stage, StagePopulationMask};
use openusd_schemas::geom::{
    BasisCurves as UsdBasisCurves, Camera as UsdCamera, Mesh as UsdMesh, PointInstancer,
    Sphere as UsdSphere,
};
use openusd_schemas::lux::{
    CylinderLight, DiskLight, DistantLight as UsdDistantLight, DomeLight, RectLight, SphereLight,
};

mod adaptive;
mod attrs;
mod camera;
mod instancing;
mod light_links;
mod lights;
mod materials;
mod mesh;
mod preview;
mod settings;
mod shapes;
mod time;
mod volume;
mod xform;

use adaptive::{Frustum, ScreenRate};
use attrs::{
    custom_token, resolve_adaptive_max_level, resolve_subdiv_edge_length, resolve_subdiv_level,
    value_at,
};
use camera::{build_camera, screen_projection};
use instancing::{ProtoPart, emit_native_instance, emit_point_instancer};
use light_links::LightLinks;
use lights::{
    emit_cylinder_light, emit_disk_light, emit_distant_light, emit_dome_light, emit_rect_light,
    emit_sphere_light,
};
use materials::{MaterialCache, resolve_material};
use mesh::{MeshArena, MeshPlacement, SubdivPolicy, emit_mesh, flush_meshes};
use settings::{
    CameraChoice, check_time_range, dome_light_camera_visibility, import_render_settings,
    render_settings_camera, render_settings_subdiv_edge_length, render_settings_subdiv_level,
};
use shapes::{emit_curves, emit_sphere};
use time::EvalTimeScope;
use volume::emit_volume;
use xform::{local_matrix_at, resets_xform_stack_at};

/// `Stage::prim` for a path that is already an `sdf::Path`.
///
/// openusd 0.7 widened the argument to any path-like value, so the call now
/// returns a `Result` whose error is a *parse* failure. Every caller here
/// hands it a path that was parsed or composed already, which is why the
/// error arm is unreachable rather than merely unlikely. A `Prim` handle
/// asserts nothing about composed content either way — querying it is what
/// finds out whether a prim is there.
fn prim_at(stage: &Stage, path: sdf::Path) -> Prim {
    stage
        .prim(path)
        .expect("an already-parsed sdf::Path cannot fail to parse")
}

/// Below this many streamable chunks, importing under one stage is
/// simpler and no slower — masked opens each re-compose the root layer,
/// so they only pay off when the payloads they exclude are the bulk of
/// the cost.
const MIN_STREAM_CHUNKS: usize = 4;

/// The subtrees to import one at a time, each under its own masked stage.
///
/// Returns the children of the stage's single top-level prim — the shape
/// every scene here takes, a `/world`-style root holding one prim per
/// element — or the top-level prims when there is not exactly one. Empty
/// when there are too few chunks for streaming to be worth it, which
/// tells the caller to import the whole stage in one pass.
///
/// # Why this is worth the extra opens
///
/// Composing the whole Moana island costs openusd 75.74 GiB and 6m19s;
/// composing a stage masked to one element costs 1.10 GiB and 2.6s. So
/// importing element by element, dropping each stage when done, bounds
/// the composed set at roughly one element instead of all twenty:
/// **117.10 GiB and 13:20 become 43.76 GiB and 09:19**, with output
/// pixel-identical to the single-stage import.
///
/// The re-opens are not free — each masked open re-composes the root
/// layer — which is what [`MIN_STREAM_CHUNKS`] guards against on scenes
/// too small for the excluded payloads to pay for them.
///
/// Getting this right depends on the caches distinguishing paths that
/// are stable across stages from paths that are not; see
/// [`MaterialCache::key`] and [`ImportCaches::epoch`].
fn stream_roots(stage: &Stage) -> Vec<sdf::Path> {
    // Escape hatch, and the way to A/B the two import paths against each
    // other on one scene: `CRUST_STREAM_IMPORT=0` forces a single stage.
    if !crate::config().stream_import {
        debug!("Streaming import disabled by CRUST_STREAM_IMPORT=0");
        return Vec::new();
    }
    let chunks = subtree_roots(stage);
    if chunks.len() < MIN_STREAM_CHUNKS {
        return Vec::new();
    }
    chunks
}

/// The stage's top-level subtrees: the children of its one root prim, or its
/// root prims when it has several. The unit [`stream_roots`] streams, and the
/// scope adaptive subdivision counts native placements in — in every import
/// mode, so that streaming cannot change which prototypes are shared.
fn subtree_roots(stage: &Stage) -> Vec<sdf::Path> {
    let pseudo_root = prim_at(stage, sdf::Path::abs_root());
    let Ok(top) = pseudo_root.children() else {
        return Vec::new();
    };
    match top.as_slice() {
        [only] => match only.children() {
            Ok(children) => children.iter().map(|c| c.path().clone()).collect(),
            Err(_) => Vec::new(),
        },
        many => many.iter().map(|p| p.path().clone()).collect(),
    }
}

/// Counts the native placements of every prototype on `stage`, per top-level
/// subtree, into `caches.placements`: the same walk and pruning as
/// [`traverse_into`] (abstract, then [`prune_reason`]), not descending into an
/// instance or a `PointInstancer`, whose contents the traversal does not reach
/// directly either. An invisible subtree is pruned outright: the traversal
/// walks it only for cameras, and places nothing from it.
fn count_placements(stage: &Stage, caches: &mut ImportCaches<'_>) {
    let mut stack = vec![(prim_at(stage, sdf::Path::abs_root()), GMat4::IDENTITY)];
    while let Some((prim, parent_world)) = stack.pop() {
        if prim.is_abstract().unwrap_or(false) || prune_reason(&prim).is_some() {
            continue;
        }
        let local = local_matrix_at(stage, &prim);
        let world = if resets_xform_stack_at(stage, &prim) {
            local
        } else {
            parent_world * local
        };
        if prim.is_instance().unwrap_or(false)
            && let Ok(Some(proto)) = prim.prototype()
        {
            // A zero-scale placement draws nothing (`attach_proto_parts`
            // skips it), so it does not make its prototype shared.
            if world.determinant().abs() >= 1e-12 {
                let key = (caches.subtree_of(prim.path()), proto.to_string());
                *caches.placements.entry(key).or_default() += 1;
            }
            continue;
        }
        if let Ok(Some(instancer)) = PointInstancer::get(stage, prim.path().clone()) {
            let subtree = caches.subtree_of(prim.path());
            for (target, n) in instancing::instancer_target_counts(&prim, &instancer, &world) {
                *caches
                    .placements
                    .entry((subtree.clone(), target))
                    .or_default() += n as u32;
            }
            continue;
        }
        if let Ok(children) = prim.children() {
            stack.extend(children.into_iter().map(|c| (c, world)));
        }
    }
}

/// Everything a traversal accumulates, kept separate from the stage that
/// feeds it so several stages can be walked into the same scene and each
/// dropped when done — see [`traverse_into`].
struct ImportCtx<'a> {
    world: WorldBuilder,
    lights: LightList,
    volumes: Vec<VolumeRegion>,
    camera: Option<Camera>,
    /// The camera to render through, when one was named; see
    /// [`CameraChoice`]. `None` takes the first camera met.
    wanted_camera: Option<CameraChoice>,
    /// The first camera met, kept while a named one is still being looked
    /// for: it is the fallback when a `RenderSettings.camera` target turns
    /// out not to exist.
    first_camera: Option<(Camera, sdf::Path)>,
    /// Every camera prim met, for the error that names the alternatives when
    /// a requested camera is missing. A path per camera, and a stage has a
    /// handful of them.
    cameras_seen: Vec<sdf::Path>,
    caches: ImportCaches<'a>,
    /// Direct mesh placements whose representation is not yet decided, in
    /// traversal order. Drained by [`flush_meshes`] after the last chunk —
    /// the decision needs every chunk's placement counts, so it cannot be
    /// made while walking. Holds ~88 bytes per mesh prim, not per triangle.
    pending_meshes: Vec<MeshPlacement>,
    /// Receivers seen and lights whose `collection:lightLink` restricts
    /// them, resolved after the last chunk (see [`light_links`]).
    links: LightLinks,
    settings: RenderSettings,
    /// The whole stage with payloads unloaded, where light collections are
    /// resolved (a chunk's mask may exclude what they name). Dropped as soon
    /// as the links are resolved, before the top-level BVH commit that is
    /// the import's memory peak.
    index: Option<Stage>,
}

/// Walks `root` and its subtree, emitting geometry, lights and volumes
/// into `ctx`. Takes the stage by reference and keeps nothing borrowed
/// from it, so the caller may drop the stage afterwards and walk another.
fn traverse_into(stage: &Stage, root: Prim, root_xf: GMat4, ctx: &mut ImportCtx) {
    // The flag is "under an invisible ancestor", which hides everything but
    // cameras — see below.
    let mut stack: Vec<(Prim, GMat4, bool)> = vec![(root, root_xf, false)];

    while let Some((prim, parent_world, parent_hidden)) = stack.pop() {
        // `class` prims (and their descendants) describe geometry that
        // exists only to be referenced or instanced — they are never
        // rendered in their own right. Prototypes reach the same prims
        // through `collect_proto_parts`, which deliberately does not apply
        // this rule.
        if prim.is_abstract().unwrap_or(false) {
            debug!("Skipping abstract (class) prim {}", prim.path());
            continue;
        }
        let invisible = match prune_reason(&prim) {
            Some(PruneReason::Invisible) => true,
            Some(reason) => {
                debug!("Skipping {reason} prim {}", prim.path());
                continue;
            }
            None => false,
        };

        let local = local_matrix_at(stage, &prim);
        let resets = resets_xform_stack_at(stage, &prim);
        let this_world = if resets { local } else { parent_world * local };

        // An invisible subtree draws nothing and lights nothing, but is still
        // walked for cameras: a camera's own visibility only hides its gizmo
        // in a viewport, and a rig hidden that way is still rendered through.
        // Pruning it outright would make `--camera` fail on it and move the
        // first-camera fallback. Instances are not entered — crust never
        // takes a camera from a prototype.
        let hidden = parent_hidden || invisible;
        if hidden {
            if !parent_hidden {
                debug!("Skipping invisible prim {} and its subtree", prim.path());
            }
            if prim.is_instance().unwrap_or(false) {
                continue;
            }
            visit_camera(stage, &prim, ctx);
            if let Ok(children) = prim.children() {
                for child in children {
                    stack.push((child, this_world, true));
                }
            }
            continue;
        }

        // What this prim emits, for the light links: ids are handed out in
        // traversal order, so the range recorded after dispatch is exactly
        // this prim's (see `light_links`).
        let geoms_before = ctx.world.count();
        let vols_before = ctx.volumes.len();
        let lights_before = ctx.lights.count();

        // Native instancing: an `instanceable` prim with a composition arc
        // shares one prototype with every other instance of it. Take the
        // geometry from the prototype and place it — never descend into
        // the instance's own (proxy) subtree, which would rebuild the same
        // triangles once per instance.
        if prim.is_instance().unwrap_or(false) {
            match prim.prototype() {
                Ok(Some(proto_path)) => {
                    emit_native_instance(
                        stage,
                        &mut ctx.world,
                        &prim,
                        &proto_path,
                        this_world,
                        &mut ctx.caches,
                    );
                    ctx.links
                        .saw(prim.path(), geoms_before..ctx.world.count(), 0..0);
                    continue;
                }
                _ => warn!(
                    "Prim {} is instanceable but has no prototype — importing directly",
                    prim.path()
                ),
            }
        }

        // Dispatch by schema. Volume prims are checked first: a prim
        // carrying `crust:volume:type` imports as a participating-media
        // region only — never as geometry, so its bounds cannot occlude
        // shadow rays. Otherwise order matters only for Meshes vs Sphere
        // prims — both check first so we don't recurse into their
        // materials as prims.
        if let Ok(Some(instancer)) = PointInstancer::get(stage, prim.path().clone()) {
            emit_point_instancer(
                stage,
                &mut ctx.world,
                &prim,
                &instancer,
                this_world,
                &mut ctx.caches,
            );
            ctx.links
                .saw(prim.path(), geoms_before..ctx.world.count(), 0..0);
            // Prototypes are conventionally authored beneath the
            // instancer; they are drawn through it, never on their own.
            continue;
        } else if let Some(ty) = custom_token(&prim, "crust:volume:type") {
            emit_volume(&prim, &ty, this_world, &mut ctx.volumes);
        } else if let Ok(Some(mesh)) = UsdMesh::get(stage, prim.path().clone()) {
            let mat = resolve_material(stage, &prim, &mut ctx.caches);
            emit_mesh(
                &mut ctx.world,
                &prim,
                &mesh,
                this_world,
                mat,
                &mut ctx.caches.meshes,
                &mut ctx.pending_meshes,
            );
        } else if let Ok(Some(sphere)) = UsdSphere::get(stage, prim.path().clone()) {
            let mat = resolve_material(stage, &prim, &mut ctx.caches);
            emit_sphere(&mut ctx.world, &prim, &sphere, this_world, mat);
        } else if let Ok(Some(curves)) = UsdBasisCurves::get(stage, prim.path().clone()) {
            let mat = resolve_material(stage, &prim, &mut ctx.caches);
            emit_curves(&mut ctx.world, &prim, &curves, this_world, mat);
        } else if visit_camera(stage, &prim, ctx) {
            // Recorded (and built, if it is the one) inside the call.
        } else if let Ok(Some(light)) = SphereLight::get(stage, prim.path().clone()) {
            emit_sphere_light(stage, ctx, &prim, &light, this_world);
        } else if let Ok(Some(light)) = RectLight::get(stage, prim.path().clone()) {
            emit_rect_light(stage, ctx, &prim, &light, this_world);
        } else if let Ok(Some(light)) = DiskLight::get(stage, prim.path().clone()) {
            emit_disk_light(stage, ctx, &prim, &light, this_world);
        } else if let Ok(Some(light)) = CylinderLight::get(stage, prim.path().clone()) {
            emit_cylinder_light(stage, ctx, &prim, &light, this_world);
        } else if let Ok(Some(light)) = UsdDistantLight::get(stage, prim.path().clone()) {
            emit_distant_light(&mut ctx.lights, &prim, &light, this_world);
        } else if let Ok(Some(light)) = DomeLight::get(stage, prim.path().clone()) {
            emit_dome_light(&mut ctx.lights, &prim, &light, this_world, &mut ctx.caches);
        }
        ctx.links.saw(
            prim.path(),
            geoms_before..ctx.world.count(),
            vols_before..ctx.volumes.len(),
        );
        // Every light adds at most one entry, so a grown list means `prim`
        // is the light that grew it.
        if ctx.lights.count() > lights_before {
            ctx.links
                .light_added(ctx.index.as_ref(), stage, prim.path(), lights_before);
        }

        // Recurse. We push children onto the stack unconditionally; the
        // per-prim dispatch above will pick up any typed schemas encountered.
        if let Ok(children) = prim.children() {
            for child in children {
                stack.push((child, this_world, false));
            }
        }
    }
}

/// Records `prim` if it is a camera — as a candidate for the error listing
/// alternatives, and built when it is the one to render through. `false` when
/// it is not a camera.
fn visit_camera(stage: &Stage, prim: &Prim, ctx: &mut ImportCtx) -> bool {
    if UsdCamera::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .is_none()
    {
        return false;
    }
    ctx.cameras_seen.push(prim.path().clone());
    let named = ctx.wanted_camera.as_ref().map(CameraChoice::path);
    let is_named = named == Some(prim.path());
    // A named camera is built when met; otherwise only the first
    // camera is, and kept aside as the fallback while a named one
    // might still turn up later in the traversal.
    if ctx.camera.is_none() && (is_named || ctx.first_camera.is_none()) {
        match build_camera(stage, prim, &ctx.settings) {
            Some(c) if is_named || named.is_none() => {
                debug!("Imported USD camera at {}", prim.path());
                ctx.camera = Some(c);
            }
            Some(c) => ctx.first_camera = Some((c, prim.path().clone())),
            None => warn!("Failed to build camera from {}", prim.path()),
        }
    }
    true
}

/// Why a walk of the stage leaves a prim and its whole subtree out of what a
/// render draws. These are the rules every walk shares — the traversal
/// ([`traverse_into`]), the placement count ([`count_placements`]) and the
/// prototype walk (`instancing::prototype_prunes`) — so that the count makes
/// the same decision as the traversal, or a prototype's shared/unshared
/// verdict (and so its adaptive level) would change. Each caller logs on its
/// own terms and adds its own rules: the stage walks also skip abstract
/// (`class`) prims, which a prototype walk must not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PruneReason {
    /// `active = false`. USD prunes an inactive prim and its whole namespace
    /// subtree from the composed scene — the standard way a stage disables
    /// geometry (e.g. an LOD or a too-dense archive) without editing its
    /// source.
    Inactive,
    /// A purpose a final render does not draw ([`non_render_purpose`]).
    Purpose(&'static str),
    /// `visibility = "invisible"` ([`is_invisible`]).
    Invisible,
}

/// Reads as the adjective the skip messages use: "Skipping {reason} prim".
impl std::fmt::Display for PruneReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PruneReason::Inactive => f.write_str("inactive"),
            PruneReason::Purpose(purpose) => write!(f, "{purpose}-purpose"),
            PruneReason::Invisible => f.write_str("invisible"),
        }
    }
}

/// The first [`PruneReason`] that applies to `prim`, checked in the order
/// above; `None` when the prim is drawn.
fn prune_reason(prim: &Prim) -> Option<PruneReason> {
    if !prim.is_active().unwrap_or(true) {
        return Some(PruneReason::Inactive);
    }
    if let Some(purpose) = non_render_purpose(prim) {
        return Some(PruneReason::Purpose(purpose));
    }
    if is_invisible(prim) {
        return Some(PruneReason::Invisible);
    }
    None
}

/// Whether `prim` authors `visibility = "invisible"` at the evaluated time.
///
/// Visibility is inherited and cannot be undone below: an `invisible`
/// ancestor hides its whole subtree whatever the descendants author (their
/// only other value, `inherited`, defers to it), so as with `purpose` the
/// subtree is decided where the opinion is authored (UsdGeomImageable's
/// `ComputeVisibility`). It applies to lights as it does to geometry: an
/// invisible light does not illuminate. ALab's rig parks three interior
/// fills/bounces and a debug dome this way, and all four used to light the
/// shot.
fn is_invisible(prim: &Prim) -> bool {
    value_at(&prim.attribute("visibility")).is_some_and(|v| v.as_str() == Some("invisible"))
}

/// Drops a stage the traversal is done with, or — for a single-stage import
/// whose host asked for it (`UsdImportOptions::skip_stage_teardown`) — leaves
/// it allocated, since freeing openusd's index cache is a long tail of small
/// deallocations (45 s on ALab) that a render-and-exit process never needs.
fn release_stage(stage: Stage, keep: bool) {
    if keep {
        debug!("Leaving the final composed stage allocated (skip_stage_teardown)");
        std::mem::forget(stage);
    } else {
        drop(stage);
    }
}

/// Opens the stage with payloads loaded, optionally masked to one subtree.
fn open_stage(path: &Path, path_str: &str, mask: Option<sdf::Path>) -> Result<Stage, crate::Error> {
    let started = Instant::now();
    let masked = mask.as_ref().map(|p| p.to_string());
    let mut builder = Stage::builder().load(InitialLoadSet::LoadAll);
    if let Some(p) = mask {
        // Fallible since openusd 0.7: a mask path must be an absolute prim
        // path. These come from `stream_roots`, which yields composed
        // top-level prim paths, so a failure here is a bug rather than bad
        // input — but it is reported rather than panicked on, like every
        // other way opening a stage can fail.
        let mask = StagePopulationMask::new([p]).map_err(|e| crate::Error::UsdOpen {
            path: path.to_path_buf(),
            source: e.into(),
        })?;
        builder = builder.mask(mask);
    }
    let stage = builder.open(path_str).map_err(|e| crate::Error::UsdOpen {
        path: path.to_path_buf(),
        source: e.into(),
    })?;
    match &masked {
        Some(m) => debug!(
            "Composed {} masked to {m} in {:?}",
            path.display(),
            started.elapsed()
        ),
        None => debug!("Composed {} in {:?}", path.display(), started.elapsed()),
    }
    Ok(stage)
}

pub(crate) fn load_scene(
    path: &Path,
    assets: &dyn AssetLoader,
    options: &crate::UsdImportOptions,
) -> Result<Scene, crate::Error> {
    let time = options.frame;
    // The authoritative check: every host reaches the importer through here,
    // and nothing past this point expects a non-finite time.
    if let Some(t) = time
        && !t.is_finite()
    {
        return Err(crate::Error::InvalidFrame(t));
    }
    // Same for the camera: a malformed path is refused before the stage is
    // opened, not discovered after a four-minute traversal.
    let requested_camera = match &options.camera {
        Some(c) => Some(
            sdf::path(c)
                .ok()
                .filter(|p| p.is_abs() && p.is_prim_path() && !p.is_abs_root())
                .ok_or_else(|| crate::Error::InvalidCameraPath(c.clone()))?,
        ),
        None => None,
    };
    let _time_scope = EvalTimeScope::enter(time);
    let import_start = Instant::now();
    let mut stats = RenderStats::new();

    let path_str = path
        .to_str()
        .ok_or_else(|| crate::Error::NonUtf8Path(path.to_path_buf()))?;

    let open_start = Instant::now();
    let index = open_index_stage(path, path_str)?;
    let StageSettings {
        settings,
        domes_seen_by_camera,
        wanted_camera,
        subdiv,
    } = resolve_settings(&index, path, path_str, options, requested_camera)?;
    let chunks = stream_roots(&index);
    let subtrees: std::collections::HashSet<String> = if subdiv.adaptive.is_some() {
        subtree_roots(&index)
            .iter()
            .map(ToString::to_string)
            .collect()
    } else {
        Default::default()
    };
    // Kept, not dropped: a light's link collections are read on it (see
    // `light_links`), because a streamed chunk's population mask can leave
    // out the prims a collection refers to. It stays cheap — openusd composes
    // lazily, and only the lights' prims are ever asked for.
    let open_elapsed = open_start.elapsed();
    let open_mem = MemorySample::now();

    let mut ctx = ImportCtx {
        world: WorldBuilder::new(),
        lights: LightList::new(),
        volumes: Vec::new(),
        camera: None,
        wanted_camera,
        first_camera: None,
        cameras_seen: Vec::new(),
        // Prims binding the same material path share one Arc, and prims
        // with identical local geometry + material share one copy of that
        // geometry — placed by an instance when it is placed more than once,
        // baked flat into the parent BVH when it is placed exactly once.
        caches: ImportCaches {
            subtrees,
            ..ImportCaches::new(assets, path, subdiv)
        },
        pending_meshes: Vec::new(),
        links: LightLinks::default(),
        settings,
        index: Some(index),
    };

    let traverse_start = Instant::now();
    traverse_stages(
        &mut ctx,
        path,
        path_str,
        &chunks,
        options.skip_stage_teardown,
    )?;

    // The traverse also builds each mesh's and prototype's kernel scene,
    // so its own BVH work is inside this figure; the separate "Commit
    // acceleration structure" phase below is the *top-level* build.
    // Host asset decoding (environment maps, Ptex) happens *during* traversal but
    // is reported as its own phase, so it comes back out of this figure.
    let asset_time = ctx.caches.asset_time;
    let traverse_elapsed = traverse_start.elapsed().saturating_sub(asset_time);
    let traverse_mem = MemorySample::now();
    debug!(
        "Traversal done in {:?}, plus {:?} of host asset decoding taken out of it and \
         reported as its own phase: {} geometries, {} light(s), {} volume region(s)",
        traverse_elapsed,
        asset_time,
        ctx.world.count(),
        ctx.lights.count(),
        ctx.volumes.len()
    );

    let camera = choose_camera(&mut ctx)?;
    if let Some(subdivision) = finish_world(&mut ctx, domes_seen_by_camera, &camera) {
        stats.subdivision = subdivision;
    }

    let commit_start = Instant::now();
    let committed = ctx.world.commit();
    let commit_elapsed = commit_start.elapsed();
    debug!("Top-level acceleration structure committed in {commit_elapsed:?}");
    let commit_mem = MemorySample::now();

    record_stats(
        &mut stats,
        PhaseTimes {
            import_start,
            open: (open_elapsed, open_mem),
            traverse: (traverse_elapsed, traverse_mem),
            assets: asset_time,
            commit: (commit_elapsed, commit_mem),
        },
        &committed,
        &ctx.lights,
        ctx.volumes.len(),
        &settings,
    );

    let mut scene = Scene::new(camera, committed, ctx.lights, settings).with_volumes(ctx.volumes);
    scene.stats = stats;
    Ok(scene)
}

/// Opens the stage without payloads: enough to read render settings and see
/// the stage's shape, but none of the geometry. On a production scene the
/// payloads *are* the cost — composing the whole Moana island costs openusd
/// 75.74 GiB, where this costs a fraction of that.
fn open_index_stage(path: &Path, path_str: &str) -> Result<Stage, crate::Error> {
    let open_start = Instant::now();
    let index = Stage::builder()
        .load(InitialLoadSet::LoadNone)
        .open(path_str)
        .map_err(|e| crate::Error::UsdOpen {
            path: path.to_path_buf(),
            source: e.into(),
        })?;
    debug!(
        "Opened index stage (payloads unloaded) for {} in {:?}",
        path.display(),
        open_start.elapsed()
    );
    Ok(index)
}

/// What the index stage decides before the traversal starts.
struct StageSettings {
    settings: RenderSettings,
    /// `domeLightCameraVisibility`.
    domes_seen_by_camera: bool,
    wanted_camera: Option<CameraChoice>,
    subdiv: SubdivPolicy,
}

/// Reads the render settings, the camera to render through and the load's
/// subdivision policy from the index stage.
fn resolve_settings(
    index: &Stage,
    path: &Path,
    path_str: &str,
    options: &crate::UsdImportOptions,
    requested_camera: Option<sdf::Path>,
) -> Result<StageSettings, crate::Error> {
    let time = options.frame;
    if let Some(t) = time {
        check_time_range(index, t);
    }
    // Render settings come first — the camera importer needs the aspect ratio.
    let mut settings = import_render_settings(index);
    let domes_seen_by_camera = dome_light_camera_visibility(index);
    // Geometry, not tracer, settings: every mesh whose scheme is not `none`
    // is refined to this one level, so it must be known before the traversal.
    let subdiv_level = resolve_subdiv_level(
        options.subdivision_level,
        render_settings_subdiv_level(index),
    );
    debug!("Subdivision level {subdiv_level} for every mesh whose subdivisionScheme is not none");
    // Which camera to render through: the host's explicit choice, else the
    // stage's own `RenderSettings.camera`. Decided here, on the index stage,
    // because the traversal needs it before it meets any camera.
    let wanted_camera = match requested_camera {
        Some(p) => Some(CameraChoice::Requested(p)),
        None => render_settings_camera(index).map(CameraChoice::Settings),
    };
    if let Some(choice) = &wanted_camera {
        debug!("Rendering through {choice}");
    }
    let subdiv = subdiv_policy(
        index,
        path,
        path_str,
        options,
        subdiv_level,
        wanted_camera.as_ref(),
        &settings,
    )?;
    if let Some(t) = time {
        // The sampler's frame seed follows the frame being rendered, so an
        // image sequence gets independent noise per frame instead of one
        // pattern swimming over moving geometry. Integer part only: the
        // seed is an integer, and a subframe shares its frame's seed.
        let seed = t.floor() as isize;
        debug!("Frame {t} sets the sampler frame seed to {seed} (over crust:frame)");
        settings = settings.with_frame(seed);
    }
    Ok(StageSettings {
        settings,
        domes_seen_by_camera,
        wanted_camera,
        subdiv,
    })
}

/// Walks the stage into `ctx`: in one pass when `chunks` is empty (a small or
/// flat stage, or `CRUST_STREAM_IMPORT=0`), else one stage masked to each
/// chunk at a time.
fn traverse_stages(
    ctx: &mut ImportCtx,
    path: &Path,
    path_str: &str,
    chunks: &[sdf::Path],
    skip_stage_teardown: bool,
) -> Result<(), crate::Error> {
    if chunks.is_empty() {
        debug!(
            "Single-stage import (fewer than {MIN_STREAM_CHUNKS} subtrees, or \
             CRUST_STREAM_IMPORT=0)"
        );
    } else {
        debug!("Streaming import over {} subtrees", chunks.len());
    }
    // `None` is the single pass over the whole stage.
    let passes = chunks
        .iter()
        .map(Some)
        .chain(chunks.is_empty().then_some(None));
    for (n, chunk) in passes.enumerate() {
        // Per chunk rather than per prim: this is the loop whose memory
        // high-water mark the streaming import exists to bound, so the
        // running totals are what say whether it is doing its job.
        let chunk_start = Instant::now();
        if let Some(chunk) = chunk {
            debug!("Chunk {}/{}: {}", n + 1, chunks.len(), chunk);
        }
        let stage = open_stage(path, path_str, chunk.cloned())?;
        if ctx.caches.meshes.subdiv.adaptive.is_some() {
            count_placements(&stage, &mut ctx.caches);
        }
        // Traverse from the root, not from `chunk`: a mask keeps the
        // masked path's *ancestors* populated, so starting at the
        // root picks up their transforms exactly as a full traversal
        // would, while everything outside the chunk stays absent.
        traverse_into(
            &stage,
            prim_at(&stage, sdf::Path::abs_root()),
            GMat4::IDENTITY,
            ctx,
        );
        // A streamed chunk's stage is always dropped, the last one included:
        // that is the memory bound streaming exists for, and a streamed
        // import's peak often comes *after* the traversal — at the
        // top-level BVH commit, on the island — where a kept stage would
        // stack on top of it. Only a single-stage import may keep it.
        release_stage(stage, chunk.is_none() && skip_stage_teardown);
        if chunk.is_some() {
            ctx.caches.next_epoch();
            debug!(
                "Chunk {}/{} done in {:?} — running totals: {} geometries, {} light(s), \
                 {} volume region(s), {} mesh placement(s) pending",
                n + 1,
                chunks.len(),
                chunk_start.elapsed(),
                ctx.world.count(),
                ctx.lights.count(),
                ctx.volumes.len(),
                ctx.pending_meshes.len()
            );
        }
    }
    Ok(())
}

/// The camera the traversal settled on: the named one, else the first met
/// (with a warning when `RenderSettings.camera` named a missing one), else
/// the fallback scene's. A camera the host requested must exist.
fn choose_camera(ctx: &mut ImportCtx) -> Result<Camera, crate::Error> {
    let camera = match (
        ctx.camera.take(),
        ctx.wanted_camera.take(),
        ctx.first_camera.take(),
    ) {
        (Some(c), _, _) => c,
        (None, Some(CameraChoice::Requested(p)), _) => {
            return Err(crate::Error::CameraNotFound {
                path: p.to_string(),
                available: ctx.cameras_seen.iter().map(ToString::to_string).collect(),
            });
        }
        (None, Some(CameraChoice::Settings(p)), Some((c, first))) => {
            warn!(
                "RenderSettings.camera targets {p}, which is not a camera on this stage — \
                     rendering through {first} instead"
            );
            c
        }
        (None, _, _) => {
            warn!("USD stage has no UsdGeomCamera — falling back to world::get_settings camera");
            crate::world::get_settings().0
        }
    };
    Ok(camera)
}

/// What waits for the last chunk: light links judged against every receiver,
/// the index stage dropped, dome visibility applied, and the deferred mesh
/// placements baked or instanced — all before the top-level commit, which
/// freezes geometry masks and consumes the geometry table. Returns the
/// adaptive-subdivision counters, read against the render `camera`.
fn finish_world(
    ctx: &mut ImportCtx,
    domes_seen_by_camera: bool,
    camera: &Camera,
) -> Option<SubdivisionCounters> {
    // Every chunk has been walked, so each mesh's placement count is final
    // and the deferred instance-vs-bake decisions can be made. Must happen
    // before `commit`, which is what consumes the geometry table.
    // Likewise every receiver has been seen, so a light's link can be judged
    // against all of them; before `commit`, which freezes geometry masks.
    std::mem::take(&mut ctx.links).resolve(&mut ctx.lights, &mut ctx.world, &mut ctx.volumes);
    drop(ctx.index.take());
    if !domes_seen_by_camera {
        debug!(
            "domeLightCameraVisibility = false: every light at infinity is hidden from the camera"
        );
        ctx.lights.hide_infinite_from_camera();
    }

    if ctx.caches.meshes.subdiv.adaptive.is_some() {
        let [interior, stitched] = ctx.caches.meshes.subdiv.quality;
        debug!(
            "Per-face triangle shapes (4√3·area / Σ edge², bins ≥0.9 · ≥0.5 · ≥0.1 · ≥0.01 · \
             <0.01): interior {interior:?}, stitched {stitched:?}"
        );
    }
    let subdivision = ctx.caches.meshes.subdiv.adaptive.map(|rate| {
        // The two reads of one camera must agree, or every level was chosen
        // for a viewpoint the render does not use. Both go through
        // `camera::camera_frame`, but `screen_projection` may have read the
        // camera on the index stage or a stage masked to it, and the render
        // camera comes from the traversal's stage.
        debug_assert!(
            (Vec3::from(camera.origin()) - rate.eye).length() <= 1e-4 * rate.eye.length().max(1.0),
            "adaptive subdivision read the camera at {:?}, the render camera is at {:?}",
            rate.eye,
            camera.origin()
        );
        SubdivisionCounters {
            adaptive: Some((rate.target, rate.max)),
            levels: ctx.caches.meshes.subdiv.levels.clone(),
            shared_meshes: ctx.caches.meshes.subdiv.shared_meshes,
            shared_level: ctx.caches.meshes.subdiv.level,
            per_face_meshes: ctx.caches.meshes.subdiv.per_face_meshes,
            per_face_fallbacks: ctx.caches.meshes.subdiv.per_face_fallbacks,
            rate_bins: ctx.caches.meshes.subdiv.rate_bins.clone(),
        }
    });

    let pending = std::mem::take(&mut ctx.pending_meshes);
    flush_meshes(&mut ctx.world, &mut ctx.caches.meshes, pending);
    subdivision
}

/// Each import phase's duration and the memory sampled where it ended.
struct PhaseTimes {
    import_start: Instant,
    open: (Duration, MemorySample),
    traverse: (Duration, MemorySample),
    /// Host asset decoding, taken out of the traversal's figure.
    assets: Duration,
    commit: (Duration, MemorySample),
}

/// Records the import's phases and the scene and image counters in `stats`.
fn record_stats(
    stats: &mut RenderStats,
    phases: PhaseTimes,
    committed: &World,
    lights: &LightList,
    volumes: usize,
    settings: &RenderSettings,
) {
    let (open_elapsed, open_mem) = phases.open;
    let (traverse_elapsed, traverse_mem) = phases.traverse;
    let (commit_elapsed, commit_mem) = phases.commit;
    // Memory is sampled where each phase actually ended, not here — the
    // phases are all recorded together, so `record`'s sample-now would
    // give every one of them the same figures.
    stats.record_at(
        "Parse USD stage",
        0,
        phases.import_start.elapsed(),
        commit_mem,
    );
    stats.record_at("Open stage", 1, open_elapsed, open_mem);
    stats.record_at("Traverse prims", 1, traverse_elapsed, traverse_mem);
    if !phases.assets.is_zero() {
        stats.record_at("Load assets", 1, phases.assets, traverse_mem);
    }
    stats.record_at(
        "Commit acceleration structure",
        1,
        commit_elapsed,
        commit_mem,
    );

    stats.scene = SceneCounters {
        geometries: committed.count(),
        top_level: committed.primitive_breakdown().into(),
        unique: committed.unique_primitive_breakdown().into(),
        footprint: committed.memory_footprint(),
        lights: lights.count(),
        volumes,
    };
    let (w, h) = settings.get_dimensions();
    stats.image = ImageCounters {
        width: w,
        height: h,
        samples_per_pixel: settings.samples_per_pixel(),
        max_depth: settings.max_depth(),
    };
}

/// How this load refines subdivision surfaces: one `subdiv_level` for every
/// mesh, or — when a target edge length is set — adaptively, from the render
/// camera, which must then be resolved here, before the traversal meets it.
///
/// The camera is read from the index stage when it is composed there, else
/// from a stage population-masked to its path with payloads loaded (a shot
/// camera under a payload is the production case). Without a named camera,
/// or when the path is not a camera, adaptive mode warns once and the uniform
/// level applies: finding the first camera would take a traversal of its own.
fn subdiv_policy(
    index: &Stage,
    path: &Path,
    path_str: &str,
    options: &crate::UsdImportOptions,
    subdiv_level: u32,
    wanted_camera: Option<&CameraChoice>,
    settings: &RenderSettings,
) -> Result<SubdivPolicy, crate::Error> {
    let target = resolve_subdiv_edge_length(
        options.subdivision_edge_length,
        render_settings_subdiv_edge_length(index),
    );
    let Some(target) = target.filter(|_| crate::config().subdiv) else {
        return Ok(SubdivPolicy::new(subdiv_level));
    };
    let Some(choice) = wanted_camera else {
        warn!(
            "Adaptive subdivision ({target} px) needs the render camera before the stage \
             is read — name it with --camera or RenderSettings.camera. Using the uniform \
             level {subdiv_level}"
        );
        return Ok(SubdivPolicy::new(subdiv_level));
    };
    let camera_path = choice.path().clone();
    let projection = match screen_projection(index, &prim_at(index, camera_path.clone()), settings)
    {
        Some(p) => Some(p),
        None => {
            let started = Instant::now();
            let stage = open_stage(path, path_str, Some(camera_path.clone()))?;
            let p = screen_projection(&stage, &prim_at(&stage, camera_path.clone()), settings);
            release_stage(stage, false);
            debug!(
                "Read {camera_path} for adaptive subdivision from a stage masked to it in {:?}",
                started.elapsed()
            );
            p
        }
    };
    let Some(projection) = projection else {
        warn!(
            "Adaptive subdivision ({target} px): {camera_path} is not a camera on this stage. \
             Using the uniform level {subdiv_level}"
        );
        return Ok(SubdivPolicy::new(subdiv_level));
    };
    let max = resolve_adaptive_max_level(
        options.subdivision_level,
        render_settings_subdiv_level(index),
    );
    info!(
        "Adaptive subdivision: unshared cage edges refined to at most {target} px through \
         {camera_path}, up to level {max}; shared prototypes at level {subdiv_level}"
    );
    // Shared prototypes take the uniform level: the level setting when one is
    // given (it is also the ceiling above), else 0.
    Ok(SubdivPolicy::adaptive(
        ScreenRate {
            eye: projection.eye,
            f_px: projection.f_px,
            target,
            max,
            frustum: crate::config().adaptive_frustum.then(|| {
                Frustum::new(
                    projection.eye,
                    projection.forward,
                    projection.up,
                    projection.tan_v,
                    projection.tan_h,
                )
            }),
        },
        subdiv_level,
    ))
}

// -----------------------------------------------------------------------
// Import-wide state and traversal predicates
// -----------------------------------------------------------------------

/// The importer's memoization, threaded through geometry import.
///
/// All three caches exist for the same reason — authored geometry should
/// be turned into kernel geometry exactly once, however many prims,
/// instances or prototypes refer to it.
struct ImportCaches<'a> {
    /// Material path (memoized) → shared material.
    materials: MaterialCache,
    /// Distinct meshes by content + material. Holds each one's triangles
    /// until its representation is decided (see [`flush_meshes`]), then its
    /// committed kernel scene if it needed one.
    meshes: MeshArena,
    /// `(epoch, prototype path)` → its shared parts, for both instancing
    /// mechanisms. See [`ImportCaches::epoch`] for the epoch. A prototype placed
    /// once in adaptive mode is built for its placement and not cached here.
    protos: HashMap<(u32, String), Arc<Vec<ProtoPart>>>,
    /// The same prototypes as one [`ProtoPart`] each (see
    /// `instancing::group_parts`), `None` for one with no geometry. Same keys
    /// as `protos`.
    groups: HashMap<(u32, String), Option<ProtoPart>>,
    /// Adaptive mode only: native placements per `(top-level subtree,
    /// prototype path)`, counted before the subtree is walked
    /// ([`count_placements`]). A prototype placed once in its subtree is
    /// unshared, and refined by its size on screen.
    placements: HashMap<(String, String), u32>,
    /// The top-level subtrees placements are counted in ([`subtree_roots`]).
    subtrees: std::collections::HashSet<String>,
    /// Which stage the entries above came from.
    ///
    /// Prototype paths (`/__Prototype_N`) are numbered per composition, so
    /// under the streaming importer the same name denotes different
    /// geometry in each stage. Bumping the epoch between stages keeps
    /// those apart.
    ///
    /// It bumps rather than clearing because the mesh cache's `MeshKey` identifies a
    /// material by its `Arc` *address*: dropping the parts would free
    /// material `Arc`s whose addresses a later allocation could reuse,
    /// and a stale mesh-cache entry would then match the wrong material.
    /// Nothing here is freed before the import ends, so those addresses
    /// stay unique — and the mesh and material caches keep deduplicating
    /// across stages, which is what stops streaming costing extra memory.
    epoch: u32,
    /// The host's decoder, for materials that carry a texture asset.
    assets: &'a dyn AssetLoader,
    /// Root layer, the fallback anchor for an asset path openusd handed back
    /// unresolved.
    stage_path: &'a Path,
    /// Time the host spent decoding assets — environment maps *and* Ptex files.
    ///
    /// One accumulator for both, deliberately: it is reported as the "Load
    /// assets" phase and subtracted out of the traversal figure, so a second
    /// one that nobody folded in would silently bill texture loading to
    /// traversal. That is exactly what a separate `ptex_time` did, and on a
    /// Ptex-heavy stage the host's decode can dominate the import.
    asset_time: Duration,
    /// Resolved `.ies` path → the decoded profile (`None`: the host declined).
    /// A light rig commonly points dozens of fixtures at one profile.
    ies: HashMap<std::path::PathBuf, Option<Arc<crate::IesProfile>>>,
    /// Resolved path → a `RectLight`'s decoded colour map, for the same reason.
    light_textures: HashMap<std::path::PathBuf, Option<Arc<crate::LightTexture>>>,
    /// Resolved path → a `DomeLight`'s decoded environment map. A stage rarely
    /// has two domes on one file, but a decode is the most expensive load
    /// there is, so it is never done twice.
    environments: HashMap<std::path::PathBuf, Option<Arc<crate::EnvironmentMap>>>,
}

impl ImportCaches<'_> {
    /// Moves on to the next stage's prototypes — see
    /// [`epoch`](Self::epoch). Deliberately not a clear: the mesh cache keys
    /// materials by `Arc` address, so nothing may be freed while it is live.
    fn next_epoch(&mut self) {
        self.epoch += 1;
        self.materials.epoch = self.epoch;
    }

    /// The top-level subtree `path` lies in, as [`subtree_roots`] partitions the
    /// stage; empty above every subtree.
    fn subtree_of(&self, path: &sdf::Path) -> String {
        let mut cur = Some(path.clone());
        while let Some(p) = cur {
            let key = p.to_string();
            if self.subtrees.contains(&key) {
                return key;
            }
            cur = p.parent();
        }
        String::new()
    }

    /// How many native placements `proto_path` has in the subtree `prim` lies in.
    pub(super) fn placement_count(&self, prim: &sdf::Path, proto_path: &sdf::Path) -> u32 {
        let key = (self.subtree_of(prim), proto_path.to_string());
        self.placements.get(&key).copied().unwrap_or(0)
    }

    /// One host decode through `assets`, memoized in the map `cache` selects:
    /// keyed by `key` (a resolved path, plus whatever else changes the
    /// decode), and remembering a failure too, so a file that would not load
    /// is not retried per material or light. The decode's time is billed to
    /// [`asset_time`](Self::asset_time). `report` runs after each actual
    /// decode — never on a cache hit — with what it returned, which is where
    /// a load's log line belongs.
    fn load_cached<K: Eq + Hash, V: Clone>(
        &mut self,
        cache: fn(&mut Self) -> &mut HashMap<K, Option<V>>,
        key: K,
        load: impl FnOnce(&dyn AssetLoader, &K) -> Option<V>,
        report: impl FnOnce(&K, Option<&V>),
    ) -> Option<V> {
        if let Some(hit) = cache(self).get(&key) {
            return hit.clone();
        }
        let started = Instant::now();
        let loaded = load(self.assets, &key);
        self.asset_time += started.elapsed();
        report(&key, loaded.as_ref());
        cache(self).insert(key, loaded.clone());
        loaded
    }
}

impl<'a> ImportCaches<'a> {
    fn new(assets: &'a dyn AssetLoader, stage_path: &'a Path, subdiv: SubdivPolicy) -> Self {
        ImportCaches {
            materials: MaterialCache::default(),
            meshes: MeshArena::new(subdiv),
            protos: HashMap::new(),
            groups: HashMap::new(),
            placements: HashMap::new(),
            subtrees: std::collections::HashSet::new(),
            epoch: 0,
            assets,
            stage_path,
            asset_time: Duration::ZERO,
            ies: HashMap::new(),
            light_textures: HashMap::new(),
            environments: HashMap::new(),
        }
    }
}

/// `Some("proxy")` / `Some("guide")` when `prim` authors a purpose a final
/// render does not draw.
///
/// A render draws `default` and `render` purpose only (UsdGeomImageable).
/// Purpose is inherited, and a non-default purpose on an ancestor wins over
/// whatever its descendants author, so pruning the subtree where the purpose
/// is authored is exactly `ComputePurpose` for a traversal that descends from
/// the root — the same shape as the `active = false` pruning beside it.
/// Without it a production asset renders twice: ALab publishes every asset
/// with a `GEO_PROXY` scope (`purpose = "proxy"`, bound only for `preview`)
/// next to its `GEO`, and the proxies drew as grey duplicates of 1 505 meshes.
fn non_render_purpose(prim: &Prim) -> Option<&'static str> {
    match value_at(&prim.attribute("purpose"))?.as_str()? {
        "proxy" => Some("proxy"),
        "guide" => Some("guide"),
        _ => None,
    }
}
