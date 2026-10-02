//! Per-face tessellation of a subdivision surface's Ptex quads.
//!
//! Pure: four edge rates in, `(u, v)` points and triangles out. The caller
//! (`subdiv`) evaluates the points on the limit surface and shares the corner
//! and edge points between the faces that meet there, which is what makes the
//! tessellation watertight: a point on a face's boundary is always one its
//! edge chose, never one the interior grid put there. See "Per-face
//! tessellation" in `openspec/specs/usd-scene-import/design.md`.
//!
//! A Ptex quad's corners are `(0,0)`, `(1,0)`, `(1,1)`, `(0,1)`, and its edges
//! run counter-clockwise between them: edge 0 along `v = 0`, edge 1 along
//! `u = 1`, edge 2 along `v = 1` (toward `u = 0`), edge 3 along `u = 0` (toward
//! `v = 0`). Triangles wind counter-clockwise in `(u, v)`.

/// Where a point of a face's tessellation comes from, so the caller can share
/// it with the faces it borders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointKey {
    /// Corner `0..4`.
    Corner(u8),
    /// The `i`-th of the `n − 1` inner points of edge `edge`, counted from the
    /// edge's counter-clockwise start (`1 ≤ i < n`).
    Edge { edge: u8, i: u32 },
    /// Inside the face: belongs to this face alone.
    Interior,
}

/// One Ptex quad's tessellation.
#[derive(Clone, Debug, Default)]
pub(crate) struct QuadTessellation {
    pub(crate) points: Vec<[f32; 2]>,
    pub(crate) keys: Vec<PointKey>,
    /// Counter-clockwise in `(u, v)`. The interior grid's come first, then
    /// from [`QuadTessellation::stitched_from`] on the stitched rings'.
    pub(crate) tris: Vec<[u32; 3]>,
    pub(crate) stitched_from: usize,
}

/// An edge's segment count: `segments` is its projected length over the target
/// (`ℓ · σ / t`), rounded up and clamped to `1..=2^max_level`. An edge an
/// `n`-gon's Ptex quads split at its midpoint (`even`) is rounded up to an even
/// count of at least 2 — even past the ceiling, since those quads need the
/// midpoint whatever the level — so each half gets the same whole number.
pub(crate) fn edge_rate(segments: f32, max_level: u32, even: bool) -> u32 {
    let cap = 1u32 << max_level.min(16);
    let n = if segments.is_nan() || segments <= 1.0 {
        1
    } else if segments >= cap as f32 {
        cap
    } else {
        segments.ceil() as u32
    };
    if even {
        n.max(2).next_multiple_of(2)
    } else {
        n
    }
}

/// Tessellates a Ptex quad whose edges are split into `rates[e]` segments
/// (each at least 1).
///
/// The interior is a grid of `max(rates[0], rates[2])` by
/// `max(rates[1], rates[3])` cells. The ring between its outer row and each
/// edge's own points is triangulated by merging the two point sequences,
/// always taking the shorter diagonal, so a fine interior meets a coarse edge
/// without a T-junction. A face whose grid would be one cell wide is
/// triangulated directly between its two long sides.
pub(crate) fn tessellate_quad(rates: [u32; 4]) -> QuadTessellation {
    let rates = rates.map(|n| n.max(1));
    let mut t = QuadTessellation::default();
    let corner = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let corners: Vec<u32> = (0..4)
        .map(|c| push(&mut t, corner[c], PointKey::Corner(c as u8)))
        .collect();
    // Each edge's points from its start corner to its end corner, inclusive.
    let sides: Vec<Vec<u32>> = (0..4)
        .map(|e| {
            let n = rates[e];
            let (a, b) = (corner[e], corner[(e + 1) % 4]);
            let mut side = vec![corners[e]];
            for i in 1..n {
                let s = i as f32 / n as f32;
                let p = [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s];
                side.push(push(&mut t, p, PointKey::Edge { edge: e as u8, i }));
            }
            side.push(corners[(e + 1) % 4]);
            side
        })
        .collect();
    let rev = |s: &Vec<u32>| s.iter().rev().copied().collect::<Vec<u32>>();

    let nu = rates[0].max(rates[2]);
    let nv = rates[1].max(rates[3]);
    if nu == 1 {
        // Edges 0 and 2 are single segments: a strip from the left side up to
        // the right one.
        t.stitched_from = 0;
        stitch(&mut t, &sides[1], &rev(&sides[3]));
        return t;
    }
    if nv == 1 {
        t.stitched_from = 0;
        stitch(&mut t, &sides[0], &rev(&sides[2]));
        return t;
    }

    // The interior grid, `g(i, j)` at `(i / nu, j / nv)` for `1 ≤ i < nu`,
    // `1 ≤ j < nv`.
    let (w, h) = (nu - 1, nv - 1);
    let mut grid = Vec::with_capacity((w * h) as usize);
    for j in 1..nv {
        for i in 1..nu {
            let p = [i as f32 / nu as f32, j as f32 / nv as f32];
            grid.push(push(&mut t, p, PointKey::Interior));
        }
    }
    let g = |i: u32, j: u32| grid[((j - 1) * w + (i - 1)) as usize];
    for j in 1..nv - 1 {
        for i in 1..nu - 1 {
            let (a, b, c, d) = (g(i, j), g(i + 1, j), g(i + 1, j + 1), g(i, j + 1));
            t.tris.push([a, b, c]);
            t.tris.push([a, c, d]);
        }
    }
    t.stitched_from = t.tris.len();
    // The grid's outer row along each side, in that side's direction.
    let inner: [Vec<u32>; 4] = [
        (1..nu).map(|i| g(i, 1)).collect(),
        (1..nv).map(|j| g(nu - 1, j)).collect(),
        (1..nu).rev().map(|i| g(i, nv - 1)).collect(),
        (1..nv).rev().map(|j| g(1, j)).collect(),
    ];
    for e in 0..4 {
        stitch(&mut t, &sides[e], &inner[e]);
    }
    t
}

