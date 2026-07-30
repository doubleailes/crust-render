//! Per-face textures, as the engine consumes them.
//!
//! Ptex indexes texels by *face*, not by a shared UV chart: every face of the
//! control cage owns its own little image, at its own resolution. That makes it
//! the one texture kind that cannot be handed across the [`crate::AssetLoader`]
//! seam as a decoded pixel rectangle the way an environment map is — a
//! production `.ptx` is a per-face mip pyramid that can run to gigabytes, and
//! the sensible representations (resolution caps, lazily decoded faces) are
//! decoding concerns. So the seam inverts for Ptex: the host hands back a
//! *sampler*, and crust-core only ever asks it for a value.
//!
//! The trait is deliberately narrower than Ptex proper. It answers one
//! question — what colour is face `f` at `(u, v)` — because that is all a
//! surface shader needs, and it keeps every filtering and file-format decision
//! on the host's side of the seam.

use glam::Vec3A;

/// A per-face texture the host has opened and can sample.
///
/// Implementations live in the host (`crust-render` wraps the `ptex` crate);
/// crust-core only holds `Arc<dyn PtexTexture>` handles on materials. Shared
/// immutably across every render thread, so `eval` takes `&self` — a host that
/// caches lazily needs its own interior synchronisation.
pub trait PtexTexture: Send + Sync {
    /// Samples face `face_id` at `(u, v)`, both in the face's own `[0, 1]`
    /// parametric space.
    ///
    /// Must not panic: an out-of-range `face_id` or a non-finite coordinate is
    /// the caller's bug, but a texture is consulted from inside the integrator
    /// where a panic would take down a render thread. Return a sensible
    /// fallback instead. Values are linear, not display-encoded — undoing any
    /// transfer function baked into the file is the host's job.
    fn eval(&self, face_id: u32, u: f32, v: f32) -> Vec3A;

    /// Number of faces the file holds. The importer compares this against the
    /// bound mesh's face count: Ptex face ids *are* mesh face indices, so a
    /// mismatch means the texture does not belong to the geometry, and every
    /// lookup after that would be silently wrong rather than obviously broken.
    fn num_faces(&self) -> usize;
}

/// A shared handle to a [`PtexTexture`], carryable on a material.
///
/// Exists only so materials can keep deriving `Debug`: a `dyn` trait object
/// has none, and hand-writing `Debug` for a 40-field shader is worse than
/// wrapping the one field that needs it.
#[derive(Clone)]
pub struct PtexRef(pub std::sync::Arc<dyn PtexTexture>);

impl PtexRef {
    /// Samples the texture — see [`PtexTexture::eval`].
    #[inline]
    pub fn eval(&self, face_id: u32, u: f32, v: f32) -> Vec3A {
        self.0.eval(face_id, u, v)
    }
}

impl std::fmt::Debug for PtexRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ptex({} faces)", self.0.num_faces())
    }
}
