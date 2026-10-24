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
//! | [`adaptive`]   | the camera's projection; adaptive subdivision: a mesh's level from its size on screen |
//! | [`shapes`]     | `UsdGeomSphere`, `UsdGeomBasisCurves`                        |
//! | [`implicit`]   | `Cube`, `Plane`, `Cylinder`, `Cone`, `Capsule` (+ `_1`) → GeomUtil meshes |
//! | [`instancing`] | `PointInstancer` and native `instanceable` prototypes        |
//! | [`lights`]     | every UsdLux light type, shaping and IES                     |
//! | [`light_links`] | `collection:lightLink`: lights that illuminate nothing      |
//! | [`passes`]     | the active `RenderPass`: `matte` holdouts, `cameraVisibility` |
//! | [`materials`]  | binding resolution, the material cache, shader dispatch      |
//! | [`preview`]    | `UsdPreviewSurface` + `UsdUVTexture` networks                |
//! | [`mtlx_network`] | inline MaterialX (`ND_*`) networks → crust-mtlx documents  |
//! | [`volume`]     | `crust:volume:*` regions                                     |
//! | [`listing`]    | what a stage holds, without importing it (`crust ls`)        |
//!
//! Submodules expose what their siblings need as `pub(super)` and import
//! each other explicitly, so a file's `use super::…` block is its real
//! dependency list. The traversal is single-threaded; see [`time`] for why
//! that matters.

use crate::warning;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Mat4 as GMat4, Vec3};
use tracing::{debug, info};

use crate::aov::AovSource;
use crate::camera::Camera;
use crate::light::LightList;
use crate::live::{LightOrigin, Provenance, StageFacts};
use crate::rt_world::WorldBuilder;
use crate::scene::AssetLoader;
use crate::scene::Scene;
use crate::stats::{MemorySample, RenderStats, SceneCounters, SubdivisionCounters};
use crate::tracer::RenderSettings;
use crate::volume::VolumeRegion;

#[cfg(feature = "mmap")]
use openusd::ar::DefaultResolver;
use openusd::sdf;
use openusd::usd::{
    InitialLoadSet, Prim, SchemaRegistry, Stage, StageBuilder, StagePopulationMask,
};
use openusd_schemas::geom::{
    BasisCurves as UsdBasisCurves, Camera as UsdCamera, Mesh as UsdMesh,
    NurbsCurves as UsdNurbsCurves, PointInstancer, Sphere as UsdSphere,
};
use openusd_schemas::lux::{
    CylinderLight, DiskLight, DistantLight as UsdDistantLight, DomeLight, DomeLight_1, RectLight,
    SphereLight,
};

mod adaptive;
mod assets;
mod attrs;
mod camera;
pub(crate) mod implicit;
mod instancing;
mod light_links;
mod lights;
mod listing;
mod live;
mod materials;
mod mesh;
mod mtlx_network;
mod passes;
mod precomp;
mod preview;
mod products;
mod settings;
mod shapes;
mod strands;
mod time;
mod volume;
mod xform;

