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

/// Records what 8-wide packets would buy, now that leaves can hold two.
///
/// Until the packet-aware leaf cost (`CommitOptions::packet_sah`) no leaf on
/// a dense mesh held more than four triangles, so an 8-wide leaf intersector
/// (AVX2) would have run exactly as many vector rounds as the 4-wide one with
/// half its lanes idle — the equality this test used to pin. Leaves of five to
/// eight triangles now exist where splitting them would have made two
/// half-empty packets, and on those an 8-wide packet would merge two rounds
/// into one. The saving is bounded by the share of such leaves, which this
/// test measures and bounds; the reasons the kernel stays at 128 bits
/// (`docs/simd.md`: nightly-only `std::simd`, and in-cache BVH8 measured
/// slower) are unchanged by it.
#[test]
fn eight_wide_packets_would_save_at_most_the_two_packet_leaves() {
    let bvh = Bvh::new(uv_sphere_prims(80, 40), Layout::Gathered, true);
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
        max <= MAX_LEAF,
        "a leaf holds {max} triangles, past MAX_LEAF"
    );

    let rounds4: usize = per_leaf.iter().map(|n| n.div_ceil(4)).sum();
    let rounds8: usize = per_leaf.iter().map(|n| n.div_ceil(8)).sum();
    let two_packet = per_leaf.iter().filter(|&&n| n > 4).count();
    let saving = 1.0 - rounds8 as f64 / rounds4 as f64;
    println!(
        "leaves {} (two-packet {two_packet}), rounds 4-wide {rounds4} / 8-wide {rounds8}: \
         8-wide would save {:.1}%",
        per_leaf.len(),
        100.0 * saving
    );
    assert!(rounds8 <= rounds4);
    assert_eq!(
        rounds4 - rounds8,
        two_packet,
        "8-wide packets save exactly one round per two-packet leaf"
    );
}
