//! The C ABI as hdCrust calls it: every entry point through its C signature
//! (a function pointer of the header's type, not the Rust item), and the
//! header's version against the library's.
#![allow(unsafe_code)] // Calling the ABI is the test.

use crate::ffi::{self, CrustHydraSession, LogFn, Status};
use crate::tests::{own_settings, sample, session_dir, settings, write_edits};
use std::ffi::{CString, c_char, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// The header's prototypes, as Rust function pointer types.
type AbiVersion = extern "C" fn() -> u32;
type New =
    unsafe extern "C" fn(*const c_char, Option<LogFn>, *mut c_void) -> *mut CrustHydraSession;
type Free = unsafe extern "C" fn(*mut CrustHydraSession);
type Load = unsafe extern "C" fn(*mut CrustHydraSession, *const c_char) -> Status;
type Apply = unsafe extern "C" fn(*mut CrustHydraSession) -> Status;
type SnapshotFn = unsafe extern "C" fn(
    *mut CrustHydraSession,
    *mut f32,
    usize,
    usize,
    u64,
    *mut u64,
    *mut u32,
) -> Status;
type Converged = unsafe extern "C" fn(*mut CrustHydraSession) -> i32;
type Applied = unsafe extern "C" fn(*mut CrustHydraSession) -> i32;
type AovFn = unsafe extern "C" fn(
    *mut CrustHydraSession,
    *const c_char,
    *mut f32,
    usize,
    usize,
    usize,
) -> Status;
type Cancel = unsafe extern "C" fn(*mut CrustHydraSession);
type LastError = unsafe extern "C" fn(*mut CrustHydraSession, *mut c_char, usize) -> usize;

const ABI_VERSION: AbiVersion = ffi::crust_hydra_abi_version;
const NEW: New = ffi::crust_hydra_new;
const FREE: Free = ffi::crust_hydra_free;
const LOAD: Load = ffi::crust_hydra_load;
const APPLY: Apply = ffi::crust_hydra_apply;
const SNAPSHOT: SnapshotFn = ffi::crust_hydra_snapshot;
const CONVERGED: Converged = ffi::crust_hydra_converged;
const APPLIED: Applied = ffi::crust_hydra_applied;
const AOV: AovFn = ffi::crust_hydra_aov;
const CANCEL: Cancel = ffi::crust_hydra_cancel;
const LAST_ERROR: LastError = ffi::crust_hydra_last_error;

fn c(s: &str) -> CString {
    CString::new(s).expect("no NUL")
}

fn path(p: &Path) -> CString {
    c(p.to_str().expect("UTF-8"))
}

/// `crust_hydra_last_error` into a fresh buffer.
fn last_error(session: *mut CrustHydraSession) -> String {
    let len = unsafe { LAST_ERROR(session, std::ptr::null_mut(), 0) };
    let mut buffer = vec![0 as c_char; len + 1];
    let again = unsafe { LAST_ERROR(session, buffer.as_mut_ptr(), buffer.len()) };
    assert_eq!(again, len);
    let bytes: Vec<u8> = buffer[..len].iter().map(|&b| b as u8).collect();
    String::from_utf8(bytes).expect("UTF-8")
}

#[test]
fn the_header_declares_the_library_version() {
    let header = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("include/crust_hydra.h"),
    )
    .expect("the header");
    let declared: u32 = header
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define CRUST_HYDRA_ABI_VERSION "))
        .expect("the header defines CRUST_HYDRA_ABI_VERSION")
        .trim()
        .parse()
        .expect("a number");
    assert_eq!(ABI_VERSION(), declared);
    // The statuses, as the header numbers them.
    for (name, status) in [
        ("CRUST_HYDRA_OK = 0", Status::Ok),
        ("CRUST_HYDRA_BUSY = 1", Status::Busy),
        ("CRUST_HYDRA_IMPORT_FAILED = 2", Status::ImportFailed),
        ("CRUST_HYDRA_PANICKED = 3", Status::Panicked),
        ("CRUST_HYDRA_INVALID_ARGUMENT = 4", Status::InvalidArgument),
    ] {
        assert!(header.contains(name), "the header lacks {name}");
        assert_eq!(
            name.rsplit(' ').next(),
            Some((status as i32).to_string().as_str())
        );
    }
}