use adaptive::{Frustum, ScreenProjection, ScreenRate};
use attrs::{
    custom_token, prim_value, resolve_adaptive_max_level, resolve_subdiv_edge_length,
    resolve_subdiv_level,
};
use camera::{
    RenderCamera, Shutter, ShutterOffsets, build_camera, screen_projection, shutter_offsets,
};
use implicit::{ImplicitPolicy, Shape, undisplaced};
use instancing::{ProtoPart, emit_native_instance, emit_point_instancer};
use light_links::LightLinks;
use lights::{
    DomeInputs, emit_cylinder_light, emit_disk_light, emit_distant_light, emit_dome_light,
    emit_rect_light, emit_sphere_light,
};
pub(crate) use listing::{list_prims, list_records};
pub(crate) use live::{prim_frames, rebuild};
use materials::{MaterialCache, mtlx_library, resolve_bound, resolve_material};
use mesh::{
    MeshArena, MeshPlace, MeshPlacement, SubdivPolicy, emit_mesh, flush_meshes, place_mesh_source,
    with_constant_primvars,
};
use passes::ActivePass;
pub use passes::PassSummary;
use products::import_render_products;
use settings::{
    CameraChoice, CameraPick, check_time_range, dome_light_camera_visibility,
    enable_exposure_compensation, import_render_settings, pick_camera, render_settings_color_space,
    render_settings_dicing_camera, render_settings_subdiv_edge_length,
    render_settings_subdiv_level,
};
use shapes::{curve_segments, emit_curves, emit_sphere, nurbs_curve_segments};
use strands::StrandRequest;
use time::EvalTimeScope;
use volume::emit_volume;
use xform::{Timing, TransformMotion, compose_with_parent};

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
/// subtree, into `caches.placements`: the same walk and pruning
/// ([`prune_reason`]) as [`traverse_into`], not
/// descending into an instance or a `PointInstancer`, whose contents the
/// traversal does not reach directly either.
fn count_placements(stage: &Stage, caches: &mut ImportCaches<'_>) {
    let mut stack = vec![(prim_at(stage, sdf::Path::abs_root()), GMat4::IDENTITY)];
    while let Some((prim, parent_world)) = stack.pop() {
        if prune_reason(&prim, WalkScope::Stage).is_some() {
            continue;
        }
        let world = compose_with_parent(&prim, parent_world);
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
    /// The camera built to render through, and its prim.
    camera: Option<(RenderCamera, sdf::Path)>,
    /// The camera to render through, when one was named; see
    /// [`CameraChoice`]. `None` takes the first camera met.
    wanted_camera: Option<CameraChoice>,
    /// The first camera met, kept while a named one is still being looked
    /// for: it is the fallback when a `RenderSettings.camera` target turns
    /// out not to exist.
    first_camera: Option<(RenderCamera, sdf::Path)>,
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
    /// The `geom_id`s of the static analytic spheres the traversal placed.
    spheres: Vec<u32>,
    /// With `keep_geometry`, the prims other prims compose from: their arc
    /// sites (`prim_stack` paths other than their own) and the
    /// `PointInstancer`s' prototypes. A live geometry edit there would move
    /// more than the prims that emitted geometry from it.
    arc_sources: Option<Vec<String>>,
    /// Receivers seen and lights whose `collection:lightLink` restricts
    /// them, resolved after the last chunk (see [`light_links`]).
    links: LightLinks,
    /// The active render pass, decided at dispatch (see [`passes`]).
    pass: Option<ActivePass>,
    /// Each light met, by prim path: where it went in the light list and the
    /// geometry it emitted ([`crate::live::Provenance`]).
    light_origins: Vec<(String, LightOrigin)>,
    settings: RenderSettings,
    /// How the cameras' apertures are conformed to the image.
    framing: products::Framing,
    /// The stage file, for resolving asset paths against its directory.
    stage_path: &'a Path,
    /// The whole stage with payloads unloaded, where light collections are
    /// resolved (a chunk's mask may exclude what they name). Dropped as soon
    /// as the links are resolved, before the top-level BVH commit that is
    /// the import's memory peak.
    index: Option<Stage>,
    assets: &'a dyn AssetLoader,
    /// The identifiers of the layers the import's stages composed, each
    /// stage's added before it is released ([`Scene::layers`]).
    layers: std::collections::BTreeSet<String>,
}

/// Walks `root` and its subtree, emitting geometry, lights and volumes
/// into `ctx`. Takes the stage by reference and keeps nothing borrowed
/// from it, so the caller may drop the stage afterwards and walk another.
fn traverse_into(stage: &Stage, root: Prim, root_xf: GMat4, ctx: &mut ImportCtx) {
    if let Some(pass) = &mut ctx.pass {
        pass.begin_chunk(stage);
    }
    // The flag is "under an invisible ancestor", which hides everything but
    // cameras — see below. The timing is the parent's over the shutter
    // ([`TransformMotion`]).
    let mut stack: Vec<(Prim, GMat4, bool, Timing)> = vec![(root, root_xf, false, Timing::Static)];

    while let Some((prim, parent_world, parent_hidden, parent_timing)) = stack.pop() {
        let pruned = prune_reason(&prim, WalkScope::Stage);
        if let Some(reason) = pruned.filter(|&r| r != Prune::Invisible) {
            debug!("Skipping {reason} prim {}", prim.path());
            continue;
        }

        let this_world = compose_with_parent(&prim, parent_world);
        if let Some(sources) = &mut ctx.arc_sources {
            record_arc_sources(stage, &prim, sources);
        }

        // An invisible subtree draws nothing and lights nothing, but is still
        // walked for cameras: a camera's own visibility only hides its gizmo
        // in a viewport, and a rig hidden that way is still rendered through.
        // Pruning it outright would make `--camera` fail on it and move the
        // first-camera fallback. Instances are not entered — crust never
        // takes a camera from a prototype.
        let hidden = parent_hidden || pruned == Some(Prune::Invisible);
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
                    stack.push((child, this_world, true, Timing::Static));
                }
            }
            continue;
        }

        let timing = ctx
            .caches
            .motion
            .timing(&prim, &parent_timing, parent_world, this_world);
        let span = ctx.caches.motion.span(this_world, &timing);

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
                    ctx.caches.motion.drawn(&prim, &timing);
                    emit_native_instance(
                        stage,
                        &mut ctx.world,
                        &prim,
                        &proto_path,
                        span,
                        &mut ctx.caches,
                    );
                    saw(ctx, prim.path(), geoms_before..ctx.world.count(), 0..0);
                    continue;
                }
                _ => warning!(
                    InstancingInstanceableWithoutPrototype,
                    at = prim.path(),
                    "Prim {} is instanceable but has no prototype — importing directly",
                    prim.path()
                ),
            }
        }

        // Dispatch by schema. Volume prims are checked first: a prim
        // carrying `crust:volume:type` imports as a participating-media
        // region only — never as geometry, so its bounds cannot occlude
        // shadow rays, and a volume `Cube` is never a box. The geometry
        // branches then go in the prototype walk's order
        // (`collect_proto_parts`), and a geometry prim no branch reads is
        // counted for `geom.unsupported_type`.
        if let Ok(Some(instancer)) = PointInstancer::get(stage, prim.path().clone()) {
            ctx.caches.motion.drawn(&prim, &timing);
            emit_point_instancer(
                stage,
                &mut ctx.world,
                &prim,
                &instancer,
                span,
                &mut ctx.caches,
            );
            saw(ctx, prim.path(), geoms_before..ctx.world.count(), 0..0);
            // Prototypes are conventionally authored beneath the
            // instancer; they are drawn through it, never on their own.
            continue;
        } else if custom_token(&prim, "crust:volume:type").is_some() {
            ctx.caches.motion.unmoved(&prim, &timing);
            emit_volume(&prim, this_world, &mut ctx.volumes, ctx.caches.working);
        } else if let Ok(Some(mesh)) = UsdMesh::get(stage, prim.path().clone()) {
            ctx.caches.motion.drawn(&prim, &timing);
            let mat = resolve_bound(stage, &prim, &mut ctx.caches);
            let baked_moving = emit_mesh(
                &mut ctx.world,
                &prim,
                &mesh,
                span,
                mat,
                &mut ctx.caches.meshes,
                &mut ctx.pending_meshes,
            );
            if baked_moving {
                ctx.caches.motion.ignored.saw(&prim);
            }
        } else if let Ok(Some(sphere)) = UsdSphere::get(stage, prim.path().clone()) {
            ctx.caches.motion.drawn(&prim, &timing);
            let mat = resolve_material(stage, &prim, &mut ctx.caches);
            if let Some(id) = emit_sphere(
                &mut ctx.world,
                &prim,
                &sphere,
                span,
                mat,
                ctx.caches.working,
            ) {
                ctx.spheres.push(id);
            }
        } else if let Ok(Some(curves)) = UsdBasisCurves::get(stage, prim.path().clone()) {
            ctx.caches.motion.unmoved(&prim, &timing);
            let mat = resolve_material(stage, &prim, &mut ctx.caches);
            let request = StrandRequest::of(&*mat, ctx.caches.working, &ctx.caches.meshes.primvars);
            let imported = curve_segments(&prim, &curves, request.as_ref());
            emit_curves(&mut ctx.world, &prim, imported, this_world, mat);
        } else if let Ok(Some(curves)) = UsdNurbsCurves::get(stage, prim.path().clone()) {
            ctx.caches.motion.unmoved(&prim, &timing);
            let mat = resolve_material(stage, &prim, &mut ctx.caches);
            let request = StrandRequest::of(&*mat, ctx.caches.working, &ctx.caches.meshes.primvars);
            let imported = nurbs_curve_segments(&prim, &curves, request.as_ref());
            emit_curves(&mut ctx.world, &prim, imported, this_world, mat);
        } else if let Some(shape) = Shape::read(&prim) {
            ctx.caches.motion.drawn(&prim, &timing);
            let bound = undisplaced(&prim, resolve_bound(stage, &prim, &mut ctx.caches));
            let src = ctx
                .caches
                .implicit
                .source(prim.path(), &shape, MeshPlace::World(&span.open));
            let src = with_constant_primvars(src, &prim, &*bound.material, ctx.caches.working);
            let baked_moving = place_mesh_source(
                &mut ctx.world,
                &prim,
                src,
                span,
                bound,
                &mut ctx.caches.meshes,
                &mut ctx.pending_meshes,
            );
            if baked_moving {
                ctx.caches.motion.ignored.saw(&prim);
            }
        } else if visit_camera(stage, &prim, ctx) {
            // Recorded (and built, if it is the one) inside the call. A
            // moving camera is out of scope: it renders from the frame.
        } else if emit_light(stage, ctx, &prim, this_world) {
            ctx.caches.motion.unmoved(&prim, &timing);
        } else {
            ctx.caches.unread.saw(&prim);
        }
        saw(
            ctx,
            prim.path(),
            geoms_before..ctx.world.count(),
            vols_before..ctx.volumes.len(),
        );
        // Every light adds at most one entry, so a grown list means `prim`
        // is the light that grew it.
        if ctx.lights.count() > lights_before {
            ctx.links
                .light_added(ctx.index.as_ref(), stage, prim.path(), lights_before);
            if let Some(pass) = &ctx.pass {
                pass.light_added(prim.path(), lights_before, &mut ctx.lights);
            }
            ctx.light_origins.push((
                prim.path().to_string(),
                LightOrigin {
                    index: lights_before,
                    geoms: geoms_before as u32..ctx.world.count() as u32,
                },
            ));
        }

        // Recurse. We push children onto the stack unconditionally; the
        // per-prim dispatch above will pick up any typed schemas encountered.
        if let Ok(children) = prim.children() {
            for child in children {
                stack.push((child, this_world, false, timing.clone()));
            }
        }
    }
}

/// Records that the prim at `path` emitted the geometries `geoms` and the
/// volume regions `vols`: for the light links, which judge them after the
/// last chunk, and the active pass, which flags them now.
fn saw(
    ctx: &mut ImportCtx,
    path: &sdf::Path,
    geoms: std::ops::Range<usize>,
    vols: std::ops::Range<usize>,
) {
    if let Some(pass) = &mut ctx.pass {
        pass.saw(path, geoms.clone(), vols.clone(), &mut ctx.world);
    }
    ctx.links.saw(path, geoms, vols);
}

/// Adds the prims `prim` composes from to `sources`: the paths of its
/// `prim_stack` sites other than its own (a site inside one of its variants
/// is its own), and a `PointInstancer`'s prototype targets.
fn record_arc_sources(stage: &Stage, prim: &Prim, sources: &mut Vec<String>) {
    let own = prim.path().as_str();
    for site in prim.prim_stack().unwrap_or_default() {
        // Only prim sites: a layer's pseudo-root is no prim another composes.
        if !site.path.is_prim() {
            continue;
        }
        let path = without_variants(site.path.as_str());
        if path != own && !path.is_empty() {
            sources.push(path);
        }
    }
    if let Ok(Some(instancer)) = PointInstancer::get(stage, prim.path().clone()) {
        use openusd_schemas::geom::PointInstancerSchema;
        for target in instancer.prototypes_rel().targets().unwrap_or_default() {
            sources.push(target.to_string());
        }
    }
}

