## Why

Subdivision is one level for the whole load (Hydra's `refineLevel`). On a production
set that is the wrong trade at both ends:
- **Level 1 is too much for the island as a whole.** It multiplies every Catmull-Clark
  cage by four, and did not fit in 61 GiB: killed at 56.8 GiB, still importing, on
  2026-10-01. Most of those triangles are kilometres from `shotCam` and cover a
  fraction of a pixel.
- **Level 0 is too little up close.** The few hero meshes near the camera still show
  their cage silhouettes.

RenderMan solves this by dicing to a screen-space rate rather than a fixed level. crust
can take the first step of that at import time: refine each mesh only as far as its
size on screen asks.

## What Changes

- **Adaptive subdivision, opt-in.**
  - The setting: `float crust:subdivisionEdgeLength` on the `RenderSettings` prim, a
    target cage-edge length in **pixels**, or `--subdiv-edge-length <px>` on the CLI,
    which wins.
  - When set, each subdivision mesh (scheme not `none`) is refined to the smallest
    level at which its mean cage edge, projected at its nearest distance to the render
    camera, is no longer than the target.
  - The level is capped by the existing `crust:subdivisionLevel` / `--subdiv-level`,
    which in adaptive mode means "maximum level", defaulting to 3 when neither is given.
  - Unset, nothing changes: one uniform level, default 0.
- **The level is per placement, so instancing keeps working.** A prototype placed at
  very different distances is built once per distinct screen-rate bucket (a power of
  two). Each placement uses the version its own distance and scale ask for: the
  level-of-detail for instances that production renderers use. Meshes that come out
  identical across buckets still share one BVH through the existing `MeshKey` content
  hash.
- **Conservative by construction.**
  - The distance is to the nearest point of the placement's world bounds, and is zero
    when the camera is inside them, which gives the maximum level.
  - Off-screen geometry is *not* coarsened by the frustum, so reflections and shadows
    of nearby meshes keep their detail.
  - The rate rounds up.
- **Deterministic.** A mesh's level is a pure function of its placement, the camera at
  shutter open, the resolution and the settings, never of import or chunk order.
- **It needs a camera before traversal**: `--camera` or `RenderSettings.camera`, read
  up front as it already is. Without one, adaptive mode warns once and falls back to
  the uniform level.
- **`--stats`** reports how many meshes were refined to each level, and how many
  prototype versions the buckets produced.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `usd-scene-import`: gains an "Adaptive subdivision level" requirement. Added rather
  than modified, so it composes with the in-flight `usd-driven-subdivision` change
  that defines the uniform level.
- `cli`: gains a requirement for `--subdiv-edge-length`, added for the same reason
  (`usd-driven-subdivision` and `compact-triangle-layout` both modify
  "Command-line argument parsing").

## Impact

- **Depends on `usd-driven-subdivision`** (the uniform level, `SubdivPolicy`,
  refinement in `mesh_source`). Implement after it is archived.
- **`crates/crust-core/src/scene/usd_import/`:**
  - `mod.rs`: resolve the target and the camera before traversal, opening a
    population-masked stage on the camera path when it lives under a payload;
  - `settings.rs`: `crust:subdivisionEdgeLength`;
  - `mesh.rs`: the per-placement level in `mesh_source`, with the cage's mean edge
    length computed once per cage;
  - `instancing.rs`: prototype parts cached per `(prototype, epoch, rate bucket)`,
    PointInstancer placements grouped by bucket, nested placements inheriting the
    outer rate;
  - `UsdImportOptions`: `subdivision_edge_length`.
- **`crates/crust-core/src/scene/subdiv.rs`:** `SubdivPolicy` gains the adaptive mode
  and its level function.
- **`crates/crust-render`:** the `--subdiv-edge-length` flag.
- **Memory and time:** an instanced prototype can now exist at several levels, and
  that cost is reported in `--stats`. The island is the measurement: adaptive at a
  2 px target against uniform level 0. The change records peak RSS, kernel memory, the
  level histogram and the image difference near camera.
- **Out of scope:**
  - per-face adaptive refinement (feature-adaptive patches, crack-free transitions
    within a mesh);
  - per-ray tessellation and caches;
  - frustum-aware coarsening of off-screen geometry;
  - a level that varies across frames of a sequence. Each frame imports at its own
    camera.
