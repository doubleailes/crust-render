//! Scalar displacement of a tessellated mesh.
//!
//! Pure: local-space points, polygon topology, per-vertex charts and a
//! [`Displacement`] in, displaced points and shading normals out. No USD, no
//! texture loading — the samplers are resolved before this runs — so it may
//! fan out over rayon without touching the import's thread-local time. See
//! "Displacement" in `openspec/specs/usd-scene-import/design.md`.
//!
//! Two rules carry the whole design:
//!
//! - **Every unique vertex moves once.** Positions are shared between the
//!   faces that meet at them, and each is displaced exactly once, from one
//!   *owner* corner — the first face corner that references it in face order.
//!   The displaced mesh therefore has the undisplaced mesh's connectivity and
//!   is watertight exactly where it was, across UV seams and Ptex face
//!   boundaries alike, where sampling per corner would tear.
//! - **The footprint is the dicing rate.** A lookup's width is the longer of
//!   the two chart edges meeting at the owner corner, so a coarse
//!   tessellation reads a coarse mip level — the map low-passed at the rate
//!   it is being sampled — instead of aliasing its finest texels.

use crate::material::{Displacement, VertexCtx};
use crate::scene::subdiv::smooth_normals;
use glam::Vec3A;
use rayon::prelude::*;

/// One unique vertex's chart coordinates, taken from its owner corner, with
/// their footprints. `owned` is false for a point no face references, which
/// is left where it is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct VertexChart {
    pub owned: bool,
    pub uv: Option<[f32; 2]>,
    pub uv_width: f32,
    pub ptex: Option<(u32, [f32; 2])>,
    pub ptex_width: f32,
}

/// A chart's coordinate at face-vertex `fv` whose point is `point`.
pub(crate) type UvAt<'a> = &'a dyn Fn(usize, usize) -> Option<[f32; 2]>;

/// Corner `k` of face `face` as a Ptex face and its coordinate there.
pub(crate) type PtexAt<'a> = &'a dyn Fn(usize, usize) -> Option<(u32, [f32; 2])>;

/// Builds every vertex's [`VertexChart`] from its owner corner.
///
/// `uv(fv, point)` answers the texture coordinate at face-vertex `fv` (the
/// running offset into `indices`) whose point is `point` — a `faceVarying`
/// chart reads the first, a `vertex` chart the second. `ptex(face, k)` answers
/// corner `k` of face `face` as a Ptex face and a coordinate in its unit
/// square, or `None` where the face is not Ptex-addressable. Either is `None`
/// when the displacement does not read that chart.
///
/// Sequential and in face order, so the owner of a vertex is a property of
/// the authored mesh — never of thread count or import order.
pub(crate) fn owner_charts(
    counts: &[i32],
    indices: &[i32],
    n_points: usize,
    uv: Option<UvAt<'_>>,
    ptex: Option<PtexAt<'_>>,
) -> Vec<VertexChart> {
    let mut charts = vec![VertexChart::default(); n_points];
    let mut off = 0usize;
    for (face, &fc) in counts.iter().enumerate() {
        let fc = fc.max(0) as usize;
        if fc < 3 || off + fc > indices.len() {
            off += fc;
            continue;
        }
        for k in 0..fc {
            let Ok(p) = usize::try_from(indices[off + k]) else {
                continue;
            };
            if p >= n_points || charts[p].owned {
                continue;
            }
            let (prev, next) = ((k + fc - 1) % fc, (k + 1) % fc);
            let mut c = VertexChart {
                owned: true,
                ..VertexChart::default()
            };
            if let Some(uv) = uv {
                let at = |j: usize| {
                    let q = usize::try_from(indices[off + j]).ok()?;
                    uv(off + j, q)
                };
                c.uv = at(k);
                if let Some(here) = c.uv {
                    c.uv_width = spacing(here, at(prev), at(next));
                }
            }
            if let Some(ptex) = ptex {
                c.ptex = ptex(face, k);
                if let Some((f, here)) = c.ptex {
                    // A neighbour corner only measures the spacing when it
                    // lies in the same face's chart.
                    let same = |j: usize| ptex(face, j).filter(|n| n.0 == f).map(|n| n.1);
                    c.ptex_width = spacing(here, same(prev), same(next));
                }
            }
            charts[p] = c;
        }
        off += fc;
    }
    charts
}

