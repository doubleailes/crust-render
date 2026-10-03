## Why

Crust renders displaced assets as their undisplaced base surface, and says nothing about
it. `UsdPreviewSurface.inputs:displacement` is unread (`preview_surface.rs`). MaterialX's
`surfacematerial.displacementshader` is never followed. The Moana island's
`outputs:ri:displacement` and `inputs:displacementMap` are acknowledged only in comments
(`materials.rs`, textures `design.md` § Ptex). So silhouettes, contact shadows and
self-occlusion all come from the base mesh. A normal map can fake the shading, but not
the shape.

The pieces a displacement pass needs are now in place:
- **Tessellation exists.** Uniform refinement and per-face adaptive tessellation
  (archived 2026-10-02) produce dense tessellations, and adaptive mode sizes them by
  screen size.
- **Each new vertex knows where it is.** Every tessellated vertex already carries the
  chart position (UV, or Ptex face plus face-local coordinate) it was evaluated at.
- **The kernel stores arbitrary positions.** `Geometry::TriangleMesh` takes any vertex
  positions, so offset points need no kernel change.

Production path tracers built on pre-tessellation (Hyperion, Arnold, Manuka) displace in
exactly this place, at dicing time. Crust already pre-tessellates.

## What Changes

- **Scalar displacement at tessellation time.** After a mesh is tessellated, and before
  it is interned and handed to the kernel:
  - each unique vertex is moved along its pre-displacement smooth normal by a scalar
    evaluated from the bound material, in the mesh's local space;
  - shading normals are then recomputed from the displaced surface.
  The kernel, the integrator and render throughput per triangle are unchanged.
- **Three authoring paths are read:**
  - `UsdPreviewSurface.inputs:displacement`, as a constant or through a `UsdUVTexture`
    (channel, `scale` and `bias`), through the same input resolution as every other
    preview input.
  - MaterialX `surfacematerial.displacementshader` ← `displacement` node: a `float`
    `displacement` input times `scale`. The input subgraph is compiled to a one-root
    program, as `geometry_opacity` already is.
  - RenderMan / Moana: `outputs:ri:displacement` → `PxrDisplace`. The scalar is
    `dispScalar · dispAmount`, with `dispScalar` read from a Ptex or UV file. The value
    is read off the Material prim's interface where the island authors it, as
    `PxrDisneyBsdf` already is.
- **Watertight by construction.** A vertex shared by several faces is displaced once:
  - its value is sampled from the first face that references it, the rule the per-face
    tessellator already uses to deduplicate shared points;
  - so UV seams and Ptex face boundaries cannot open cracks.
- **Band-limited to the dicing rate.** Each displacement lookup's footprint is the local
  vertex spacing in chart units. A coarse tessellation reads a coarse mip level instead
  of aliasing the finest one.
- **Meshes with `subdivisionScheme = "none"`** that carry displacement are diced
  bilinearly, so the displacement has vertices to move. Today such a mesh never refines.
  The faces keep their flat shape before displacement.
- **Adaptive dicing accounts for displacement.** The frustum test and the per-mesh
  nearest-point distance use cage bounds padded by a displacement bound. The bound is:
  - exact for a constant;
  - otherwise `float crust:displacementBound` authored on the mesh or its material;
  - when neither applies, the frustum term is skipped for that mesh, so geometry that
    displacement pushes into view is never under-diced.
- **Ptex requests carry a colour space**, as UV texture requests already do. Displacement
  maps are read raw. Colour Ptex keeps today's gamma-2.2 decode, so existing renders are
  bit-identical.
- **Displacement runs once per distinct mesh.** It is applied when a new mesh is interned,
  keyed on the undisplaced source plus material. Prims sharing a mesh pay for it once.
- **`CRUST_DISPLACE`** (default on). `0` imports every mesh undisplaced, as before this
  change, for an honest A/B.
- **`--stats`** reports displaced meshes, displaced vertices, displacement time, the
  largest offset applied, meshes displaced at cage resolution, and meshes whose frustum
  term was skipped.
