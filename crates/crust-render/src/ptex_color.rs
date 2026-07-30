//! Host-side Ptex decoding: the CLI's implementation of
//! [`crust_core::PtexTexture`], on top of the pure-Rust `ptex` reader.
//!
//! # Why the whole file is read up front
//!
//! `ptex::PtexReader` takes `&mut self` for every pixel read and caches only
//! level indexes, not pixel data — so a texel lookup means a seek, a read and
//! possibly a zlib inflate. A path tracer asks for texels from every Rayon
//! worker, millions of times per frame, in an order nothing can predict. Going
//! back to the file per lookup would be both a lock-contention disaster and
//! orders of magnitude too slow, so every face is decoded once at load time
//! into an immutable buffer that threads then share without synchronisation.
//!
//! # Why it is not read at full resolution
//!
//! Production Ptex is authored for close-ups: the island's
//! `isLavaRocks/Color/rockfacemain0001_geo.ptx` is 11 384 faces over 631 MB
//! compressed, which at full resolution inflates to several GB — for an asset
//! that covers a few hundred thousand pixels in the reference framing. Faces
//! are therefore loaded at the coarsest mip level that still exceeds the
//! resolution the render can resolve, capped by [`DEFAULT_MAX_LOG2`]. Ptex
//! files carry stored mipmaps and the reader computes any level they lack, so
//! this costs nothing but a smaller read.
//!
//! `CRUST_PTEX_MAX_LOG2` overrides the cap as a log2 edge length (`5` → 32×32
//! per face). Raise it for a close-up, lower it to cut memory.

use crust_core::{PtexTexture, Vec3A};
use std::path::Path;

/// Default per-face resolution cap, as a log2 edge length: 32×32 texels.
///
/// A quad covering `n` pixels on screen cannot show more than about `n` texels,
/// and the island's meshes are dense — `mountain_geo` is 33 503 quads, so in a
/// 595×520 framing a quad lands on a handful of pixels. 32×32 is already far
/// past what such a framing resolves, while keeping a 3 272-face texture near
/// 10 MB instead of a gigabyte.
const DEFAULT_MAX_LOG2: i8 = 5;

/// One face's texels within [`PtexColor::texels`].
struct Face {
    offset: u32,
    width: u16,
    height: u16,
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
        let mut tx = ptex::PtexReader::open(path).map_err(|e| e.to_string())?;

        let n_chan = tx.num_channels();
        if n_chan == 0 {
            return Err("file has no channels".into());
        }
        let dt = tx.data_type();
        let scale = dt.one_value_inv();
        let max_log2 = max_log2_from_env();

        let n_faces = tx.num_faces();
        let mut faces = Vec::with_capacity(n_faces);
        let mut texels: Vec<f32> = Vec::new();

        for faceid in 0..n_faces {
            let info = *tx.face_info(faceid).map_err(|e| e.to_string())?;
            // Clamp each axis independently: Ptex faces are frequently
            // non-square (64x16 is common) and clamping the pair together
            // would distort the aspect the file chose.
            let res = ptex::Res::new(
                info.res.ulog2.min(max_log2),
                info.res.vlog2.min(max_log2),
            );
            let (w, h) = (res.u(), res.v());
            let offset = texels.len() as u32;
            faces.push(Face {
                offset,
                width: w as u16,
                height: h as u16,
            });
            texels.resize(texels.len() + w * h * 3, 0.0);

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
        }

        Ok(PtexColor {
            faces,
            texels,
            fallback: Vec3A::splat(0.5),
        })
    }

    /// Bytes held, for the load-time report.
    pub fn bytes(&self) -> usize {
        self.texels.len() * std::mem::size_of::<f32>()
            + self.faces.len() * std::mem::size_of::<Face>()
    }

    #[inline]
    fn texel(&self, f: &Face, x: usize, y: usize) -> Vec3A {
        let i = f.offset as usize + (y * f.width as usize + x) * 3;
        Vec3A::new(self.texels[i], self.texels[i + 1], self.texels[i + 2])
    }
}

impl PtexTexture for PtexColor {
    fn eval(&self, face_id: u32, u: f32, v: f32) -> Vec3A {
        let Some(f) = self.faces.get(face_id as usize) else {
            return self.fallback;
        };
        let (w, h) = (f.width as usize, f.height as usize);
        if w == 0 || h == 0 {
            return self.fallback;
        }

        // Bilinear with clamped borders — every island colour file authors
        // `uBorderMode`/`vBorderMode = clamp`. Filtering across face
        // boundaries (what a real `PtexFilter` does with the adjacency data)
        // is not attempted: at these face resolutions the seam is far below a
        // pixel, and getting adjacency edge-rotations wrong is worse than not
        // filtering across at all.
        let fu = if u.is_finite() { u.clamp(0.0, 1.0) } else { 0.0 };
        let fv = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };

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
            .texel(f, x0i, y0i)
            .lerp(self.texel(f, x1i, y0i), tx);
        let bot = self
            .texel(f, x0i, y1i)
            .lerp(self.texel(f, x1i, y1i), tx);
        top.lerp(bot, ty)
    }

    fn num_faces(&self) -> usize {
        self.faces.len()
    }
}

/// Reads one channel of Ptex data as an unnormalized float.
#[inline]
fn read_channel(src: &[u8], dt: ptex::DataType) -> f32 {
    match dt {
        ptex::DataType::UInt8 => src[0] as f32,
        ptex::DataType::UInt16 => u16::from_le_bytes([src[0], src[1]]) as f32,
        ptex::DataType::Half => ptex::half_to_float(u16::from_le_bytes([src[0], src[1]])),
        ptex::DataType::Float => f32::from_le_bytes([src[0], src[1], src[2], src[3]]),
    }
}

fn max_log2_from_env() -> i8 {
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
