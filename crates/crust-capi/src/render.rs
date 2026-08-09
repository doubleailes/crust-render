//! The renderer entry points: stepping and framebuffer/AOV reads.

use crate::handles::RendererHandle;
use crate::status::{CrustStatus, CrustStepStatus};
use crate::validate::{CResult, require, require_mut, slice_mut, write_out};

/// `CrustStatus crust_renderer_step(CrustRenderer*, uint32_t spp,
///     CrustStepStatus* out_status, uint32_t* out_spp_done);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_step(
    renderer: *mut RendererHandle,
    spp: u32,
    out_status: *mut CrustStepStatus,
    out_spp_done: *mut u32,
) -> CrustStatus {
    let result = (|| -> CResult<(CrustStepStatus, u32)> {
        let handle = require_mut(renderer)?;
        if spp == 0 {
            return Err(CrustStatus::InvalidArgument);
        }
        Ok(handle.step(spp))
    })();
    match result {
        Ok((status, spp_done)) => {
            write_out(out_status, status);
            write_out(out_spp_done, spp_done);
            CrustStatus::Ok
        }
        Err(status) => status,
    }
}

/// `bool crust_renderer_is_converged(const CrustRenderer*);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_is_converged(renderer: *const RendererHandle) -> bool {
    require(renderer).map(|h| h.is_converged()).unwrap_or(false)
}

/// `uint32_t crust_renderer_spp_done(const CrustRenderer*);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_spp_done(renderer: *const RendererHandle) -> u32 {
    require(renderer).map(|h| h.spp_done()).unwrap_or(0)
}

/// `void crust_renderer_get_dimensions(const CrustRenderer*,
///     uint32_t* width, uint32_t* height);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_get_dimensions(
    renderer: *const RendererHandle,
    width: *mut u32,
    height: *mut u32,
) {
    let (w, h) = require(renderer).map(|r| r.dimensions()).unwrap_or((0, 0));
    write_out(width, w);
    write_out(height, h);
}

/// Shared shape of the read entry points: capacity check, then a bounded
/// output slice of `stride` elements per pixel.
fn read_into<F>(
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
        let handle = require_mut(renderer)?;
        let pixels = handle.pixel_count();
        if capacity_px < pixels {
            return Err(CrustStatus::BufferTooSmall);
        }
        let len = pixels
            .checked_mul(stride)
            .ok_or(CrustStatus::InvalidArgument)?;
        let out = slice_mut(out, len)?;
        fill(handle, out);
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_renderer_read_color(CrustRenderer*, float* rgba,
///     size_t capacity_px);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_read_color(
    renderer: *mut RendererHandle,
    rgba: *mut f32,
    capacity_px: usize,
) -> CrustStatus {
    read_into(renderer, rgba, capacity_px, 4, RendererHandle::read_color)
}

/// `CrustStatus crust_renderer_read_aov_depth(CrustRenderer*, float* depth,
///     size_t capacity_px);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_read_aov_depth(
    renderer: *mut RendererHandle,
    depth: *mut f32,
    capacity_px: usize,
) -> CrustStatus {
    read_into(renderer, depth, capacity_px, 1, RendererHandle::read_depth)
}

/// `CrustStatus crust_renderer_read_aov_normal(CrustRenderer*, float* xyz,
///     size_t capacity_px);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_read_aov_normal(
    renderer: *mut RendererHandle,
    xyz: *mut f32,
    capacity_px: usize,
) -> CrustStatus {
    read_into(renderer, xyz, capacity_px, 3, RendererHandle::read_normal)
}

/// `CrustStatus crust_renderer_read_aov_id(CrustRenderer*, uint32_t* geom_prim,
///     size_t capacity_px);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_read_aov_id(
    renderer: *mut RendererHandle,
    geom_prim: *mut u32,
    capacity_px: usize,
) -> CrustStatus {
    let result = (|| -> CResult<()> {
        let handle = require_mut(renderer)?;
        let pixels = handle.pixel_count();
        if capacity_px < pixels {
            return Err(CrustStatus::BufferTooSmall);
        }
        let len = pixels
            .checked_mul(2)
            .ok_or(CrustStatus::InvalidArgument)?;
        let out = slice_mut(geom_prim, len)?;
        handle.read_id(out);
        Ok(())
    })();
    result.err().unwrap_or(CrustStatus::Ok)
}

/// `CrustStatus crust_renderer_read_aov_alpha(CrustRenderer*, float* alpha,
///     size_t capacity_px);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_read_aov_alpha(
    renderer: *mut RendererHandle,
    alpha: *mut f32,
    capacity_px: usize,
) -> CrustStatus {
    read_into(renderer, alpha, capacity_px, 1, RendererHandle::read_alpha)
}

/// `void crust_renderer_destroy(CrustRenderer*);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_renderer_destroy(renderer: *mut RendererHandle) {
    if !renderer.is_null() {
        // SAFETY: created by `crust_scene_commit`; use after destroy is
        // forbidden by the header contract. RendererHandle's Drop releases
        // the session before the Renderer (see handles.rs).
        drop(unsafe { Box::from_raw(renderer) });
    }
}
