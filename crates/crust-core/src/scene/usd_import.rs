//! USD scene import: opens a stage and produces a runtime `Scene`
//! (camera, world, lights, render settings). See `Scene::from_usd`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Mat4 as GMat4;
use tracing::{debug, info, warn};

use crate::camera::Camera;
use crate::light::{
    AreaLight, DistantLight as CoreDistantLight, DomeLight as CoreDomeLight, LightList, RectShape,
    SphereShape,
};
use crate::scene::AssetLoader;
use crate::material::{Emissive, Material, OpenPBR};
use crate::ray::MASK_ALL;
use crate::rt_world::{FaceMap, FanSlice, WorldBuilder};
use crate::scene::Scene;
use crate::stats::{ImageCounters, MemorySample, RenderStats, SceneCounters};
use crust_rt::{
    CubicCurveSegment, CurveSegment, Geometry, Scene as RtScene, SceneBuilder as RtSceneBuilder,
};
use crate::filter::PixelFilter;
use crate::tracer::{RenderSettings, SamplingStrategy};
use crate::volume::{DensityField, VolumeRegion};
use glam::{Affine3A, Mat3A, Vec3, Vec3A};

use openusd::gf::{Matrix4d, Vec3f};
use openusd::schemas::geom::{
    BasisCurves as UsdBasisCurves, Camera as UsdCamera, Curves as UsdCurves, Mesh as UsdMesh,
    PointBased, PointInstancer, Sphere as UsdSphere, Xform, Xformable,
};
use openusd::schemas::lux::{
    CylinderLight, DiskLight, DistantLight as UsdDistantLight, DomeLight, Light as UsdLight,
    RectLight, SphereLight,
};
use openusd::schemas::render::{RenderSettings as UsdRenderSettings, RenderSettingsBase};
use openusd::schemas::shade::{self, Material as UsdMaterial, MaterialBindingAPI, Shader};
use openusd::sdf;
use openusd::usd::{InitialLoadSet, Prim, Stage, StagePopulationMask};

const DEFAULT_SPP: u32 = 128;
const DEFAULT_MAX_DEPTH: u32 = 32;
const DEFAULT_WIDTH: usize = 640;
const DEFAULT_HEIGHT: usize = 360;
const DEFAULT_MIN_SPP: u32 = 32;
const DEFAULT_VARIANCE: f32 = 0.05;
const DEFAULT_FRAME: isize = 0;
const DEFAULT_GUIDING_TRAIN_ITERATIONS: u32 = 4;
const DEFAULT_GUIDING_PROB: f32 = 0.5;
/// How deep prototypes may nest before the importer gives up. USD forbids
/// an instancing cycle, but a malformed stage can still describe one, and
/// each level multiplies traversal cost — so this is a backstop, set far
/// above any plausible authoring depth.
const MAX_INSTANCE_NESTING: usize = 8;

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
    if std::env::var("CRUST_STREAM_IMPORT").is_ok_and(|v| v == "0") {
        debug!("Streaming import disabled by CRUST_STREAM_IMPORT=0");
        return Vec::new();
    }
    let pseudo_root = stage.prim(sdf::Path::abs_root());
    let Ok(top) = pseudo_root.children() else {
        return Vec::new();
    };

    let chunks: Vec<sdf::Path> = match top.as_slice() {
        [only] => match only.children() {
            Ok(children) => children.iter().map(|c| c.path().clone()).collect(),
            Err(_) => return Vec::new(),
        },
        many => many.iter().map(|p| p.path().clone()).collect(),
    };

    if chunks.len() < MIN_STREAM_CHUNKS {
        return Vec::new();
    }
    chunks
}

/// Everything a traversal accumulates, kept separate from the stage that
/// feeds it so several stages can be walked into the same scene and each
/// dropped when done — see [`traverse_into`].
struct ImportCtx<'a> {
    world: WorldBuilder,
    lights: LightList,
    volumes: Vec<VolumeRegion>,
    camera: Option<Camera>,
    caches: ImportCaches<'a>,
    /// Direct mesh placements whose representation is not yet decided, in
    /// traversal order. Drained by [`flush_meshes`] after the last chunk —
    /// the decision needs every chunk's placement counts, so it cannot be
    /// made while walking. Holds ~88 bytes per mesh prim, not per triangle.
    pending_meshes: Vec<MeshPlacement>,
    settings: RenderSettings,
    /// The stage file, for resolving asset paths against its directory.
    stage_path: &'a Path,
    assets: &'a dyn AssetLoader,
}

/// Walks `root` and its subtree, emitting geometry, lights and volumes
/// into `ctx`. Takes the stage by reference and keeps nothing borrowed
/// from it, so the caller may drop the stage afterwards and walk another.
fn traverse_into(stage: &Stage, root: Prim, root_xf: GMat4, ctx: &mut ImportCtx) {
    let mut stack: Vec<(Prim, GMat4)> = vec![(root, root_xf)];

    while let Some((prim, parent_world)) = stack.pop() {
        // `class` prims (and their descendants) describe geometry that
        // exists only to be referenced or instanced — they are never
        // rendered in their own right. Prototypes reach the same prims
        // through `collect_proto_parts`, which deliberately does not apply
        // this rule.
        if prim.is_abstract().unwrap_or(false) {
            debug!("Skipping abstract (class) prim {}", prim.path());
            continue;
        }
        // USD prunes an inactive prim and its whole namespace subtree from
        // the composed scene — the standard way a stage disables geometry
        // (e.g. an LOD or a too-dense archive) without editing its source.
        if !prim.is_active().unwrap_or(true) {
            debug!("Skipping inactive prim {}", prim.path());
            continue;
        }

        let local = local_matrix_at(stage, &prim);
        let resets = resets_xform_stack_at(stage, &prim);
        let this_world = if resets { local } else { parent_world * local };

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
            // Prototypes are conventionally authored beneath the
            // instancer; they are drawn through it, never on their own.
            continue;
        } else if custom_token(&prim, "crust:volume:type").is_some() {
            emit_volume(&prim, this_world, &mut ctx.volumes);
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
        } else if UsdCamera::get(stage, prim.path().clone())
            .ok()
            .flatten()
            .is_some()
        {
            if ctx.camera.is_none() {
                match build_camera(stage, &prim, &ctx.settings) {
                    Some(c) => {
                        info!("Imported USD camera at {}", prim.path());
                        ctx.camera = Some(c);
                    }
                    None => warn!("Failed to build camera from {}", prim.path()),
                }
            }
        } else if let Ok(Some(light)) = SphereLight::get(stage, prim.path().clone()) {
            emit_sphere_light(&mut ctx.world, &mut ctx.lights, &light, this_world);
        } else if let Ok(Some(light)) = RectLight::get(stage, prim.path().clone()) {
            emit_rect_light(&mut ctx.world, &mut ctx.lights, &light, this_world);
        } else if let Ok(Some(light)) = UsdDistantLight::get(stage, prim.path().clone()) {
            emit_distant_light(&mut ctx.lights, &light, this_world);
        } else if let Ok(Some(light)) = DomeLight::get(stage, prim.path().clone()) {
            emit_dome_light(
                &mut ctx.lights,
                &prim,
                &light,
                this_world,
                ctx.stage_path,
                ctx.assets,
                &mut ctx.caches.asset_time,
            );
        } else {
            warn_unsupported_light(stage, &prim);
        }

        // Recurse. We push children onto the stack unconditionally; the
        // per-prim dispatch above will pick up any typed schemas encountered.
        if let Ok(children) = prim.children() {
            for child in children {
                stack.push((child, this_world));
            }
        }
    }
}

