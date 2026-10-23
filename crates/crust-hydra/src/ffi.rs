//! The C ABI `hdCrust` links (`include/crust_hydra.h`, design D7): an
//! opaque handle on a [`Session`], status codes, and the last error's
//! message kept per handle.
//!
//! Every entry point runs its body under `catch_unwind`, so a panic inside
//! crust returns [`Status::Panicked`] instead of unwinding into C++ (which
//! would abort the host); built with `--profile hydra`, which unwinds.
//!
//! The `unsafe` here is the pointers C hands in, and nothing else. Each
//! `extern "C"` function is `unsafe` (its pointers are the caller's
//! promise) and exported with `#[unsafe(no_mangle)]`; every dereference goes
//! through one of the five helpers below, each one `unsafe` block with its
//! `// SAFETY:` argument.

use crate::session::{Error, Session};
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Mutex;

/// The header's `CRUST_HYDRA_ABI_VERSION`: bumped whenever a signature or a
/// status changes, so a plugin built against another header refuses to run.
pub const ABI_VERSION: u32 = 2;

/// A call's outcome: `crust_hydra_status` in the header.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok = 0,
    Busy = 1,
    ImportFailed = 2,
    Panicked = 3,
    InvalidArgument = 4,
}

pub use crate::log::LogFn;

/// The handle C holds: the session and what the ABI keeps beside it.
pub struct CrustHydraSession {
    pub(crate) session: Session,
    last_error: Mutex<String>,
}

impl CrustHydraSession {
    /// Records `e` for `crust_hydra_last_error` and returns its status.
    fn fail(&self, e: Error) -> Status {
        let status = match &e {
            Error::Busy(_) => Status::Busy,
            Error::ImportFailed(_) => Status::ImportFailed,
            Error::Panicked(_) => Status::Panicked,
            Error::InvalidArgument(_) => Status::InvalidArgument,
        };
        *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = e.to_string();
        status
    }

    fn status(&self, result: Result<(), Error>) -> Status {
        match result {
            Ok(()) => Status::Ok,
            Err(e) => self.fail(e),
        }
    }
}

/// The message of a caught panic.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// Runs `body` on the handle, a panic caught and recorded as `Panicked`.
fn on(handle: *mut CrustHydraSession, body: impl FnOnce(&CrustHydraSession) -> Status) -> Status {
    let Some(h) = borrow(handle) else {
        return Status::InvalidArgument;
    };
    match catch_unwind(AssertUnwindSafe(|| body(h))) {
        Ok(status) => status,
        Err(payload) => h.fail(Error::Panicked(panic_message(&*payload))),
    }
}

// ---- The five dereferences --------------------------------------------------

/// `ptr` as a UTF-8 string, `None` for null or invalid UTF-8.
fn c_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null, and every entry point's contract (the header) is
    // that a string argument is NUL-terminated and outlives the call; the
    // borrow does not leave it.
    #[allow(unsafe_code)]
    let s = unsafe { CStr::from_ptr(ptr) };
    s.to_str().ok()
}

/// The handle behind `ptr`, `None` for null.
fn borrow<'a>(ptr: *mut CrustHydraSession) -> Option<&'a CrustHydraSession> {
    // SAFETY: a non-null handle came from `crust_hydra_new` and has not been
    // passed to `crust_hydra_free` (the header's contract); only shared
    // references are made, and the session synchronizes itself.
    #[allow(unsafe_code)]
    unsafe {
        ptr.as_ref()
    }
}

/// `len` writable `T`s at `ptr`, `None` for null.
fn out_slice<'a, T>(ptr: *mut T, len: usize) -> Option<&'a mut [T]> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null, and the header's contract is that an output buffer
    // holds the `len` elements its size arguments describe, aligned, and is
    // not read by anyone else during the call.
    #[allow(unsafe_code)]
    Some(unsafe { std::slice::from_raw_parts_mut(ptr, len) })
}

/// Writes `value` through `ptr` unless it is null (an output the caller
/// does not want).
fn put<T>(ptr: *mut T, value: T) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: non-null, and the header's contract is that an output pointer
    // points to one writable, aligned `T`.
    #[allow(unsafe_code)]
    unsafe {
        ptr.write(value)
    }
}

/// Takes back the box `crust_hydra_new` leaked.
fn reclaim(ptr: *mut CrustHydraSession) -> Option<Box<CrustHydraSession>> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: a non-null handle came from `Box::into_raw` in
    // `crust_hydra_new`, and `crust_hydra_free` is called once per handle,
    // after every other call on it has returned (the header's contract).
    #[allow(unsafe_code)]
    Some(unsafe { Box::from_raw(ptr) })
}

