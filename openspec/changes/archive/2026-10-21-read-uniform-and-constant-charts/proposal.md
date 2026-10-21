# Proposal

## Why

`mesh_uvs` asked only whether a chart's `interpolation` was `faceVarying` and read
every other value as `vertex`, so `UvSource::index_at` looked a `constant` or
`uniform` chart up by **point** index (#354). A constant chart resolved its one value
at point 0 and `(0, 0)` everywhere else, smearing the texture between the two; a
uniform chart gave faces another face's coordinate, then `(0, 0)` once the point index
passed the face count. No warning was raised, and the same chart reached both refiners
as a vertex channel.

## What Changes

- **A `constant` or `uniform` chart is rewritten into faceVarying when read.** The
  importer synthesizes per-face-vertex indices (all `0`, or the face index) composed
  with any authored `:indices`. Triangulation, uniform and adaptive subdivision and
  `MeshKey` already handle faceVarying; under subdivision each face's value is behind
  a seam wherever it differs from its neighbour's, so it stays constant per face
  whatever `faceVaryingLinearInterpolation` says.
- **An unauthored `interpolation` stays `vertex`**, except on a chart of exactly one
  value, which can only be constant.
- No image changes for `vertex`, `varying` or `faceVarying` charts.

## Impact

- `crust-core/src/scene/usd_import/mesh.rs` (`mesh_uvs`),
  `crust-core/tests/usd_inline.rs` (its test).
- `openspec/specs/usd-scene-import` (spec and design record).
