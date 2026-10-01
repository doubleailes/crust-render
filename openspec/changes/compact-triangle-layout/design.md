## Context

This change starts from the layout `compact-geometry-storage` leaves behind:

- `Prims.tris: [TriRecord]` holds 24 B per triangle: `geom_id`, `prim_id`, mask, and an
  absolute normal-index triplet with a `NO_NORMALS` sentinel.
- `Prims.normals: [[f32; 3]]` holds each smooth mesh's normals once.
- `Tri4` packets are the only home of positions. The scalar tie-break and the
  geometric-normal fallback read `Tri4::lane_vertices`.
- The SBVH build runs over transient `BuildPrim`s with full triangles. `collapse`
  turns each all-triangle leaf range into contiguous packets, and `Tri4::prim[lane]`
  indexes `tris`.

Facts that constrain the design:

- **Every committed scene is built during the import, not after it.** Prototype scenes
  are built during traversal, and the top level at the end. So the layout has to be
  known before traversal starts, the same way the subdivision level is
  (`resolve_subdiv_level` in `usd_import/mod.rs`).
- **SBVH duplicates references**, so a triangle can appear in several leaves. Packets
  copy it once per reference; indices do not.
- **An existing per-triangle rule:** a triangle is smooth only if all three of its
  indices fall inside its mesh's normal array. Otherwise it is flat, even inside a
  smooth mesh.

## Goals / Non-Goals

**Goals:**
- A `Compact` layout whose query results are bit-identical to `Packed` for every input,
  including partial normal arrays and shared-edge ties.
- Per-triangle resident cost of about 24 B, plus about 12 B per vertex for positions and
  12 B per vertex for normals, with no packets.
- A measured speed cost, so a later change can decide on automatic selection.

**Non-Goals:**
- Mixing layouts inside one committed scene. The choice is per scene, and since each
  instanced prototype is its own scene, a later policy can still pick per prototype.
- Changing `Packed` in any way. Its traversal must not get slower from this change.
- Lossy encodings: quantised positions, oct-encoded normals, compressed nodes.

## Decisions

### 1. Layout is a `SceneBuilder` option, chosen by the host before import

`SceneBuilder::with_layout(TriangleLayout::{Packed, Compact})`, with `Packed` as the
default. crust-core resolves the layout once per render:
1. `--geometry-layout`, through `UsdImportOptions`;
2. else `crust:geometryLayout` from the `RenderSettings` prim, read with the
   subdivision level before traversal;
3. else `packed`.

Every `SceneBuilder` the import creates gets that layout, for prototypes and the top
level alike. An unknown token warns once and stays `packed`.

- **Alternative: an environment switch.** Rejected as the primary control.
  Environment switches here exist to A/B an optimisation against the code it replaced
  (CLAUDE.md), and this is a user-facing memory/speed trade with no "old behaviour"
  side. A CLI flag plus a render setting match how `--subdiv-level` works.
- **Alternative: an automatic heuristic now** (scene triangle count, or a memory
  budget). Deferred. A threshold needs the speed cost this change measures.

### 2. Compact storage: one vertex buffer per scene, a per-geometry table

Compact mode adds to `Prims`:

```text
positions: Box<[[f32; 3]]>   // every mesh's vertices, appended once per mesh
meshes:    Box<[MeshBase]>   // indexed by geom_id: { vert_base, normal_base, n_normals }
```

`TriRecord` keeps its 24 B and shape, but in compact mode its triplet holds **absolute
position indices** (`vert_base + i`):
- **Positions** read `positions[v[k]]`.
- **Normals** read `normals[normal_base + (v[k] - vert_base)]`, if all three local
  indices are below `n_normals`. Otherwise the triangle is flat.

This is the same per-triangle rule the packed commit applies, evaluated at hit time
instead of commit time, so a partial normal array behaves identically. Non-mesh
`geom_id`s get an empty `MeshBase`.

Positions are stored as `[f32; 3]` and converted to `Vec3A` on gather. The conversion
is exact.

