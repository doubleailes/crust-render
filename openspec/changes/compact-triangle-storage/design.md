## Context

`usd-driven-subdivision` made every mesh that does not author `subdivisionScheme =
none` a subdivision surface, refined to one global level. The refinement itself is
cheap and bounded (`subdivide()` runs per mesh, ~313 B per refined face transiently);
what grew is everything downstream of it, because the renderer stores a refined
triangle several times over. This record starts from where the bytes go today, surveys
what production and research renderers do about it, and decides what to adopt.

### Where a refined triangle's bytes go today

Measured on `scripts/gen_subdiv_stress.py` (a 128² + 2×32² quad-grid cage at level 3,
2 228 354 resident triangles, `--stats`, this commit):

| structure | bytes | per triangle | what it holds |
|---|---|---|---|
| primitive nodes | 170.01 MiB | **80** | `PrimNode::Triangle`: three `Vec3A` vertices (48 B, a copy), ids, mask, normal index, enum tag |
| vertex normals | 102.01 MiB | **48** | `[Vec3A; 3]` per triangle — three copies of each per-vertex normal, padded |
| triangle packets | 104.41 MiB | **49** | `Tri4`, 192 B for four lanes: the vertices again, SoA |
| BVH4 nodes | 31.76 MiB | 15 | 128 B per node |
| leaves | 8.70 MiB | 4 | 16 B per leaf, 3.9 triangles per leaf here |
| **kernel** | **416.89 MiB** | **196** | |
| peak RSS | 808.79 MiB | 380 | the SBVH commit's transient on top (traverse-phase RSS was 284 MiB) |

A refined quad mesh has about one vertex per face and two triangles per face. Stored
once, a triangle is 12 B of indices plus half a vertex — 6 B of position and 6 B of
normal — so the kernel's 196 B is roughly eight times the data. Two of the three vertex
copies are never read on the hot path: traversal tests packet lanes, and `PrimNode`'s
copy is consulted only on a candidate hit (ids, normal index), on the rare `f64`
tie-break, and during the build.

Production scenes are worse than the grid, because their leaves are half empty:

| scene (design records) | triangle packets | per triangle | lanes filled |
|---|---|---|---|
| ALab, 21.17 M triangles | 2.35 GiB | 119 B | ~48 % (a full packet is 48 B per lane) |
| Moana island, 60.9 M triangles | 5.71 GiB | 100 B | ~48 % |
| stress grid, 2.23 M | 104 MiB | 49 B | ~98 % |

and their BVH4 nodes cost 63–66 B per primitive against the grid's 15: the SAH leaf rule
(`o.cost >= area × count`, cost 1 per *triangle*) splits irregular geometry down to
two-primitive leaves, so most packets carry one or two triangles and every leaf costs a
node lane, a `Leaf` record and a 192-byte packet. Spatial splits duplicate whole
packets as well, since a reference lands in a packet per leaf it is in.

Beside the kernel, the importer keeps per-*corner* tables that also scale with 4^L:
`UvMap.uvs` (24 B per triangle), `UvMap.tangents` (16 B, baked meshes only),
`UvMap.density` (4 B), and for Ptex on a subdivided mesh `FaceMap.uvs` (24 B),
`faces` (4 B), `slices` (1 B), `density` (4 B). The ALab level-1 delta — +60.7 M
triangles for +12.77 GiB of kernel memory and +16.7 GiB of peak RSS — is 210 B per
triangle in the kernel and ~65 B outside it, consistent with this accounting.

