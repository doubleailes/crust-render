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

// ---------------------------------------------------------------------------
// UV textures
// ---------------------------------------------------------------------------

/// How a texture file's stored values relate to linear radiometric ones.
///
/// Carried across the [`crate::AssetLoader`] seam rather than decided inside
/// it, because the file itself does not say: an 8-bit PNG holding albedo is
/// display-encoded while the *same encoding* holding a normal map, a roughness
/// or a mask is raw data, and un-gamma'ing the latter would bend every value
/// toward zero. MaterialX states it per input (`colorspace="srgb_texture"`),
/// which is where crust reads it from — see `docs/color_management.md`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ColorSpace {
    /// sRGB display-encoded; the host applies the piecewise inverse EOTF at
    /// load.
    Srgb,
    /// A pure power law of 2.2 — MaterialX's `g22_rec709`, which is *not* the
    /// sRGB curve (see [`ColorSpace::from_mtlx`]).
    Gamma22,
    /// A pure power law of 1.8 — MaterialX's `g18_rec709`, the legacy Apple
    /// display gamma.
    Gamma18,
    /// Already linear, or not a colour at all (normals, roughness, masks).
    Raw,
}

impl ColorSpace {
    /// Maps a MaterialX `colorspace` attribute onto a decode.
    ///
    /// `srgb_texture` (and the older `sRGB`-family spellings) mean
    /// display-encoded, and **anything else, including an absent attribute,
    /// means raw**. That default is the correct one and not merely
    /// convenient — normal, roughness, ORM and mask maps carry no colour, and
    /// the DPEL assets mark only their albedos. One function so the importer
    /// and the probe examples cannot drift on which spellings they accept.
    ///
    /// `g22_rec709` and `g18_rec709` are *not* spellings of sRGB. MaterialX
    /// names them separately because they are separate transfer functions:
    /// both are pure power laws with no linear toe, where sRGB's EOTF is
    /// piecewise. Decoding either through the sRGB curve is invisible in
    /// midtones and up to an order of magnitude too bright in near-black —
    /// exactly the error `docs/color_management.md` tabulates — and for
    /// gamma 1.8 the whole curve is wrong, not just the toe. The primaries in
    /// those names are Rec.709, which is what crust works in already, so only
    /// the curve differs.
    pub fn from_mtlx(name: Option<&str>) -> ColorSpace {
        match name.map(str::to_ascii_lowercase).as_deref() {
            Some("srgb_texture" | "srgb" | "srgb_tx") => ColorSpace::Srgb,
            Some("g22_rec709") => ColorSpace::Gamma22,
            Some("g18_rec709") => ColorSpace::Gamma18,
            _ => ColorSpace::Raw,
        }
    }

    /// The exponent of this space's power law, for the two spaces that are
    /// one. `None` for the piecewise sRGB curve and for raw data, neither of
    /// which is a plain `powf`.
    pub fn gamma(self) -> Option<f32> {
        match self {
            ColorSpace::Gamma22 => Some(2.2),
            ColorSpace::Gamma18 => Some(1.8),
            ColorSpace::Srgb | ColorSpace::Raw => None,
        }
    }
}

/// The UV-addressed texture sampler, and its shareable handle.
///
/// Defined by the `crust-mtlx` crate rather than here, for the same reason
/// `Geometry` is defined by `crust-rt`: the standalone library has to *name*
/// the thing it consumes, and crust-core adopts that name as its own UV
/// sampler interface. A separate crust-core trait with an adapter would put a
/// second vtable hop on every texel fetch that fat LTO cannot remove — and a
/// blanket `impl` bridging the two is forbidden by the orphan rule anyway.
/// `Texture2D` is the crust-side name; it is the same trait.
pub use crust_mtlx::{Texture as Texture2D, TextureRef};

#[cfg(test)]
mod color_space_tests {
    use super::ColorSpace;

    #[test]
    fn materialx_gamma_tags_are_not_srgb() {
        // The bug this pins: both tags used to land on `Srgb` and decode
        // through the piecewise curve.
        assert_eq!(ColorSpace::from_mtlx(Some("g22_rec709")), ColorSpace::Gamma22);
        assert_eq!(ColorSpace::from_mtlx(Some("g18_rec709")), ColorSpace::Gamma18);
        assert_eq!(ColorSpace::from_mtlx(Some("G22_Rec709")), ColorSpace::Gamma22);
    }

    #[test]
    fn srgb_spellings_still_mean_srgb_and_everything_else_is_raw() {
        for s in ["srgb_texture", "sRGB", "srgb_tx"] {
            assert_eq!(ColorSpace::from_mtlx(Some(s)), ColorSpace::Srgb, "{s}");
        }
        // Absent, linear, and a space whose *primaries* crust does not
        // convert (ACEScg / AP1) all stay raw rather than guessing.
        for s in [None, Some("lin_rec709"), Some("acescg"), Some("g22_ap1")] {
            assert_eq!(ColorSpace::from_mtlx(s), ColorSpace::Raw, "{s:?}");
        }
    }

    #[test]
    fn only_the_power_law_spaces_report_a_gamma() {
        assert_eq!(ColorSpace::Gamma22.gamma(), Some(2.2));
        assert_eq!(ColorSpace::Gamma18.gamma(), Some(1.8));
        assert_eq!(ColorSpace::Srgb.gamma(), None);
        assert_eq!(ColorSpace::Raw.gamma(), None);
    }
}
