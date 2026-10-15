//! [`HashGrid`]: the multi-resolution hash encoding.

use crate::Real;

/// A [`HashGrid`]'s shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridConfig {
    /// Resolution levels, coarsest first.
    pub levels: usize,
    /// Features stored per grid vertex, per level.
    pub features: usize,
    /// Each level's table holds at most `2^log2_entries` vertices; a level
    /// with more is hashed into that many.
    pub log2_entries: u32,
    /// Cells per axis at the coarsest level.
    pub base: u32,
    /// Each level's resolution over the previous one's.
    pub growth: f64,
}

/// One level: its resolution, where its vertices start in the table, and
/// whether they are indexed one to one or hashed.
#[derive(Clone, Copy, Debug)]
struct Level {
    res: u32,
    offset: usize,
    /// Vertices in the level's table: `(res + 1)³` when that fits, else the
    /// power of two `2^log2_entries`.
    entries: u32,
    dense: bool,
}

/// A multi-resolution hash encoding (Müller et al., *Instant Neural Graphics
/// Primitives*, 2022): per level, the trilinear interpolation of the
/// features stored at the eight vertices of the cell holding `x`, the levels'
/// features concatenated. A level whose vertices fit its table indexes them
/// one to one; a finer one hashes them, and lets training sort out the
/// collisions.
///
/// It holds only the layout: the table lives in the network's flat parameter
/// vector, vertex-major (`features` values per vertex), level after level.
#[derive(Clone, Debug)]
pub struct HashGrid {
    config: GridConfig,
    levels: Vec<Level>,
    params: usize,
}

/// The spatial hash's primes (Teschner et al. 2003, as Instant NGP uses
/// them); the first is 1, for coherence along x.
const PRIMES: [u32; 3] = [1, 2_654_435_761, 805_459_861];

impl HashGrid {
    pub fn new(config: GridConfig) -> Self {
        assert!(config.levels > 0 && config.features > 0);
        assert!(config.base > 0 && config.growth >= 1.0);
        assert!(config.log2_entries <= 24, "a 2^24-vertex level is plenty");
        let cap = 1u64 << config.log2_entries;
        let mut offset = 0;
        let levels = (0..config.levels)
            .map(|l| {
                // In f64 and once, at construction: the resolutions are
                // part of the layout, not of the arithmetic.
                let res = (config.base as f64 * config.growth.powi(l as i32)).floor() as u32;
                let vertices = (res as u64 + 1).pow(3);
                let (entries, dense) = if vertices <= cap {
                    (vertices as u32, true)
                } else {
                    (cap as u32, false)
                };
                let level = Level {
                    res,
                    offset,
                    entries,
                    dense,
                };
                offset += entries as usize * config.features;
                level
            })
            .collect();
        HashGrid {
            config,
            levels,
            params: offset,
        }
    }

    pub fn config(&self) -> &GridConfig {
        &self.config
    }

    /// Table entries: the encoding's parameter count.
    pub fn params(&self) -> usize {
        self.params
    }

    /// Encoded features: `levels × features`.
    pub fn outputs(&self) -> usize {
        self.config.levels * self.config.features
    }

    /// Cells per axis at `level`.
    pub fn resolution(&self, level: usize) -> u32 {
        self.levels[level].res
    }

    /// Whether `level` indexes its vertices one to one rather than hashing.
    pub fn is_dense(&self, level: usize) -> bool {
        self.levels[level].dense
    }

    /// The table index of vertex `v`'s first feature at `level`: the
    /// definition [`HashGrid::corners`] computes axis by axis.
    #[cfg(test)]
    fn vertex(&self, level: &Level, v: [u32; 3]) -> usize {
        let entry = if level.dense {
            let side = level.res + 1;
            v[0] + side * (v[1] + side * v[2])
        } else {
            (v[0].wrapping_mul(PRIMES[0])
                ^ v[1].wrapping_mul(PRIMES[1])
                ^ v[2].wrapping_mul(PRIMES[2]))
                & (level.entries - 1)
        };
        level.offset + entry as usize * self.config.features
    }

