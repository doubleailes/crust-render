## Context

The motivation and the step-1 numbers are in proposal.md.

- **The kernel today.** `collapse()` turns the binary SBVH into `WideNode`s: six SoA
  `Vec4`s of `f32` bounds, one `u32` child index per lane and a flags word, 128 bytes at
  the default width (`bvh/mod.rs`). Leaf payloads live in a side `leaves` table. `Bvh`
  traversal sorts hit lanes near-to-far by entry distance, intersects leaf lanes at
  once and pushes internal lanes far-to-near.
- **The width seam.** The traversal, the collapse and the tests are written against a
  `LANES` constant. `lanes4.rs` (glam `Vec4`, stable) and `lanes8.rs`
  (`std::simd::f32x8`, nightly, the `bvh8` feature) each supply the lane vector, the
  pre-splatted `RaySlab` and the slab test. The choice is a `cfg`, so it is per build.
- **What step 1 added.** `lanesq.rs` is a third module behind a stable `bvh8q` feature.
  `finish()` converts the collapsed `f32` nodes into `QNode`s and renumbers them
  breadth-first. The traversal calls `node.child_at(lane)` instead of reading
  `node.child[lane]`.
- **Constraints that shape this design:**
  - `forbid(unsafe_code)` in `crust-rt`;
  - stable Rust only;
  - the tree is deterministic across thread counts;
  - closest-hit images are pinned bitwise (`crust diff`);
  - the default path's instruction count is pinned (callgrind on `cornellbox`);
  - environment switches live in `Config`, with today's behaviour as the off side.

## Goals / Non-Goals

**Goals:**
- Give large, out-of-cache trees the compressed 8-wide node without slowing small ones.
- Keep every closest-hit and any-hit result the same as today (modulo exact distance
  ties; see D5).
- Make the choice automatic by tree size, overridable by a switch.

**Non-Goals:**
- Matching GPU compressed-BVH numbers (Ylitie measured a GPU).
- A new traversal order for its own sake. Phase 2 is gated on evidence.
- Any `unsafe`, any new dependency, any nightly feature.

## Decisions

### D1. The node: 8 children, byte-quantised bounds, derived child indices

The prototype's `QNode` is 76 bytes:

| field | bytes | meaning |
| --- | --- | --- |
| `p` | 12 | min corner of the node's box, the union of its children |
| `e` | 3 | biased exponent of the cell size per axis, cell = `2^(e-127)` |
| `imask` | 1 | lane is an internal child |
| `valid` | 1 | lane holds a child |
| `child_base`, `leaf_base` | 8 | first internal child node, first leaf entry |
| `qmin`, `qmax` | 48 | `[[u8; 8]; 3]` each, per axis |
| padding | 3 | alignment to 4 |

A lane's internal child is node `child_base + popcount(imask & (2^l - 1))`, and its leaf
is `leaves[leaf_base + popcount((valid & !imask) & (2^l - 1))]`. This requires a node's
internal children to be consecutive in the node array, and its leaf lanes consecutive in
`leaves`. The breadth-first renumbering guarantees both. The root stays at index 0.

Alternatives considered:
- **Keep a `u32` child index per lane (32 bytes).** Rejected: it brings the node to 108
  bytes, near the 128-byte padded control, which the measurements say gives back part of
  the gain.
- **obvhs's inline layout** (`primitive_base_idx` plus a per-child unary primitive
  count, at most 3 primitives per child). Rejected here: crust leaves hold up to 8
  triangles in `Tri4` packets plus scalar indices for spheres, curves and instances,
  which the unary scheme cannot express without a second table. The `leaves` table
  stays, at the cost of a second indirection.

### D2. Quantisation is conservative, and says so

Each child's min is floored and its max is ceiled onto the grid, with a one-step
correction when float rounding in the subtraction would leave the grid line inside the
child. The dequantised box therefore contains the exact box: the test can say "maybe"
where the exact one says "no", never the reverse. The triangle test is exact, so hits
do not change; the cost is some extra node visits. The cell exponent is the smallest
power of two whose 255 cells span the node's extent, computed from the float's bits
rather than `log2`/`exp2`, which can round to the wrong power. A unit test asserts
containment on every lane of a 6 400-triangle sphere's tree.

### D3. The slab test: safe, two `Vec4` halves

The ray is held as scalars. Per node, a cell-size vector times the reciprocal direction,
and the origin offset times the same reciprocal, give two per-axis constants. A child's
slab distance is `q · (cell · inv) + (p − o) · inv`, with `q` the byte converted by
`as f32`. Eight lanes run as two `Vec4` halves, so there is no `unsafe`, no `std::simd`
and no `core::arch`.

`safe_inv3` bounds the reciprocal by 1e20, so nothing overflows to the `0 · ∞ = NaN` that
would poison `min`/`max` for any scene below about 1e18 units. This is stated in the
code and in a spec scenario.

Why not hand-written intrinsics: obvhs reports its SSE path at more than twice the speed
of its scalar one. The measurements here are the stable-Rust answer to the same
question: in cache, the conversion is the cost (15–30 % slower), and that is exactly what
per-tree selection (D4) avoids paying where it cannot be repaid.

### D4. The format is per tree, chosen at `commit()`

