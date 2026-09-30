## Why

Subdivision made the renderer's memory scale with the refinement level, and the
per-triangle cost it scales by is far above what the data needs. Every refined
triangle is stored **three times** in the kernel: as an 80-byte primitive node holding
its own copy of the three vertices, again inside its 4-wide SIMD packet, and — for a
subdivided mesh — as three unshared shading normals (48 B per triangle for what is one
12-byte normal per vertex). The importer keeps a fourth set of per-*corner* tables
beside it (UVs, tangents, densities, Ptex sub-face corners), and the SBVH build holds
48-byte references and 48-byte binary nodes for every triangle while it runs.

Measured on the numbers already in the design records (`usd-scene-import/design.md`,
`docs/alab_profile.md`, `docs/moana_profile.md`):

| | |
|---|---|
| ALab frame 1004, level 0 → level 1 | +60.7 M triangles, kernel 6.87 → 19.64 GiB, peak RSS 32.3 → 49.0 GiB |
| kernel bytes per refined triangle (that delta) | **~210 B**; ~275 B per triangle at peak, once the importer's tables and the build transient are counted |
| DPEL teapot, cage → level 2 | 64 930 → 1 039 462 triangles, kernel 11.05 → 212.95 MiB (215 B/triangle) |
| triangle packets, ALab / Moana | 2.35 GiB for 21.2 M triangles, 5.71 GiB for 60.9 M: **119 and 100 B per triangle** for a 48-byte lane, i.e. lanes are less than half full |
| BVH4 nodes, ALab / Moana | 66 and 63 B per primitive: one 128-byte node per two primitives, because leaves average about two |

A subdivided quad mesh has about as many vertices as faces and two triangles per face,
so the information content of a refined triangle is roughly 6 B of position, 6 B of
normal and 12 B of indices. Level 2 on ALab, or level 1 on the Moana island, do not fit
a 61 GiB machine today; the same triangles stored once would.

## What Changes

Everything here is **lossless**: the same `f32` vertices, indices, normals and UVs are
stored, only fewer times and in narrower records. Nothing is quantized. The
state-of-the-art survey behind the choices (Embree's compact layouts, compressed-leaf
and quantized-node BVHs, tessellation caching, PLOC++/fused collapsing, DGF) is in
`design.md`; the lossy techniques it lists are explicitly *not* adopted.

- **Kernel: triangles leave `PrimNode`.** A committed scene keeps one shared
  vertex table and one shared per-vertex normal table (`[f32; 3]`, 12 B each) and a
  24-byte triangle record (`geom_id`, `prim_id`, three vertex indices, mask). The
  `PrimNode` enum keeps only the primitives that are not triangles. Shading normals are
  interpolated from the per-vertex table through the record's indices, so a subdivided
  mesh pays 12 B per vertex for normals instead of 48 B per triangle.
- **Kernel: two packet layouts, chosen per tree at commit.** The existing gathered
  SoA `Tri4` (192 B, fastest in cache) and a new indexed `Tri4i` (96 B: four lanes of
  vertex indices and record ids, vertices gathered from the shared table at test time)
  are **bit-identical** to each other and to the scalar test — the lanes hold the same
  `f32`s either way — pinned by a bitwise test like `simd_matches_scalar_bitwise`. A
  tree above a triangle-count threshold uses `Tri4i`; `CRUST_TRI_PACKETS=gathered|indexed|auto`
  forces either side (`gathered` is the behaviour this replaces).