/// `path` with each variant selection (`{set=name}`) read as a separator, so
/// a site inside a prim's own variant is its own path.
fn without_variants(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut depth = 0usize;
    for c in path.chars() {
        match c {
            '{' => {
                depth += 1;
                if depth == 1 && !out.ends_with('/') {
                    out.push('/');
                }
            }
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim_end_matches('/').to_owned()
}

/// Emits `prim` if it is a UsdLux light — the one dispatch the traversal
/// and an in-place light rebuild ([`live`]) share. `false` when it is not one.
fn emit_light(stage: &Stage, ctx: &mut ImportCtx, prim: &Prim, world_xf: GMat4) -> bool {
    if let Ok(Some(light)) = SphereLight::get(stage, prim.path().clone()) {
        emit_sphere_light(stage, ctx, prim, &light, world_xf);
    } else if let Ok(Some(light)) = RectLight::get(stage, prim.path().clone()) {
        emit_rect_light(stage, ctx, prim, &light, world_xf);
    } else if let Ok(Some(light)) = DiskLight::get(stage, prim.path().clone()) {
        emit_disk_light(stage, ctx, prim, &light, world_xf);
    } else if let Ok(Some(light)) = CylinderLight::get(stage, prim.path().clone()) {
        emit_cylinder_light(stage, ctx, prim, &light, world_xf);
    } else if let Ok(Some(light)) = UsdDistantLight::get(stage, prim.path().clone()) {
        emit_distant_light(&mut ctx.lights, prim, &light, world_xf, ctx.caches.working);
    } else if let Ok(Some(light)) = DomeLight::get(stage, prim.path().clone()) {
        emit_dome_light(ctx, prim, &DomeInputs::dome_light(&light), world_xf);
    } else if let Ok(Some(light)) = DomeLight_1::get(stage, prim.path().clone()) {
        let dome = DomeInputs::dome_light_1(stage, &light);
        emit_dome_light(ctx, prim, &dome, world_xf);
    } else {
        return false;
    }
    true
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
        match build_camera(stage, prim, &ctx.settings, ctx.framing) {
            Some(c) if is_named || named.is_none() => {
                debug!("Imported USD camera at {}", prim.path());
                ctx.camera = Some((c, prim.path().clone()));
            }
            Some(c) => ctx.first_camera = Some((c, prim.path().clone())),
            None => warning!(
                CameraUnreadable,
                at = prim.path(),
                "Failed to build camera from {}",
                prim.path()
            ),
        }
    }
    true
}

/// Why a traversal leaves a prim and its whole subtree out of the render —
/// see [`prune_reason`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Prune {
    /// A `class` prim: it exists only to be referenced or instanced.
    Abstract,
    /// `active = false`.
    Inactive,
    /// A purpose a final render does not draw (`"proxy"` / `"guide"`).
    Purpose(&'static str),
    /// `visibility = "invisible"`.
    Invisible,
}

impl std::fmt::Display for Prune {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Prune::Abstract => f.write_str("abstract (class)"),
            Prune::Inactive => f.write_str("inactive"),
            Prune::Purpose(p) => write!(f, "{p}-purpose"),
            Prune::Invisible => f.write_str("invisible"),
        }
    }
}

/// Where a prim is met, which decides whether [`Prune::Abstract`] applies.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WalkScope {
    /// The stage's own namespace: the top-level traversal and the placement
    /// count that must agree with it.
    Stage,
    /// Inside a prototype, reached through an instance. A prototype is
    /// commonly authored under a `class` prim, so abstractness does not prune
    /// here.
    Prototype,
}

