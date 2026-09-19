//!! Per-face Ptex textures, fully decoded at load.
//!!
//!! The host's implementation of `crust_core::PtexTexture`, on top of the
//!! pure-Rust `ptex` reader.
//!!
//!! Why the whole file is read up front: `ptex::PtexReader` takes `&mut self` for
//! every pixel read and caches only level indexes, not pixel data, so a texel
//! lookup means a seek, a read and possibly a zlib inflate. A path tracer asks
//! for texels from every Rayon worker, millions of times per frame, in an order
//! nothing can predict. Going back to the file per lookup would be both a
//! lock-contention disaster and orders of magnitude too slow, so every face is
//! decoded once at load time into an immutable buffer that threads then share
//! without synchronisation.
//!
//! Why not at full resolution: production Ptex is authored for close-ups. The
//! island's `isLavaRocks/Color/rockfacemain0001_geo.ptx` is 11 384 faces over
//! 631 MB compressed, which inflates to several GB at full resolution — for an
//! asset covering a few hundred thousand pixels in the reference framing. Faces
//! load at the coarsest mip level that still exceeds what the render can
//! resolve, capped by `DEFAULT_MAX_LOG2`. Ptex files carry stored mipmaps and the
//! reader computes any level they lack, so this costs nothing but a smaller read.
//! `CRUST_PTEX_MAX_LOG2` overrides the cap as a log2 edge length.

use crust_core::{PtexTexture, Vec3A};
use std::path::Path;

/// Default per-face resolution cap, as a log2 edge length: 32×32 texels.
///
/// A quad covering `n` pixels on screen cannot show more than about `n` texels,
/// and the island's meshes are dense — `mountain_geo` is 33 503 quads, so in a
/// 595×520 framing a quad lands on a handful of pixels. 32×32 is already far
/// past what such a framing resolves, while keeping a 3 272-face texture near
/// 10 MB instead of a gigabyte.
pub const DEFAULT_MAX_LOG2: i8 = 5;

/// Are per-face mip pyramids built? `CRUST_PTEX_MIP=0` keeps one level per
/// face — the pre-pyramid layout bit for bit, since a one-level face has
/// nothing to select between — and a third less memory. Note the cap and the
/// pyramid answer different questions: the cap is the *ceiling* on detail,
/// the pyramid is what makes minification below it correct. With both, the
/// cap can come *down*: the island at a 16x16 base plus a full pyramid is
/// ~2.45 GiB against 4.58 GiB flat at 32x32, and filters better at distance.
fn mip_enabled() -> bool {
    std::env::var("CRUST_PTEX_MIP").as_deref() != Ok("0")
}

/// Mip levels a `w`x`h` face holds: halve both axes until both reach one.
fn level_count(w: usize, h: usize) -> u8 {
    let (mut w, mut h, mut n) = (w, h, 1u8);
    while w > 1 || h > 1 {
        w = (w / 2).max(1);
        h = (h / 2).max(1);
        n += 1;
    }
    n
}

/// One face's mip pyramid within [`PtexColor::texels`].
///
/// `offset`/`width`/`height` describe level 0 — the face at the cap. The
/// `levels - 1` coarser levels follow it contiguously, each halving both axes
/// until both reach one texel, so a level's offset is derived by walking the
/// chain rather than stored: the walk is a handful of shifts and adds against
/// a `levels` that never exceeds the cap's log2, and per-face offset arrays
/// would cost more than they save over 2.5 M faces.
///
/// `levels == 1` is the pre-pyramid layout exactly, which is what
/// `CRUST_PTEX_MIP=0` produces.
struct Face {
    offset: u32,
    width: u16,
    height: u16,
    levels: u8,
}