/// The longer of the two chart edges leaving `here`.
fn spacing(here: [f32; 2], a: Option<[f32; 2]>, b: Option<[f32; 2]>) -> f32 {
    let len = |o: Option<[f32; 2]>| {
        o.map_or(0.0, |o| {
            ((o[0] - here[0]).powi(2) + (o[1] - here[1]).powi(2)).sqrt()
        })
    };
    len(a).max(len(b))
}

/// A displaced mesh.
pub(crate) struct Displaced {
    pub points: Vec<[f32; 3]>,
    /// Smooth normals of the displaced surface when the source had shading
    /// normals; `None` for a faceted source, which stays faceted.
    pub normals: Option<Vec<[f32; 3]>>,
    /// The largest `|offset|` applied, in local units.
    pub max_abs: f32,
    /// Vertices displaced (every owned one).
    pub vertices: u64,
}

/// Vertices per rayon task: fixed, so the work split never depends on the
/// pool — though each vertex is independent, so the result could not anyway.
const CHUNK: usize = 4096;

/// Moves every owned vertex along its unit pre-displacement normal by the
/// displacement's value there, then recomputes shading normals.
///
/// `normals` are the tessellation's own (limit, refined-smooth or smooth cage
/// normals); a faceted source passes `None` and gets area-weighted smooth
/// normals computed here, for the direction only — a face normal per face
/// would send the two sides of an edge two ways and crack it. An offset of
/// exactly zero leaves its point bit-identical (adding `0.0` would turn a
/// `-0.0` coordinate into `+0.0`).
pub(crate) fn displace(
    points: &[[f32; 3]],
    counts: &[i32],
    indices: &[i32],
    normals: Option<&[[f32; 3]]>,
    charts: &[VertexChart],
    d: &Displacement,
) -> Displaced {
    debug_assert_eq!(points.len(), charts.len());
    let computed;
    let dirs: &[[f32; 3]] = match normals {
        Some(n) if n.len() == points.len() => n,
        _ => {
            computed = smooth_normals(points, counts, indices);
            &computed
        }
    };

    let mut out = points.to_vec();
    let mut offsets = vec![0.0f32; points.len()];
    out.par_chunks_mut(CHUNK)
        .zip(offsets.par_chunks_mut(CHUNK))
        .enumerate()
        .for_each(|(chunk, (out, offsets))| {
            let base = chunk * CHUNK;
            for (i, (p, off)) in out.iter_mut().zip(offsets.iter_mut()).enumerate() {
                let v = base + i;
                let c = &charts[v];
                if !c.owned {
                    continue;
                }
                let n = Vec3A::from_array(dirs[v]).normalize_or_zero();
                let position = Vec3A::from_array(*p);
                let ctx = VertexCtx {
                    uv: c.uv,
                    uv_width: c.uv_width,
                    ptex: c.ptex,
                    ptex_width: c.ptex_width,
                    position,
                    normal: n,
                };
                let h = d.eval(&ctx);
                if h != 0.0 && n != Vec3A::ZERO {
                    *p = (position + n * h).to_array();
                    *off = h.abs();
                }
            }
        });

    let max_abs = offsets.iter().copied().fold(0.0f32, f32::max);
    let vertices = charts.iter().filter(|c| c.owned).count() as u64;
    let normals = normals.map(|_| smooth_normals(&out, counts, indices));
    Displaced {
        points: out,
        normals,
        max_abs,
        vertices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::{DisplacementValue, VertexCtx};
    use std::sync::Arc;

    /// A cube of edge 2 centred on the origin, six outward quads.
    fn cube() -> (Vec<[f32; 3]>, Vec<i32>, Vec<i32>) {
        let p = vec![
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        let idx = vec![
            0, 3, 2, 1, // -z
            4, 5, 6, 7, // +z
            0, 1, 5, 4, // -y
            3, 7, 6, 2, // +y
            0, 4, 7, 3, // -x
            1, 2, 6, 5, // +x
        ];
        (p, vec![4; 6], idx)
    }

    /// An `n x n` quad grid over `[-1, 1]^2` in XZ, facing +Y, with a vertex
    /// chart `uv = ((x + 1) / 2, (z + 1) / 2)`.
    fn grid(n: usize) -> (Vec<[f32; 3]>, Vec<i32>, Vec<i32>) {
        let mut p = Vec::new();
        for j in 0..=n {
            for i in 0..=n {
                let x = -1.0 + 2.0 * i as f32 / n as f32;
                let z = -1.0 + 2.0 * j as f32 / n as f32;
                p.push([x, 0.0, z]);
            }
        }
        let mut idx = Vec::new();
        let at = |i: usize, j: usize| (j * (n + 1) + i) as i32;
        for j in 0..n {
            for i in 0..n {
                // Counter-clockwise seen from +Y.
                idx.extend([at(i, j), at(i, j + 1), at(i + 1, j + 1), at(i + 1, j)]);
            }
        }
        (p, vec![4; n * n], idx)
    }

    fn constant(c: f32) -> Displacement {
        Displacement::new(DisplacementValue::Constant(c))
    }

    fn charts_of(counts: &[i32], idx: &[i32], n: usize) -> Vec<VertexChart> {
        owner_charts(counts, idx, n, None, None)
    }

    #[test]
    fn a_constant_moves_every_smooth_cube_vertex_along_its_normal() {
        let (p, c, i) = cube();
        let normals = smooth_normals(&p, &c, &i);
        let charts = charts_of(&c, &i, p.len());
        let out = displace(&p, &c, &i, Some(&normals), &charts, &constant(0.1));
        for (v, (a, b)) in p.iter().zip(&out.points).enumerate() {
            let want = Vec3A::from_array(*a) + Vec3A::from_array(normals[v]).normalize() * 0.1;
            assert_eq!(*b, want.to_array(), "vertex {v}");
            let moved = (Vec3A::from_array(*b) - Vec3A::from_array(*a)).length();
            assert!((moved - 0.1).abs() < 1e-6, "vertex {v} moved {moved}");
        }
        assert_eq!(out.max_abs, 0.1);
        assert_eq!(out.vertices, 8);
        // A cube's corner normals are its diagonals, so the displaced cube is
        // the same cube scaled about its centre: the normals do not change.
        let n = out.normals.expect("smooth source keeps smooth normals");
        for (a, b) in normals.iter().zip(&n) {
            assert!((Vec3A::from_array(*a) - Vec3A::from_array(*b)).length() < 1e-6);
        }
    }

    #[test]
    fn a_faceted_source_moves_along_smooth_directions_and_stays_faceted() {
        let (p, c, i) = cube();
        let charts = charts_of(&c, &i, p.len());
        let out = displace(&p, &c, &i, None, &charts, &constant(0.1));
        assert!(out.normals.is_none());
        // The direction is the smooth (diagonal) normal, so shared corners
        // move together and the faces stay connected.
        let s = 0.1 / 3f32.sqrt();
        assert!((out.points[6][0] - (1.0 + s)).abs() < 1e-6);
        assert!((out.points[0][2] - (-1.0 - s)).abs() < 1e-6);
    }

    #[test]
    fn a_zero_displacement_leaves_positions_bit_identical() {
        let (mut p, c, i) = cube();
        p[0][0] = -0.0;
        p[1] = [-0.0, -0.0, -0.0];
        let normals = smooth_normals(&p, &c, &i);
        let charts = charts_of(&c, &i, p.len());
        let out = displace(&p, &c, &i, Some(&normals), &charts, &constant(0.0));
        let bits =
            |v: &[[f32; 3]]| -> Vec<u32> { v.iter().flatten().map(|x| x.to_bits()).collect() };
        assert_eq!(bits(&out.points), bits(&p));
        assert_eq!(out.max_abs, 0.0);
    }

    #[test]
    fn a_ridges_flank_normals_tilt() {
        let n = 16;
        let (p, c, i) = grid(n);
        let normals = smooth_normals(&p, &c, &i);
        for nv in &normals {
            assert_eq!(*nv, [0.0, 1.0, 0.0]);
        }
        // A tent along z: 0.5 at x = 0, falling to 0 at |x| = 1.
        let ridge = Displacement::new(DisplacementValue::Field {
            field: Arc::new(|c: &VertexCtx| 0.5 * (1.0 - c.position.x.abs())),
            scale: 1.0,
        });
        let charts = charts_of(&c, &i, p.len());
        let out = displace(&p, &c, &i, Some(&normals), &charts, &ridge);
        let nn = out.normals.expect("smooth");
        // A vertex on each flank, off the ridge line and the border.
        let at = |i: usize, j: usize| j * (n + 1) + i;
        let left = Vec3A::from_array(nn[at(4, 8)]);
        let right = Vec3A::from_array(nn[at(12, 8)]);
        // Left flank rises toward +x, so its normal leans toward -x.
        assert!(left.x < -0.3, "left flank normal {left}");
        assert!(right.x > 0.3, "right flank normal {right}");
        assert!((left.x + right.x).abs() < 1e-5, "symmetric");
        assert!((out.max_abs - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_owner_is_the_first_corner_and_its_footprint_the_longer_edge() {
        // Two quads sharing an edge, with a face-varying chart that differs
        // across it (a seam): the shared points take the first face's values.
        let p = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
            [2.0, 0.0, 0.0],
            [2.0, 0.0, 1.0],
        ];
        let counts = [4, 4];
        let idx = [0, 1, 2, 3, 1, 4, 5, 2];
        // Face 0 spans u in [0, 0.5], face 1 is charted elsewhere: [0.75, 1].
        let fv_uv = [
            [0.0, 0.0],
            [0.5, 0.0],
            [0.5, 0.25],
            [0.0, 0.25],
            [0.75, 0.0],
            [1.0, 0.0],
            [1.0, 0.25],
            [0.75, 0.25],
        ];
        let uv = |fv: usize, _p: usize| Some(fv_uv[fv]);
        let charts = owner_charts(&counts, &idx, p.len(), Some(&uv), None);
        assert_eq!(charts[1].uv, Some([0.5, 0.0]), "owned by face 0");
        assert_eq!(charts[2].uv, Some([0.5, 0.25]), "owned by face 0");
        assert_eq!(charts[4].uv, Some([1.0, 0.0]));
        // Corner 1 of face 0: edges to (0, 0) and (0.5, 0.25) — 0.5 is longer.
        assert_eq!(charts[1].uv_width, 0.5);
        assert!(charts.iter().all(|c| c.owned));
    }

    #[test]
    fn ptex_footprints_stay_inside_one_face() {
        let (p, c, i) = grid(2);
        // Each grid quad is its own Ptex face with the standard corners.
        const Q: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let ptex = |f: usize, k: usize| Some((f as u32, Q[k]));
        let charts = owner_charts(&c, &i, p.len(), None, Some(&ptex));
        // The centre vertex (index 4) is first referenced by face 0's corner
        // 2, at (1, 1) of face 0.
        assert_eq!(charts[4].ptex, Some((0, [1.0, 1.0])));
        assert_eq!(charts[4].ptex_width, 1.0);
    }

    #[test]
    fn the_result_does_not_depend_on_the_thread_count() {
        // Large enough to span several chunks.
        let n = 120;
        let (p, c, i) = grid(n);
        let normals = smooth_normals(&p, &c, &i);
        let uv = |_: usize, q: usize| Some([(p[q][0] + 1.0) * 0.5, (p[q][2] + 1.0) * 0.5]);
        let charts = owner_charts(&c, &i, p.len(), Some(&uv), None);
        let wave = Displacement::new(DisplacementValue::Field {
            field: Arc::new(|c: &VertexCtx| {
                let [u, v] = c.uv.unwrap();
                (u * 37.0).sin() * (v * 23.0).cos() * 0.1 + c.uv_width
            }),
            scale: 1.0,
        });
        assert!(p.len() > 3 * CHUNK);
        let run = || displace(&p, &c, &i, Some(&normals), &charts, &wave);
        let one = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(run);
        let many = run();
        let bits =
            |v: &[[f32; 3]]| -> Vec<u32> { v.iter().flatten().map(|x| x.to_bits()).collect() };
        assert_eq!(bits(&one.points), bits(&many.points));
        assert_eq!(
            bits(one.normals.as_ref().unwrap()),
            bits(many.normals.as_ref().unwrap())
        );
        assert_eq!(one.max_abs.to_bits(), many.max_abs.to_bits());
    }
}