    /// The eight vertices of the cell holding `x` at `level`, as table
    /// indices, with their trilinear weights. Corner `c` is offset by
    /// `(c & 1, c >> 1 & 1, c >> 2 & 1)`.
    ///
    /// `x` is clamped into the unit cube. On the far face a coordinate
    /// stays in the last cell with a fraction of one, so the face's vertices
    /// carry it, as they do from the inside.
    ///
    /// Per axis first, then per corner: an axis's low and high vertex
    /// contribute one index term and one weight each, so a corner is two
    /// adds (or xors) and two multiplies. Per corner and axis, as the
    /// definition reads, it was a third of an inference's instructions.
    #[inline(always)]
    fn corners<R: Real>(&self, level: &Level, x: [f32; 3]) -> ([usize; 8], [R; 8]) {
        let res = level.res;
        let mut terms = [[0u32; 2]; 3];
        let mut w = [[R::ZERO; 2]; 3];
        // A dense level holds at most 2^24 vertices, so its strides fit.
        let side = res + 1;
        let stride = if level.dense {
            [1, side, side * side]
        } else {
            [0; 3]
        };
        for a in 0..3 {
            let pos = x[a].clamp(0.0, 1.0) * res as f32;
            // `pos` is not negative, so truncation is the floor (and needs
            // no call into the C library, as `floor` does without SSE4.1).
            let cell = (pos as u32).min(res - 1);
            let f = R::from_f32(pos - cell as f32);
            w[a] = [R::ONE - f, f];
            terms[a] = if level.dense {
                [cell * stride[a], (cell + 1) * stride[a]]
            } else {
                [
                    cell.wrapping_mul(PRIMES[a]),
                    (cell + 1).wrapping_mul(PRIMES[a]),
                ]
            };
        }
        let mask = level.entries.wrapping_sub(1);
        let mut index = [0usize; 8];
        let mut weight = [R::ZERO; 8];
        for c in 0..8 {
            let (dx, dy, dz) = (c & 1, (c >> 1) & 1, (c >> 2) & 1);
            let entry = if level.dense {
                terms[0][dx] + terms[1][dy] + terms[2][dz]
            } else {
                (terms[0][dx] ^ terms[1][dy] ^ terms[2][dz]) & mask
            };
            index[c] = level.offset + entry as usize * self.config.features;
            weight[c] = w[0][dx] * w[1][dy] * w[2][dz];
        }
        (index, weight)
    }

    /// Encodes `x` (in the unit cube) into `out`, `levels × features` long:
    /// per level and feature, the weighted sum of the cell's corners in
    /// corner order.
    #[inline]
    pub fn forward<R: Real>(&self, table: &[R], x: [f32; 3], out: &mut [R]) {
        debug_assert_eq!(table.len(), self.params);
        let f = self.config.features;
        if f == 2 {
            // The visibility network's: a pair per vertex, read as one.
            for (level, out) in self.levels.iter().zip(out.as_chunks_mut::<2>().0) {
                let (index, weight) = self.corners::<R>(level, x);
                let (mut o0, mut o1) = (R::ZERO, R::ZERO);
                for (&i, &w) in index.iter().zip(&weight) {
                    let t: &[R; 2] = table[i..i + 2].try_into().expect("a pair");
                    o0 += w * t[0];
                    o1 += w * t[1];
                }
                out[0] = o0;
                out[1] = o1;
            }
            return;
        }
        for (level, out) in self.levels.iter().zip(out.chunks_exact_mut(f)) {
            let (index, weight) = self.corners::<R>(level, x);
            out.fill(R::ZERO);
            for (&i, &w) in index.iter().zip(&weight) {
                for (o, &t) in out.iter_mut().zip(&table[i..i + f]) {
                    *o += w * t;
                }
            }
        }
    }

