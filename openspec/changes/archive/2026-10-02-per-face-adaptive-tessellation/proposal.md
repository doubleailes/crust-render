## Why

Adaptive subdivision (`adaptive-subdivision`) picks **one level per mesh**, from the
mesh's nearest point to the camera. That works where near meshes are hero-sized (ALab
at 2 px: fewer triangles and less memory than uniform level 1, with 240 meshes at level
3), and fails where one mesh spans near and far. The Moana island's terrain and beach
meshes are kilometres wide and pass close to `shotCam`, so each is refined whole to the
ceiling: at a 2 px target the import passes 56 GiB at a ceiling of 2 or 3, and fits only
capped at 1 (2026-10-01).

RenderMan, Arnold and Cycles all refine **per patch**: each face is diced at the rate its
own size on screen asks, and neighbouring faces agree on their shared edge so no crack
opens. `opensubdiv-rs` 0.3.0, which crust already depends on, ports the pieces this
needs: feature-adaptive refinement (`TopologyRefiner::refine_adaptive`), the limit patch
table (`PatchTable`, `PatchMap`) and its evaluation with first derivatives.

## What Changes

- **Per-face tessellation in adaptive mode.** With a target edge length set
  (`--subdiv-edge-length`, `crust:subdivisionEdgeLength`), a subdivision mesh is no
  longer refined uniformly to one level. Its limit surface is sampled per cage face, on
  a grid as fine as that face's size on screen asks.
  - **Edge rates.** Each cage edge is split into `n` segments from its own projected
    length at its own nearest point to the camera, rounded up, between 1 and
    `2^max` (the ceiling keeps its meaning: at most the density of uniform level `max`).
    An edge is decided once, from data both of its faces share, so the two faces
    always agree and the tessellation is watertight.
  - **Face interiors** are gridded at the rate of their edges and stitched to edges
    split at a different rate, so a face between a near and a far neighbour has no
    T-junction.
  - **On the limit surface.** Every vertex is a limit-surface point evaluated from the
    patch table, with the normal from its derivatives, as uniform refinement snaps its
    vertices today. At a uniform rate of `2^L` the vertices are the uniform level-`L`
    limit points.
- **Only unshared geometry is adaptive** (MoonRay's rule: it sets the adaptive error of
  every shared primitive to 0). A direct mesh, and a prototype placed exactly once in
  its top-level subtree, are tessellated per face through their world transform. A
  prototype placed more than once is refined to the uniform level
  (`--subdiv-level` / `crust:subdivisionLevel`, default 0), whatever its distance. This
  **replaces** `adaptive-subdivision`'s per-placement prototype versions (rate buckets
  and the prototype survey), which spent memory on shared geometry, and fixes the
  island, whose terrain elements are `instanceable` but placed once.
- **Ptex** reads the sample's own patch coordinates, which *are* Ptex coordinates.
  Triangles carry explicit corner UVs for these meshes, since a stitched grid is not
  made of dyadic cells.
- **UV charts** are evaluated with the same patches: a `vertex` chart with the position
  basis, a `faceVarying` one with face-varying patches (opensubdiv-rs 0.4.0), its UVs
  per triangle corner so seams keep each side.
- **Patches only where they are needed** (opensubdiv-rs 0.5.0, selected faces): a face
  whose every edge is split once renders its smooth cage, as level 0 does and as MoonRay
  does with a tessellation factor of 0.
- **A frustum term:** geometry wholly outside the view is split once
  (`CRUST_ADAPTIVE_FRUSTUM`, default on), as MoonRay leaves out-of-view faces at their
  cage.
- **Isolation depth 1**, with Gregory patches where it leaves the surface irregular: the
  memory of an all-triangle cage grows with the depth.
- **`CRUST_ADAPTIVE_PER_FACE`** (default on): `0` is the per-mesh level behaviour it
  replaces, for an honest A/B.
- **`--stats`** reports the per-face meshes, the fallbacks, the shared meshes and the
  distribution of edge rates.
- **Not bit-identical** to per-mesh adaptive mode, by construction. Uniform mode (no
  target) is untouched and stays bit-identical.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `usd-scene-import`:
  - **modifies** "Adaptive subdivision level": shared prototypes take the uniform level
    instead of per-placement versions, a prototype placed once counts as unshared, and
    placements are counted per top-level subtree;
  - **adds** "Per-face adaptive tessellation".

## Impact

- **Depends on `adaptive-subdivision`** (target, camera before traversal, rate buckets,
  stats). Implement after it is archived.
- **`crates/crust-core/src/scene/`:**
  - `subdiv.rs`: an adaptive-refinement path beside `subdivide()`: build the
    `PatchTable`, evaluate limit points and normals at given patch coordinates.
  - a new `tessellate.rs`: edge rates, per-face grids, stitching, shared edge
    vertices, Ptex corner coordinates. Pure, no USD.
  - `usd_import/mesh.rs`: `mesh_source` chooses per-face tessellation in adaptive mode
    for unshared Catmull-Clark and bilinear meshes, or the per-mesh level for a `loop`
    one.
  - `usd_import/adaptive.rs`: the per-edge rate function.
  - `rt_world.rs`: a `FaceMap` variant with explicit corner UVs.
- **`crates/crust-core/src/config.rs`**: `CRUST_ADAPTIVE_PER_FACE`, with its
  `docs/architecture.md` row and user-docs page.
- **Memory**: per-face meshes need only the feature-adaptive hierarchy (proportional to
  creases, extraordinary vertices and isolation depth), not `4^L` refined levels, and
  emit only the triangles their size asks for. Explicit Ptex corners cost 24 B per
  triangle on Ptex meshes, against 4 B for a dyadic cell.
- **Measurement**: the island at 2 px with the default ceiling of 3 must complete under
  the 56 GiB guard; ALab must stay at or below its per-mesh adaptive figures.
- **Out of scope**: displacement; per-ray or lazily cached tessellation; Loop
  tessellation; a level stable across frames (a dicing reference camera).
- **Upstream** (opensubdiv-rs, filed and shipped during this change): compact patch
  storage (#21, #22), face-varying patches (#23), selected faces (#29). crust pins tag
  0.5.0.