- **Alternative: mesh-local indices plus `vert_base` looked up per triangle.**
  Equivalent. Absolute indices save that lookup on the hot gather, and only the
  smooth-normal path needs the `MeshBase`.
- **Alternative: separate position and normal triplets.** Rejected. Geometry normals
  are per vertex, indexed by the same `indices`, so one triplet serves both, and the
  record does not grow.

### 3. Leaves reference records; traversal gathers into a stack `Tri4`

In compact mode `collapse` emits no packets. For each all-triangle leaf it pushes the
`tris` indices into a `tri_refs: [u32]` array: 4 B per reference, where a packet costs
48 B. `Leaf` reuses `pkt_first` / `pkt_count` as a range in `tri_refs`; the meaning is
fixed by the scene's layout.

At traversal, each group of up to four references is gathered into a `Tri4` on the
stack, through the same constructor `Tri4::new` uses: an extracted `Tri4::from_lanes`
that both call. Tail padding repeats the last real triangle, exactly as at commit. The
existing `Tri4::intersect`, the scalar tie-break (reading `lane_vertices` of the stack
packet) and `hit_from_barycentric` then run unchanged.

The packed and compact layouts therefore feed the intersector identical `f32`s in an
identical lane order, which is what makes the results bit-identical:
- the order of `tri_refs` is the order `collapse` would have packed;
- grouping into fours follows the same leaf ranges.

- **Alternative: a scalar loop over the referenced triangles.** Rejected. It would be
  simpler, but it would give up the 4-wide intersector and change the order of
  closest-hit tie resolution relative to `Packed`. The strict `>` comparison in
  `intersect_leaf` makes the order observable on exact ties.
- **Alternative: cache gathered packets.** Rejected. It would bring back the memory
  this layout exists to save.

### 4. The dispatch cost is paid once per leaf, not per node

`Bvh` holds the layout as a field, and the leaf functions branch on it once per leaf
visit. Node traversal is untouched. `Packed` pays one predictable branch per leaf. If
callgrind shows that costs measurable instructions, the alternative is to make
`intersect_leaf` / `occlude_leaf` generic over a layout type parameter, with the `Bvh`
calling the right instantiation once per query.

### 5. Reporting

- `MemoryFootprint` gains `positions` and `tri_refs`. `packets` is zero in compact
  mode.
- `--stats` prints `geometry layout  packed|compact` beside kernel memory, and the
  `vertex positions` / `triangle references` lines when they are non-zero.
- The import logs the chosen layout once at DEBUG, and once at INFO only when it is
  not the default. INFO is bounded per render.

## Risks / Trade-offs

- **The gather is a random access per vertex per lane**: up to 12 scattered loads per
  packet test, where packed loads 192 contiguous bytes. Traversal could be markedly
  slower on scenes whose working set exceeds cache.
  → Mitigation: this is why the layout is opt-in. The change measures the cost with
  `bench_ab.sh` (same binary, both flags, interleaved) on cornellbox, Kitchen_set,
  ALab and the island, and records it in the design record before any automatic
  policy is proposed.
- **Bit-identity depends on the gather order matching the commit order** of
  `Tri4::new`.
  → Mitigation: one shared `Tri4::from_lanes`, a kernel test that casts the same rays
  against both layouts and compares results bitwise, `check_images.sh` in both
  layouts, and `simd_matches_scalar_bitwise` extended to gathered packets.
- **The spec interacts with in-flight deltas.** The `cli` and `usd-scene-import`
  requirements modified here are also modified by `usd-driven-subdivision`, and the
  deltas here contain that change's text.
  → Mitigation: archive `usd-driven-subdivision` before this change, and re-check the
  delta text at archive time.
- **Leaf field reuse (`pkt_first` / `pkt_count`) is layout-dependent.**
  → Mitigation: give the fields neutral names (`tri_first` / `tri_count`) and document
  that their meaning is per layout.

## Migration Plan

No migration is needed: `packed` stays the default, and its output and speed must be
unchanged against the parent commit (`check_images.sh` plus `bench_ab.sh`). Compact is
opt-in. Rollback is a revert.
