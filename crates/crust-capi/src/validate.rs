//! Argument validation — the boundary's safety net.
//!
//! The workspace builds release with `panic = "abort"`, so nothing here may
//! rely on unwinding: every raw pointer and count is checked *before* a
//! slice or reference is materialized, and every float the engine assumes
//! finite is checked on the way in.
//!
//! Every helper that dereferences a caller pointer is an `unsafe fn`: what
//! the checks here CANNOT establish — validity, alignment, initialization,
//! liveness, exclusivity — is the caller's `# Safety` contract, threaded up
//! through the `unsafe extern "C"` exports to crust.h.

use crate::status::CrustStatus;
use crust_core::Vec3A;
use std::mem::size_of;

pub(crate) type CResult<T> = Result<T, CrustStatus>;

/// The engine's own dimension cap (see `crust.h`).
pub(crate) const MAX_DIM: u32 = 65_536;

/// A required shared reference.
///
/// # Safety
/// `ptr` must be NULL or valid, aligned, initialized and live for the
/// duration of the call, with no concurrent mutation.
pub(crate) unsafe fn require<'a, T>(ptr: *const T) -> CResult<&'a T> {
    // SAFETY: non-null checked here; everything else is this fn's contract.
    unsafe { ptr.as_ref() }.ok_or(CrustStatus::NullArgument)
}

/// A required exclusive reference.
///
/// # Safety
/// As [`require`], plus: nothing else may access the pointee for the
/// duration of the call.
pub(crate) unsafe fn require_mut<'a, T>(ptr: *mut T) -> CResult<&'a mut T> {
    // SAFETY: non-null checked here; exclusivity is this fn's contract.
    unsafe { ptr.as_mut() }.ok_or(CrustStatus::NullArgument)
}

/// A required input array of exactly `len` elements (`len > 0`).
///
/// # Safety
/// `ptr` must be NULL or point to at least `len` valid, aligned,
/// initialized elements that stay live and unmutated for the call.
pub(crate) unsafe fn slice<'a, T>(ptr: *const T, len: usize) -> CResult<&'a [T]> {
    if ptr.is_null() {
        return Err(CrustStatus::NullArgument);
    }
    if len == 0 || len > isize::MAX as usize / size_of::<T>().max(1) {
        return Err(CrustStatus::InvalidArgument);
    }
    // SAFETY: non-null and byte size representable, checked above; element
    // validity is this fn's contract.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// A required output array of exactly `len` elements.
///
/// # Safety
/// As [`slice`], plus: nothing else may access the elements for the
/// duration of the call.
pub(crate) unsafe fn slice_mut<'a, T>(ptr: *mut T, len: usize) -> CResult<&'a mut [T]> {
    if ptr.is_null() {
        return Err(CrustStatus::NullArgument);
    }
    if len == 0 || len > isize::MAX as usize / size_of::<T>().max(1) {
        return Err(CrustStatus::InvalidArgument);
    }
    // SAFETY: non-null and byte size representable, checked above;
    // validity and exclusivity are this fn's contract.
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
///
/// # Safety
/// `ptr` must be NULL or point to at least 3 valid, initialized floats
/// (see [`slice`]).
pub(crate) unsafe fn finite3(ptr: *const f32) -> CResult<Vec3A> {
    // SAFETY: forwarded — this fn carries `slice`'s contract.
    let v = unsafe { slice(ptr, 3) }?;
    let v = Vec3A::new(v[0], v[1], v[2]);
    if v.is_finite() {
        Ok(v)
    } else {
        Err(CrustStatus::InvalidArgument)
    }
}

/// Writes an out-parameter if the caller supplied one.
///
/// # Safety
/// `ptr` must be NULL or a valid, aligned, exclusively-accessible slot.
pub(crate) unsafe fn write_out<T>(ptr: *mut T, value: T) {
    if !ptr.is_null() {
        // SAFETY: non-null checked here; slot validity and exclusivity are
        // this fn's contract.
        unsafe { *ptr = value };
    }
}