    /// The backward pass: hands each table entry `x`'s encoding read, with
    /// its share of `dout = ∂L/∂out`, to `scatter(index, ∂L/∂entry)`, level by
    /// level in corner order. A vertex two corners share (a hash collision)
    /// is handed twice.
    #[inline]
    pub fn backward<R: Real>(&self, x: [f32; 3], dout: &[R], mut scatter: impl FnMut(usize, R)) {
        let f = self.config.features;
        for (level, d) in self.levels.iter().zip(dout.chunks_exact(f)) {
            let (index, weight) = self.corners::<R>(level, x);
            for (&i, &w) in index.iter().zip(&weight) {
                for (k, &d) in d.iter().enumerate() {
                    scatter(i + k, w * d);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> HashGrid {
        // 4, 8 and 16 cells per axis: the first two fit their tables of 2^10
        // vertices (125 and 729), the third (4913) is hashed.
        HashGrid::new(GridConfig {
            levels: 3,
            features: 2,
            log2_entries: 10,
            base: 4,
            growth: 2.0,
        })
    }

    fn table(grid: &HashGrid) -> Vec<f32> {
        // Distinct, irregular values, so a wrong index cannot pass.
        (0..grid.params())
            .map(|i| ((i as f32 * 0.618_034).fract() - 0.5) * 2.0)
            .collect()
    }

    #[test]
    fn layout() {
        let g = grid();
        assert_eq!(
            (g.resolution(0), g.resolution(1), g.resolution(2)),
            (4, 8, 16)
        );
        assert!(g.is_dense(0) && g.is_dense(1) && !g.is_dense(2));
        assert_eq!(g.params(), (125 + 729 + 1024) * 2);
        assert_eq!(g.outputs(), 6);
        // The visibility cache's own shape (design D3): finest level 107
        // cells; the four coarsest levels fit their tables, the four finest
        // are hashed into 2^14 vertices; under 1 MiB of f32.
        let d3 = HashGrid::new(GridConfig {
            levels: 8,
            features: 2,
            log2_entries: 14,
            base: 4,
            growth: 1.6,
        });
        assert_eq!(d3.resolution(7), 107);
        assert!((0..8).all(|l| d3.is_dense(l) == (l < 4)));
        assert!(d3.params() * 4 <= 1 << 20);
    }

    /// At a grid vertex the encoding is that vertex's features, exactly, at
    /// every level — the far face included.
    #[test]
    fn exact_at_grid_corners() {
        let g = grid();
        let t = table(&g);
        let mut out = [0.0f32; 6];
        // Multiples of 1/4 are vertices of all three levels, and exact in f32.
        for &x in &[
            [0.0, 0.0, 0.0],
            [0.25, 0.5, 0.75],
            [1.0, 0.25, 1.0],
            [1.0, 1.0, 1.0],
        ] {
            g.forward(&t, x, &mut out);
            for l in 0..3 {
                let level = &g.levels[l];
                let res = level.res as f32;
                let v = x.map(|c| (c * res) as u32);
                let i = g.vertex(level, v);
                assert_eq!(&out[l * 2..l * 2 + 2], &t[i..i + 2], "x {x:?}, level {l}");
            }
        }
    }

    /// `corners` computes, axis by axis, the vertices and weights of the
    /// definition: the cell's eight vertices through `vertex`, and products
    /// of `1 − f` and `f`.
    #[test]
    fn corners_are_the_definition() {
        let g = grid();
        let mut rand = crate::network::test_rand(7);
        for _ in 0..200 {
            let x = [rand(), rand(), rand()];
            for level in &g.levels {
                let (index, weight) = g.corners::<f32>(level, x);
                let res = level.res as f32;
                let cell = x.map(|c| ((c * res).floor() as u32).min(level.res - 1));
                let frac: Vec<f32> = (0..3).map(|a| x[a] * res - cell[a] as f32).collect();
                for c in 0..8 {
                    let up = [c & 1, (c >> 1) & 1, (c >> 2) & 1];
                    let v = [0, 1, 2].map(|a| cell[a] + up[a] as u32);
                    assert_eq!(index[c], g.vertex(level, v));
                    let w = |a: usize| if up[a] == 1 { frac[a] } else { 1.0 - frac[a] };
                    assert_eq!(weight[c].to_bits(), (w(0) * w(1) * w(2)).to_bits());
                }
            }
        }
    }

    /// Crossing a cell face, the encoding moves by no more than its slope
    /// allows: the two cells agree on the face they share.
    #[test]
    fn continuous_across_a_cell_face() {
        let g = grid();
        let t: Vec<f64> = table(&g).iter().map(|&v| v as f64).collect();
        let mut below = [0.0f64; 6];
        let mut above = [0.0f64; 6];
        // x = 0.5 is a face at every level; y and z are anywhere.
        for &(y, z) in &[(0.13, 0.71), (0.5, 0.5), (0.99, 0.02)] {
            for &delta in &[1e-3f32, 1e-5] {
                g.forward(&t, [0.5 - delta, y, z], &mut below);
                g.forward(&t, [0.5 + delta, y, z], &mut above);
                // |∂out/∂x| ≤ res · 2 max|t| at the finest level (16 · 2).
                let bound = 2.0 * delta as f64 * 32.0 * 1.0001;
                for k in 0..6 {
                    assert!(
                        (below[k] - above[k]).abs() <= bound,
                        "feature {k} at y {y}, z {z}, delta {delta}: {} vs {}",
                        below[k],
                        above[k]
                    );
                }
            }
        }
    }

    /// The backward pass is the forward's transpose: the encoding is linear
    /// in the table, so `out · d` equals `Σ table[i] · ∂/∂table[i]`.
    #[test]
    fn backward_is_the_transpose() {
        let g = grid();
        let t: Vec<f64> = table(&g).iter().map(|&v| v as f64).collect();
        let d = [0.3f64, -1.2, 0.7, 2.0, -0.4, 1.1];
        for &x in &[[0.1f32, 0.2, 0.3], [0.77, 0.01, 0.5], [0.999, 0.999, 0.0]] {
            let mut out = [0.0f64; 6];
            g.forward(&t, x, &mut out);
            let lhs: f64 = out.iter().zip(&d).map(|(o, d)| o * d).sum();
            let mut rhs = 0.0f64;
            g.backward(x, &d, |i, g| rhs += t[i] * g);
            assert!((lhs - rhs).abs() < 1e-12, "{lhs} vs {rhs}");
        }
    }
}
