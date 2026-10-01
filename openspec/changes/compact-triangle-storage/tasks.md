## 1. Baseline

- [x] 1.1 Record goldens for every sample scene with the current binary (`scripts/check_images.sh record <dir>`, 16 spp, `--indirect-clamp 0`) and keep that binary for `bench_ab.sh`; verified by one EXR per sample in `<dir>`
- [x] 1.2 Record `--stats` for `gen_subdiv_stress.py` at level 3 (kernel 416.89 MiB = 196 B/triangle: nodes 80, normals 48, packets 49, BVH 15, leaves 4; commit peak 808.79 MiB against a 284 MiB traverse RSS) and for `samples/subdivision.usda` at `--subdiv-level 4` (15 376 triangles, kernel 2.92 MiB = 199 B/triangle); verified by the numbers copied into the design record's baseline table
- [x] 1.3 Callgrind `Bvh::hit` / `intersect_leaf` instruction counts on cornellbox and materialx_basic at `-s 2`, single thread, as the codegen baseline for D1–D3; verified by the counts noted for task 8.2 (`Bvh::hit` + its instance recursion: cornellbox 947 550 540, materialx_basic 282 853 419)

## 2. Kernel: records and shared tables (D1, D3, D5)

- [x] 2.1 `TriangleRecord { geom_id, prim_id, v: [u32; 3], mask }` in `prim.rs` and a size test pinning 24 B; `PrimNode` loses its `Triangle` variant and a test pins it at ≤ 64 B; verified by `cargo test -p crust-rt`
- [x] 2.2 `SceneBuilder::commit` concatenates every `TriangleMesh` into `vertices: Box<[[f32; 3]]>`, `normals: Box<[[f32; 3]]>` and `records: Box<[TriangleRecord]>` with global indices and a per-geometry `normals_base`, moving each geometry's arrays before the build, erroring past `u32::MAX` vertices; verified by a test on two meshes (one with normals, one without) reading back every vertex and normal by global index
- [x] 2.3 `PrimSource` (records + tables + remaining `PrimNode`s) answering `bbox`, `clipped_aabb`, `as_triangle` by index; `build_subtree`, `collapse` and `push_leaf` read through it; verified by `build_twice_is_identical` and the kernel test suite
- [x] 2.4 `PrimRef` (28 B) and `Node` (32 B) store unpadded bounds (size-tested); the leaf, packet and index tables are sized exactly from the binary leaves; `merge` splices the right subtree into the left's vectors instead of a third allocation (a pre-sized arena needs a node bound the SBVH's duplicating splits do not give up front); verified by `build_is_deterministic`, every BVH test, and the commit-peak measurement of task 8.1
- [x] 2.5 `hit_from_barycentric` and the `f64` tie-break read the record and gather normals / vertices from the tables; geometric-normal fallback from the lane's vertices; verified by `simd_matches_scalar_bitwise` and by `check_images.sh check` against 1.1 being bit-identical on every sample
- [x] 2.6 `Scene::triangle_vertices(geom_id, prim_id) -> Option<[Vec3A; 3]>`, local space for an instanced scene; verified by a test on a baked and an instanced mesh comparing against the attached arrays

## 3. Kernel: indexed packets (D2)

