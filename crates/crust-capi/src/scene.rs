//! The scene-builder entry points.

use crate::handles::{RendererHandle, SceneHandle, TokenHandle};
use crate::material::{CrustMaterial, CrustRenderSettings};
use crate::status::CrustStatus;
use crate::validate::{self, CResult, require, require_mut, slice, write_out};
use crust_core::rt::Geometry;
use crust_core::{
    AreaLight, Camera, DistantLight, DomeLight, Emissive, EnvironmentMap, LightList,
    MASK_INDIRECT, MASK_SHADOW, Mat4, RectShape, Renderer, SphereShape, StopToken, Vec3A,
};
use glam::Mat3A;
use std::sync::Arc;

/// `CrustScene* crust_scene_create(void);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_scene_create() -> *mut SceneHandle {
    Box::into_raw(Box::new(SceneHandle::new()))
}

/// `void crust_scene_destroy(CrustScene* scene);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_destroy(scene: *mut SceneHandle) {
    if !scene.is_null() {
        // SAFETY: created by `crust_scene_create`; use after destroy is
        // forbidden by the header contract.
        drop(unsafe { Box::from_raw(scene) });
    }
}

/// Reads a required point/vector array (`3 * count` floats, all finite).
///
/// # Safety
/// Carries [`crate::validate::slice`]'s contract for `ptr` with
/// `3 * count` elements.
unsafe fn read_vec3s(ptr: *const f32, count: usize) -> CResult<Vec<Vec3A>> {
    let len = count
        .checked_mul(3)
        .ok_or(CrustStatus::InvalidArgument)?;
    let flat = unsafe { slice(ptr, len) }?;
    if !flat.iter().all(|v| v.is_finite()) {
        return Err(CrustStatus::InvalidArgument);
    }
    Ok(flat
        .chunks_exact(3)
        .map(|c| Vec3A::new(c[0], c[1], c[2]))
        .collect())
}

/// Validates and reads a triangle mesh's arrays into kernel form — the
/// shared front half of `crust_scene_add_mesh` and
/// `crust_scene_add_instance`.
///
/// # Safety
/// Carries [`crate::validate::slice`]'s contract for each array:
/// `3 * vertex_count` floats, `3 * triangle_count` indices, and (when
/// non-NULL) `3 * vertex_count` normal floats.
unsafe fn read_mesh_geometry(
    positions: *const f32,
    vertex_count: usize,
    tri_indices: *const u32,
    triangle_count: usize,
    normals_or_null: *const f32,
) -> CResult<Geometry> {
    if vertex_count > u32::MAX as usize {
        return Err(CrustStatus::InvalidArgument);
    }
    let vertices = unsafe { read_vec3s(positions, vertex_count) }?;
    let index_len = triangle_count
        .checked_mul(3)
        .ok_or(CrustStatus::InvalidArgument)?;
    let indices: Vec<[u32; 3]> = unsafe { slice(tri_indices, index_len) }?
        .chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    let normals = if normals_or_null.is_null() {
        None
    } else {
        Some(unsafe { read_vec3s(normals_or_null, vertex_count) }?)
    };
    Ok(Geometry::TriangleMesh {
        vertices,
        indices,
        normals,
    })
}

/// `CrustStatus crust_scene_add_mesh(CrustScene*, const float* positions,
///     size_t vertex_count, const uint32_t* tri_indices,
///     size_t triangle_count, const float* normals_or_null,
///     const CrustMaterial*, uint32_t* out_geom_id);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_add_mesh(
    scene: *mut SceneHandle,
    positions: *const f32,
    vertex_count: usize,
    tri_indices: *const u32,
    triangle_count: usize,
    normals_or_null: *const f32,
    material: *const CrustMaterial,
    out_geom_id: *mut u32,
) -> CrustStatus {
    let result = (|| -> CResult<u32> {
        let handle = unsafe { require_mut(scene) }?;
        let pbr = unsafe { require(material) }?.to_openpbr()?;
        // SAFETY: forwarded from this export's `# Safety` contract.
        let geometry = unsafe {
            read_mesh_geometry(
                positions,
                vertex_count,
                tri_indices,
                triangle_count,
                normals_or_null,
            )
        }?;
        let builder = handle.builder_mut()?;
        Ok(builder.attach(geometry, Arc::new(pbr)))
    })();
    match result {
        Ok(id) => {
            unsafe { write_out(out_geom_id, id) };
            CrustStatus::Ok
        }
        Err(status) => status,
    }
}

