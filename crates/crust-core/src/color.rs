//! Colour management through OpenColorIO.
//!
//! Every transfer curve the renderer applies, and every colour-space name it
//! accepts, comes from one OCIO config: the builtin ACES CG config
//! ([`CONFIG_URI`]), read by [`ocio`], a pure-Rust port of OpenColorIO. crust
//! renders in [`WORKING_SPACE`], so a decode is an OCIO processor from the
//! authored space to `lin_rec709` and an encode is its inverse.
//!
//! Two things are crust's rather than the config's, and are stated here so
//! they are not mistaken for OCIO behaviour:
//!
//! - **Which spaces a texture may be bound in.** [`ResolvedColorSpace`] is the
//!   four whose processor is a per-channel curve on Rec.709 primaries: a
//!   texture is stored as one byte per channel and decoded through a 256-entry
//!   table, which cannot express a gamut conversion. A name with different
//!   primaries (`acescg`, `g22_ap1`, `adobergb`, …) therefore still binds raw
//!   ([`crate::ColorSpace::from_mtlx`]), as before OCIO resolved the names.
//! - **Encoded values below zero decode to zero.** The config's power-law
//!   spaces pass negatives through unchanged (`style: pass_thru`), where crust
//!   has always clamped them: a display-encoded value below black means
//!   nothing, and an albedo must not go negative. Raw data is never clamped,
//!   since a height may be negative.
//!
//! See `docs/color_management.md` for which input is decoded from which space.

use crate::texture::ResolvedColorSpace;
use glam::Vec3A;
use std::sync::OnceLock;

/// The OCIO config every name resolves against and every curve comes from:
/// the builtin ACES CG config, compiled into the binary.
///
/// Named by its full version rather than `ocio://cg-config-latest`, so an
/// `ocio` bump that ships a newer config cannot move a render on its own.
pub const CONFIG_URI: &str = "ocio://cg-config-v4.0.0_aces-v2.0_ocio-v2.5";

/// The space the renderer works in: linear light on Rec.709 primaries.
pub const WORKING_SPACE: &str = "lin_rec709";

/// The display the preview PNG is encoded for.
pub const PREVIEW_DISPLAY: &str = "sRGB - Display";

/// The view the preview PNG is encoded with: the piecewise sRGB curve and
/// nothing else, so the PNG stays a clamp-and-encode of the EXR rather than a
/// grade of it.
pub const PREVIEW_VIEW: &str = "Un-tone-mapped";

/// The config, loaded once.
///
/// It is compiled into the `ocio` crate, so failing to load it is a build
/// defect rather than an input error, and is not reported as one.
pub fn config() -> &'static ocio::Config {
    static CONFIG: OnceLock<ocio::Config> = OnceLock::new();
    CONFIG.get_or_init(|| {
        ocio::Config::create_from_file(CONFIG_URI)
            .unwrap_or_else(|e| panic!("builtin OCIO config {CONFIG_URI}: {e}"))
    })
}

/// A CPU processor for `src` → `dst` in [`config`].
///
/// Panics when either name is missing from the config: every caller passes a
/// name from this module, and a config without them is a build defect.
fn processor(src: &str, dst: &str) -> ocio::CpuProcessor {
    let p = config()
        .get_processor(src, dst)
        .unwrap_or_else(|e| panic!("OCIO processor {src} -> {dst}: {e}"));
    let cpu = p.default_cpu_processor();
    // `decode_slice` / `encode_slice` treat any run of samples as RGB
    // triplets, which is only right for a per-channel curve.
    assert!(
        !cpu.has_channel_crosstalk(),
        "{src} -> {dst} is not a per-channel curve"
    );
    cpu
}

/// The decode and encode processors of one [`ResolvedColorSpace`].
struct Curve {
    decode: ocio::CpuProcessor,
    encode: ocio::CpuProcessor,
}

/// The [`Curve`] of `space`, built on first use and shared after.
fn curve(space: ResolvedColorSpace) -> &'static Curve {
    static CURVES: OnceLock<[Curve; 4]> = OnceLock::new();
    let curves = CURVES.get_or_init(|| {
        ResolvedColorSpace::ALL.map(|s| Curve {
            decode: processor(s.ocio_name(), WORKING_SPACE),
            encode: processor(WORKING_SPACE, s.ocio_name()),
        })
    });
    let i = ResolvedColorSpace::ALL
        .iter()
        .position(|&s| s == space)
        .expect("ALL lists every variant");
    &curves[i]
}

