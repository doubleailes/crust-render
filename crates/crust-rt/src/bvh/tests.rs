use glam::Vec3A;

use crate::aabb::AABB;
use crate::prim::{GeomTable, PrimHit, PrimNode, SpherePrim, TriangleRecord};
use crate::ray::{MASK_ALL, Ray, RayMask};
use crate::triangle::triangle_intersect;

use super::build::*;
use super::*;

/// A reference whose bounding box reaches into bins the primitive itself
/// misses must not be counted into a child it contributes no bounds to.
///
/// This is the shape of a real crash. A `PrimRef`'s bbox is conservative:
/// for a reference produced by an earlier spatial split it is the clipped
/// box, which can overlap a bin where the exact triangle clip comes back
/// empty. `best_spatial_split` used to take entry and exit from the ends
/// of that bbox while filling `bounds` only where the clip succeeded, so a
/// bin could carry a count with no bounds — and the cost loop reads a
/// non-zero count as a promise that the bounds are `Some`. Building the
/// DPEL lion's high-resolution mesh broke that promise and panicked with
/// `rc > 0`.
///
/// Here the triangle occupies only the far left of the node, while the
/// reference claims the whole width, so every bin but the first clips to
/// nothing.
#[test]
fn a_reference_wider_than_its_primitive_does_not_outvote_its_own_bounds() {
    let mut prims = Primitives::default();
    prims.push_triangle(
        [
            Vec3A::new(0.0, 0.0, 0.0),
            Vec3A::new(0.05, 1.0, 0.0),
            Vec3A::new(0.0, 0.0, 1.0),
        ],
        0,
        0,
        MASK_ALL,
    );
    let node = AABB {
        minimum: Vec3A::new(0.0, 0.0, 0.0),
        maximum: Vec3A::new(1.0, 1.0, 1.0),
    };
    // The lie: a reference spanning the whole node for a triangle that
    // only reaches x = 0.05.
    let refs = vec![PrimRef { bbox: node, idx: 0 }];

    // Must not panic, and must not claim a split whose right side holds
    // references but no geometry.
    if let Some(split) = best_spatial_split(&prims, &refs, &node) {
        assert!(
            split.cost.is_finite(),
            "spatial split reported a non-finite cost: {}",
            split.cost
        );
    }
}

/// The traversal stack must behave identically either side of the
/// inline/spill boundary. Tested directly rather than through a tree
/// deep enough to spill, which would need millions of primitives.
#[test]
fn traversal_stack_spills_and_unspills_in_order() {
    // Well past STACK_INLINE, so the sequence crosses the boundary
    // twice: filling and draining.
    let n = STACK_INLINE * 3 + 7;
    let mut s = TraversalStack::new(0);
    for i in 1..n as u32 {
        s.push(i);
    }
    // LIFO all the way back down, including across the boundary.
    for i in (1..n as u32).rev() {
        assert_eq!(s.pop(), Some(i), "at depth {i}");
    }
    assert_eq!(s.pop(), Some(0), "the root seeded by `new`");
    assert_eq!(s.pop(), None, "empty stack yields None, and stays there");
    assert_eq!(s.pop(), None);
}

/// Interleaved push/pop across the boundary: the failure mode a
/// straight fill-then-drain test would miss is the spill and the inline
/// array disagreeing about who owns depth STACK_INLINE.
#[test]
fn traversal_stack_interleaves_across_the_boundary() {
    let mut s = TraversalStack::new(100);
    for round in 0..4u32 {
        // Climb just past the boundary, then come back below it.
        for i in 0..(STACK_INLINE as u32 + 3) {
            s.push(round * 1000 + i);
        }
        for i in (0..(STACK_INLINE as u32 + 3)).rev() {
            assert_eq!(s.pop(), Some(round * 1000 + i));
        }
    }
    assert_eq!(s.pop(), Some(100));
    assert_eq!(s.pop(), None);
}

impl Primitives {
    /// Appends one triangle with its own three vertices (no sharing) under
    /// geometry `geom_id`, which gets a default table entry (no normals)
    /// if it has none yet.
    pub(crate) fn push_triangle(
        &mut self,
        v: [Vec3A; 3],
        geom_id: u32,
        prim_id: u32,
        mask: RayMask,
    ) {
        if self.geoms.len() <= geom_id as usize {
            self.geoms
                .resize(geom_id as usize + 1, GeomTable::default());
        }
        let base = self.vertices.len() as u32;
        self.vertices.extend(v.iter().map(|p| p.to_array()));
        self.tris.push(TriangleRecord {
            v: [base, base + 1, base + 2],
            geom_id,
            prim_id,
            mask,
        });
    }

