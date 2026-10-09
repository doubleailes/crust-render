//! A render's control: what a host holds to watch a render improve and to
//! stop it ([`RenderControl`]), and how the render ended ([`RenderOutcome`]).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::buffer::Buffer;

use super::PixelRect;

/// The handle a host passes to
/// [`Renderer::render_with_control`](crate::Renderer::render_with_control)
/// to watch the render and to stop it.
///
/// The contract:
///
/// - **One per render.** A control serves the one render call it is given
///   to. Cancelling it never reaches another render, and a new render (a
///   restart) takes a fresh control.
/// - **Cancel is sticky.** [`cancel`](Self::cancel) can be called from any
///   thread, at any time, any number of times. Once set it stays set: a
///   render given a cancelled control traces nothing.
/// - **Snapshots.** As each work unit finishes a stage of the first sweep or
///   an adaptive round, the render publishes that unit's current per-pixel
///   estimates into the control's region-sized beauty image.
///   [`snapshot`](Self::snapshot) is `None` until the first publish; after
///   it, a copy of the whole image. A pixel not yet sampled reads as zero;
///   during a guided render, a pixel shows the pass in progress where its
///   unit has published and the previous pass where it has not.
/// - **Generation.** [`generation`](Self::generation) counts the publishes:
///   it is monotonic, starts at 0 and is bumped with every publish, so a
///   reader that saw the same value twice has nothing new to read. The value
///   [`snapshot`](Self::snapshot) returns is the one its image belongs to.
///
/// A control is `Sync`, as is the `Renderer`: run the render on one thread
/// and read or cancel from another, borrowing both (`std::thread::scope`) or
/// sharing them through `Arc`s. Only the beauty is published; the AOVs are
/// gathered once, when the render returns. A host that only ever cancels
/// takes [`without_snapshots`](Self::without_snapshots), and its render
/// pays nothing for publishing.
pub struct RenderControl {
    cancel: AtomicBool,
    /// Whether the render publishes into this control at all.
    snapshots: bool,
    generation: AtomicU64,
    /// The latest published beauty, region-sized; `None` before the first
    /// publish. Workers publish under the lock, readers clone under it.
    display: Mutex<Option<Buffer>>,
}

impl Default for RenderControl {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderControl {
    /// A control that is not cancelled and has published nothing.
    pub fn new() -> Self {
        RenderControl {
            cancel: AtomicBool::new(false),
            snapshots: true,
            generation: AtomicU64::new(0),
            display: Mutex::new(None),
        }
    }

    /// A control that only cancels: the render publishes nothing into it, so
    /// [`snapshot`](Self::snapshot) stays `None` and
    /// [`generation`](Self::generation) 0. What a host that never shows the
    /// render in progress takes — the CLI without `--checkpoint` — since each
    /// publish copies a unit's estimates under the lock.
    pub fn without_snapshots() -> Self {
        RenderControl {
            snapshots: false,
            ..Self::new()
        }
    }

    /// Whether the render publishes into this control.
    pub(crate) fn takes_snapshots(&self) -> bool {
        self.snapshots
    }

    /// Asks the render to stop. Workers check before each pixel's next batch
    /// of samples, so the render returns once the advances already in flight
    /// finish, with what it traced.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether [`cancel`](Self::cancel) has been called.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// How many times the render has published: 0 before the first.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// A copy of the latest published beauty, with the generation it
    /// belongs to; `None` before the first publish. The image covers the
    /// render's region, as the [`Buffer`] the render returns does.
    pub fn snapshot(&self) -> Option<(u64, Buffer)> {
        let display = self.display.lock().unwrap_or_else(|e| e.into_inner());
        // Read under the lock the publishes bump it under, so the image and
        // its generation agree.
        let generation = self.generation.load(Ordering::Acquire);
        display.as_ref().map(|b| (generation, b.clone()))
    }

    /// Writes one unit's pixels into the display image through `write` and
    /// bumps the generation, under the one lock. The image is created black
    /// on the first publish, sized to `rect` of a `width` × `height` frame
    /// (raster space).
    pub(crate) fn publish(
        &self,
        width: usize,
        height: usize,
        rect: PixelRect,
        write: impl FnOnce(&mut Buffer),
    ) {
        let mut display = self.display.lock().unwrap_or_else(|e| e.into_inner());
        write(display.get_or_insert_with(|| Buffer::for_raster_rect(width, height, rect)));
        self.generation.fetch_add(1, Ordering::Release);
    }
}

/// How a render ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderOutcome {
    /// Every pixel took the samples the settings asked for (or stopped
    /// early by the adaptive rule).
    Completed,
    /// The control was cancelled: the image, the AOVs and the counters are
    /// those of the samples traced before it stopped.
    Cancelled,
}
