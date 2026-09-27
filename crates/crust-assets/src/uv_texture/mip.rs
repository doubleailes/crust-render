//! Mip reduction and colour-curve tables, shared with the `.tx` writers
//! (`tiled/write.rs`, `tiled/exr_write.rs`): an odd axis is an area-weighted
//! resample, an even one an exact 2x2 box.

/// Which source texels one destination texel of an axis reduction covers, and
/// how much of each it covers.
///
/// A destination texel is an equal-width slice of the `[0, 1]` domain — that
/// is exactly how `sample_level` reconstructs it, mapping
/// `x = u * width - 0.5` — so texel `j` of a `src -> dst` reduction owns the
/// source interval `[j·s, (j+1)·s]` for `s = src/dst`, and its value is the
/// average of the source *over that interval*. Weights are the overlaps.
///
/// Three taps is the ceiling, not a guess: `dst` is `src.div_ceil(2)`, so
/// `s <= 2`, and an interval of length at most 2 meets at most three unit
/// cells (worst case `src = 5`, whose middle texel spans `[5/3, 10/3]` and
/// touches source texels 1, 2 and 3). Checked for every `src` up to 9000.
#[derive(Clone, Copy, Debug)]
pub(super) struct Tap {
    /// First source texel this destination texel overlaps.
    pub(super) start: usize,
    /// How many it overlaps: 1, 2 or 3.
    pub(super) count: usize,
    /// Overlap per source texel, **unnormalised** — see [`weighted`].
    pub(super) weight: [f32; 3],
}

/// The taps for one axis, and the weight each destination texel totals.
///
/// Built once per axis per level rather than per texel: every row of a level
/// resamples its columns identically, and there are only `dst` of them.
pub(super) fn axis_taps(src: usize, dst: usize) -> (Vec<Tap>, f32) {
    // **The bounds are `j * src / dst`, not `lo + s`, and they are `f64`.**
    // Accumulating `lo + s` in `f32` lets the interval drift off the end of
    // the axis: at `src = 1795` the last texel came out covering 1.99878
    // source texels against the 1.99889 every other texel covered, so
    // dividing them all by one total biased that texel by 5e-5. Computing
    // each bound from its own integers instead makes `hi(j)` and `lo(j + 1)`
    // the same expression — the taps tile with no gap and no overlap — and
    // makes the last `hi` exactly `src`, since the quotient is an integer and
    // IEEE division returns it exactly. `f64` because `j * src` passes 2^24
    // for a large level, which is where `f32` stops counting integers.
    let (fsrc, fdst) = (src as f64, dst as f64);
    let mut taps = Vec::with_capacity(dst);
    for j in 0..dst {
        let lo = j as f64 * fsrc / fdst;
        let hi = (j + 1) as f64 * fsrc / fdst;
        let mut tap = Tap {
            start: (lo as usize).min(src - 1),
            count: 0,
            weight: [0.0; 3],
        };
        let mut i = tap.start;
        while i < src && (i as f64) < hi {
            let overlap = hi.min((i + 1) as f64) - lo.max(i as f64);
            if overlap > 0.0 {
                debug_assert!(tap.count < 3, "an axis tap cannot reach four texels");
                if tap.count < 3 {
                    tap.weight[tap.count] = overlap as f32;
                    tap.count += 1;
                }
            }
            i += 1;
        }
        // Unreachable while `dst <= src` — but a zero-weight texel would
        // divide by zero below, so it falls back to its own start texel whole.
        if tap.count == 0 {
            tap.weight[0] = 1.0;
            tap.count = 1;
        }
        taps.push(tap);
    }
    // The same for every destination texel, the taps tiling the axis. Summed
    // from the first texel's weights rather than from `src / dst` so it is
    // the `f32` sum the inner loop actually accumulates against — on an even
    // axis that is exactly `2.0`, which is what keeps the even case exact.
    let sum: f32 = taps[0].weight[..taps[0].count].iter().sum();
    (taps, sum)
}

/// One destination texel: the source over its footprint, area-weighted.
///
/// **`total` divides once, at the end.** On an even axis every overlap is
/// exactly `1.0` and the axis sum exactly `2.0`, so `total` is exactly `4.0`
/// and this reduces to `(((a + b) + c) + d) / 4.0` in row-major order — bit
/// for bit what the old `0.25 * (a + b + c + d)` produced. That is what keeps
/// every power-of-two texture, and with it every checked-in golden and the
/// streamed-versus-preloaded invariant, exactly where it was. Pre-normalising
/// the weights instead would spend four roundings where this spends one, and
/// the even case would drift.
#[inline]
pub(super) fn weighted(row: &Tap, col: &Tap, total: f32, at: impl Fn(usize, usize) -> f32) -> f32 {
    let mut acc = 0.0;
    for dy in 0..row.count {
        let wy = row.weight[dy];
        for dx in 0..col.count {
            acc += wy * col.weight[dx] * at(col.start + dx, row.start + dy);
        }
    }
    acc / total
}

