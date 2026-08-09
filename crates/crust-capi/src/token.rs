//! The stop-token entry points — the only cross-thread part of the ABI.

use crate::handles::TokenHandle;
use crust_core::StopToken;

/// `CrustStopToken* crust_stop_token_create(void);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_stop_token_create() -> *mut TokenHandle {
    Box::into_raw(Box::new(TokenHandle(StopToken::new())))
}

/// `void crust_stop_token_stop(CrustStopToken* token);`
///
/// Callable from any thread, including while a `crust_renderer_step`
/// holding a clone of this token is in flight — `StopToken` is an atomic
/// flag behind an `Arc`, which is the entire point of the type.
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_stop_token_stop(token: *mut TokenHandle) {
    // SAFETY: non-null checked; the pointee is valid until its destroy call
    // per the header contract, and `StopToken::stop` is `&self` + atomic.
    if let Some(token) = unsafe { token.as_ref() } {
        token.0.stop();
    }
}

/// `bool crust_stop_token_is_stopped(const CrustStopToken* token);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_stop_token_is_stopped(token: *const TokenHandle) -> bool {
    // SAFETY: as `crust_stop_token_stop`.
    unsafe { token.as_ref() }.is_some_and(|t| t.0.is_stopped())
}

/// `void crust_stop_token_destroy(CrustStopToken* token);`
///
/// # Safety
/// Every pointer argument must satisfy the crust.h contract: NULL where the
/// header allows it, otherwise valid, aligned and initialized for the whole
/// call, with arrays holding at least the stated element counts, out-params
/// exclusively accessible, and handle pointers live (not destroyed) and
/// externally synchronized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_stop_token_destroy(token: *mut TokenHandle) {
    if !token.is_null() {
        // SAFETY: created by `crust_stop_token_create`; the header forbids
        // use after destroy. Renderers hold their own clone, so dropping
        // the handle never invalidates an in-flight render.
        drop(unsafe { Box::from_raw(token) });
    }
}