- **Kernel: leaves sized for packets.** The SAH leaf cost counts packets
  (`ceil(n / 4)`) instead of triangles, so all-triangle leaves fill their lanes (the
  measured 48 % fill is the single largest waste). This is the one item that changes
  the tree's *shape*, so its images may differ on exact-tie hits only; it ships behind
  `CRUST_BVH_PACKET_SAH` (off = today's per-triangle cost) and is accepted only after the
  1/√N noise proof of `CLAUDE.md` § Measuring a change.
- **Kernel: a lighter build transient.** `PrimRef` and the binary `Node` shrink from 48
  to 32 B, subtrees are written into one pre-sized arena instead of `merge`
  concatenating child arrays, and the build reads vertices through the shared table
  rather than from a second copy. Decisions stay input-only, so the build-twice test
  holds.
- **Kernel API.** `Scene::triangle_vertices(geom_id, prim_id)` exposes a hit
  triangle's (local-space) vertices, which the importer's on-demand tables need.
- **Importer: per-corner tables become on-demand or indexed.** World-space tangents
  and texture-footprint densities are computed at the hit from the shared vertices and
  the corner UVs instead of being stored per triangle (16 + 4 B per triangle) — which
  also gives **instanced meshes tangents**, retiring the "normal maps on instanced
  geometry fall back to the geometric normal" gap. A face-varying chart is kept as its
  values plus per-corner indices (≈17 B per triangle) instead of expanded corners
  (24 B). A subdivided mesh's Ptex sub-face corners are an 8-byte per-face patch
  parameter (`base face, dyadic origin, depth, rotation`) instead of 24 B of corner UVs
  per triangle; the corners it reproduces are dyadic fractions, exact in `f32`.
  Importer-side vertex and normal arrays are `[f32; 3]`, not padded `Vec3A`.
- **Refiner tail.** `subdivide()` drops the retained refiner before it materialises
  the limit copies, the third of its ~313 B/face transient the design record already
  names.
- **`--stats` shows the effect.** The kernel-memory block gains rows for vertices,
  per-vertex normals, triangle records, packets by layout with their lane fill, and a
  `bytes per triangle` line, so a regression is visible in the report rather than in
  RSS.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `intersection-kernel`: triangle storage (shared vertex/normal tables, records,
  two bit-identical packet layouts), packet-aware leaf sizing, the build transient, the
  vertex accessor; the memory footprint report.
- `usd-scene-import`: per-corner side tables replaced by on-demand and indexed ones;
  tangents on instanced meshes.
- `cli`: the `--stats` kernel-memory rows and two environment switches.
- `textures`: "tangents on instanced geometry" leaves the known-gaps list.

## Impact

- **Code.** `crust-rt`: `prim.rs` (triangle record, `PrimNode` without triangles),
  `triangle.rs` (`Tri4i`), `bvh/{mod,build,collapse}.rs` (shared tables, `PrimSource`
  for the build, arena, packet-aware leaf cost, footprint), `scene.rs` (tables at
  commit, `triangle_vertices`, `MemoryFootprint`). `crust-core`: `rt_world.rs` (on-demand
  tangents and densities, indexed `UvMap`, patch-param `FaceMap`), `scene/subdiv.rs`
  (tail, `[f32; 3]`, `PatchParam` output), `scene/usd_import/mesh.rs` (`MeshGeom`,
  `triangulate`, `remap_subdivided_faces`), `config.rs` (two switches), `stats.rs`
  (rows). `docs/architecture.md` (switch rows, invariant list), the four design records.
- **Output.** Bit-identical for every item but the packet-aware leaf cost
  (`check_images.sh check` against goldens recorded before the change, with
  `CRUST_BVH_PACKET_SAH=0`); with it on, differences are confined to exact-tie hits and
  must fall as 1/√N.
- **Memory (targets, model in `design.md`).** For a refined quad mesh, kernel-resident
  bytes per triangle from 196 today on the stress grid (210 on ALab) to **≤ 110 gathered / ≤ 85 indexed**; the
  importer's side tables from 44 (UV-textured) or 57 (Ptex) B per triangle to ≤ 20; the
  top-level build transient by a third. Acceptance is measured, not modelled: the
  `gen_subdiv_stress.py` scene at level 3 with `--stats`, the DPEL teapot at level 2, and
  ALab at level 1 where the machine allows.
- **Speed.** Gathered packets keep today's inner loop; the record table adds nothing on
  the miss path and one 24-byte read on a candidate hit. Indexed packets pay a
  12-vertex gather per packet test (Embree's `Triangle4i` trade) and are used only
  where the tree is large enough that memory traffic, not arithmetic, bounds
  traversal — the size threshold is set from `ray_throughput --large` A/Bs, per
  `docs/simd.md`'s "out of cache the answer flips" finding. Any change to `Bvh::hit` is
  callgrind-compared per `CLAUDE.md`.
- **Not in this change** (recorded in `design.md` § Deferred): tessellation on demand
  behind a bounded cache (Benthin 2015; opensubdiv-rs 0.1.4 ships the patch table it
  needs), quantized BVH4 nodes, the 320-byte instance primitive that dominates the
  Moana island, the composed USD stage's residency.