impl Face {
    /// Offset and size of mip level `k`, clamped to the coarsest level held.
    #[inline]
    fn level(&self, k: usize) -> (usize, usize, usize) {
        let k = k.min(self.levels as usize - 1);
        let (mut off, mut w, mut h) = (
            self.offset as usize,
            self.width as usize,
            self.height as usize,
        );
        for _ in 0..k {
            off += w * h * 3;
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        (off, w, h)
    }

    /// Texels the whole pyramid occupies, in floats.
    fn floats(&self) -> usize {
        let (off, w, h) = self.level(self.levels as usize - 1);
        off + w * h * 3 - self.offset as usize
    }
}

/// A Ptex colour texture, fully decoded to linear RGB.
pub struct PtexColor {
    faces: Vec<Face>,
    /// Interleaved RGB, row-major within each face, v-major as Ptex stores it.
    texels: Vec<f32>,
    /// Constant colour for a face that failed to decode.
    fallback: Vec3A,
}

impl PtexColor {
    /// Opens `path` and decodes every face to linear RGB.
    ///
    /// `Err` carries a message suitable for a warning; the caller falls back to
    /// a constant colour rather than failing the render.
    pub fn open(path: &Path) -> Result<Self, String> {
        PtexColor::open_with(path, mip_enabled())
    }

    /// [`PtexColor::open`] with the mip decision passed in rather than read
    /// from the environment — the same seam `UvTexture::open_with` offers,
    /// and for the same reason: comparing both sides should not mean mutating
    /// a process-global the rest of the program is reading.
    pub fn open_with(path: &Path, mip: bool) -> Result<Self, String> {
        let mut tx = ptex::PtexReader::open(path).map_err(|e| e.to_string())?;

        let n_chan = tx.num_channels();
        if n_chan == 0 {
            return Err("file has no channels".into());
        }
        let dt = tx.data_type();
        let scale = dt.one_value_inv();
        let max_log2 = max_log2_from_env();
        // Which reduction the pyramid is built with, decided once for the
        // file rather than guessed per face. A triangle texture packs *two*
        // triangles into its square of texels — the upright one and its
        // mirror across the anti-diagonal — so averaging a 2x2 block of it
        // mixes texels from both, and every level above 0 comes out wrong in
        // a way that still looks like plausible texture. See `reduce_triangle`.
        let triangle = tx.mesh_type() == ptex::MeshType::Triangle;
        let reduce: LevelReduction = if triangle {
            reduce_triangle
        } else {
            reduce_quad
        };

        let n_faces = tx.num_faces();
        let mut faces = Vec::with_capacity(n_faces);
        let mut texels: Vec<f32> = Vec::new();

        for faceid in 0..n_faces {
            let info = *tx.face_info(faceid).map_err(|e| e.to_string())?;
            // Clamp each axis independently: Ptex faces are frequently
            // non-square (64x16 is common) and clamping the pair together
            // would distort the aspect the file chose. A *triangle* face is
            // square by construction and the format defines only symmetric
            // reductions for one (`ptex-rs` refuses an anisotropic request
            // outright), so there the pair clamps together.
            let res = if triangle {
                let l = info.res.ulog2.min(info.res.vlog2).min(max_log2);
                ptex::Res::new(l, l)
            } else {
                ptex::Res::new(info.res.ulog2.min(max_log2), info.res.vlog2.min(max_log2))
            };
            let (w, h) = (res.u(), res.v());
            let offset = texels.len() as u32;
            let levels = if mip { level_count(w, h) } else { 1 };
            let face = Face {
                offset,
                width: w as u16,
                height: h as u16,
                levels,
            };
            texels.resize(texels.len() + face.floats(), 0.0);
            faces.push(face);

            let Ok(raw) = tx.get_data_at_res(faceid, res) else {
                // A single unreadable face should not sink the texture: it
                // stays the zero it was resized to, and the rest still loads.
                tracing::debug!("Ptex {}: face {faceid} unreadable", path.display());
                continue;
            };

            let px = dt.size() * n_chan;
            let out = &mut texels[offset as usize..];
            for i in 0..(w * h) {
                let src = &raw[i * px..];
                for ch in 0..3 {
                    // A single-channel (displacement-style) file feeds channel
                    // 0 to all three, so it reads as greyscale rather than red.
                    let c = if ch < n_chan { ch } else { 0 };
                    let v = read_channel(&src[c * dt.size()..], dt) * scale;
                    // Ptex colour here is display-encoded: the island's
                    // shading network runs it through a gamma-1/2.2 node
                    // (`PxrColorCorrect`) and the GL path declares
                    // `sourceColorSpace = "sRGB"`. Both mean decode by 2.2,
                    // and the engine works in linear light. Done once here
                    // rather than per lookup.
                    out[i * 3 + ch] = v.max(0.0).powf(2.2);
                }
            }
            // Reduce in memory from the level just decoded, rather than
            // asking the reader for each coarser resolution. The reader takes
            // `&mut self` per read and caches no pixels, so every extra level
            // would be another seek and inflate through this serial loop —
            // and would come back display-encoded, needing the `powf` again,
            // which is the part that matters: these texels are already linear
            // `f32`, and a mip level stands in for integrating light over a
            // footprint, so the average belongs in the light it represents
            // rather than in the file's encoding. Ptex resolutions are
            // power-of-two per axis by construction, so halving is exact.
            //
            // What `reduce` does *not* get to choose is which texels to
            // average: that is the file's parameterisation, and it is taken
            // from the mesh type above.
            let face = faces.last().expect("just pushed");
            for k in 1..face.levels as usize {
                let (src_off, sw, sh) = face.level(k - 1);
                let (dst_off, dw, dh) = face.level(k);
                reduce(&mut texels, src_off, sw, sh, dst_off, dw, dh);
            }
        }

        Ok(PtexColor {
            faces,
            texels,
            fallback: Vec3A::splat(0.5),
        })
    }

