# Proposal

## Why

A session can render and probe a stage, but it cannot place anything on it.

- Putting a prop on a table, or scattering stones over terrain, means the agent has to
  guess heights and hand-write transforms or PointInstancer arrays through `author_usda`.
- Scene-assembly agents such as BowerBot do this placement with separate surface
  queries in numpy.
- The session already holds the composed scene's BVH, casts rays against it (`probe`),
  and the import reads PointInstancers. So placement can be computed against exactly
  the geometry that renders, and then checked with a render.

Depends on `live-session-source-reload`. Placement must never be computed against a
stale scene.

## What Changes

- **`drop_to_surface(prims, target?, align_to_normal?)`**:
  - Casts rays along the stage's down axis (from `upAxis`) from each prim's world-space
    footprint against the session's scene, or against the `target` prims only.
  - Authors a translate opinion in the override layer that rests each prim's lowest
    point on the highest hit; optionally also authors a rotation to the hit normal.
  - Prims that hit nothing are reported and left unchanged.
- **`scatter_on_surface(target, prototypes, count | density, seed, ...)`**:
  - Samples points on the target prims' triangles by area, with the session's `openqmc`
    domain keyed by `seed`, so the same inputs give the same result.
  - Authors one `PointInstancer` in the override layer: positions, orientations,
    scales and `protoIndices`, with `prototypes` referencing the given prims or assets.
  - Has optional random rotation about the up axis, a scale range and a maximum slope.
- Both tools are one edit batch each: saved, imported, undoable, and reporting warning
  codes gained or lost, like every other edit.
- The session can answer the world-space bounds of a prim's geometry. Today the
  provenance maps geometry to prim only (`prim_of_geometry`); placement needs the
  reverse.

The override layer remains the only layer the session writes. Placement moves only
prims, never vertices.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mcp-session`:
  - The tool list gains `drop_to_surface` and `scatter_on_surface`.
  - New requirements for:
    - placement resting on the rendered geometry;
    - deterministic scattering under a seed;
    - the authored `PointInstancer` rendering as the import reads it.

## Impact

- `crates/crust-render/src/mcp/`: the tools, their snippet authoring (`edit.rs`) and
  tests:
  - a dropped prim's lowest point lies on the surface, within float tolerance;
  - a scatter repeated with the same seed is byte-identical in the layer;
  - a scatter renders as a full import of the saved layer does.
- `crust-core`:
  - provenance from a prim to its geometry runs (or bounds), kept in proportion to the
    prim count, nothing per triangle;
  - a ray query on the committed world;
  - area-weighted triangle sampling, shared with mesh area lights rather than copied.
- Performance: no render-path change. Callgrind on the Cornell box must show no render
  function's count moving.
- Docs:
  - `openspec/specs/mcp-session/design.md`;
  - `site/content/docs/help/claude-desktop.md`.
- Out of scope:
  - `scatter_along_path`, grid layout, unit conversion;
  - asset libraries, writing any layer other than the override.