/// Opens the stage with payloads loaded, optionally masked to one subtree.
fn open_stage(
    path: &Path,
    path_str: &str,
    mask: Option<sdf::Path>,
) -> Result<Stage, crate::Error> {
    let mut builder = Stage::builder().load(InitialLoadSet::LoadAll);
    if let Some(p) = mask {
        builder = builder.mask(StagePopulationMask::new([p]));
    }
    builder.open(path_str).map_err(|e| crate::Error::UsdOpen {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

pub(crate) fn load_scene(path: &Path, assets: &dyn AssetLoader) -> Result<Scene, crate::Error> {
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
    let index = Stage::builder()
        .load(InitialLoadSet::LoadNone)
        .open(path_str)
        .map_err(|e| crate::Error::UsdOpen {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
    // Render settings come first — the camera importer needs the aspect ratio.
    let settings = import_render_settings(&index);
    let chunks = stream_roots(&index);
    drop(index);
    let open_elapsed = open_start.elapsed();
    let open_mem = MemorySample::now();

    let mut ctx = ImportCtx {
        world: WorldBuilder::new(),
        lights: LightList::new(),
        volumes: Vec::new(),
        camera: None,
        // Prims binding the same material path share one Arc, and prims
        // with identical local geometry + material share one copy of that
        // geometry — placed by an instance when it is placed more than once,
        // baked flat into the parent BVH when it is placed exactly once.
        caches: ImportCaches::new(assets, path),
        pending_meshes: Vec::new(),
        settings,
        stage_path: path,
        assets,
    };

    let traverse_start = Instant::now();
    if chunks.is_empty() {
        // Small or flat stage: one pass, exactly as before.
        let stage = open_stage(path, path_str, None)?;
        traverse_into(
            &stage,
            stage.prim(sdf::Path::abs_root()),
            GMat4::IDENTITY,
            &mut ctx,
        );
    } else {
        debug!("Streaming import over {} subtrees", chunks.len());
        for chunk in &chunks {
            let stage = open_stage(path, path_str, Some(chunk.clone()))?;
            // Traverse from the root, not from `chunk`: a mask keeps the
            // masked path's *ancestors* populated, so starting at the
            // root picks up their transforms exactly as a full traversal
            // would, while everything outside the chunk stays absent.
            traverse_into(
                &stage,
                stage.prim(sdf::Path::abs_root()),
                GMat4::IDENTITY,
                &mut ctx,
            );
            // Separate this stage's prototypes from the next stage's —
            // see ImportCaches::epoch. Deliberately not a clear: the
            // mesh cache keys materials by Arc address, so nothing may
            // be freed while it is live.
            ctx.caches.epoch += 1;
            ctx.caches.materials.epoch = ctx.caches.epoch;
        }
    }

    // The traverse also builds each mesh's and prototype's kernel scene,
    // so its own BVH work is inside this figure; the separate "Commit
    // acceleration structure" phase below is the *top-level* build.
    // Host asset decoding (environment maps, Ptex) happens *during* traversal but
    // is reported as its own phase, so it comes back out of this figure.
    let asset_time = ctx.caches.asset_time;
    let traverse_elapsed = traverse_start.elapsed().saturating_sub(asset_time);
    let traverse_mem = MemorySample::now();

    let camera = ctx.camera.unwrap_or_else(|| {
        warn!("USD stage has no UsdGeomCamera — falling back to world::get_settings camera");
        crate::world::get_settings().0
    });

    // Every chunk has been walked, so each mesh's placement count is final
    // and the deferred instance-vs-bake decisions can be made. Must happen
    // before `commit`, which is what consumes the geometry table.
    let pending = std::mem::take(&mut ctx.pending_meshes);
    flush_meshes(&mut ctx.world, &mut ctx.caches.meshes, pending);

    let commit_start = Instant::now();
    let committed = ctx.world.commit();
    let commit_elapsed = commit_start.elapsed();
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
        lights: ctx.lights.count(),
        volumes: ctx.volumes.len(),
    };
    let (w, h) = settings.get_dimensions();
    stats.image = ImageCounters {
        width: w,
        height: h,
        samples_per_pixel: settings.samples_per_pixel(),
        max_depth: settings.max_depth(),
    };

    let mut scene =
        Scene::new(camera, committed, ctx.lights, settings).with_volumes(ctx.volumes);
    scene.stats = stats;
    Ok(scene)
}

// -----------------------------------------------------------------------
// Volumes
// -----------------------------------------------------------------------

/// Import a `crust:volume:*` prim as a `VolumeRegion`. The local box is
/// `[-size/2, size/2]^3` when the prim authors a `size` attribute (a
/// `Cube`'s convention; USD's default cube size is 2) and the unit cube
/// `[-0.5, 0.5]^3` otherwise; placement, orientation and scale come from
/// the composed prim transform.
fn emit_volume(prim: &Prim, world_xf: GMat4, volumes: &mut Vec<VolumeRegion>) {
    let ty = custom_token(prim, "crust:volume:type").expect("checked by dispatch");

    let field = match ty.as_str() {
        "homogeneous" => DensityField::Homogeneous,
        "smoke" => DensityField::Noise {
            scale: custom_f32(prim, "crust:volume:noiseScale").unwrap_or(4.0),
            octaves: custom_i32(prim, "crust:volume:noiseOctaves").unwrap_or(4).max(1) as u32,
            gain: custom_f32(prim, "crust:volume:noiseGain").unwrap_or(0.5),
            lacunarity: custom_f32(prim, "crust:volume:noiseLacunarity").unwrap_or(2.0),
            threshold: custom_f32(prim, "crust:volume:noiseThreshold").unwrap_or(0.3),
            seed: custom_i32(prim, "crust:volume:noiseSeed").unwrap_or(0) as u32,
        },
        "grid" => {
            let dims = custom_i32_array(prim, "crust:volume:gridDims");
            let data = custom_f32_array(prim, "crust:volume:gridData");
            match (dims, data) {
                (Some(d), Some(data)) if d.len() == 3 => {
                    let (nx, ny, nz) = (d[0].max(1) as usize, d[1].max(1) as usize, d[2].max(1) as usize);
                    if nx * ny * nz != data.len() {
                        warn!(
                            "Volume at {}: gridDims {}x{}x{} does not match gridData length {} — skipped",
                            prim.path(), nx, ny, nz, data.len()
                        );
                        return;
                    }
                    DensityField::Grid { nx, ny, nz, data }
                }
                _ => {
                    warn!(
                        "Volume at {}: grid type needs int[3] crust:volume:gridDims and float[] crust:volume:gridData — skipped",
                        prim.path()
                    );
                    return;
                }
            }
        }
        other => {
            warn!(
                "Volume at {}: unknown crust:volume:type \"{}\" (expected homogeneous | smoke | grid) — skipped",
                prim.path(),
                other
            );
            return;
        }
    };

    let sigma_s = custom_color3(prim, "crust:volume:sigmaS").unwrap_or(Vec3A::splat(0.5));
    let sigma_a = custom_color3(prim, "crust:volume:sigmaA").unwrap_or(Vec3A::ZERO);
    let emission = custom_color3(prim, "crust:volume:emission").unwrap_or(Vec3A::ZERO);
    let g = custom_f32(prim, "crust:volume:anisotropy").unwrap_or(0.0);
    let density_scale = custom_f32(prim, "crust:volume:densityScale").unwrap_or(1.0);
    let half = custom_f32(prim, "size").map_or(0.5, |s| s * 0.5);

    info!(
        "Imported {} volume at {} (densityScale={})",
        ty,
        prim.path(),
        density_scale
    );
    volumes.push(VolumeRegion::new(
        world_xf,
        Vec3A::splat(half),
        sigma_s,
        sigma_a,
        g,
        emission,
        density_scale,
        field,
    ));
}

// -----------------------------------------------------------------------
// Transform helpers
// -----------------------------------------------------------------------

/// USD authors 4x4 matrices as row-vector row-major (translation in the
/// last row, indices 12..15). glam::Mat4 is column-major with the
/// column-vector convention, so USD's row-major layout is exactly the
/// column-major layout of the transposed matrix — which is what we want
/// for M * v evaluation.
fn usd_mat_to_glam(m: Matrix4d) -> GMat4 {
    let a = m.0;
    GMat4::from_cols_array(&[
        a[0] as f32,
        a[1] as f32,
        a[2] as f32,
        a[3] as f32,
        a[4] as f32,
        a[5] as f32,
        a[6] as f32,
        a[7] as f32,
        a[8] as f32,
        a[9] as f32,
        a[10] as f32,
        a[11] as f32,
        a[12] as f32,
        a[13] as f32,
        a[14] as f32,
        a[15] as f32,
    ])
}

/// Local-to-parent transform of `prim`, composed from its authored
/// `xformOp:*` attributes by [`compose_xform_ops`].
///
/// This deliberately does NOT use openusd's `local_to_parent_transform`:
/// openusd 0.5.0 composes multi-op `xformOpOrder` stacks in the wrong
/// order (the authored translate comes back multiplied by the scale — e.g.
/// cornellbox's `pCube1`, translate `(0,2,0)` · scale 4, yielded
/// translation `(0,8,0)`), which made `samples/cornellbox.usda` render as
/// floating objects against sky. Stacks with an op we cannot decode fall
/// back to openusd's composition with a warning, so unusual scenes behave
/// no worse than before.
fn local_matrix_at(stage: &Stage, prim: &Prim) -> GMat4 {
    match compose_xform_ops(prim) {
        Some(m) => m,
        None => {
            warn!(
                "could not decode the xformOp stack at {} — falling back to openusd's \
                 composition (known to be wrong for multi-op stacks)",
                prim.path()
            );
            local_matrix_via_openusd(stage, prim)
        }
    }
}

/// Composes the prim's `xformOpOrder` stack into a local-to-parent matrix.
///
/// UsdGeomXformable semantics, in the column-vector convention used here:
/// for `xformOpOrder = [op1, op2, …, opN]` a point transforms as
/// `p' = M(op1)·M(op2)·…·M(opN)·p` — the last-listed op is applied to the
/// point first and the first-listed is outermost. (Maya's
/// `["xformOp:translate", "xformOp:scale"]` therefore scales points first
/// and translates last: the composed translation equals the authored
/// translate.)
///
/// Returns `None` if any op token or value cannot be decoded.
fn compose_xform_ops(prim: &Prim) -> Option<GMat4> {
    let order = match prim.attribute("xformOpOrder").get::<sdf::Value>() {
        Ok(Some(sdf::Value::TokenVec(order))) => order,
        Ok(Some(_)) => return None,
        // No order authored: authored xformOp attrs (if any) do not apply.
        _ => return Some(GMat4::IDENTITY),
    };

    let mut local = GMat4::IDENTITY;
    for token in &order {
        // Not a transform op: it truncates the inherited stack, which
        // `resets_xform_stack_at` reports separately.
        if token == "!resetXformStack!" {
            continue;
        }
        let (name, inverted) = match token.strip_prefix("!invert!") {
            Some(rest) => (rest, true),
            None => (token.as_str(), false),
        };
        let mut m = xform_op_matrix(prim, name)?;
        if inverted {
            m = m.inverse();
        }
        local *= m;
    }
    Some(local)
}

/// Matrix of a single `xformOp:<kind>[:<suffix>]` attribute on `prim`, or
/// `None` for op kinds/value types we do not support.
fn xform_op_matrix(prim: &Prim, name: &str) -> Option<GMat4> {
    let kind = name.strip_prefix("xformOp:")?;
    // Suffixes name op instances (`xformOp:translate:pivot`); the kind is
    // the first segment.
    let kind = kind.split(':').next().unwrap_or(kind);
    let value = prim.attribute(name).get::<sdf::Value>().ok().flatten()?;

    match kind {
        "translate" => Some(GMat4::from_translation(value_as_vec3(&value)?)),
        "scale" => Some(GMat4::from_scale(value_as_vec3(&value)?)),
        "transform" => match value {
            sdf::Value::Matrix4d(m) => Some(usd_mat_to_glam(m)),
            _ => None,
        },
        "orient" => match value {
            sdf::Value::Quatf(q) => Some(GMat4::from_quat(
                glam::Quat::from_xyzw(q.x, q.y, q.z, q.w).normalize(),
            )),
            sdf::Value::Quatd(q) => Some(GMat4::from_quat(
                glam::Quat::from_xyzw(q.x as f32, q.y as f32, q.z as f32, q.w as f32).normalize(),
            )),
            _ => None,
        },
        "rotateX" => Some(GMat4::from_rotation_x(value_as_f32(&value)?.to_radians())),
        "rotateY" => Some(GMat4::from_rotation_y(value_as_f32(&value)?.to_radians())),
        "rotateZ" => Some(GMat4::from_rotation_z(value_as_f32(&value)?.to_radians())),
        // Euler triples: the vector components are always the X/Y/Z-axis
        // angles in degrees; the op name gives the application order, first
        // named axis applied to the point first (so it sits rightmost).
        "rotateXYZ" | "rotateXZY" | "rotateYXZ" | "rotateYZX" | "rotateZXY" | "rotateZYX" => {
            let v = value_as_vec3(&value)?;
            let rx = GMat4::from_rotation_x(v.x.to_radians());
            let ry = GMat4::from_rotation_y(v.y.to_radians());
            let rz = GMat4::from_rotation_z(v.z.to_radians());
            Some(match kind {
                "rotateXYZ" => rz * ry * rx,
                "rotateXZY" => ry * rz * rx,
                "rotateYXZ" => rz * rx * ry,
                "rotateYZX" => rx * rz * ry,
                "rotateZXY" => ry * rx * rz,
                _ => rx * ry * rz, // rotateZYX
            })
        }
        _ => None,
    }
}

fn value_as_vec3(value: &sdf::Value) -> Option<Vec3> {
    match value {
        sdf::Value::Vec3f(v) => Some(Vec3::new(v.x, v.y, v.z)),
        sdf::Value::Vec3d(v) => Some(Vec3::new(v.x as f32, v.y as f32, v.z as f32)),
        sdf::Value::Vec3h(v) => Some(Vec3::new(v.x.to_f32(), v.y.to_f32(), v.z.to_f32())),
        _ => None,
    }
}

fn value_as_f32(value: &sdf::Value) -> Option<f32> {
    match value {
        sdf::Value::Float(v) => Some(*v),
        sdf::Value::Double(v) => Some(*v as f32),
        sdf::Value::Half(v) => Some(v.to_f32()),
        _ => None,
    }
}

/// openusd's own composition, kept as the fallback for op stacks
/// `compose_xform_ops` cannot decode. Known to compose multi-op stacks in
/// the wrong order (see `local_matrix_at`).
fn local_matrix_via_openusd(stage: &Stage, prim: &Prim) -> GMat4 {
    if let Ok(Some(x)) = Xform::get(stage, prim.path().clone()) {
        if let Ok(m) = x.local_to_parent_transform(0.0) {
            return usd_mat_to_glam(m);
        }
    }
    if let Ok(Some(m)) = UsdMesh::get(stage, prim.path().clone()) {
        if let Ok(mat) = m.local_to_parent_transform(0.0) {
            return usd_mat_to_glam(mat);
        }
    }
    if let Ok(Some(s)) = UsdSphere::get(stage, prim.path().clone()) {
        if let Ok(mat) = s.local_to_parent_transform(0.0) {
            return usd_mat_to_glam(mat);
        }
    }
    if let Ok(Some(c)) = UsdCamera::get(stage, prim.path().clone()) {
        if let Ok(mat) = c.local_to_parent_transform(0.0) {
            return usd_mat_to_glam(mat);
        }
    }
    if let Ok(Some(l)) = SphereLight::get(stage, prim.path().clone()) {
        if let Ok(mat) = l.local_to_parent_transform(0.0) {
            return usd_mat_to_glam(mat);
        }
    }
    if let Ok(Some(l)) = RectLight::get(stage, prim.path().clone()) {
        if let Ok(mat) = l.local_to_parent_transform(0.0) {
            return usd_mat_to_glam(mat);
        }
    }
    GMat4::IDENTITY
}

fn resets_xform_stack_at(stage: &Stage, prim: &Prim) -> bool {
    if let Ok(Some(x)) = Xform::get(stage, prim.path().clone()) {
        return x.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(m)) = UsdMesh::get(stage, prim.path().clone()) {
        return m.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(s)) = UsdSphere::get(stage, prim.path().clone()) {
        return s.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(c)) = UsdCamera::get(stage, prim.path().clone()) {
        return c.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(l)) = SphereLight::get(stage, prim.path().clone()) {
        return l.resets_xform_stack().unwrap_or(false);
    }
    if let Ok(Some(l)) = RectLight::get(stage, prim.path().clone()) {
        return l.resets_xform_stack().unwrap_or(false);
    }
    false
}

// -----------------------------------------------------------------------
// Per-prim geometry attributes (visibility mask, motion)
// -----------------------------------------------------------------------

/// `crust:rayMask` — which ray categories see this geometry (bit 0 camera,
/// bit 1 shadow, bit 2 indirect; default: all). E.g. `crust:rayMask = 6`
/// makes a light-blocker invisible to the camera.
fn prim_ray_mask(prim: &Prim) -> u32 {
    custom_i32(prim, "crust:rayMask")
        .map(|m| m as u32)
        .unwrap_or(MASK_ALL)
}

/// `crust:motion:translate` — a world-space translation the prim moves
/// through over the shutter interval (transform motion blur).
fn prim_motion_translate(prim: &Prim) -> Option<Vec3> {
    custom_color3(prim, "crust:motion:translate").map(|v| Vec3::new(v.x, v.y, v.z))
}

// -----------------------------------------------------------------------
// Mesh
// -----------------------------------------------------------------------

/// Identity of an imported mesh's shared geometry: a content hash of the
/// authored points/counts/indices plus the (memoized, so pointer-comparable)
/// material. Prims agreeing on all of it share one local-space triangle BVH.
#[derive(PartialEq, Eq, Hash)]
struct MeshKey {
    geo_hash: u64,
    n_points: usize,
    n_indices: usize,
    material: usize,
}

impl MeshKey {
    fn new(
        points: &[Vec3f],
        counts: &[i32],
        indices: &[i32],
        material: &Arc<dyn Material>,
    ) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in points {
            p.x.to_bits().hash(&mut h);
            p.y.to_bits().hash(&mut h);
            p.z.to_bits().hash(&mut h);
        }
        counts.hash(&mut h);
        indices.hash(&mut h);
        MeshKey {
            geo_hash: h.finish(),
            n_points: points.len(),
            n_indices: indices.len(),
            material: Arc::as_ptr(material) as *const u8 as usize,
        }
    }
}

/// Local-space triangles of one distinct mesh, held only while that mesh
/// might still be baked flat into the parent BVH rather than instanced.
struct MeshGeom {
    verts: Vec<Vec3A>,
    tris: Vec<[u32; 3]>,
}

/// What the importer knows about one distinct mesh (one [`MeshKey`]).
struct MeshSlot {
    /// The triangles, kept until the instance-vs-bake decision is made.
    /// Dropped as soon as `committed` is set: a committed slot is already
    /// resident as a kernel scene, so every placement of it instances and
    /// baking would only add a second copy.
    local: Option<MeshGeom>,
    /// Triangle-to-source-face table, for a mesh whose material samples a
    /// per-face texture. Lives here rather than on [`MeshGeom`] because it has
    /// to outlive both endings — baking *and* committing drop `local`, but
    /// every placement still needs to resolve face ids at render time. Shared
    /// by `Arc` across the placements of one distinct mesh.
    faces: Option<Arc<FaceMap>>,
    /// Set once some path needed this mesh as a real kernel scene — which
    /// prototypes always do, since an instance is the only way to place one.
    committed: Option<Arc<RtScene>>,
    /// Instanceable direct placements recorded so far. World-space-baked
    /// placements (a non-invertible transform) are not counted: they never
    /// reference the slot again.
    n_place: u32,
}

/// Distinct meshes seen so far, and the index of each by content.
///
/// Replaces a bare `HashMap<MeshKey, Arc<RtScene>>`: the same content-hash
/// deduplication, but a mesh's *representation* is no longer decided the
/// moment it is first seen.
#[derive(Default)]
struct MeshArena {
    /// Indexed by the `u32` in `by_key`. Iterate this, never `by_key` — a
    /// `HashMap`'s order is not stable and the build must be deterministic.
    slots: Vec<MeshSlot>,
    by_key: HashMap<MeshKey, u32>,
}

/// A direct mesh prim whose geometry is recorded but not yet attached.
struct MeshPlacement {
    /// Reserved during traversal, so ids keep their traversal order.
    geom_id: u32,
    /// Index into [`MeshArena::slots`].
    slot: u32,
    l2w: Affine3A,
    motion: Option<Vec3>,
}

impl MeshArena {
    /// Interns a mesh by content, returning its slot index. Triangulates on
    /// first sight; `None` if nothing survives triangulation (matching the
    /// old behaviour, which also did not cache a failed mesh).
    fn intern(
        &mut self,
        prim: &Prim,
        points: &[Vec3f],
        counts: &[i32],
        indices: &[i32],
        material: &Arc<dyn Material>,
    ) -> Option<u32> {
        let key = MeshKey::new(points, counts, indices, material);
        if let Some(&slot) = self.by_key.get(&key) {
            debug!("Mesh at {} shares geometry with an earlier prim", prim.path());
            return Some(slot);
        }
        let verts: Vec<Vec3A> = points.iter().map(|p| Vec3A::new(p.x, p.y, p.z)).collect();
        let want_faces = material.face_texture().is_some();
        let (tris, faces) =
            triangulate(counts, indices, verts.len(), want_faces).or_else(|| {
                debug!("Mesh at {} produced no triangles", prim.path());
                None
            })?;
        check_face_count(prim, counts, material.as_ref());
        let slot = self.slots.len() as u32;
        self.slots.push(MeshSlot {
            local: Some(MeshGeom { verts, tris }),
            faces: faces.map(Arc::new),
            committed: None,
            n_place: 0,
        });
        self.by_key.insert(key, slot);
        Some(slot)
    }

    /// The slot's geometry as a committed local-space kernel scene, built on
    /// first demand and shared thereafter. Once this is called the slot can
    /// no longer be baked — see [`MeshSlot::local`].
    fn committed_scene(&mut self, slot: u32) -> Arc<RtScene> {
        let s = &mut self.slots[slot as usize];
        if let Some(scene) = &s.committed {
            return Arc::clone(scene);
        }
        let geom = s
            .local
            .take()
            .expect("a slot is either still local or already committed");
        let mut b = RtSceneBuilder::new();
        b.attach(Geometry::TriangleMesh {
            vertices: geom.verts,
            indices: geom.tris,
            normals: None,
        });
        let scene = Arc::new(b.commit());
        s.committed = Some(Arc::clone(&scene));
        scene
    }
}

fn emit_mesh(
    world: &mut WorldBuilder,
    prim: &Prim,
    mesh: &UsdMesh,
    world_xf: GMat4,
    material: Arc<dyn Material>,
    meshes: &mut MeshArena,
    pending: &mut Vec<MeshPlacement>,
) {
    let Some((points, counts, indices)) = mesh_arrays(mesh) else {
        debug!(
            "Mesh at {} missing points / faceVertexCounts / faceVertexIndices — skipped",
            prim.path()
        );
        return;
    };

    let mask = prim_ray_mask(prim);
    let motion = prim_motion_translate(prim);

    // Non-invertible placements (a zero scale axis) cannot be instanced —
    // bake the degenerate transform into world-space triangles as before.
    if world_xf.determinant().abs() < 1e-12 {
        warn!(
            "Mesh at {} has a non-invertible transform — baking instead of instancing",
            prim.path()
        );
        if motion.is_some() {
            warn!(
                "Mesh at {}: crust:motion:translate is ignored on baked (non-invertible) geometry",
                prim.path()
            );
        }
        let verts: Vec<Vec3A> = points
            .iter()
            .map(|p| {
                let v = world_xf.transform_point3(Vec3::new(p.x, p.y, p.z));
                Vec3A::new(v.x, v.y, v.z)
            })
            .collect();
        let want_faces = material.face_texture().is_some();
        check_face_count(prim, &counts, material.as_ref());
        match triangulate(&counts, &indices, verts.len(), want_faces) {
            Some((tris, faces)) => {
                let geom_id = world.attach_masked(
                    Geometry::TriangleMesh {
                        vertices: verts,
                        indices: tris,
                        normals: None,
                    },
                    material,
                    mask,
                );
                // This path bakes world-space vertices directly without going
                // through `bake_indices`, so the winding — and with it the
                // barycentric order — is whatever the transform produced.
                if let Some(map) = faces {
                    world.set_face_map(geom_id, Arc::new(map), false);
                }
            }
            None => debug!("Mesh at {} produced no triangles", prim.path()),
        }
        return;
    }

    // Record the placement rather than attaching it. Whether this mesh is
    // better placed by an instance or baked into world-space triangles
    // depends on how many times it is placed in total, which is not known
    // until the whole stage has been walked — so claim the `geom_id` now (it
    // must keep its traversal order) and decide in `flush_meshes`.
    let Some(slot) = meshes.intern(prim, &points, &counts, &indices, &material) else {
        return;
    };
    meshes.slots[slot as usize].n_place += 1;

    let geom_id = world.reserve_slot(material, mask);
    pending.push(MeshPlacement {
        geom_id,
        slot,
        l2w: Affine3A::from_mat4(world_xf),
        motion,
    });
}

/// Turns the deferred [`MeshPlacement`]s into real geometry, now that every
/// mesh's placement count is final.
///
/// A mesh placed exactly once is baked into world-space triangles in the
/// parent BVH; anything placed more than once keeps one shared kernel scene
/// and an instance per placement.
///
/// Why baking a single placement is worth it: an instance costs every ray
/// that enters its box a transform into local space, a fresh ray/slab setup,
/// and a cold descent into a second tree — and the box the parent BVH sees is
/// the transformed AABB of the inner tree's root AABB, a box of a box, which
/// spatial splits cannot tighten. For geometry that exists in exactly one
/// place that buys nothing at all: there is no sharing to amortise it
/// against. It is also *less* memory, not more, since the inner tree's nodes,
/// leaf table and packets all go away and the triangles exist once either
/// way.
///
/// The decision has to be global — made after every streamed chunk, not
/// per chunk. Per-chunk would make it depend on stage layout (cornellbox
/// streams, so each of its meshes would look like a per-chunk singleton and
/// results would differ under `CRUST_STREAM_IMPORT=0`), and geometry
/// referenced from two elements of a production stage would get one resident
/// copy per element — a memory regression in exactly the case sharing exists
/// for.
///
/// `CRUST_MESH_BAKE=0` forces every placement to instance, i.e. the old
/// behaviour. That is the A/B switch: with it set the output must be
/// bit-identical, which is what separates "the deferral is wrong" from "the
/// baking changed something".
fn flush_meshes(world: &mut WorldBuilder, meshes: &mut MeshArena, pending: Vec<MeshPlacement>) {
    let bake_enabled = std::env::var("CRUST_MESH_BAKE").as_deref() != Ok("0");
    let mut baked = 0usize;
    let mut instanced = 0usize;

    for p in pending {
        let slot = &meshes.slots[p.slot as usize];
        // Bake only when this is the mesh's sole placement, nothing has
        // already made it resident as a kernel scene, and it does not move —
        // a baked mesh has no transform left to interpolate over the shutter.
        let bake = bake_enabled
            && slot.n_place == 1
            && slot.committed.is_none()
            && p.motion.is_none();

        let faces = slot.faces.clone();
        if bake {
            let geom = meshes.slots[p.slot as usize]
                .local
                .take()
                .expect("an unbaked, uncommitted slot still holds its triangles");
            world.set_geometry(
                p.geom_id,
                Geometry::TriangleMesh {
                    vertices: bake_verts(&geom.verts, &p.l2w),
                    indices: bake_indices(geom.tris, &p.l2w),
                    normals: None,
                },
            );
            // `bake_indices` swaps a triangle's second and third vertices for a
            // mirroring placement, which exchanges the barycentrics the kernel
            // reports — so the face lookup has to exchange them back.
            if let Some(map) = faces {
                world.set_face_map(p.geom_id, map, p.l2w.matrix3.determinant() < 0.0);
            }
            baked += 1;
        } else {
            let scene = meshes.committed_scene(p.slot);
            let l2w = p.l2w;
            world.set_geometry(
                p.geom_id,
                Geometry::Instance {
                    scene,
                    transform: l2w,
                    transform_end: p
                        .motion
                        .map(|v| Box::new(Affine3A::from_translation(v) * l2w)),
                },
            );
            // An instance keeps the prototype's own winding: the transform is
            // applied to the ray, not to the triangles, so no swap.
            if let Some(map) = faces {
                world.set_face_map(p.geom_id, map, false);
            }
            instanced += 1;
        }
    }

    if baked + instanced > 0 {
        debug!(
            "Direct meshes: {baked} baked flat, {instanced} instanced ({} distinct)",
            meshes.slots.len()
        );
    }
}

/// Local-space vertices into world space.
fn bake_verts(verts: &[Vec3A], l2w: &Affine3A) -> Vec<Vec3A> {
    verts.iter().map(|v| l2w.transform_point3a(*v)).collect()
}

/// Triangle winding for baked geometry, flipped under a mirroring transform.
///
/// The instanced path derives its geometric normal inside the prototype and
/// maps it out through the inverse transpose. Baking derives it from the
/// world-space vertices instead, and for `det(M) < 0` those disagree in sign:
/// `(p1−p0)×(p2−p0) = det(M)·(M⁻¹)ᵀn`. Left unhandled, every mirrored prim
/// would render inside-out — `front_face` inverted, which flips which side of
/// a refractive interface the ray thinks it is on. Swapping two indices
/// restores the original orientation.
///
/// (This also swaps the roles of the barycentric `u`/`v` a hit reports.
/// Nothing reads them today — `HitRecord` carries no UVs — but whoever adds
/// texture coordinates needs to know.)
fn bake_indices(mut tris: Vec<[u32; 3]>, l2w: &Affine3A) -> Vec<[u32; 3]> {
    if l2w.matrix3.determinant() < 0.0 {
        for t in &mut tris {
            t.swap(1, 2);
        }
    }
    tris
}

/// Reads a mesh prim's authored arrays. `None` when any of the three
/// required attributes is missing.
fn mesh_arrays(mesh: &UsdMesh) -> Option<(Vec<Vec3f>, Vec<i32>, Vec<i32>)> {
    let int_vec = |v: sdf::Value| match v {
        sdf::Value::IntVec(v) => Some(v),
        _ => None,
    };
    let points = match mesh.points_attr().get::<sdf::Value>().ok().flatten()? {
        sdf::Value::Vec3fVec(v) => v,
        _ => return None,
    };
    let counts = int_vec(mesh.face_vertex_counts_attr().get::<sdf::Value>().ok().flatten()?)?;
    let indices = int_vec(mesh.face_vertex_indices_attr().get::<sdf::Value>().ok().flatten()?)?;
    Some((points, counts, indices))
}

/// Warns when a per-face texture's face count disagrees with the mesh's.
///
/// A Ptex face id *is* a mesh face index, so the two counts must match
/// exactly. When they do not, the texture belongs to different geometry and
/// every lookup is quietly wrong — the render still completes, and still looks
/// like a plausible rock, which is precisely what makes it worth a warning.
fn check_face_count(prim: &Prim, counts: &[i32], material: &dyn Material) {
    let Some(tex) = material.face_texture() else {
        return;
    };
    if tex.num_faces() != counts.len() {
        warn!(
            "Mesh at {} has {} faces but its per-face texture has {} — \
             the texture does not match this geometry, so shading will be wrong",
            prim.path(),
            counts.len(),
            tex.num_faces()
        );
    }
}

/// Fan-triangulates the faces into an index-triple list; `None` if
/// nothing survives.
///
/// With `want_faces`, also returns the table mapping each emitted triangle
/// back to the source face it was cut from — what a per-face (Ptex) texture
/// needs, since its face ids index `counts`, not the triangles. The two
/// outputs are index-parallel by construction: every `push` to one pushes to
/// the other in the same statement, so the skip paths cannot desynchronise
/// them.
fn triangulate(
    counts: &[i32],
    indices: &[i32],
    n_verts: usize,
    want_faces: bool,
) -> Option<(Vec<[u32; 3]>, Option<FaceMap>)> {
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut faces: Vec<u32> = Vec::new();
    let mut slices: Vec<FanSlice> = Vec::new();
    let mut offset = 0usize;
    for (face, &fc) in counts.iter().enumerate() {
        let fc = fc as usize;
        if fc < 3 || offset + fc > indices.len() {
            offset += fc;
            continue;
        }
        for k in 1..(fc - 1) {
            let i0 = indices[offset];
            let i1 = indices[offset + k];
            let i2 = indices[offset + k + 1];
            if i0 < 0 || i1 < 0 || i2 < 0 {
                continue;
            }
            let (i0, i1, i2) = (i0 as u32, i1 as u32, i2 as u32);
            if i0 as usize >= n_verts || i1 as usize >= n_verts || i2 as usize >= n_verts {
                continue;
            }
            tris.push([i0, i1, i2]);
            if want_faces {
                faces.push(face as u32);
                // Ptex defines quad and triangle faces only, so a larger
                // polygon has no addressable texture — mark it rather than
                // inventing a parameterisation for it.
                slices.push(match (fc, k) {
                    (3, _) => FanSlice::Triangle,
                    (4, 1) => FanSlice::QuadLower,
                    (4, 2) => FanSlice::QuadUpper,
                    _ => FanSlice::Unmappable,
                });
            }
        }
        offset += fc;
    }
    if tris.is_empty() {
        return None;
    }
    // `then_some` rather than `then`: the value is two already-built vectors
    // being moved, so there is no work for a closure to defer.
    let map = want_faces.then_some(FaceMap { faces, slices });
    Some((tris, map))
}

// -----------------------------------------------------------------------
// Sphere
// -----------------------------------------------------------------------

/// The authored `radius`, defaulting to USD's 1.0.
fn sphere_radius(sphere: &UsdSphere) -> f32 {
    sphere
        .radius_attr()
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::Double(d) => Some(d as f32),
            sdf::Value::Float(f) => Some(f),
            _ => None,
        })
        .unwrap_or(1.0)
}

fn emit_sphere(
    world: &mut WorldBuilder,
    prim: &Prim,
    sphere: &UsdSphere,
    world_xf: GMat4,
    material: Arc<dyn Material>,
) {
    let radius = sphere_radius(sphere);
    let center_world = world_xf.transform_point3(Vec3::ZERO);
    let center = Vec3A::new(center_world.x, center_world.y, center_world.z);
    debug!(
        "Sphere at {} radius={} center={:?}",
        prim.path(),
        radius,
        center
    );
    let mask = prim_ray_mask(prim);
    match prim_motion_translate(prim) {
        // A moving sphere rides an identity-placed instance whose end
        // transform is the shutter translation.
        Some(v) => {
            let mut b = RtSceneBuilder::new();
            b.attach(Geometry::Sphere { center, radius });
            world.attach_masked(
                Geometry::Instance {
                    scene: Arc::new(b.commit()),
                    transform: Affine3A::IDENTITY,
                    transform_end: Some(Box::new(Affine3A::from_translation(v))),
                },
                material,
                mask,
            );
        }
        None => {
            world.attach_masked(Geometry::Sphere { center, radius }, material, mask);
        }
    }
}

// -----------------------------------------------------------------------
// Instancing: prototypes
//
// Both instancing mechanisms — `UsdGeomPointInstancer` and native
// `instanceable` prims — reduce to the same thing: build a prototype's
// geometry *once*, then place it many times by transform. The shared
// currency is a [`ProtoPart`]: one leaf geometry of the prototype, held as
// a committed kernel scene in its own local space.
//
// The split into parts (rather than one scene per prototype) exists
// because `World` maps materials per top-level geometry: a prototype whose
// subtree binds two materials has to become two instances, or one of the
// materials would be lost. Instances are cheap — a transform and a
// pointer — so this costs a little top-level BVH and buys correct shading.
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
    /// `(epoch, prototype path)` → its parts, for both instancing
    /// mechanisms. See [`ImportCaches::epoch`] for the epoch.
    protos: HashMap<(u32, String), Arc<Vec<ProtoPart>>>,
    /// Which stage the entries above came from.
    ///
    /// Prototype paths (`/__Prototype_N`) are numbered per composition, so
    /// under the streaming importer the same name denotes different
    /// geometry in each stage. Bumping the epoch between stages keeps
    /// those apart.
    ///
    /// It bumps rather than clearing because [`MeshKey`] identifies a
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
}

