# intersection-kernel — design record

> Design record for the **intersection-kernel** capability: the reasoning, measurements and
> history behind the behaviour `spec.md` states. Moved out of `CLAUDE.md`, which
> now keeps only the rules and pointers. Section and path references such as
> "above" or "see X" may point to another capability's `design.md` —
> `openspec/specs/*/design.md` is the whole record; `docs/architecture.md` is the map.

## Crate: crust-rt

- **`crust-rt`** (lib name `crust_rt`) — the intersection kernel, factored out the way
  `openqmc-rs` was, behind a deliberately **Embree-shaped API**: `Geometry` values
  (triangle meshes with optional per-vertex shading normals, analytic spheres, disks
  and open cylinders — the last two exist for UsdLux's `DiskLight` / `CylinderLight` —
  round curve segments, `Instance`s — which nest — with transform motion blur) attach to a
  `SceneBuilder` with per-geometry visibility masks; `commit()` builds the acceleration
  structure; `Scene::intersect`/`Scene::occluded` mirror `rtcIntersect1`/`rtcOccluded1`.
  Hits are plain `Copy` `RayHit`s carrying `geom_id`/`prim_id` — the kernel never sees
  materials. Instanced hits report the *instance's* top-level `geom_id` with the inner
  `prim_id`, unless the instance carries an `InstanceHitId` label (`attach_labelled`):
  `As(id)` reports a fixed id, `Offset(base)` adds `base` to the inner hit's id. That is
  the one-id version of Embree's `instID[]` stack. It lets a host place a prototype of
  many parts as one instance and still tell the parts apart. The importer relies on it
  (`docs/moana_profile.md`). The offset sits in `InstancePrim`'s alignment padding (pinned
  at 96 bytes). Internals: watertight Woop-2013 triangles, rounded-cone curves, and the
  parallel deterministic SBVH build collapsed to BVH4 (details below). Depends only on
  glam + rayon; deliberately swappable for Embree bindings behind the same seam.

## Geometry and intersection

