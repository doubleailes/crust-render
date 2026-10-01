## Why

The Moana island at subdivision level 1 does not fit in this machine's 61 GiB: the import
was killed at 56.8 GiB RSS, still traversing prims (2026-10-01, `bb53ff2`). At level 0
it peaks at 43.1 GiB, or 37.3 GiB with Ptex streamed, and 26.58 GiB of that is kernel
memory. Much of the kernel memory is the same data stored more than once:

- A triangle's vertices sit in its 64-byte `TrianglePrim` *and* in its `Tri4` packet lane.
- Its shading normals are copied out per triangle (48 B), so each shared vertex's normal
  is stored about six times.
- Every instance and cubic curve span pays an 80-byte `PrimNode` that holds a pointer to
  its real payload.
- An instance's 240-byte payload caches three transforms and a box that traversal does
  not need.

None of this buys speed that has been measured, and level 1 multiplies the triangle
share by about 4.

## What Changes

- **Triangles store their vertices once, in the `Tri4` packets.**
  - The resident per-triangle record keeps only `geom_id`, `prim_id`, mask and its
    three normal-vertex indices: 24 B instead of the 80-byte `PrimNode`.
  - The scalar f64 tie-break re-test and the degenerate-normal fallback read the
    vertices from the packet lane, which holds the exact same `f32`s.
  - The full `TrianglePrim` survives only as a transient build input, because the SBVH
    still clips triangles.
- **Shading normals are indexed.**
  - A BVH keeps each mesh's per-vertex normals once (`[f32; 3]`), and the triangle
    record's index triplet reaches them: about 6 B per triangle on a quad mesh,
    instead of 48.
  - The interpolated normal is computed from the same values, so it is bit-identical.
- **Instances and cubic curves leave `PrimNode`.**
  - The BVH stores primitives in per-kind arrays (triangle records, instances, cubic
    spans, and the remaining analytic primitives), referenced by a kind-tagged index.
    Instances and cubic spans are stored inline: no `Box`, no 80-byte enum slot, and
    about 47 M fewer heap allocations on the island.
- **`InstancePrim` shrinks from 240 to 96 bytes.**
  - It keeps the `Arc<Scene>`, `w2l`, the ids, the mask, and one boxed motion record
    holding `l2w` and `l2w_end`, present only for moving instances.
  - The normal matrix is `w2l`'s transposed linear part, which is exact and so is not
    cached.
  - The bounds are a build input only.
- **`--stats` kernel memory lines** are renamed to the new arrays: `triangle records`,
  `instances`, `cubic curve spans`, `other primitives` and `vertex normals`. They replace `primitive nodes` and `boxed primitives`.
- **Rendered output is bit-identical.** This is a layout change only, with no switch:
  the old layout is the parent commit, and `scripts/check_images.sh` plus `bench_ab.sh`
  against it are the A/B.

Expected effect on the island at level 0, arithmetic from struct sizes × the measured
counts: kernel memory **26.6 → about 13.6 GiB**.

| saving | from | amount |
|---|---|---|
| triangle records and normals | 60.9 M triangles × about 98 B (80 + 48 → 24 + about 6) | about 5.6 GiB |
| instances | 27.6 M × 224 B | 5.8 GiB |
| cubic spans | 19.3 M × 80 B | 1.4 GiB |

The change measures it for real, and also measures whether level 1 then completes under
61 GiB.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `intersection-kernel`: gains a requirement that resident geometry is stored once:
  vertices only in the SIMD packets, normals indexed per mesh, and no per-primitive
  indirection for instances and cubic curves. Rendered output stays bit-identical to
  the layout it replaces.

## Impact

- **`crates/crust-rt`**:
  - `prim.rs`: `TrianglePrim` becomes build-only, and a resident `TriRecord` is added;
    `InstancePrim` is slimmed; `PrimNode` loses `Triangle`, `Instance` and
    `CubicCurve` as resident variants.
  - `bvh/mod.rs`: per-kind arrays, leaf traversal, footprint, breakdown.
  - `bvh/collapse.rs`: packets reference triangle records.
  - `bvh/build.rs`: builds over a transient build-prim array.
  - `triangle.rs`: lane-vertex accessor on `Tri4`.
  - `scene.rs`: commit, normal tables, `MemoryFootprint` fields,
    `describe_instances`.
- **`crates/crust-core/src/stats.rs`**: the kernel memory breakdown labels.
- **Pinned sizes:**
  - `a_triangle_is_one_cache_line`, which pins `TrianglePrim` at 64 and `PrimNode`
    at 80, is rewritten.
  - The `InstancePrim == 240` assertion becomes 96.
- **Bit-identity pairs:** `simd_matches_scalar_bitwise` (packet ↔ scalar) must keep
  holding, and the scalar side now reads packet vertices.
- **Performance:** traversal reads the same packets. A closest hit reads a 24-byte
  record instead of an 80-byte one, and a per-vertex normal through one more index.
  An instance hit recomputes a 3×3 transpose. Expected neutral to slightly faster;
  verified with callgrind and `bench_ab.sh`.
- **Out of scope:**
  - The fully indexed triangle layout (Embree `triangle4i`).
  - Indexing crust-core's per-triangle `FaceMap` / `UvMap` tables.
  - Counting non-kernel memory in `--stats`.
  - Ptex streaming defaults.
  - Screen-space adaptive subdivision.