/// One mip level from the one above it: a box average over each destination
/// texel's own footprint, in linear light, re-encoded to `u8` in the file's
/// own space. On an even axis that footprint is exactly 2x2.
///
/// Shared by the in-memory pyramid ([`Tile::build_pyramid`](super::tile::Tile::build_pyramid)) and the `.tx`
/// writer ([`crate::tiled::write_tx`]) **on purpose**. A streamed render and a
/// preloaded one are supposed to agree texel for texel, and the only way to be
/// sure of that is for the two paths to run the same code rather than two
/// copies of the same intent.
///
/// Averaging in linear and not in the file's encoding is the point: summing
/// display-encoded values is not summing light, and a chain built that way
/// drifts darker at every level (a black/white checker comes out at 0.21
/// instead of 0.5). Note that the `CRUST_TEX_MAX` reduction in `decode_tile`
/// deliberately does the opposite — it is a *resize*, meant to match a DCC's
/// preview of the same file, not a filter. `docs/color_management.md` records
/// both conventions.
///
/// Axes halve by `div_ceil`, never `>> 1`: level 0 is routinely odd (an
/// arbitrary integer `CRUST_TEX_MAX` factor, or an odd authored size), and the
/// samplers map `x = u * width - 0.5`, so every level has to span the whole
/// `[0, 1]` domain. Flooring an odd axis drops its last half-texel and that
/// level's domain slips against level 0's — a crawl across mip transitions on
/// a slow camera move.
///
/// **On an odd axis that makes the reduction a resample, not a 2x2 box**, and
/// it is weighted by area — see [`axis_taps`]. Taking the 2x2 with the source
/// index clamped to the last column instead, which is what this did before,
/// hands the trailing column a third of the level's weight where it is owed a
/// fifth: a 5-wide row of `[250, 200, 150, 100, 50]` came out
/// `[225, 125, 50]`, mean 133 against the source's 150, and the drift
/// compounds at every level. A 25-wide tile lit only at its right edge
/// bottomed out **6x too bright**, and the same tile lit at its *left* edge
/// too dark — the clamp is at one end, so the two disagreed by 8x.
pub(crate) fn reduce_half(
    src: &[u8],
    sw: usize,
    sh: usize,
    to_linear: &[f32; 256],
    encode: fn(f32) -> f32,
) -> (Vec<u8>, usize, usize) {
    let (w, h) = (sw.div_ceil(2), sh.div_ceil(2));
    let (cols, xsum) = axis_taps(sw, w);
    let (rows, ysum) = axis_taps(sh, h);
    // One divisor for the whole level: the taps tile each axis exactly, so
    // every destination texel carries the same total weight.
    let total = ysum * xsum;
    let mut pixels = vec![0u8; w * h * 3];
    for (y, row) in rows.iter().enumerate() {
        for (x, col) in cols.iter().enumerate() {
            let o = (y * w + x) * 3;
            for k in 0..3 {
                let at = |xi: usize, yi: usize| to_linear[src[(yi * sw + xi) * 3 + k] as usize];
                let mean = weighted(row, col, total, at);
                pixels[o + k] = (encode(mean) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    }
    (pixels, w, h)
}

/// The same reduction for data that is **already linear**: the same
/// area-weighted box average of `f32` RGB, with no decode and no re-encode
/// because there is no encoding.
///
/// Deliberately written next to [`reduce_half`] rather than generalised over
/// the sample type. The two have to agree on everything *except* the transfer
/// curve — the `div_ceil` halving, the [`axis_taps`] weighting, the order the
/// taps are summed in — and an EXR-backed `.tx` and a TIFF-backed one that
/// disagreed on odd levels would each be internally consistent and produce
/// different images, which is the hardest kind of disagreement to see. Keeping
/// them adjacent is what makes a change to one an obvious omission in the
/// other; `the_two_reducers_agree_on_an_odd_level` is what makes it a failing
/// test rather than a reading exercise.
///
/// Axes halve by `div_ceil`, never `>> 1`, for the reason [`reduce_half`]
/// records: the samplers map `x = u * width - 0.5`, so flooring an odd axis
/// drops its last half-texel and that level's domain slips against level 0's.
pub(crate) fn reduce_half_linear(src: &[f32], sw: usize, sh: usize) -> (Vec<f32>, usize, usize) {
    let (w, h) = (sw.div_ceil(2), sh.div_ceil(2));
    let (cols, xsum) = axis_taps(sw, w);
    let (rows, ysum) = axis_taps(sh, h);
    let total = ysum * xsum;
    let mut pixels = vec![0.0f32; w * h * 3];
    for (y, row) in rows.iter().enumerate() {
        for (x, col) in cols.iter().enumerate() {
            let o = (y * w + x) * 3;
            for k in 0..3 {
                let at = |xi: usize, yi: usize| src[(yi * sw + xi) * 3 + k];
                pixels[o + k] = weighted(row, col, total, at);
            }
        }
    }
    (pixels, w, h)
}