    /// Bytes held, for the load-time report. Counts every mip level, so a
    /// full pyramid reports about 4/3 of its base — a shade over it for
    /// non-square faces, whose chain halves rather than quarters once the
    /// short axis has pinned at one texel.
    pub fn bytes(&self) -> usize {
        self.texels.len() * std::mem::size_of::<f32>()
            + self.faces.len() * std::mem::size_of::<Face>()
    }

    #[inline]
    fn texel(&self, off: usize, w: usize, x: usize, y: usize) -> Vec3A {
        let i = off + (y * w + x) * 3;
        Vec3A::new(self.texels[i], self.texels[i + 1], self.texels[i + 2])
    }

    /// Bilinear lookup within one mip level of one face.
    ///
    /// Bilinear with clamped borders — every island colour file authors
    /// `uBorderMode`/`vBorderMode = clamp`. Filtering across face boundaries
    /// (what a real `PtexFilter` does with the adjacency data) is still not
    /// attempted: at these face resolutions the seam is far below a pixel,
    /// and getting adjacency edge-rotations wrong is worse than not filtering
    /// across at all.
    fn sample_level(&self, off: usize, w: usize, h: usize, fu: f32, fv: f32) -> Vec3A {
        // Texel centres sit at (i + 0.5)/n.
        let x = fu * w as f32 - 0.5;
        let y = fv * h as f32 - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let tx = x - x0;
        let ty = y - y0;
        let cx = |c: f32| (c.max(0.0) as usize).min(w - 1);
        let cy = |c: f32| (c.max(0.0) as usize).min(h - 1);
        let (x0i, x1i) = (cx(x0), cx(x0 + 1.0));
        let (y0i, y1i) = (cy(y0), cy(y0 + 1.0));

        let top = self
            .texel(off, w, x0i, y0i)
            .lerp(self.texel(off, w, x1i, y0i), tx);
        let bot = self
            .texel(off, w, x0i, y1i)
            .lerp(self.texel(off, w, x1i, y1i), tx);
        top.lerp(bot, ty)
    }
}

impl PtexTexture for PtexColor {
    fn eval(&self, face_id: u32, u: f32, v: f32, width: f32) -> Vec3A {
        let Some(f) = self.faces.get(face_id as usize) else {
            return self.fallback;
        };
        let (w, h) = (f.width as usize, f.height as usize);
        if w == 0 || h == 0 {
            return self.fallback;
        }

        let fu = if u.is_finite() {
            u.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let fv = if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        };

        // No footprint, or no pyramid to choose from: level 0 bilinear, which
        // is bit for bit what this returned before it had levels.
        if f.levels == 1 || !width.is_finite() || width <= 0.0 {
            return self.sample_level(f.offset as usize, w, h, fu, fv);
        }

        // `width` is a fraction of the face, so it converts to texels by the
        // level-0 resolution. Measured against the denser axis: an isotropic
        // footprint over a 64x16 face is minified most where the texels are,
        // and reading the coarser level is the choice that does not alias.
        let texels = width * w.max(h) as f32;
        // Magnification short-circuit, as in `UvTexture::sample_tile`: the
        // answer is level 0 and the `log2` would only confirm it.
        if texels <= 1.0 {
            return self.sample_level(f.offset as usize, w, h, fu, fv);
        }
        let lod = texels.log2().clamp(0.0, (f.levels - 1) as f32);
        let lo = lod.floor();
        let frac = lod - lo;
        let (off_a, wa, ha) = f.level(lo as usize);
        let a = self.sample_level(off_a, wa, ha, fu, fv);
        if frac <= 0.0 {
            return a;
        }
        let (off_b, wb, hb) = f.level(lo as usize + 1);
        a.lerp(self.sample_level(off_b, wb, hb, fu, fv), frac)
    }

