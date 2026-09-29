## Why

Subdivision is decided by the crust-only per-prim attribute `crust:subdivisionLevel`, so
USD assets that author `subdivisionScheme` are never refined. The DPEL MaterialX teapot
is the example: it authors `catmullClark` on 32k-quad cages, and crust renders the raw
cage flat-shaded, so its glaze highlights break into facets. Forcing a level does not
help either: a refined mesh drops `primvars:st`, so the glaze texture disappears. An asset
should render as it is authored, with no crust attributes added to it.

## What Changes

- **BREAKING**: every mesh is a subdivision surface unless its `subdivisionScheme` is
  `none`. An unauthored scheme is USD's fallback, `catmullClark`, as in Hydra and
  RenderMan. That is how production assets mark subdivision meshes: ALab's and
  Kitchen_set's render meshes author neither a scheme nor normals, while ALab's polygonal
  display proxies author `none` and normals. (A first cut refined only explicitly
  authored schemes; ALab then rendered its subdivision cages faceted — the foam hand and
  the glassware.) The checked-in polygon samples now author `none`.
- **BREAKING**: the per-prim `crust:subdivisionLevel` attribute is removed. It is ignored,
  with one `WARN` per stage that authors it.
- One global refinement level, as in Hydra's `refineLevel`. It defaults to **0** — nothing is refined unless asked — is read
  from `crust:subdivisionLevel` on the `RenderSettings` prim, can be overridden with a new
  `--subdiv-level <n>` CLI flag (through `UsdImportOptions`), and is clamped to 6. At
  level 0 a subdivision surface renders its cage shaded with smooth normals, as Hydra's
  Storm does at low complexity. `CRUST_SUBDIV=0` stays the old behaviour: every cage
  faceted.
- The authored UV primvar (`st` and its fallbacks, with `:indices`) is refined along with
  the mesh:
  - `faceVarying` UVs become a real face-varying channel that honours
    `faceVaryingLinearInterpolation` (`none` / `cornersOnly` / `cornersPlus1` /
    `cornersPlus2` / `boundaries` / `all`, USD fallback `cornersPlus1`).
  - `vertex` UVs are refined like points.

  UV-textured MaterialX and UsdPreviewSurface meshes therefore keep their textures when
  subdivided. This retires the "UV charts on subdivided meshes" known gap.
- `samples/subdivision.usda` is re-authored around schemes rather than levels: no scheme
  (the fallback), `none`, `bilinear`, `catmullClark`, a creased `catmullClark` cube, and a
  UV-textured `catmullClark` mesh. Levels are compared with `--subdiv-level`.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `usd-scene-import`: the geometry mapping gains a subdivision requirement (trigger by
  authored scheme, global level, refined UVs); render settings gain
  `crust:subdivisionLevel`.
- `cli`: the new `--subdiv-level` override.
- `textures`: "UV charts on subdivided meshes" leaves the known-gaps list.

## Impact

- Code:
  - `crust-core/src/scene/usd_import/{attrs,mesh,settings,mod}.rs`:
    - drop `subdiv_level(prim)`
    - the resolved scheme (schema fallback `catmullClark`) decides; smooth cage normals
      at level 0
    - thread the resolved level through the import context
  - `crust-core/src/scene/subdiv.rs`: UV channel and face-varying interpolation option
  - `crust-core/src/scene.rs`: `UsdImportOptions::subdivision_level`
  - `crust-render/src/main.rs`: the flag
- Output: by default nothing is refined. Every mesh not authoring `none` renders its
  cage shaded with smooth normals, where before it was faceted; the triangle count and
  memory are unchanged. The checked-in samples author `none` on their polygon meshes, so
  their goldens are bit-identical except `subdivision.usda`. The DPEL teapot wrappers
  (`materialx_teapot`, `materialx_showcase`) set `crust:subdivisionLevel = 1` on their
  RenderSettings and render smooth and textured. ALab, Kitchen_set and Moana (which
  authors `catmullClark` in 189 of its 213 mesh files) are subdivision scenes: a
  requested level 1 costs them 4× (ALab: 32 → 49 GiB peak), which is why the default is
  conservative.
- Performance and memory: a refined mesh costs 4^L× the triangles, so level 1 is 4×
  per subdivided cage; the default, 0, costs only the smooth cage normals. The refined UV channel adds a parallel
  face-varying hierarchy during refinement (measured by `subdivision_memory_probe`).
  Scenes that author no scheme pay nothing.
- Tests: `usd_inline.rs` and `usd_scene.rs` subdivision tests move from per-prim levels
  to authored schemes plus an import option. `scripts/gen_subdiv_stress.py` authors the
  scheme and the settings level.
- Docs: `usd-scene-import/design.md` (subdivision section), `cli/design.md`,
  `textures/design.md`, and the `CRUST_SUBDIV` row in `docs/architecture.md`.
