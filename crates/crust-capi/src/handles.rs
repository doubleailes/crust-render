//! The opaque handle types behind the C pointers — including the one piece
//! of genuinely delicate `unsafe` in this crate, `RendererHandle`'s pinned
//! self-reference.

use crate::status::{CrustStatus, CrustStepStatus};
use crate::validate::CResult;
use crust_core::{
    AovBuffers, AovRequest, Buffer, Camera, LightList, ProgressiveRender, RenderSettings,
    Renderer, StepStatus, StopToken, WorldBuilder,
};
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

/// The C `CrustStopToken`: a boxed clone-able engine token. The one type
/// whose functions are callable from any thread — `StopToken` is an
/// `Arc<AtomicBool>` underneath, which is exactly what makes that sound.
pub struct TokenHandle(pub(crate) StopToken);

/// The C `CrustScene`: everything accumulated before commit. `builder`
/// turning `None` is the "spent" state the header describes — every
/// add/set call answers `CRUST_ERROR_BAD_STATE` from then on.
pub struct SceneHandle {
    pub(crate) builder: Option<WorldBuilder>,
    pub(crate) lights: LightList,
    pub(crate) camera: Option<Camera>,
    pub(crate) settings: Option<RenderSettings>,
}

impl SceneHandle {
    pub(crate) fn new() -> Self {
        SceneHandle {
            builder: Some(WorldBuilder::new()),
            lights: LightList::new(),
            camera: None,
            settings: None,
        }
    }

    /// The builder, or `BadState` once the scene has been committed.
    pub(crate) fn builder_mut(&mut self) -> CResult<&mut WorldBuilder> {
        self.builder.as_mut().ok_or(CrustStatus::BadState)
    }

    /// Spent check for the non-geometry setters (camera/settings/lights).
    pub(crate) fn ensure_live(&self) -> CResult<()> {
        if self.builder.is_some() {
            Ok(())
        } else {
            Err(CrustStatus::BadState)
        }
    }
}

/// The C `CrustRenderer`: a committed render — the `Renderer` and the
/// `ProgressiveRender` session advancing it — behind one opaque pointer.
///
/// `ProgressiveRender<'a>` borrows `&'a Renderer`, which C's "one handle"
/// shape cannot express safely, so this struct is self-referential.
///
/// SAFETY ARGUMENT (the whole of it):
/// - The `Renderer` lives at a stable heap address: it is boxed once in
///   [`RendererHandle::new`] and freed only in `Drop`. The session's
///   `&'static Renderer` is a lie about *lifetime*, never about *address*.
/// - No `&mut Renderer` is ever created while the session exists: this
///   crate's C surface exposes no post-commit mutation (rebuild-everything
///   semantics — an edit means a new scene handle and a new commit), and
///   internally only shared `renderer()` references are taken.
///   `ProgressiveRender::step` mutates the *session*, not the renderer.
/// - `renderer` is a raw `NonNull`, not a `Box` field: materializing
///   `&mut RendererHandle` from the C pointer at every call would retag a
///   `Box`'s unique pointer and invalidate the session's outstanding
///   borrow under Stacked Borrows; raw pointers are never retagged.
/// - Drop order is explicit: `Drop::drop` first drops the session (which
///   merely releases its film — dropping a `&Renderer` field dereferences
///   nothing), then frees the `Renderer`. `ManuallyDrop` makes that
///   ordering independent of field order, and nothing can observe the
///   handle between the two.
pub struct RendererHandle {
    session: ManuallyDrop<ProgressiveRender<'static>>,
    renderer: NonNull<Renderer>,
    stop: StopToken,
    /// Reused `snapshot_into` target so per-step color reads do not
    /// allocate a frame each time.
    color_scratch: Buffer,
    /// The four AOV planes, probed lazily on the first read (the committed
    /// scene is immutable, so once is enough) and cached.
    aovs: Option<AovBuffers>,
}

impl RendererHandle {
    pub(crate) fn new(renderer: Renderer, stop: StopToken) -> Box<RendererHandle> {
        let (width, height) = renderer.settings.get_dimensions();
        let renderer = NonNull::from(Box::leak(Box::new(renderer)));
        // SAFETY: see the struct-level argument — stable address (freed
        // only in Drop, after the session), and no exclusive reference to
        // the Renderer is ever created while the session lives.
        let session: ProgressiveRender<'static> =
            unsafe { renderer.as_ref() }.begin_progressive(true);
        Box::new(RendererHandle {
            session: ManuallyDrop::new(session),
            renderer,
            stop,
            color_scratch: Buffer::new(width, height),
            aovs: None,
        })
    }