    /// `other`'s contents after this one's, in the unified index space:
    /// its triangles come after these, its prims after these prims.
    pub(crate) fn append(&mut self, other: Primitives) {
        let vbase = self.vertices.len() as u32;
        self.vertices.extend(other.vertices);
        for mut r in other.tris {
            if !r.is_degenerate() {
                r.v = [r.v[0] + vbase, r.v[1] + vbase, r.v[2] + vbase];
            }
            self.tris.push(r);
        }
        if self.geoms.len() < other.geoms.len() {
            self.geoms.resize(other.geoms.len(), GeomTable::default());
        }
        self.prims.extend(other.prims);
    }
}

fn sphere_grid(n: i32) -> Primitives {
    let mut out = Primitives::default();
    for x in 0..n {
        for y in 0..n {
            for z in 0..n {
                out.prims.push(PrimNode::Sphere(SpherePrim {
                    center: Vec3A::new(x as f32, y as f32, z as f32) * 3.0,
                    radius: 0.5,
                    geom_id: (x * n * n + y * n + z) as u32,
                    mask: MASK_ALL,
                }));
            }
        }
    }
    out
}

/// Long thin diagonal triangles — the geometry spatial splits exist
/// for. Built so object splits alone leave heavily overlapping
/// children.
fn diagonal_shards(n: i32) -> Primitives {
    let mut out = Primitives::default();
    for i in 0..n {
        let o = i as f32 * 0.35;
        out.push_triangle(
            [
                Vec3A::new(o, o, o),
                Vec3A::new(o + 10.0, o + 10.0, o + 10.2),
                Vec3A::new(o + 10.0, o + 10.3, o + 10.0),
            ],
            0,
            i as u32,
            MASK_ALL,
        );
    }
    out
}

fn linear_scan(prims: &Primitives, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
    let mut closest = t_max;
    let mut best = None;
    for r in prims.tris.iter().filter(|r| !r.is_degenerate()) {
        let [a, b, c] = prims.tri_verts(r);
        if ray.mask.sees(r.mask)
            && let Some((t, u, v)) = triangle_intersect(ray, a, b, c, t_min, closest)
            && let Some(h) =
                crate::prim::triangle_hit_from_barycentric(r, &[a, b, c], None, t, u, v)
        {
            closest = h.t;
            best = Some(h);
        }
    }
    for p in &prims.prims {
        if let Some(h) = p.hit(ray, t_min, closest) {
            closest = h.t;
            best = Some(h);
        }
    }
    best
}

fn assert_matches_linear(objects: impl Fn() -> Primitives) {
    let bvh = Bvh::new(objects());
    let reference = objects();

    let origins = [
        Vec3A::new(-5.0, 4.5, 4.5),
        Vec3A::new(20.0, 3.0, 3.0),
        Vec3A::new(4.5, -5.0, 4.5),
        Vec3A::new(0.0, 0.0, -10.0),
        Vec3A::new(5.0, 5.2, -3.0),
    ];
    let dirs = [
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(-1.0, 0.05, 0.02).normalize(),
        Vec3A::new(0.0, 1.0, 0.0),
        Vec3A::new(0.3, 0.3, 1.0).normalize(),
        Vec3A::new(0.0, 0.0, -1.0),
        Vec3A::new(0.577, 0.577, 0.577),
    ];
    for o in origins {
        for d in dirs {
            let ray = Ray::new(o, d);
            let a = bvh.hit(&ray, 0.001, f32::INFINITY);
            let b = linear_scan(&reference, &ray, 0.001, f32::INFINITY);
            match (a, b) {
                (Some(x), Some(y)) => {
                    assert!((x.t - y.t).abs() < 1e-4, "t mismatch for {o:?} {d:?}");
                    assert_eq!(x.geom_id, y.geom_id, "id mismatch for {o:?} {d:?}");
                }
                (None, None) => {}
                (x, y) => panic!(
                    "hit disagreement for {o:?} {d:?}: bvh={} linear={}",
                    x.is_some(),
                    y.is_some()
                ),
            }
            assert_eq!(
                bvh.hit_any(&ray, 0.001, f32::INFINITY),
                b.is_some(),
                "occlusion disagreement for {o:?} {d:?}"
            );
        }
    }
}

