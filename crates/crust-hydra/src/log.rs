//! What crust says, forwarded to the host's log callback (design D7, D8):
//! `tracing` events at `WARN` and above (lower when the host asks), the
//! import's coded warnings once per import, and panics instead of stderr.
//!
//! Both a `tracing` subscriber and a panic hook are process-wide, so each is
//! installed once, the first time a session asks for a callback, and routes
//! to the callbacks registered here: an event or a panic on a session's own
//! thread (its session thread or its render thread) goes to that session's
//! callback; one on another thread (a rayon worker) goes to all of them.
//!
//! The import's coded warnings reach the host from the scene's report, with
//! their code and first prim, once per import ([`forward_warnings`]); the
//! `[code] message` lines `crust_core::warning!` also logs, per occurrence,
//! are not forwarded, so the host sees each once.

use std::cell::Cell;
use std::ffi::{CString, c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber, span};

/// `crust_hydra_log_fn`.
pub type LogFn = extern "C" fn(level: i32, message: *const c_char, user: *mut c_void);

/// The header's levels: 0 error, 1 warning, 2 info, 3 debug.
pub const ERROR: i32 = 0;
pub const WARNING: i32 = 1;

/// A registered callback.
#[derive(Clone, Copy)]
struct Sink {
    session: u64,
    log: LogFn,
    /// The host's `user` pointer, as an address: only handed back.
    user: usize,
    /// The most verbose level forwarded.
    max: i32,
}

static SINKS: Mutex<Vec<Sink>> = Mutex::new(Vec::new());
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// The session whose thread this is, on the threads a session spawns.
    static CURRENT: Cell<Option<u64>> = const { Cell::new(None) };
}

/// A new session's id, for [`enter`] and [`register`].
pub fn next_session() -> u64 {
    NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
}

/// Marks the calling thread as session `id`'s for the rest of its life.
pub fn enter(id: u64) {
    CURRENT.with(|c| c.set(Some(id)));
}

/// Forwards session `id`'s output to `log`, at `WARN` and above; installs
/// the subscriber and the panic hook on first use.
pub fn register(id: u64, log: LogFn, user: usize) {
    install();
    let mut sinks = SINKS.lock().unwrap_or_else(|e| e.into_inner());
    sinks.retain(|s| s.session != id);
    sinks.push(Sink {
        session: id,
        log,
        user,
        max: WARNING,
    });
}

/// Forwards session `id`'s output down to `max` (0 error … 3 debug).
pub fn set_level(id: u64, max: i32) {
    let mut sinks = SINKS.lock().unwrap_or_else(|e| e.into_inner());
    for s in sinks.iter_mut().filter(|s| s.session == id) {
        s.max = max.clamp(ERROR, 3);
    }
}

/// Stops forwarding session `id`'s output: before its callback's `user`
/// goes away.
pub fn unregister(id: u64) {
    SINKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|s| s.session != id);
}

/// The sinks a message at `level` from the calling thread goes to.
fn targets(level: i32) -> Vec<Sink> {
    let current = CURRENT.with(Cell::get);
    SINKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|s| level <= s.max && current.is_none_or(|id| s.session == id))
        .copied()
        .collect()
}

/// Calls every sink `targets` names with `message`.
fn send(level: i32, message: &str) {
    let sinks = targets(level);
    if sinks.is_empty() {
        return;
    }
    // A NUL inside a message would cut it short; it never should be there.
    let Ok(text) = CString::new(message.replace('\0', " ")) else {
        return;
    };
    for s in sinks {
        (s.log)(level, text.as_ptr(), s.user as *mut c_void);
    }
}

/// Forwards the import's coded warnings once each, with code and prim, to
/// the session whose thread calls it.
pub fn forward_warnings(warnings: &[crust_core::Warning]) {
    for w in warnings {
        let at = match w.prims.as_slice() {
            [] => String::new(),
            [one] => format!(" (at {one})"),
            [first, rest @ ..] => format!(" (at {first} and {} more)", rest.len()),
        };
        let times = if w.count > 1 {
            format!(", {} times", w.count)
        } else {
            String::new()
        };
        send(
            WARNING,
            &format!("[{}] {}{at}{times}", w.code.as_str(), w.message),
        );
    }
}

/// The header's level of a `tracing` level.
fn level_of(level: &Level) -> i32 {
    match *level {
        Level::ERROR => ERROR,
        Level::WARN => WARNING,
        Level::INFO => 2,
        _ => 3,
    }
}

/// Whether `message` is a coded import warning's log line, `[domain.cause]
/// …`, which [`forward_warnings`] reports instead.
fn is_coded(message: &str) -> bool {
    message
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
        .is_some_and(|(code, _)| {
            crust_core::WarningCode::ALL
                .iter()
                .any(|c| c.as_str() == code)
        })
}

/// The subscriber: events only, spans ignored.
struct Forward;

struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        } else {
            self.0.push_str(&format!(" {}={value:?}", field.name()));
        }
    }
}

impl Subscriber for Forward {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        // Which levels pass depends on the sinks registered now.
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        !targets(level_of(metadata.level())).is_empty()
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut message = Message(String::new());
        event.record(&mut message);
        if !is_coded(&message.0) {
            send(level_of(event.metadata().level()), &message.0);
        }
    }

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}

/// Installs the subscriber, unless the host process already has one, and a
/// panic hook that sends a panic on a session's thread to that session
/// instead of stderr; anywhere else it also runs the hook it replaced.
fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(Forward);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let ours = CURRENT.with(Cell::get).is_some();
            if !ours {
                previous(info);
            }
            let message = crate::ffi::panic_message(info.payload());
            let at = info
                .location()
                .map(|l| format!(" at {}:{}", l.file(), l.line()))
                .unwrap_or_default();
            send(ERROR, &format!("crust panicked{at}: {message}"));
        }));
    });
}
