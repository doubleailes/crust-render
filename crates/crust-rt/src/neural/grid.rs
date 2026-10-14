//! The multi-resolution hash-grid encoding (Müller et al. 2022, Instant NGP):
//! [`LEVELS`] grids over the unit cube, resolutions growing geometrically from
//! 16 to 1 024 cells per axis, each storing [`FEATURES`] trainable values per
//! vertex in a table of [`TABLE`] entries. A coarse level whose vertices fit
//! the table is indexed densely; every finer one through the spatial hash, so
//! colliding vertices share an entry and training resolves the collision.
//!
//! A point reads, per level, the eight vertices of its cell and blends their
//! features trilinearly. The backward pass hands the same eight weights back
//! as sparse gradient entries, which is what lets the trainer merge them in a
//! fixed order.

/// Levels of the encoding.
pub(crate) const LEVELS: usize = 8;
/// Entries per level (2^14).
pub(crate) const TABLE: usize = 1 << 14;
/// Features stored per entry.
pub(crate) const FEATURES: usize = 2;
/// Features one point produces: one blend per level and feature.
pub(crate) const OUT: usize = LEVELS * FEATURES;

/// Cells per axis at each level: `floor(16 · b^l)` with `b = (1024 / 16)^(1/7)`,
/// written out so no `powf` decides the grid (`resolutions_are_geometric`
/// checks the table against the formula).
pub(crate) const RES: [u32; LEVELS] = [16, 28, 52, 95, 172, 312, 565, 1024];

/// Instant NGP's spatial-hash primes (the first is 1, so `x` is not mixed).
const PRIMES: [u32; 3] = [1, 2_654_435_761, 805_459_861];

/// The trainable table: `LEVELS · TABLE · FEATURES` values, level-major,
/// then entry, then feature.
pub(crate) struct HashGrid {
    pub(crate) params: Vec<f32>,
}

/// Where one level's eight corners of a point's cell live in the table, and
/// how much each contributes.
struct Corners {
    /// Index of each corner's first feature in `params`.
    at: [usize; 8],
    weight: [f32; 8],
}

/// Table entry of vertex `(x, y, z)` at level `l`, in `0..TABLE`.
#[inline]
fn entry(l: usize, x: u32, y: u32, z: u32) -> usize {
    let side = RES[l] + 1;
    if (side as usize).pow(3) <= TABLE {
        (x + side * (y + side * z)) as usize
    } else {
        ((x.wrapping_mul(PRIMES[0]) ^ y.wrapping_mul(PRIMES[1]) ^ z.wrapping_mul(PRIMES[2]))
            as usize)
            & (TABLE - 1)
    }
}

/// The corners of `p`'s cell at level `l`. `p` is clamped into the unit cube,
/// so every corner is a real vertex and every entry is in the table.
#[inline]
fn corners(l: usize, p: [f32; 3]) -> Corners {
    let res = RES[l];
    let mut cell = [0u32; 3];
    let mut frac = [0f32; 3];
    for a in 0..3 {
        let x = p[a].clamp(0.0, 1.0) * res as f32;
        let c = (x as u32).min(res - 1);
        cell[a] = c;
        frac[a] = x - c as f32;
    }
    let mut at = [0usize; 8];
    let mut weight = [0f32; 8];
    for c in 0..8 {
        let (dx, dy, dz) = ((c & 1) as u32, ((c >> 1) & 1) as u32, ((c >> 2) & 1) as u32);
        let w = |d: u32, f: f32| if d == 1 { f } else { 1.0 - f };
        weight[c] = w(dx, frac[0]) * w(dy, frac[1]) * w(dz, frac[2]);
        at[c] = (l * TABLE + entry(l, cell[0] + dx, cell[1] + dy, cell[2] + dz)) * FEATURES;
    }
    Corners { at, weight }
}

impl HashGrid {
    /// A table of `LEVELS · TABLE · FEATURES` values from `init`, called once
    /// per value in order.
    pub(crate) fn new(mut init: impl FnMut() -> f32) -> Self {
        HashGrid {
            params: (0..LEVELS * TABLE * FEATURES).map(|_| init()).collect(),
        }
    }

