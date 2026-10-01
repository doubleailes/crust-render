## Context

See `proposal.md` (Why) for the island measurements. The resident layout today, from
`crates/crust-rt/src/bvh/mod.rs` and `prim.rs`:

- `Bvh::prims: Box<[PrimNode]>` holds every primitive in input order. `PrimNode` is an
  80-byte enum sized by `TrianglePrim` (64 B: three `Vec3A` vertices, `geom_id`,
  `prim_id`, mask, normal index), so every kind pays 80 B.
- `Instance` and `CubicCurve` are boxed variants, holding a 240 B `InstancePrim` and a
  96 B `CubicCurvePrim`. Each is its own heap allocation.
- `Bvh::normals: Box<[[Vec3A; 3]]>` holds one normal triple per triangle, copied out of
  the mesh at commit (`SceneBuilder::commit`).
- `Bvh::packets: Box<[Tri4]>` is 192 B per four triangle *references*. SBVH spatial
  splits can list a triangle in several leaves, so a triangle can sit in several
  packets. Each lane already holds the triangle's exact `f32` vertices, and `prim[lane]`
  indexes `prims`.
- Traversal reads `prims` in three places only:
  - the closest-hit tail (`hit_from_barycentric`: ids, plus the normal or, failing that,
    the geometric normal from the vertices);
  - the scalar f64 tie-break re-test for lanes whose edge function is exactly `0.0`;
  - the scalar `indices` of non-triangle primitives.
- The SBVH build (`build.rs`) needs full triangles, for `bbox`, `clipped_aabb` and the
  all-triangles `MIN_LEAF_PACKED` test.
- `primitive_extent_sum` and the `traversal-stats` `describe_instances` read bounds from
  the resident primitives.

Constraints:
- Safe Rust (`forbid(unsafe_code)` in crust-rt).
- `crust-rt` stays free of crust types.
- Every query result must stay bit-identical: `simd_matches_scalar_bitwise`,
  `check_images.sh` at 16 spp.

## Goals / Non-Goals

**Goals:**
- Resident primitive storage with no duplicated vertex positions or normals, and no
  per-primitive pointer slot or heap allocation for instances and cubic spans.
- Unchanged query results, bit for bit, on every SIMD codegen
  (`scripts/test_simd_matrix.sh`) and the nightly `bvh8` leg.
- Traversal cost neutral or better, verified by instruction count.

**Non-Goals:**
- Lowering the commit's *transient* peak. The build keeps its current inputs. Only what
  survives `commit()` shrinks.
- Changing `Tri4`'s layout or lane width, or quantising nodes or normals. Lossy
  encodings change output and belong to a separate change.
- The per-triangle tables in crust-core (`FaceMap`, `UvMap`).

## Decisions

### 1. Build over a transient `BuildPrim`, then split into resident per-kind arrays

`SceneBuilder::commit` keeps producing today's full primitives, renamed `BuildPrim` and
still carrying triangle vertices for the SBVH. After `collapse`, a `finish` step moves
them into the resident `Prims`, and the `BuildPrim` array is dropped before `commit`
returns.

```text
Prims {
    tris:      Box<[TriRecord]>,      // 24 B, referenced by Tri4::prim[lane]
    instances: Box<[InstancePrim]>,   // 96 B, inline
    cubics:    Box<[CubicCurvePrim]>, // 96 B, inline
    others:    Box<[OtherPrim]>,      // sphere, disk, cylinder, linear curve segment
    normals:   Box<[[f32; 3]]>,       // per normal-carrying vertex
}
```

Each kind keeps its input order, so the per-kind index is a pure function of the input
and the build stays deterministic.

- **Alternative: keep one enum and only shrink `TriangleVariant`.** Rejected. The enum
  would then be sized by the next largest inline variant (a linear curve, 64 B), so
  triangles would still pay 64 B. Instances and cubic spans would still pay a slot plus
  a box.
- **Alternative: change the build to work from packets.** Rejected. Spatial clipping
  wants whole triangles, and the build is transient anyway.

### 2. Packets are the only home of triangle positions

`Tri4` gains `lane_vertices(lane) -> (Vec3A, Vec3A, Vec3A)`, which reassembles the
lane's `v[i][axis][lane]`. The two scalar consumers take vertices from it:

- **`TriRecord::hit_from_barycentric(normals, verts, t, u, v)`**. The geometric-normal
  fallback for triangles without shading normals uses `verts`.
- **The tie-break re-test.** `triangle_intersect(ray, v0, v1, v2, …)` is called with the
  lane vertices.

These are the same `f32` values the `TrianglePrim` held: `Tri4::new` copies them and
`Vec4` ↔ `Vec3A` lane moves are exact. So the scalar path's inputs, and its outputs,
are unchanged. `simd_matches_scalar_bitwise` keeps pinning the pair. The `bvh8`
feature widens nodes only, so its leaves run the same `Tri4` packets and need nothing
extra.

The tail-lane padding repeats the last real triangle. It is never read, because only
lanes in `active` reach either consumer.

