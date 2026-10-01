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
  **Triangles are stored once** (`compact-triangle-storage`). A committed scene holds
  one shared vertex table and one per-vertex normal table (`[f32; 3]` each, unpadded),
  a 24-byte `TriangleRecord` per triangle (three global vertex indices, `geom_id`,
  `prim_id`, mask — `a_triangle_record_is_24_bytes`) and a 12-byte `GeomTable` per
  geometry saying where its vertices, normals and records start. Packet lanes index
  records; `PrimNode` keeps only the primitives that are not triangles and is 64 bytes
  (the linear curve segment stores its endpoints unpadded so the enum is not 80). A
  record stays even when its attached indices were out of range (`DEGENERATE_VERTEX`,
  so `prim_id`s remain dense) but is never built over, and neither is a sliver of a
  normal-less geometry whose geometric normal is exactly zero: closest-hit rejected
  such a sliver on every candidate anyway, so excluding it at commit changes no
  reported hit and lets the candidate path store four values and nothing else. A hit's
  normal is derived once, for the lane that won (`Bvh::resolve`), by interpolating the
  three per-vertex normals through the record or taking the geometric normal of its
  gathered vertices — the same arithmetic on the same `f32`s as before, so every
  sample renders bit-identical (`check_images.sh check` against goldens recorded
  before the change). `Scene::triangle_vertices(geom_id, prim_id)` hands an
  application the attached vertices of a hit triangle (local space in an instanced
  scene), which is what lets it derive per-hit quantities instead of storing them.
  Measured on the `gen_subdiv_stress.py` grid at level 3 (2.23 M triangles): kernel
  memory 416.89 → 221.44 MiB, **196 → 104 bytes per triangle** (records 24, packets 49,
  vertices 6, normals 6, nodes 15, leaves 4), peak RSS 808.79 → 572 MiB;
  `subdivision.usda` at level 4: 199 → 107 bytes per triangle.
  The history this replaces: the per-corner normal side table (`PrimNode` 128 → 80
  bytes, cornellbox kernel memory −22%) and before it the inline `Option<[Vec3A; 3]>`.
  **Trap, found by per-line callgrind:** with the triangle variant gone the `PrimNode`
  dispatch became small enough for LLVM to inline into `Bvh::hit` — curve subdivision,
  cone intersection and the instance descent with it — and the traversal loop grew by
  a third and spilled its stack: +6% instructions on cornellbox and +17% on
  materialx_basic with a byte-identical tree. The scalar dispatch is
  `inline(never)` (now `Bvh::scalar_hit` / `scalar_hit_any`, see below); with that, `Bvh::hit` (with its instance recursion) is 953.4 M
  instructions on cornellbox against the baseline's 1 001.6 M (−4.8%) and 291.9 M
  against 282.9 M on materialx_basic (+3.2%, a ten-triangle scene where per-query
  setup dominates). Moving the once-per-query `resolve` out of line as well was
  measured and is worse (+7% / +15%).
  **Two packet layouts, bit-identical; the default is gathered.** `Tri4` (192 B, nine
  `Vec4`s of gathered vertices plus per-lane record ids and masks) and `Tri4i` (92 B,
  `[[u32; 3]; 4]` vertex indices and the same lane masks). `Tri4i::intersect` gathers
  the twelve vertices into the same nine `Vec4`s and runs the same lane code, so every
  bit, `fallback` lanes included, is identical (`tri4i_matches_tri4_bitwise`,
  `packet_layouts_are_bit_identical`). The layout is per tree (`commit_with`), chosen
  by `CRUST_TRI_PACKETS=gathered|indexed|auto`. The proposal expected an out-of-cache
  tree to favour the smaller packet; it does not. `ray_throughput --layout` in cache:
  indexed 1–8% slower; the 4 M-triangle out-of-cache soup (581 → 451 MiB of kernel):
  intersect and occluded 30% slower; the stress-grid render 7.20 → 6.27 Mray/s (−13%)
  for 104 → 79 kernel bytes per triangle. Twelve dependent vertex loads per packet test
  cost more than the 100 bytes of bandwidth they save, so a size threshold has no value
  and `auto` is gathered. On the Moana island at level 1 (2026-10-01, interleaved,
  `-s 16`, two runs per side, `docs/moana_profile.md`): kernel 36.59 → 28.22 GiB, peak
  RSS 51.2 → 42.4 GiB, render 8–9% slower, but import 3–4% *faster* (8.4 GiB less
  packet data written, most of it for prototypes committed during the traverse), so the
  whole run is 3–4% faster below roughly 600 spp. That is a whole-run argument for
  indexed on import-bound scenes, not a render one; the threshold stays unset until a
  proposal measures more than one scene.
  **Instances and cubic curve spans are stored inline** (`slim-instance-and-curve-storage`,
  `compact-triangle-storage`'s Deferred item 3). Each has an array of its own on
  `Primitives` / `Bvh`, so neither pays a `PrimNode` slot or a box any more:
  - **an instance** is a 96-byte `InstancePrim`: the inner scene, `w2l`, the ids and
    mask, and an `Option<Box<InstanceMotion>>` holding both endpoint `l2w`s for the
    moving ones. The normal matrix is `w2l.matrix3.transpose()`, recomputed per hit
    (the expression it used to be cached from — a transpose is exact). Its world
    bounds live only in the build (`Primitives::instance_bounds`). Before: a 64-byte
    slot plus a 240-byte box.
  - **a cubic span** is its 96-byte `CubicCurvePrim`, inline. Before: a slot plus a
    96-byte box.
  - `PrimNode` keeps spheres, disks, cylinders and linear curve segments (still 64 B).

  **Why the images cannot move:** the build's index space is unchanged. A transient
  `order` table gives each non-triangle, in attach order, its kind-tagged resident id
  (two bits of kind, 30 of index), so the references, the split ties and the leaf
  order are exactly what one shared array gave; leaves store the tagged id and the
  out-of-line `Bvh::scalar_hit` decodes it. Splitting by index *range* instead
  (others, then instances, then cubics) was rejected for that reason: it reorders the
  references whenever kinds interleave. Measured 2026-10-01 against `a50b1a8`: the
  Moana island's kernel memory 20.02 → 13.52 GiB (peak RSS 30.3 → 23.8 GiB), ALab
  4.38 → 3.82 GiB; every sample, the Kitchen_set pair and the island and ALab frames
  bit-identical, in both packet layouts; callgrind −0.7% (cornellbox) and −0.6%
  (nested_instancing) instructions overall, `Scene::occluded` −4.8% / −3.4%.
  **Leaves are sized by packet rounds** (`CommitOptions::packet_sah`, the
  `CRUST_BVH_PACKET_SAH` switch, default on). The SAH leaf decision used to charge one
  unit per triangle with no node cost, so a range of five to eight overlapping
  triangles always split into two half-empty packets — on ALab and the Moana island
  packet lanes were 48% full, and every half-empty packet is 192 resident bytes, a
  `Leaf` and a node lane. Now an all-triangle range of at most `MAX_LEAF` stays a leaf
  when the object split's `ceil(n / 4)` rounds per side plus one node test (a 4-wide
  slab test, about a packet's worth) cost at least the leaf's rounds, decided before any
  spatial split is weighed (`splitting_pays`; the per-triangle path never leafed there,
  which is what makes the off side the rule it replaces). Measured, off → on:
  cornellbox BVH nodes 34.38 → 23.50 KiB and leaves 8.62 → 6.23 KiB (125 → 115 bytes
  per triangle); the stress grid's nodes 31.76 → 18.27 MiB and 104.2 → 95.5 bytes per
  triangle with lanes 98.6 → 99.2% filled; `subdivision.usda` at level 4 107.4 → 100.8.
  The checked-in samples' lanes were already 75–98% full, so the fill gain the
  production scenes promise is not visible on them; the node count is. Not
  bit-identical by construction: the tree's shape changes, so two triangles at exactly
  the same hit distance can be reported the other way round. On the goldens that is
  one pixel of `veach_mis` (1 of 518 400, relmse 1.7e-6) and nothing on the other 27
  samples. The 1/√N check (`--indirect-clamp 0`, on against off): veach_mis relmse
  4.56e-7 at 16 spp, 5.03e-8 at 64, 5.63e-9 at 256 — one or two pixels, falling faster
  than 1/N, no plateau; cornellbox 0 / 0 / 0; instancing 0 / 0 / 3.97e-11. Interleaved
  on/off timing, four reps, Render phase: cornellbox min 7.51 s on against 7.50 s off,
  veach_mis 12.37 against 12.40 — neutral in cache; the stress grid's single run went
  7.72 → 8.28 Mray/s. The goldens were re-recorded with the rule on after this check.
  The build reads vertices through `Primitives` rather than from a per-triangle copy,
  `PrimRef` is 28 bytes and the binary `Node` 32 (`a_build_reference_is_28_bytes`;
  both were 48 with `Vec3A` bounds), the leaf, packet and index tables are sized
  exactly from the binary leaves before any is emitted, and `merge` splices the right
  subtree into the left's vectors instead of allocating a third. Decisions are
  unchanged (`build_is_deterministic`).
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
  8-wide leaf packets used to buy exactly nothing because no leaf held more than 4
  triangles; with packet-sized leaves (`CommitOptions::packet_sah`, below) a leaf holds
  up to two packets, and an 8-wide packet would merge those pairs — 31.6% fewer
  rounds on the test sphere mesh, measured and bounded by
  `eight_wide_packets_would_save_at_most_the_two_packet_leaves` — which changes the
  arithmetic of the leaf question, not the toolchain reasons below. **BVH8
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