impl<'a> ImportCaches<'a> {
    fn new(assets: &'a dyn AssetLoader, stage_path: &'a Path) -> Self {
        ImportCaches {
            materials: MaterialCache::default(),
            meshes: MeshArena::default(),
            protos: HashMap::new(),
            epoch: 0,
            assets,
            stage_path,
            asset_time: Duration::ZERO,
        }
    }
}

/// One leaf geometry of a prototype: a committed kernel scene in its own
/// local space, the transform placing it relative to the prototype root,
/// and the material and visibility mask authored on it.
#[derive(Clone)]
struct ProtoPart {
    scene: Arc<RtScene>,
    /// Prototype-root-relative placement. An instance's world transform is
    /// composed onto the left of this.
    local: GMat4,
    material: Arc<dyn Material>,
    mask: u32,
    /// Triangle-to-source-face table, when this part's material samples a
    /// per-face texture.
    ///
    /// A part is always exactly one leaf geometry — the walk splits per bound
    /// mesh, and a nested instancer groups its output per (prototype, part) —
    /// so one table serves every placement of it, and the `prim_id` a hit
    /// reports indexes that table unambiguously however many levels of
    /// instancing it passed through (the kernel forwards the innermost
    /// `prim_id` unchanged).
    faces: Option<Arc<FaceMap>>,
}