- **Both formats are compiled in.** The prototype's `cfg` modules cannot do this, because
  `LANES` is a `const` at crate level. The collapse, the traversal and the tests become
  generic over a `NodeFormat` trait that supplies the node type, `WIDTH`, the slab test
  and `child_at`. The traversal is monomorphised, so there is no per-node branch.
- **`Bvh` holds the format as an enum** of the two node arrays, matched once per tree
  descent, not per node. An instance descent matches again for its inner tree, so
  one scene can mix formats, which is the point.
- **The rule.** A tree above the threshold gets `quant8`; others get `wide4`. The
  threshold is a count of primitive references, set from the crossover measured in task
  5.2, not from a guess. The measured crossover is expected between the in-cache scenes
  (43 k triangles, `quant8` slower) and the 8 M-triangle soup (`quant8` faster), and
  between a prototype that fits in cache and a top level that does not (the 2 M-instance
  field is the mixed case).
- **Alternative: decide by estimated bytes against the last-level cache.** Better in
  principle, but the kernel does not know the machine's cache size and the numbers here
  come from one VM. A reference count is testable and reproducible; the switch is the
  escape hatch.

### D5. Hits are unchanged, except exact distance ties

Closest-hit selection does not depend on visit order except on exact distance ties, as
the `bvh8` record found, and the quantised boxes only change which extra nodes are
visited and in what order. Two primitives at exactly the same `t` may therefore resolve
to either, as they already can between the two packet layouts' tie-break lanes. The spec
states the contract that way, and task 3.3 tests it by brute force on random tris and by
`crust diff` on the sample scenes. Step 1 already measured 0 differing pixels on three
scenes. Any-hit results are exact (a boolean), so occlusion is unaffected.

### D6. Determinism

The conversion is a pure function of the collapse output. The breadth-first queue and the
lane order fix every index, so the tree is identical across thread counts. The existing
`build_is_deterministic` test is extended to compare the quantised nodes.

### D7. Phase 2 (octant ordering) is gated, not assumed

Step 1 kept the per-ray hit sort, so it did not test Ylitie's other idea. Ordering
children per ray-direction octant at build time removes the sort and its eight-entry
insertion loop, which the existing record names as the cost that hurts closest-hit
most. It changes visit order, so the spec's tie clause matters more, and it needs the
8-orientation child assignment the paper describes, which is a real build-time cost.

Phase 2 is built only if Phase 1's profile shows the sort is a measurable share of
`quant8` closest-hit time on the soup (task 5.3 measures it). If it is not, Phase 2 is
dropped by `/opsx:update` and the design record says why.

### D8. How far the measurements can be trusted

The evidence is one shared 4-core VM and is noisy.
- **Interleaving was not possible inside one process**, because the format was a
  compile-time feature. The A/B ran all four variants concurrently on pinned cores, with
  the core assignment rotated per round. That removes time drift but adds memory-bandwidth
  contention, so absolute times are 15–30 % higher than single-process runs.
- **Closest-hit varies by round** (quantised against `f32` 8-wide: −25 %, −22 %, −2 %,
  −7 % on the median), and the cause is unexplained. Occlusion is steadier (−11 % to
  −22 %).
- **The padded control is not clean.** At 128 bytes it kept most of the gain in some
  rounds and none in others, and its footprint (1 049 MiB) differs from the 76-byte
  node's (1 015 MiB), so it does not separate bytes from arithmetic as sharply as
  intended.
- **The in-cache loss is from single-process runs**, not the concurrent ones.

Task 5 therefore repeats the A/B on the production-shaped scenes, and the gate (D9) is
set low enough that a modest real win passes and a null result fails.

### D9. Gate between Phase 1 and the default

`auto` may select `quant8` by default only if all of these hold:
- on a tree of at least 1 M triangles out of cache, `quant8` is no slower than `wide4`
  on both queries, with the interleaved method, and at least 10 % faster on occlusion;
- the in-cache scenes under `auto` are within 1 % of today's instruction count and
  within noise on `bin_before` / `bin_after` A/B (they select `wide4`);
- `crust diff` shows 0 differing pixels on cornellbox, veach_mis and the stress scene;
- the 2 M-instance field is no worse than `wide4` under `auto`.

If the gate fails, the default stays `wide4`, the format is kept behind
`CRUST_BVH_NODES=quant8` as an opt-in for memory-bound scenes (the 9–16 % saving), and
the record says so.

## Risks / Trade-offs

- **The refactor touches the hottest loop.** Mitigated by pinning the default path's
  callgrind instruction count before and after (task 1.1, 6.1), and by keeping the
  generic code monomorphised. The step-1 `cfg` seam added zero measured cost to the
  default build, and the `LANES` refactor of BVH4 changed it by −0.3 %.
- **Commit-time cost.** One linear conversion pass and a node-array copy. Measured in
  the first `--large` run's soup build: 23.1 s with `bvh8q` against 22.8–24.2 s for the
  others, within noise.
- **Two code paths to keep bit-exact.** Mitigated by running the whole kernel test suite
  under each forced format (task 3.2).
- **The size threshold is a tuned constant.** It is recorded with the machine it was
  measured on, and the switch overrides it.
