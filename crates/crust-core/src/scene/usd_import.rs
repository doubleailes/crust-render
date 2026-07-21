//! USD scene import: opens a stage and produces a runtime `Scene`
//! (camera, world, lights, render settings). See `Scene::from_usd`.

use std::path::Path;
use std::sync::Arc;

use glam::Mat4 as GMat4;
use tracing::{debug, info, warn};

use crate::bvhnode::BVHNode;
use crate::camera::Camera;
use crate::hittable::Hittable;
use crate::hittable_list::HittableList;
use crate::light::{Light, LightList};
use crate::material::{Emissive, Material, OpenPBR};
use crate::primitives::{Sphere as CrustSphere, Triangle};
use crate::scene::Scene;
use crate::tracer::RenderSettings;
use glam::{Vec3, Vec3A};

use openusd::gf::{Matrix4d, Vec3f};
use openusd::schemas::geom::{
    Camera as UsdCamera, Mesh as UsdMesh, PointBased, Sphere as UsdSphere, Xform, Xformable,
};
use openusd::schemas::lux::{
    CylinderLight, DiskLight, DistantLight, DomeLight, Light as UsdLight, RectLight, SphereLight,
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

pub(crate) fn load_scene(path: &Path) -> std::io::Result<Scene> {
    let path_str = path.to_str().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "USD path is not UTF-8")
    })?;

    let stage = Stage::open(path_str).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("Failed to open USD stage {}: {}", path_str, e),
        )
    })?;

    // Render settings come first — the camera importer needs the aspect ratio.
    let settings = import_render_settings(&stage);

    let mut world = HittableList::new();
    let mut lights = LightList::new();
    let mut camera_candidate: Option<Camera> = None;

    let mut stack: Vec<(Prim, GMat4)> = vec![(stage.prim_at(sdf::Path::abs_root()), GMat4::IDENTITY)];

    while let Some((prim, parent_world)) = stack.pop() {
        let local = local_matrix_at(&stage, &prim);
        let resets = resets_xform_stack_at(&stage, &prim);
        let this_world = if resets { local } else { parent_world * local };

        // Dispatch by schema. Order matters only for Meshes vs Sphere prims —
        // both check first so we don't recurse into their materials as prims.
        if let Ok(Some(mesh)) = UsdMesh::get(&stage, prim.path().clone()) {
            let mat = resolve_material(&stage, &prim);
            emit_mesh(&mut world, &prim, &mesh, this_world, mat);
        } else if let Ok(Some(sphere)) = UsdSphere::get(&stage, prim.path().clone()) {
            let mat = resolve_material(&stage, &prim);
            emit_sphere(&mut world, &prim, &sphere, this_world, mat);
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

    Ok(Scene::new(camera, world, lights, settings))
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

/// Local-to-parent transform of `prim`, tried across the concrete
/// Xformable schemas the tracer cares about.
fn local_matrix_at(stage: &Stage, prim: &Prim) -> GMat4 {
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
    false
}

// -----------------------------------------------------------------------
// Mesh
// -----------------------------------------------------------------------

fn emit_mesh(
    world: &mut HittableList,
    prim: &Prim,
    mesh: &UsdMesh,
    world_xf: GMat4,
    material: Arc<dyn Material>,
) {
    let points: Option<Vec<Vec3f>> = mesh
        .points_attr()
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::Vec3fVec(v) => Some(v),
            _ => None,
        });
    let counts: Option<Vec<i32>> = mesh
        .face_vertex_counts_attr()
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::IntVec(v) => Some(v),
            _ => None,
        });
    let indices: Option<Vec<i32>> = mesh
        .face_vertex_indices_attr()
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::IntVec(v) => Some(v),
            _ => None,
        });

    let (points, counts, indices) = match (points, counts, indices) {
        (Some(p), Some(c), Some(i)) => (p, c, i),
        _ => {
            debug!(
                "Mesh at {} missing points / faceVertexCounts / faceVertexIndices — skipped",
                prim.path()
            );
            return;
        }
    };

    let verts: Vec<Vec3A> = points
        .iter()
        .map(|p| {
            let v = world_xf.transform_point3(Vec3::new(p.x, p.y, p.z));
            Vec3A::new(v.x, v.y, v.z)
        })
        .collect();

    let mut tris: Vec<Arc<dyn Hittable>> = Vec::new();
    let mut offset = 0usize;
    for &fc in &counts {
        let fc = fc as usize;
        if fc < 3 || offset + fc > indices.len() {
            offset += fc;
            continue;
        }
        for k in 1..(fc - 1) {
            let i0 = indices[offset] as usize;
            let i1 = indices[offset + k] as usize;
            let i2 = indices[offset + k + 1] as usize;
            if i0 >= verts.len() || i1 >= verts.len() || i2 >= verts.len() {
                continue;
            }
            tris.push(Arc::new(Triangle::new(
                verts[i0],
                verts[i1],
                verts[i2],
                material.clone(),
            )));
        }
        offset += fc;
    }

    if tris.is_empty() {
        debug!("Mesh at {} produced no triangles", prim.path());
        return;
    }

    let bvh = BVHNode::build(tris);
    world.add(Box::new(BvhBox(bvh)) as Box<dyn Hittable>);
}

