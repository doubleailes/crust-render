# SIMD in crust

An audit of where crust uses SIMD, what was changed to use more of it, and
what is measured. Numbers below come from a 4-core Xeon @ 2.80 GHz
(SSE2/AVX2/AVX-512F/FMA available), Rust 1.94.1 stable, `--release`.

## What crust uses, and why not `std::simd`

Every vector type in the renderer is a `glam` type, and on x86-64 `glam`'s
`Vec3A`, `Vec4`, `Mat3A`, `Mat4`, `Affine3A` and `Quat` are backed by SSE2
`__m128` registers. So the arithmetic in the hot paths — ray/sphere
intersection, normal transforms, colour math, the BVH slab test — is already
vectorized *horizontally*: one instruction operates on the three or four
components of a vector.

`std::simd` (the `portable_simd` feature) is **not** used, and cannot be on
this toolchain:

```
$ rustc --version
rustc 1.94.1 (e408947bf 2026-03-25)
$ echo 'fn main(){ let _ = std::simd::f32x4::splat(1.0); }' | rustc - 2>&1 | head -2
error[E0658]: use of unstable library feature `portable_simd`
```

It is still nightly-only (tracking issue rust-lang/rust#86656), and crust
builds on stable.

`glam` is deliberately kept on its default (SSE2) backend rather than its
optional `core-simd` feature, which requires nightly for the same reason.

### The 128-bit ceiling, and why it is where it is

This is worth stating plainly rather than leaving implicit, because current
practice disagrees with it. Kerkour's *SIMD programming in pure Rust* puts it
bluntly: "it makes no sense to implement SSE2 SIMDs these days, as most
processors produced since 2015 support AVX2." Everything vectorized here —
`glam`'s `Vec3A`/`Vec4`, the BVH4 node test, the `Tri4` leaf packets — is
128-bit. So why stop there?

Two reasons, one measured and one structural.

**Measured: widening the *codegen* buys almost nothing, because the
algorithm is 4 lanes wide.** LLVM cannot turn a 4-lane algorithm into an
8-lane one; enabling AVX2 only lets it pick FMA and better instruction
sequences for the lanes already there.

| min-of-40, ms | baseline (SSE2) | `-C target-feature=+avx2,+fma` |
| --- | --- | --- |
| `tri_spheres intersect` | 3.450 | 3.386 |
| `sphere_grid intersect` | 1.697 | 1.622 |
| `instances occluded` | 2.268 | 2.243 |

2–4%. Real 8-wide throughput needs what the article describes as the generic
recipe — split the work into chunks of X blocks where X is the lane count —
applied to the data structures themselves, not a compiler flag.

**Structural: for the leaves, there is no eighth lane to fill.** A histogram
of leaf occupancy on a dense mesh (80×40 UV sphere, 6400 triangles):

```
 1 tris/leaf:     13 leaves
 2 tris/leaf:    505 leaves
 3 tris/leaf:    601 leaves
 4 tris/leaf:   1200 leaves
vector rounds if 4-wide: 2319   if 8-wide: 2319
```

No leaf holds more than four triangles, because the SAH is free to split
anything above `MIN_LEAF_PACKED`. A `Tri8` packet would run the *same
number* of vector rounds with half the lanes idle. This is pinned by
`eight_wide_packets_would_not_reduce_vector_rounds`, which fails if a future
retune makes leaves big enough to change the answer.

That leaves **BVH8 nodes** — 8 child boxes per vector round, and roughly half
the tree depth — as the one remaining place where 256-bit vectors would pay.
It is not implemented; see "Not done".

## The two kinds of SIMD in a ray tracer

Worth separating, because crust does one of them well and used to do the
other not at all:

- **Horizontal / AoS** — one instruction over the x, y, z of *one* vector.
  This is what `glam` gives you for free. It wastes the fourth lane on
  `Vec3A` and cannot vectorize a comparison chain, but it is effortless.
- **Vertical / SoA** — one instruction over the *same component of four
  different objects*. This is what BVH4 node tests and 4-triangle packets
  do, and it is where the real throughput is: four boxes or four triangles
  per vector round, no wasted lanes.

The node slab test was already vertical (`WideNode` holds `bmin_x` as a
`Vec4` across four children). Leaf intersection was not: it looped over
`Box<dyn Prim>` and called a scalar intersector per triangle.

## Changes

### 1. Release codegen so the existing SIMD survives (`Cargo.toml`)

`[profile.release]` now sets `lto = "thin"` and `codegen-units = 1`. The hot
loop crosses three crates (`crust-core` → `crust-rt` → `glam`), all built
from small `#[inline]` functions whose whole purpose is to disappear into
their caller. With the default 16 codegen units and no LTO those calls
survive as real calls and the vector bodies never fuse.

Free, no source change:

| | before | after |
| --- | --- | --- |
| `tri_spheres intersect` (criterion mean) | 6.71 ms | 5.32 ms (**−21%**) |

### 2. Mask-driven BVH4 traversal (`bvh.rs`)

The 4-wide slab test computed `(tnear, tfar)` as `Vec4`s and then threw the
vectorization away, reading lanes back one at a time (`tnear[l] <= tfar[l]`,
four times, plus four branches to skip unused lanes). Now:

- `RaySlab` pre-broadcasts the ray origin, reciprocal direction and `t_min`
  into `Vec4` lanes **once per traversal**, not once per visited node (the
  old code rebuilt seven splats per node).
- `safe_inv3` computes all three reciprocals component-wise and branch-free
  (`copysign` + `select`) instead of three scalar calls with a branch each.
- The lane verdicts come out as a nibble: `tnear.cmple(tfar).bitmask()` —
  one compare plus one `movmskps`. Traversal iterates set bits with
  `trailing_zeros`.
- Lane validity moved into a bitmask (`WideNode::flags`) that is ANDed into
  that nibble, replacing four per-lane branches with one integer `and`.
  (An unused lane cannot simply be given empty bounds: the slab test's
  per-axis min/max un-inverts an inverted box, and +INF/+INF bounds pass
  when `t_max == INF` and the ray direction is all-positive.)

### 3. 4-wide triangle packets in the leaves (`triangle.rs`, `bvh.rs`)

The Woop et al. 2013 watertight intersector splits cleanly into a per-*ray*
part and a per-*triangle* part: the axis permutation and shear depend only
on the ray. So:

- `RayShear` holds that per-ray part, derived once per traversal (it costs
  two divides, previously paid once *per triangle*). It is only built for
  scenes that actually contain packets — on sphere/instance scenes it would
  be pure overhead.
- `Tri4` holds four triangles in SoA layout (`v[vertex][axis]` as `Vec4`s)
  plus each lane's primitive index and visibility mask. `Tri4::intersect`
  is the `Vec4` transcription of the scalar path: edge functions, sign
  test, determinant, and the range test — the last folded from the scalar
  path's branch on `sign(det)` into one `select` (`t·sign(det)` against
  `t_min·|det|` and `t_max·|det|`).