/// Walks a prototype subtree and builds its [`ProtoPart`]s, in the
/// prototype root's local space (the root itself contributes no
/// transform — an instance supplies the placement).
///
/// Abstract (`class`) prims are *not* skipped here, unlike in the main
/// traversal: naming a class as a prototype is exactly how one authors
/// "geometry that exists only to be instanced".
fn collect_proto_parts(
    stage: &Stage,
    root: &Prim,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Vec<ProtoPart> {
    let mut parts = Vec::new();
    if depth > MAX_INSTANCE_NESTING {
        warn!(
            "Prototype {} exceeds {MAX_INSTANCE_NESTING} levels of instance nesting — not expanded",
            root.path()
        );
        return parts;
    }
    let mut stack: Vec<(Prim, GMat4)> = vec![(root.clone(), GMat4::IDENTITY)];

    while let Some((prim, parent_local)) = stack.pop() {
        // Same pruning as the top-level traversal: an inactive prim (and
        // its subtree) is absent from the composed scene, prototype or not.
        if !prim.is_active().unwrap_or(true) {
            debug!("Skipping inactive prim {} (prototype {})", prim.path(), root.path());
            continue;
        }

        // The prototype root's own transform is deliberately excluded: a
        // `PointInstancer` prototype is placed entirely by its per-instance
        // transform, and a native prototype root carries none.
        let this_local = if prim.path() == root.path() {
            GMat4::IDENTITY
        } else if resets_xform_stack_at(stage, &prim) {
            local_matrix_at(stage, &prim)
        } else {
            parent_local * local_matrix_at(stage, &prim)
        };

        let mask = prim_ray_mask(&prim);

        // Checked before any schema lookup, because a schema `get()` reads
        // the prim's type name and that is exactly what aborts here.
        //
        // A natively-instanced prim *inside* a prototype is unreachable
        // with openusd 0.5.0: resolving its prototype, or reading the type
        // of any prim beneath it, trips an internal assertion
        // (`pcp/instancing.rs`: "materialized prototype root's
        // instanceable must be inert"), which aborts debug builds. The
        // prim itself is safe to inspect; its contents are not. So there
        // is no route to the geometry — not the prototype, not the proxy
        // subtree — and the honest response is to say so and move on
        // rather than abort. Nested *PointInstancer* is unaffected and is
        // expanded below.
        //
        // Four-line repro and the full diagnosis live in
        // `nested_native_instance_degrades_gracefully` in
        // `crates/crust-core/tests/usd_scene.rs`. Delete this arm when
        // upstream is fixed; `collect_proto_parts` can then splice the
        // inner prototype's parts in with composed transforms.
        if prim.path() != root.path() && prim.is_instance().unwrap_or(false) {
            warn!(
                "Nested native instance at {} skipped: openusd 0.5 cannot read \
                 an instanceable prim's contents inside a prototype. Author it \
                 as a PointInstancer, or flatten the inner instance.",
                prim.path()
            );
            continue;
        }

        if let Ok(Some(mesh)) = UsdMesh::get(stage, prim.path().clone()) {
            let material = resolve_material(stage, &prim, caches);
            // A prototype part is placed by an instance by definition, so it
            // always needs a real kernel scene — committing here is also what
            // marks the slot as ineligible for baking, so a mesh used both
            // directly and as a prototype is not stored twice.
            if let Some((points, counts, indices)) = mesh_arrays(&mesh)
                && let Some(slot) =
                    caches
                        .meshes
                        .intern(&prim, &points, &counts, &indices, &material)
            {
                let faces = caches.meshes.slots[slot as usize].faces.clone();
                parts.push(ProtoPart {
                    scene: caches.meshes.committed_scene(slot),
                    local: this_local,
                    material,
                    mask,
                    faces,
                });
            }
        } else if let Ok(Some(sphere)) = UsdSphere::get(stage, prim.path().clone()) {
            let material = resolve_material(stage, &prim, caches);
            let radius = sphere_radius(&sphere);
            let mut b = RtSceneBuilder::new();
            // Local-space sphere at the origin: unlike the top-level
            // sphere path, which bakes the centre into world space, this
            // lets the instance transform scale it (a non-uniform scale
            // correctly yields an ellipsoid, since rays enter local space).
            b.attach(Geometry::Sphere {
                center: Vec3A::ZERO,
                radius,
            });
            parts.push(ProtoPart {
                scene: Arc::new(b.commit()),
                local: this_local,
                material,
                mask,
                faces: None,
            });
        } else if let Ok(Some(curves)) = UsdBasisCurves::get(stage, prim.path().clone()) {
            let material = resolve_material(stage, &prim, caches);
            if let Some((segments, cubic_segments)) = curve_segments(&prim, &curves) {
                let mut b = RtSceneBuilder::new();
                if !segments.is_empty() {
                    b.attach(Geometry::RoundCurves { segments });
                }
                if !cubic_segments.is_empty() {
                    b.attach(Geometry::CubicCurves {
                        segments: cubic_segments,
                    });
                }
                parts.push(ProtoPart {
                    scene: Arc::new(b.commit()),
                    local: this_local,
                    material,
                    mask,
                    faces: None,
                });
            }
        } else if custom_token(&prim, "crust:volume:type").is_some() {
            // Volumes live outside the surface BVH entirely (their bounds
            // must not occlude shadow rays), so they cannot ride an
            // instance transform. Say so rather than dropping silently.
            warn!(
                "Volume at {} is inside a prototype — volumes cannot be instanced, skipped",
                prim.path()
            );
        } else if let Ok(Some(instancer)) = PointInstancer::get(stage, prim.path().clone()) {
            // A PointInstancer inside a prototype: expand it into nested
            // sub-scenes rather than flattening. Flattening would multiply
            // the *outer* instance count by this instancer's, which is
            // exactly the blow-up instancing exists to avoid — a prototype
            // holding 500 leaves, itself placed 500 times, must stay 500
            // outer instances, not 250 000.
            parts.extend(nested_instancer_parts(
                stage,
                &prim,
                &instancer,
                this_local,
                mask,
                caches,
                depth + 1,
            ));
            // Its prototypes are reached through it, never drawn directly.
            continue;
        }

        if let Ok(children) = prim.children() {
            for child in children {
                stack.push((child, this_local));
            }
        }
    }
    parts
}

/// Expands a `PointInstancer` found *inside* a prototype into parts.
///
/// One part per prototype-part of the nested instancer, each holding a
/// committed sub-scene of that part placed once per nested instance. The
/// grouping is by material, not by instance, because `World` resolves
/// materials from the top-level `geom_id`: everything inside one part must
/// therefore share a material.
fn nested_instancer_parts(
    stage: &Stage,
    prim: &Prim,
    instancer: &PointInstancer,
    local: GMat4,
    mask: u32,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Vec<ProtoPart> {
    let Some(layout) = read_instancer(prim, instancer) else {
        return Vec::new();
    };
    let proto_parts = instancer_proto_parts(stage, &layout, caches, depth);

    // Group placements by (prototype, part), so each output part collects
    // every placement that draws that one piece of geometry.
    let mut out: Vec<ProtoPart> = Vec::new();
    for (k, parts) in proto_parts.iter().enumerate() {
        for part in parts.iter() {
            let mut sub = RtSceneBuilder::new();
            let mut placed = 0usize;
            for &(target, xf) in layout.placements.iter().filter(|(t, _)| *t == k) {
                let _ = target;
                let placement = xf * part.local;
                if placement.determinant().abs() < 1e-12 {
                    continue; // zero scale: the "hide this instance" idiom
                }
                sub.attach_masked(
                    Geometry::Instance {
                        scene: part.scene.clone(),
                        transform: Affine3A::from_mat4(placement),
                        transform_end: None,
                    },
                    part.mask,
                );
                placed += 1;
            }
            if placed == 0 {
                continue;
            }
            out.push(ProtoPart {
                scene: Arc::new(sub.commit()),
                local,
                material: part.material.clone(),
                mask,
                faces: part.faces.clone(),
            });
        }
    }

    info!(
        "Expanded nested PointInstancer at {} ({} instances -> {} part(s))",
        prim.path(),
        layout.placements.len(),
        out.len()
    );
    out
}

/// Imports one natively-instanced prim (`instanceable = true` plus a
/// composition arc) by placing its shared prototype's parts.
///
/// This is the mechanism Moana-scale scenes rely on, and the reason it
/// matters is memory: without it the importer walks each instance's proxy
/// subtree and re-reads its geometry, so cost scales with the *instance*
/// count. Here every instance of a prototype shares one set of committed
/// kernel scenes, and costs only its transforms.
fn emit_native_instance(
    stage: &Stage,
    world: &mut WorldBuilder,
    prim: &Prim,
    proto_path: &sdf::Path,
    world_xf: GMat4,
    caches: &mut ImportCaches<'_>,
) {
    let parts = prototype_parts(stage, proto_path, caches, 0);
    debug!(
        "Instance {} uses prototype {proto_path} ({} part(s))",
        prim.path(),
        parts.len()
    );
    attach_proto_parts(world, &parts, world_xf, "native instance");
}

/// Attaches every part of a prototype at `placement`, one instance each.
/// Non-invertible placements are skipped: the kernel's instance transform
/// must be invertible, and a zero scale is a common "hide this instance"
/// idiom rather than an error.
fn attach_proto_parts(
    world: &mut WorldBuilder,
    parts: &[ProtoPart],
    placement: GMat4,
    what: &str,
) -> usize {
    let mut attached = 0;
    for part in parts {
        let xf = placement * part.local;
        if xf.determinant().abs() < 1e-12 {
            debug!("{what}: non-invertible instance transform — skipped");
            continue;
        }
        let geom_id = world.attach_masked(
            Geometry::Instance {
                scene: part.scene.clone(),
                transform: Affine3A::from_mat4(xf),
                transform_end: None,
            },
            part.material.clone(),
            part.mask,
        );
        // An instance transforms the ray, not the triangles, so the winding —
        // and with it the barycentric order — is the prototype's own: no swap.
        if let Some(map) = &part.faces {
            world.set_face_map(geom_id, map.clone(), false);
        }
        attached += 1;
    }
    attached
}

/// Imports a `UsdGeomPointInstancer`: every entry of the per-instance
/// arrays places the prototype selected by `protoIndices`.
///
/// The per-instance transform is USD's `translate ∘ orient ∘ scale`
/// (spec: scale first, then orientation, then position), composed under
/// the instancer's own world transform. `invisibleIds` prunes instances by
/// `ids`; where `ids` is absent the array index is the id, as USD
/// specifies.
///
/// Memory is what this is for: N instances of a prototype cost one copy of
/// its geometry plus N transforms, instead of N baked copies.
/// A `PointInstancer`'s prototypes and the placements that select them,
/// resolved once and reused by both the top-level emitter and the nested
/// (inside-a-prototype) path.
struct InstancerLayout {
    /// The `prototypes` relationship's ordered targets.
    targets: Vec<sdf::Path>,
    /// Visible instances as `(index into targets, transform relative to
    /// the instancer)`. Instances hidden by `invisibleIds` are already
    /// dropped.
    placements: Vec<(usize, GMat4)>,
    /// How many instances `invisibleIds` removed, for reporting.
    hidden: usize,
}

/// Reads the per-instance arrays into placements. `None` when the prim is
/// not a usable instancer.
///
/// The transform is USD's `translate ∘ orient ∘ scale` — scale first, then
/// orientation, then position. `orientationsf` (single precision) wins over
/// `orientations` (half) where both are authored, and `invisibleIds`
/// prunes by `ids`, with the array index standing in as the id where `ids`
/// is absent.
fn read_instancer(prim: &Prim, instancer: &PointInstancer) -> Option<InstancerLayout> {
    let targets = match instancer.prototypes_rel().targets() {
        Ok(t) if !t.is_empty() => t,
        _ => {
            warn!(
                "PointInstancer at {} has no `prototypes` targets — skipped",
                prim.path()
            );
            return None;
        }
    };

    let Ok(Some(sdf::Value::IntVec(proto_indices))) =
        instancer.proto_indices_attr().get::<sdf::Value>()
    else {
        warn!(
            "PointInstancer at {} has no `protoIndices` — skipped",
            prim.path()
        );
        return None;
    };

    let positions = value_vec3f_array(&instancer.positions_attr()).unwrap_or_default();
    let scales = value_vec3f_array(&instancer.scales_attr());
    let orientations = instance_orientations(instancer);
    let ids = match instancer.ids_attr().get::<sdf::Value>() {
        Ok(Some(sdf::Value::Int64Vec(v))) => Some(v),
        _ => None,
    };
    let invisible: std::collections::HashSet<i64> =
        match instancer.invisible_ids_attr().get::<sdf::Value>() {
            Ok(Some(sdf::Value::Int64Vec(v))) => v.into_iter().collect(),
            _ => Default::default(),
        };

    if positions.len() < proto_indices.len() {
        warn!(
            "PointInstancer at {}: {} protoIndices but only {} positions — extra instances skipped",
            prim.path(),
            proto_indices.len(),
            positions.len()
        );
    }

    let mut placements = Vec::with_capacity(proto_indices.len());
    let mut hidden = 0usize;
    for (i, &proto_index) in proto_indices.iter().enumerate() {
        let Some(pos) = positions.get(i) else { break };

        let id = ids
            .as_ref()
            .map_or(i as i64, |ids| ids.get(i).copied().unwrap_or(i as i64));
        if invisible.contains(&id) {
            hidden += 1;
            continue;
        }

        let Some(k) = usize::try_from(proto_index).ok().filter(|k| *k < targets.len()) else {
            warn!(
                "PointInstancer at {}: protoIndices[{i}] = {proto_index} is out of range — instance skipped",
                prim.path()
            );
            continue;
        };

        let scale = scales
            .as_ref()
            .and_then(|s| s.get(i))
            .map_or(Vec3::ONE, |s| Vec3::new(s.x, s.y, s.z));
        let rotation = orientations
            .as_ref()
            .and_then(|q| q.get(i).copied())
            .unwrap_or(glam::Quat::IDENTITY);
        placements.push((
            k,
            GMat4::from_scale_rotation_translation(
                scale,
                rotation,
                Vec3::new(pos.x, pos.y, pos.z),
            ),
        ));
    }

    Some(InstancerLayout {
        targets,
        placements,
        hidden,
    })
}

/// The parts of each of an instancer's prototypes, built once and memoized
/// by path.
fn instancer_proto_parts(
    stage: &Stage,
    layout: &InstancerLayout,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Vec<Arc<Vec<ProtoPart>>> {
    layout
        .targets
        .iter()
        .map(|target| prototype_parts(stage, target, caches, depth))
        .collect()
}

/// A prototype's parts, from the cache or freshly built.
fn prototype_parts(
    stage: &Stage,
    proto_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Arc<Vec<ProtoPart>> {
    let key = (caches.epoch, proto_path.to_string());
    if let Some(parts) = caches.protos.get(&key) {
        return parts.clone();
    }
    let root = stage.prim(proto_path.clone());
    let parts = Arc::new(collect_proto_parts(stage, &root, caches, depth));
    if parts.is_empty() {
        warn!("Prototype {} contributed no geometry", key.1);
    }
    caches.protos.insert(key, parts.clone());
    parts
}

/// Imports a `UsdGeomPointInstancer`: every entry of the per-instance
/// arrays places the prototype selected by `protoIndices`.
///
/// Memory is what this is for: N instances of a prototype cost one copy of
/// its geometry plus N transforms, instead of N baked copies.
fn emit_point_instancer(
    stage: &Stage,
    world: &mut WorldBuilder,
    prim: &Prim,
    instancer: &PointInstancer,
    world_xf: GMat4,
    caches: &mut ImportCaches<'_>,
) {
    let Some(layout) = read_instancer(prim, instancer) else {
        return;
    };
    let proto_parts = instancer_proto_parts(stage, &layout, caches, 0);

    // A dense scatter can place millions of instances in this one call;
    // reserving the exact total up front avoids both the doubling-copy
    // cost and the over-allocation of growing the geometry table
    // incrementally (see `WorldBuilder::reserve`).
    let part_counts: Vec<usize> = proto_parts.iter().map(|p| p.len()).collect();
    let total_geometries: usize = layout.placements.iter().map(|&(k, _)| part_counts[k]).sum();
    world.reserve(total_geometries);

    let mut attached = 0usize;
    for &(k, xf) in &layout.placements {
        attached += attach_proto_parts(
            world,
            &proto_parts[k],
            world_xf * xf,
            "PointInstancer instance",
        );
    }

    info!(
        "Imported PointInstancer at {} ({} instances of {} prototype(s), {} geometries attached{})",
        prim.path(),
        layout.placements.len(),
        layout.targets.len(),
        attached,
        if layout.hidden > 0 {
            format!(", {} hidden by invisibleIds", layout.hidden)
        } else {
            String::new()
        }
    );
}

/// A `point3f[]` / `float3[]` attribute as a plain vector.
fn value_vec3f_array(attr: &openusd::usd::Attribute) -> Option<Vec<Vec3f>> {
    match attr.get::<sdf::Value>() {
        Ok(Some(sdf::Value::Vec3fVec(v))) => Some(v),
        _ => None,
    }
}

/// Per-instance rotations, preferring single-precision `orientationsf`
/// over half-precision `orientations` as USD specifies.
fn instance_orientations(instancer: &PointInstancer) -> Option<Vec<glam::Quat>> {
    let quat = |w: f32, x: f32, y: f32, z: f32| glam::Quat::from_xyzw(x, y, z, w).normalize();
    if let Ok(Some(sdf::Value::QuatfVec(v))) = instancer.orientationsf_attr().get::<sdf::Value>() {
        return Some(v.iter().map(|q| quat(q.w, q.x, q.y, q.z)).collect());
    }
    match instancer.orientations_attr().get::<sdf::Value>() {
        Ok(Some(sdf::Value::QuathVec(v))) => Some(
            v.iter()
                .map(|q| {
                    quat(
                        q.w.to_f32(),
                        q.x.to_f32(),
                        q.y.to_f32(),
                        q.z.to_f32(),
                    )
                })
                .collect(),
        ),
        _ => None,
    }
}

// -----------------------------------------------------------------------
// Curves
// -----------------------------------------------------------------------

/// Cubic basis matrices (row-major, `[t³ t² t 1] · M · [P0 P1 P2 P3]ᵀ`).
const BEZIER_M: [[f32; 4]; 4] = [
    [-1.0, 3.0, -3.0, 1.0],
    [3.0, -6.0, 3.0, 0.0],
    [-3.0, 3.0, 0.0, 0.0],
    [1.0, 0.0, 0.0, 0.0],
];
const BSPLINE_M: [[f32; 4]; 4] = [
    [-1.0 / 6.0, 3.0 / 6.0, -3.0 / 6.0, 1.0 / 6.0],
    [3.0 / 6.0, -6.0 / 6.0, 3.0 / 6.0, 0.0],
    [-3.0 / 6.0, 0.0, 3.0 / 6.0, 0.0],
    [1.0 / 6.0, 4.0 / 6.0, 1.0 / 6.0, 0.0],
];
const CATMULL_ROM_M: [[f32; 4]; 4] = [
    [-0.5, 1.5, -1.5, 0.5],
    [1.0, -2.5, 2.0, -0.5],
    [-0.5, 0.0, 0.5, 0.0],
    [0.0, 1.0, 0.0, 0.0],
];

/// Only `basis_to_bezier`'s tests evaluate a basis matrix directly now —
/// production code always converts to Bézier form first.
#[cfg(test)]
fn eval_cubic(m: &[[f32; 4]; 4], cp: &[Vec3A; 4], t: f32) -> Vec3A {
    let pow = [t * t * t, t * t, t, 1.0];
    let mut p = Vec3A::ZERO;
    for (row, &w) in m.iter().zip(&pow) {
        for (c, &coeff) in row.iter().enumerate() {
            p += cp[c] * (coeff * w);
        }
    }
    p
}

/// Converts 4 control points from a cubic basis (`BEZIER_M`, `BSPLINE_M`,
/// `CATMULL_ROM_M`) to the equivalent standard (Bernstein) Bézier control
/// points tracing the *same* curve. Needed because the analytic curve
/// intersector (`crust_rt::curve::cubic_curve_intersect`) subdivides via
/// de Casteljau, which only has its usual convex-hull/subdivision
/// properties in Bézier form.
///
/// `eval_cubic`'s row `r` gives the curve's monomial coefficient of `t^(3-r)`
/// (row 0 → t³, …, row 3 → the constant term) as `Σ_c m[r][c]·cp[c]`.
/// Matching those same four coefficients against the expansion of the
/// Bernstein basis — `B(t) = P0(1-t)³ + 3P1·t(1-t)² + 3P2·t²(1-t) + P3·t³`
/// — inverts cleanly to: `P0 = d`, `P1 = d + c/3`, `P2 = d + 2c/3 + b/3`,
/// `P3 = a+b+c+d`, where `a,b,c,d` are those coefficients of
/// `t³,t²,t,1`. Passing `BEZIER_M` itself through this is the identity
/// (pinned by `bezier_basis_round_trips_unchanged`).
fn basis_to_bezier(m: &[[f32; 4]; 4], cp: &[Vec3A; 4]) -> [Vec3A; 4] {
    let coeff = |row: usize| -> Vec3A {
        let mut s = Vec3A::ZERO;
        for c in 0..4 {
            s += cp[c] * m[row][c];
        }
        s
    };
    let (a, b, c, d) = (coeff(0), coeff(1), coeff(2), coeff(3));
    let p0 = d;
    let p1 = d + c / 3.0;
    let p2 = d + c * (2.0 / 3.0) + b / 3.0;
    let p3 = a + b + c + d;
    [p0, p1, p2, p3]
}

/// Import a `UsdGeomBasisCurves` batch as round (sphere-swept) curves:
/// `linear` curves directly as [`CurveSegment`]s, `cubic` curves (bezier /
/// bspline / catmullRom) as one [`CubicCurveSegment`] per span — its
/// control points converted to Bézier form (`basis_to_bezier`) and
/// intersected analytically (`crust_rt::curve::cubic_curve_intersect`)
/// rather than flattened to a polyline. A dense xgen-style archive (grass,
/// needles) attaches tens of millions of these; one primitive per
/// authored span instead of several flattened straight segments is the
/// difference between that fitting in memory and not.
///
/// Widths (diameters, per USD) may be authored per point (`vertex`), per
/// curve, or constant; anything else falls back to the first value. Both
/// vectors live in local space under an `Instance`, like meshes. Shared by
/// the top-level emitter and the prototype collector; the caller supplies
/// the placement.
fn curve_segments(
    prim: &Prim,
    curves: &UsdBasisCurves,
) -> Option<(Vec<CurveSegment>, Vec<CubicCurveSegment>)> {
    let points: Option<Vec<Vec3f>> = curves
        .points_attr()
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::Vec3fVec(v) => Some(v),
            _ => None,
        });
    let counts: Option<Vec<i32>> = curves
        .curve_vertex_counts_attr()
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::IntVec(v) => Some(v),
            _ => None,
        });
    let (points, counts) = match (points, counts) {
        (Some(p), Some(c)) => (p, c),
        _ => {
            debug!(
                "BasisCurves at {} missing points / curveVertexCounts — skipped",
                prim.path()
            );
            return None;
        }
    };
    let pts: Vec<Vec3A> = points.iter().map(|p| Vec3A::new(p.x, p.y, p.z)).collect();

    let widths: Vec<f32> = curves
        .widths_attr()
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::FloatVec(v) => Some(v),
            _ => None,
        })
        .unwrap_or_else(|| vec![1.0]);

    // USD defaults: type = cubic, basis = bezier.
    let ty = custom_token(prim, "type").unwrap_or_else(|| "cubic".to_string());
    let basis_name = custom_token(prim, "basis").unwrap_or_else(|| "bezier".to_string());
    let (basis, vstep) = match basis_name.as_str() {
        "bezier" => (&BEZIER_M, 3usize),
        "bspline" => (&BSPLINE_M, 1),
        "catmullRom" => (&CATMULL_ROM_M, 1),
        other => {
            warn!(
                "BasisCurves at {}: basis \"{}\" is not supported (bezier | bspline | catmullRom) — skipped",
                prim.path(),
                other
            );
            return None;
        }
    };

    // Width (diameter) of control point `global_idx` under the authored
    // interpolation, resolved structurally from the array length.
    let n_points = pts.len();
    let n_curves = counts.len();
    let width_of = |global_idx: usize, curve_idx: usize| -> f32 {
        if widths.len() == n_points {
            widths[global_idx] // vertex
        } else if widths.len() == n_curves {
            widths[curve_idx] // uniform (per curve)
        } else {
            widths[0] // constant / fallback
        }
    };

    let mut segments: Vec<CurveSegment> = Vec::new();
    let mut cubic_segments: Vec<CubicCurveSegment> = Vec::new();
    let mut offset = 0usize;
    for (curve_idx, &cnt) in counts.iter().enumerate() {
        let cnt = cnt as usize;
        if offset + cnt > pts.len() {
            warn!(
                "BasisCurves at {}: curveVertexCounts overruns points — remaining curves skipped",
                prim.path()
            );
            break;
        }
        let cp = &pts[offset..offset + cnt];
        let radius =
            |k: usize| 0.5 * width_of(offset + k, curve_idx).max(1e-6);

        if ty == "linear" {
            for k in 0..cnt.saturating_sub(1) {
                segments.push(CurveSegment {
                    p0: cp[k],
                    p1: cp[k + 1],
                    r0: radius(k),
                    r1: radius(k + 1),
                });
            }
        } else {
            // Cubic: one CubicCurveSegment per span. Span k uses control
            // points [k·vstep .. k·vstep+3], converted to Bézier form;
            // widths interpolate linearly over the curve parameter
            // between the span's end control points.
            if cnt < 4 {
                offset += cnt;
                continue;
            }
            let n_spans = (cnt - 4) / vstep + 1;
            for s in 0..n_spans {
                let base = s * vstep;
                let ctrl = [cp[base], cp[base + 1], cp[base + 2], cp[base + 3]];
                let (r0, r1) = (radius(base), radius(base + 3));
                cubic_segments.push(CubicCurveSegment {
                    cp: basis_to_bezier(basis, &ctrl),
                    r0,
                    r1,
                });
            }
        }
        offset += cnt;
    }

    if segments.is_empty() && cubic_segments.is_empty() {
        debug!("BasisCurves at {} produced no segments", prim.path());
        return None;
    }
    info!(
        "Imported BasisCurves at {} ({} {} curves, {} segments, {} cubic spans)",
        prim.path(),
        counts.len(),
        ty,
        segments.len(),
        cubic_segments.len()
    );
    Some((segments, cubic_segments))
}