    fn renderer(&self) -> &Renderer {
        // SAFETY: the pointee is alive for the whole life of the handle
        // (freed only in Drop) and only ever shared-borrowed.
        unsafe { self.renderer.as_ref() }
    }

    pub(crate) fn dimensions(&self) -> (u32, u32) {
        let (w, h) = self.renderer().settings.get_dimensions();
        (w as u32, h as u32)
    }

    pub(crate) fn pixel_count(&self) -> usize {
        let (w, h) = self.renderer().settings.get_dimensions();
        w * h
    }

    pub(crate) fn step(&mut self, spp: u32) -> (CrustStepStatus, u32) {
        match self.session.step(spp, None, Some(&self.stop)) {
            StepStatus::InProgress { spp_done } => (CrustStepStatus::InProgress, spp_done),
            StepStatus::Stopped { spp_done } => (CrustStepStatus::Stopped, spp_done),
            StepStatus::Complete { spp_done } => (CrustStepStatus::Complete, spp_done),
        }
    }

    pub(crate) fn is_converged(&self) -> bool {
        self.session.is_complete()
    }

    pub(crate) fn spp_done(&self) -> u32 {
        self.session.spp_done()
    }

    fn ensure_aovs(&mut self) -> &AovBuffers {
        if self.aovs.is_none() {
            self.aovs = Some(self.renderer().render_aovs(AovRequest {
                depth: true,
                normal: true,
                prim_id: true,
                alpha: true,
            }));
        }
        self.aovs.as_ref().expect("just filled")
    }

    /// Linear RGBA into `out` (4 floats per pixel, bottom-up rows). Alpha
    /// comes from the primary-hit coverage plane. Per-pixel copies on
    /// purpose: `Vec3A` is 16 bytes with an undefined fourth lane, so the
    /// accumulators must never be memcpy'd as float data.
    pub(crate) fn read_color(&mut self, out: &mut [f32]) {
        self.ensure_aovs();
        self.session.snapshot_into(&mut self.color_scratch);
        let alpha = self
            .aovs
            .as_ref()
            .and_then(|a| a.alpha.as_deref())
            .expect("alpha requested at probe time");
        for (i, (px, a)) in self
            .color_scratch
            .as_slice()
            .iter()
            .zip(alpha)
            .enumerate()
        {
            out[4 * i] = px.x;
            out[4 * i + 1] = px.y;
            out[4 * i + 2] = px.z;
            out[4 * i + 3] = *a;
        }
    }

    pub(crate) fn read_depth(&mut self, out: &mut [f32]) {
        let depth = self.ensure_aovs().depth.as_deref().expect("requested");
        out.copy_from_slice(depth);
    }

    pub(crate) fn read_normal(&mut self, out: &mut [f32]) {
        let normals = self.ensure_aovs().normal.as_deref().expect("requested");
        for (i, n) in normals.iter().enumerate() {
            out[3 * i] = n.x;
            out[3 * i + 1] = n.y;
            out[3 * i + 2] = n.z;
        }
    }

    pub(crate) fn read_id(&mut self, out: &mut [u32]) {
        let ids = self.ensure_aovs().prim_id.as_deref().expect("requested");
        for (i, [geom, prim]) in ids.iter().enumerate() {
            out[2 * i] = *geom;
            out[2 * i + 1] = *prim;
        }
    }

    pub(crate) fn read_alpha(&mut self, out: &mut [f32]) {
        let alpha = self.ensure_aovs().alpha.as_deref().expect("requested");
        out.copy_from_slice(alpha);
    }
}

impl Drop for RendererHandle {
    fn drop(&mut self) {
        // SAFETY: the session (which borrows the renderer) is dropped
        // strictly before the renderer's box is reclaimed, and neither is
        // touched again — `self` is being destroyed.
        unsafe {
            ManuallyDrop::drop(&mut self.session);
            drop(Box::from_raw(self.renderer.as_ptr()));
        }
    }
}