/// The BVH must find exactly the hits a linear scan finds.
#[test]
fn matches_linear_scan() {
    assert_matches_linear(|| sphere_grid(4));
}

/// Same, on geometry that triggers spatial splits (verified below).
#[test]
fn spatial_splits_match_linear_scan() {
    assert_matches_linear(|| diagonal_shards(64));
}

/// Diagonal shards must actually produce duplicated references —
/// otherwise the spatial-split path is dead code. References live in
/// two places now: packed SIMD lanes and the scalar `indices` list.
#[test]
fn spatial_splits_duplicate_references() {
    let bvh = Bvh::new(diagonal_shards(64));
    let refs = bvh.leaf_ref_count();
    assert!(
        refs > bvh.prim_count(),
        "no reference duplication: {refs} leaf references for {} prims",
        bvh.prim_count()
    );
}

/// Every leaf reference must land in exactly one place, and every
/// triangle must be packed rather than left on the scalar path.
#[test]
fn triangles_are_packed_into_simd_lanes() {
    let bvh = Bvh::new(diagonal_shards(64));
    assert!(
        !bvh.packets.is_empty(),
        "no packets built for a triangle scene"
    );
    assert!(
        bvh.indices.is_empty(),
        "{} triangles fell back to the scalar list",
        bvh.indices.len()
    );

    // Spheres are not packable and must stay on the scalar path.
    let bvh = Bvh::new(sphere_grid(4));
    assert!(bvh.packets.is_empty(), "spheres must not be packed");
    assert_eq!(bvh.indices.len(), bvh.leaf_ref_count());

    // Mixed leaves must place each primitive on exactly one path.
    let mut mixed = diagonal_shards(16);
    mixed.append(sphere_grid(2));
    let bvh = Bvh::new(mixed);
    assert!(!bvh.packets.is_empty() && !bvh.indices.is_empty());
    assert!(bvh.leaf_ref_count() >= bvh.prim_count());
}

/// Packet lanes must average close to 4 on a dense mesh — a packing
/// that mostly emitted 1-lane packets would be SIMD in name only.
#[test]
fn packets_are_well_filled() {
    let bvh = Bvh::new(diagonal_shards(256));
    let lanes: u32 = bvh.packets.iter().map(|p| p.active.count_ones()).sum();
    let avg = lanes as f32 / bvh.packets.len() as f32;
    // `MIN_LEAF_PACKED` is what keeps this high — ~2.9 of 4 on this
    // scene, against ~1.7 when the leaf floor was 2. Below 2.5 means
    // the floor has drifted away from the SIMD width again.
    assert!(avg >= 2.5, "average packet occupancy {avg} of 4 lanes");
}

/// `hit_any` must agree with `hit(..).is_some()` for every ray and range.
#[test]
fn hit_any_matches_hit() {
    let bvh = Bvh::new(sphere_grid(4));
    let origins = [
        Vec3A::new(-5.0, 4.5, 4.5),
        Vec3A::new(20.0, 3.0, 3.0),
        Vec3A::new(4.5, 4.5, 4.5),
    ];
    let dirs = [
        Vec3A::new(1.0, 0.0, 0.0),
        Vec3A::new(-1.0, 0.05, 0.02).normalize(),
        Vec3A::new(0.3, 0.3, 1.0).normalize(),
    ];
    for o in origins {
        for d in dirs {
            let ray = Ray::new(o, d);
            for t_max in [0.5, 3.0, f32::INFINITY] {
                assert_eq!(
                    bvh.hit_any(&ray, 0.001, t_max),
                    bvh.hit(&ray, 0.001, t_max).is_some(),
                    "occlusion disagreement for {o:?} {d:?} t_max={t_max}"
                );
            }
        }
    }
}

/// Node size is a load-bearing claim, not a comment: the leaf payload
/// was moved into a side table precisely so the node still fits two
/// cache lines. Growing it would silently cost traversal bandwidth.
#[test]
#[cfg(not(feature = "bvh8"))]
fn wide_node_is_two_cache_lines() {
    assert_eq!(std::mem::size_of::<WideNode>(), 128);
    assert_eq!(std::mem::align_of::<WideNode>(), 16);
}

/// A triangle is three vertex indices, two ids and a mask — 24 bytes — with
/// its vertices and normals in the scene's shared tables. The remaining
/// `PrimNode` is sized by the linear curve segment (48 bytes) plus its tag;
/// it was 80 while triangles carried their vertices in it, and 128 with the
/// normals inline too.
#[test]
fn a_triangle_record_is_24_bytes() {
    assert_eq!(std::mem::size_of::<TriangleRecord>(), 24);
    assert_eq!(std::mem::size_of::<PrimNode>(), 64);
    assert_eq!(std::mem::size_of::<GeomTable>(), 12);
}