fn emit_curves(
    world: &mut WorldBuilder,
    prim: &Prim,
    curves: &UsdBasisCurves,
    world_xf: GMat4,
    material: Arc<dyn Material>,
) {
    let Some((segments, cubic_segments)) = curve_segments(prim, curves) else {
        return;
    };
    if world_xf.determinant().abs() < 1e-12 {
        warn!(
            "BasisCurves at {} has a non-invertible transform — skipped",
            prim.path()
        );
        return;
    }
    let mut b = RtSceneBuilder::new();
    if !segments.is_empty() {
        b.attach(Geometry::RoundCurves { segments });
    }
    if !cubic_segments.is_empty() {
        b.attach(Geometry::CubicCurves {
            segments: cubic_segments,
        });
    }
    world.attach_masked(
        Geometry::Instance {
            scene: Arc::new(b.commit()),
            transform: Affine3A::from_mat4(world_xf),
            transform_end: None,
        },
        material,
        prim_ray_mask(prim),
    );
}

// -----------------------------------------------------------------------
// Camera
// -----------------------------------------------------------------------

fn build_camera(stage: &Stage, prim: &Prim, settings: &RenderSettings) -> Option<Camera> {
    let cam = UsdCamera::get(stage, prim.path().clone()).ok().flatten()?;
    let world = local_to_world(stage, prim);

    // USD camera looks down -Z with +Y up in local space.
    let lookfrom_v = world.transform_point3(Vec3::ZERO);
    let forward_v = world.transform_vector3(Vec3::NEG_Z).normalize();
    let up_v = world.transform_vector3(Vec3::Y).normalize();

    let focal_length = attr_f32(&cam.focal_length_attr()).unwrap_or(50.0);
    let horiz_aperture = attr_f32(&cam.horizontal_aperture_attr()).unwrap_or(20.955);
    let vert_aperture_authored = attr_f32(&cam.vertical_aperture_attr());
    let f_stop = attr_f32(&cam.f_stop_attr()).unwrap_or(0.0);
    let focus_distance = attr_f32(&cam.focus_distance_attr()).unwrap_or(10.0);

    let (w, h) = settings.get_dimensions();
    let (w_f, h_f) = (w as f32, h as f32);
    let vert_aperture = vert_aperture_authored.unwrap_or(horiz_aperture * h_f / w_f);

    let vfov_deg = 2.0 * (vert_aperture / (2.0 * focal_length)).atan().to_degrees();
    let aperture = if f_stop > 0.0 {
        focal_length / f_stop
    } else {
        0.0
    };

    let aspect = w_f / h_f;
    let lookfrom = Vec3A::new(lookfrom_v.x, lookfrom_v.y, lookfrom_v.z);
    let lookat_v = lookfrom_v + forward_v * focus_distance;
    let lookat = Vec3A::new(lookat_v.x, lookat_v.y, lookat_v.z);
    let vup = Vec3A::new(up_v.x, up_v.y, up_v.z);

    debug!(
        "USD camera: lookfrom={:?} lookat={:?} vup={:?} vfov={} aspect={} aperture={} focus={}",
        lookfrom, lookat, vup, vfov_deg, aspect, aperture, focus_distance
    );

    Some(Camera::new(
        lookfrom,
        lookat,
        vup,
        vfov_deg,
        aspect,
        aperture,
        focus_distance,
    ))
}

