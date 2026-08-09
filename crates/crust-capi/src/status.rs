//! Status codes and version reporting — the error half of the C contract.

use std::ffi::c_char;

/// Result code of every fallible C entry point. Mirrors `CrustStatus` in
/// `crust.h` — same names, same discriminants.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrustStatus {
    Ok = 0,
    NullArgument = 1,
    InvalidArgument = 2,
    InvalidCamera = 3,
    BadState = 4,
    BufferTooSmall = 5,
}

/// Outcome of one `crust_renderer_step`. Mirrors `CrustStepStatus` in
/// `crust.h`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrustStepStatus {
    InProgress = 0,
    Stopped = 1,
    Complete = 2,
}

/// `const char* crust_status_string(CrustStatus status);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_status_string(status: CrustStatus) -> *const c_char {
    let s = match status {
        CrustStatus::Ok => c"ok",
        CrustStatus::NullArgument => c"a required pointer was NULL",
        CrustStatus::InvalidArgument => c"invalid argument (count, non-finite float, or malformed data)",
        CrustStatus::InvalidCamera => c"invalid camera matrices (singular, orthographic, or bad focus)",
        CrustStatus::BadState => c"handle is in the wrong state for this call",
        CrustStatus::BufferTooSmall => c"output buffer capacity is smaller than width*height",
    };
    s.as_ptr()
}

/// `uint32_t crust_api_version(void);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_api_version() -> u32 {
    // Bump together with CRUST_API_VERSION in crust.h.
    1
}

/// `void crust_library_version(uint32_t* major, uint32_t* minor, uint32_t* patch);`
#[unsafe(no_mangle)]
pub extern "C" fn crust_library_version(major: *mut u32, minor: *mut u32, patch: *mut u32) {
    let parts = [
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH"),
    ]
    .map(|s| s.parse::<u32>().unwrap_or(0));
    for (out, value) in [major, minor, patch].into_iter().zip(parts) {
        if !out.is_null() {
            // SAFETY: non-null checked; the caller promises a valid u32 slot
            // per the header contract.
            unsafe { *out = value };
        }
    }
}