The SBVH build's transient is the other term: 48-byte `PrimRef`s and 48-byte binary
`Node`s per reference, `merge` concatenating child arrays while both children are
alive, and the `Geometry::TriangleMesh` arrays still alive while the primitive array
is filled from them (`intersection-kernel/design.md`, "Known gaps: geometry and
acceleration"). On the grid it is the difference between the traverse-phase RSS and
the commit peak: ~390 MiB for a 417 MiB result.

### State of the art

What renderers do to hold more geometry in the same memory, grouped by whether the
stored geometry is exact. Only the first group is in scope for this change.

**Lossless, image-identical — store each value once, in the width it needs.**

- *Indexed triangles over a shared vertex buffer.* Embree's default is pre-gathered
  vertices per 4/8-wide leaf (`Triangle4v`), and its compact mode
  (`RTC_SCENE_FLAG_COMPACT`, "uses compact acceleration structures and avoids
  algorithms that consume much memory") stores per-lane vertex *indices* into the
  application's vertex buffer (`Triangle4i`), which is also what makes the vertex
  buffer shareable between the application and the kernel
  ([Embree API](https://github.com/RenderKit/embree/blob/master/doc/src/api/rtcSetSceneFlags.md)).
  The trade is one gather per packet test against a leaf that is a quarter of the
  size; Embree's own comparison runs "highest performance (SBVH + pre-gathered
  triangle data)" against "lowest memory (BVH + triangle indices)" as the two ends of
  one axis (["Digesting the Elephant"](https://arxiv.org/pdf/2001.02620), Wald et al.
  2020, which renders the Moana island interactively by exactly this plus multi-level
  instancing).
- *Per-vertex attributes, not per-corner.* Pharr's Moana series found the converter's
  per-triangle filler arrays (texture coordinates, normals, extra data emitted per
  quad-as-two-triangles) to be a large share of the geometry footprint and removed
  them by computing on the fly
  ([Swallowing the elephant, part 4](https://pharr.org/matt/blog/2018/07/15/moana-island-pbrt-4)).
  Same lesson as the 48 B of normals per triangle here.
- *Compressed-leaf BVH* (Benthin, Wald, Woop, Áfra, HPG 2018): dedicated compressed
  multi-leaf nodes only where they pay, regular nodes elsewhere, "roughly the same
  memory savings as Embree's compressed BVH layout while maintaining almost the full
  performance of its fastest non-compressed BVH"
  ([ACM](https://dl.acm.org/doi/10.1145/3231578.3231581)). Its leaf-level idea —
  size and pack leaves for the SIMD width rather than for the SAH's per-primitive cost —
  is adopted here as the packet-aware leaf cost; its compressed leaf *nodes* belong to
  the image-identical group below and are deferred with them.
- *Build memory.* PLOC++ (Benthin et al., HPG 2022) builds a binary BVH in N × 72 B
  ([Intel](https://cdrdv2-public.intel.com/737298/ploc-for-bounding-volume.pdf));
  fused collapsing (Barbier et al., CGF 2025,
  [Wiley](https://onlinelibrary.wiley.com/doi/10.1111/cgf.70213)) builds the wide tree
  without materialising the binary one. The cheap third of that — narrower references
  and nodes, an arena instead of `merge` copies — is in this change; a builder that
  never holds the binary tree is deferred.
- *Tessellation caching* (Benthin, Woop, Nießner, Selgrad, Wald, HPG 2015): keep the
  subdivision cages and patch tables resident, tessellate a patch lazily when a ray
  reaches its bound, hold the tessellations in a shared bounded cache; "a 60 MB lazy-build
  cache allows rendering at over 91 % of the performance of an unbounded memory cache",
  and "compared to ray tracing a pre-tessellated version, memory consumption is reduced
  by 6–7×" ([HPG 2015](https://www.embree.org/papers/2015-HPG-tcache.pdf)). This is
  the technique that makes memory independent of the level. It is lossless in the sense
  that the same limit surface is evaluated, but not bit-identical to uniform
  refinement, and it is a kernel architecture change; see § Deferred for why it is the
  *next* change and what opensubdiv-rs 0.1.4 already provides for it. Among production
  path tracers the pre-tessellating design is at least as common: Hyperion
  "subdivides/tessellates/displaces everything to as close to sub-poly-per-pixel as it
  can" and leans on instancing
  ([Burley et al. 2018](https://dl.acm.org/doi/fullHtml/10.1145/3182159)); Arnold and
  Manuka likewise tessellate up front
  ([Georgiev et al. 2018](https://dl.acm.org/doi/fullHtml/10.1145/3182160),
  [Fascione et al. 2018](https://dl.acm.org/doi/10.1145/3182161)).

**Lossless in the image, not bit-identical — the same triangles, tested in a different
order.**

- *Quantized wide nodes.* Ylitie, Karras and Laine (HPG 2017) quantize child boxes to
  one byte per coordinate against the parent's frame, an 8-wide node in 80 B, "35–60 %
  of a typical uncompressed BVH"
  ([ACM](https://dl.acm.org/doi/10.1145/3105762.3105773)); Embree ships quantized-node
  trees for its compact configurations on the same idea. Rounding
  bounds *outward* keeps every true hit inside its box, so the hit set is unchanged and
  only exact-tie ordering can differ; Vaidyanathan, Akenine-Möller and Salvi (HPG 2016)
  show the traversal stays watertight under reduced precision
  ([HPG 2016](https://fileadmin.cs.lth.se/graphics/research/papers/2016/watertight/wrtrp.pdf)).
  Haydel, Kensler, Brunvand and Yuksel (HPG 2026) merge internal and leaf nodes into
  bandwidth-sized blocks for a reported 48.3 % smaller BVH and 31.7 % less render time
  on GPU hardware ([Utah](https://graphics.cs.utah.edu/research/projects/bvh-merged-nodes/)).
  Deferred: a BVH4 node here is 15 B per triangle on a well-packed tree, so it pays only
  after the leaves are fixed.

**Lossy — out of scope.**

- *Quantized vertices*: AMD's DGF (HPG 2024) packs meshlets with quantized positions
  and local indices, "reduces the triangle leaf data by 6–10× and total BVH size by
  50 %" at "up to 2.4× higher" decode cost; "the initial quantization is the only
  source of error" ([GPUOpen](https://gpuopen.com/learn/amd-dgf-an-open-geometry-compression-standard/)).
  A 2025 preprint quantizes boxes *and* triangles to 8 bits in local frames, 9 B per
  triangle, 18 % of the memory traffic ([arXiv 2505.24653](https://arxiv.org/abs/2505.24653)).
  Both move vertices; a subdivided limit surface snapped to a 16-bit grid is a
  different surface, and this codebase verifies shading in numbers, so nothing here
  moves a vertex.
- *Transport codecs* (Draco, meshoptimizer's index and vertex encoders) are lossless
  for indices but are storage formats, not something a ray can traverse.

The pattern across the lossless group is the same three moves: index instead of copy,
store per vertex instead of per corner, and size leaves for the SIMD width. That is
this change.

## Goals / Non-Goals

**Goals**

- Halve or better the kernel-resident bytes per refined triangle without changing a
  single stored `f32`; pin bit-identity where it holds and prove noise where it cannot.
- Cut the importer's per-corner tables to what cannot be recomputed from a hit.
- Shrink the SBVH build transient, which sets the peak on subdivided scenes.
- Make the effect legible in `--stats`, so the next regression is a number in a report.

**Non-Goals**

- Any quantization of positions, normals or UVs.
- Tessellation on demand, quantized nodes, a binary-tree-free builder, the instance
  primitive's size, the USD stage's residency — each is named in § Deferred with what
  it would take.
- Changing which meshes are refined or to what level.

## Decisions

**D1 — Triangles leave `PrimNode`; a scene owns shared vertex and normal tables.**
`SceneBuilder::commit` concatenates every `TriangleMesh`'s vertices into one
`Box<[[f32; 3]]>` and its normals into a parallel `Box<[[f32; 3]]>` (a mesh without
normals contributes nothing there; a per-geometry `normals_base: Option<u32>` says
so). A triangle becomes a 24-byte `TriangleRecord { geom_id, prim_id, v: [u32; 3],
mask }` in its own `Box<[TriangleRecord]>`; `PrimNode` keeps spheres, disks,
cylinders, curves and instances and shrinks to its next-largest inline variant. Global
vertex indices (`u32`, 4 G vertices per scene) rather than per-geometry bases keep the
packet gather one load.
*Why not fold the record into the packet?* A lane's ids and vertex indices inside the
packet save one dependent read on a candidate hit, but they make a gathered packet 256
B and are read only on hits; the record table is the same bytes and a smaller diff. The
`normals` side-table precedent (`PrimNode` 128 → 80 B, `rust_leverage.md` § 3.4) is
the same call.
*Alternative rejected:* keep `PrimNode::Triangle` and only drop its vertices. The enum
would still be sized by `CurvePrim` (48 B) plus tag, and every triangle would pay it.

**D2 — Two packet layouts, bit-identical; the default is gathered.** `Tri4` stays as
it is (192 B, nine `Vec4`s of gathered vertices, per-lane record ids and masks).
`Tri4i` is 92 B: `[[u32; 3]; 4]` vertex indices and the same lane masks. Its
`intersect` gathers the twelve vertices into the same nine `Vec4`s and calls the same
lane code, so the arithmetic — and therefore every bit, `fallback` lanes included — is
identical; `tri4i_matches_tri4_bitwise` and `packet_layouts_are_bit_identical` pin it,
and `simd_matches_scalar_bitwise` pins both against the scalar test. The layout is
per tree (`commit_with`), and `CRUST_TRI_PACKETS=gathered|indexed|auto` selects it;
`gathered` on every tree is the behaviour this replaces, so the A/B is honest.
*Measured outcome (task 3.3), against the expectation written here first:* the
proposal assumed a tree too large for the cache would favour the smaller packet, on
`docs/simd.md`'s finding that out-of-cache trees answer layout questions oppositely.
They do not here. `ray_throughput --layout` in cache: indexed 1–8 % slower; the
4 M-triangle out-of-cache soup (581 → 451 MiB kernel): intersect 9.48 → 12.32 s,
occluded 8.21 → 11.23 s (−30 %); the stress-grid render 7.20 → 6.27 Mray/s (−13 %) for
104 → 79 kernel bytes per triangle. Twelve dependent vertex loads per packet test cost
more than the 100 bytes of bandwidth they save. So `auto` is gathered, and `indexed`
is the explicit trade for a scene that otherwise does not fit — a quarter of the
kernel's bytes per triangle for a tenth to a third of its traversal speed.
*Alternative not taken:* a per-tree size threshold, which the measurement left with
no value.

**D3 — Normals interpolate from the per-vertex table through the record.** On a
candidate hit, `hit_from_barycentric` reads the record, gathers three normals by its
vertex indices, and computes exactly today's `n0·(1−u−v) + n1·u + n2·v` on the same
`f32`s. A geometry without normals falls back to the geometric normal from the lane's
vertices (gathered: the packet's; indexed: the table's — same values). The `f64`
tie-break path reads the vertices the same way. Bit-identical by construction and
pinned by the golden check.

**D4 — Leaves are sized for packets, behind a switch.** The SAH leaf decision
(`bvh/build.rs`, `object_partition_or_leaf`) charges an all-triangle range
`ceil(count / 4)` packet tests instead of `count` triangle tests, so a range of 5–8
triangles is a leaf when splitting it would only make two half-empty packets. Curves,
spheres and instances keep the per-primitive cost, since they still run one at a time.
This changes the tree's shape, so it is the one non-bit-identical item: differences
are confined to exact-tie closest hits (two triangles at the same `t`, which order
decides) and are accepted only when the difference to the `CRUST_BVH_PACKET_SAH=0`
render falls as 1/√N across 16 / 64 / 256 spp on every sample scene
(`CLAUDE.md` § Measuring a change). The switch's off side is today's cost, which is
what makes the traversal A/B honest: leaf fill and node count are read straight off
`--stats` (D8), traversal speed from `bench_ab.sh` and `ray_throughput`. Deterministic
either way: the rule reads only the input.

**D5 — The build reads through a `PrimSource`, and holds less.** `build_subtree` and
`collapse` take a `PrimSource` (the record table + vertex table + the remaining
`PrimNode`s) that answers `bbox`, `clipped_aabb` and `as_triangle` by index, so no
`TrianglePrim` copy exists during the build. `PrimRef` becomes `[f32; 6] + u32` (32 B,
from 48) and `Node` likewise (32 B); a subtree's nodes are written into one arena
sized by the `2n − 1` bound of its reference count, with child offsets fixed up in
place, replacing `merge`'s concatenation of two live child arrays. Decisions are
unchanged, so `build_twice_is_identical` holds and every golden is bit-identical with
`CRUST_BVH_PACKET_SAH=0`. `SceneBuilder::commit` moves each geometry's arrays into the
shared tables *before* the build rather than keeping `Geometry::TriangleMesh` alive
beside a primitive copy.

**D6 — The importer derives at the hit what it used to store per corner.**
`Scene::triangle_vertices(geom_id, prim_id) -> Option<[Vec3A; 3]>` (local space for an
instanced scene) is the one new kernel accessor; `World` keeps, per top-level geometry,
the placement it already has (`l2w`, or identity for a baked mesh).

- *Tangents.* `UvMap::resolve` computes `dP/du` from the three vertices and three
  corner UVs with `build_tangents`' formula, per hit, instead of reading a stored
  `Vec3A` — the same inputs, the same bits (`tangents_on_demand_match_the_table` pins it
  against the old table on every sample). Because the vertices come back in the
  mesh's frame and the placement is known, an **instanced** mesh gets a world-space
  tangent too, which retires the "normal maps on instanced geometry use the geometric
  normal" gap in `textures/spec.md`.
- *Densities.* `triangle_density` (UV area over local area) is likewise computed at
  the hit; the placement scale is applied as today.
- *Face-varying charts.* `UvMap` holds `values: Vec<[f32; 2]>` and
  `corners: Vec<[u32; 3]>` (indices into `values`, one per triangle corner) instead of
  `Vec<[[f32; 2]; 3]>`; a `vertex` chart's corners are its point indices. 17 B per
  triangle on a refined chart instead of 24; the lookup is one more indirection, on
  hits only.
- *Ptex sub-faces.* `SubdivFaces` returns, per refined face, an 8-byte
  `SubFace { base: u32, origin: [u16; 2], depth: u8, rotation: u8 }` — the same data as
  opensubdiv's `PatchParam`, read off the refiner's child-face relations — instead of
  four corner UVs; `FaceMap::resolve` reconstructs a triangle's corners from it. Every
  corner is a dyadic fraction `k / 2^depth`, exact in `f32`, and the refined
  face-varying channel it replaces produced those same values by halving, so the
  reconstruction is bit-identical (`sub_face_corners_match_the_refined_channel` pins it
  under all six face-varying rules, as `ptex_channel_is_invariant_under_every_fvar_rule`
  does today). This also removes the second refiner pass a Ptex-and-`none`-chart mesh
  pays (D4 of `usd-driven-subdivision`), since the Ptex channel is no longer refined at
  all.
- `MeshGeom`, `MeshSource` and `SubdividedMesh` carry `[f32; 3]` positions and normals
  (the kernel takes them as such); `Vec3A` is built where arithmetic happens.

**D7 — `subdivide()` drops the refiner before the tail.** The last-level topology and
the limit positions are extracted, then the `TopologyRefiner` (every level, ×4/3 of
the last) is dropped before `points`, `verts_a` and `normals` are built — the
restructuring the kernel design record already costs at a third of the ~313 B/face
transient. `subdivision_memory_probe`'s ceilings move down with it; the probe gains
the `SubFace` case.

**D8 — `--stats` reports the layout.** The kernel-memory block adds `vertices`,
`vertex normals` (now per vertex), `triangle records`, `triangle packets (gathered)`
/ `(indexed)` with `lanes filled` as a percentage, and a closing
`bytes per triangle` line. `MemoryFootprint` gains the matching fields, still counted
from boxed slices (no capacity slack, `collapsed_tables_hold_no_spare_capacity`).

## Memory model and targets

For a refined quad mesh (half a vertex per triangle, `r` references per triangle after
spatial splits, lane fill `φ`), kernel-resident bytes per triangle:

| | today (grid, measured) | gathered `Tri4` | indexed `Tri4i` |
|---|---|---|---|
| primitive node / record | 80 | 24 | 24 |
| normals | 48 | 6 | 6 |
| vertices | (in the node) | 6 | 6 |
| packets | 49 (`48·r/φ`) | `48·r/φ` ≈ 49 | `23·r/φ` ≈ 25 |
| BVH nodes + leaves | 19 | 19 | 19 |
| **total** | **196** | **~104** (measured 104.2) | **~80** (measured 78.6) |

On ALab-shaped geometry (`φ ≈ 0.48`, nodes 65 B per primitive) D4 is the larger term:
bringing `φ` to ~0.85 and leaves to 4–8 triangles is worth ~120 B per triangle on its
own before D1–D3 remove another ~100.

Acceptance, measured with `--stats` rather than modelled:

- stress grid, level 3: kernel ≤ 110 B per triangle with `CRUST_TRI_PACKETS=gathered`,
  ≤ 85 with `indexed`; commit-phase peak minus traverse-phase RSS at most two thirds of
  today's 390 MiB;
- DPEL teapot, level 2 (1.04 M triangles, textured): kernel from 212.95 MiB to ≤ 115
  MiB; the importer's UV tables from 44 B to ≤ 20 B per triangle (the `Traverse prims`
  RSS delta against `CRUST_SUBDIV=0`);
- ALab frame 1004 at level 1, on a machine that holds it: kernel from 19.64 GiB to
  ≤ 11 GiB, lanes filled ≥ 80 %;
- `bench_ab.sh` over the sample scenes within noise for `gathered`; the `indexed`
  threshold chosen so that `ray_throughput --large` is not slower than `gathered` at
  the sizes where it engages.

## Risks / Trade-offs

- [Indexed packets are slower in cache] → they engage only above a size threshold set
  by measurement, and `CRUST_TRI_PACKETS=gathered` restores the old inner loop on
  every tree. `Bvh::hit` is callgrind-compared for the gathered path per `CLAUDE.md`,
  and must be within 1 % of today's instruction count on cornellbox.
- [Packet-aware leaves change images] → tie-only by construction; gated on the 1/√N
  proof and shipped behind `CRUST_BVH_PACKET_SAH` with the old cost as the off side.
  Goldens are re-recorded only after the proof, in the same commit, with the reason
  in the design record.
- [On-demand tangents and densities cost per hit] → a cross product and a few
  divisions on a closest hit that is about to run a MaterialX or OpenPBR shade; the
  material fidelity suite (`docs/material_fidelity.md`) and `bench_ab.sh` on
  `materialx_teapot` bound it. If a normal-mapped scene measures a regression above
  noise, the tangent stays on demand and the density returns to a 4-byte table.
- [Global `u32` vertex indices cap a scene at 4 G vertices] → a `commit()` past the cap
  is a hard error naming the count; no sample or production scene is within a factor
  of 40 of it.
- [`PrimNode` shrinking exposes size assumptions] → `a_triangle_is_one_cache_line` is
  replaced by size tests on `TriangleRecord` (24), `Tri4i` (96), `PrimNode` (≤ 64) and
  `PrimRef` / `Node` (32).
- [The `f64` tie-break and the geometric-normal fallback read vertices from two
  sources (packet lanes or the table)] → both are the same `f32`s by construction;
  the kernel's bitwise test suite runs under `scripts/test_simd_matrix.sh` for both
  layouts.

## Deferred, in the order they would pay

1. **Tessellation on demand** (Benthin 2015). Keep every subdivision cage resident with
   a feature-adaptive `PatchTable` (opensubdiv-rs 0.1.4 ships `refine_adaptive`,
   `PatchTableFactory`, `PatchMap`, `evaluate_basis`, regular B-spline and Gregory
   patches — verified in the crate source), bound each patch conservatively by the
   convex hull of its control points, tessellate a patch to the level's grid when a
   ray first reaches its bound, and hold the tessellated micro-meshes plus their
   per-patch BVHs in a shared, budgeted cache (`CRUST_TESS_CACHE_MB`). Memory becomes
   independent of the level, as Embree's subdivision geometry is. Not bit-identical to
   uniform refinement: patch evaluation and level-by-level stencils sum in different
   orders. The patch table's coverage was the other blocker, and is now closed:
   opensubdiv-rs 0.1.4 capped only smooth interior extraordinary vertices with
   Gregory patches; 0.2.0 (2026-09-30, [#8](https://github.com/doubleailes/OpenSubdiv-rs/issues/8))
   added caps on boundaries, infinitely sharp creases, sharp corners and darts, and
   `useInfSharpPatch`; 0.3.0 (2026-10-01) closed the rest of the list filed against
   the reference — [#10](https://github.com/doubleailes/OpenSubdiv-rs/issues/10)
   patches over non-manifold spans, [#11](https://github.com/doubleailes/OpenSubdiv-rs/issues/11)
   single-crease patches for semi-sharp creases (`AdaptiveOptions::with_single_crease_patch`),
   [#12](https://github.com/doubleailes/OpenSubdiv-rs/issues/12) Loop box-spline patches,
   Gregory triangles and adaptive refinement, [#13](https://github.com/doubleailes/OpenSubdiv-rs/issues/13)
   stencil tables over adaptive hierarchies and `LimitStencilTableFactory` (limit
   position and derivatives at any `(ptex face, u, v)`, factorised to the base cage —
   the route to one sparse dot product per tessellation sample), and
   [#14](https://github.com/doubleailes/OpenSubdiv-rs/issues/14) the `smooth`
   triangle rule USD's `triangleSubdivisionRule` names. `PatchType::Quads` now
   appears only on unsharpened (`VtxBoundaryInterpolation::None`) boundaries and under
   the Bilinear scheme, where the reference defines no limit surface either; the
   crate's remaining roadmap item is the GPU back-ends, which this CPU renderer does not
   need. Two 0.3.0 behaviour changes reach the uniform path crust uses today and are
   checked by the dependency bump: Loop's refined children are ordered as the
   reference's `TriRefinement` orders them, and non-manifold edges are made infinitely
   sharp at the base level as `applyComponentTagsAndBoundarySharpness` does.
   crust-core requires 0.3.0 from this change on. It also touches the two-level traversal, the deterministic-build
   requirement (a cache fills in ray order; the tessellation itself must be
   deterministic per patch) and the material side tables. That is a change of its own,
   and it is easier after this one, since the record/table split is what a lazily
   filled leaf needs.
2. **Quantized BVH4 nodes** (Ylitie 2017 / Embree QBVH): 128 → ~64 B per node with
   outward rounding; image-identical up to tie order. Worth ~10 B per triangle once D4
   has made leaves full — before that, the node count is the problem, not the node
   size.
3. **The instance primitive.** `PrimNode::Instance` is an 80-byte enum slot plus a
   240-byte box (`l2w`, `w2l`, `normal_mat`, `Arc`, motion). `normal_mat` is the
   transpose of `w2l`'s linear part and the two affines are `Vec3A`-padded: ~200 B
   would hold the same data. The Moana island's 39.9 M instances are 10.65 GiB of
   "boxed primitives", a quarter of its kernel memory; this is the lever there, not
   triangles.
4. **A builder that never materialises the binary tree** (fused collapsing), for the
   remaining two thirds of the commit transient.
5. **The composed USD stage.** ALab's 32 GiB peak at level 0 holds 6.87 GiB of kernel;
   most of the rest is openusd's composed stage and preloaded textures, outside this
   capability.