### 3. Normals: per-vertex table, index triplet inline in the record

`TriRecord { geom_id: u32, prim_id: u32, mask: RayMask, n: [u32; 3] }` is 24 B.

- `n` indexes `Prims::normals`.
- `n[0] == NO_NORMALS` means flat.
- At commit, a mesh with normals appends its normal array once, as `[f32; 3]` (12 B
  rather than 16 B `Vec3A`). Each triangle stores `base + i0`, `base + i1`, `base + i2`.
- The existing per-triangle validity check (all three indices within the normal array)
  still decides flat versus smooth per triangle.

Interpolation converts to `Vec3A` and evaluates the same expression in the same order,
so the result is bit-identical.

- **Alternative: a 16 B record pointing at a separate `[u32; 3]` table.** Rejected. That
  is 28 B per smooth triangle rather than 24, plus a second dependent load. Flat
  triangles would save 8 B, but on the production scenes nearly every triangle is
  smooth: at level 0 every subdivision cage is.
- **Alternative: a 4 B mesh-local base plus the mesh's own index buffer.** Rejected. It
  needs a per-geometry table lookup, and the mesh index buffer would then have to stay
  resident.

### 4. `InstancePrim`: 240 → 96 B

```text
InstancePrim {
    scene: Arc<Scene>,
    w2l: Affine3A,                  // static world-to-local, exactly as cached today
    motion: Option<Box<Motion>>,    // Motion { l2w, l2w_end }, moving instances only
    geom_id, id_offset, mask: u32,
}
```

- **The normal matrix.** It is `w2l.matrix3.transpose()`, computed per hit. A transpose
  is a lane permutation, so it is exact, and it is what the motion path already does.
- **Bounds.** These live in the `BuildPrim` only.
- **Diagnostics that read bounds.**
  - `primitive_extent_sum` gets a `(count, sum, max)` computed once at commit from the
    build references and stored on the `Bvh`.
  - `traversal-stats`' `describe_instances` recomputes a box from the inner scene's
    bounds and `w2l.inverse()`. This is a diagnostic only, and is documented as
    approximate.
- **Alternative: store `w2l` as three `Vec4` rows (48 B, giving 80 B total).** Rejected
  for now. `Affine3A::transform_point3a` and a hand-written row form need not agree to
  the bit, which would break the bit-identity requirement. It can be revisited with an
  image A/B.

### 5. Cubic spans inline, unchanged

`CubicCurvePrim` keeps its 96 B layout and moves from `Box` into `Prims::cubics`.
Packing the radii into the control points' `w` lanes would save 16 B per span, but it
changes the intersector's inputs, so it is left out.

### 6. Leaf references

- Triangles are reached only through packets, via `prim[lane]` into `tris`, so a
  packet's `prim` field keeps its meaning.
- A leaf's scalar `indices` now hold a kind-tagged `u32`: two bits of kind (instance,
  cubic, other) and 30 bits of index.
- `commit` panics with a clear message if one BVH holds 2^30 or more primitives of one
  non-triangle kind. That is 30× the island's largest count.
- Dispatch is a `match` on the tag, as on the enum discriminant today.

### 7. Reporting

`MemoryFootprint` changes its fields:
- `prim_nodes` and `boxed_prims` become `triangle_records`, `instances`, `cubic_spans`
  and `other_prims`.
- `vertex_normals` keeps its name and its meaning: resident normal bytes.

The `--stats` breakdown in `crust-core/src/stats.rs` prints the new labels.
`accumulate_footprint` keeps descending into each distinct instanced scene once. The
`Box` payload accounting disappears, because nothing is boxed except `Motion`, and
that is counted under `instances`.

## Risks / Trade-offs

- **Splitting prims by kind touches every traversal path** (closest hit, occlusion,
  `bvh8`), and a mistake can still give a plausible image.
  → Mitigation: `check_images.sh check` over every sample at 16 spp must be
  bit-identical, alongside the simd matrix and the nightly `bvh8` tests.
- **The commit's transient peak grows briefly**: the resident arrays are allocated
  while the `BuildPrim` array is still alive.
  → Mitigation: fill and drop kind by kind. The resident arrays are a fraction of the
  build array, and the measured peak, not just the resident total, is reported in the
  island A/B.
- **A closest hit with smooth normals takes one more dependent load**: record → three
  normal reads, where a contiguous triple was read before.
  → Mitigation: measure with callgrind on cornellbox and `bench_ab.sh` on the island
  and ALab. The record shrinking from 80 to 24 B should more than pay for it.
- **An instance hit now pays a 3×3 transpose.** It is three shuffles, negligible next
  to the inner traversal, and it is checked in the same A/B.
- **The 2^30 per-kind cap** is a new failure mode. It is far above any measured scene,
  and the panic message names the kind and the count.

## Migration Plan

This is an internal layout change with no switch: the A/B baseline is the parent
commit's binary.
- Record goldens with the parent: `check_images.sh record`.
- Check them with the change: `check_images.sh check`.
- Time the two binaries with `bench_ab.sh`.

Rollback is a revert.
