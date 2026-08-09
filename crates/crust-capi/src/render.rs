//! The renderer entry points: stepping, framebuffer/AOV reads, and the
//! in-place edits that restart sampling without touching the world.

use crate::handles::{RendererHandle, TokenHandle};
use crate::material::CrustRenderSettings;
use crate::status::{CrustStatus, CrustStepStatus};
use crate::validate::{CResult, require, require_mut, slice, slice_mut, write_out};
use crust_core::{Camera, Mat4};

/// `CrustStatus crust_renderer_step(CrustRenderer*, uint32_t spp,
///     CrustStepStatus* out_status, uint32_t* out_spp_done);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_step(
    renderer: *mut RendererHandle,
    spp: u32,
    out_status: *mut CrustStepStatus,
    out_spp_done: *mut u32,
) -> CrustStatus {
    let result = (|| -> CResult<(CrustStepStatus, u32)> {
        let handle = unsafe { require_mut(renderer) }?;
        if spp == 0 {
            return Err(CrustStatus::InvalidArgument);
        }
        Ok(handle.step(spp))
    })();
    match result {
        Ok((status, spp_done)) => {
            unsafe { write_out(out_status, status) };
            unsafe { write_out(out_spp_done, spp_done) };
            CrustStatus::Ok
        }
        Err(status) => status,
    }
}

/// `bool crust_renderer_is_converged(const CrustRenderer*);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_is_converged(renderer: *const RendererHandle) -> bool {
    unsafe { require(renderer) }.map(|h| h.is_converged()).unwrap_or(false)
}

/// `uint32_t crust_renderer_spp_done(const CrustRenderer*);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_spp_done(renderer: *const RendererHandle) -> u32 {
    unsafe { require(renderer) }.map(|h| h.spp_done()).unwrap_or(0)
}

/// `void crust_renderer_get_dimensions(const CrustRenderer*,
///     uint32_t* width, uint32_t* height);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_get_dimensions(
    renderer: *const RendererHandle,
    width: *mut u32,
    height: *mut u32,
) {
    let (w, h) = unsafe { require(renderer) }.map(|r| r.dimensions()).unwrap_or((0, 0));
    unsafe { write_out(width, w) };
    unsafe { write_out(height, h) };
}

/// Shared shape of the read entry points: capacity check, then a bounded
/// output slice of `stride` elements per pixel.
///
/// # Safety
/// Carries [`crate::validate::require_mut`]'s contract for `renderer` and
/// [`crate::validate::slice_mut`]'s for `out` with `capacity_px * stride`
/// elements.
unsafe fn read_into<F>(
    renderer: *mut RendererHandle,
    out: *mut f32,
    capacity_px: usize,
    stride: usize,
    fill: F,
) -> CrustStatus
where
    F: FnOnce(&mut RendererHandle, &mut [f32]),
{
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(renderer) }?;
        let pixels = handle.pixel_count();
        if capacity_px < pixels {
            return Err(CrustStatus::BufferTooSmall);
        }
        let len = pixels
            .checked_mul(stride)
            .ok_or(CrustStatus::InvalidArgument)?;
        let out = unsafe { slice_mut(out, len) }?;
        fill(handle, out);
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_renderer_read_color(CrustRenderer*, float* rgba,
///     size_t capacity_px);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_read_color(
    renderer: *mut RendererHandle,
    rgba: *mut f32,
    capacity_px: usize,
) -> CrustStatus {
    // SAFETY: forwarded from this export's `# Safety` contract.
    unsafe { read_into(renderer, rgba, capacity_px, 4, RendererHandle::read_color) }
}

/// `CrustStatus crust_renderer_read_aov_depth(CrustRenderer*, float* depth,
///     size_t capacity_px);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_read_aov_depth(
    renderer: *mut RendererHandle,
    depth: *mut f32,
    capacity_px: usize,
) -> CrustStatus {
    // SAFETY: forwarded from this export's `# Safety` contract.
    unsafe { read_into(renderer, depth, capacity_px, 1, RendererHandle::read_depth) }
}

