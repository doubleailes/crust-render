//! USD scene import: opens a stage and produces a runtime `Scene`
//! (camera, world, lights, render settings). See `Scene::from_usd`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;

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
use crate::rt_world::WorldBuilder;
use crate::scene::Scene;
use crust_rt::{CurveSegment, Geometry, Scene as RtScene, SceneBuilder as RtSceneBuilder};
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
use openusd::usd::{Prim, Stage};

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

pub(crate) fn load_scene(path: &Path, assets: &dyn AssetLoader) -> Result<Scene, crate::Error> {
    let path_str = path
        .to_str()
        .ok_or_else(|| crate::Error::NonUtf8Path(path.to_path_buf()))?;

    let stage = Stage::open(path_str).map_err(|e| crate::Error::UsdOpen {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;

    // Render settings come first — the camera importer needs the aspect ratio.
    let settings = import_render_settings(&stage);

    let mut world = WorldBuilder::new();
    let mut lights = LightList::new();
    let mut volumes: Vec<VolumeRegion> = Vec::new();
    let mut camera_candidate: Option<Camera> = None;
    // Prims binding the same material path share one Arc, and prims with
    // identical local geometry + material share one committed kernel
    // scene through instancing — N placements of a mesh cost one copy of
    // its triangles.
    let mut caches = ImportCaches::default();

    let mut stack: Vec<(Prim, GMat4)> = vec![(stage.prim_at(sdf::Path::abs_root()), GMat4::IDENTITY)];

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

        let local = local_matrix_at(&stage, &prim);
        let resets = resets_xform_stack_at(&stage, &prim);
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
                        &stage,
                        &mut world,
                        &prim,
                        &proto_path,
                        this_world,
                        &mut caches,
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
        if let Ok(Some(instancer)) = PointInstancer::get(&stage, prim.path().clone()) {
            emit_point_instancer(&stage, &mut world, &prim, &instancer, this_world, &mut caches);
            // Prototypes are conventionally authored beneath the
            // instancer; they are drawn through it, never on their own.
            continue;
        } else if custom_token(&prim, "crust:volume:type").is_some() {
            emit_volume(&prim, this_world, &mut volumes);
        } else if let Ok(Some(mesh)) = UsdMesh::get(&stage, prim.path().clone()) {
            let mat = resolve_material(&stage, &prim, &mut caches.materials);
            emit_mesh(&mut world, &prim, &mesh, this_world, mat, &mut caches.meshes);
        } else if let Ok(Some(sphere)) = UsdSphere::get(&stage, prim.path().clone()) {
            let mat = resolve_material(&stage, &prim, &mut caches.materials);
            emit_sphere(&mut world, &prim, &sphere, this_world, mat);
        } else if let Ok(Some(curves)) = UsdBasisCurves::get(&stage, prim.path().clone()) {
            let mat = resolve_material(&stage, &prim, &mut caches.materials);
            emit_curves(&mut world, &prim, &curves, this_world, mat);
        } else if UsdCamera::get(&stage, prim.path().clone())
            .ok()
            .flatten()
            .is_some()
        {
            if camera_candidate.is_none() {
                match build_camera(&stage, &prim, &settings) {
                    Some(c) => {
                        info!("Imported USD camera at {}", prim.path());
                        camera_candidate = Some(c);
                    }
                    None => warn!("Failed to build camera from {}", prim.path()),
                }
            }
        } else if let Ok(Some(light)) = SphereLight::get(&stage, prim.path().clone()) {
            emit_sphere_light(&mut world, &mut lights, &light, this_world);
        } else if let Ok(Some(light)) = RectLight::get(&stage, prim.path().clone()) {
            emit_rect_light(&mut world, &mut lights, &light, this_world);
        } else if let Ok(Some(light)) = UsdDistantLight::get(&stage, prim.path().clone()) {
            emit_distant_light(&mut lights, &light, this_world);
        } else if let Ok(Some(light)) = DomeLight::get(&stage, prim.path().clone()) {
            emit_dome_light(&mut lights, &prim, &light, this_world, path, assets);
        } else {
            warn_unsupported_light(&stage, &prim);
        }

        // Recurse. We push children onto the stack unconditionally; the
        // per-prim dispatch above will pick up any typed schemas encountered.
        if let Ok(children) = prim.children() {
            for child in children {
                stack.push((child, this_world));
            }
        }
    }

    let camera = camera_candidate.unwrap_or_else(|| {
        warn!("USD stage has no UsdGeomCamera — falling back to world::get_settings camera");
        crate::world::get_settings().0
    });

    Ok(Scene::new(camera, world.commit(), lights, settings).with_volumes(volumes))
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

fn emit_mesh(
    world: &mut WorldBuilder,
    prim: &Prim,
    mesh: &UsdMesh,
    world_xf: GMat4,
    material: Arc<dyn Material>,
    meshes: &mut HashMap<MeshKey, Arc<RtScene>>,
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
        match triangulate(&counts, &indices, verts.len()) {
            Some(tris) => {
                world.attach_masked(
                    Geometry::TriangleMesh {
                        vertices: verts,
                        indices: tris,
                        normals: None,
                    },
                    material,
                    mask,
                );
            }
            None => debug!("Mesh at {} produced no triangles", prim.path()),
        }
        return;
    }

    // The mesh's kernel scene is built in the prim's *local* space and
    // shared by every prim with identical geometry + material; the
    // Instance geometry carries the placement.
    let Some(inner) = shared_mesh_scene(prim, &points, &counts, &indices, &material, meshes) else {
        return;
    };

    let l2w = Affine3A::from_mat4(world_xf);
    world.attach_masked(
        Geometry::Instance {
            scene: inner,
            transform: l2w,
            transform_end: motion.map(|v| Affine3A::from_translation(v) * l2w),
        },
        material,
        mask,
    );
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

/// The mesh's triangles as a committed kernel scene in the prim's *local*
/// space, shared with every earlier prim whose points/topology/material
/// match. This is the unit of geometry sharing: N placements of a mesh —
/// whether by repeated authoring, by a `PointInstancer`, or by native
/// instancing — cost one copy of its triangles and one BVH.
fn shared_mesh_scene(
    prim: &Prim,
    points: &[Vec3f],
    counts: &[i32],
    indices: &[i32],
    material: &Arc<dyn Material>,
    meshes: &mut HashMap<MeshKey, Arc<RtScene>>,
) -> Option<Arc<RtScene>> {
    let key = MeshKey::new(points, counts, indices, material);
    if let Some(shared) = meshes.get(&key) {
        debug!("Mesh at {} shares geometry with an earlier prim", prim.path());
        return Some(shared.clone());
    }
    let verts: Vec<Vec3A> = points.iter().map(|p| Vec3A::new(p.x, p.y, p.z)).collect();
    let Some(tris) = triangulate(counts, indices, verts.len()) else {
        debug!("Mesh at {} produced no triangles", prim.path());
        return None;
    };
    let mut b = RtSceneBuilder::new();
    b.attach(Geometry::TriangleMesh {
        vertices: verts,
        indices: tris,
        normals: None,
    });
    let scene = Arc::new(b.commit());
    meshes.insert(key, scene.clone());
    Some(scene)
}

/// Fan-triangulates the faces into an index-triple list; `None` if
/// nothing survives.
fn triangulate(counts: &[i32], indices: &[i32], n_verts: usize) -> Option<Vec<[u32; 3]>> {
    let mut tris: Vec<[u32; 3]> = Vec::new();
    let mut offset = 0usize;
    for &fc in counts {
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
        }
        offset += fc;
    }
    if tris.is_empty() { None } else { Some(tris) }
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
                    transform_end: Some(Affine3A::from_translation(v)),
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
#[derive(Default)]
struct ImportCaches {
    /// Material path (memoized) → shared material.
    materials: MaterialCache,
    /// Mesh content + material → the mesh's local-space kernel scene.
    meshes: HashMap<MeshKey, Arc<RtScene>>,
    /// Prototype path → its parts, for both instancing mechanisms.
    protos: HashMap<String, Arc<Vec<ProtoPart>>>,
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
    caches: &mut ImportCaches,
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
            let material = resolve_material(stage, &prim, &mut caches.materials);
            if let Some((points, counts, indices)) = mesh_arrays(&mesh)
                && let Some(scene) = shared_mesh_scene(
                    &prim,
                    &points,
                    &counts,
                    &indices,
                    &material,
                    &mut caches.meshes,
                )
            {
                parts.push(ProtoPart {
                    scene,
                    local: this_local,
                    material,
                    mask,
                });
            }
        } else if let Ok(Some(sphere)) = UsdSphere::get(stage, prim.path().clone()) {
            let material = resolve_material(stage, &prim, &mut caches.materials);
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
            });
        } else if let Ok(Some(curves)) = UsdBasisCurves::get(stage, prim.path().clone()) {
            let material = resolve_material(stage, &prim, &mut caches.materials);
            if let Some(segments) = curve_segments(&prim, &curves) {
                let mut b = RtSceneBuilder::new();
                b.attach(Geometry::RoundCurves { segments });
                parts.push(ProtoPart {
                    scene: Arc::new(b.commit()),
                    local: this_local,
                    material,
                    mask,
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
    caches: &mut ImportCaches,
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
    caches: &mut ImportCaches,
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
        world.attach_masked(
            Geometry::Instance {
                scene: part.scene.clone(),
                transform: Affine3A::from_mat4(xf),
                transform_end: None,
            },
            part.material.clone(),
            part.mask,
        );
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
    caches: &mut ImportCaches,
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
    caches: &mut ImportCaches,
    depth: usize,
) -> Arc<Vec<ProtoPart>> {
    let key = proto_path.to_string();
    if let Some(parts) = caches.protos.get(&key) {
        return parts.clone();
    }
    let root = stage.prim_at(proto_path.clone());
    let parts = Arc::new(collect_proto_parts(stage, &root, caches, depth));
    if parts.is_empty() {
        warn!("Prototype {key} contributed no geometry");
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
    caches: &mut ImportCaches,
) {
    let Some(layout) = read_instancer(prim, instancer) else {
        return;
    };
    let proto_parts = instancer_proto_parts(stage, &layout, caches, 0);

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

/// Segments each cubic span is flattened into before intersection.
const CURVE_FLATTEN_SEGS: usize = 8;

/// Import a `UsdGeomBasisCurves` batch as round (sphere-swept) curve
/// segments: `linear` curves directly, `cubic` curves (bezier / bspline /
/// catmullRom) flattened to a polyline at `CURVE_FLATTEN_SEGS` samples per
/// span. Widths (diameters, per USD) may be authored per point (`vertex`),
/// per curve, or constant; anything else falls back to the first value.
/// The segments live in local space under an `Instance`, like meshes.
/// The curve batch flattened into round segments, in the prim's *local*
/// space. Shared by the top-level emitter and the prototype collector; the
/// caller supplies the placement.
fn curve_segments(prim: &Prim, curves: &UsdBasisCurves) -> Option<Vec<CurveSegment>> {
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
            // Cubic: flatten each span. Span k uses control points
            // [k·vstep .. k·vstep+3]; widths interpolate linearly over the
            // curve parameter between the span's end control points.
            if cnt < 4 {
                offset += cnt;
                continue;
            }
            let n_spans = (cnt - 4) / vstep + 1;
            for s in 0..n_spans {
                let base = s * vstep;
                let ctrl = [cp[base], cp[base + 1], cp[base + 2], cp[base + 3]];
                let (w0, w1) = (radius(base), radius(base + 3));
                let mut prev_p = eval_cubic(basis, &ctrl, 0.0);
                let mut prev_r = w0;
                for i in 1..=CURVE_FLATTEN_SEGS {
                    let t = i as f32 / CURVE_FLATTEN_SEGS as f32;
                    let p = eval_cubic(basis, &ctrl, t);
                    let r = w0 + (w1 - w0) * t;
                    segments.push(CurveSegment {
                        p0: prev_p,
                        p1: p,
                        r0: prev_r,
                        r1: r,
                    });
                    prev_p = p;
                    prev_r = r;
                }
            }
        }
        offset += cnt;
    }

    if segments.is_empty() {
        debug!("BasisCurves at {} produced no segments", prim.path());
        return None;
    }
    info!(
        "Imported BasisCurves at {} ({} {} curves, {} segments)",
        prim.path(),
        counts.len(),
        ty,
        segments.len()
    );
    Some(segments)
}

fn emit_curves(
    world: &mut WorldBuilder,
    prim: &Prim,
    curves: &UsdBasisCurves,
    world_xf: GMat4,
    material: Arc<dyn Material>,
) {
    let Some(segments) = curve_segments(prim, curves) else {
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
    b.attach(Geometry::RoundCurves { segments });
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
    ancestors.push(stage.prim_at(cur_path.clone()));
    while let Some(parent) = cur_path.parent() {
        cur_path = parent;
        ancestors.push(stage.prim_at(cur_path.clone()));
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
                let loaded = assets.load_environment(&texture);
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

/// The dome's `inputs:texture:file`, resolved against the USD layer's
/// directory when it is relative — the usual way an asset path is authored.
fn dome_texture_path(light: &DomeLight, stage_path: &Path) -> Option<std::path::PathBuf> {
    let asset = match light.texture_file_attr().get::<sdf::Value>().ok().flatten()? {
        sdf::Value::AssetPath(p) => p.to_string(),
        sdf::Value::String(p) => p,
        _ => return None,
    };
    if asset.is_empty() {
        return None;
    }
    let candidate = std::path::Path::new(&asset);
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
    by_path: HashMap<String, Arc<dyn Material>>,
    default: Option<Arc<dyn Material>>,
}

impl MaterialCache {
    fn default_material(&mut self) -> Arc<dyn Material> {
        self.default.get_or_insert_with(default_material).clone()
    }
}

fn resolve_material(
    stage: &Stage,
    prim: &Prim,
    cache: &mut MaterialCache,
) -> Arc<dyn Material> {
    let mat_path = MaterialBindingAPI::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .and_then(|b| b.direct_binding("").ok().flatten());

    let Some(mat_path) = mat_path else {
        return cache.default_material();
    };

    if let Some(hit) = cache.by_path.get(mat_path.as_str()) {
        return hit.clone();
    }
    let resolved = resolve_material_uncached(stage, &mat_path);
    cache
        .by_path
        .insert(mat_path.as_str().to_string(), resolved.clone());
    resolved
}

fn resolve_material_uncached(stage: &Stage, mat_path: &sdf::Path) -> Arc<dyn Material> {

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
    match shader_id.as_deref() {
        Some("crust:openpbr") => decode_crust_openpbr(&shader),
        Some("UsdPreviewSurface") => preview_surface_to_openpbr(stage, &mat_path),
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
            sdf::Value::Token(t) | sdf::Value::String(t) => Some(t),
            _ => None,
        })
}

fn preview_surface_to_openpbr(stage: &Stage, mat_path: &sdf::Path) -> Arc<dyn Material> {
    let ps = match shade::read_preview_surface(stage, mat_path) {
        Ok(Some(ps)) => ps,
        _ => return default_material(),
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

    Arc::new(o)
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
    let prim = stage.prim_at(path);
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

    RenderSettings::new(spp, max_depth, w, h, min_spp, variance, frame)
        .with_guiding(guiding, guiding_iters, guiding_prob)
        .with_sampling_strategy(strategy)
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
        sdf::Value::Token(t) => Some(t),
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
