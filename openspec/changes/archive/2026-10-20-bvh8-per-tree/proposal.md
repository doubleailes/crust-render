# Proposal

## Why

A look at tinybvh (its `BVH8_CPU` layout traces 1.8–2.4× faster than crust-rt on a
triangle soup) asked whether crust should swap its BVH for that library. It should not:
the Rust wrapper is a beta `cxx` binding that segfaults when its 8-wide BVH is dropped,
hardcodes `-march=native`, exposes triangles and closest-hit only (no occlusion query,
`t_min`, masks, instancing, curves or motion blur), and would put C++ behind `unsafe` on
the hot path. Its plain and HQ (SBVH) binary BVHs trace no faster than crust-rt. The
speed comes from 8-wide nodes.

crust already measured 8-wide nodes (`docs/simd.md`, "BVH8 on nightly"): slower in
cache, 18–25% faster on an out-of-cache soup, a loss on an instance field whose
prototypes fit in cache. Its verdict was that the width should be chosen per tree by
size, which needs both widths compiled in. This change does that, on stable.

## What Changes

- The BVH is generic over a node `Width` (`W4`, `W8`); both are compiled into the stable
  build. 8 wide on stable is two `Vec4` halves per coordinate (safe code); the nightly
  `bvh8` feature now only swaps them for one `std::simd::f32x8`.
- `crust_rt::NodeWidth` (`Four` | `Eight` | `Auto`) on `CommitOptions::width`; `Auto`
  (the default) collapses a tree of 2^18 or more primitives 8 wide.
- `CRUST_BVH_WIDTH` (`four` | `eight` | `auto`) forces it in the renderer; `four` is the
  behaviour before.
- `ray_throughput --width four|eight|auto` A/Bs it.
- Every query answers bit-identically at either width, except exact ties (two primitives
  at the same `t`, which go to the leaf visited first).

## Capabilities

### Modified Capabilities

- `intersection-kernel`: node width chosen per tree.
- `cli`: the `CRUST_BVH_WIDTH` switch.

## Impact

Trees under 2^18 primitives are collapsed exactly as before, so the sample scenes'
images and trees do not change. Large trees (baked production geometry, big instance
top levels) trace 4–21% faster out of cache. API: `CommitOptions` gains a field (its
struct literals need `..Default::default()`); `NodeWidth` is new.
