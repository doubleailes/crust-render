//! Evaluation time: the USD time code every attribute read resolves at.

use std::marker::PhantomData;

use openusd::usd::TimeCode;

thread_local! {
    /// The USD time code every attribute read in this import resolves at —
    /// `None` for the attribute's *default* value, which is what the importer
    /// read before a frame could be asked for.
    ///
    /// A thread-local rather than a parameter because the answer is the
    /// same for every one of the ~40 read sites and several of them sit in
    /// helpers that are handed only a `Prim` or an `Attribute`; threading a
    /// time through all of them would change every signature in this module
    /// to carry a value none of them decide. It is sound only because every
    /// attribute read happens on the importing thread — the one piece of
    /// rayon work, `MeshArena::commit_slots`, builds BVHs from arrays already
    /// read and never touches the stage — and it is scoped by
    /// [`EvalTimeScope`], so a second import on the same thread (the tests)
    /// never sees a stale time. A change that reads attributes from a rayon
    /// task must thread the time explicitly instead.
    static EVAL_TIME: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// Sets [`EVAL_TIME`] for the lifetime of one `load_scene` call and restores
/// whatever was there before, including on an early `?` return.
///
/// `!Send`: the guard restores a thread-local, so dropping it on any other
/// thread than the one that entered it would restore the wrong thread's time.
/// The marker makes moving it into a rayon task a compile error.
#[must_use = "the evaluation time is restored when the scope is dropped"]
pub(super) struct EvalTimeScope(Option<f64>, PhantomData<*const ()>);

impl EvalTimeScope {
    pub(super) fn enter(time: Option<f64>) -> Self {
        EvalTimeScope(EVAL_TIME.with(|t| t.replace(time)), PhantomData)
    }
}

impl Drop for EvalTimeScope {
    fn drop(&mut self) {
        EVAL_TIME.with(|t| t.set(self.0));
    }
}

/// The time an attribute read resolves at: `None` reads the default value
/// (`Attribute::get`), `Some` resolves time samples at that code, falling
/// back to the default when the attribute has none — so a static attribute
/// reads the same either way and only animated ones move.
pub(super) fn eval_time() -> Option<TimeCode> {
    EVAL_TIME.with(|t| t.get()).map(TimeCode::new)
}

/// The time openusd's own xformable composition is asked for. That API has
/// no "default" arm and was always called at 0.0, so without a frame this
/// keeps exactly that.
pub(super) fn xform_time() -> TimeCode {
    eval_time().unwrap_or(TimeCode::new(0.0))
}