/// `CrustStatus crust_scene_add_instance(CrustScene*, CrustGeoCache*,
///     uint64_t key, uint32_t version,
///     const float* positions_or_null, size_t vertex_count,
///     const uint32_t* tri_indices_or_null, size_t triangle_count,
///     const float* normals_or_null, const double xform[16],
///     const CrustMaterial*, uint32_t* out_geom_id);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn crust_scene_add_instance(
    scene: *mut SceneHandle,
    cache: *mut crate::geo_cache::GeoCacheHandle,
    key: u64,
    version: u32,
    positions_or_null: *const f32,
    vertex_count: usize,
    tri_indices_or_null: *const u32,
    triangle_count: usize,
    normals_or_null: *const f32,
    xform: *const f64,
    material: *const CrustMaterial,
    out_geom_id: *mut u32,
) -> CrustStatus {
    let result = (|| -> CResult<u32> {
        let handle = unsafe { require_mut(scene) }?;
        let cache = unsafe { require(cache.cast_const()) }?;
        let pbr = unsafe { require(material) }?.to_openpbr()?;

        // The placement, validated before any geometry work: the kernel's
        // instance path requires an invertible transform (a zero scale is
        // the common "hide this" idiom — the caller skips those).
        let m = unsafe { slice(xform, 16) }?;
        if !m.iter().all(|v| v.is_finite()) {
            return Err(CrustStatus::InvalidArgument);
        }
        let mut cols = [0.0f64; 16];
        cols.copy_from_slice(m);
        let placement64 = glam::DMat4::from_cols_array(&cols);
        if placement64.determinant().abs() < 1e-12 {
            return Err(CrustStatus::InvalidArgument);
        }
        let placement = glam::Affine3A::from_mat4(placement64.as_mat4());

        // The prototype: cached committed scene, or built from the arrays
        // on a miss. On a hit the arrays are never read (they may be NULL —
        // that is the whole point of `crust_geo_cache_contains`).
        let proto = match cache.lookup(key, version) {
            Some(scene) => scene,
            None => {
                // SAFETY: forwarded from this export's `# Safety` contract
                // (on a cache miss the arrays are required and valid).
                let geometry = unsafe {
                    read_mesh_geometry(
                        positions_or_null,
                        vertex_count,
                        tri_indices_or_null,
                        triangle_count,
                        normals_or_null,
                    )
                }?;
                let mut inner = crust_core::rt::SceneBuilder::new();
                inner.attach(geometry);
                let scene = Arc::new(inner.commit());
                cache.insert(key, version, scene.clone());
                scene
            }
        };

        let builder = handle.builder_mut()?;
        Ok(builder.attach(
            crust_core::rt::Geometry::Instance {
                scene: proto,
                transform: placement,
                transform_end: None,
            },
            Arc::new(pbr),
        ))
    })();
    match result {
        Ok(id) => {
            unsafe { write_out(out_geom_id, id) };
            CrustStatus::Ok
        }
        Err(status) => status,
    }
}

/// `CrustStatus crust_scene_add_sphere(CrustScene*, const float center[3],
///     float radius, const CrustMaterial*, uint32_t* out_geom_id);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_add_sphere(
    scene: *mut SceneHandle,
    center: *const f32,
    radius: f32,
    material: *const CrustMaterial,
    out_geom_id: *mut u32,
) -> CrustStatus {
    let result = (|| -> CResult<u32> {
        let handle = unsafe { require_mut(scene) }?;
        let pbr = unsafe { require(material) }?.to_openpbr()?;
        let center = unsafe { validate::finite3(center) }?;
        if !(radius.is_finite() && radius > 0.0) {
            return Err(CrustStatus::InvalidArgument);
        }
        let builder = handle.builder_mut()?;
        Ok(builder.attach(Geometry::Sphere { center, radius }, Arc::new(pbr)))
    })();
    match result {
        Ok(id) => {
            unsafe { write_out(out_geom_id, id) };
            CrustStatus::Ok
        }
        Err(status) => status,
    }
}

/// Geometry-backed lights are hidden from camera rays (the industry
/// convention crust's USD importer also follows): shadow and indirect rays
/// see the source, the camera does not.
const LIGHT_MASK: u32 = MASK_SHADOW | MASK_INDIRECT;

