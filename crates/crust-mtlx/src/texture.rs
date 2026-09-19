//! The one thing this crate asks of its host: a way to sample an image.
//!
//! An `image` node names a file; this crate never opens it. Decoding, UDIM
//! expansion, resolution policy and colour-space handling are all the host's
//! (a production UDIM set is fourteen 4K images per map, and how many of them
//! to hold is not a MaterialX question). What the compiled program needs is a
//! value at `(u, v)` over a given filter width, and that is the whole trait.

use std::sync::Arc;

/// A UV-addressed texture the host has opened and can sample.
///
/// The compiled [`crate::Program`] holds `Arc<dyn Texture>` handles and calls
/// [`Texture::eval`] from every render thread, so implementations are shared
/// immutably — a host that caches lazily needs its own synchronisation.
pub trait Texture: Send + Sync {
    /// Samples at `(u, v)` over a footprint `width` wide, returning linear
    /// RGBA.
    ///
    /// `u`/`v` arrive **unwrapped** — `u = 3.4` is the fourth UDIM tile, not
    /// `0.4` of the first — because tile selection is the host's addressing
    /// job. A non-UDIM texture wraps them itself (MaterialX's default address
    /// mode is `periodic`).
    ///
    /// `width` is the **diameter** of that footprint in the same UV units, so
    /// one unit is one UDIM tile. It is a request, not a promise: how (or
    /// whether) to filter over it is the host's resolution policy, exactly as
    /// the decode is. **`0.0` means point-sample the finest detail the host
    /// holds**, and is what a caller with no derivatives must pass — a
    /// renderer that tracks no footprint therefore gets the unfiltered
    /// behaviour rather than a wrong one.
    ///
    /// Must not panic: a non-finite coordinate or width is the caller's bug,
    /// but this is consulted from inside a renderer's inner loop where a
    /// panic takes down a worker thread. Return a fallback instead.
    fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4];
}

/// A shared handle to a [`Texture`], carryable inside an [`crate::Op`].
///
/// A newtype rather than a bare `Arc<dyn Texture>` so that `Debug` can be
/// implemented for it: `Op` derives `Debug`, and a trait object has none.
#[derive(Clone)]
pub struct TextureRef(pub Arc<dyn Texture>);

impl TextureRef {
    /// Samples the texture — see [`Texture::eval`].
    #[inline]
    pub fn eval(&self, u: f32, v: f32, width: f32) -> [f32; 4] {
        self.0.eval(u, v, width)
    }
}

impl std::fmt::Debug for TextureRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Texture")
    }
}