- [x] 3.1 `Tri4i` (92 B, size-tested): per-lane vertex indices plus the shared `LaneMasks` (records, `active`, masks); `intersect` gathers into the same nine `Vec4`s and calls the shared `intersect_lanes`; the layout is per tree (`Bvh::layout`), not per leaf; verified by `tri4i_matches_tri4_bitwise` (4 000 random packets × 3 masks, hits, `fallback`, `t`/`u`/`v` bitwise) and `packet_layouts_are_bit_identical` (a committed scene, 3 000 rays, closest-hit and occlusion)
- [x] 3.2 `Config::tri_packets: gathered | indexed | auto` (`CRUST_TRI_PACKETS`, default `auto`), every crust-core kernel commit goes through `packet_layout()` → `SceneBuilder::commit_with`; `auto` picks `Tri4i` above `crust_rt::INDEXED_PACKETS_FROM`; verified by the config test per spelling (and a bad value keeping `auto`) and `auto_layout_gathers_small_scenes` / the footprint's per-layout rows
- [x] 3.3 Measured `ray_throughput --layout` in cache and `--large 4` out of cache: indexed is slower at every size (1–8 % in cache, 30 % on the 4 M-triangle soup; stress-grid render −13 %) for 104 → 79 kernel B/triangle, so there is no threshold: `auto` is gathered and `indexed` the explicit memory trade; the design record's D2 records the numbers against the expectation it replaced
- [x] 3.4 `scripts/test_simd_matrix.sh -p crust-rt` clean under every codegen configuration, no FMA contraction (the kernel's own tests build both layouts explicitly, so the switch does not enter); verified by its exit code

## 4. Kernel: packet-aware leaves (D4)

- [ ] 4.1 `Config::bvh_packet_sah` (`CRUST_BVH_PACKET_SAH`, default on); the leaf decision charges all-triangle ranges `ceil(count / 4)` and other ranges `count`; verified by a build test on 8 coplanar triangles yielding one leaf of two full packets with the switch on and more with it off
- [x] 4.2 Noise proof on the one sample whose goldens differ (veach_mis: relmse 4.56e-7 / 5.03e-8 / 5.63e-9 at 16 / 64 / 256 spp, one or two pixels, falling faster than 1/N) and on two controls (cornellbox 0 / 0 / 0, instancing 0 / 0 / 3.97e-11); the other 26 samples are bit-identical at 16 spp; verified by the table in the design record
- [x] 4.3 Lane fill and node bytes on/off from `--stats` (neither ALab nor Kitchen_set is in this checkout): stress grid nodes 31.76 → 18.27 MiB, 104.2 → 95.5 B/triangle, lanes 98.6 → 99.2 %; cornellbox nodes 34.38 → 23.50 KiB, 125 → 115 B/triangle, lanes 78 → 80 %; `subdivision.usda` level 4 107.4 → 100.8; `instancing` lanes 64 → 75 %; timing by an interleaved on/off loop (`bench_ab.sh` compares binaries, this is one binary) recorded in the design record
- [ ] 4.4 Re-record goldens with the switch on, in the same commit as 4.2's proof, with `check_images.sh check` under `CRUST_BVH_PACKET_SAH=0` still bit-identical to 1.1; verified by both script runs

## 5. Importer: on-demand and indexed side tables (D6)

- [x] 5.1 `World` records the scene and transform of each direct static instance (`SideTables::placement`) and reads a hit triangle's world-space vertices through `Scene::triangle_vertices` (top-level scene for baked meshes); verified by `instanced_meshes_get_tangents_through_their_placement`
- [x] 5.2 `UvMap` becomes `values` + `corners`; `resolve` derives the tangent at the hit (`tangent_of`, the former `build_tangents` arithmetic) from the vertices `World` reads; the stored `tangents` vector is removed, the density table (local frame) stays; verified by the tangent tests in `world_material.rs` and by `check_images.sh check` being bit-identical on every sample
- [x] 5.3 Normal maps on directly instanced meshes shade with a tangent frame (prototype parts through a group and motion-blurred instances remain the gap, so `textures/spec.md` narrows it); verified by `instanced_meshes_get_tangents_through_their_placement` (a rotated placement's tangent is the chart's +u rotated with it)
- [x] 5.4 `FaceMap` stores one 8-byte `SubFace` per triangle (dyadic origin, depth and origin corner), derived exactly from the refined channel's corners at remap time, and `resolve` reconstructs the corners; the channel itself is still refined (the transient stays; deriving cells from the child relations is a follow-up); verified by `sub_face_round_trips_every_cell_exactly` (every cell to depth 6, every rotation, bitwise), `subdivided_face_table_resolves_into_the_base_face` and `ptex_quads.usda` at `--subdiv-level 2` bit-identical to 1.1
- [x] 5.5 `MeshGeom`, `MeshSource`, `SubdividedMesh`, `Geometry::TriangleMesh` carry `[f32; 3]` positions and normals; `bake_verts` / `bake_normals` / `smooth_normals` adapt; verified by `cargo test --workspace` and `check_images.sh check`
- [x] 5.6 `subdivide()` drops the refiner before the result's copies; the probe's resident ceilings follow the new measurements (44.0 / 84.0 / 68.1 B/face, ceilings 53 / 100 / 82; the probe counts requested bytes, so the earlier drop shows in RSS, not in its table); verified by the probe passing at the new ceilings

## 6. `--stats` (D8)

- [x] 6.1 `MemoryFootprint` gains `vertices`, `vertex_normals` (per vertex), `triangle_records`, `packets` / `packets_indexed`, `geometry_tables`, `lanes` / `lanes_filled`; the report prints them plus `lanes filled` (%) and `bytes per triangle`; verified by `report_shows_the_geometry_layout` (every row, the fill and the per-triangle arithmetic)
- [x] 6.2 `cli/design.md` cookbook gains the memory-layout recipe (`CRUST_TRI_PACKETS` on the stress grid; `CRUST_BVH_PACKET_SAH` joins it with section 4); verified by running each line

## 7. Switches and records

- [x] 7.1 `docs/architecture.md`: rows for `CRUST_TRI_PACKETS` and `CRUST_BVH_PACKET_SAH`, the bit-identity pair "`Tri4` ↔ `Tri4i`" and "derived, not stored" under Invariants, the debt register's paid-down entry and the remaining build-transient item; verified by `grep -n CRUST_TRI_PACKETS docs/architecture.md`
- [x] 7.2 `intersection-kernel/design.md` (storage, packet layouts and the measured no-threshold outcome, leaf rule with its noise proof and timing, build transient), `usd-scene-import/design.md` (stress numbers after the change, resident B/face, probe), `textures/design.md` (tangents derived at the hit, remaining gap), `cli/design.md` (cookbook); the acceptance list carries its outcomes; verified by every target in `design.md` § Memory model having a measured value or a stated reason it has none

## 8. Acceptance

- [x] 8.1 Stress grid level 3: 95.5 B/triangle gathered (packet-sized leaves), 78.6 indexed; commit peak minus traverse RSS 285 MiB against 524 before (the 260 figure assumed a 390 MiB baseline that was mis-derived; two thirds of the real one is 349); whole-process peak 809 → 508 MiB; verified by `--stats` output quoted in the design record
- [x] 8.2 Callgrind `Bvh::hit` with its instance recursion, gathered: cornellbox 953.4 M against 1 001.6 M (−4.8 %), materialx_basic 291.9 M against 282.9 M (+3.2 %, a ten-triangle scene where per-query setup dominates); the 1 % band was written for a codegen-neutral refactor and this is not one — the per-line attribution, the inlining trap it found and the fix are in the design record; verified by the two counts
- [x] 8.3 `bench_ab.sh` base against the records build, eight samples, three reps: min −4.5 % (openpbr_showcase), −3.7 % (veach_mis), −3.0 % (materialx_basic), −2.7 % (cornellbox), −1.2 % (teapot), +1.0 % (instancing), +3.4 % (subdivision, a 0.56 s render) and curves at 5 ms; the leaf rule itself is neutral in cache (interleaved on/off: cornellbox 7.51 / 7.50 s, veach_mis 12.37 / 12.40 s) and +7 % on the stress grid's single run
- [ ] 8.4 DPEL teapot level 2 and, where a machine holds it, ALab level 1: kernel and peak RSS per the design record's acceptance rows; verified by `--stats` — **not run**: neither the teapot payload nor ALab is in this checkout
- [ ] 8.5 `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --no-fail-fast`, the nightly `bvh8` leg, `openspec validate compact-triangle-storage --strict`; verified by their exit codes