- **Unchanged for scenes without displacement.** Every existing sample renders
  bit-identical.
- **Out of scope:**
  - vector displacement (a MaterialX `vector3` `displacement`, `PxrDisplace.dispVector`):
    warned about and ignored;
  - displacement on Loop meshes beyond their per-mesh level;
  - lazy or per-ray dicing;
  - bump from detail finer than the dicing rate;
  - displacement on `UsdGeomSphere`.

## Capabilities

### New Capabilities

- `displacement`: scalar displacement of tessellated meshes. It covers:
  - which material bindings define it;
  - how it is evaluated, filtered and applied along the normal;
  - watertightness;
  - its interaction with uniform and adaptive dicing and with the displacement bound;
  - shading normals after displacement;
  - its switch and its `--stats` lines.

### Modified Capabilities

- `usd-scene-import`: "Subdivision surfaces follow the scheme". A `none` mesh carrying
  displacement is diced bilinearly instead of rendering its faceted cage.
- `textures`: "Known gaps" narrows the Ptex colour-space gap. Ptex requests carry a colour
  space, and displacement Ptex is read raw. Colour Ptex is still always decoded by gamma
  2.2.

## Impact

- **`crates/crust-core/src/scene/`:**
  - a new `displace.rs` that applies a `Displacement` to a `MeshSource`. It is pure and
    has no USD in it.
  - `usd_import/mesh.rs`: `mesh_source` dices displaced `none` meshes bilinearly, keeps
    per-vertex chart coordinates for displaced meshes, and calls the pass from
    `MeshArena::intern` only on a new slot.
  - `usd_import/adaptive.rs`: the padded frustum and nearest-point tests.
  - `subdiv.rs`: chart coordinates exposed per emitted vertex, and normals recomputed
    after displacement.
- **`crates/crust-core/src/scene/usd_import/`:**
  - `materials.rs` / `preview.rs`: material resolution returns an optional
    `Displacement` beside the `Material`, cached with it.
  - `PxrDisplace` and the island's interface inputs.
- **`crates/crust-core/src/material/`:**
  - `UvInput` gains a sampling entry point that needs no `HitRecord`;
  - `materialx.rs` gains the one-root displacement program, interpreted or JIT-compiled.
- **`crates/crust-mtlx`:** follows `displacementshader` and exposes its input as a root.
  No crust types are introduced.
- **`crust_core::AssetLoader::load_ptex`** gains a `ColorSpace` argument, implemented in
  `crust-assets` on both the preloaded and the streamed path. The streamed ↔ preloaded
  bit-identity pin covers raw data too.
- **`crates/crust-core/src/config.rs`:** `CRUST_DISPLACE`. It needs a row in
  `docs/architecture.md` § Environment switches and an entry on the user-docs page.
- **`crates/crust-core/src/stats.rs`:** displacement counters.
- **User docs (`site/`):**
  - `usd/materials.md`: displacement inputs;
  - `usd/geometry.md`: `crust:displacementBound` and the `none` scheme rule;
  - `reference/environment-variables.md`;
  - architecture limitations.
- **Design records:**
  - `usd-scene-import`: the new section;
  - `materials`: drop the "`displacement` not read" line;
  - `textures`: the Ptex colour space, and the island's `displacementMap`.
- **Samples and tests:**
  - a new `samples/displacement.usda` covering a preview surface with a UV height map,
    MaterialX, and raw Ptex;
  - unit tests for offsets, watertightness across seams and Ptex faces, raw Ptex, and
    bit-identity with `CRUST_DISPLACE=0`;
  - `check_images.sh` unchanged on every existing sample.
- **Performance:**
  - import pays one texture lookup per displaced vertex, plus a normal recompute;
  - render throughput per triangle does not change;
  - memory follows the dicing rate the user asks for. A `none` mesh that becomes
    displaced gains per-vertex normals (12 B per vertex).
- **Ordering:** independent of the in-flight `compact-triangle-layout`. The kernel's
  view of a mesh, positions plus normals, does not change shape.