/// A handle on `session`, as `crust_hydra_new` would make, with `log`
/// registered: how the tests drive a session built on a loader of their own
/// through the C ABI.
#[cfg(test)]
pub(crate) fn handle(
    session: Session,
    log: Option<LogFn>,
    user: *mut c_void,
) -> *mut CrustHydraSession {
    if let Some(log) = log {
        crate::log::register(session.log_id(), log, user as usize);
    }
    Box::into_raw(Box::new(CrustHydraSession {
        session,
        last_error: Mutex::new(String::new()),
    }))
}

// ---- The entry points -------------------------------------------------------

/// `CRUST_HYDRA_ABI_VERSION` of the library.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn crust_hydra_abi_version() -> u32 {
    ABI_VERSION
}

/// A session on the layers in `session_dir` (created if missing). `log`
/// may be null. Null on error, reported through `log` when there is one.
///
/// # Safety
/// `session_dir` is a NUL-terminated string; `log`, when not null, may be
/// called from any crust thread with `user` until `crust_hydra_free`.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_new(
    session_dir: *const c_char,
    log: Option<LogFn>,
    user: *mut c_void,
) -> *mut CrustHydraSession {
    let made = catch_unwind(|| {
        let dir = c_str(session_dir)
            .ok_or_else(|| Error::InvalidArgument("the session directory is not UTF-8".into()))?;
        Session::new(dir)
    });
    let report = |message: String| {
        if let Some(log) = log
            && let Ok(text) = std::ffi::CString::new(message)
        {
            log(0, text.as_ptr(), user);
        }
    };
    match made {
        Ok(Ok(session)) => {
            if let Some(log) = log {
                // The user pointer as an address: only handed back.
                crate::log::register(session.log_id(), log, user as usize);
            }
            Box::into_raw(Box::new(CrustHydraSession {
                session,
                last_error: Mutex::new(String::new()),
            }))
        }
        Ok(Err(e)) => {
            report(e.to_string());
            std::ptr::null_mut()
        }
        Err(payload) => {
            report(format!("crust panicked: {}", panic_message(&*payload)));
            std::ptr::null_mut()
        }
    }
}

/// Cancels the render, joins crust's threads and frees the handle. Null is
/// ignored.
///
/// # Safety
/// `session` came from `crust_hydra_new`, is freed once, and no other call
/// on it is running or follows.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_free(session: *mut CrustHydraSession) {
    if let Some(h) = reclaim(session) {
        let id = h.session.log_id();
        // Joining the threads may still log: the callback stays until then.
        // A panic while joining is lost with the handle, with no one left to
        // report it to.
        let _ = catch_unwind(AssertUnwindSafe(|| drop(h)));
        crate::log::unregister(id);
    }
}

/// Imports `edits_path` whole and starts rendering it.
///
/// # Safety
/// `session` is a live handle; `edits_path` is a NUL-terminated string.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_load(
    session: *mut CrustHydraSession,
    edits_path: *const c_char,
) -> Status {
    on(session, |h| match c_str(edits_path) {
        Some(path) => h.status(h.session.load(Path::new(path))),
        None => h.fail(Error::InvalidArgument("the layer path is not UTF-8".into())),
    })
}

/// The override layer changed on disk: applies it and restarts the render.
///
/// # Safety
/// `session` is a live handle.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_apply(session: *mut CrustHydraSession) -> Status {
    on(session, |h| h.status(h.session.apply()))
}

/// Copies the latest image into `rgba` (`width` × `height` RGBA floats, rows
/// top-down) when its generation is not `since` (the last one the caller
/// copied, 0 for none), and writes its generation and samples per pixel. An
/// unchanged generation copies nothing, writes `since` back and returns
/// `Ok`; an image of another size returns `Busy` (the render for the new
/// size has not published yet).
///
/// # Safety
/// `session` is a live handle; `rgba` holds `width * height * 4` floats;
/// `generation` and `spp` are null or point to one writable value each.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_snapshot(
    session: *mut CrustHydraSession,
    rgba: *mut f32,
    width: usize,
    height: usize,
    since: u64,
    generation: *mut u64,
    spp: *mut u32,
) -> Status {
    on(session, |h| {
        let current = match h.session.generation() {
            Ok(g) => g,
            Err(e) => return h.fail(e),
        };
        if current == since {
            put(generation, since);
            return Status::Ok;
        }
        let snapshot = match h.session.snapshot() {
            Ok(s) => s,
            Err(e) => return h.fail(e),
        };
        if (snapshot.width, snapshot.height) != (width, height) {
            return h.fail(Error::Busy(format!(
                "the image is {}x{}, the buffer {width}x{height}",
                snapshot.width, snapshot.height
            )));
        }
        let Some(out) = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| out_slice(rgba, n))
        else {
            return h.fail(Error::InvalidArgument("rgba is null".into()));
        };
        out.copy_from_slice(&snapshot.rgba);
        put(generation, snapshot.generation);
        put(spp, snapshot.spp);
        Status::Ok
    })
}