/// `CrustStatus crust_scene_add_sphere_light(CrustScene*,
///     const float center[3], float radius, const float radiance[3]);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_add_sphere_light(
    scene: *mut SceneHandle,
    center: *const f32,
    radius: f32,
    radiance: *const f32,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(scene) }?;
        let center = unsafe { validate::finite3(center) }?;
        let radiance = unsafe { validate::finite3(radiance) }?;
        if !(radius.is_finite() && radius > 0.0) {
            return Err(CrustStatus::InvalidArgument);
        }
        // The engine convention (usd_import's emit_sphere_light): one
        // shared Emissive drives both the world geometry the rays hit and
        // the light-list entry NEE samples, tied together by the geom_id.
        let material = Arc::new(Emissive::new(radiance));
        let builder = handle.builder_mut()?;
        let geom_id =
            builder.attach_masked(Geometry::Sphere { center, radius }, material.clone(), LIGHT_MASK);
        handle.lights.add(Arc::new(AreaLight::new(
            Box::new(SphereShape { center, radius }),
            material,
            geom_id,
        )));
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_scene_add_rect_light(CrustScene*, const float origin[3],
///     const float edge_u[3], const float edge_v[3], const float radiance[3]);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_add_rect_light(
    scene: *mut SceneHandle,
    origin: *const f32,
    edge_u: *const f32,
    edge_v: *const f32,
    radiance: *const f32,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(scene) }?;
        let origin = unsafe { validate::finite3(origin) }?;
        let edge_u = unsafe { validate::finite3(edge_u) }?;
        let edge_v = unsafe { validate::finite3(edge_v) }?;
        let radiance = unsafe { validate::finite3(radiance) }?;
        let normal = edge_u.cross(edge_v);
        if normal.length_squared() < 1e-20 {
            return Err(CrustStatus::InvalidArgument); // degenerate rectangle
        }
        let normal = normal.normalize();
        // Same two-triangle emissive quad the USD importer builds for a
        // UsdLux RectLight, shared-Emissive + geom_id wiring included.
        let material = Arc::new(Emissive::new(radiance));
        let (c00, c10, c11, c01) = (
            origin,
            origin + edge_u,
            origin + edge_u + edge_v,
            origin + edge_v,
        );
        let builder = handle.builder_mut()?;
        let geom_id = builder.attach_masked(
            Geometry::TriangleMesh {
                vertices: vec![c00, c10, c11, c01],
                indices: vec![[0, 1, 2], [0, 2, 3]],
                normals: None,
            },
            material.clone(),
            LIGHT_MASK,
        );
        handle.lights.add(Arc::new(AreaLight::new(
            Box::new(RectShape::new(origin, edge_u, edge_v, normal)),
            material,
            geom_id,
        )));
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_scene_add_distant_light(CrustScene*,
///     const float direction[3], const float irradiance[3], float angle_deg);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_add_distant_light(
    scene: *mut SceneHandle,
    direction: *const f32,
    irradiance: *const f32,
    angle_deg: f32,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(scene) }?;
        handle.ensure_live()?;
        let direction = unsafe { validate::finite3(direction) }?;
        let irradiance = unsafe { validate::finite3(irradiance) }?;
        if direction.length_squared() < 1e-20 || !(angle_deg.is_finite() && angle_deg >= 0.0) {
            return Err(CrustStatus::InvalidArgument);
        }
        // DistantLight widens tiny angles to its own floor internally.
        handle.lights.add(Arc::new(DistantLight::new(
            direction.normalize(),
            irradiance,
            angle_deg,
        )));
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// A dome is at infinity — only the rotation of its frame matters.
/// Normalize the columns like the USD importer does, so a rotation with
/// uniform scale folded in still orients correctly.
///
/// # Safety
/// Carries [`crate::validate::slice`]'s contract for 9 floats.
unsafe fn read_dome_rotation(rotation: *const f32) -> CResult<Mat3A> {
    let r = unsafe { slice(rotation, 9) }?;
    if !r.iter().all(|v| v.is_finite()) {
        return Err(CrustStatus::InvalidArgument);
    }
    let mut rotation =
        Mat3A::from_cols_array(&[r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8]]);
    for col in [
        &mut rotation.x_axis,
        &mut rotation.y_axis,
        &mut rotation.z_axis,
    ] {
        if col.length_squared() < 1e-20 {
            return Err(CrustStatus::InvalidArgument);
        }
        *col = col.normalize();
    }
    Ok(rotation)
}

