//! Decoded tile storage: one mip level, the texel types it can hold, and
//! the per-tile pyramid.

use super::mip::{reduce_half, reduce_half_linear};

/// One resolution of one tile: row-major RGB, or RGBA when the tile carries
/// alpha — display-encoded bytes (`u8`) or linear floats (`f32`).
pub(super) struct Level<T> {
    pub(super) pixels: Vec<T>,
    pub(super) width: usize,
    pub(super) height: usize,
}

/// A stored sample type, and how three of them become linear RGB.
///
/// The `u8` arm is the table lookup the preload path has always done; the
/// `f32` arm is the identity, because an `f32` tile is linear by construction.
pub(super) trait Texel: Copy {
    fn linear(pixels: &[Self], o: usize, to_linear: &[f32; 256]) -> [f32; 3];

    /// The alpha stored at `o`, as coverage: `a / 255` for a byte, never
    /// through the colour space's curve, and the value itself for a float.
    fn alpha(pixels: &[Self], o: usize) -> f32;
}

impl Texel for u8 {
    #[inline(always)]
    fn linear(pixels: &[u8], o: usize, to_linear: &[f32; 256]) -> [f32; 3] {
        [
            to_linear[pixels[o] as usize],
            to_linear[pixels[o + 1] as usize],
            to_linear[pixels[o + 2] as usize],
        ]
    }

    #[inline(always)]
    fn alpha(pixels: &[u8], o: usize) -> f32 {
        crate::ALPHA_U8[pixels[o] as usize]
    }
}

impl Texel for f32 {
    #[inline(always)]
    fn linear(pixels: &[f32], o: usize, _: &[f32; 256]) -> [f32; 3] {
        [pixels[o], pixels[o + 1], pixels[o + 2]]
    }

    #[inline(always)]
    fn alpha(pixels: &[f32], o: usize) -> f32 {
        pixels[o]
    }
}

/// One decoded UDIM tile, as a mip pyramid.
///
/// `levels[0]` is the tile as `decode_tile` produced it (already under the
/// `CRUST_TEX_MAX` cap); each further level halves both axes until both reach
/// one texel. A tile with a single level is the pre-pyramid behaviour exactly:
/// no level to select between, so `width` is ignored structurally rather than
/// by a branch, which is what `CRUST_TEX_MIP=0` relies on.
pub(super) struct Tile<T> {
    /// UDIM number, `1001 + u + 10·v`.
    pub(super) number: u32,
    /// Whether every level holds four samples a texel, RGBA, rather than
    /// three. Per tile, because a UDIM set's tiles are separate files and only
    /// some of them may cut anything (see [`crate::drop_opaque_alpha`]).
    pub(super) alpha: bool,
    pub(super) levels: Vec<Level<T>>,
}

impl<T> Tile<T> {
    /// The tile as authored, with no coarser levels.
    pub(super) fn unmipped(
        number: u32,
        pixels: Vec<T>,
        width: usize,
        height: usize,
        alpha: bool,
    ) -> Tile<T> {
        Tile {
            number,
            alpha,
            levels: vec![Level {
                pixels,
                width,
                height,
            }],
        }
    }
}

impl Tile<f32> {
    /// [`Tile::build_pyramid`] for a linear tile: the same area-weighted
    /// reduction ([`reduce_half_linear`] shares `axis_taps` with
    /// [`reduce_half`]) with no decode or re-encode, since there is no
    /// encoding.
    pub(super) fn build_pyramid_linear(&mut self) {
        loop {
            let src = self.levels.last().expect("a tile always has level 0");
            if src.width <= 1 && src.height <= 1 {
                break;
            }
            let (pixels, width, height) =
                reduce_half_linear(&src.pixels, src.width, src.height, self.alpha);
            self.levels.push(Level {
                pixels,
                width,
                height,
            });
        }
    }
}

impl Tile<u8> {
    /// Appends halved levels until both axes reach one texel.
    ///
    /// Each level is a box average of its parent over each new texel's own
    /// footprint — a plain 2x2 whenever both axes are even — computed in
    /// **linear light** and re-encoded to `u8` through `steps` (see
    /// [`crate::TransferCurve::code_steps`]); averaging
    /// display-encoded values is not averaging light, and a mip chain built
    /// that way drifts darker at every level. (The `CRUST_TEX_MAX` reduction
    /// in `decode_tile` deliberately does average in the file's encoding, to
    /// keep a capped tile matching a DCC's preview of the same file; the two
    /// conventions are recorded in `docs/color_management.md`.)
    ///
    /// Axes halve by `div_ceil`, not by `>> 1`. `decode_tile` reduces by an
    /// arbitrary integer factor, so level 0 is routinely odd — a 3000x2000
    /// source under a 1024 cap is 1000x666 — and `sample_tile` maps
    /// `x = u·width − 0.5`, so every level has to cover the whole `[0, 1]`
    /// domain. Flooring an odd axis drops its last half-texel and the level's
    /// domain slips against level 0's, which shows up as a crawl across mip
    /// transitions on a slow camera move.
    pub(super) fn build_pyramid(&mut self, to_linear: &[f32; 256], steps: &[f32; 255]) {
        loop {
            let src = self.levels.last().expect("a tile always has level 0");
            if src.width <= 1 && src.height <= 1 {
                break;
            }
            let (pixels, width, height) = reduce_half(
                &src.pixels,
                src.width,
                src.height,
                self.alpha,
                to_linear,
                steps,
            );
            self.levels.push(Level {
                pixels,
                width,
                height,
            });
        }
    }
}