- Per-lane visibility masks get a two-compare fast path: the packet stores
  the AND and OR of its lanes' masks, so an all-visible packet (the normal
  case) needs no per-lane test at all.
- Leaf payloads moved out of the node into a separate `Leaf` table, which
  keeps `WideNode` at exactly 128 bytes / two cache lines — the same size
  as before, despite the node now needing to point at packets.

**Watertightness is preserved exactly.** The scalar intersector re-evaluates
edge functions in f64 whenever one comes out exactly `0.0`, which is what
stops a ray crossing a shared edge from missing both triangles. That
tie-break is inherently scalar, so `Tri4::intersect` reports those lanes in
a `fallback` mask and takes no position on them; the caller re-tests them
through the scalar path. The lanes are rare (it takes a ray passing exactly
through an edge or vertex) so the branch costs nothing on real geometry.

The two paths are written to be **bit-identical** — same operations, same
order, no FMA contraction — and `simd_matches_scalar_bitwise` pins that with
`to_bits()` equality over 2000 random packets (~8000 lanes), not an epsilon.
Anything looser would mean the two paths could disagree about a hit near an
edge, which is exactly what watertightness forbids.

### 4. Leaf size retuned to the SIMD width (`bvh.rs`)

With a 4-wide leaf intersector, `MIN_LEAF = 2` leaves half the lanes idle:
measured packet occupancy was **1.66 of 4 lanes**. But raising the floor for
*everything* makes sphere and instance leaves bigger for no benefit — that
cost instanced scenes ~12%.

`MIN_LEAF_PACKED = 4` therefore applies only to ranges made entirely of
triangles (`min_leaf_for`). Occupancy rises to **2.89 of 4**, and both
scene kinds get their best number:

| min-of-40, ms | `MIN_LEAF=2` | `=4` for all | packed-only (chosen) |
| --- | --- | --- | --- |
| `tri_spheres intersect` | 3.814 | 3.365 | 3.426 |
| `sphere_grid intersect` | 1.653 | 1.727 | 1.655 |
| `instances intersect`   | 3.916 | 4.406 | 3.787 |

