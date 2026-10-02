## Context

The layout after `compact-triangle-storage` (#182), in `crates/crust-rt/src/bvh/mod.rs`
and `prim.rs`:

- **`Primitives`**, the build input, and **`Bvh`**, resident, hold the triangle records
  with their vertex, normal and geometry tables, plus `prims: Vec<PrimNode>` for
  everything else, in attach order.
- **`PrimNode` is a 64-byte enum** of sphere, disk, cylinder, linear curve,
  `CubicCurve(Box<CubicCurvePrim>)` (96 B payload) and `Instance(Box<InstancePrim>)`
  (240 B payload).
- **The build's index space** is `0..tris.len()` for records and `tris.len()..` for
  `prims`. References are created in that order, and the order decides ties, so it is
  part of the build's determinism.
- **Leaves** run triangles through packets and keep `indices` into `prims` for the
  rest, dispatched by the out-of-line `PrimNode::hit` / `hit_any`.
- **#182's measured trap:** inlining that dispatch spilled `Bvh::hit` (+6% to +17%
  instructions).

## Goals / Non-Goals

**Goals:**
- Instances at 96 B and cubic spans at 96 B resident, inline, with no slot.
- Bit-identical query results, guaranteed by an unchanged build reference order.
- Traversal instruction count neutral, compared with callgrind.

**Non-Goals:**
- Touching the triangle side: records, tables and packets.
- Shrinking `PrimNode` below 64 B. A linear curve sets that size.
- A 48-byte row-form `w2l`. It is not bit-safe, see the proposal.

## Decisions

### 1. Per-kind arrays behind an unchanged build index space

`Primitives` gains the following, alongside `others: Vec<PrimNode>` (the old `prims`,
now only the four analytic kinds):

```text
instances:       Vec<InstancePrim>     // resident, 96 B
instance_bounds: Vec<AABB>             // build only
cubics:          Vec<CubicCurvePrim>   // resident, 96 B
order:           Vec<u32>              // build only: one kind-tagged id per non-triangle,
                                       // in attach order
```

The build's index space is unchanged: `tris.len() + k` is the `k`-th non-triangle in
attach order. `bbox` and `clipped_aabb` resolve it through `order[k]`.
- The instance bounds come from `instance_bounds`.
- An instance's `clipped_aabb` is the default bbox clip that `Prim::clipped_aabb`
  applies today.

`collapse` writes `order[k]` into the leaf `indices`. `Bvh` keeps `others`,
`instances` and `cubics`; `order` and `instance_bounds` are dropped after the build.

The tag is two bits of kind (other, instance, cubic) and 30 bits of index. `commit`
panics at 2^30 or more primitives of one kind in one BVH, which is about 50× the
island's largest.

- **Alternative: index ranges** (others first, then instances, then cubics). Rejected.
  It would reorder the build's references whenever kinds are interleaved in attach
  order, and the order decides ties. The output could then change, and the
  bit-identity requirement would become a hope instead of a construction.

### 2. `InstancePrim`: 240 → 96 B

```text
InstancePrim { scene: Arc<Scene>, w2l: Affine3A, motion: Option<Box<InstanceMotion>>,
               geom_id, id_offset, mask }
InstanceMotion { l2w: Affine3A, l2w_end: Affine3A }
```

- **`transforms_at`.** A moving instance lerps `l2w → l2w_end` and inverts, exactly as
  today. A static one returns `(w2l, w2l.matrix3.transpose())`. That is the expression
  `normal_mat` was cached from, and a transpose is exact, so the result is unchanged.
- **`Prim` trait.** `InstancePrim` keeps `hit` / `hit_any` as inherent methods. It no
  longer implements `Prim`, because it has no bounds.
- **`describe_instances`** (`traversal-stats` only) recomputes a box from the inner
  scene's bounds through `w2l.inverse()`, or the motion endpoints. It is a diagnostic,
  and documented as approximate.

### 3. Dispatch stays out of line

`Bvh` gains one `#[inline(never)]` pair, `scalar_hit` / `scalar_hit_any`, which decode
the tag and call the kind's method. This keeps #182's finding: the traversal loop must
not absorb curve and instance code.

### 4. Reporting

`MemoryFootprint`:
- `boxed_prims` is replaced by `instances`, which includes the `InstanceMotion` boxes
  of moving instances, and `cubic_spans`;
- `prim_nodes` keeps its meaning for the remaining enum.

The `--stats` labels follow.

## Risks / Trade-offs

- **The transient `order` table** adds 4 B per non-triangle during the build, and
  `instance_bounds` keeps 32 B per instance there.
  → Both are a fraction of the 304 B per instance the box path held, and both are
  dropped once the tree exists.
- **The tag decode on the scalar path** is a shift, a mask and a branch per scalar test.
  → It is out of line already, and is compared with callgrind on cornellbox and on
  instance-heavy `nested_instancing`.
- **The bit-identity claim rests on the order table.**
  → It is pinned by `check_images.sh` over every sample, the Kitchen_set pair, and the
  island and ALab frames, all bit-identical against the parent.

## Migration Plan

Internal layout only, with no switch: the parent commit's binary is the A/B. Rollback
is a revert.