- **Geometry & intersection** — there is no `Hittable` trait anymore: all intersection
  lives in the **`crust-rt`** kernel crate (see the workspace layout), and crust-core
  talks to it through `rt_world.rs`: a `WorldBuilder` pairs every attached
  `rt::Geometry` with its `Arc<dyn Material>` (`attach(...) -> geom_id`), and the
  committed `World` resolves kernel hits back to materials **by `geom_id`** —
  `World::intersect` returns a `WorldHit { rec: HitRecord, mat, geom_id, prim_id }`,
  `World::occluded` is the shadow-ray early-exit query (NEE never uses closest-hit).
  `HitRecord` (`hittable.rs`) remains the material-facing `Copy` hit geometry (point,
  ray-facing normal, `t`, `front_face`). Inside the kernel: `Sphere`,
  triangles with one shared **watertight** intersector (Woop et al. 2013 — dominant-axis
  shear, 2D edge functions with f64 fallback on exact-zero ties; no pinholes along
  shared edges), `RoundCurves` (sphere-swept cones for hair), and `Instance` (a
  committed inner `rt::Scene` placed by a transform — rays transform into local space
  with unnormalized direction so `t` carries over, normals map back by
  inverse-transpose; an optional end-of-shutter transform lerps per-ray for motion
  blur). Per-geometry masks gate intersection on `ray.mask` (`MASK_*` consts,
  re-exported from the kernel). The build is reference-based **SBVH** (binned object
  SAH + spatial splits gated by the α-overlap test, references clipped via exact
  Sutherland-Hodgman for triangles and duplicated across children), runs subtrees in
  parallel via `rayon::join` above 4096 refs, is **deterministic** (input-only
  decisions, pinned by a build-twice test), and is then **collapsed to BVH4**: 128-byte
  4-wide SoA nodes whose slab tests run on `Vec4` lanes (`safe_inv3` keeps
  zero-direction components NaN-free; closest-hit traversal orders lanes near-to-far,
  occlusion traversal early-exits). Traversal is mask-driven: `RaySlab` pre-splats the
  ray once per query, `cmple(..).bitmask()` yields all four lane verdicts at once, and
  a validity nibble in `WideNode::flags` masks unused lanes (they cannot just be given
  empty bounds — the slab test's per-axis min/max un-inverts an inverted box).
  Leaf payloads live in a side `Leaf` table (keeping the node at two cache lines), and
  each leaf's triangles are packed into **4-wide `Tri4` SIMD packets**: the Woop shear
  is per-*ray* (`RayShear`, derived once per traversal), so four triangles are
  intersected per vector round, with lanes whose edge functions come out exactly `0.0`
  handed back to the scalar path for its f64 tie-break — watertightness intact. The
  packet and scalar intersectors are **bit-identical** (pinned by
  `simd_matches_scalar_bitwise`); change one and you must change the other.
  **Resident geometry is stored once** (`compact-geometry-storage`). The build runs over
  transient `BuildPrim`s (full triangles, so the SBVH can clip them); once the tree is
  collapsed, `Prims::from_build` moves them into one array per kind, and leaves refer
  to them by resident id (`Prims::resident_ids`: a packet lane holds an index into
  `tris`, a scalar leaf entry a kind-tagged `u32`, two bits of kind and 30 of index).
  What each primitive then costs, pinned by `resident_primitives_are_their_pinned_sizes`:
  - **a triangle** is a 24-byte `TriRecord` (ids, mask, three normal indices) plus its
    share of the 192-byte `Tri4` packets. The packets are the *only* copy of its
    vertices: the scalar f64 tie-break and the geometric-normal fallback read them back
    with `Tri4::lane_vertices`, which returns the exact `f32`s `Tri4::new` received, so
    `simd_matches_scalar_bitwise` now runs its scalar side on them. (Before: an 80-byte
    `PrimNode` holding the vertices a second time, *and* the packet.)
  - **shading normals** are one `[f32; 3]` per vertex of each smooth mesh, appended once
    at commit; the record's triplet indexes them. A triangle is smooth only when all
    three of its corners have a normal, exactly the old per-triangle rule. (Before: a
    48-byte copy per triangle, so each shared vertex's normal about six times.)
  - **an instance** is a 96-byte `InstancePrim`, inline: the inner scene, `w2l`, the
    ids and mask, and an `Option<Box<InstanceMotion>>` with both endpoint `l2w`s for
    the moving ones. The normal matrix is `w2l.matrix3.transpose()`, recomputed per
    hit (a transpose is exact), and the world bounds live only on the
    `BuildInstance`. (Before: a 240-byte box behind an 80-byte `PrimNode`.)
  - **a cubic curve span** is its 96-byte `CubicCurvePrim`, inline (before: boxed
    behind an 80-byte `PrimNode`). Spheres, disks, cylinders and linear curve
    segments share one small `OtherPrim` enum.

  Measured 2026-10-01: the Moana island's kernel memory 26.58 → 13.85 GiB (peak RSS
  37.3 → 26.1 GiB) and its render 32% faster in an interleaved `bench_ab.sh` (cache
  behaviour: callgrind sees +0.15% instructions on cornellbox); Kitchen_set's
  105.6 → 64.5 MiB (normals 20.1 → 2.6 MiB). Details in `docs/moana_profile.md`.
  Every checked-in sample renders bit-identical. **Trap:** `Prims::hit` /
  `hit_any` (the scalar, non-packet leaf entries) are `#[inline(never)]`. Inlined,
  they pulled the instance path — which re-enters `Bvh::hit` — into the traversal
  loop, and the bigger frame stopped LLVM building `TraversalStack` in place: its
  constructor went from 59 M to 127 M instructions on cornellbox, +7% for `Bvh::hit`.
  Out of line, the change is +0.15% instructions overall.
  `MIN_LEAF_PACKED` (4) is the leaf floor for all-triangle ranges so packets fill,
  while non-packable prims keep `MIN_LEAF` (2) — see `docs/simd.md` for the audit,
  the measurements, and why `std::simd` is not used by default (nightly-only; the
  opt-in `bvh8` experiment below is the one place it is).

## Known gaps: geometry and acceleration

- **Geometry/acceleration caveats.** Motion blur is transform-only and lerps the *matrix*
  linearly (no deformation blur, no quaternion motion — a large shutter rotation bows
  slightly, but the union-of-endpoints bbox stays conservative). Cubic curve spans are
  kept as cubic primitives (converted to Bézier control points) and adaptively
  subdivided per ray query into rounded cones (`crust_rt::curve::cubic_curve_intersect`),
  so they are not stored as polylines; widths lerp across a span in parameter; the
  rounded-cone can report an interior sphere surface for rays *starting inside* the hull (irrelevant for opaque hair). Mesh-BVH sharing needs identical
  points/topology *and* material binding. Emissive curves/instances are not light-list
  entries (BSDF-sampled only, like emissive volumes).
  Baking single-placement meshes (above) leaves *resident* memory unchanged — the same
  triangles, one fewer BVH — but it moves work into a single large top-level SBVH build,
  and that build's **transient** peak is higher: on `Kitchen_set` (1 394 meshes baked,
  414 599 top-level triangles) kernel memory went 134.59 → 134.44 MiB while peak RSS went
  374 → 579 MiB. The cause is pre-existing and not specific to baking: `PrimRef` and the
  binary `Node` are 48 bytes each, and `merge` concatenates child node arrays while both
  children are still alive, so one build over ~600 K references (with SBVH duplication)
  transiently holds a few hundred MiB where 1 788 small builds held almost nothing. If that
  peak ever matters more than the ~20% render win, the lever is a triangle-count cap on
  baking; the real fix is a builder that does not materialise the whole binary tree.
  Subdivision surfaces sharpen this: a level-L cage feeds 4^L× more triangles into the
  same build, and on the `gen_subdiv_stress.py` scene at level 4 the SBVH commit peak
  (2.19 GiB) is nearly twice the refiner's own traverse-phase peak (1.16 GiB) — so the
  build transient, not opensubdiv, is the first lever if a subdivided scene runs out of
  memory. Within `subdivide()` itself the tail holds ~5 copies of the last level's
  positions (`verts`, `limit`, `points`, `verts_a`, `normals`) plus the whole retained
  refiner; restructuring to drop the refiner before the copies would shave roughly a
  third off its ~313 B/face transient if that ever matters.

