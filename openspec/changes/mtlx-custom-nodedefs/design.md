## Context

The stages this change targets keep their materials as inline UsdShade networks
under a top-level `/MaterialX/Materials` scope, beside several subtrees holding
geometry. Each network ends in `ND_UsdPreviewSurface_surfaceshader`, whose inputs
come from custom nodes defined in a studio `.mtlx` library.

## Decisions

### D1. Out-of-chunk materials resolve on one side stage

`materials::material_stage` decides which stage a bound material resolves on:

- the chunk's own stage, when its population mask holds the material's subtree
  and every prim its network's connections reach (`connected_outside`). This is
  always the case outside streaming, and prototype paths are exempt;
- otherwise, the stage that `OutsideMaterials` keeps.

That stage is opened at the first miss it does not cover. Its mask is the union
of four sets: what it held before, every `material:binding*` target outside the
chunk (`outside_binding_targets`, instance proxies included), the material
itself, and the prims its connections reach. Those are followed on the side
stage once they are populated, one more open per hop of a chain of shared node
graphs, at most `MAX_SIDE_STAGE_OPENS` (8) per material. Dangling targets stay
in the mask, so they cost no second open; the mask moves only with a stage
opened to it. The
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

A definition is looked up by nodedef name first, then by `node` category,
output type and authored `version`, because a studio's nodedef names need not
follow `ND_<category>_<type>`. No lookup falls back to another type or version.

Each directory is walked once by its canonical path, so a link back to an
ancestor reads nothing twice. The import keeps the library in its provenance:
an in-place rebuild reuses it, rather than reading every file again and
raising `mtlx.library_unreadable` a second time, which would force a full
re-import.

### D4. Expansion before compilation

A pass on `Doc` replaces each instance of a custom node with a prefixed copy of
its graph:

- each `interfacename` is wired to the instance's input, falling back to the
  nodedef default;
- `nodename=inst output=X` is redirected to the graph output `X`, keeping that
  output's own `output=` selection;
- the pass recurses, so a graph that instantiates other custom nodes expands
  too. Each copy carries the nodedefs it was expanded through: a definition
  reaching itself is a cycle, refused at its first re-entry and reported once.
  `MAX_DEPTH` (16) and a budget of `MAX_EXPANDED_NODES` (262 144) nodes in all
  cut the rest, since k instances per level grow as k to the depth;
- an instance's `version`, when authored, selects its nodedef; a category
  defined for another type or version only is reported, never expanded with
  that signature;
- the compiler's own operators always win over a definition of the same
  category.

Both the `.mtlx` path and the inline-USD path go through this pass.

### D5. Matrices are compile-time constants

`Val` holds four lanes, and a projection's matrix is in practice a constant, so
`transformmatrix` folds its matrix into an affine op. A matrix from a varying
source is reported, not approximated. The product is genosl's, the reference
the rest of crust-mtlx is checked against (the OSL oracle, BSDL): a `vector3`
by a `matrix44` is `transform(matrix, vector)`, a direction, so the last row
does not translate; a `vector2` by a `matrix33` is `point(x, y, 1)`, so it
does. MaterialX's GLSL translates the `vector3` too (`(v, 1)` times the
matrix); it was followed at first, and dropped to keep one reference.

## Risks

- The Cornell box's `/scene/Looks/initialShadingGroup` now resolves on the side
  stage. It is empty, so it still gets the default grey. The warning text
  changes from "not resolvable" to "has no surface shader".
- The OSL oracle fixture does not cover the new nodes until it is regenerated
  on an exact-math OSL build; they are pinned by hand meanwhile.
- A custom node that places a point with `transformmatrix` (`vector3` by
  `matrix44`), authored against a GLSL viewer, renders without the
  translation, as it does under OSL.
