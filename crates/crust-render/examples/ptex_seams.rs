//! Tests the Ptex quad `(u,v)` convention against a file's own adjacency data,
//! numerically — the check a render cannot make for you.
//!
//! # The question
//!
//! Each Ptex face holds an independent little image, and the renderer has to
//! decide which corner of it is `(0,0)`. This assumes Ptex's quad convention —
//! `v0=(0,0) v1=(1,0) v2=(1,1) v3=(0,1)`, with texel row = `v` and column =
//! `u` — but nothing in a rendered image says so out loud. Get it wrong (a
//! transpose, say) and every face is textured with a rotated copy of its own
//! correct data, which still looks like a plausible surface.
//!
//! # The test
//!
//! A `.ptx` records, per face, which face adjoins each of its four edges
//! (`adjface`) and which edge of that neighbour is the shared one (`adjedge`).
//! The texture was baked continuous across those seams. So: walk each shared
//! edge, read the boundary texels from both sides, and see whether they agree.
//!
//! Under the correct convention they should nearly match. Under a transposed
//! one they are sampling unrelated parts of the neighbour's image and should
//! not. Both are computed and compared, alongside the disagreement between
//! *unrelated* faces as a scale for "uncorrelated" — a ratio is meaningless
//! without knowing what chance looks like on this texture.
//!
//! With consistent winding the two sides traverse a shared edge in opposite
//! directions (face A's edge `e` runs `v_e -> v_e+1`, and the neighbour's runs
//! back), so the neighbour's samples are reversed before comparing.
//!
//! ```sh
//! cargo run --release -p crust-render --example ptex_seams -- texture.ptx
//! ```

use std::collections::HashMap;

/// Samples per edge. The seam is a 1-D signal; a handful of points along it is
/// plenty to separate "matches" from "uncorrelated".
const K: usize = 8;
/// Per-face resolution cap (log2), matching the renderer's default load.
const MAX_LOG2: i8 = 5;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: ptex_seams <texture.ptx>");
        std::process::exit(2);
    };
    let mut tx = match ptex::PtexReader::open(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("open failed: {e}");
            std::process::exit(1);
        }
    };
    if tx.mesh_type() != ptex::MeshType::Quad {
        eprintln!("not a quad-mesh file; this test only covers the quad convention");
        std::process::exit(2);
    }
    let n_chan = tx.num_channels();
    let dt = tx.data_type();
    let scale = dt.one_value_inv();
    let n_faces = tx.num_faces();
    println!("{path}\n{n_faces} faces, {n_chan} channels of {}\n", dt.name());

    // Sample faces spread across the mesh rather than the first N, which on a
    // baked cage tend to be one contiguous patch.
    let stride = (n_faces / 500).max(1);
    let mut cache: HashMap<usize, Face> = HashMap::new();

    let mut agree = Stat::default();
    let mut transposed = Stat::default();
    let mut unrelated = Stat::default();
    let mut seams = 0usize;

    for f in (0..n_faces).step_by(stride) {
        let info = match tx.face_info(f) {
            Ok(i) => *i,
            Err(_) => continue,
        };
        for e in 0..4usize {
            let nb = info.adjface(e);
            if nb < 0 || nb as usize >= n_faces {
                continue;
            }
            let nb = nb as usize;
            let ae = info.adjedge(e) as usize;

            let (Some(a), Some(b)) = (
                load(&mut tx, &mut cache, f, dt, n_chan, scale),
                load(&mut tx, &mut cache, nb, dt, n_chan, scale),
            ) else {
                continue;
            };

            // Correct convention, and the transposed alternative, on the same
            // seam — so the two hypotheses see identical data.
            for (stat, swap) in [(&mut agree, false), (&mut transposed, true)] {
                let mine = edge_samples(&a, e, swap);
                let theirs = edge_samples(&b, ae, swap);
                for i in 0..K {
                    stat.add(diff(mine[i], theirs[K - 1 - i]));
                }
            }

            // Chance baseline: same edge against a face that is not its
            // neighbour, picked deterministically far away in the file.
            let far = (f + n_faces / 2 + 7) % n_faces;
            if far != nb
                && far != f
                && let Some(c) = load(&mut tx, &mut cache, far, dt, n_chan, scale)
            {
                let mine = edge_samples(&a, e, false);
                let other = edge_samples(&c, ae, false);
                for i in 0..K {
                    unrelated.add(diff(mine[i], other[K - 1 - i]));
                }
            }
            seams += 1;
        }
    }

    if seams == 0 {
        println!("no shared edges found");
        std::process::exit(1);
    }

    println!("seams tested: {seams}  ({} samples each side)\n", seams * K);
    println!("mean |difference| across a shared edge, 0..1 per channel:");
    println!("  v0=(0,0) convention (what crust uses)   {:.4}", agree.mean());
    println!("  same seam, u/v transposed               {:.4}", transposed.mean());
    println!("  unrelated faces (chance)                {:.4}", unrelated.mean());

    let vs_chance = unrelated.mean() / agree.mean().max(1e-9);
    let vs_transpose = transposed.mean() / agree.mean().max(1e-9);
    println!("\n  chance / convention   = {vs_chance:.2}x");
    println!("  transpose / convention = {vs_transpose:.2}x");

    println!();
    // The convention must beat both alternatives by a clear margin. 1.5x is
    // well outside what texel-resolution mismatch across a seam contributes,
    // and well inside what a genuinely wrong orientation costs.
    if vs_chance > 1.5 && vs_transpose > 1.5 {
        println!(
            "OK — texels are continuous across shared edges under the v0=(0,0) \
             convention, and markedly less so transposed or against chance."
        );
    } else {
        println!(
            "INCONCLUSIVE — the convention does not separate from its \
             alternatives on this texture (too flat, or the mapping is wrong)."
        );
        std::process::exit(1);
    }
}