/// The clamp below black that a non-raw decode or encode applies first (see
/// the module documentation).
#[inline]
fn floor(space: ResolvedColorSpace, values: &mut [f32]) {
    if space != ResolvedColorSpace::Raw {
        for v in values {
            *v = v.max(0.0);
        }
    }
}

/// Runs a per-channel processor over any number of samples: whole triplets
/// in one batch, and a short tail padded to one.
fn apply_per_channel(cpu: &ocio::CpuProcessor, values: &mut [f32]) {
    let whole = values.len() / 3 * 3;
    let (head, tail) = values.split_at_mut(whole);
    if !head.is_empty() {
        cpu.apply_rgb_slice(head);
    }
    if !tail.is_empty() {
        let mut px = [0.0f32; 3];
        px[..tail.len()].copy_from_slice(tail);
        cpu.apply_rgb(&mut px);
        tail.copy_from_slice(&px[..tail.len()]);
    }
}

impl ResolvedColorSpace {
    /// The space's name in the OCIO config, which is also the MaterialX name
    /// and the spelling recorded in a `.tx` (`crust:mipspace=`). Matched on
    /// the variant, so a new space is a compile error here rather than a
    /// silently unnamed one.
    pub fn ocio_name(self) -> &'static str {
        match self {
            ResolvedColorSpace::Srgb => "srgb_texture",
            ResolvedColorSpace::Gamma22 => "g22_rec709",
            ResolvedColorSpace::Gamma18 => "g18_rec709",
            ResolvedColorSpace::Raw => "raw",
        }
    }

    /// One encoded sample as linear light in [`WORKING_SPACE`].
    ///
    /// Per call this is an OCIO processor run on one pixel, so loops over
    /// many samples use [`ResolvedColorSpace::decode_slice`] instead.
    pub fn decode(self, encoded: f32) -> f32 {
        let mut px = [encoded; 3];
        self.decode_slice(&mut px);
        px[0]
    }

    /// An encoded colour as linear light.
    pub fn decode_rgb(self, encoded: Vec3A) -> Vec3A {
        let mut px = encoded.to_array();
        self.decode_slice(&mut px);
        Vec3A::from_array(px)
    }

    /// Decodes every sample in place. The curve is per channel, so the
    /// samples need not be RGB.
    pub fn decode_slice(self, values: &mut [f32]) {
        if self == ResolvedColorSpace::Raw {
            return;
        }
        floor(self, values);
        apply_per_channel(&curve(self).decode, values);
    }

    /// One linear sample re-encoded in this space: the inverse of
    /// [`ResolvedColorSpace::decode`].
    pub fn encode(self, linear: f32) -> f32 {
        let mut px = [linear; 3];
        self.encode_slice(&mut px);
        px[0]
    }

    /// Encodes every sample in place.
    pub fn encode_slice(self, values: &mut [f32]) {
        if self == ResolvedColorSpace::Raw {
            return;
        }
        floor(self, values);
        apply_per_channel(&curve(self).encode, values);
    }

    /// The space an OCIO colour-space name binds a texture with: `Some` for
    /// any name or alias of one of the four, `None` for a name the config
    /// does not know or whose space is not one of them.
    pub fn from_ocio_name(name: &str) -> Option<ResolvedColorSpace> {
        let found = config().get_color_space(name)?;
        ResolvedColorSpace::ALL.into_iter().find(|s| {
            config()
                .get_color_space(s.ocio_name())
                .is_some_and(|c| c.name() == found.name())
        })
    }
}

