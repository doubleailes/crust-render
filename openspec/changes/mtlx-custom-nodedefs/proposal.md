## Why

A DCC's MaterialX export that relies on a studio node library renders grey.
Such an export writes each material as an inline UsdShade network of custom
nodes, under an `ND_UsdPreviewSurface_surfaceshader`. The nodes are defined in
the studio's own `.mtlx` library, which USD's `usdMtlx` plugin finds through
`PXR_MTLX_PLUGIN_SEARCH_PATHS`. Nothing in crust or in `openusd` reads that
variable, `crust-mtlx` skips `<nodedef>` and nodegraph implementations, and an
`interfacename` input becomes a zero.

Two problems in front of the nodes kept every such material grey, even if the
nodes had been supported:

- **Streaming.** An export that puts every material under one top-level
  `/MaterialX/Materials` scope, beside three or more geometry subtrees, is
  streamed one masked stage per subtree. The materials are never on the stage
  that traverses the geometry bound to them, so each raised
  `material.fallback_default` ("not resolvable"). This was a documented gap of
  the streaming import.
- **The surface.** `ND_UsdPreviewSurface_surfaceshader` had no MaterialX
  operator, so every such material compiled to no leaves (`mtlx.incomplete`).

## What Changes

- **A material bound from outside a streamed chunk resolves.** At the first
  such miss in a chunk, the import walks the chunk's `material:binding*`
  relationships and opens one stage masked to every out-of-chunk target, and
  keeps it across chunks. A chunk that binds nothing outside itself pays
  nothing. Output is unchanged for every stage whose materials resolved before.
- **The MaterialX `UsdPreviewSurface` is its nodegraph.** `crust-mtlx` builds
  `IMP_UsdPreviewSurface_surfaceshader` (MaterialX 1.39.4) node for node, beside
  `open_pbr_surface`, `standard_surface` and `gltf_pbr`, with its nodedef
  vendored and checked. The UsdShade `info:id = "UsdPreviewSurface"` keeps its
  OpenPBR mapping.
- **Custom nodedefs from `PXR_MTLX_PLUGIN_SEARCH_PATHS`.** A
  `crust_core::Config` field reads the search paths. `crust-mtlx` parses
  `<nodedef>` and nodegraph implementations from every `.mtlx` under them and
  from the document itself. An expansion pass on `Doc` replaces each instance of
  a custom node with its graph before compilation, for `.mtlx` files and inline
  USD networks alike.
- **The standard nodes such libraries use**: `separate2`, `separate3`,
  `separate4`, `switch`, `over`, `in`, `out`, `difference`, `premult`, `place2d`,
  and `transformmatrix` with constant `matrix44` values.

## Capabilities

### Modified Capabilities

- `usd-scene-import`: streamed chunks resolve materials bound from outside them.
- `materials`: the MaterialX `UsdPreviewSurface`, custom nodedefs, and new
  standard nodes.

## Impact

- `crust-core/src/scene/usd_import/materials.rs` and `mod.rs` (the side stage).
- `crust-mtlx/src/library.rs`, `expand.rs`, `surface/usd_preview_surface.rs`,
  `eval/compile.rs`, and `crust-mtlx/tests/nodedefs/`.
- No render-throughput change. The integrator, the kernel and the existing
  materials are untouched. The side stage costs one masked open per import that
  needs it.