/// `CrustStatus crust_scene_add_dome_light_file(CrustScene*,
///     const float tint[3], const char* path, const float rotation[9]);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_add_dome_light_file(
    scene: *mut SceneHandle,
    tint: *const f32,
    path: *const std::ffi::c_char,
    rotation: *const f32,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(scene) }?;
        handle.ensure_live()?;
        let tint = unsafe { validate::finite3(tint) }?;
        let rotation = unsafe { read_dome_rotation(rotation) }?;
        if path.is_null() {
            return Err(CrustStatus::NullArgument);
        }
        // SAFETY: non-null checked; a NUL-terminated string is the caller's
        // contract per the header.
        let path = unsafe { std::ffi::CStr::from_ptr(path) }
            .to_str()
            .map_err(|_| CrustStatus::InvalidArgument)?;
        let map = crate::dome::load_environment(std::path::Path::new(path))
            .ok_or(CrustStatus::InvalidArgument)?;
        handle
            .lights
            .add(Arc::new(DomeLight::new(tint, Some(Arc::new(map)), rotation)));
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_scene_add_dome_light(CrustScene*, const float tint[3],
///     uint32_t tex_width, uint32_t tex_height, const float* pixels_or_null,
///     const float rotation[9]);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_add_dome_light(
    scene: *mut SceneHandle,
    tint: *const f32,
    tex_width: u32,
    tex_height: u32,
    pixels_or_null: *const f32,
    rotation: *const f32,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(scene) }?;
        handle.ensure_live()?;
        let tint = unsafe { validate::finite3(tint) }?;
        let rotation = unsafe { read_dome_rotation(rotation) }?;
        let map = if pixels_or_null.is_null() {
            None
        } else {
            let (w, h) = (tex_width as usize, tex_height as usize);
            let count = w
                .checked_mul(h)
                .and_then(|px| px.checked_mul(3))
                .ok_or(CrustStatus::InvalidArgument)?;
            let flat = unsafe { slice(pixels_or_null, count) }?;
            let pixels: Vec<Vec3A> = flat
                .chunks_exact(3)
                .map(|c| Vec3A::new(c[0], c[1], c[2]))
                .collect();
            if pixels.iter().any(|p| !p.is_finite()) {
                return Err(CrustStatus::InvalidArgument);
            }
            Some(Arc::new(
                EnvironmentMap::new(w, h, pixels).ok_or(CrustStatus::InvalidArgument)?,
            ))
        };
        handle
            .lights
            .add(Arc::new(DomeLight::new(tint, map, rotation)));
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_scene_set_camera(CrustScene*, const double view[16],
///     const double proj[16], float aperture, float focus_distance);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_set_camera(
    scene: *mut SceneHandle,
    view: *const f64,
    proj: *const f64,
    aperture: f32,
    focus_distance: f32,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(scene) }?;
        handle.ensure_live()?;
        let to_mat4 = |ptr: *const f64| -> CResult<Mat4> {
            let m = unsafe { slice(ptr, 16) }?;
            let mut cols = [0.0f32; 16];
            for (dst, src) in cols.iter_mut().zip(m) {
                if !src.is_finite() {
                    return Err(CrustStatus::InvalidArgument);
                }
                *dst = *src as f32;
            }
            Ok(Mat4::from_cols_array(&cols))
        };
        let view = to_mat4(view)?;
        let proj = to_mat4(proj)?;
        let aperture = validate::finite(aperture)?;
        if aperture < 0.0 {
            return Err(CrustStatus::InvalidArgument);
        }
        // `from_view_projection` validates the matrices and the focus
        // distance itself (singular / orthographic / non-positive focus).
        let camera = Camera::from_view_projection(view, proj, aperture, focus_distance)
            .map_err(|_| CrustStatus::InvalidCamera)?;
        handle.camera = Some(camera);
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_scene_set_render_settings(CrustScene*,
///     const CrustRenderSettings*);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_set_render_settings(
    scene: *mut SceneHandle,
    settings: *const CrustRenderSettings,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(scene) }?;
        handle.ensure_live()?;
        handle.settings = Some(unsafe { require(settings) }?.to_settings()?);
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_scene_commit(CrustScene*,
///     const CrustStopToken* token_or_null, CrustRenderer** out_renderer);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_scene_commit(
    scene: *mut SceneHandle,
    token_or_null: *const TokenHandle,
    out_renderer: *mut *mut RendererHandle,
) -> CrustStatus {
    let result = (|| -> CResult<*mut RendererHandle> {
        let handle = unsafe { require_mut(scene) }?;
        if out_renderer.is_null() {
            return Err(CrustStatus::NullArgument);
        }
        let camera = handle.camera.ok_or(CrustStatus::BadState)?;
        let settings = handle.settings.ok_or(CrustStatus::BadState)?;
        // Cloned, not moved: the caller keeps its token handle and may
        // stop/destroy it independently of this renderer.
        let stop = match unsafe { token_or_null.as_ref() } {
            Some(token) => token.0.clone(),
            None => StopToken::new(),
        };
        // Spends the builder: this is what flips every later add/set call
        // on this handle to BAD_STATE.
        let builder = handle.builder.take().ok_or(CrustStatus::BadState)?;
        let lights = std::mem::replace(&mut handle.lights, LightList::new());
        let renderer = Renderer::new(camera, builder.commit(), lights, settings);
        Ok(Box::into_raw(RendererHandle::new(renderer, stop)))
    })();
    match result {
        Ok(ptr) => {
            unsafe { write_out(out_renderer, ptr) };
            CrustStatus::Ok
        }
        Err(status) => status,
    }
}