/// `CrustStatus crust_renderer_read_aov_normal(CrustRenderer*, float* xyz,
///     size_t capacity_px);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_read_aov_normal(
    renderer: *mut RendererHandle,
    xyz: *mut f32,
    capacity_px: usize,
) -> CrustStatus {
    // SAFETY: forwarded from this export's `# Safety` contract.
    unsafe { read_into(renderer, xyz, capacity_px, 3, RendererHandle::read_normal) }
}

/// `CrustStatus crust_renderer_read_aov_id(CrustRenderer*, uint32_t* geom_prim,
///     size_t capacity_px);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_read_aov_id(
    renderer: *mut RendererHandle,
    geom_prim: *mut u32,
    capacity_px: usize,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(renderer) }?;
        let pixels = handle.pixel_count();
        if capacity_px < pixels {
            return Err(CrustStatus::BufferTooSmall);
        }
        let len = pixels
            .checked_mul(2)
            .ok_or(CrustStatus::InvalidArgument)?;
        let out = unsafe { slice_mut(geom_prim, len) }?;
        handle.read_id(out);
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_renderer_read_aov_alpha(CrustRenderer*, float* alpha,
///     size_t capacity_px);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_read_aov_alpha(
    renderer: *mut RendererHandle,
    alpha: *mut f32,
    capacity_px: usize,
) -> CrustStatus {
    // SAFETY: forwarded from this export's `# Safety` contract.
    unsafe { read_into(renderer, alpha, capacity_px, 1, RendererHandle::read_alpha) }
}

/// # Safety
/// Carries [`crate::validate::slice`]'s contract for 16 doubles.
unsafe fn read_mat4(ptr: *const f64) -> CResult<Mat4> {
    let m = unsafe { slice(ptr, 16) }?;
    let mut cols = [0.0f32; 16];
    for (dst, src) in cols.iter_mut().zip(m) {
        if !src.is_finite() {
            return Err(CrustStatus::InvalidArgument);
        }
        *dst = *src as f32;
    }
    Ok(Mat4::from_cols_array(&cols))
}

/// `CrustStatus crust_renderer_update_camera(CrustRenderer*,
///     const double view[16], const double proj[16], float aperture,
///     float focus_distance, const CrustStopToken* token_or_null);`
///
/// Restarts sampling from zero with the new camera — no world/BVH work at
/// all, which is what makes a viewport orbit cheap. The film cannot survive
/// a camera change, so progress resets; a stopped token stays stopped, so
/// pass a fresh one to make the restarted render cancellable.
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_update_camera(
    renderer: *mut RendererHandle,
    view: *const f64,
    proj: *const f64,
    aperture: f32,
    focus_distance: f32,
    token_or_null: *const TokenHandle,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(renderer) }?;
        let view = unsafe { read_mat4(view) }?;
        let proj = unsafe { read_mat4(proj) }?;
        if !(aperture.is_finite() && aperture >= 0.0) {
            return Err(CrustStatus::InvalidArgument);
        }
        let camera = Camera::from_view_projection(view, proj, aperture, focus_distance)
            .map_err(|_| CrustStatus::InvalidCamera)?;
        // SAFETY: read-only access to a caller-owned token, per the header.
        let token = unsafe { token_or_null.as_ref() }.map(|t| t.0.clone());
        handle.edit(token, |r| r.camera = camera);
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_renderer_update_settings(CrustRenderer*,
///     const CrustRenderSettings*, const CrustStopToken* token_or_null);`
///
/// As `crust_renderer_update_camera`, for the render settings (resolution,
/// budget, depth, adaptive stop). Sampling restarts from zero.
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_update_settings(
    renderer: *mut RendererHandle,
    settings: *const CrustRenderSettings,
    token_or_null: *const TokenHandle,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = unsafe { require_mut(renderer) }?;
        let settings = unsafe { require(settings) }?.to_settings()?;
        // SAFETY: read-only access to a caller-owned token, per the header.
        let token = unsafe { token_or_null.as_ref() }.map(|t| t.0.clone());
        handle.edit(token, |r| r.settings = settings);
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `void crust_renderer_destroy(CrustRenderer*);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_renderer_destroy(renderer: *mut RendererHandle) {
    if !renderer.is_null() {
        // SAFETY: created by `crust_scene_commit`; use after destroy is
        // forbidden by the header contract. RendererHandle's Drop releases
        // the session before the Renderer (see handles.rs).
        drop(unsafe { Box::from_raw(renderer) });
    }
}