/// Composed local-to-world by walking the prim path upwards. Slower than
/// tracking it during DFS, but exact and only used at build_camera time.
fn local_to_world(stage: &Stage, prim: &Prim) -> GMat4 {
    let mut ancestors: Vec<Prim> = Vec::new();
    let mut cur_path = prim.path().clone();
    ancestors.push(stage.prim(cur_path.clone()));
    while let Some(parent) = cur_path.parent() {
        cur_path = parent;
        ancestors.push(stage.prim(cur_path.clone()));
        if cur_path.as_str() == "/" {
            break;
        }
    }
    ancestors.reverse();
    let mut acc = GMat4::IDENTITY;
    for p in &ancestors {
        let local = local_matrix_at(stage, p);
        let resets = resets_xform_stack_at(stage, p);
        acc = if resets { local } else { acc * local };
    }
    acc
}

// -----------------------------------------------------------------------
// Lights
// -----------------------------------------------------------------------

/// Effective emitted radiance of a lux light: color scaled by intensity and
/// exposure gain.
fn lux_emission(light: &impl UsdLight) -> Vec3A {
    let intensity = attr_f32(&light.intensity_attr()).unwrap_or(1.0);
    let exposure = attr_f32(&light.exposure_attr()).unwrap_or(0.0);
    let color = attr_color3f(&light.color_attr()).unwrap_or([1.0, 1.0, 1.0]);
    let gain = intensity * 2f32.powf(exposure);
    Vec3A::new(color[0] * gain, color[1] * gain, color[2] * gain)
}

fn emit_sphere_light(
    world: &mut WorldBuilder,
    lights: &mut LightList,
    light: &SphereLight,
    world_xf: GMat4,
) {
    let radius = attr_f32(&light.radius_attr()).unwrap_or(0.5);
    let effective = lux_emission(light);
    let pos_v = world_xf.transform_point3(Vec3::ZERO);
    let position = Vec3A::new(pos_v.x, pos_v.y, pos_v.z);

    // The visible sphere geometry and the AreaLight share one surface;
    // the integrator attributes a bounce hit to the light by the
    // geometry id the attach returns.
    let material = Arc::new(Emissive::new(effective));
    let geom_id = world.attach(
        Geometry::Sphere {
            center: position,
            radius,
        },
        material.clone(),
    );
    lights.add(Arc::new(AreaLight::new(
        Box::new(SphereShape {
            center: position,
            radius,
        }),
        material,
        geom_id,
    )));
    debug!(
        "SphereLight: pos={:?} radius={} effective_color={:?}",
        position, radius, effective
    );
}

fn emit_rect_light(
    world: &mut WorldBuilder,
    lights: &mut LightList,
    light: &RectLight,
    world_xf: GMat4,
) {
    let width = attr_f32(&light.width_attr()).unwrap_or(1.0);
    let height = attr_f32(&light.height_attr()).unwrap_or(1.0);
    let effective = lux_emission(light);

    // UsdLux RectLight: a rectangle in the local XY plane, centered at the
    // origin, emitting along local -Z.
    let corner = world_xf.transform_point3(Vec3::new(-0.5 * width, -0.5 * height, 0.0));
    let origin = Vec3A::new(corner.x, corner.y, corner.z);
    let eu = world_xf.transform_vector3(Vec3::new(width, 0.0, 0.0));
    let ev = world_xf.transform_vector3(Vec3::new(0.0, height, 0.0));
    let nz = world_xf.transform_vector3(Vec3::NEG_Z);
    let edge_u = Vec3A::new(eu.x, eu.y, eu.z);
    let edge_v = Vec3A::new(ev.x, ev.y, ev.z);
    let normal = Vec3A::new(nz.x, nz.y, nz.z);

    // The visible geometry (one mesh: two triangles spanning the
    // rectangle) and the AreaLight share one surface; bounce hits are
    // attributed to the light by the geometry id.
    let material = Arc::new(Emissive::new(effective));
    let (c00, c10, c11, c01) = (
        origin,
        origin + edge_u,
        origin + edge_u + edge_v,
        origin + edge_v,
    );
    let geom_id = world.attach(
        Geometry::TriangleMesh {
            vertices: vec![c00, c10, c11, c01],
            indices: vec![[0, 1, 2], [0, 2, 3]],
            normals: None,
        },
        material.clone(),
    );
    lights.add(Arc::new(AreaLight::new(
        Box::new(RectShape::new(origin, edge_u, edge_v, normal)),
        material,
        geom_id,
    )));
    debug!(
        "RectLight: origin={:?} edge_u={:?} edge_v={:?} effective_color={:?}",
        origin, edge_u, edge_v, effective
    );
}

/// Imports a `UsdLuxDistantLight`. The light points down its local -Z, so
/// the world direction it travels toward is that axis under the prim's
/// transform. `inputs:angle` is the source's angular *diameter* in degrees
/// (default 0.53 — the sun's).
///
/// `intensity × color × 2^exposure` is taken as the irradiance on a surface
/// facing the light; [`DistantLight`] derives the radiance over the cone.
/// The light has no scene geometry, so it is light-list-only: bounce rays
/// find it by escaping along a direction inside its cone.
fn emit_distant_light(lights: &mut LightList, light: &UsdDistantLight, world_xf: GMat4) {
    let direction = world_xf.transform_vector3(Vec3::NEG_Z);
    if direction.length_squared() < 1e-12 {
        warn!("DistantLight has a degenerate orientation — skipped");
        return;
    }
    let angle = attr_f32(&light.angle_attr()).unwrap_or(0.53);
    let irradiance = lux_emission(light);
    debug!(
        "DistantLight: direction={:?} angle={}° irradiance={:?}",
        direction, angle, irradiance
    );
    lights.add(Arc::new(CoreDistantLight::new(
        Vec3A::new(direction.x, direction.y, direction.z),
        irradiance,
        angle,
    )));
}

/// Imports a `UsdLuxDomeLight` as an infinite environment.
///
/// `inputs:texture:file` is resolved against the USD layer's directory and
/// handed to the host's [`AssetLoader`] — crust-core decodes nothing
/// itself. Without a file, or when the host declines, the dome is its
/// uniform `intensity × color × 2^exposure`.
///
/// Only `latlong` is supported; `inputs:texture:format` values that mean
/// anything else warn and fall back to the uniform colour rather than
/// silently mapping the image wrongly. The prim's rotation orients the sky.
fn emit_dome_light(
    lights: &mut LightList,
    prim: &Prim,
    light: &DomeLight,
    world_xf: GMat4,
    stage_path: &Path,
    assets: &dyn AssetLoader,
    // Accumulates time spent in the host's decoder, so the report can
    // separate "decoding a 14k HDRI" from the rest of the traversal.
    asset_time: &mut Duration,
) {
    let tint = lux_emission(light);

    let format = light
        .texture_format_attr()
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::Token(t) => Some(t.to_string()),
            _ => None,
        });
    let map = match dome_texture_path(light, stage_path) {
        Some(texture) => match format.as_deref() {
            // `automatic` infers from the image; for the equirectangular
            // images a dome light normally carries that means latlong.
            None | Some("latlong") | Some("automatic") => {
                let started = Instant::now();
                let loaded = assets.load_environment(&texture);
                *asset_time += started.elapsed();
                if loaded.is_none() {
                    warn!(
                        "DomeLight at {}: could not load {} — falling back to \
                         the uniform colour",
                        prim.path(),
                        texture.display()
                    );
                }
                loaded.map(Arc::new)
            }
            Some(other) => {
                warn!(
                    "DomeLight at {}: texture:format \"{other}\" is not supported \
                     (only latlong) — falling back to the uniform colour",
                    prim.path()
                );
                None
            }
        },
        None => None,
    };

    // Only the rotation orients the sky; a dome is at infinity, so its
    // translation and scale are meaningless.
    let m = world_xf.to_cols_array_2d();
    let rotation = Mat3A::from_cols(
        Vec3A::new(m[0][0], m[0][1], m[0][2]).normalize_or(Vec3A::X),
        Vec3A::new(m[1][0], m[1][1], m[1][2]).normalize_or(Vec3A::Y),
        Vec3A::new(m[2][0], m[2][1], m[2][2]).normalize_or(Vec3A::Z),
    );

    info!(
        "Imported DomeLight at {} (tint={:?}, {})",
        prim.path(),
        tint,
        match &map {
            Some(m) => format!("{}x{} environment map", m.width(), m.height()),
            None => "uniform".to_string(),
        }
    );
    lights.add(Arc::new(CoreDomeLight::new(tint, map, rotation)));
}

/// The dome's `inputs:texture:file` as a filesystem path.
///
/// Goes through [`asset_value_path`], which prefers openusd's `resolved_path()`
/// — anchored against the layer that *authored* the path, not the root layer.
/// That distinction only shows up once a stage has depth: the Moana island's
/// lights author `../textures/islandsun.exr` relative to `usd/island.usda`, so
/// a root layer sitting anywhere else would otherwise resolve it against the
/// wrong directory and silently fall back to the dome's uniform colour.
fn dome_texture_path(light: &DomeLight, stage_path: &Path) -> Option<std::path::PathBuf> {
    let value = light.texture_file_attr().get::<sdf::Value>().ok().flatten()?;
    asset_value_path(&value, stage_path)
}

fn warn_unsupported_light(stage: &Stage, prim: &Prim) {
    let warn_type = |name: &str| {
        warn!(
            "USD light type '{}' at {} is not yet supported — skipped",
            name,
            prim.path()
        );
    };
    if DiskLight::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .is_some()
    {
        warn_type("DiskLight");
    } else if CylinderLight::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .is_some()
    {
        warn_type("CylinderLight");
    }
}

// -----------------------------------------------------------------------
// Materials
// -----------------------------------------------------------------------

/// Memoizes resolved materials by binding path (and shares one default),
/// so prims bound to the same USD material get pointer-identical Arcs —
/// which is what lets `MeshKey` recognize shared mesh geometry.
#[derive(Default)]
struct MaterialCache {
    by_path: HashMap<(u32, String), Arc<dyn Material>>,
    default: Option<Arc<dyn Material>>,
    /// Resolved `.ptx` path -> the opened texture, or `None` if it could not
    /// be opened. Keyed by filesystem path, so it needs no epoch scoping.
    ptex: HashMap<String, Option<Arc<dyn crate::PtexTexture>>>,
    /// Which stage the prototype-scoped entries belong to; see
    /// [`MaterialCache::key`]. Kept in step with [`ImportCaches::epoch`].
    epoch: u32,
}

impl MaterialCache {
    fn default_material(&mut self) -> Arc<dyn Material> {
        self.default.get_or_insert_with(default_material).clone()
    }

    /// Cache key for a bound material path.
    ///
    /// Authored scene paths are stable across stages, so they key on the
    /// path alone and a material shared by several subtrees resolves to
    /// one `Arc` — which is what lets the mesh cache deduplicate across
    /// them. Paths *inside* a prototype are not stable: prototypes are
    /// numbered per composition, so under the streaming importer every
    /// stage has its own `/__Prototype_0`, and keying those on the path
    /// alone hands one chunk's material to the next chunk's geometry.
    /// Those are therefore scoped by epoch.
    ///
    /// This bit the streaming importer for real: on the Moana island it
    /// silently merged distinct meshes onto shared materials, losing
    /// 5 835 258 triangles. No single element reproduces it — it needs
    /// two chunks that both carry prototype-internal materials.
    fn key(&self, path: &str) -> (u32, String) {
        let epoch = if path.starts_with("/__Prototype") {
            self.epoch
        } else {
            0
        };
        (epoch, path.to_string())
    }
}

fn resolve_material(stage: &Stage, prim: &Prim, caches: &mut ImportCaches<'_>) -> Arc<dyn Material> {
    let mat_path = MaterialBindingAPI::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .and_then(|b| b.direct_binding("").ok().flatten());

    let Some(mat_path) = mat_path else {
        return caches.materials.default_material();
    };

    let key = caches.materials.key(mat_path.as_str());
    if let Some(hit) = caches.materials.by_path.get(&key) {
        return hit.clone();
    }
    let resolved = resolve_material_uncached(stage, &mat_path, caches);
    caches.materials.by_path.insert(key, resolved.clone());
    resolved
}

fn resolve_material_uncached(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Arc<dyn Material> {

    let mat = match UsdMaterial::get(stage, mat_path.clone()) {
        Ok(Some(m)) => m,
        _ => {
            warn!(
                "Material at {} not resolvable — using default grey OpenPBR",
                mat_path
            );
            return default_material();
        }
    };

    // Checked before asking for the surface source, because that answer cannot
    // be trusted to name the network that matters. A material carrying several
    // render-context outputs — the Moana island authors `outputs:ri:surface`
    // (PxrDisneyBsdf), `outputs:glslfx:surface` (UsdPreviewSurface) and
    // `outputs:ri:displacement` on every prim — resolves through
    // `compute_surface_source` to the *preview* shader, whose inputs are all
    // `.connect`ed to the material's interface rather than authored as values.
    // Decoding that gives a material with every parameter at its default: the
    // island rendered uniformly pale and glossy instead of matte dark rock.
    if has_shader_id(stage, mat_path, "PxrDisneyBsdf") {
        return Arc::new(disney_to_openpbr(stage, mat_path, caches));
    }

    let shader = match mat.compute_surface_source() {
        Ok(Some(s)) => s,
        _ => {
            warn!(
                "Material {} has no surface shader — using default grey OpenPBR",
                mat_path
            );
            return default_material();
        }
    };

    let shader_id = shader_info_id(&shader);
    debug!("Material {mat_path}: surface shader id = {shader_id:?}");
    match shader_id.as_deref() {
        Some("crust:openpbr") => decode_crust_openpbr(&shader),
        Some("UsdPreviewSurface") => {
            // The preview surface may still be the Ptex-driven one — the Moana
            // island wires its `diffuseColor` to a Ptex node — so consult the
            // material's own interface input either way.
            let mut o = preview_surface_openpbr(stage, mat_path);
            o.base_color_ptex = material_ptex(stage, mat_path, caches);
            Arc::new(o)
        }
        Some("PxrDisneyBsdf") => Arc::new(disney_to_openpbr(stage, mat_path, caches)),
        Some(other) => {
            warn!(
                "Unrecognized shader id '{}' at {} — using default grey OpenPBR",
                other, mat_path
            );
            default_material()
        }
        None => {
            warn!(
                "Shader at {} has no info:id — using default grey OpenPBR",
                mat_path
            );
            default_material()
        }
    }
}

fn default_material() -> Arc<dyn Material> {
    Arc::new(OpenPBR::diffuse(Vec3A::new(0.5, 0.5, 0.5)))
}

fn shader_info_id(shader: &Shader) -> Option<String> {
    // `Shader::id()` is the higher-level accessor and does the correct
    // `get::<String>()` (which extracts from both String and Token variants).
    if let Ok(Some(id)) = shader.id() {
        return Some(id);
    }
    // Fallback for older openusd revisions or shaders that author info:id
    // via a raw attribute rather than the schema helper.
    shader
        .attribute("info:id")
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            // `Token` carries an interned `tf::Token`, `String` a plain
            // `String`, so the two arms cannot bind the same name.
            sdf::Value::Token(t) => Some(t.as_str().to_owned()),
            sdf::Value::String(t) => Some(t),
            _ => None,
        })
}