/// How the last `load` or `apply` reached the scene: 0 `settings`, 1
/// `materials`, 2 `lights` (each in place), 3 `full` (an import); -1 before
/// the first.
///
/// # Safety
/// `session` is a live handle.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_applied(session: *mut CrustHydraSession) -> i32 {
    use crust_render::session::delta::Applied;
    let mut applied = -1;
    on(session, |h| {
        applied = match h.session.applied() {
            None => -1,
            Some(Applied::Settings) => 0,
            Some(Applied::Materials) => 1,
            Some(Applied::Lights) => 2,
            Some(Applied::Full) => 3,
        };
        Status::Ok
    });
    applied
}

/// 1 when the render completed (Hydra's `IsConverged`), else 0.
///
/// # Safety
/// `session` is a live handle.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_converged(session: *mut CrustHydraSession) -> i32 {
    let mut converged = 0;
    on(session, |h| {
        converged = i32::from(h.session.converged());
        Status::Ok
    });
    converged
}

/// Copies the completed render's AOV `name` into `data` (`width` × `height`
/// × `channels` floats, interleaved, rows top-down). `Busy` until the render
/// completes; `InvalidArgument` for an unknown name or another shape.
///
/// # Safety
/// `session` is a live handle; `name` is a NUL-terminated string; `data`
/// holds `width * height * channels` floats.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_aov(
    session: *mut CrustHydraSession,
    name: *const c_char,
    data: *mut f32,
    width: usize,
    height: usize,
    channels: usize,
) -> Status {
    on(session, |h| {
        let Some(name) = c_str(name) else {
            return h.fail(Error::InvalidArgument("the AOV name is not UTF-8".into()));
        };
        let aov = match h.session.aov(name) {
            Ok(aov) => aov,
            Err(e) => return h.fail(e),
        };
        if (aov.width, aov.height, aov.channels) != (width, height, channels) {
            return h.fail(Error::InvalidArgument(format!(
                "{name} is {}x{}x{}, the buffer {width}x{height}x{channels}",
                aov.width, aov.height, aov.channels
            )));
        }
        let Some(out) = out_slice(data, aov.data.len()) else {
            return h.fail(Error::InvalidArgument("data is null".into()));
        };
        out.copy_from_slice(&aov.data);
        Status::Ok
    })
}

/// Cancels the running render and waits for it to stop; the last image
/// stays.
///
/// # Safety
/// `session` is a live handle.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_cancel(session: *mut CrustHydraSession) {
    on(session, |h| {
        h.session.cancel();
        Status::Ok
    });
}

/// Forwards log lines down to `level` (0 error, 1 warning, the default, 2
/// info, 3 debug) to the session's callback. Import warnings arrive at 1,
/// once per import with their code and prim; panics at 0.
///
/// # Safety
/// `session` is a live handle.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_set_log_level(session: *mut CrustHydraSession, level: i32) {
    on(session, |h| {
        crate::log::set_level(h.session.log_id(), level);
        Status::Ok
    });
}

/// Copies the last error's message, NUL-terminated and truncated to fit,
/// into `buffer` of `len` bytes (null or 0 copies nothing), and returns its
/// full length in bytes without the NUL, as `snprintf` does.
///
/// # Safety
/// `session` is a live handle; `buffer` is null or holds `len` bytes.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn crust_hydra_last_error(
    session: *mut CrustHydraSession,
    buffer: *mut c_char,
    len: usize,
) -> usize {
    let Some(h) = borrow(session) else {
        return 0;
    };
    let message = h
        .last_error
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    if len > 0
        && let Some(out) = out_slice(buffer, len)
    {
        let n = message.len().min(len - 1);
        for (o, &b) in out.iter_mut().zip(&message.as_bytes()[..n]) {
            *o = b as c_char;
        }
        out[n] = 0;
    }
    message.len()
}
