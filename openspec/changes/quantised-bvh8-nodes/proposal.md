## Why

`docs/simd.md` ends its BVH8 section with a pointer: the literature's BVH8 wins (Embree;
Ylitie et al., HPG 2017) come from *compressed* nodes, where quantised child bounds let
eight children fit in about the bytes four take. The nightly `bvh8` feature tested width
alone, on `f32` nodes. This change tests the other half, and then makes the result usable.

**Step 1 is done and measured** (branch `bvh8q-prototype`, a stable-Rust `bvh8q` feature
on `crust-rt`). The collapse output is converted after the fact into 76-byte nodes whose
child bounds are bytes. It sits next to the `f32` 8-wide node, with the same tree,
leaves and packets, so it isolates quantisation. Results, on the `--large` 8 M-triangle
soup (4 variants run concurrently on pinned cores, 4 rotated rounds, median of 6 repeats
each):

- **Out of cache it is faster than the `f32` 8-wide node**: 11–22 % on occlusion in
  every round, 2–25 % on closest-hit (the spread is large and unexplained).
- **A control that pads the same node to 128 bytes gives back part of the gain**, so node
  bytes matter, not only the arithmetic. The control is not clean (see design D8).
- **In cache it is slower**: 15–30 % on `ray_throughput`'s small scenes, because the
  `u8 → f32` conversion costs more than the saved bytes buy.
- **Kernel memory** falls 9 % on the soup and 16 % on the instance field, against BVH4.
- **Images are bit-identical** to the default build on cornellbox, veach_mis and the
  generated stress scene (`crust diff`, 16 spp, 0 differing pixels).

So there is a regime where the format wins and a regime where it loses. The existing
design record already says the width "wants choosing *per tree*, by size, at
`commit()`". This change builds that, because a compile-time feature cannot serve a
scene whose big trees are out of cache and whose small ones are not.

## What Changes

- **A quantised 8-wide node format in `crust-rt`, on stable, safe Rust.** Child bounds
  are 8-bit offsets on a per-node power-of-two grid, rounded outward, so a quantised
  box contains the exact one. A lane's child index is derived (`base + popcount`)
  instead of stored. The format is the one in Ylitie et al. 2017, written from the paper
  and from our own measurements; no obvhs code is copied, and obvhs is credited as the
  reference implementation that was studied.
- **A per-tree node format, chosen at `commit()`.** Both formats are compiled in. A tree
  above a size threshold gets the quantised 8-wide node, and a smaller one keeps the
  4-wide `f32` node. The traversal is generic over the node format, so there is no
  per-node branch.
- **`CRUST_BVH_NODES`** (`auto` | `wide4` | `quant8`, default `auto`) forces a format on
  every tree, the way `CRUST_TRI_PACKETS` forces a packet layout. `wide4` is exactly
  today's behaviour.
- **Footprint reports bytes per format**, so `--stats` shows what the choice cost or
  saved.
- **Phase 2, gated: build-time octant child ordering.** Ylitie's format orders each
  node's children per ray-direction octant at build time, so traversal pops hits in
  priority order with no per-ray distance sort. The existing record identifies the
  sort as the cost that hurts closest-hit most. Phase 2 is built only if Phase 1's
  numbers show the sort is still the bottleneck (design D7).
- The nightly `bvh8` feature stays as the experiment's record. This change adds no
  nightly requirement.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `intersection-kernel`: gains requirements for per-tree node formats, for conservative
  quantised bounds with unchanged hits, and for the node-bytes footprint.
- `cli`: gains a requirement that `--stats` reports node bytes per format, and that
  `CRUST_BVH_NODES` selects the format.

## Impact

- **`crates/crust-rt`:**
  - `bvh/` gains the quantised node and its slab test (`lanesq.rs` in the prototype),
    the breadth-first conversion with contiguous children, and a `NodeFormat`
    abstraction the traversal and collapse are generic over;
  - the `LANES` constant and the `lanes4` / `lanes8` / `lanesq` `cfg` modules become a
    const generic;
  - `CommitOptions` gains the node-format choice;
  - `MemoryFootprint` reports node bytes per format;
  - `ray_throughput` already gained `CRUST_SOUP_ONLY`, `CRUST_LARGE_REPEATS` and
    `CRUST_PRINT_REPEATS`.
- **`crates/crust-core`, `crates/crust-render`:** the switch in `Config` and
  `commit_options()`, `--stats` rows, and the `bvh8q` forwarding the prototype added.
- **Docs:**
  - `openspec/specs/intersection-kernel/design.md` records the measurements, as the
    `bvh8` result is recorded;
  - `docs/simd.md` ("BVH8 on nightly" and "Not done");
  - `docs/architecture.md` § Environment switches;
  - `docs/embree_comparison.md`'s memory-options row.
- **Performance:**
  - Small trees keep today's node, so in-cache scenes are unchanged. Task 6.2 pins
    that with a callgrind count and an interleaved A/B.
  - Large trees trade in-cache arithmetic for fewer bytes.
  - Commit gains one linear pass over the nodes.
- **Risk, stated plainly.** Making the traversal generic over the node format touches
  the hottest loop in the renderer. The tasks pin instruction counts on the default
  path before and after.
- **Out of scope:**
  - the `unsafe` SSE path obvhs uses (crust stays 100 % safe Rust);
  - a dynamic-programming collapse (Ylitie's cost-based child merging);
  - inline leaf primitives per node (the leaf table stays);
  - 16-wide nodes;
  - a nightly requirement;
  - any GPU path.
- **Honest expectation.** On the data so far, the out-of-cache win is real but modest
  and noisy, 10–20 % on occlusion. A neutral result on the Moana-class scenes would
  still leave a 9–16 % kernel-memory saving, which is why the format can be justified
  on memory even where speed is flat. If Phase 1's gate fails, this change is revised
  through `/opsx:update` and only the design record lands.