    /// The encoding of `p` (unit-cube coordinates) into `out`.
    #[inline]
    pub(crate) fn encode(&self, p: [f32; 3], out: &mut [f32]) {
        for l in 0..LEVELS {
            let k = corners(l, p);
            let mut acc = [0f32; FEATURES];
            for c in 0..8 {
                for (f, a) in acc.iter_mut().enumerate() {
                    *a += k.weight[c] * self.params[k.at[c] + f];
                }
            }
            out[l * FEATURES..(l + 1) * FEATURES].copy_from_slice(&acc);
        }
    }

    /// The gradient of the table given `dout`, the gradient of
    /// [`HashGrid::encode`]'s output at `p`: one `(parameter, gradient)`
    /// entry per corner and feature, appended to `sink` in a fixed order.
    pub(crate) fn backward(p: [f32; 3], dout: &[f32], sink: &mut Vec<(u32, f32)>) {
        for l in 0..LEVELS {
            let k = corners(l, p);
            for c in 0..8 {
                for f in 0..FEATURES {
                    sink.push(((k.at[c] + f) as u32, k.weight[c] * dout[l * FEATURES + f]));
                }
            }
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        std::mem::size_of_val(&*self.params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolutions_are_geometric() {
        let b = (1024.0f64 / 16.0).powf(1.0 / 7.0);
        for (l, &r) in RES.iter().enumerate() {
            assert_eq!(
                r,
                (16.0 * b.powi(l as i32) + 1e-6).floor() as u32,
                "level {l}"
            );
        }
    }

    #[test]
    fn every_lookup_reads_within_the_table() {
        let mut rng = openqmc::pcg::Rng::new(7);
        let edge = [0.0, 1.0, -0.5, 1.5, 0.999_999_9, f32::MIN_POSITIVE];
        for i in 0..20_000 {
            let p = if i < 216 {
                [edge[i % 6], edge[(i / 6) % 6], edge[(i / 36) % 6]]
            } else {
                [rng.next_f32(), rng.next_f32(), rng.next_f32()]
            };
            for l in 0..LEVELS {
                let k = corners(l, p);
                for c in 0..8 {
                    assert!(k.at[c] + FEATURES <= LEVELS * TABLE * FEATURES);
                    assert!(
                        k.at[c] >= l * TABLE * FEATURES && k.at[c] < (l + 1) * TABLE * FEATURES
                    );
                }
                let s: f32 = k.weight.iter().sum();
                assert!((s - 1.0).abs() < 1e-5, "weights sum to {s}");
            }
        }
    }

    #[test]
    fn backward_matches_finite_differences() {
        let mut rng = openqmc::pcg::Rng::new(11);
        let mut grid = HashGrid::new(|| rng.next_f32() - 0.5);
        let mut prng = openqmc::pcg::Rng::new(3);
        // A scalar loss: a fixed weighted sum of the outputs.
        let coef: Vec<f32> = (0..OUT).map(|i| 0.25 + 0.1 * i as f32).collect();
        let loss = |g: &HashGrid, p: [f32; 3]| {
            let mut out = [0f32; OUT];
            g.encode(p, &mut out);
            out.iter()
                .zip(&coef)
                .map(|(o, c)| (o * c) as f64)
                .sum::<f64>()
        };
        for _ in 0..20 {
            let p = [prng.next_f32(), prng.next_f32(), prng.next_f32()];
            let mut sink = Vec::new();
            HashGrid::backward(p, &coef, &mut sink);
            // Collisions within a point's own corners add up.
            let mut grad = std::collections::BTreeMap::<u32, f32>::new();
            for (i, g) in &sink {
                *grad.entry(*i).or_default() += g;
            }
            for (&i, &g) in grad.iter().take(16) {
                let h = 1e-2;
                let v = grid.params[i as usize];
                grid.params[i as usize] = v + h;
                let up = loss(&grid, p);
                grid.params[i as usize] = v - h;
                let down = loss(&grid, p);
                grid.params[i as usize] = v;
                let fd = ((up - down) / (2.0 * h as f64)) as f32;
                assert!(
                    (fd - g).abs() < 1e-3,
                    "param {i}: analytic {g}, numeric {fd}"
                );
            }
        }
    }
}