/// `HittableList::add` requires `Box<dyn Hittable>`; the BVH we build is an
/// `Arc<dyn Hittable>`. Wrap it in a boxed newtype that forwards `Hittable`.
struct BvhBox(Arc<dyn Hittable>);

impl Hittable for BvhBox {
    fn hit(
        &self,
        r: &crate::ray::Ray,
        t_min: f32,
        t_max: f32,
        rec: &mut crate::hittable::HitRecord,
    ) -> bool {
        self.0.hit(r, t_min, t_max, rec)
    }
    fn bounding_box(&self) -> Option<crate::aabb::AABB> {
        self.0.bounding_box()
    }
}

// -----------------------------------------------------------------------
// Sphere
// -----------------------------------------------------------------------

fn emit_sphere(
    world: &mut HittableList,
    prim: &Prim,
    sphere: &UsdSphere,
    world_xf: GMat4,
    material: Arc<dyn Material>,
) {
    let radius = sphere
        .radius_attr()
        .get::<sdf::Value>()
        .ok()
        .flatten()
        .and_then(|v| match v {
            sdf::Value::Double(d) => Some(d as f32),
            sdf::Value::Float(f) => Some(f),
            _ => None,
        })
        .unwrap_or(1.0);
    let center_world = world_xf.transform_point3(Vec3::ZERO);
    let center = Vec3A::new(center_world.x, center_world.y, center_world.z);
    debug!(
        "Sphere at {} radius={} center={:?}",
        prim.path(),
        radius,
        center
    );
    world.add(Box::new(CrustSphere::new(center, radius, material)));
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

fn emit_sphere_light(
    world: &mut HittableList,
    lights: &mut LightList,
    light: &SphereLight,
    world_xf: GMat4,
) {
    let radius = attr_f32(&light.radius_attr()).unwrap_or(0.5);
    let intensity = attr_f32(&light.intensity_attr()).unwrap_or(1.0);
    let exposure = attr_f32(&light.exposure_attr()).unwrap_or(0.0);
    let color = attr_color3f(&light.color_attr()).unwrap_or([1.0, 1.0, 1.0]);
    let gain = intensity * 2f32.powf(exposure);
    let effective = Vec3A::new(color[0] * gain, color[1] * gain, color[2] * gain);
    let pos_v = world_xf.transform_point3(Vec3::ZERO);
    let position = Vec3A::new(pos_v.x, pos_v.y, pos_v.z);

    let emissive = Emissive::new(effective, position, radius);
    let em_light: Arc<dyn Light> = Arc::new(emissive.clone());
    lights.add(em_light);
    let em_material: Arc<dyn Material> = Arc::new(emissive);
    world.add(Box::new(CrustSphere::new(position, radius, em_material)));
    debug!(
        "SphereLight: pos={:?} radius={} effective_color={:?}",
        position, radius, effective
    );
}

fn warn_unsupported_light(stage: &Stage, prim: &Prim) {
    let warn_type = |name: &str| {
        warn!(
            "USD light type '{}' at {} is not yet supported — skipped",
            name,
            prim.path()
        );
    };
    if DistantLight::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .is_some()
    {
        warn_type("DistantLight");
    } else if RectLight::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .is_some()
    {
        warn_type("RectLight");
    } else if DiskLight::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .is_some()
    {
        warn_type("DiskLight");
    } else if DomeLight::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .is_some()
    {
        warn_type("DomeLight");
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

fn resolve_material(stage: &Stage, prim: &Prim) -> Arc<dyn Material> {
    let mat_path = MaterialBindingAPI::get(stage, prim.path().clone())
        .ok()
        .flatten()
        .and_then(|b| b.direct_binding("").ok().flatten());

    let Some(mat_path) = mat_path else {
        return default_material();
    };

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

    RenderSettings::new(spp, max_depth, w, h, min_spp, variance, frame)
        .with_guiding(guiding, guiding_iters, guiding_prob)
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