/// The one pruning rule every walk applies, so that the traversal, the
/// placement count and the prototype walk cannot disagree about which prims
/// exist.
///
/// Each rule prunes a whole subtree, decided where the opinion is authored:
///
/// - **abstract** (stage scope only) — `class` prims describe geometry that
///   is never rendered in its own right.
/// - **inactive** — USD prunes an inactive prim and its namespace subtree
///   from the composed scene, the standard way a stage disables geometry (an
///   LOD, a too-dense archive) without editing its source.
/// - **non-render purpose** — a render draws `default` and `render` purpose
///   only (UsdGeomImageable). Purpose is inherited and a non-default one on an
///   ancestor wins, so pruning where it is authored is exactly
///   `ComputePurpose` for a walk from the root. Without it a production asset
///   renders twice: ALab publishes every asset with a `GEO_PROXY` scope
///   (`purpose = "proxy"`) next to its `GEO`, and the proxies drew as grey
///   duplicates of 1 505 meshes.
/// - **invisible** — visibility is inherited and cannot be undone below (the
///   only other value, `inherited`, defers to it), as `ComputeVisibility`
///   has it. It applies to lights as to geometry: ALab's rig parks three
///   interior fills/bounces and a debug dome this way, and all four used to
///   light the shot. The top-level traversal still walks an invisible
///   subtree for cameras (see [`traverse_into`]).
///
/// Checked in that order, so the reason reported is the first that applies.
pub(super) fn prune_reason(prim: &Prim, scope: WalkScope) -> Option<Prune> {
    if scope == WalkScope::Stage && prim.is_abstract().unwrap_or(false) {
        return Some(Prune::Abstract);
    }
    if !prim.is_active().unwrap_or(true) {
        return Some(Prune::Inactive);
    }
    let token = |name: &str| prim_value(prim, name);
    if let Some(v) = token("purpose") {
        match v.as_str() {
            Some("proxy") => return Some(Prune::Purpose("proxy")),
            Some("guide") => return Some(Prune::Purpose("guide")),
            _ => {}
        }
    }
    token("visibility")
        .is_some_and(|v| v.as_str() == Some("invisible"))
        .then_some(Prune::Invisible)
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

/// The typed schemas' registry every stage the import opens carries: the
/// families crust-core enables on openusd-schemas (`geom`, `lux`, `shade`,
/// `render`, `skel`, `vol`, `proc`, `media`). Built once per process. A stage
/// that authors the scene (`crust mcp`'s session) opens with this one, so
/// it resolves a declared type and a schema fallback as the import does.
pub fn usd_schema_registry() -> Arc<SchemaRegistry> {
    openusd_schemas::schema_registry()
}

/// `Stage::builder()` carrying the typed schemas' registry, and reading files
/// through memory mappings when the `mmap` feature is built in, unless
/// `CRUST_USD_MMAP=0`. Every stage the import
/// reads must open through this: a stage without the registry knows only the
/// core `usd` family, so every typed `get` (`Mesh`, `SphereLight`,
/// `Settings`, …) answers `None` and no schema fallback resolves — the scene
/// would import as empty, without an error.
pub(super) fn stage_builder() -> StageBuilder {
    let builder = Stage::builder().schema_registry(usd_schema_registry());
    #[cfg(feature = "mmap")]
    if crate::config().usd_mmap {
        return builder.resolver(mapping_resolver());
    }
    builder
}

/// openusd's filesystem resolver, serving every file from a read-only memory
/// mapping: a layer holds no copy of its file, and only the pages a read
/// touches are loaded.
// The crate's one `unsafe` outside a test: `map_files` is an `unsafe fn` so
// that its caller states the promise below; it does nothing unsafe itself.
#[cfg(feature = "mmap")]
#[allow(unsafe_code)]
fn mapping_resolver() -> DefaultResolver {
    // SAFETY: `map_files` asks that no mapped file be modified while anything
    // read from it is alive — a layer, a package index, an asset view. Here
    // that is only the stage: the import copies every value out (`sdf::Value`
    // owns its data) and takes no asset bytes from openusd, so nothing read
    // from a mapping outlives the stage that read it, and every stage this
    // builder opens lives for one import, or one `crust ls` listing. The final
    // stage a render leaves allocated (`skip_stage_teardown`) is never read
    // again. Within that window the promise is the user's, as it is for any
    // file a program has open: a USD file rewritten in place *during* the
    // import can fault the process. Writers that replace a file by renaming
    // over it (openusd's own `Layer::save`) leave the mapped bytes intact.
    // The `crust mcp` session, whose authoring stage lives as long as the
    // session, opens it with its own plain builder and never maps.
    unsafe { DefaultResolver::new().map_files() }
}

/// Opens the stage with payloads loaded, optionally masked to one subtree.
fn open_stage(path: &Path, path_str: &str, mask: Option<sdf::Path>) -> Result<Stage, crate::Error> {
    open_stage_masked(path, path_str, mask.map(|p| vec![p]))
}

/// [`open_stage`] population-masked to every path of `mask` (and their
/// ancestors and descendants), payloads loaded.
fn open_stage_masked(
    path: &Path,
    path_str: &str,
    mask: Option<Vec<sdf::Path>>,
) -> Result<Stage, crate::Error> {
    let started = Instant::now();
    let masked = mask.as_ref().map(|ps| {
        ps.iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    });
    let mut builder = stage_builder().load(InitialLoadSet::LoadAll);
    if let Some(p) = mask {
        // Fallible since openusd 0.7: a mask path must be an absolute prim
        // path. These come from `stream_roots`, which yields composed
        // top-level prim paths, so a failure here is a bug rather than bad
        // input — but it is reported rather than panicked on, like every
        // other way opening a stage can fail.
        let mask = StagePopulationMask::new(p).map_err(|e| crate::Error::UsdOpen {
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

/// The host's [`UsdImportOptions`](crate::UsdImportOptions) checked before
/// the stage is opened: a bad frame, camera path or working space is refused
/// here, not discovered after a four-minute traversal.
struct ValidOptions {
    time: Option<f64>,
    camera: Option<sdf::Path>,
    working: Option<crate::color::Space>,
    /// The active render pass ([`passes`]), checked to be a prim path; that
    /// it is a `RenderPass` is checked on the stage, by [`read_index`].
    pass: Option<sdf::Path>,
}

impl ValidOptions {
    fn check(options: &crate::UsdImportOptions) -> Result<Self, crate::Error> {
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
                    .filter(|p| p.is_abs() && p.is_prim() && !p.is_abs_root())
                    .ok_or_else(|| crate::Error::InvalidCameraPath(c.clone()))?,
            ),
            None => None,
        };
        // A working space the host names is refused here, before the stage is
        // opened, like a bad camera path.
        let host_working = match &options.working_space {
            Some(name) => Some(crate::color::working_space(name)?),
            None => None,
        };
        let pass = match &options.pass {
            Some(p) => Some(
                sdf::path(p)
                    .ok()
                    .filter(|p| p.is_abs() && p.is_prim() && !p.is_abs_root())
                    .ok_or_else(|| crate::Error::PassNotFound {
                        path: p.clone(),
                        why: "not an absolute prim path such as /Render/Passes/char".into(),
                    })?,
            ),
            None => None,
        };
        Ok(ValidOptions {
            time,
            camera: requested_camera,
            working: host_working,
            pass,
        })
    }
}

/// Walks the stage into `ctx`: in one pass when it is small or flat, else one
/// masked stage per top-level subtree (`chunks`), each dropped before the next
/// is composed — the memory bound the streaming import exists for.
fn traverse_stage(
    path: &Path,
    path_str: &str,
    chunks: &[sdf::Path],
    skip_stage_teardown: bool,
    ctx: &mut ImportCtx,
) -> Result<(), crate::Error> {
    if chunks.is_empty() {
        // Small or flat stage: one pass, exactly as before.
        debug!(
            "Single-stage import (fewer than {MIN_STREAM_CHUNKS} subtrees, or \
             CRUST_STREAM_IMPORT=0)"
        );
        let stage = open_stage(path, path_str, None)?;
        if ctx.caches.meshes.subdiv.adaptive.is_some() {
            count_placements(&stage, &mut ctx.caches);
        }
        traverse_into(
            &stage,
            prim_at(&stage, sdf::Path::abs_root()),
            GMat4::IDENTITY,
            ctx,
        );
        ctx.layers.extend(stage.layer_identifiers());
        release_stage(stage, skip_stage_teardown);
    } else {
        debug!("Streaming import over {} subtrees", chunks.len());
        for (n, chunk) in chunks.iter().enumerate() {
            // Per chunk rather than per prim: this is the loop whose memory
            // high-water mark the streaming import exists to bound, so the
            // running totals are what say whether it is doing its job.
            let chunk_start = Instant::now();
            debug!("Chunk {}/{}: {}", n + 1, chunks.len(), chunk);
            // The active pass's collections are read on each chunk's stage
            // (see `passes`), so the pass prim is composed in every one.
            let mask = match &ctx.pass {
                Some(pass) => vec![chunk.clone(), pass.path().clone()],
                None => vec![chunk.clone()],
            };
            let stage = open_stage_masked(path, path_str, Some(mask))?;
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
            // Always dropped, the last chunk included: that is the memory
            // bound streaming exists for, and a streamed import's peak often
            // comes *after* the traversal — at the top-level BVH commit, on
            // the island — where a kept stage would stack on top of it.
            ctx.layers.extend(stage.layer_identifiers());
            release_stage(stage, false);
            // Separate this stage's prototypes from the next stage's —
            // see ImportCaches::epoch. Deliberately not a clear: the
            // mesh cache keys materials by Arc address, so nothing may
            // be freed while it is live.
            ctx.caches.epoch += 1;
            ctx.caches.materials.epoch = ctx.caches.epoch;
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
        // The materials the chunks bound from outside themselves, dropped
        // before the top-level commit as every chunk's stage is.
        let layers = ctx.caches.materials.outside.release();
        ctx.layers.extend(layers);
    }
    Ok(())
}

/// The camera the traversal settled on: the one asked for, else — when the
/// stage's `RenderSettings.camera` names one that is not there — the first
/// camera met, else the procedural fallback's. A camera the host asked for by
/// path and the stage does not have is an error naming the alternatives.
/// Returned with the prim it was built from, `None` for the procedural one,
/// whose exposure scale is 1.
fn resolve_camera(ctx: &mut ImportCtx) -> Result<(RenderCamera, Option<String>), crate::Error> {
    // `camera` is the wanted one when it was met, else (nothing named) the
    // first; `first_camera` is the first, kept only while a named one was
    // still being looked for.
    let (built, wanted, first) = (
        ctx.camera.take(),
        ctx.wanted_camera.take(),
        ctx.first_camera.take(),
    );
    let wanted_met = wanted.is_some() && built.is_some();
    let any_met = built.is_some() || first.is_some();
    let with_path = |(c, path): (RenderCamera, sdf::Path)| (c, Some(path.to_string()));
    Ok(match pick_camera(wanted.as_ref(), wanted_met, any_met) {
        CameraPick::Missing(p) => {
            return Err(crate::Error::CameraNotFound {
                path: p.to_string(),
                available: ctx.cameras_seen.iter().map(ToString::to_string).collect(),
            });
        }
        CameraPick::Wanted => with_path(built.expect("the wanted camera was met")),
        CameraPick::First => match (built, first) {
            (Some(c), _) => with_path(c),
            (None, Some((c, first))) => {
                if let Some(p) = wanted.as_ref().map(CameraChoice::path) {
                    warning!(
                        CameraNotACamera,
                        at = p,
                        "RenderSettings.camera targets {p}, which is not a camera on this \
                         stage — rendering through {first} instead"
                    );
                }
                with_path((c, first))
            }
            (None, None) => unreachable!("a camera was met"),
        },
        CameraPick::Procedural => {
            warning!(
                CameraMissing,
                "USD stage has no UsdGeomCamera — falling back to world::get_settings camera"
            );
            let camera = crate::world::get_settings().0;
            let camera = if ctx.settings.depth_of_field() {
                camera
            } else {
                camera.pinhole()
            };
            (
                RenderCamera {
                    camera,
                    exposure_scale: 1.0,
                    refused_meters_per_unit: None,
                    shutter: ShutterOffsets::default(),
                    unhonoured_pixel_aspect: None,
                    lens: None,
                },
                None,
            )
        }
    })
}

/// The subdivision and displacement counters the traversal accumulated, into
/// `--stats`. `dicer` is the render camera when it is the camera adaptive
/// subdivision read, `None` when a `crust:dicingCamera` dices.
fn record_geometry_counters(meshes: &MeshArena, dicer: Option<&Camera>, stats: &mut RenderStats) {
    if meshes.subdiv.adaptive.is_some() {
        let [interior, stitched] = meshes.subdiv.quality;
        debug!(
            "Per-face triangle shapes (4√3·area / Σ edge², bins ≥0.9 · ≥0.5 · ≥0.1 · ≥0.01 · \
             <0.01): interior {interior:?}, stitched {stitched:?}"
        );
    }
    if let Some(rate) = meshes.subdiv.adaptive {
        // Both reads derive from one `CameraFrame`, but adaptive subdivision
        // may read it off a different stage (the unloaded index while
        // streaming); the two must still agree, or every level was chosen for
        // a viewpoint the render does not use.
        if let Some(camera) = dicer {
            debug_assert!(
                (Vec3::from(camera.origin()) - rate.eye).length()
                    <= 1e-4 * rate.eye.length().max(1.0),
                "adaptive subdivision read the camera at {:?}, the render camera is at {:?}",
                rate.eye,
                camera.origin()
            );
        }
        stats.subdivision = SubdivisionCounters {
            adaptive: Some((rate.target, rate.max)),
            levels: meshes.subdiv.levels.clone(),
            shared_meshes: meshes.subdiv.shared_meshes,
            shared_level: meshes.subdiv.level,
            per_face_meshes: meshes.subdiv.per_face_meshes,
            per_face_fallbacks: meshes.subdiv.per_face_fallbacks,
            rate_bins: meshes.subdiv.rate_bins.clone(),
        };
    }

    stats.displacement = crate::stats::DisplacementCounters {
        frustum_skipped: meshes.subdiv.frustum_skipped,
        ..meshes.displaced.clone()
    };
}

pub(crate) fn load_scene(
    path: &Path,
    assets: &dyn AssetLoader,
    options: &crate::UsdImportOptions,
) -> Result<Scene, crate::Error> {
    let valid = ValidOptions::check(options)?;
    let time = valid.time;
    let _time_scope = EvalTimeScope::enter(time);
    // Every coded warning this import raises, on this thread, is recorded
    // here and handed back on the scene.
    let warning_scope = crate::WarningScope::enter();
    let import_start = Instant::now();
    let mut stats = RenderStats::new();

    let path_str = path
        .to_str()
        .ok_or_else(|| crate::Error::NonUtf8Path(path.to_path_buf()))?;

    // Open once without payloads: enough to read render settings and see
    // the stage's shape, but none of the geometry. On a production scene
    // the payloads *are* the cost — composing the whole Moana island
    // costs openusd 75.74 GiB, where this costs a fraction of that.
    let open_start = Instant::now();
    let index = stage_builder()
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
    if let Some(t) = time {
        check_time_range(&index, t);
    }
    let IndexRead {
        settings,
        products,
        domes_seen_by_camera,
        exposure_compensation,
        working,
        wanted_camera,
        subdiv,
        projection,
        dicing,
        shutter,
    } = read_index(&index, path, path_str, options, &valid)?;
    let pass = valid.pass.as_ref();
    let render_settings = settings::render_settings_path(&index, pass).map(|p| p.to_string());
    let settings_prim = settings::read_settings_prim(&index, pass).map(|p| p.to_string());
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
    let asks_time = asks_about_time(&shutter, &products);
    let frame_posed = frame_pose(&shutter, &products);

    let mut ctx = ImportCtx {
        world: {
            let mut world = WorldBuilder::new();
            if options.keep_geometry {
                world.keep_inputs();
            }
            world
        },
        lights: {
            let mut lights = LightList::new();
            lights.set_luma(crate::color::luma(working));
            lights
        },
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
            implicit: ImplicitPolicy::new(projection),
            motion: TransformMotion::new(
                shutter.interval,
                shutter.offsets.open == 0.0,
                frame_posed,
                asks_time,
            ),
            ..ImportCaches::new(assets, path, subdiv, working, mtlx_library())
        },
        pending_meshes: Vec::new(),
        spheres: Vec::new(),
        arc_sources: options.keep_geometry.then(Vec::new),
        links: LightLinks::default(),
        pass: pass.map(|p| ActivePass::read(&index, p)),
        light_origins: Vec::new(),
        settings,
        framing: products.framing,
        stage_path: path,
        index: Some(index),
        assets,
        layers: Default::default(),
    };
    // The primvars the render vars read, resolved on the index stage before
    // any geometry: every mesh read from here keeps exactly these.
    ctx.caches.meshes.primvars = products.primvars.clone();

    let traverse_start = Instant::now();
    traverse_stage(
        path,
        path_str,
        &chunks,
        options.skip_stage_teardown,
        &mut ctx,
    )?;

    // The traverse also builds each mesh's and prototype's kernel scene,
    // so its own BVH work is inside this figure; the separate "Commit
    // acceleration structure" phase below is the *top-level* build.
    // Host asset decoding (environment maps, Ptex) happens *during* traversal but
    // is reported as its own phase, so it comes back out of this figure.
    let asset_time = ctx.caches.asset_time;
    let traverse_elapsed = traverse_start.elapsed().saturating_sub(asset_time);
    ctx.caches.unread.report();
    report_unmoved(
        &ctx.caches.motion,
        &shutter,
        products.request.asks_for(AovSource::MotionVector),
    );
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

    let (render_camera, camera_path) = resolve_camera(&mut ctx)?;
    render_camera.warn_lens_units(camera_path.as_deref());
    render_camera.warn_framing(camera_path.as_deref());
    check_shutter_camera(&shutter, camera_path.as_deref(), render_camera.shutter);
    let RenderCamera {
        camera,
        exposure_scale,
        lens,
        ..
    } = render_camera;
    let settings = apply_exposure(
        settings,
        exposure_compensation,
        exposure_scale,
        camera_path.as_deref(),
    );

    // Every chunk has been walked, so each mesh's placement count is final
    // and the deferred instance-vs-bake decisions can be made. Must happen
    // before `commit`, which is what consumes the geometry table.
    // Likewise every receiver has been seen, so a light's link can be judged
    // against all of them; before `commit`, which freezes geometry masks.
    let geometry_runs = ctx.links.geometry_runs();
    let volume_paths = ctx.links.volume_paths();
    let remap =
        std::mem::take(&mut ctx.links).resolve(&mut ctx.lights, &mut ctx.world, &mut ctx.volumes);
    // The provenance names each light by its place in the list as the links
    // left it; a light they took out of the list is no longer one to rebuild.
    ctx.light_origins
        .retain_mut(|(_, origin)| match remap[origin.index] {
            Some(index) => {
                origin.index = index;
                true
            }
            None => false,
        });
    let mut root_stack = (Vec::new(), None);
    if let Some(index) = ctx.index.take() {
        ctx.layers.extend(index.layer_identifiers());
        root_stack = (index.layer_stack(), Some(invisible_overrides(&index)));
    }
    if !domes_seen_by_camera {
        debug!(
            "domeLightCameraVisibility = false: every light at infinity is hidden from the camera"
        );
        ctx.lights.hide_infinite_from_camera();
    }

    let dicer = dicing.is_none().then_some(&camera);
    record_geometry_counters(&ctx.caches.meshes, dicer, &mut stats);
    let implicit = &ctx.caches.implicit;
    if implicit.shapes > 0 {
        debug!(
            "Implicit shapes: {} tessellated into {} triangles; curved ones by radial count \
             {:?}{}",
            implicit.shapes,
            implicit.triangles,
            implicit.counts,
            if implicit.projection.is_some() {
                ""
            } else {
                " (no camera named before the traversal: the fixed count)"
            }
        );
    }

    let pending = std::mem::take(&mut ctx.pending_meshes);
    let (mesh_origins, mesh_slots) = flush_meshes(&mut ctx.world, &mut ctx.caches.meshes, pending);

    let commit_start = Instant::now();
    let committed = ctx.world.commit();
    let commit_elapsed = commit_start.elapsed();
    debug!("Top-level acceleration structure committed in {commit_elapsed:?}");
    let commit_mem = MemorySample::now();

    // Memory is sampled where each phase actually ended, not here — the
    // phases are all recorded together, so `record`'s sample-now would
    // give every one of them the same figures.
    stats.record_at("Parse USD stage", 0, import_start.elapsed(), commit_mem);
    stats.record_at("Open stage", 1, open_elapsed, open_mem);
    stats.record_at("Traverse prims", 1, traverse_elapsed, traverse_mem);
    if !asset_time.is_zero() {
        stats.record_at("Load assets", 1, asset_time, traverse_mem);
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
        strand_tables_bytes: committed.strand_table_bytes(),
        primvar_tables_bytes: committed.primvar_table_bytes(),
        lights: ctx.lights.count(),
        volumes: ctx.volumes.len(),
    };
    stats.image = (&settings).into();

    let mut request = products.request;
    if let Some(pc) = request.precomp.as_mut() {
        if let Some(pc) = std::sync::Arc::get_mut(pc) {
            pc.load_kernels(|p| assets.load_light_texture(p, crate::ColorSpace::RAW));
        }
        // Nothing lights a stage with no light, so its beauty is black and the
        // stop rule, which reads only the beauty, sees no variance.
        if ctx.lights.count() == 0 && pc.has_filters() {
            warning!(
                PrecompBlackBeauty,
                "The stage has a precomp but no light, so its beauty is black: adaptive \
                 sampling stops every pixel at crust:minSamplesPerPixel. Author \
                 crust:minSamplesPerPixel = crust:samplesPerPixel for the precomp to take every \
                 sample"
            );
        }
    }
    let mut scene = Scene::new(camera, committed, ctx.lights, settings).with_volumes(ctx.volumes);
    scene.stats = stats;
    scene.aovs = request;
    scene.working_space = working;
    scene.camera_path = camera_path;
    scene.time = time;
    scene.warnings = warning_scope.finish();
    scene.layers = layer_files(&ctx.layers, path);
    scene.pass = ctx.pass.as_ref().map(ActivePass::summary);
    scene.settings_prim = settings_prim;
    scene.source = crate::scene::Source {
        lens,
        depth_of_field: scene.camera.has_depth_of_field(),
        motion_blur: scene.settings.motion_blur() && shutter.interval.is_some(),
        layer_stack: stack_files(&root_stack.0, path),
        invisible_overrides: root_stack.1,
    };
    scene.provenance = Some(Provenance {
        materials: ctx.caches.materials.origins(&scene.world),
        material_reads: ctx.caches.materials.reads(),
        arc_sources: {
            let mut sources = ctx.arc_sources.take().unwrap_or_default();
            sources.sort();
            sources.dedup();
            sources
        },
        lights: ctx.light_origins.into_iter().collect(),
        geometry: geometry_runs,
        meshes: mesh_origins,
        mesh_slots,
        spheres: std::mem::take(&mut ctx.spheres),
        volumes: volume_paths,
        stage: StageFacts {
            options: options.clone(),
            render_settings,
            subdiv_level: ctx.caches.meshes.subdiv.level,
            adaptive: ctx.caches.meshes.subdiv.adaptive.map(|r| r.key()),
            domes_seen_by_camera,
            precomp: scene.aovs.precomp.is_some(),
            working: Some(working),
            camera: scene.camera_path.clone(),
            dicing: dicing.map(|p| p.to_string()),
            on_screen_shapes: ctx
                .caches
                .implicit
                .on_screen
                .then(|| ctx.caches.implicit.projection.map(|p| p.key()))
                .flatten(),
            unsized_shapes: ctx.caches.implicit.fixed_unshared,
            primvars: ctx.caches.meshes.primvars.key(),
            shutter: shutter.key(asks_time, frame_posed),
            mtlx_library: ctx.caches.materials.library.clone(),
        },
    });
    Ok(scene)
}

/// The files behind layer `identifiers`, sorted and without repeats: a layer
/// inside a package is the package file, a relative identifier is anchored
/// to the root layer `root`'s directory, file format arguments are dropped,
/// and an anonymous layer, which has no file, is left out.
fn layer_files(
    identifiers: &std::collections::BTreeSet<String>,
    root: &Path,
) -> Vec<std::path::PathBuf> {
    let dir = root.parent().unwrap_or(Path::new(""));
    let files: std::collections::BTreeSet<std::path::PathBuf> = identifiers
        .iter()
        .filter(|id| !sdf::Layer::is_anonymous_identifier(id))
        .map(|id| {
            let id = id.split(":SDF_FORMAT_ARGS:").next().unwrap_or(id);
            let file = openusd::ar::split_package_relative_path_outer(id)
                .map(|(package, _)| package)
                .unwrap_or_else(|| id.to_owned());
            let file = Path::new(&file);
            if file.is_absolute() {
                file.to_path_buf()
            } else {
                dir.join(file)
            }
        })
        .collect();
    files.into_iter().collect()
}

/// The files of the root layer stack `identifiers`, in their order: each
/// mapped as [`layer_files`] maps it and made absolute against the working
/// directory (links not resolved), an anonymous layer left out, and a
/// package met twice kept once, where it is strongest.
fn stack_files(identifiers: &[String], root: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    for id in identifiers {
        let one = std::collections::BTreeSet::from([id.clone()]);
        for file in layer_files(&one, root) {
            // Absolute, so the same layer stamps the same path whatever the
            // working directory and however the stage path was typed.
            let file = std::path::absolute(&file).unwrap_or(file);
            if !files.contains(&file) {
                files.push(file);
            }
        }
    }
    files
}

/// The prims the strongest layer of `stage`'s root layer stack — its session
/// layer when it has one, else its root layer — hides with an `over`: a
/// `visibility` spec on an `over` prim whose default or a time sample is
/// `invisible`. What a session or override layer hid of the scene beneath
/// it; a prim the layer itself defines invisible is the scene's own.
pub(super) fn invisible_overrides(stage: &Stage) -> u32 {
    let strongest = stage.session_layer().unwrap_or_else(|| stage.root_layer());
    let data = strongest.data();
    let invisible = |v: &sdf::Value| matches!(v, sdf::Value::Token(t) if t.as_str() == "invisible");
    let mut count = 0u32;
    for path in data.spec_paths() {
        if !path.is_property() || !path.as_str().ends_with(".visibility") {
            continue;
        }
        let prim = path.prim_path();
        let is_over = matches!(
            data.try_field(&prim, "specifier"),
            Ok(Some(v)) if matches!(&*v, sdf::Value::Specifier(sdf::Specifier::Over))
        );
        if !is_over {
            continue;
        }
        let default = matches!(data.try_field(&path, "default"), Ok(Some(v)) if invisible(&v));
        let sampled = matches!(
            data.try_field(&path, "timeSamples"),
            Ok(Some(v)) if matches!(&*v, sdf::Value::TimeSamples(samples)
                if samples.iter().any(|(_, v)| invisible(v)))
        );
        if default || sampled {
            count = count.saturating_add(1);
        }
    }
    count
}

/// What the import reads before it traverses anything: the index stage's
/// render settings and products, and the stage-wide choices made from them.
/// The settings carry the products' resolution, region and motion blur, and
/// the frame's seed; the camera's exposure comes later, from the camera.
struct IndexRead {
    settings: RenderSettings,
    products: products::RenderProducts,
    domes_seen_by_camera: bool,
    exposure_compensation: bool,
    working: crate::color::Space,
    wanted_camera: Option<CameraChoice>,
    subdiv: SubdivPolicy,
    /// The projection tessellation reads — the dicing camera's — when a
    /// camera was named (see [`subdiv_policy`]).
    projection: Option<ScreenProjection>,
    /// `crust:dicingCamera`'s target, when it is a camera ([`Dicer`]).
    dicing: Option<sdf::Path>,
    /// The interval motion is measured over ([`read_camera_ahead`]).
    shutter: Shutter,
}

/// Reads [`IndexRead`] off the payload-free `index` stage: once by
/// [`load_scene`], and again by an in-place settings update ([`live`]), so
/// the two read the same settings by construction.
fn read_index(
    index: &Stage,
    path: &Path,
    path_str: &str,
    options: &crate::UsdImportOptions,
    valid: &ValidOptions,
) -> Result<IndexRead, crate::Error> {
    // The active pass first: it may choose the settings prim every read
    // below goes through.
    let pass = valid.pass.as_ref();
    if let Some(p) = pass {
        settings::check_pass(index, p)?;
        debug!("Rendering pass {p}");
    }
    // Render settings come first — the camera importer needs the aspect ratio.
    let mut settings = import_render_settings(index, pass);
    // The products the render writes. The first one's camera and resolution
    // are the render's: the settings' own unless that product overrides them.
    let products = import_render_products(index, Some(path), pass);
    if let Some((w, h)) = products.resolution {
        settings = settings.with_resolution(w, h);
    }
    // The region, after the resolution it is counted in. A host's region
    // (`--region`) replaces it later, on the settings.
    if let Some(window) = products.data_window {
        let (w, h) = settings.get_dimensions();
        if let Some(region) = products::region_from_ndc(window, w, h) {
            settings = settings
                .with_region(region)
                .expect("region_from_ndc returns a region inside the frame");
            debug!("dataWindowNDC: rendering region {region} of the {w}x{h} frame");
        }
    }
    settings = settings
        .with_motion_blur(products.motion_blur)
        .with_depth_of_field(products.depth_of_field);
    let domes_seen_by_camera = dome_light_camera_visibility(index, pass);
    let exposure_compensation = enable_exposure_compensation(index, pass);
    // The working colour space, before any colour is read: every texture
    // request and authored colour is converted into it.
    let working = valid
        .working
        .unwrap_or_else(|| render_settings_color_space(index, pass));
    debug!("Rendering in {} ({})", working.label(), working.name());
    // Geometry, not tracer, settings: every mesh whose scheme is not `none`
    // is refined to this one level, so it must be known before the traversal.
    let subdiv_level = resolve_subdiv_level(
        options.subdivision_level,
        render_settings_subdiv_level(index, pass),
    );
    debug!("Subdivision level {subdiv_level} for every mesh whose subdivisionScheme is not none");
    // Which camera to render through: the host's explicit choice, else the
    // stage's own `RenderSettings.camera`. Decided here, on the index stage,
    // because the traversal needs it before it meets any camera.
    let wanted_camera =
        settings::wanted_camera(valid.camera.clone(), products.camera.clone(), index, pass);
    if let Some(choice) = &wanted_camera {
        debug!("Rendering through {choice}");
    }
    // The camera tessellation reads: `crust:dicingCamera` when it names one,
    // else the render camera. Products do not override it.
    let (dicer, shutter) = read_camera_ahead(
        index,
        path,
        path_str,
        wanted_camera.as_ref(),
        render_settings_dicing_camera(index, pass).as_ref(),
        &settings,
        products.framing,
        valid.time,
    )?;
    if let Some((open, close)) = shutter.interval {
        debug!("Motion is measured over time codes {open} .. {close}");
    }
    let dicing = dicer.authored.then(|| dicer.path.clone()).flatten();
    let (subdiv, projection) = subdiv_policy(index, options, pass, subdiv_level, dicer);
    if let Some(t) = valid.time {
        // The sampler's frame seed follows the frame being rendered, so an
        // image sequence gets independent noise per frame instead of one
        // pattern swimming over moving geometry. Integer part only: the
        // seed is an integer, and a subframe shares its frame's seed.
        let seed = t.floor() as isize;
        debug!("Frame {t} sets the sampler frame seed to {seed} (over crust:frame)");
        settings = settings.with_frame(seed);
    }
    Ok(IndexRead {
        settings,
        products,
        domes_seen_by_camera,
        exposure_compensation,
        working,
        wanted_camera,
        subdiv,
        projection,
        dicing,
        shutter,
    })
}

/// `settings` with the render camera's exposure scale applied:
/// `enableExposureCompensation = false` renders at 1, as Hydra does. The
/// scale is checked here, once, for the camera rendered through: a
/// candidate the traversal built and dropped, or an exposure turned off,
/// warns about nothing.
fn apply_exposure(
    settings: RenderSettings,
    exposure_compensation: bool,
    exposure_scale: f32,
    camera_path: Option<&str>,
) -> RenderSettings {
    if !exposure_compensation {
        debug!("enableExposureCompensation = false: the camera's exposure is not applied");
        settings
    } else if exposure_scale.is_finite() && exposure_scale > 0.0 {
        settings.with_exposure_scale(exposure_scale)
    } else {
        let camera = camera_path.unwrap_or("the render camera");
        warning!(
            CameraInvalidExposure,
            at = camera,
            "{camera}: the exposure attributes give a scale of {exposure_scale} — the image is \
             not scaled"
        );
        settings
    }
}

/// The camera tessellation reads, as [`read_camera_ahead`] read it: the
/// dicing camera.
struct Dicer {
    /// `crust:dicingCamera`'s target when it is a camera, else the render
    /// camera's path when one is named; `None` when neither is.
    path: Option<sdf::Path>,
    /// Its screen projection; `None` when `path` is not a camera on the stage.
    projection: Option<camera::ScreenProjection>,
    /// Whether `path` is `crust:dicingCamera`'s target.
    authored: bool,
}

/// The cameras as the import reads them before the traversal: the dicing
/// camera's screen projection ([`Dicer`]), and the render camera's shutter.
///
/// A named camera — the render camera, or the `dicing` target — is read from
/// the index stage when it is composed there, else from a stage
/// population-masked to its path with payloads loaded (a shot camera under a
/// payload is the production case). A `dicing` target that is not a camera
/// warns, and the render camera dices. With no render camera named, or a named
/// path that is not a camera, the shutter comes from the first camera of a
/// walk over the index stage ([`first_camera`]), and without a dicing camera
/// there is no projection; the traversal may still settle on another camera,
/// which [`check_shutter_camera`] reports.
#[allow(clippy::too_many_arguments)]
fn read_camera_ahead(
    index: &Stage,
    path: &Path,
    path_str: &str,
    wanted_camera: Option<&CameraChoice>,
    dicing: Option<&sdf::Path>,
    settings: &RenderSettings,
    framing: products::Framing,
    frame: Option<f64>,
) -> Result<(Dicer, Shutter), crate::Error> {
    let read_on = |stage: &Stage, at: &sdf::Path| {
        let prim = prim_at(stage, at.clone());
        shutter_offsets(stage, &prim)
            .map(|s| (screen_projection(stage, &prim, settings, framing), s))
    };
    let read = |camera_path: &sdf::Path| -> Result<_, crate::Error> {
        Ok(match read_on(index, camera_path) {
            Some(r) => Some(r),
            None => {
                let started = Instant::now();
                let stage = open_stage(path, path_str, Some(camera_path.clone()))?;
                let r = read_on(&stage, camera_path);
                release_stage(stage, false);
                debug!(
                    "Read {camera_path} ahead of the traversal from a stage masked to it in {:?}",
                    started.elapsed()
                );
                r
            }
        })
    };
    let named = match wanted_camera {
        Some(choice) => read(choice.path())?.map(|r| (choice.path().clone(), r)),
        None => None,
    };
    // Only a prim path names a camera: a property path would be read as its
    // prim (the schema lookup takes the prim part) and cannot be masked to.
    let diced = match dicing {
        Some(target) => match target
            .is_prim()
            .then(|| read(target))
            .transpose()?
            .flatten()
        {
            Some((projection, _)) => Some(Dicer {
                path: Some(target.clone()),
                projection,
                authored: true,
            }),
            None => {
                warning!(
                    CameraDicingNotCamera,
                    at = target,
                    "crust:dicingCamera targets {target}, which is not a camera on this stage — \
                     tessellation reads the render camera"
                );
                None
            }
        },
        None => None,
    };
    let shutter = match &named {
        Some((camera_path, (_, offsets))) => {
            Shutter::new(Some(camera_path.clone()), *offsets, frame)
        }
        None => {
            let first = first_camera(index);
            let offsets = first
                .as_ref()
                .and_then(|p| shutter_offsets(index, &prim_at(index, p.clone())))
                .unwrap_or_default();
            Shutter::new(first, offsets, frame)
        }
    };
    let dicer = diced.unwrap_or_else(|| Dicer {
        path: wanted_camera.map(|c| c.path().clone()),
        projection: named.and_then(|(_, (projection, _))| projection),
        authored: false,
    });
    Ok((dicer, shutter))
}

/// The first camera the traversal would meet, on the payload-free `index`:
/// the same walk order (a stack, so the last child first), the same pruning,
/// an invisible subtree still walked, instances and `PointInstancer`s not
/// entered, and one top-level subtree after another when the import streams.
/// A camera inside an unloaded payload is not seen.
fn first_camera(index: &Stage) -> Option<sdf::Path> {
    let chunks = stream_roots(index);
    let roots = if chunks.is_empty() {
        vec![sdf::Path::abs_root()]
    } else {
        chunks
    };
    for root in roots {
        let mut stack = vec![(prim_at(index, root), false)];
        while let Some((prim, parent_hidden)) = stack.pop() {
            let pruned = prune_reason(&prim, WalkScope::Stage);
            if pruned.is_some_and(|r| r != Prune::Invisible) {
                continue;
            }
            let hidden = parent_hidden || pruned == Some(Prune::Invisible);
            if prim.is_instance().unwrap_or(false) {
                continue;
            }
            if UsdCamera::get(index, prim.path().clone())
                .ok()
                .flatten()
                .is_some()
            {
                return Some(prim.path().clone());
            }
            if !hidden
                && PointInstancer::get(index, prim.path().clone())
                    .ok()
                    .flatten()
                    .is_some()
            {
                continue;
            }
            if let Ok(children) = prim.children() {
                stack.extend(children.into_iter().map(|c| (c, hidden)));
            }
        }
    }
    None
}

/// Whether the import asks prims whether their transform varies over time:
/// when the shutter's offsets open an interval, or a `motionvector` var
/// would report zero for what does ([`TransformMotion`]).
fn asks_about_time(shutter: &Shutter, products: &products::RenderProducts) -> bool {
    shutter.offsets.opens() || products.request.asks_for(AovSource::MotionVector)
}

/// Whether moving placements are posed at the frame rather than at shutter
/// open: `disableMotionBlur` (not `instantaneousShutter` alone) over a
/// shutter that opens an interval away from the frame. Opening at the frame,
/// the two poses are one.
fn frame_pose(shutter: &Shutter, products: &products::RenderProducts) -> bool {
    products.sharp_at_frame
        && !products.motion_blur
        && shutter.interval.is_some()
        && shutter.offsets.open != 0.0
}

/// The summarised warnings for time-varying transforms the render does not
/// move ([`TransformMotion`]): over a shutter with no frame to place it at,
/// over an instantaneous shutter while a `motionvector` var is requested, and
/// on prims that get no motion whatever the shutter.
fn report_unmoved(motion: &TransformMotion, shutter: &Shutter, motion_vectors: bool) {
    let n = motion.varying.count;
    let first = motion.varying.first.as_deref().unwrap_or("");
    let prims = if n == 1 { "prim's" } else { "prims'" };
    if n > 0 && shutter.needs_frame() {
        let camera = shutter
            .camera
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        warning!(
            TimeMotionNeedsFrame,
            at = first,
            "{camera} opens its shutter ({} .. {}), but no frame was given: {n} drawn {prims} \
             time-sampled transforms are not evaluated over it and render sharp (the first at \
             {first}). Render a frame with -f",
            shutter.offsets.open,
            shutter.offsets.close
        );
    } else if n > 0 && motion_vectors && !shutter.offsets.opens() {
        warning!(
            CameraShutterInstantaneous,
            at = first,
            "A motionvector var is requested, but the camera's shutter is instantaneous: the \
             time-sampled transforms of {n} drawn {prims} move nothing and their vectors read \
             zero (the first at {first}). Author the camera's shutter:open / shutter:close to \
             measure them"
        );
    }
    let n = motion.ignored.count;
    if n > 0 {
        let first = motion.ignored.first.as_deref().unwrap_or("");
        let one = n == 1;
        warning!(
            XformMotionIgnored,
            at = first,
            "{n} prim{} whose transform moves over the shutter get{} no motion (curves, lights, \
             volumes, prims inside a prototype and baked non-invertible meshes are not moved); \
             {} render{} at shutter open (the first at {first})",
            if one { "" } else { "s" },
            if one { "s" } else { "" },
            if one { "it" } else { "they" },
            if one { "s" } else { "" },
        );
    }
}

/// Warns when the camera the traversal settled on (`rendered`, `None` for the
/// procedural one) is not the one the shutter was read from ahead of it, and
/// the two shutters differ: motion was measured over the other camera's.
fn check_shutter_camera(
    shutter: &Shutter,
    rendered: Option<&str>,
    rendered_offsets: ShutterOffsets,
) {
    let read = shutter.camera.as_ref().map(ToString::to_string);
    if read.as_deref() == rendered || shutter.offsets == rendered_offsets {
        return;
    }
    let name = |c: Option<&str>| c.unwrap_or("the procedural camera").to_owned();
    let (read, rendered) = (name(read.as_deref()), name(rendered));
    let ShutterOffsets { open, close } = shutter.offsets;
    warning!(
        CameraShutterMismatch,
        at = rendered.as_str(),
        "The render goes through {rendered}, but motion was measured over the shutter of \
         {read}, the camera found before the traversal ({open} .. {close}): name the camera \
         with --camera or RenderSettings.camera to measure over its own"
    );
}

/// How this load refines subdivision surfaces — one `subdiv_level` for every
/// mesh, or, when a target edge length is set, adaptively from the dicing
/// camera — and that camera's [`ScreenProjection`], which also sizes implicit
/// shapes ([`implicit::ImplicitPolicy`]).
///
/// `dicer` is the dicing camera as [`read_camera_ahead`] read it. With no
/// camera named, neither a dicing nor a render camera, or when the path is
/// not a camera, there is no projection: adaptive mode then warns once and the
/// uniform level applies, and implicit shapes take their fixed count,
/// silently. Finding the first camera's projection would take a traversal of
/// its own.
fn subdiv_policy(
    index: &Stage,
    options: &crate::UsdImportOptions,
    pass: Option<&sdf::Path>,
    subdiv_level: u32,
    dicer: Dicer,
) -> (SubdivPolicy, Option<ScreenProjection>) {
    let target = resolve_subdiv_edge_length(
        options.subdivision_edge_length,
        render_settings_subdiv_edge_length(index, pass),
    )
    .filter(|_| crate::config().subdiv);
    let Some(camera_path) = dicer.path else {
        if let Some(target) = target {
            warning!(
                SubdivAdaptiveNeedsCamera,
                "Adaptive subdivision ({target} px) needs the render camera before the stage \
                 is read — name it with --camera or RenderSettings.camera, or a dicing camera \
                 with crust:dicingCamera. Using the uniform level {subdiv_level}"
            );
        }
        return (SubdivPolicy::new(subdiv_level), None);
    };
    let projection = dicer.projection.map(|p| ScreenProjection {
        eye: p.eye,
        f_px: p.f_px,
        frustum: crate::config()
            .adaptive_frustum
            .then(|| Frustum::new(p.eye, p.forward, p.up, p.tan_v, p.tan_h)),
    });
    let Some(target) = target else {
        return (SubdivPolicy::new(subdiv_level), projection);
    };
    let Some(projection) = projection else {
        warning!(
            SubdivAdaptiveNeedsCamera,
            at = camera_path,
            "Adaptive subdivision ({target} px): {camera_path} is not a camera on this stage. \
             Using the uniform level {subdiv_level}"
        );
        return (SubdivPolicy::new(subdiv_level), None);
    };
    let max = resolve_adaptive_max_level(
        options.subdivision_level,
        render_settings_subdiv_level(index, pass),
    );
    info!(
        "Adaptive subdivision: unshared cage edges refined to at most {target} px through \
         {camera_path}, up to level {max}; shared prototypes at level {subdiv_level}"
    );
    // Shared prototypes take the uniform level: the level setting when one is
    // given (it is also the ceiling above), else 0.
    (
        SubdivPolicy::adaptive(
            ScreenRate {
                projection,
                target,
                max,
            },
            subdiv_level,
        ),
        Some(projection),
    )
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
    /// It bumps rather than clearing because [`MeshKey`](mesh::MeshKey) identifies a
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
    light_textures:
        HashMap<(std::path::PathBuf, crate::ColorSpace), Option<Arc<crate::LightTexture>>>,
    /// Asset files the host failed to read, as opposed to ones it declined on
    /// purpose ([`assets::explained`]): only references to these count a
    /// `*.unreadable` warning.
    failed_assets: std::collections::HashSet<std::path::PathBuf>,
    /// How implicit shapes are tessellated, and what was.
    implicit: ImplicitPolicy,
    /// Geometry prims neither walk reads, for `geom.unsupported_type`.
    unread: UnreadGprims,
    /// How time-sampled transforms move over the shutter, and what did not.
    motion: TransformMotion,
    /// The working colour space every texture and authored colour is
    /// converted into ([`crate::color`]).
    pub(super) working: crate::color::Space,
    /// Its luminance weights, which every material's lobe selection weighs
    /// colours by ([`crate::color::luma`]).
    pub(super) luma: utils::Luma,
}

/// Geometry prims that neither walk reads as a surface or a volume — a
/// `Points`, a `NurbsPatch`, a future type — by type, in first-seen order: one
/// per prim the top-level walk meets, one per prim per prototype build.
/// Reported once per type at the end of the load ([`UnreadGprims::report`]).
#[derive(Default)]
struct UnreadGprims {
    /// `(type name, first prim, prims read)`.
    types: Vec<(String, String, u64)>,
}

impl UnreadGprims {
    /// Counts `prim`, which every reading branch has passed over, when it is
    /// a `Gprim`: a camera, a light, a material or a transform is not one.
    fn saw(&mut self, prim: &Prim) {
        if !prim.is_a("Gprim").unwrap_or(false) {
            return;
        }
        let ty = match prim.type_name() {
            Ok(Some(t)) => t.as_str().to_owned(),
            _ => return,
        };
        match self.types.iter_mut().find(|(t, ..)| *t == ty) {
            Some((.., n)) => *n += 1,
            None => self.types.push((ty, prim.path().to_string(), 1)),
        }
    }

    /// One `geom.unsupported_type` per type, carrying its count — raised at
    /// the end of the load, so a type met in several streamed chunks is
    /// still one warning.
    fn report(&self) {
        for (ty, first, n) in &self.types {
            let prims = if *n == 1 { "prim" } else { "prims" };
            warning!(
                GeomUnsupportedType,
                at = first.as_str(),
                "{n} {ty} {prims} render nothing: crust does not read the {ty} type \
                 (the first at {first})"
            );
        }
    }
}

impl ImportCaches<'_> {
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
}

impl<'a> ImportCaches<'a> {
    fn new(
        assets: &'a dyn AssetLoader,
        stage_path: &'a Path,
        subdiv: SubdivPolicy,
        working: crate::color::Space,
        library: Arc<crust_mtlx::Library>,
    ) -> Self {
        ImportCaches {
            materials: MaterialCache::new(library),
            meshes: MeshArena {
                working,
                ..MeshArena::new(subdiv)
            },
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
            failed_assets: std::collections::HashSet::new(),
            implicit: ImplicitPolicy::default(),
            unread: UnreadGprims::default(),
            motion: TransformMotion::default(),
            working,
            luma: crate::color::luma(working),
        }
    }
}

#[cfg(all(test, feature = "mmap"))]
mod tests {
    use super::*;
    use openusd::usd::PrimPredicate;

    /// An attribute's name, default value and time samples.
    type Read = (String, Option<sdf::Value>, Option<sdf::TimeSampleMap>);

    /// Every prim of `stage`, with each authored attribute read.
    fn contents(stage: &Stage) -> Vec<(String, Vec<Read>)> {
        let mut paths = Vec::new();
        stage
            .traverse(PrimPredicate::DEFAULT_PROXIES, |p| paths.push(p.clone()))
            .expect("traverses");
        paths
            .into_iter()
            .map(|path| {
                let prim = prim_at(stage, path.clone());
                let attrs = prim
                    .authored_attributes()
                    .expect("lists attributes")
                    .iter()
                    .map(|a| {
                        let value = a.get::<sdf::Value>().expect("reads");
                        (
                            a.name().to_string(),
                            value,
                            a.time_samples().expect("reads"),
                        )
                    })
                    .collect();
                (path.to_string(), attrs)
            })
            .collect()
    }

    /// A stage read through memory mappings (`CRUST_USD_MMAP`) holds exactly
    /// what one read into memory does — on a `.usdc`, whose values decode
    /// from the layer's bytes on demand, so from the mapping itself.
    #[test]
    fn a_mapped_usdc_reads_as_a_copied_one() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/PointInstancedMedCity.usd"
        );
        let registry = openusd_schemas::schema_registry;
        let mapped = Stage::builder()
            .schema_registry(registry())
            .resolver(mapping_resolver())
            .open(path)
            .expect("opens mapped");
        let copied = Stage::builder()
            .schema_registry(registry())
            .open(path)
            .expect("opens copied");
        let mapped = contents(&mapped);
        let read = |(_, v, t): &&Read| v.is_some() || t.is_some();
        let n = mapped.iter().flat_map(|(_, a)| a).filter(read).count();
        assert!(n > 50, "the sample reads {n} values");
        assert!(mapped == contents(&copied));
    }
}
