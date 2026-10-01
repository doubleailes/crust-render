## Why

`compact-geometry-storage` removes the duplicated copies, but one is deliberate: each
triangle *reference* still carries its own vertices in a 192-byte `Tri4` packet. On the
Moana island that is the largest remaining per-triangle cost. Packets are 4.02 GiB for
60.9 M triangles, about 71 B per triangle once SBVH reference duplication and partial
packets are counted, against about 6 B for the positions of an indexed mesh. At
subdivision level 1 the triangle count grows about fourfold, and packets alone would be
on the order of 16 GiB.

Embree solves the same tension by letting a scene choose: `triangle4` (vertices copied
into the leaf, fastest) or `triangle4i` (vertex indices in the leaf, gathered at
intersection time, compact), under `RTC_SCENE_FLAG_COMPACT`. crust needs the same choice
so a scene that does not fit can trade some traversal speed for memory, without
changing the image.

## What Changes

- **A compact triangle layout in the kernel.**
  - A committed scene MAY keep each mesh's vertex positions once, as `[f32; 3]`, and
    store no triangle packets.
  - Leaves reference triangle records, which hold mesh-local vertex indices.
  - Traversal gathers the four referenced triangles into a stack `Tri4` and runs the
    same intersector.
  - The gathered values are the exact `f32`s the packed layout copies, so **every query
    result is bit-identical between the two layouts**. The layout is a memory/speed
    trade only.
- **A per-scene choice at commit.** The `SceneBuilder` gains a layout option (`Packed`,
  the default, or `Compact`), in the spirit of Embree's scene flags.
- **Host control**, with `packed` as the default everywhere:
  - `--geometry-layout packed|compact` on the CLI;
  - `token crust:geometryLayout` on the `RenderSettings` prim;
  - the CLI overrides the stage, as `--subdiv-level` does.
- **Normals share the position indices.** A compact mesh's normals are reached through
  the same mesh-local index triplet as its positions, plus a per-geometry base. The
  triangle record does not grow over `compact-geometry-storage`'s 24 bytes.
- **`--stats` reports the layout**, and kernel memory gains a `vertex positions` line.

Expected effect on the island (arithmetic, not measured):
- **Level 0:** the triangle share goes from about 101 B per triangle (24 B record, about
  6 B normals, about 71 B packets) to about 40 B (24 B record, about 6 B positions,
  about 6 B normals, about 4 B leaf reference). That is roughly −3.5 GiB.
- **Level 1:** with about 4× the triangles, roughly −14 GiB.
- **Speed:** the cost is a gather per packet test. Embree reports `triangle4i` as
  noticeably slower than `triangle4`, so this change measures it rather than assuming.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `intersection-kernel`: gains a requirement for a selectable compact triangle layout
  that is bit-identical to the packed one.
- `cli`: "Command-line argument parsing" gains `--geometry-layout`.
- `usd-scene-import`: "Render settings from USD with defaults" gains
  `crust:geometryLayout`.

## Impact

- **Depends on `compact-geometry-storage`**, which supplies `TriRecord`, the per-kind
  `Prims` arrays and the lane-vertex tie-break path. It also touches the same
  `cli` / `usd-scene-import` requirements as the in-flight `usd-driven-subdivision`,
  so the deltas here are written on top of that change's text. Archive
  `usd-driven-subdivision` first.
- **`crates/crust-rt`:**
  - `scene.rs`: the layout option on `SceneBuilder`, retained vertex buffers and
    per-geometry bases;
  - `bvh/collapse.rs`: no packets in compact mode, references only;
  - `bvh/mod.rs`: the gather-then-intersect leaf path for closest hit and occlusion,
    and the footprint;
  - `triangle.rs`: `Tri4` assembly from gathered vertices, shared with `Tri4::new`.
- **`crates/crust-core`:** `UsdImportOptions` / `RenderSettings` plumbing, a
  `WorldBuilder` / scene-commit option, and the stats lines.
- **`crates/crust-render`:** the CLI flag.
- **Tests:**
  - every sample renders bit-identical in both layouts;
  - `simd_matches_scalar_bitwise` runs over both layouts;
  - a footprint test per layout.
- **Out of scope:**
  - automatic layout selection (by memory budget or per hot mesh). It needs the
    measured speed cost from this change first;
  - quantised or compressed vertices and BVH nodes;
  - the crust-core `FaceMap` / `UvMap` tables.