## Known gaps: SIMD stops at 128 bits

- **SIMD stops at 128 bits** (`docs/simd.md` has the audit and the numbers). Everything
  vectorized — `glam`'s `Vec3A`/`Vec4`, the BVH4 slab test, `Tri4` leaf packets — is
  SSE2/NEON-width, because `std::simd` is still nightly-only and crust builds on stable.
  Current practice says target AVX2 instead, so the reasons not to are recorded: merely
  *enabling* AVX2 codegen is worth 2–4% (LLVM cannot widen a 4-lane algorithm), and
  8-wide leaf packets would buy exactly nothing because no leaf holds more than 4
  triangles — pinned by `eight_wide_packets_would_not_reduce_vector_rounds`. **BVH8
  nodes** were the one place 256-bit vectors might pay, and were **tried**: crust-rt's
  nightly-only **`bvh8`** feature (forwarded by crust-core and crust-render) swaps
  `bvh/lanes4.rs` (glam `Vec4`) for `bvh/lanes8.rs` (`std::simd::f32x8`) behind a
  `LANES` constant, safe code throughout. Images are bit-identical, node visits fall
  7–36%, and on in-cache trees the kernel is still **12–37% slower**
  (`ray_throughput`, min-of-10, same nightly, `x86-64-v3` both sides): a 256-byte node
  and an 8-lane sort cost more than the visits saved. End to end it is within noise.
  **Out of cache the answer flips** (`ray_throughput -- --large`, trees of 0.8–1.7 GiB
  against a 260 MiB L3): an 8 M-triangle soup traces **18–25% faster** with BVH8 (node
  visits −38%, latency-bound at ~140 ns a node), while a 2 M-instance field is 5–9%
  slower because most of its time is in small prototype trees that fit in cache.
  8-wide nodes cost 0.4–3% more kernel memory. So the width wants choosing *per tree
  by size* at `commit()`, which needs both widths compiled in rather than a `cfg`; that
  is a refactor, not a toolchain question, and crust stays on stable. The feature is
  kept as the record and a starting point, and the parallel nightly workflow
  (`.github/workflows/nightly.yml`) lints and tests it on a pinned and the latest
  nightly (`docs/simd.md`, "BVH8 on nightly"). Reaching 256 bits on stable still
  needs `unsafe` `core::arch` intrinsics (against this crate's "100% safe Rust" claim),
  a new dependency (`wide`/`multiversion`), or a non-distributable `-C target-cpu`.
  **The collapsed tables hold no spare capacity.** `collapse()` used to reserve one
  wide node per binary leaf — three times what a BVH4 needs, seven times a BVH8 — and
  `Bvh::accumulate_footprint` counts capacity, so the slack was both resident and
  reported. Reserving for `LANES` and trimming all four tables after the build cut the
  default BVH4's kernel memory 11–21% on the `--large` scenes; pinned by
  `collapsed_tables_hold_no_spare_capacity`.
  Also: only *triangles* pack — sphere, curve and instance leaves still run scalar (no
  `Sphere4`); rays are traced one at a time, so there is no coherent ray-packet tracing
  (that needs the integrator restructured, not just the kernel); and the checked-in
  sample scenes are shading-bound, so kernel speedups barely show up there — use
  `scripts/gen_stress_scene.py` to benchmark traversal changes end to end.