/// Encodes linear [`WORKING_SPACE`] RGB for the preview PNG, in place, through
/// [`PREVIEW_DISPLAY`] / [`PREVIEW_VIEW`]. Values are clamped to `[0, 1]`
/// first: the preview is a clamp and an encode, never a tone map.
pub fn encode_preview(rgb: &mut [f32]) {
    static PREVIEW: OnceLock<ocio::CpuProcessor> = OnceLock::new();
    let cpu = PREVIEW.get_or_init(|| {
        config()
            .get_display_view_processor(WORKING_SPACE, PREVIEW_DISPLAY, PREVIEW_VIEW)
            .unwrap_or_else(|e| panic!("OCIO {PREVIEW_DISPLAY} / {PREVIEW_VIEW}: {e}"))
            .default_cpu_processor()
    });
    for v in rgb.iter_mut() {
        // `clamp` keeps a NaN; the preview has no use for one.
        *v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    }
    assert!(rgb.len().is_multiple_of(3), "preview pixels are RGB");
    cpu.apply_rgb_slice(rgb);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference curves the OCIO config must reproduce, written out so
    /// the tests do not compare OCIO with itself.
    fn srgb_eotf(c: f32) -> f32 {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    #[test]
    fn the_builtin_config_loads_and_knows_the_working_space() {
        let cs = config().get_color_space(WORKING_SPACE).expect("lin_rec709");
        assert_eq!(cs.name(), "Linear Rec.709 (sRGB)");
        for s in ResolvedColorSpace::ALL {
            assert!(
                config().get_color_space(s.ocio_name()).is_some(),
                "{}",
                s.ocio_name()
            );
        }
    }

    #[test]
    fn utils_luminance_uses_the_config_luma_coefficients() {
        // `utils::luminance` hard-codes Rec.709 luma for the hot path; this
        // is the pair it must stay equal to.
        let [r, g, b] = config().default_luma_coefs();
        for (axis, want) in [(Vec3A::X, r), (Vec3A::Y, g), (Vec3A::Z, b)] {
            assert_eq!(utils::luminance(axis), want as f32, "{axis}");
        }
    }

    #[test]
    fn the_curves_match_their_definitions_at_every_byte() {
        for i in 0..=255 {
            let e = i as f32 / 255.0;
            let srgb = ResolvedColorSpace::Srgb.decode(e);
            assert!((srgb - srgb_eotf(e)).abs() <= 1e-6, "srgb {i}: {srgb}");
            // The power laws are bit-identical to `powf`.
            assert_eq!(
                ResolvedColorSpace::Gamma22.decode(e),
                e.powf(2.2),
                "g22 {i}"
            );
            assert_eq!(
                ResolvedColorSpace::Gamma18.decode(e),
                e.powf(1.8),
                "g18 {i}"
            );
            assert_eq!(ResolvedColorSpace::Raw.decode(e).to_bits(), e.to_bits());
        }
    }

    #[test]
    fn encode_inverts_decode() {
        for s in ResolvedColorSpace::ALL {
            for i in 0..=1000 {
                let l = i as f32 / 1000.0;
                let back = s.decode(s.encode(l));
                assert!((back - l).abs() < 1e-5, "{s:?} {l} -> {back}");
            }
        }
    }

    #[test]
    fn negatives_clamp_for_curves_and_pass_for_raw() {
        assert_eq!(ResolvedColorSpace::Gamma22.decode(-0.5), 0.0);
        assert_eq!(ResolvedColorSpace::Srgb.decode(-0.5), 0.0);
        assert_eq!(ResolvedColorSpace::Gamma18.encode(-0.5), 0.0);
        assert_eq!(ResolvedColorSpace::Raw.decode(-0.5), -0.5);
    }

    #[test]
    fn slices_of_any_length_decode_like_single_samples() {
        for n in [1, 2, 3, 4, 5, 7] {
            let mut v: Vec<f32> = (0..n).map(|i| i as f32 / 7.0).collect();
            let want: Vec<f32> = v
                .iter()
                .map(|&x| ResolvedColorSpace::Srgb.decode(x))
                .collect();
            ResolvedColorSpace::Srgb.decode_slice(&mut v);
            assert_eq!(v, want, "len {n}");
        }
    }

    #[test]
    fn ocio_names_resolve_through_every_alias() {
        for (name, want) in [
            ("srgb_texture", Some(ResolvedColorSpace::Srgb)),
            (
                "sRGB Encoded Rec.709 (sRGB)",
                Some(ResolvedColorSpace::Srgb),
            ),
            ("Utility - sRGB - Texture", Some(ResolvedColorSpace::Srgb)),
            ("g22_rec709_tx", Some(ResolvedColorSpace::Gamma22)),
            (
                "Gamma 1.8 Rec.709 - Texture",
                Some(ResolvedColorSpace::Gamma18),
            ),
            ("raw", Some(ResolvedColorSpace::Raw)),
            ("none", Some(ResolvedColorSpace::Raw)),
            // Known to the config, but not a curve on Rec.709 primaries.
            ("lin_rec709", None),
            ("acescg", None),
            ("g22_ap1", None),
            ("not_a_space", None),
        ] {
            assert_eq!(ResolvedColorSpace::from_ocio_name(name), want, "{name}");
        }
    }

    #[test]
    fn the_preview_is_a_clamped_srgb_encode() {
        let mut px = [0.5, -1.0, 7.0, f32::NAN, 0.0, 1.0];
        encode_preview(&mut px);
        // The display/view path goes through the config's reference spaces,
        // so white comes back to within a matrix round trip, not exactly.
        let want = [0.735_356_6, 0.0, 1.0, 0.0, 0.0, 1.0];
        for (got, want) in px.iter().zip(want) {
            assert!((got - want).abs() < 1e-6, "{px:?}");
        }
    }
}