/// What the log callback received.
static LOGGED: Mutex<Vec<(i32, String)>> = Mutex::new(Vec::new());

extern "C" fn record(level: i32, message: *const c_char, user: *mut c_void) {
    assert_eq!(user as usize, 0xC0FFEE, "the user pointer comes back");
    let text = unsafe { std::ffi::CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned();
    LOGGED.lock().unwrap().push((level, text));
}

#[test]
fn every_entry_point_through_its_c_signature() {
    // A null handle is refused everywhere, never dereferenced.
    let null = std::ptr::null_mut();
    unsafe {
        assert_eq!(APPLY(null), Status::InvalidArgument);
        assert_eq!(CONVERGED(null), 0);
        assert_eq!(APPLIED(null), -1);
        assert_eq!(LAST_ERROR(null, std::ptr::null_mut(), 0), 0);
        CANCEL(null);
        FREE(null);
    }
    // A directory that cannot be created: null, and the reason logged.
    let dir = session_dir("ffi");
    let blocker = dir.join("file");
    std::fs::write(&blocker, b"").unwrap();
    let refused = unsafe {
        NEW(
            path(&blocker.join("sub")).as_ptr(),
            Some(record),
            0xC0FFEE as *mut c_void,
        )
    };
    assert!(refused.is_null());
    assert!(LOGGED.lock().unwrap().iter().any(|(level, _)| *level == 0));

    let session = unsafe { NEW(path(&dir).as_ptr(), Some(record), 0xC0FFEE as *mut c_void) };
    assert!(!session.is_null());
    unsafe {
        // Nothing loaded yet.
        assert_eq!(APPLY(session), Status::InvalidArgument);
        assert!(last_error(session).contains("no layer"));
        // A layer that is not there.
        assert_eq!(
            LOAD(session, path(&dir.join("missing.usda")).as_ptr()),
            Status::ImportFailed
        );
        assert!(!last_error(session).is_empty());
        // Truncation, as snprintf: the full length back, a NUL in the buffer.
        let mut small = [1 as c_char; 4];
        let full = LAST_ERROR(session, small.as_mut_ptr(), small.len());
        assert!(full > 3);
        assert_eq!(small[3], 0);
    }

    let layer = write_edits(&dir, &sample("aovs.usda"), &settings(4));
    let (w, h) = (32, 18);
    let mut rgba = vec![0.0f32; w * h * 4];
    let (mut generation, mut spp) = (0u64, 0u32);
    unsafe {
        assert_eq!(APPLIED(session), -1);
        assert_eq!(LOAD(session, path(&layer).as_ptr()), Status::Ok);
        assert_eq!(APPLIED(session), 3, "a load is a full import");
        let started = Instant::now();
        while CONVERGED(session) == 0 {
            assert!(started.elapsed() < Duration::from_secs(300));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            SNAPSHOT(
                session,
                rgba.as_mut_ptr(),
                w,
                h,
                0,
                &mut generation,
                &mut spp
            ),
            Status::Ok
        );
        assert!(generation > 0);
        assert!(rgba.iter().any(|&v| v > 0.0));
        // The same generation again: nothing copied, `since` written back.
        rgba.fill(-1.0);
        let mut again = 0;
        assert_eq!(
            SNAPSHOT(
                session,
                rgba.as_mut_ptr(),
                w,
                h,
                generation,
                &mut again,
                std::ptr::null_mut()
            ),
            Status::Ok
        );
        assert_eq!(again, generation);
        assert!(rgba.iter().all(|&v| v == -1.0));
        // A buffer of another size: not yet.
        let mut other = vec![0.0f32; 8 * 8 * 4];
        assert_eq!(
            SNAPSHOT(
                session,
                other.as_mut_ptr(),
                8,
                8,
                0,
                &mut again,
                std::ptr::null_mut()
            ),
            Status::Busy
        );
        // The depth AOV, then a wrong shape and an unknown name.
        let mut depth = vec![0.0f32; w * h];
        assert_eq!(
            AOV(session, c("Z").as_ptr(), depth.as_mut_ptr(), w, h, 1),
            Status::Ok
        );
        assert!(depth.iter().any(|d| d.is_finite() && *d > 0.0));
        assert_eq!(
            AOV(session, c("Z").as_ptr(), depth.as_mut_ptr(), w, h, 3),
            Status::InvalidArgument
        );
        assert_eq!(
            AOV(session, c("nope").as_ptr(), depth.as_mut_ptr(), w, h, 1),
            Status::InvalidArgument
        );
        // A change applied, then cancelled: the image stays, no convergence.
        write_edits(&dir, &sample("aovs.usda"), &settings(1 << 16));
        assert_eq!(APPLY(session), Status::Ok);
        CANCEL(session);
        assert_eq!(CONVERGED(session), 0);
        FREE(session);
    }
}

/// A callback that keeps what it receives in the `Mutex` its `user` points
/// to, so each test reads only its own session's lines.
extern "C" fn keep(level: i32, message: *const c_char, user: *mut c_void) {
    let lines = unsafe { &*(user as *const Mutex<Vec<(i32, String)>>) };
    let text = unsafe { std::ffi::CStr::from_ptr(message) }
        .to_string_lossy()
        .into_owned();
    lines.lock().unwrap().push((level, text));
}

/// A session whose log lands in the returned lines.
fn logged_session(dir: &Path) -> (*mut CrustHydraSession, &'static Mutex<Vec<(i32, String)>>) {
    let lines: &'static Mutex<Vec<(i32, String)>> = Box::leak(Box::new(Mutex::new(Vec::new())));
    let user = lines as *const _ as *mut c_void;
    let session = unsafe { NEW(path(dir).as_ptr(), Some(keep), user) };
    assert!(!session.is_null());
    (session, lines)
}

/// A loader that decodes everything as crust does, but whose UV textures
/// panic when a material samples them, while `armed`: a material's bug, met
/// inside the render on a rayon worker. (`Texture::eval` must not panic;
/// this one breaks that contract on purpose.)
struct PanickingTextures {
    files: crust_assets::FileAssets,
    armed: Arc<AtomicBool>,
}

struct PanickingTexture(Arc<AtomicBool>);

impl crust_core::Texture2D for PanickingTexture {
    fn eval(&self, _u: f32, _v: f32, _width: f32) -> [f32; 4] {
        if self.0.load(Ordering::SeqCst) {
            panic!("a test texture panicked");
        }
        [0.5, 0.5, 0.5, 1.0]
    }
}

impl crust_core::AssetLoader for PanickingTextures {
    fn load_environment(
        &self,
        path: &Path,
        space: crust_core::ColorSpace,
    ) -> Option<crust_core::EnvironmentMap> {
        self.files.load_environment(path, space)
    }

    fn load_texture(
        &self,
        _path: &Path,
        _space: crust_core::ColorSpace,
    ) -> Option<Arc<dyn crust_core::Texture2D>> {
        Some(Arc::new(PanickingTexture(self.armed.clone())))
    }

    fn load_ptex(
        &self,
        path: &Path,
        space: crust_core::ColorSpace,
    ) -> Option<Arc<dyn crust_core::PtexTexture>> {
        self.files.load_ptex(path, space)
    }

    fn load_light_texture(
        &self,
        path: &Path,
        space: crust_core::ColorSpace,
    ) -> Option<Arc<crust_core::LightTexture>> {
        self.files.load_light_texture(path, space)
    }

    fn load_ies(&self, path: &Path) -> Option<Arc<crust_core::IesProfile>> {
        self.files.load_ies(path)
    }
}

impl crust_render::Assets for PanickingTextures {
    fn preloaded(&self) -> u64 {
        0
    }
}

#[test]
fn a_panicking_material_during_a_render_is_an_error_and_the_next_apply_renders() {
    let dir = session_dir("ffi_panic");
    // Four quads whose materials sample UV textures.
    let layer = write_edits(&dir, &sample("usdpreview_textured.usda"), &own_settings(4));
    let armed = Arc::new(AtomicBool::new(true));
    let loader = PanickingTextures {
        files: crust_assets::FileAssets::new(),
        armed: armed.clone(),
    };
    let lines: &'static Mutex<Vec<(i32, String)>> = Box::leak(Box::new(Mutex::new(Vec::new())));
    let session = crate::ffi::handle(
        crate::Session::with_assets(&dir, Arc::new(loader)).expect("a session"),
        Some(keep),
        lines as *const _ as *mut c_void,
    );
    let (w, h) = (32, 32);
    let mut rgba = vec![0.0f32; w * h * 4];
    let mut generation = 0;
    unsafe {
        assert_eq!(LOAD(session, path(&layer).as_ptr()), Status::Ok);
        let started = Instant::now();
        let status = loop {
            let s = SNAPSHOT(
                session,
                rgba.as_mut_ptr(),
                w,
                h,
                0,
                &mut generation,
                std::ptr::null_mut(),
            );
            if s == Status::Panicked {
                break s;
            }
            assert!(
                started.elapsed() < Duration::from_secs(120),
                "no panic reported: {s:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status, Status::Panicked);
        assert!(last_error(session).contains("a test texture panicked"));
        // Reported once: the host raises an error per failed poll and polls
        // on every redraw. The stopped render counts as converged, so the
        // host stops redrawing for it.
        assert_eq!(
            SNAPSHOT(
                session,
                rgba.as_mut_ptr(),
                w,
                h,
                0,
                &mut generation,
                std::ptr::null_mut()
            ),
            Status::Busy
        );
        assert_eq!(CONVERGED(session), 1);
        // The panic hook sent the panic to the host, not to stderr.
        assert!(
            lines
                .lock()
                .unwrap()
                .iter()
                .any(|(level, m)| *level == 0 && m.contains("a test texture panicked")),
            "{:?}",
            lines.lock().unwrap()
        );
        // The material fixed: the next change imports the layer again and
        // renders it to completion.
        armed.store(false, Ordering::SeqCst);
        assert_eq!(APPLY(session), Status::Ok);
        assert_eq!(
            (*session).session.applied(),
            Some(crust_render::session::delta::Applied::Full)
        );
        let started = Instant::now();
        while CONVERGED(session) == 0 {
            assert!(started.elapsed() < Duration::from_secs(300));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            SNAPSHOT(
                session,
                rgba.as_mut_ptr(),
                w,
                h,
                0,
                &mut generation,
                std::ptr::null_mut()
            ),
            Status::Ok
        );
        FREE(session);
    }
}

#[test]
fn an_import_warning_reaches_the_host_once() {
    let dir = session_dir("ffi_warning");
    let layer = write_edits(&dir, &sample("aovs.usda"), &settings(4));
    let (session, lines) = logged_session(&dir);
    unsafe {
        assert_eq!(LOAD(session, path(&layer).as_ptr()), Status::Ok);
        let started = Instant::now();
        while CONVERGED(session) == 0 {
            assert!(started.elapsed() < Duration::from_secs(300));
            std::thread::sleep(Duration::from_millis(5));
        }
        FREE(session);
    }
    let lines = lines.lock().unwrap();
    let ours: Vec<_> = lines
        .iter()
        .filter(|(_, m)| m.contains("[aov.unknown_source]"))
        .collect();
    assert_eq!(ours.len(), 1, "{lines:?}");
    let (level, message) = ours[0];
    assert_eq!(*level, 1);
    // With its prim.
    assert!(message.contains(" (at /Render/"), "{message}");
}