fn preview_surface_openpbr(stage: &Stage, mat_path: &sdf::Path) -> OpenPBR {
    let ps = match shade::read_preview_surface(stage, mat_path) {
        Ok(Some(ps)) => ps,
        _ => return OpenPBR::diffuse(Vec3A::new(0.5, 0.5, 0.5)),
    };

    let mut o = OpenPBR::default();

    if let Some(rgb) = ps.diffuse_color.value() {
        o.base_color = Vec3A::new(rgb[0], rgb[1], rgb[2]);
    } else if ps.diffuse_color.texture().is_some() {
        warn!("UsdPreviewSurface at {}: diffuseColor is a texture — textures are not supported yet", mat_path);
    }
    if let Some(m) = ps.metallic.value() {
        o.base_metalness = *m;
    }
    if let Some(r) = ps.roughness.value() {
        o.specular_roughness = *r;
    }
    if let Some(op) = ps.opacity.value() {
        o.geometry_opacity = *op;
    }
    if let Some(rgb) = ps.emissive_color.value() {
        o.emission_color = Vec3A::new(rgb[0], rgb[1], rgb[2]);
        let max = rgb[0].max(rgb[1]).max(rgb[2]);
        if max > 0.0 {
            o.emission_luminance = 1.0;
        }
    }
    if let Some(ior) = ps.ior.value() {
        o.specular_ior = *ior;
    }
    if let Some(c) = ps.clearcoat.value() {
        o.coat_weight = *c;
    }
    if let Some(cr) = ps.clearcoat_roughness.value() {
        o.coat_roughness = *cr;
    }

    o
}

/// Whether the material has a child `Shader` prim with this `info:id`.
///
/// Cheaper and more reliable than resolving a render-context output for the
/// question actually being asked — "does this material shade with X" — since a
/// material may declare several context outputs and USD gives no ordering
/// between them without a configured render context.
fn has_shader_id(stage: &Stage, mat_path: &sdf::Path, id: &str) -> bool {
    let Ok(children) = stage.prim(mat_path.clone()).children() else {
        return false;
    };
    children.iter().any(|c| {
        match c.attribute("info:id").get::<sdf::Value>() {
            Ok(Some(sdf::Value::Token(t))) => t.as_str() == id,
            Ok(Some(sdf::Value::String(t))) => t == id,
            _ => false,
        }
    })
}

