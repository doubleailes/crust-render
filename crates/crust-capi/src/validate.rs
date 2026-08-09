//! Argument validation — the boundary's safety net.
//!
//! The workspace builds release with `panic = "abort"`, so nothing here may
//! rely on unwinding: every raw pointer and count is checked *before* a
//! slice or reference is materialized, and every float the engine assumes
//! finite is checked on the way in.

use crate::status::CrustStatus;
use crust_core::Vec3A;

pub(crate) type CResult<T> = Result<T, CrustStatus>;

/// The engine's own dimension cap (see `crust.h`).
pub(crate) const MAX_DIM: u32 = 65_536;

/// A required shared reference.
pub(crate) fn require<'a, T>(ptr: *const T) -> CResult<&'a T> {
    // SAFETY: non-null checked; validity and aliasing are the caller's
    // contract per the header ("externally synchronized" handles).
    unsafe { ptr.as_ref() }.ok_or(CrustStatus::NullArgument)
}

/// A required exclusive reference.
pub(crate) fn require_mut<'a, T>(ptr: *mut T) -> CResult<&'a mut T> {
    // SAFETY: as `require`, plus the header's exclusivity contract.
    unsafe { ptr.as_mut() }.ok_or(CrustStatus::NullArgument)
}

/// A required input array of exactly `len` elements (`len > 0`).
pub(crate) fn slice<'a, T>(ptr: *const T, len: usize) -> CResult<&'a [T]> {
    if ptr.is_null() {
        return Err(CrustStatus::NullArgument);
    }
    if len == 0 || len > isize::MAX as usize / size_of::<T>().max(1) {
        return Err(CrustStatus::InvalidArgument);
    }
    // SAFETY: non-null and byte size representable, checked above; contents
    // and lifetime are the caller's contract.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// A required output array of exactly `len` elements.
pub(crate) fn slice_mut<'a, T>(ptr: *mut T, len: usize) -> CResult<&'a mut [T]> {
    if ptr.is_null() {
        return Err(CrustStatus::NullArgument);
    }
    if len == 0 || len > isize::MAX as usize / size_of::<T>().max(1) {
        return Err(CrustStatus::InvalidArgument);
    }
    // SAFETY: as `slice`, plus the caller's exclusivity contract for the
    // duration of the call.
    Ok(unsafe { std::slice::from_raw_parts_mut(ptr, len) })
}

pub(crate) fn finite(v: f32) -> CResult<f32> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(CrustStatus::InvalidArgument)
    }
}

/// A required, all-finite float triple, as the engine's vector type.
pub(crate) fn finite3(ptr: *const f32) -> CResult<Vec3A> {
    let v = slice(ptr, 3)?;
    let v = Vec3A::new(v[0], v[1], v[2]);
    if v.is_finite() {
        Ok(v)
    } else {
        Err(CrustStatus::InvalidArgument)
    }
}

/// Writes an out-parameter if the caller supplied one.
pub(crate) fn write_out<T>(ptr: *mut T, value: T) {
    if !ptr.is_null() {
        // SAFETY: non-null checked; a valid slot is the caller's contract.
        unsafe { *ptr = value };
    }
}
