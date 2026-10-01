use glam::Vec3A;

use crate::ray::MASK_ALL;

use super::*;

fn uv_sphere_prims(segs: usize, rings: usize) -> Primitives {
    let mut v = Vec::new();
    for r in 0..=rings {
        let phi = (r as f32 / rings as f32) * std::f32::consts::PI;
        for s in 0..=segs {
            let th = (s as f32 / segs as f32) * std::f32::consts::TAU;
            v.push(Vec3A::new(
                phi.sin() * th.cos(),
                phi.cos(),
                phi.sin() * th.sin(),
            ));
        }
    }
    let row = segs + 1;
    let mut out = Primitives::default();
    let mut id = 0u32;
    for r in 0..rings {
        for s in 0..segs {
            let (a, b, c, d) = (
                r * row + s,
                r * row + s + 1,
                (r + 1) * row + s + 1,
                (r + 1) * row + s,
            );
            for (i, j, k) in [(a, b, c), (a, c, d)] {
                out.push_triangle([v[i], v[j], v[k]], 0, id, MASK_ALL);
                id += 1;
            }
        }
    }
    out
}

/// Records *why* the packets are 4 wide and not 8.
///
/// The obvious next step from a 4-wide leaf intersector is an 8-wide one
/// (AVX2). It would buy nothing here: with `MIN_LEAF_PACKED` at 4 and
/// the SAH free to split above it, no leaf on a dense mesh holds more
/// than four triangles, so a leaf already costs exactly one 4-lane
/// round — and one 8-lane round, with half the lanes idle. This test
/// asserts that equality, so if a future retune makes leaves bigger
/// (raising `MIN_LEAF_PACKED`, or `MAX_LEAF` with a cheaper leaf cost)
/// it fails and says the trade-off has changed.
#[test]
fn eight_wide_packets_would_not_reduce_vector_rounds() {
    let bvh = Bvh::new(uv_sphere_prims(80, 40), Layout::Gathered);
    let per_leaf: Vec<usize> = bvh
        .leaves
        .iter()
        .map(|leaf| {
            bvh.packets[leaf.pkt_first as usize..(leaf.pkt_first + leaf.pkt_count) as usize]
                .iter()
                .map(|p| p.lanes.active.count_ones() as usize)
                .sum()
        })
        .collect();

    let max = per_leaf.iter().copied().max().unwrap_or(0);
    assert!(max > 0, "no triangles ended up in leaves");
    assert!(
        max <= 4,
        "a leaf holds {max} triangles — 8-wide packets are now worth evaluating"
    );

    let rounds4: usize = per_leaf.iter().map(|n| n.div_ceil(4)).sum();
    let rounds8: usize = per_leaf.iter().map(|n| n.div_ceil(8)).sum();
    assert_eq!(
        rounds4, rounds8,
        "8-wide packets would cut vector rounds {rounds4} -> {rounds8}; \
         widening the leaf intersector is worth revisiting"
    );
}