### 5. `powf(x, 5.0)` → `powi(5)` in Schlick Fresnel (`material/brdf.rs`)

Not a vector change, but a vectorization one: `powf` is an opaque libm call
costing tens of cycles that the surrounding vector code cannot be optimized
across. An integer exponent lowers to a multiply chain in registers. The
same file already used `.powi(6)` two lines below — the three
`powf(_, 5.0)` sites were simply missed. Fresnel runs on every glossy lobe
of every shading event.

`cornellbox` render: **9.09 s → 8.41 s (−7.5%)**, from three characters.
Image difference is at most one ULP (max relative 3.1e-7).

## Measured results

Kernel throughput, `cargo run --release -p crust-rt --example ray_throughput`
(min of 40 repeats over a fixed 4096-ray batch; both columns built with the
LTO profile, so this isolates the SIMD work):

| scene / query | before | after | change |
| --- | --- | --- | --- |
| `tri_spheres` (43k tris) intersect | 4.482 ms | 3.426 ms | **−23.6%** |
| `tri_spheres` occluded | 3.126 ms | 1.925 ms | **−38.4%** |
| `sphere_grid` (1728 spheres) intersect | 1.960 ms | 1.655 ms | **−15.6%** |
| `sphere_grid` occluded | 1.451 ms | 0.931 ms | **−35.8%** |
| `instances` (125 instances) intersect | 4.861 ms | 3.787 ms | **−22.1%** |
| `instances` occluded | 3.546 ms | 2.313 ms | **−34.8%** |

Occlusion gains more than closest-hit because its traversal is almost
entirely node tests and early exits — the part that went from four scalar
lane reads to one `movmskps`.

End-to-end renders (min of 3, wall clock, versus original `main`):

| scene | before | after | change |
| --- | --- | --- | --- |
| `scripts/gen_stress_scene.py` (383k tris), *render time* | 8.03 s | 6.35 s | **−21%** |
| `samples/cornellbox.usda` | 9.14 s | 8.38 s | −8% |
| `samples/openpbr_showcase.usda` | 3.97 s | 3.63 s | −9% |
| `samples/smoke.usda` | 8.10 s | 7.98 s | −1.5% |

The gap between the kernel numbers and the sample-scene numbers is the point
worth remembering: **the checked-in sample scenes are shading- and
sampling-bound, not traversal-bound.** They hold a handful of spheres and
cubes, and spend their time in OpenPBR evaluation, sampling and volume
integration. A 20–38% faster kernel only shows up end-to-end on geometry
heavy enough for traversal to matter, which is what the stress scene is for.
(Its 8.2 s wall clock is mostly parsing 15 MB of USDA and building the BVH;
the `rendering()` figure above is the render itself.)

## Testing across instruction sets

`scripts/test_simd_matrix.sh` runs the suite under four codegen
configurations (target defaults; AVX/AVX2/AVX-512 disabled; AVX2+FMA;
`target-cpu=native`). This is not ceremony: the kernel's correctness claims
are about exact float bit patterns — `simd_matches_scalar_bitwise` asserts
`to_bits()` equality, and watertightness depends on adjacent triangles
computing *exactly* equal edge functions. Contracting `a*b + c` into an FMA
rounds once instead of twice and would break both. Rust does not contract by
default, but that is worth re-verifying rather than assuming, especially
across toolchain bumps. All four configurations pass.

(GitHub Actions runners generally do not expose AVX-512, so that leg only
really runs locally — as the article notes.)

## Correctness verification

- `cargo test` — 33 tests in `crust-rt`, whole workspace green.
- `simd_matches_scalar_bitwise` — the packet and scalar intersectors agree
  bit-for-bit per lane, over hits, misses and both windings.
- `simd_respects_t_range_like_scalar`, `simd_inactive_lanes_never_hit`,
  `simd_masks_gate_lanes` — range tests, padding lanes and per-lane
  visibility masks.
- `packets_are_well_filled` — guards the leaf-floor / SIMD-width match.
- `triangles_are_packed_into_simd_lanes` — every triangle is packed, no
  primitive lands on both paths.
- `build_is_deterministic` — extended to cover the new leaf and packet
  tables.
- The pre-existing `shared_edge_is_watertight` / `shared_vertex_is_covered`
  and `matches_linear_scan` / `hit_any_matches_hit` tests still pass.