fn push(t: &mut QuadTessellation, p: [f32; 2], key: PointKey) -> u32 {
    t.points.push(p);
    t.keys.push(key);
    (t.points.len() - 1) as u32
}

/// Triangulates the strip between `outer` and `inner`, two point sequences
/// running the same way with `inner` on `outer`'s left, by always taking the
/// shorter of the two possible next diagonals (ties advance `inner`).
fn stitch(t: &mut QuadTessellation, outer: &[u32], inner: &[u32]) {
    let d2 = |a: u32, b: u32| {
        let (p, q) = (t.points[a as usize], t.points[b as usize]);
        (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)
    };
    let (mut i, mut j) = (0usize, 0usize);
    while i + 1 < outer.len() || j + 1 < inner.len() {
        let advance_outer = j + 1 == inner.len()
            || (i + 1 < outer.len() && d2(outer[i + 1], inner[j]) < d2(outer[i], inner[j + 1]));
        if advance_outer {
            t.tris.push([outer[i], outer[i + 1], inner[j]]);
            i += 1;
        } else {
            t.tris.push([outer[i], inner[j + 1], inner[j]]);
            j += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn area(t: &QuadTessellation, tri: [u32; 3]) -> f32 {
        let [a, b, c] = tri.map(|k| t.points[k as usize]);
        0.5 * ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]))
    }

    /// Every check of task 2.3, for one rate tuple.
    fn check(rates: [u32; 4]) {
        let t = tessellate_quad(rates);
        let what = format!("rates {rates:?}");
        // Counter-clockwise, and together exactly the unit square.
        let mut total = 0.0f64;
        for &tri in &t.tris {
            let a = area(&t, tri);
            assert!(a > 0.0, "{what}: triangle {tri:?} is not counter-clockwise");
            total += f64::from(a);
        }
        assert!((total - 1.0).abs() < 1e-5, "{what}: area {total}");
        // Manifold: an interior edge in exactly two triangles, once each way;
        // a boundary edge in exactly one.
        let mut directed: HashMap<(u32, u32), u32> = HashMap::new();
        for &[a, b, c] in &t.tris {
            for e in [(a, b), (b, c), (c, a)] {
                *directed.entry(e).or_default() += 1;
            }
        }
        let on_boundary = |k: u32| t.keys[k as usize] != PointKey::Interior;
        for (&(a, b), &n) in &directed {
            assert_eq!(n, 1, "{what}: edge {a}->{b} used {n} times one way");
            let twin = directed.contains_key(&(b, a));
            let p = t.points[a as usize];
            let q = t.points[b as usize];
            let along_side = on_boundary(a)
                && on_boundary(b)
                && ((p[0] == q[0] && (p[0] == 0.0 || p[0] == 1.0))
                    || (p[1] == q[1] && (p[1] == 0.0 || p[1] == 1.0)));
            assert_eq!(
                twin, !along_side,
                "{what}: edge {p:?}->{q:?} boundary/interior mismatch"
            );
        }
        // The boundary is exactly the chosen edge points: no other point lies
        // on the square's border, and each edge has its `n − 1` inner points.
        for (k, p) in t.points.iter().enumerate() {
            let border = p[0] == 0.0 || p[0] == 1.0 || p[1] == 0.0 || p[1] == 1.0;
            assert_eq!(border, on_boundary(k as u32), "{what}: point {p:?}");
        }
        for e in 0..4u8 {
            let n = t
                .keys
                .iter()
                .filter(|k| matches!(k, PointKey::Edge { edge, .. } if *edge == e))
                .count();
            assert_eq!(n as u32, rates[e as usize] - 1, "{what}: edge {e}");
        }
        // Euler for a disk: T = 2·V_inside + V_border − 2.
        let inside = t.keys.iter().filter(|k| **k == PointKey::Interior).count();
        let border = t.points.len() - inside;
        assert_eq!(
            t.tris.len(),
            2 * inside + border - 2,
            "{what}: triangle count"
        );
    }

    #[test]
    fn every_rate_tuple_up_to_eight_is_watertight_and_covers_the_face() {
        for a in 1..=8 {
            for b in 1..=8 {
                for c in 1..=8 {
                    for d in 1..=8 {
                        check([a, b, c, d]);
                    }
                }
            }
        }
        for rates in [
            [16, 1, 16, 1],
            [1, 16, 1, 16],
            [16, 16, 1, 1],
            [3, 16, 7, 2],
        ] {
            check(rates);
        }
    }

    #[test]
    fn a_face_split_once_per_edge_is_two_triangles() {
        let t = tessellate_quad([1, 1, 1, 1]);
        assert_eq!(t.points.len(), 4);
        assert_eq!(t.tris.len(), 2);
    }

    #[test]
    fn a_uniform_rate_is_a_regular_grid() {
        for n in [2u32, 4, 8] {
            let t = tessellate_quad([n; 4]);
            assert_eq!(t.points.len() as u32, (n + 1) * (n + 1));
            assert_eq!(t.tris.len() as u32, 2 * n * n);
        }
    }

    #[test]
    fn edge_points_follow_the_edge_counter_clockwise() {
        let t = tessellate_quad([4, 2, 4, 2]);
        let at = |edge: u8, i: u32| {
            let k = t
                .keys
                .iter()
                .position(|k| *k == PointKey::Edge { edge, i })
                .expect("edge point");
            t.points[k]
        };
        assert_eq!(at(0, 1), [0.25, 0.0]);
        assert_eq!(at(1, 1), [1.0, 0.5]);
        assert_eq!(at(2, 1), [0.75, 1.0], "edge 2 runs toward u = 0");
        assert_eq!(at(3, 1), [0.0, 0.5]);
    }

    #[test]
    fn edge_rates_round_up_and_clamp() {
        assert_eq!(edge_rate(0.3, 3, false), 1);
        assert_eq!(edge_rate(1.0, 3, false), 1);
        assert_eq!(edge_rate(1.0001, 3, false), 2);
        assert_eq!(edge_rate(5.2, 3, false), 6);
        assert_eq!(edge_rate(100.0, 3, false), 8, "clamped to 2^max");
        assert_eq!(edge_rate(f32::INFINITY, 3, false), 8);
        assert_eq!(edge_rate(f32::NAN, 3, false), 1);
        // Monotone in the projected length.
        let mut last = 0;
        for k in 0..200 {
            let n = edge_rate(k as f32 * 0.07, 4, false);
            assert!(n >= last);
            last = n;
        }
    }

    #[test]
    fn edges_next_to_an_ngon_are_even() {
        assert_eq!(edge_rate(0.3, 3, true), 2, "at least the midpoint");
        assert_eq!(edge_rate(2.5, 3, true), 4);
        assert_eq!(edge_rate(4.0, 3, true), 4);
        assert_eq!(edge_rate(100.0, 3, true), 8);
        assert_eq!(edge_rate(0.3, 0, true), 2, "the midpoint even at ceiling 0");
    }
}