/// One face's texels at the capped resolution, as linear-ish 0..1 RGB.
struct Face {
    w: usize,
    h: usize,
    rgb: Vec<[f32; 3]>,
}

fn load(
    tx: &mut ptex::PtexReader,
    cache: &mut HashMap<usize, Face>,
    id: usize,
    dt: ptex::DataType,
    n_chan: usize,
    scale: f32,
) -> Option<Face> {
    if let Some(f) = cache.get(&id) {
        return Some(Face {
            w: f.w,
            h: f.h,
            rgb: f.rgb.clone(),
        });
    }
    let info = *tx.face_info(id).ok()?;
    let res = ptex::Res::new(info.res.ulog2.min(MAX_LOG2), info.res.vlog2.min(MAX_LOG2));
    let raw = tx.get_data_at_res(id, res).ok()?;
    let (w, h) = (res.u(), res.v());
    let px = dt.size() * n_chan;
    let mut rgb = Vec::with_capacity(w * h);
    for i in 0..(w * h) {
        let src = &raw[i * px..];
        let mut c = [0.0f32; 3];
        for (ch, out) in c.iter_mut().enumerate() {
            // A single-channel file feeds channel 0 to all three.
            let idx = if ch < n_chan { ch } else { 0 };
            *out = read_channel(&src[idx * dt.size()..], dt) * scale;
        }
        rgb.push(c);
    }
    let f = Face { w, h, rgb };
    let copy = Face {
        w: f.w,
        h: f.h,
        rgb: f.rgb.clone(),
    };
    cache.insert(id, f);
    Some(copy)
}

/// `K` texels along edge `e`, in that edge's canonical direction.
///
/// Ptex numbers a quad's edges `0:(v0,v1) 1:(v1,v2) 2:(v2,v3) 3:(v3,v0)`, which
/// under `v0=(0,0) v1=(1,0) v2=(1,1) v3=(0,1)` puts edge 0 at `v=0`, edge 1 at
/// `u=1`, edge 2 at `v=1` (running backwards) and edge 3 at `u=0` (backwards).
fn edge_samples(f: &Face, e: usize, swap_uv: bool) -> [[f32; 3]; K] {
    let mut out = [[0.0f32; 3]; K];
    for (i, o) in out.iter_mut().enumerate() {
        let t = (i as f32 + 0.5) / K as f32;
        let (u, v) = match e {
            0 => (t, 0.0),
            1 => (1.0, t),
            2 => (1.0 - t, 1.0),
            _ => (0.0, 1.0 - t),
        };
        let (u, v) = if swap_uv { (v, u) } else { (u, v) };
        let x = ((u * f.w as f32) as usize).min(f.w - 1);
        let y = ((v * f.h as f32) as usize).min(f.h - 1);
        *o = f.rgb[y * f.w + x];
    }
    out
}

fn diff(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()) / 3.0
}

#[derive(Default)]
struct Stat {
    sum: f64,
    n: u64,
}

impl Stat {
    fn add(&mut self, v: f32) {
        self.sum += v as f64;
        self.n += 1;
    }
    fn mean(&self) -> f64 {
        if self.n == 0 { 0.0 } else { self.sum / self.n as f64 }
    }
}

fn read_channel(src: &[u8], dt: ptex::DataType) -> f32 {
    match dt {
        ptex::DataType::UInt8 => src[0] as f32,
        ptex::DataType::UInt16 => u16::from_le_bytes([src[0], src[1]]) as f32,
        ptex::DataType::Half => ptex::half_to_float(u16::from_le_bytes([src[0], src[1]])),
        ptex::DataType::Float => f32::from_le_bytes([src[0], src[1], src[2], src[3]]),
    }
}