- **Whole-image diff**: every sample scene was rendered before and after and
  compared with `cargo run --release -p crust-render --example exr_diff`.
  Seven of eight are **pixel-identical**. `cornellbox` differs in **1 pixel
  of 230 400** by 4.6e-4, at the seam where the inner box's bottom face is
  exactly coplanar with the floor — a genuine `t` tie between two
  primitives, where either answer is correct and the winner depends on leaf
  visit order. Confirmed not to be the retuned tree (it persists with
  `MIN_LEAF_PACKED = 2`).

Note that `cmp`-ing two EXR files is *not* a valid image comparison here:
tile write order is nondeterministic, so the same binary run twice produces
different bytes for identical pixels. Use `exr_diff`.

## Tooling added

- `crates/crust-rt/benches/traversal.rs` — criterion benchmarks for
  `intersect`/`occluded` over triangle-mesh, analytic-sphere and instanced
  scenes, plus the build. `cargo bench -p crust-rt`.
- `crates/crust-rt/examples/ray_throughput.rs` — min-of-N throughput probe.
  Criterion's *means* drift 10%+ on a shared machine; noise only ever adds
  time, so the minimum is the better statistic for comparing two versions of
  a kernel.
- `crates/crust-render/examples/exr_diff.rs` — numeric diff of two rendered
  EXRs (differing pixel count, max absolute/relative, mean absolute, and the
  first few coordinates). The regression check for any kernel change.
- `scripts/test_simd_matrix.sh` — the test suite across four SIMD codegen
  configurations (see above).

## References

- Sylvain Kerkour, *SIMD programming in pure Rust* (January 2026) — the
  load→compute→store framing, the "chunk into X = lane-count blocks" recipe,
  the survey of stable options (`core::arch`, `wide`, `pulp`) and their
  trade-offs, the advice not to hand-vectorize what LLVM already
  auto-vectorizes, and the RUSTFLAGS test matrix. Its "don't bother with
  SSE2" guidance is addressed directly under "The 128-bit ceiling" above.
- Woop, Benthin & Wald, *Watertight Ray/Triangle Intersection* (JCGT 2013) —
  the intersector both paths implement.
- Stich, Friedrich & Dietrich, *Spatial Splits in Bounding Volume
  Hierarchies* (2009) — the build the packets hang off.

## Not done

- **BVH8 (8-wide) nodes.** The one place 256-bit vectors would still pay:
  eight child boxes per vector round and roughly half the tree depth. Note
  `Tri8` packets are ruled out by the occupancy data above — this is about
  nodes only.

  It is not just a matter of writing the 8-wide slab test, because the width
  has to be *reachable at runtime*. The options, in the terms the article
  lays out:

  | approach | 256-bit at runtime? | cost |
  | --- | --- | --- |
  | `wide::f32x8` | only if built with `+avx2`; otherwise 2×SSE2 | +1 dep, safe |
  | `core::arch` + `is_x86_feature_detected!` | yes, dispatched per host | needs `unsafe`, per-arch duplication |
  | `multiversion` | yes | +1 dep, keeps call sites safe |
  | `-C target-cpu=native` | yes | binary not distributable |

  The middle option is the article's recommendation (stable, zero
  dependencies) but it conflicts with a stated property of this crate:
  `crust-rt`'s own docs say the implementation "stays 100% safe Rust", and
  `CLAUDE.md` describes the project as written in safe Rust. Widening the
  nodes is therefore a *project* decision about safety and dependencies, not
  just an optimization, and is left for whoever owns that call.
- **`-C target-cpu`.** Not set, so the shipped binary stays baseline
  x86-64. Measured above at 2–4% on the current 4-lane code; not a portable
  default.
- **NEON / wasm32.** Only x86-64 was measured. `glam` uses NEON on aarch64,
  so the 128-bit paths carry over unchanged; nothing here is
  x86-specific (no intrinsics, no `#[cfg(target_arch)]`), which is itself an
  argument for keeping the kernel on `glam` until portable SIMD stabilizes.
- **Ray packets / streams.** crust traces one ray at a time
  (`rtcIntersect1`-shaped). Tracing 4 or 8 *coherent* rays against one node
  is the other axis of SIMD in a tracer, and would need the integrator
  restructured, not just the kernel.
- **Vectorizing the shading path.** This is where the sample scenes actually
  spend their time. OpenPBR evaluation is already `Vec3A`-based, so the win
  there is not more horizontal SIMD but removing the remaining
  transcendental calls from the inner loops and cutting branch divergence —
  the `powf` → `powi` change above is one instance of a broader pattern.
- **Curves and spheres in packets.** Only triangles pack today. A `Sphere4`
  would help sphere-heavy scenes (`sphere_grid intersect` still runs its
  leaves scalar); rounded cones are branchy enough that a 4-wide version
  needs more care.
