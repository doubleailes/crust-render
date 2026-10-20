# Design

The record is `openspec/specs/intersection-kernel/design.md` § "BVH8 per tree", with
the measurements in `docs/simd.md` § "BVH8 per tree".

## Decisions

- **D1 — generic, not `cfg`.** A `Width` trait with associated array types
  (`Children = [u32; N]`, `Order`, `Dist`) stands in for `[_; LANES]`, which a generic
  struct cannot spell on stable. The traversal is monomorphised per width; the tree's
  `enum Tree { Four, Eight }` is matched once per query.
- **D2 — two `Vec4` halves, not `wide` / `core::arch`.** Out of cache the gain is in
  node fetches, so 256-bit instructions are not needed for it, and the halves keep the
  crate dependency-free and `forbid(unsafe_code)`. The node is aligned to 32 bytes so it
  is 256 bytes at either implementation.
- **D3 — chosen by primitive count.** The kernel cannot query the cache size in safe,
  portable code, and the primitive count is known before the build. 2^18 sits at the
  measured crossover on a 33 MiB L3. A larger cache moves the crossover up; the
  switch is the escape hatch.
- **D4 — the binary tree is shared.** Build decisions do not depend on the width, so
  the leaves and packets are identical and bit-identity holds by construction except
  for traversal order, which matters for exact ties: two different primitives at the
  same `t` go to the leaf visited first. That happens at coplanar contact faces
  (`subdivision.usda`'s meshes on its ground quad). An order-independent tie-break
  would need an id compare in every primitive test and would move the 4-wide images
  too, so ties are documented instead.