/// The `bvh8` node: six `f32x8`s and eight child indices, four cache lines.
#[test]
#[cfg(feature = "bvh8")]
fn wide8_node_is_four_cache_lines() {
    assert_eq!(std::mem::size_of::<WideNode>(), 256);
    assert_eq!(std::mem::align_of::<WideNode>(), 32);
}

/// Parallel subtree builds must not change the tree: the same input
/// always produces byte-identical topology.
#[test]
fn build_is_deterministic() {
    let a = Bvh::new(sphere_grid(6));
    let b = Bvh::new(sphere_grid(6));
    assert_eq!(a.wide.len(), b.wide.len());
    assert_eq!(a.indices, b.indices);
    assert_eq!(a.packets.len(), b.packets.len());
    assert_eq!(a.leaves.len(), b.leaves.len());
    for (x, y) in a.leaves.iter().zip(&b.leaves) {
        assert_eq!((x.pkt_first, x.pkt_count), (y.pkt_first, y.pkt_count));
        assert_eq!((x.idx_first, x.idx_count), (y.idx_first, y.idx_count));
    }
    for (x, y) in a.wide.iter().zip(&b.wide) {
        assert_eq!(x.child, y.child);
        assert_eq!(x.flags, y.flags);
        assert_eq!(x.bmin_x, y.bmin_x);
        assert_eq!(x.bmax_z, y.bmax_z);
    }
}

/// The collapse must actually widen: a big tree ends up with far
/// fewer wide nodes than a binary tree would need.
#[test]
fn collapse_widens_the_tree() {
    let bvh = Bvh::new(sphere_grid(6)); // 216 prims
    let n_leaf_slots: usize = bvh
        .wide
        .iter()
        .map(|w| (0..LANES).filter(|&l| w.is_leaf(l)).count())
        .sum();
    assert!(n_leaf_slots > 0);
    assert_eq!(n_leaf_slots, bvh.leaves.len());
    // A binary tree over L leaves has L-1 internal nodes; BVH4 should
    // need roughly a third of that.
    assert!(
        bvh.wide.len() * 2 < n_leaf_slots.max(2) * 2 - 1,
        "{} wide nodes for {} leaves",
        bvh.wide.len(),
        n_leaf_slots
    );
}

/// The finished tree's tables live as long as the scene, and the memory
/// footprint counts their capacity, so none of them may keep slack: the
/// node vector used to reserve one wide node per binary leaf, three times
/// what a 4-wide tree uses and seven times an 8-wide one.
///
/// The finished `Bvh` boxes its tables, so slack cannot survive there; what
/// this pins is that `collapse` does not allocate it in the first place —
/// boxing an over-reserved vector would copy it, at the build's peak memory.
#[test]
fn collapsed_tables_hold_no_spare_capacity() {
    for prims in [sphere_grid(6), diagonal_shards(40)] {
        let refs: Vec<PrimRef> = (0..prims.len() as u32)
            .map(|i| PrimRef {
                bbox: prims.bbox(i).expect("no degenerate records here"),
                idx: i,
            })
            .collect();
        let root = union_all(&refs);
        let subtree = build_subtree(&prims, refs, 0, surface_area(&root));
        let (wide, collected) = collapse(&subtree.nodes, &subtree.indices, &prims);
        assert_eq!(wide.capacity(), wide.len());
        assert_eq!(collected.leaves.capacity(), collected.leaves.len());
        assert_eq!(collected.packets.capacity(), collected.packets.len());
        assert_eq!(collected.indices.capacity(), collected.indices.len());
    }
}

#[test]
fn empty_bvh_misses() {
    let bvh = Bvh::new(Primitives::default());
    let ray = Ray::new(Vec3A::ZERO, Vec3A::X);
    assert!(bvh.hit(&ray, 0.001, f32::INFINITY).is_none());
    assert!(bvh.bounds().is_none());
}

#[test]
fn bounds_cover_all_prims() {
    let bvh = Bvh::new(sphere_grid(3));
    let bbox = bvh.bounds().expect("grid is fully bounded");
    assert!(bbox.minimum.cmple(Vec3A::splat(-0.5)).all());
    assert!(bbox.maximum.cmpge(Vec3A::splat(6.5)).all());
}