/// Maps RenderMan's `PxrDisneyBsdf` onto [`OpenPBR`].
///
/// The Moana island — the reason this exists — shades every surface with it,
/// so without this arm the whole dataset resolves to flat grey. Both models
/// descend from Burley's, which makes most of the mapping direct; the
/// parameters are read off the **Material** prim rather than the shader, since
/// that is where the island authors them (the shader's inputs are all
/// `.connect`ed to the material's interface inputs, and following those
/// connections would buy nothing here).
///
/// Not mapped, because OpenPBR has no equivalent lobe: `subsurface*`,
/// `diffuseTransmission`, `specularTint`, `scatter*`. Those surfaces render as
/// opaque dielectrics.
///
/// `sheen` is deliberately **not** mapped onto `fuzz_weight`, despite both
/// being "the retroreflective one". Disney's sheen is a small term *added* at
/// grazing angles; OpenPBR's fuzz is a Charlie layer *mixed over* everything
/// beneath it, so at the island's authored `sheen = 1` the fuzz lobe replaced
/// the base entirely — the lava rocks lost their Ptex detail and rendered as
/// smooth blue-grey plastic. A weight is not a weight just because it shares a
/// name, and there is no honest scalar between the two.
fn disney_to_openpbr(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> OpenPBR {
    let prim = stage.prim(mat_path.clone());
    let f = |n: &str| custom_f32(&prim, &format!("inputs:{n}"));
    let c = |n: &str| custom_vec3(&prim, &format!("inputs:{n}"));

    let mut o = OpenPBR::default();

    // `inputs:baseColor` reaches the BSDF through a `PxrColorCorrect` with
    // gamma 1/2.2, i.e. the authored value is display-encoded and the shader
    // decodes it to linear. Do the same, or every surface renders washed out.
    if let Some(rgb) = c("baseColor") {
        o.base_color = srgb_to_linear(rgb);
    }
    if let Some(v) = f("metallic") {
        o.base_metalness = v;
    }
    if let Some(v) = f("roughness") {
        o.specular_roughness = v;
    }
    if let Some(v) = f("ior") {
        o.specular_ior = v;
    }
    if let Some(v) = f("anisotropic") {
        o.specular_roughness_anisotropy = v;
    }
    if let Some(v) = f("clearcoat") {
        o.coat_weight = v;
    }
    // Gloss is the complement of roughness.
    if let Some(v) = f("clearcoatGloss") {
        o.coat_roughness = (1.0 - v).clamp(0.0, 1.0);
    }
    // Either name turns up across the island's materials.
    if let Some(v) = f("specularTransmission").or_else(|| f("refractionGain")) {
        o.transmission_weight = v;
    }
    if let Some(v) = f("alpha") {
        o.geometry_opacity = v;
    }
    if let Some(v) = f("thinSurface") {
        o.geometry_thin_walled = v != 0.0;
    }

    o.base_color_ptex = material_ptex(stage, mat_path, caches);
    debug!(
        "PxrDisneyBsdf {mat_path}: baseColor={:?} (authored {:?}) roughness={} metalness={} \
         fuzz={} coat={} ior={} ptex={}",
        o.base_color,
        c("baseColor"),
        o.specular_roughness,
        o.base_metalness,
        o.fuzz_weight,
        o.coat_weight,
        o.specular_ior,
        o.base_color_ptex.is_some()
    );
    o
}

/// The per-face colour texture a material binds, if any.
///
/// `inputs:surfaceMap` is the interface input both of the island's Ptex shader
/// paths read — `PxrPtexture.filename` for RenderMan and
/// `HwPtexTexture_1.file` for the GL preview both `.connect` to it — so
/// reading it directly gets the file without walking either network.
fn material_ptex(
    stage: &Stage,
    mat_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Option<crate::PtexRef> {
    let prim = stage.prim(mat_path.clone());
    let value = prim
        .attribute("inputs:surfaceMap")
        .get::<sdf::Value>()
        .ok()
        .flatten()?;
    let path = asset_value_path(&value, caches.stage_path)?;

    // Keyed on the resolved filesystem path, which — unlike a prototype-scoped
    // scene path — is stable across the streaming importer's stages, so one
    // texture is opened once however many materials or chunks reference it.
    // Negative results are cached too: a 600 MB file that failed to open
    // should not be retried per material.
    let key = path.to_string_lossy().into_owned();
    if let Some(hit) = caches.materials.ptex.get(&key) {
        return hit.clone().map(crate::PtexRef);
    }
    let started = Instant::now();
    let loaded = caches.assets.load_ptex(&path);
    caches.asset_time += started.elapsed();
    caches.materials.ptex.insert(key, loaded.clone());
    loaded.map(crate::PtexRef)
}

/// An `asset`-valued attribute as a filesystem path.
///
/// openusd anchors default-sourced asset paths against the layer that authored
/// them and reports the result in `resolved_path` — which is what makes a
/// production stage's `../../../textures/foo.ptx` work at all, since the layer
/// authoring it is nested several directories below the root. The authored
/// string is only a fallback, anchored against the root layer.
fn asset_value_path(value: &sdf::Value, stage_path: &Path) -> Option<std::path::PathBuf> {
    let (authored, resolved) = match value {
        sdf::Value::AssetPath(p) => (p.as_str().to_string(), p.resolved_path()),
        sdf::Value::String(p) => (p.clone(), None),
        _ => return None,
    };
    if let Some(r) = resolved
        && !r.is_empty()
    {
        return Some(std::path::PathBuf::from(r));
    }
    if authored.is_empty() {
        return None;
    }
    let candidate = std::path::Path::new(&authored);
    if candidate.is_absolute() {
        return Some(candidate.to_path_buf());
    }
    Some(
        stage_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(candidate),
    )
}

/// sRGB transfer function, decoding a display-referred colour to linear.
///
/// The plain 2.2 power law rather than the piecewise sRGB curve: it is what
/// the `PxrColorCorrect` gamma node in the island's materials actually
/// applies, and matching the reference render matters more here than matching
/// the standard.
fn srgb_to_linear(c: Vec3A) -> Vec3A {
    Vec3A::new(
        c.x.max(0.0).powf(2.2),
        c.y.max(0.0).powf(2.2),
        c.z.max(0.0).powf(2.2),
    )
}

fn custom_vec3(prim: &Prim, name: &str) -> Option<Vec3A> {
    let v = prim.attribute(name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::Vec3f(p) => Some(Vec3A::new(p.x, p.y, p.z)),
        sdf::Value::Vec3d(p) => Some(Vec3A::new(p.x as f32, p.y as f32, p.z as f32)),
        _ => None,
    }
}

/// Decode a `crust:openpbr` shader into the OpenPBR material. Every input
/// name is camelCase mirror of the Rust snake_case, e.g. `base_color` →
/// `inputs:baseColor`, `subsurface_radius_scale` → `inputs:subsurfaceRadiusScale`.
fn decode_crust_openpbr(shader: &Shader) -> Arc<dyn Material> {
    let mut o = OpenPBR::default();

    let f = |n: &str, d: f32| shader_input_f32(shader, n).unwrap_or(d);
    let c = |n: &str, d: Vec3A| shader_input_vec3(shader, n).unwrap_or(d);
    let b = |n: &str, d: bool| shader_input_bool(shader, n).unwrap_or(d);

    // Base
    o.base_weight = f("baseWeight", o.base_weight);
    o.base_color = c("baseColor", o.base_color);
    o.base_diffuse_roughness = f("baseDiffuseRoughness", o.base_diffuse_roughness);
    o.base_metalness = f("baseMetalness", o.base_metalness);

    // Specular
    o.specular_weight = f("specularWeight", o.specular_weight);
    o.specular_color = c("specularColor", o.specular_color);
    o.specular_roughness = f("specularRoughness", o.specular_roughness);
    o.specular_ior = f("specularIor", o.specular_ior);
    o.specular_roughness_anisotropy = f(
        "specularRoughnessAnisotropy",
        o.specular_roughness_anisotropy,
    );

    // Transmission
    o.transmission_weight = f("transmissionWeight", o.transmission_weight);
    o.transmission_color = c("transmissionColor", o.transmission_color);
    o.transmission_depth = f("transmissionDepth", o.transmission_depth);
    o.transmission_scatter = c("transmissionScatter", o.transmission_scatter);
    o.transmission_scatter_anisotropy = f(
        "transmissionScatterAnisotropy",
        o.transmission_scatter_anisotropy,
    );
    o.transmission_dispersion_scale = f(
        "transmissionDispersionScale",
        o.transmission_dispersion_scale,
    );
    o.transmission_dispersion_abbe_number = f(
        "transmissionDispersionAbbeNumber",
        o.transmission_dispersion_abbe_number,
    );

    // Subsurface
    o.subsurface_weight = f("subsurfaceWeight", o.subsurface_weight);
    o.subsurface_color = c("subsurfaceColor", o.subsurface_color);
    o.subsurface_radius = f("subsurfaceRadius", o.subsurface_radius);
    o.subsurface_radius_scale = c("subsurfaceRadiusScale", o.subsurface_radius_scale);
    o.subsurface_scatter_anisotropy = f(
        "subsurfaceScatterAnisotropy",
        o.subsurface_scatter_anisotropy,
    );

    // Fuzz
    o.fuzz_weight = f("fuzzWeight", o.fuzz_weight);
    o.fuzz_color = c("fuzzColor", o.fuzz_color);
    o.fuzz_roughness = f("fuzzRoughness", o.fuzz_roughness);

    // Coat
    o.coat_weight = f("coatWeight", o.coat_weight);
    o.coat_color = c("coatColor", o.coat_color);
    o.coat_roughness = f("coatRoughness", o.coat_roughness);
    o.coat_roughness_anisotropy = f("coatRoughnessAnisotropy", o.coat_roughness_anisotropy);
    o.coat_ior = f("coatIor", o.coat_ior);
    o.coat_darkening = f("coatDarkening", o.coat_darkening);

    // Thin film
    o.thin_film_weight = f("thinFilmWeight", o.thin_film_weight);
    o.thin_film_thickness = f("thinFilmThickness", o.thin_film_thickness);
    o.thin_film_ior = f("thinFilmIor", o.thin_film_ior);

    // Emission
    o.emission_luminance = f("emissionLuminance", o.emission_luminance);
    o.emission_color = c("emissionColor", o.emission_color);

    // Geometry
    o.geometry_opacity = f("geometryOpacity", o.geometry_opacity);
    o.geometry_thin_walled = b("geometryThinWalled", o.geometry_thin_walled);

    Arc::new(o)
}

fn shader_input_f32(shader: &Shader, name: &str) -> Option<f32> {
    let attr_name = format!("inputs:{}", name);
    let v = shader.attribute(&attr_name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::Float(f) => Some(f),
        sdf::Value::Double(d) => Some(d as f32),
        _ => None,
    }
}

fn shader_input_bool(shader: &Shader, name: &str) -> Option<bool> {
    let attr_name = format!("inputs:{}", name);
    let v = shader.attribute(&attr_name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::Bool(b) => Some(b),
        _ => None,
    }
}

fn shader_input_vec3(shader: &Shader, name: &str) -> Option<Vec3A> {
    let attr_name = format!("inputs:{}", name);
    let v = shader.attribute(&attr_name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::Vec3f(p) => Some(Vec3A::new(p.x, p.y, p.z)),
        // USD encodes color3f as an sdf::Value::Vec3f — no dedicated variant.
        _ => None,
    }
}

// -----------------------------------------------------------------------
// Render settings
// -----------------------------------------------------------------------

fn import_render_settings(stage: &Stage) -> RenderSettings {
    let path = match UsdRenderSettings::stage_settings_path(stage).ok().flatten() {
        Some(p) => p,
        None => {
            // Fall back to the conventional /Render/settings location.
            match sdf::path("/Render/settings").ok() {
                Some(p) => p,
                None => return default_settings(),
            }
        }
    };

    let s = match UsdRenderSettings::get(stage, path.clone()).ok().flatten() {
        Some(s) => s,
        None => {
            debug!(
                "No UsdRenderSettings at {} — using defaults for render settings",
                path
            );
            return default_settings();
        }
    };

    let (mut w, mut h) = (DEFAULT_WIDTH, DEFAULT_HEIGHT);
    if let Ok(Some(v)) = s.resolution_attr().get::<sdf::Value>() {
        if let Some(v2) = v.try_as_vec_2i() {
            w = v2.x as usize;
            h = v2.y as usize;
        }
    }

    // Custom `crust:*` attrs. We look them up on the RenderSettings prim.
    let prim = stage.prim(path);
    let spp = custom_i32(&prim, "crust:samplesPerPixel").unwrap_or(DEFAULT_SPP as i32) as u32;
    let max_depth = custom_i32(&prim, "crust:maxDepth").unwrap_or(DEFAULT_MAX_DEPTH as i32) as u32;
    let min_spp = custom_i32(&prim, "crust:minSamplesPerPixel").unwrap_or(DEFAULT_MIN_SPP as i32)
        as u32;
    let variance =
        custom_f32(&prim, "crust:varianceThreshold").unwrap_or(DEFAULT_VARIANCE);
    let frame = custom_i32(&prim, "crust:frame").unwrap_or(DEFAULT_FRAME as i32) as isize;

    // Path guiding (opt-in).
    let guiding = custom_bool(&prim, "crust:pathGuiding").unwrap_or(false);
    let guiding_iters = custom_i32(&prim, "crust:guidingTrainIterations")
        .unwrap_or(DEFAULT_GUIDING_TRAIN_ITERATIONS as i32)
        .max(1) as u32;
    let guiding_prob =
        custom_f32(&prim, "crust:guidingProb").unwrap_or(DEFAULT_GUIDING_PROB);

    // MIS strategy: `power` (default) | `balance` | `light` | `bsdf`.
    let strategy = match custom_token(&prim, "crust:samplingStrategy").as_deref() {
        None | Some("power") | Some("mis") => SamplingStrategy::PowerMis,
        Some("balance") => SamplingStrategy::BalanceMis,
        Some("light") => SamplingStrategy::LightOnly,
        Some("bsdf") => SamplingStrategy::BsdfOnly,
        Some(other) => {
            warn!(
                "Unknown crust:samplingStrategy \"{}\" (expected power | balance | light | bsdf) — using power MIS",
                other
            );
            SamplingStrategy::PowerMis
        }
    };

    // Pixel reconstruction filter: `box` | `triangle` (default) | `gaussian`
    // | `blackman` | `mitchell`, each at its conventional radius unless
    // `crust:pixelFilterRadius` overrides it (in pixels, from the center).
    let mut filter = match custom_token(&prim, "crust:pixelFilter") {
        None => PixelFilter::default(),
        Some(name) => PixelFilter::from_name(&name).unwrap_or_else(|| {
            warn!(
                "Unknown crust:pixelFilter \"{}\" (expected box | triangle | gaussian | blackman | mitchell) — using the triangle filter",
                name
            );
            PixelFilter::default()
        }),
    };
    if let Some(radius) = custom_f32(&prim, "crust:pixelFilterRadius") {
        filter = filter.with_radius(radius);
    }

    RenderSettings::new(spp, max_depth, w, h, min_spp, variance, frame)
        .with_guiding(guiding, guiding_iters, guiding_prob)
        .with_sampling_strategy(strategy)
        .with_pixel_filter(filter)
}

fn default_settings() -> RenderSettings {
    RenderSettings::new(
        DEFAULT_SPP,
        DEFAULT_MAX_DEPTH,
        DEFAULT_WIDTH,
        DEFAULT_HEIGHT,
        DEFAULT_MIN_SPP,
        DEFAULT_VARIANCE,
        DEFAULT_FRAME,
    )
}

fn custom_i32(prim: &Prim, name: &str) -> Option<i32> {
    let v = prim.attribute(name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::Int(i) => Some(i),
        _ => None,
    }
}

fn custom_f32(prim: &Prim, name: &str) -> Option<f32> {
    let v = prim.attribute(name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::Float(f) => Some(f),
        sdf::Value::Double(d) => Some(d as f32),
        _ => None,
    }
}

fn custom_bool(prim: &Prim, name: &str) -> Option<bool> {
    let v = prim.attribute(name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::Bool(b) => Some(b),
        // Authoring tools sometimes write bools as ints.
        sdf::Value::Int(i) => Some(i != 0),
        _ => None,
    }
}

fn custom_token(prim: &Prim, name: &str) -> Option<String> {
    let v = prim.attribute(name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::Token(t) => Some(t.as_str().to_owned()),
        sdf::Value::String(s) => Some(s),
        _ => None,
    }
}

fn custom_color3(prim: &Prim, name: &str) -> Option<Vec3A> {
    let v = prim.attribute(name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::Vec3f(c) => Some(Vec3A::new(c.x, c.y, c.z)),
        sdf::Value::Vec3d(c) => Some(Vec3A::new(c.x as f32, c.y as f32, c.z as f32)),
        _ => None,
    }
}

fn custom_f32_array(prim: &Prim, name: &str) -> Option<Vec<f32>> {
    let v = prim.attribute(name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::FloatVec(v) => Some(v),
        sdf::Value::DoubleVec(v) => Some(v.into_iter().map(|d| d as f32).collect()),
        _ => None,
    }
}

fn custom_i32_array(prim: &Prim, name: &str) -> Option<Vec<i32>> {
    let v = prim.attribute(name).get::<sdf::Value>().ok()??;
    match v {
        sdf::Value::IntVec(v) => Some(v),
        _ => None,
    }
}

// -----------------------------------------------------------------------
// Attribute helpers
// -----------------------------------------------------------------------

fn attr_f32(attr: &openusd::usd::Attribute) -> Option<f32> {
    match attr.get::<sdf::Value>().ok()?? {
        sdf::Value::Float(f) => Some(f),
        sdf::Value::Double(d) => Some(d as f32),
        _ => None,
    }
}

fn attr_color3f(attr: &openusd::usd::Attribute) -> Option<[f32; 3]> {
    match attr.get::<sdf::Value>().ok()?? {
        // color3f is stored as Vec3f in sdf::Value
        sdf::Value::Vec3f(v) => Some([v.x, v.y, v.z]),
        _ => None,
    }
}

#[cfg(test)]
mod bake_tests {
    use super::*;
    use crate::material::OpenPBR;

    /// A unit quad in the z = 0 plane, wound counter-clockwise seen from +z.
    fn quad() -> MeshGeom {
        MeshGeom {
            verts: vec![
                Vec3A::new(-1.0, -1.0, 0.0),
                Vec3A::new(1.0, -1.0, 0.0),
                Vec3A::new(1.0, 1.0, 0.0),
                Vec3A::new(-1.0, 1.0, 0.0),
            ],
            tris: vec![[0, 1, 2], [0, 2, 3]],
        }
    }

    /// Places `geom` by `l2w` two ways — baked into world-space triangles,
    /// and as an instance of the local-space mesh — and returns what a ray
    /// down -z sees of each: `(t, front_face, normal.z)`.
    fn baked_vs_instanced(l2w: Affine3A) -> ((f32, bool, f32), (f32, bool, f32)) {
        let mat = || -> Arc<dyn Material> { Arc::new(OpenPBR::diffuse(Vec3A::splat(0.5))) };
        let geom = quad();

        let mut baked = WorldBuilder::new();
        baked.attach(
            Geometry::TriangleMesh {
                vertices: bake_verts(&geom.verts, &l2w),
                indices: bake_indices(geom.tris.clone(), &l2w),
                normals: None,
            },
            mat(),
        );
        let baked = baked.commit();

        let mut inner = RtSceneBuilder::new();
        inner.attach(Geometry::TriangleMesh {
            vertices: geom.verts.clone(),
            indices: geom.tris.clone(),
            normals: None,
        });
        let mut inst = WorldBuilder::new();
        inst.attach(
            Geometry::Instance {
                scene: Arc::new(inner.commit()),
                transform: l2w,
                transform_end: None,
            },
            mat(),
        );
        let inst = inst.commit();

        let ray = crate::ray::Ray::new(Vec3A::new(0.0, 0.0, 5.0), Vec3A::new(0.0, 0.0, -1.0));
        let probe = |w: &crate::rt_world::World| {
            let h = w.intersect(&ray, 1e-4, f32::MAX).expect("the quad is hit");
            (h.rec.t, h.rec.front_face, h.rec.normal.z)
        };
        (probe(&baked), probe(&inst))
    }

    #[test]
    fn baking_matches_instancing_for_an_ordinary_transform() {
        let l2w = Affine3A::from_scale_rotation_translation(
            glam::Vec3::new(2.0, 1.5, 1.0),
            glam::Quat::from_rotation_z(0.7),
            glam::Vec3::new(0.3, -0.2, 0.0),
        );
        let (baked, inst) = baked_vs_instanced(l2w);
        assert_eq!(baked, inst, "baked {baked:?} vs instanced {inst:?}");
    }

    /// The regression this guards: for `det(M) < 0` the world-space vertices
    /// wind the opposite way round, so a geometric normal derived from them
    /// points *against* the one the instanced path maps out through the
    /// inverse transpose. Without the compensating index swap in
    /// [`bake_indices`], `front_face` inverts — which silently flips which
    /// side of a refractive interface a ray believes it is on.
    #[test]
    fn baking_a_mirrored_transform_keeps_the_original_orientation() {
        // Negative x scale: a mirror, det < 0.
        let l2w = Affine3A::from_scale(glam::Vec3::new(-1.0, 1.0, 1.0));
        assert!(l2w.matrix3.determinant() < 0.0, "this test needs a mirror");

        let (baked, inst) = baked_vs_instanced(l2w);
        assert_eq!(
            baked, inst,
            "mirrored: baked {baked:?} vs instanced {inst:?}"
        );
        // And state the expected value outright, so the test still means
        // something if both paths ever break together.
        assert!(baked.1, "a ray down -z hits the front of a +z-facing quad");
        assert!(baked.2 > 0.0, "the ray-facing normal points back up +z");
    }

    /// Without the swap the test above would pass for the wrong reason if
    /// `bake_indices` were a no-op and the kernel happened to agree, so pin
    /// the swap itself.
    #[test]
    fn bake_indices_swaps_winding_only_when_mirrored() {
        let tris = vec![[0u32, 1, 2]];
        let plain = Affine3A::from_scale(glam::Vec3::new(2.0, 3.0, 4.0));
        assert_eq!(bake_indices(tris.clone(), &plain), vec![[0, 1, 2]]);

        let mirror = Affine3A::from_scale(glam::Vec3::new(-2.0, 3.0, 4.0));
        assert_eq!(bake_indices(tris, &mirror), vec![[0, 2, 1]]);
    }
}

#[cfg(test)]
mod curve_basis_tests {
    use super::*;

    #[test]
    fn bezier_basis_round_trips_unchanged() {
        let cp = [
            Vec3A::new(0.0, 0.0, 0.0),
            Vec3A::new(1.0, 2.0, 0.0),
            Vec3A::new(2.0, -1.0, 1.0),
            Vec3A::new(3.0, 0.0, 0.0),
        ];
        let out = basis_to_bezier(&BEZIER_M, &cp);
        for i in 0..4 {
            assert!(out[i].abs_diff_eq(cp[i], 1e-5), "index {i}: {out:?}");
        }
    }

    #[test]
    fn bspline_and_catmull_rom_convert_to_the_same_curve() {
        // The converted Bézier control points, evaluated via the standard
        // Bernstein formula, must trace exactly the curve the original
        // basis matrix evaluates directly — checked densely over the
        // span, not just at the endpoints.
        let cp = [
            Vec3A::new(0.0, 0.0, 0.0),
            Vec3A::new(1.0, 2.0, 0.5),
            Vec3A::new(2.0, -1.0, 1.0),
            Vec3A::new(3.5, 1.0, -0.5),
        ];
        for basis in [&BSPLINE_M, &CATMULL_ROM_M] {
            let bezier_cp = basis_to_bezier(basis, &cp);
            for i in 0..=10 {
                let t = i as f32 / 10.0;
                let direct = eval_cubic(basis, &cp, t);
                let via_bezier = eval_cubic(&BEZIER_M, &bezier_cp, t);
                assert!(
                    direct.abs_diff_eq(via_bezier, 1e-4),
                    "t={t}: direct={direct:?} via_bezier={via_bezier:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod face_table_tests {
    use super::*;

    /// The triangle list and the face table must stay index-parallel, because
    /// the kernel's `prim_id` indexes one to look up the other.
    #[test]
    fn quad_mesh_face_table_is_parallel_to_triangles() {
        // Three quads, 4 verts each, sharing a vertex pool of 12.
        let counts = [4, 4, 4];
        let indices: Vec<i32> = (0..12).collect();
        let (tris, map) = triangulate(&counts, &indices, 12, true).unwrap();
        let map = map.unwrap();

        assert_eq!(tris.len(), 6, "a quad fans into two triangles");
        assert_eq!(map.faces.len(), tris.len());
        assert_eq!(map.slices.len(), tris.len());
        assert_eq!(map.faces, vec![0, 0, 1, 1, 2, 2]);
        assert_eq!(
            map.slices,
            vec![
                FanSlice::QuadLower,
                FanSlice::QuadUpper,
                FanSlice::QuadLower,
                FanSlice::QuadUpper,
                FanSlice::QuadLower,
                FanSlice::QuadUpper,
            ]
        );
        // The fan is anchored at each face's first vertex.
        assert_eq!(tris[2], [4, 5, 6]);
        assert_eq!(tris[3], [4, 6, 7]);
    }

    /// A face the importer drops must not consume a face id, or every triangle
    /// after it addresses the wrong texture face — the failure mode that looks
    /// like plausible-but-wrong shading rather than an obvious break.
    #[test]
    fn skipped_faces_do_not_shift_later_face_ids() {
        // A degenerate 2-gon between two quads: skipped, but still numbered.
        let counts = [4, 2, 4];
        let indices: Vec<i32> = (0..10).collect();
        let (tris, map) = triangulate(&counts, &indices, 10, true).unwrap();
        let map = map.unwrap();
        assert_eq!(tris.len(), 4);
        // Face 1 contributed nothing; face 2 keeps its own index.
        assert_eq!(map.faces, vec![0, 0, 2, 2]);
    }

    /// Ptex has no n-gon faces, so those triangles must be marked unmappable
    /// rather than given a made-up parameterisation.
    #[test]
    fn ngons_and_triangles_get_their_own_slices() {
        let counts = [3, 5];
        let indices: Vec<i32> = (0..8).collect();
        let (_, map) = triangulate(&counts, &indices, 8, true).unwrap();
        let map = map.unwrap();
        assert_eq!(map.slices[0], FanSlice::Triangle);
        // A pentagon fans into three triangles, none of them addressable.
        assert_eq!(
            &map.slices[1..],
            &[FanSlice::Unmappable, FanSlice::Unmappable, FanSlice::Unmappable]
        );
    }

    /// No table unless a material asks for one: the common case is an
    /// untextured stage, which should allocate nothing.
    #[test]
    fn face_table_is_not_built_unless_requested() {
        let counts = [4];
        let indices = [0, 1, 2, 3];
        let (tris, map) = triangulate(&counts, &indices, 4, false).unwrap();
        assert_eq!(tris.len(), 2);
        assert!(map.is_none());
    }
}
