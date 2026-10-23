## Context

The stages this change targets keep their materials as inline UsdShade networks
under a top-level `/MaterialX/Materials` scope, beside several subtrees holding
geometry. Each network ends in `ND_UsdPreviewSurface_surfaceshader`, whose inputs
come from custom nodes defined in a studio `.mtlx` library.

## Decisions

### D1. Out-of-chunk materials resolve on one side stage

`materials::material_stage` decides which stage a bound material resolves on:

- the chunk's own stage, when its population mask holds the material's subtree.
  This is always the case outside streaming, and prototype paths are exempt;
- otherwise, the stage that `OutsideMaterials` keeps.

That stage is opened at the first miss it does not cover. Its mask is the union
of three sets: what it held before, every `material:binding*` target outside the
chunk (`outside_binding_targets`, instance proxies included), and the material
itself. Dangling targets stay in the mask, so they cost no second open. The
stage is released after the last chunk, before the top-level commit, and its
layers join `Scene::layers`.

Rejected:

- Re-opening the chunk with an expanded mask (`ExpandPopulationMask`). It
  composes the chunk's geometry twice, and a `/Looks` scope beside N geometry
  chunks would pay that cost per chunk.
- Walking the index stage up front. It composes everything outside payloads,
  which on ALab is the whole stage.
- One open per missing material. Each open parses the root layer again.

### D2. `UsdPreviewSurface` in MaterialX is MaterialX's graph

`preview.rs` maps the UsdShade id onto OpenPBR. That decoder reads
`UsdUVTexture` and primvar readers, not MaterialX graphs, and MaterialX defines
`ND_UsdPreviewSurface_surfaceshader` by its nodegraph. So the MaterialX id joins
the node-for-node surface builders.

One departure: at a threshold that folds to 0, the cutout `opacity ≥ 0` is taken
as 1. Compiled as written, every connected opacity would become a cutout, and
the shadow side would then walk every blocked ray hit by hit.

### D3. Custom nodedefs

The definitions are read once per import into `MaterialCache::library`, from two
sources:

- `Config::mtlx_plugin_search_paths`, split with the OS path-list separator;
- the document's own `<nodedef>` and `<nodegraph nodedef=…>` elements, which
  take precedence.

A definition is looked up by nodedef name first, then by `node` category and
output type, because a studio's nodedef names need not follow
`ND_<category>_<type>`.

### D4. Expansion before compilation

A pass on `Doc` replaces each instance of a custom node with a prefixed copy of
its graph:

- each `interfacename` is wired to the instance's input, falling back to the
  nodedef default;
- `nodename=inst output=X` is redirected to the graph output `X`, keeping that
  output's own `output=` selection;
- the pass recurses under a depth and cycle guard, so a graph that instantiates
  other custom nodes expands too;
- the compiler's own operators always win over a definition of the same
  category.

Both the `.mtlx` path and the inline-USD path go through this pass.

### D5. Matrices are compile-time constants

`Val` holds four lanes, and a projection's matrix is in practice a constant, so
`transformmatrix` folds its matrix into an affine op. A matrix from a varying
source is reported, not approximated. MaterialX's GLSL (`(v, 1)` times the
matrix, so the last row translates) is followed over genosl, whose
`transform(matrix, vector)` drops the translation.

## Risks

- The Cornell box's `/scene/Looks/initialShadingGroup` now resolves on the side
  stage. It is empty, so it still gets the default grey. The warning text
  changes from "not resolvable" to "has no surface shader".
- The OSL oracle fixture does not cover the new nodes until it is regenerated
  on an exact-math OSL build; they are pinned by hand meanwhile.