    fn num_faces(&self) -> usize {
        self.faces.len()
    }
}

/// How one coarser mip level is built from the level above it.
///
/// `(texels, src_off, sw, sh, dst_off, dw, dh)`; source and destination are
/// disjoint ranges of the same arena, and both levels are interleaved linear
/// RGB. Which of the two implementations a file gets is decided by its mesh
/// type at open, never per face.
type LevelReduction = fn(&mut [f32], usize, usize, usize, usize, usize, usize);

/// One coarser level of a **quad** face: the 2x2 box average, in linear light.
///
/// Matches `ptex::utils::reduce` texel for texel on a square face, including
/// the order the four are summed in. It diverges deliberately once an axis has
/// pinned at one texel: Ptex switches to a one-axis `reduce_u`/`reduce_v` there
/// and this clamps the second sample onto the first, which is the same average
/// by a different route — see `reduce_matches_ptex_rs_on_a_square_face`.
fn reduce_quad(
    texels: &mut [f32],
    src_off: usize,
    sw: usize,
    sh: usize,
    dst_off: usize,
    dw: usize,
    dh: usize,
) {
    for y in 0..dh {
        for x in 0..dw {
            // One axis pins at 1 while the other keeps halving on a
            // non-square face (64x16 is common), so the source index is
            // clamped rather than assumed to be 2x.
            let x0 = (2 * x).min(sw - 1);
            let x1 = (2 * x + 1).min(sw - 1);
            let y0 = (2 * y).min(sh - 1);
            let y1 = (2 * y + 1).min(sh - 1);
            for ch in 0..3 {
                let at = |xi: usize, yi: usize| texels[src_off + (yi * sw + xi) * 3 + ch];
                let mean = 0.25 * (at(x0, y0) + at(x1, y0) + at(x0, y1) + at(x1, y1));
                texels[dst_off + (y * dw + x) * 3 + ch] = mean;
            }
        }
    }
}

/// One coarser level of a **triangle** face: Ptex's mirrored reduction, in
/// linear light.
///
/// A triangle texture is stored as a `w x w` square holding *two* triangles:
/// the upright one at `u + v < w`, and its mirror across the anti-diagonal
/// filling the rest. So the 2x2 block a quad reduction would average is not
/// one neighbourhood — for a texel near the diagonal it straddles both
/// triangles — and the fourth sample of the quad block is simply the wrong
/// texel. `PtexUtils::reduceTri` (and `ptex::utils::reduce_tri`, which this
/// follows exactly) averages three texels of the upright block with the one
/// mirrored texel that completes it:
///
/// ```text
/// dst[v][u] = ¼·( src[2v][2u] + src[2v][2u+1] + src[2v+1][2u]
///                 + src[w-1-2u][w-1-2v] )
/// ```
///
/// Note the index swap in the mirrored term — row from `u`, column from `v`.
/// Getting it backwards is a transpose that still produces plausible texture,
/// which is why `triangle_levels_match_ptex_rs_reduction` compares against
/// `ptex-rs` rather than against an expectation written out by hand.
///
/// The reduction is done here rather than by asking the reader for each
/// coarser resolution (which would also be correct — `get_data_at_res` calls
/// `reduce_tri` itself) for the reason the caller records: the reader returns
/// display-encoded texels, and a mip level averaged in that encoding is not an
/// average of light.
fn reduce_triangle(
    texels: &mut [f32],
    src_off: usize,
    sw: usize,
    sh: usize,
    dst_off: usize,
    dw: usize,
    dh: usize,
) {
    // Triangle faces are square and reduce symmetrically; `open_with` clamps
    // both axes together to keep that true even for a malformed file.
    debug_assert_eq!(sw, sh, "a triangle face must be square");
    debug_assert_eq!(dw, dh, "a triangle level must be square");
    for v in 0..dw {
        for u in 0..dw {
            for ch in 0..3 {
                let at = |x: usize, y: usize| texels[src_off + (y * sw + x) * 3 + ch];
                let mean = 0.25
                    * (at(2 * u, 2 * v)
                        + at(2 * u + 1, 2 * v)
                        + at(2 * u, 2 * v + 1)
                        + at(sw - 1 - 2 * v, sw - 1 - 2 * u));
                texels[dst_off + (v * dw + u) * 3 + ch] = mean;
            }
        }
    }
}

/// Reads one channel of Ptex data as an unnormalized float.
#[inline]
pub fn read_channel(src: &[u8], dt: ptex::DataType) -> f32 {
    match dt {
        ptex::DataType::UInt8 => src[0] as f32,
        ptex::DataType::UInt16 => u16::from_le_bytes([src[0], src[1]]) as f32,
        ptex::DataType::Half => ptex::half_to_float(u16::from_le_bytes([src[0], src[1]])),
        ptex::DataType::Float => f32::from_le_bytes([src[0], src[1], src[2], src[3]]),
    }
}

/// `CRUST_PTEX_MAX_LOG2`, validated, or [`DEFAULT_MAX_LOG2`].
pub fn max_log2_from_env() -> i8 {
    match std::env::var("CRUST_PTEX_MAX_LOG2") {
        Ok(v) => match v.parse::<i8>() {
            // Ptex resolutions are log2-encoded in an i8; 14 is 16384, well
            // past any authored face.
            Ok(n) if (0..=14).contains(&n) => n,
            _ => {
                tracing::warn!(
                    "CRUST_PTEX_MAX_LOG2={v} is not an integer in 0..=14 — using {DEFAULT_MAX_LOG2}"
                );
                DEFAULT_MAX_LOG2
            }
        },
        Err(_) => DEFAULT_MAX_LOG2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A face pyramid over a hand-filled arena, so the layout and the lookup
    /// can be tested without a `.ptx` — the repo ships none, and `ptex-rs` is
    /// a reader, so there is nothing to write one with.
    fn checker_face(w: usize, h: usize) -> PtexColor {
        let face = Face {
            offset: 0,
            width: w as u16,
            height: h as u16,
            levels: level_count(w, h),
        };
        let mut texels = vec![0.0f32; face.floats()];
        // Level 0 is a black/white checker; every coarser level is the box
        // average of the one above, built exactly as `open_with` does.
        for y in 0..h {
            for x in 0..w {
                let c = if (x + y) % 2 == 0 { 1.0 } else { 0.0 };
                for ch in 0..3 {
                    texels[(y * w + x) * 3 + ch] = c;
                }
            }
        }
        for k in 1..face.levels as usize {
            let (src, sw, sh) = face.level(k - 1);
            let (dst, dw, dh) = face.level(k);
            // The same function `open_with` uses, not a copy of it: a fixture
            // that built its levels its own way would agree with the loader
            // only by coincidence.
            reduce_quad(&mut texels, src, sw, sh, dst, dw, dh);
        }
        PtexColor {
            faces: vec![face],
            texels,
            fallback: Vec3A::splat(0.5),
        }
    }

    /// Runs `ptex-rs`'s own reduction over the same linear values, so the two
    /// can be compared texel for texel.
    ///
    /// `ptex::utils` is generic over the file's sample type; `Float` is the
    /// one whose arithmetic is ours — `quarter_sum` there is
    /// `0.25 * (a + b + c + d)`, summed in the order reproduced here — so the
    /// comparison is exact rather than approximate, and a wrong *choice* of
    /// texels cannot hide inside a tolerance.
    fn ptex_rs_reduce(src: &[f32], w: usize, h: usize, triangle: bool) -> Vec<f32> {
        let bytes: Vec<u8> = src.iter().flat_map(|v| v.to_le_bytes()).collect();
        let (dw, dh) = (w / 2, h / 2);
        let mut dst = vec![0u8; dw * dh * 3 * 4];
        let (sstride, dstride) = (w * 3 * 4, dw * 3 * 4);
        if triangle {
            ptex::utils::reduce_tri(
                &bytes,
                sstride,
                w,
                h,
                &mut dst,
                dstride,
                ptex::DataType::Float,
                3,
            );
        } else {
            ptex::utils::reduce(
                &bytes,
                sstride,
                w,
                h,
                &mut dst,
                dstride,
                ptex::DataType::Float,
                3,
            );
        }
        dst.as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(f32::from_le_bytes)
            .collect()
    }

    /// A `w x h` face of values no two of which are equal, so any misaddressed
    /// texel changes the answer.
    fn distinct(w: usize, h: usize) -> Vec<f32> {
        (0..w * h * 3).map(|i| 1.0 + i as f32 * 0.25).collect()
    }

    /// Runs one reduction over a fresh arena and returns the coarse level.
    fn level_1(src: &[f32], w: usize, h: usize, reduce: LevelReduction) -> Vec<f32> {
        let (dw, dh) = ((w / 2).max(1), (h / 2).max(1));
        let mut arena = src.to_vec();
        arena.resize(w * h * 3 + dw * dh * 3, 0.0);
        reduce(&mut arena, 0, w, h, w * h * 3, dw, dh);
        arena[w * h * 3..].to_vec()
    }

    /// **The triangle reduction is Ptex's, not a 2x2 box.**
    ///
    /// A triangle texture packs two mirrored triangles into its square, so the
    /// fourth sample of a coarse texel is the mirrored one across the
    /// anti-diagonal rather than the neighbour below-right. Averaging the 2x2
    /// block instead mixes texels from both triangles, and the result is not
    /// obviously wrong — it is plausible texture at every coarse level, which
    /// is exactly why this is checked against `ptex-rs` rather than by eye.
    #[test]
    fn triangle_levels_match_ptex_rs_reduction() {
        for w in [2usize, 4, 8, 16] {
            let src = distinct(w, w);
            let ours = level_1(&src, w, w, reduce_triangle);
            assert_eq!(
                ours,
                ptex_rs_reduce(&src, w, w, true),
                "triangle reduction of a {w}x{w} face"
            );
        }
    }

    /// And the two reductions really do disagree, so the test above fails if
    /// the mesh-type branch in `open_with` is ever dropped.
    ///
    /// They coincide at 2x2 — where the mirrored texel *is* the fourth texel —
    /// so the check starts at 4x4, which is the smallest face that can tell
    /// the two apart.
    #[test]
    fn the_quad_reduction_would_be_a_different_answer_on_a_triangle() {
        for w in [4usize, 8, 16] {
            let src = distinct(w, w);
            assert_ne!(
                level_1(&src, w, w, reduce_triangle),
                level_1(&src, w, w, reduce_quad),
                "a {w}x{w} triangle face must not reduce like a quad"
            );
        }
    }

    /// The quad reduction is pinned the same way, so the other branch of the
    /// mesh-type decision is not the untested one.
    #[test]
    fn reduce_matches_ptex_rs_on_a_square_face() {
        for (w, h) in [(2usize, 2usize), (8, 8), (16, 4)] {
            let src = distinct(w, h);
            assert_eq!(
                level_1(&src, w, h, reduce_quad),
                ptex_rs_reduce(&src, w, h, false),
                "quad reduction of a {w}x{h} face"
            );
        }
    }

    /// Once an axis has pinned at one texel, Ptex reduces the other axis alone
    /// and this clamps instead — the same average, reached differently. Pinned
    /// so the divergence stays a deliberate one: `reduce` would produce no rows
    /// at all for `vw == 1`, so there is nothing to compare against there.
    #[test]
    fn a_pinned_axis_averages_along_the_axis_that_is_left() {
        let src = distinct(4, 1);
        let got = level_1(&src, 4, 1, reduce_quad);
        for x in 0..2 {
            for ch in 0..3 {
                let a = src[(2 * x) * 3 + ch];
                let b = src[(2 * x + 1) * 3 + ch];
                assert_eq!(got[x * 3 + ch], 0.5 * (a + b));
            }
        }
    }

    #[test]
    fn level_count_halves_until_both_axes_reach_one() {
        assert_eq!(level_count(1, 1), 1);
        assert_eq!(level_count(32, 32), 6);
        // A non-square face keeps halving the long axis after the short one
        // has pinned at 1 — Ptex's own convention, and the reason the axes
        // are clamped independently at load.
        assert_eq!(level_count(64, 16), 7);
        assert_eq!(level_count(1, 8), 4);
    }

    #[test]
    fn the_level_chain_is_contiguous_and_sums_to_under_four_thirds() {
        let f = Face {
            offset: 100,
            width: 64,
            height: 16,
            levels: level_count(64, 16),
        };
        // Each level starts exactly where the previous one ends.
        let mut expect = 100;
        for k in 0..f.levels as usize {
            let (off, w, h) = f.level(k);
            assert_eq!(off, expect, "level {k}");
            expect += w * h * 3;
        }
        assert_eq!(f.floats(), expect - 100);
        // A *square* pyramid is the 1/4 series and sums under 4/3 of its
        // base. A non-square one is not: once the short axis pins at 1 the
        // ratio becomes 1/2 per level, so 64x16 lands at 1.335x rather than
        // 1.333x. Small, but worth knowing before quoting 4/3 as a bound on a
        // file of 64x16 faces.
        let base = 64 * 16 * 3;
        assert!(f.floats() > base * 4 / 3, "{}", f.floats());
        assert!(f.floats() < base * 7 / 5, "{}", f.floats());
        let square = Face {
            offset: 0,
            width: 32,
            height: 32,
            levels: level_count(32, 32),
        };
        assert!(square.floats() < 32 * 32 * 3 * 4 / 3, "{}", square.floats());
        // Past the coarsest level the lookup clamps rather than running off
        // the end of the arena.
        assert_eq!(f.level(99), f.level(f.levels as usize - 1));
    }

    #[test]
    fn a_wide_footprint_reads_the_mean_and_a_zero_one_reads_the_texels() {
        let t = checker_face(32, 32);
        // The whole face: the 1x1 level, which is the board's average.
        let wide = t.eval(0, 0.5, 0.5, 4.0);
        assert!((wide.x - 0.5).abs() < 1e-5, "{wide}");
        // No footprint: full contrast between two adjacent texel centres,
        // which is the aliasing the pyramid exists to filter.
        let a = t.eval(0, 0.5 / 32.0, 0.5 / 32.0, 0.0);
        let b = t.eval(0, 1.5 / 32.0, 0.5 / 32.0, 0.0);
        assert!((a.x - b.x).abs() > 0.99, "{a} vs {b}");
    }

    #[test]
    fn a_zero_footprint_is_exactly_the_unmipped_lookup() {
        // What `CRUST_PTEX_MIP=0` and `CRUST_RAY_CONES=0` both rely on.
        let mipped = checker_face(16, 16);
        let mut flat = checker_face(16, 16);
        flat.faces[0].levels = 1;
        for i in 0..17 {
            for j in 0..17 {
                let (u, v) = (i as f32 / 16.0, j as f32 / 16.0);
                assert_eq!(mipped.eval(0, u, v, 0.0), flat.eval(0, u, v, 0.0));
            }
        }
    }

    #[test]
    fn selection_is_monotonic_in_the_footprint() {
        // Widening the footprint may flatten the contrast but must never
        // sharpen it — the property that makes the level selection usable.
        let t = checker_face(32, 32);
        let contrast = |w: f32| {
            let a = t.eval(0, 0.5 / 32.0, 0.5 / 32.0, w).x;
            let b = t.eval(0, 1.5 / 32.0, 0.5 / 32.0, w).x;
            (a - b).abs()
        };
        let mut prev = contrast(0.0);
        for k in 0..12 {
            let c = contrast(2f32.powi(k - 6));
            assert!(c <= prev + 1e-6, "width 2^{} raised contrast", k - 6);
            prev = c;
        }
        assert!(prev < 1e-5, "the coarsest level still has contrast: {prev}");
    }

    #[test]
    fn a_missing_face_or_a_non_finite_coordinate_falls_back_rather_than_panicking() {
        let t = checker_face(8, 8);
        assert_eq!(t.eval(7, 0.5, 0.5, 0.0), t.fallback);
        // Non-finite coordinates and widths are the caller's bug, but this
        // runs inside the integrator where a panic kills a worker thread.
        let _ = t.eval(0, f32::NAN, f32::NAN, f32::NAN);
        let _ = t.eval(0, 0.5, 0.5, f32::INFINITY);
        let _ = t.eval(0, -5.0, 5.0, -1.0);
        assert_eq!(t.num_faces(), 1);
    }
}
