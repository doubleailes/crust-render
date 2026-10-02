## Why

`compact-triangle-storage` (#182) stores each triangle once, and deferred one lever in
its design (§ Deferred, item 3): **the instance primitive**. Today an instance costs a
64-byte `PrimNode` slot plus a 240-byte box. The box caches `l2w`, `w2l`, `normal_mat`
(which is `w2l`'s transposed linear part), the world bounds and a motion pointer. A
cubic curve span is likewise a slot plus a 96-byte box.

On the Moana island these boxed payloads are the largest remaining kernel cost: 27.6 M
instances and 19.3 M cubic spans. The island at subdivision level 1 still does not fit
in 61 GiB.

## What Changes

- **Instances move out of `PrimNode` into an inline array of their own.** No box and no
  enum slot. Static instances keep only what traversal reads:
  - the inner scene and `w2l`;
  - ids and mask;
  - an `Option<Box<…>>` holding both endpoint `l2w`s, present only for moving instances.

  The normal matrix is recomputed per hit as `w2l.matrix3.transpose()`, which is exact.
  The world bounds are kept only for the build. The resident `InstancePrim` goes from
  240 B, behind a 64 B slot, to **96 B**.
- **Cubic curve spans move into an inline array of their own**, 96 B, with no box and
  no slot.
- **`PrimNode` keeps only spheres, disks, cylinders and linear curve segments.**
- **The build sees exactly the same reference order as today.** A transient,
  kind-tagged order table maps each non-triangle primitive's attach-order build index
  to its array. Splits, ties and leaf order are therefore unchanged, and so are images
  and query results, bit for bit.
- **`--stats` kernel memory:** `boxed primitives` is replaced by `instances` and
  `cubic curve spans`. `primitive nodes` now counts only the remaining analytic
  primitives.

Expected on the island, arithmetic from the measured counts:
- instances: 27.6 M × (64 + 240 → 96 B), about −5.3 GiB;
- cubic spans: 19.3 M × (64 + 96 → 96 B), about −1.15 GiB;
- plus about 47 M fewer heap allocations, whose allocator overhead is not in the
  kernel figure.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `intersection-kernel`: gains a requirement that instances and cubic curve spans are
  stored inline at bounded per-primitive cost, with query results unchanged.

## Impact

- **`crates/crust-rt`:**
  - `prim.rs`: `InstancePrim` slimmed, `InstanceMotion` added, the boxed variants
    leave `PrimNode`;
  - `bvh/mod.rs`: `Primitives` and `Bvh` gain the instance and cubic arrays plus the
    transient order table, the scalar leaf dispatch decodes the kind tag (still out of
    line, per #182's measured trap), and footprint and breakdown are updated;
  - `bvh/collapse.rs`: leaf indices are written as tagged ids;
  - `scene.rs`: commit and `describe_instances`.
- **`crates/crust-core/src/stats.rs`:** the breakdown labels.
- **Pinned sizes:** `InstancePrim` 240 → 96, plus a new pin for `CubicCurvePrim` at 96.
  `PrimNode` stays at 64, as its largest remaining variant (a linear curve) sets it.
- **Out of scope:**
  - quantized nodes;
  - instance-context hits (#182 Deferred 6);
  - storing `w2l` as a 48-byte row form. That could reach 80 B, but
    `Affine3A::transform_point3a` and a hand-written row form need not agree to the
    bit.
