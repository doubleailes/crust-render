//! The one thing this crate asks of its host: a way to sample an image.
//!
//! An `image` node names a file; this crate never opens it. Decoding, UDIM
//! expansion, resolution policy and colour-space handling are all the host's
//! (a production UDIM set is fourteen 4K images per map, and how many of them
//! to hold is not a MaterialX question). What the compiled program needs is a
//! value at `(u, v)`, and that is the whole trait.

use std::sync::Arc;

/// A UV-addressed texture the host has opened and can sample.
///
/// The compiled [`crate::Program`] holds `Arc<dyn Texture>` handles and calls
/// [`Texture::eval`] from every render thread, so implementations are shared
/// immutably — a host that caches lazily needs its own synchronisation.
pub trait Texture: Send + Sync {
    /// Samples at `(u, v)`, returning linear RGBA.
    ///
    /// `u`/`v` arrive **unwrapped** — `u = 3.4` is the fourth UDIM tile, not
    /// `0.4` of the first — because tile selection is the host's addressing
    /// job. A non-UDIM texture wraps them itself (MaterialX's default address
    /// mode is `periodic`).
    ///
    /// Must not panic: a non-finite coordinate is the caller's bug, but this
    /// is consulted from inside a renderer's inner loop where a panic takes
    /// down a worker thread. Return a fallback instead.
    fn eval(&self, u: f32, v: f32) -> [f32; 4];
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
    pub fn eval(&self, u: f32, v: f32) -> [f32; 4] {
        self.0.eval(u, v)
    }
}

impl std::fmt::Debug for TextureRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Texture")
    }
}
