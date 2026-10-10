//! The one trilinear filter every texture backend shares.
//!
//! Level selection from a footprint was written four times — preloaded and
//! streamed UV textures, preloaded and streamed Ptex — and the streamed copies
//! described themselves as "line-for-line equivalent" to the preloaded ones,
//! which is exactly the promise a copy stops keeping. The streamed ↔ preloaded
//! pairs are pinned bit for bit (`streaming_and_preloading_agree_bit_for_bit`,
//! `streamed_and_preloaded_agree_texel_for_texel`), so the selection they
//! share lives here once, generic over the mip chain it reads and
//! monomorphised per backend: no dispatch is added to a texel fetch.
//!
//! What stays per backend is the bilinear tap within one level (a UV chart's
//! rows run top-down against `v`, a Ptex face's do not) and how a texel is
//! fetched.

/// A mip chain [`trilinear`] can read: one tile, chart or face.
pub(crate) trait MipSource {
    /// What a lookup returns: `[f32; 4]` RGBA for UV textures, `Vec3A` for
    /// Ptex.
    type Texel: Copy;

    /// Levels held, at least one. A single level is the pre-pyramid behaviour
    /// exactly: nothing to select between, so the footprint is ignored.
    fn level_count(&self) -> usize;

    /// Texels across the widest axis of level 0 — what a footprint, measured
    /// as a fraction of the chart, is converted to texels by. The widest
    /// axis, because an isotropic footprint over an anisotropic chart is
    /// minified most where the texels are densest, and reading the coarser of
    /// the two levels is the choice that does not alias.
    fn texels_across(&self) -> f32;

    /// A bilinear lookup within `level`, at coordinates already reduced to
    /// `[0, 1]`. `None` only for a streamed read that failed.
    fn bilinear(&self, level: usize, u: f32, v: f32) -> Option<Self::Texel>;

    /// `a` toward `b` by `t`, the blend between two bracketing levels.
    fn blend(a: Self::Texel, b: Self::Texel, t: f32) -> Self::Texel;
}

/// Trilinear lookup, the level chosen from the footprint `width` (a fraction
/// of the chart).
///
/// The level is `log2(width · texels_across)`: a footprint covering one texel
/// of level 0 reads level 0, one covering two reads level 1, and so on. The
/// two bracketing levels are sampled bilinearly and blended, so a surface
/// receding from the camera crosses mip levels smoothly instead of stepping.
///
/// A non-positive or non-finite `width` — a caller with no derivatives, or
/// `CRUST_RAY_CONES=0` — and a chart with no pyramid both short-circuit to a
/// single bilinear tap on level 0, bit-identical to a texture with no levels
/// at all. So does magnification (the footprint fits inside one texel), the
/// common case, whose answer is level 0 whatever the `log2` says: taking it
/// before the `log2` rather than through the clamp was worth ~9% of render on
/// a scene whose output does not change at all.
///
/// A failed read of the finer level is `None`; of the coarser one, the finer
/// level alone.
#[inline(always)]
pub(crate) fn trilinear<S: MipSource>(s: &S, u: f32, v: f32, width: f32) -> Option<S::Texel> {
    let levels = s.level_count();
    if levels == 1 || !width.is_finite() || width <= 0.0 {
        return s.bilinear(0, u, v);
    }
    let texels = width * s.texels_across();
    if texels <= 1.0 {
        return s.bilinear(0, u, v);
    }
    let lod = texels.log2().clamp(0.0, (levels - 1) as f32);
    let lo = lod.floor();
    let frac = lod - lo;
    let a = s.bilinear(lo as usize, u, v)?;
    if frac <= 0.0 {
        return Some(a);
    }
    Some(match s.bilinear((lo as usize + 1).min(levels - 1), u, v) {
        Some(b) => S::blend(a, b, frac),
        None => a,
    })
}

/// The four taps and two weights of a bilinear lookup at texel-space `(x, y)`
/// (texel centres at integers, so a caller passes `u·w − 0.5`) in a `w`×`h`
/// level, clamped to its edges.
#[derive(Clone, Copy)]
pub(crate) struct Taps {
    pub(crate) x0: usize,
    pub(crate) x1: usize,
    pub(crate) y0: usize,
    pub(crate) y1: usize,
    pub(crate) fx: f32,
    pub(crate) fy: f32,
}

impl Taps {
    #[inline(always)]
    pub(crate) fn new(x: f32, y: f32, w: usize, h: usize) -> Taps {
        let x0 = x.floor();
        let y0 = y.floor();
        // `n - 1`: every level holds at least one texel on each axis.
        let clampi = |i: f32, n: usize| (i.max(0.0) as usize).min(n - 1);
        Taps {
            x0: clampi(x0, w),
            x1: clampi(x0 + 1.0, w),
            y0: clampi(y0, h),
            y1: clampi(y0 + 1.0, h),
            fx: x - x0,
            fy: y - y0,
        }
    }

    /// The bilinear blend of four RGB taps `[a b; c d]` (rows `y0`, `y1`), as
    /// RGBA with an opaque alpha — the UV textures' arithmetic, per channel.
    #[inline(always)]
    pub(crate) fn blend_rgb(&self, a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * self.fx;
            let bot = c[k] + (d[k] - c[k]) * self.fx;
            out[k] = top + (bot - top) * self.fy;
        }
        out[3] = 1.0;
        out
    }

    /// [`Taps::blend_rgb`] for four RGBA taps, alpha blended as a fourth
    /// channel by the same arithmetic — a texture that carries alpha.
    #[inline(always)]
    pub(crate) fn blend_rgba(
        &self,
        a: [f32; 4],
        b: [f32; 4],
        c: [f32; 4],
        d: [f32; 4],
    ) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * self.fx;
            let bot = c[k] + (d[k] - c[k]) * self.fx;
            out[k] = top + (bot - top) * self.fy;
        }
        out
    }
}

/// `a` toward `b` by `t` per RGB channel, alpha opaque: the level blend of the
/// UV textures.
#[inline(always)]
pub(crate) fn lerp_rgba(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for k in 0..3 {
        out[k] = a[k] + (b[k] - a[k]) * t;
    }
    out[3] = 1.0;
    out
}

/// [`lerp_rgba`] with alpha blended too: the level blend of a UV texture that
/// carries alpha.
#[inline(always)]
pub(crate) fn lerp_rgba_alpha(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for k in 0..4 {
        out[k] = a[k] + (b[k] - a[k]) * t;
    }
    out
}
